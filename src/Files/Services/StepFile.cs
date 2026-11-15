using System.Globalization;

namespace Files.Services;

/// <summary>A reference to another instance in a STEP file (#id).</summary>
internal readonly record struct Ref(int Id);

/// <summary>One entity, or one part of a complex entity: its name and arguments (numbers, refs, strings, lists, typed values).</summary>
internal sealed record Instance(string Name, IReadOnlyList<object?> Args)
{
    /// <summary>The named part of a complex entity (whose own name is empty), or this instance when it has that name.</summary>
    public Instance? Part(string name) =>
        Name == name ? this : Name == "" ? Args.OfType<Instance>().FirstOrDefault(part => part.Name == name) : null;
}

internal readonly record struct Vector3(double X, double Y, double Z)
{
    public static Vector3 operator +(Vector3 a, Vector3 b) => new(a.X + b.X, a.Y + b.Y, a.Z + b.Z);
    public static Vector3 operator -(Vector3 a, Vector3 b) => new(a.X - b.X, a.Y - b.Y, a.Z - b.Z);
    public static Vector3 operator *(Vector3 a, double s) => new(a.X * s, a.Y * s, a.Z * s);
    public double Dot(Vector3 b) => X * b.X + Y * b.Y + Z * b.Z;
    public Vector3 Cross(Vector3 b) => new(Y * b.Z - Z * b.Y, Z * b.X - X * b.Z, X * b.Y - Y * b.X);
    public double Length => Math.Sqrt(Dot(this));
    public Vector3 Unit => Length > 0 ? this * (1 / Length) : this;
    public bool IsFinite => double.IsFinite(X) && double.IsFinite(Y) && double.IsFinite(Z);
}

/// <summary>
/// A right-handed frame: an origin and three unit axes. It is also the rigid move that takes
/// coordinates in the frame to the coordinates it is placed in.
/// </summary>
internal readonly record struct Frame(Vector3 Origin, Vector3 X, Vector3 Y, Vector3 Z)
{
    public static Frame Identity { get; } = new(default, new(1, 0, 0), new(0, 1, 0), new(0, 0, 1));

    public Vector3 Point(Vector3 local) => Origin + Direction(local);

    public Vector3 Direction(Vector3 local) => X * local.X + Y * local.Y + Z * local.Z;

    /// <summary>A point's coordinates in this frame.</summary>
    public Vector3 Local(Vector3 point)
    {
        Vector3 d = point - Origin;
        return new(d.Dot(X), d.Dot(Y), d.Dot(Z));
    }

    /// <summary>This move after <paramref name="inner"/>.</summary>
    public Frame After(Frame inner) => new(Point(inner.Origin), Direction(inner.X), Direction(inner.Y), Direction(inner.Z));

    public Frame Inverse
    {
        get
        {
            var x = new Vector3(X.X, Y.X, Z.X);
            var y = new Vector3(X.Y, Y.Y, Z.Y);
            var z = new Vector3(X.Z, Y.Z, Z.Z);
            return new((x * Origin.X + y * Origin.Y + z * Origin.Z) * -1, x, y, z);
        }
    }
}

/// <summary>
/// A STEP file (ISO 10303-21) indexed by instance id, each instance parsed when first asked for.
/// Bounded for untrusted files: at most <see cref="MaxInstances"/> instances, and lists nested at most 32 deep.
/// </summary>
internal sealed class StepFile(string text)
{
    public const int MaxInstances = 1_000_000;
    private const int MaxDepth = 32;

    /// <summary>A list longer than this (a real model's longest is a B-spline's control points) drops its instance.</summary>
    private const int MaxListItems = 1_000_000;

    private readonly Dictionary<int, (int Start, int Length)> _spans = [];
    private readonly Dictionary<int, Instance?> _parsed = [];
    private readonly Dictionary<string, List<int>> _byName = [];
    private int _depth;

    /// <summary>The entity names whose ids <see cref="Named"/> lists; a complex entity is listed under "".</summary>
    private static readonly HashSet<string> Listed =
    [
        "EDGE_CURVE", "MANIFOLD_SOLID_BREP", "BREP_WITH_VOIDS", "SHELL_BASED_SURFACE_MODEL",
        "SHAPE_REPRESENTATION_RELATIONSHIP", "SHAPE_DEFINITION_REPRESENTATION", "CONTEXT_DEPENDENT_SHAPE_REPRESENTATION", "",
    ];

    /// <summary>Plane angles are in degrees (a conversion-based unit named DEGREE), not radians.</summary>
    public bool AnglesInDegrees { get; } = NamesDegrees(text);

    /// <summary>A conversion-based unit named DEGREE (or DEGREES), found without a pass of a regular expression over the whole file.</summary>
    private static bool NamesDegrees(string text)
    {
        const string Unit = "CONVERSION_BASED_UNIT";
        for (int at = text.IndexOf(Unit, StringComparison.Ordinal); at >= 0; at = text.IndexOf(Unit, at + Unit.Length, StringComparison.Ordinal))
        {
            ReadOnlySpan<char> after = text.AsSpan(at + Unit.Length).TrimStart();
            if (after.StartsWith("(") && after[1..].TrimStart().StartsWith("'DEGREE", StringComparison.OrdinalIgnoreCase))
            {
                return true;
            }
        }
        return false;
    }

    /// <summary>The ids of every instance with this entity name, in file order (names from <see cref="Listed"/> only).</summary>
    public IReadOnlyList<int> Named(string name) => _byName.TryGetValue(name, out List<int>? ids) ? ids : [];

    /// <summary>The ids of every instance that is a representation (its entity name ends in REPRESENTATION).</summary>
    public List<int> Representations { get; } = [];

    /// <summary>Finds every instance (#id = body;) after <paramref name="from"/>; false when there are too many.</summary>
    public bool Index(int from, CancellationToken cancel)
    {
        int i = from;
        long seen = 0;
        while ((i = text.IndexOf('#', i)) >= 0)
        {
            // Counted per # rather than per instance: noise or repeated ids add none, but still take time.
            if ((++seen & 0xFFF) == 0)
            {
                cancel.ThrowIfCancellationRequested();
            }
            int idStart = ++i;
            while (i < text.Length && char.IsAsciiDigit(text[i]))
            {
                i++;
            }
            if (!int.TryParse(text.AsSpan(idStart, i - idStart), CultureInfo.InvariantCulture, out int id))
            {
                continue;
            }
            while (i < text.Length && char.IsWhiteSpace(text[i]))
            {
                i++;
            }
            if (i >= text.Length || text[i] != '=')
            {
                continue;
            }
            int start = ++i;
            i = EndOfInstance(i);
            _spans[id] = (start, i - start);
            if (_spans.Count > MaxInstances)
            {
                return false;
            }
            List(id, start, i);
        }
        return true;
    }

    private void List(int id, int start, int end)
    {
        ReadOnlySpan<char> body = text.AsSpan(start, end - start).TrimStart();
        int length = 0;
        while (length < body.Length && (char.IsAsciiLetterOrDigit(body[length]) || body[length] == '_'))
        {
            length++;
        }
        string name = body[..length].ToString();
        if (Listed.Contains(name))
        {
            if (!_byName.TryGetValue(name, out List<int>? ids))
            {
                _byName[name] = ids = [];
            }
            ids.Add(id);
        }
        if (name.EndsWith("REPRESENTATION", StringComparison.Ordinal))
        {
            Representations.Add(id);
        }
    }

    /// <summary>The semicolon that ends an instance, past any strings or comments holding one.</summary>
    private int EndOfInstance(int i)
    {
        while (i < text.Length && text[i] != ';')
        {
            if (text[i] == '\'')
            {
                i = EndOfString(i);
                continue;
            }
            if (text[i] == '/' && i + 1 < text.Length && text[i + 1] == '*')
            {
                int close = text.IndexOf("*/", i + 2, StringComparison.Ordinal);
                i = close < 0 ? text.Length : close + 2;
                continue;
            }
            i++;
        }
        return i;
    }

    /// <summary>Just past a quoted string; two quotes inside it stand for one.</summary>
    private int EndOfString(int i)
    {
        for (i++; i < text.Length; i++)
        {
            if (text[i] == '\'')
            {
                if (i + 1 < text.Length && text[i + 1] == '\'')
                {
                    i++;
                    continue;
                }
                return i + 1;
            }
        }
        return i;
    }

    public Instance? Get(int id) => Get(new Ref(id));

    public Instance? Get(object? reference)
    {
        if (reference is not Ref(int id) || !_spans.TryGetValue(id, out var span))
        {
            return null;
        }
        if (!_parsed.TryGetValue(id, out Instance? instance))
        {
            int i = span.Start;
            instance = ParseInstance(ref i, span.Start + span.Length);
            _parsed[id] = instance;
        }
        return instance;
    }

    /// <summary>The references in a list argument, or none when it is not a list.</summary>
    public static IEnumerable<object?> Items(object? list) => list as IReadOnlyList<object?> ?? [];

    public Vector3? Point3(object? reference) =>
        Get(reference) is { Name: "CARTESIAN_POINT", Args.Count: >= 2 } point ? Triple(point.Args[1]) : null;

    public Vector3? Direction(object? reference) =>
        Get(reference) is { Name: "DIRECTION", Args.Count: >= 2 } direction ? Triple(direction.Args[1]) : null;

    /// <summary>
    /// An AXIS2_PLACEMENT_3D as a frame: its axis (z, up when unset) and its reference direction made square
    /// to it (x, any square direction when unset or parallel).
    /// </summary>
    public Frame? Placement(object? reference)
    {
        if (Get(reference) is not { Name: "AXIS2_PLACEMENT_3D", Args.Count: >= 2 } axes || Point3(axes.Args[1]) is not Vector3 origin)
        {
            return null;
        }
        Vector3 z = (Direction(axes.Args.ElementAtOrDefault(2)) ?? new Vector3(0, 0, 1)).Unit;
        if (z.Length == 0)
        {
            return null;
        }
        Vector3 guess = Direction(axes.Args.ElementAtOrDefault(3)) ?? default;
        Vector3 x = (guess - z * guess.Dot(z)).Unit;
        if (x.Length < 0.5)
        {
            guess = Math.Abs(z.X) < 0.9 ? new Vector3(1, 0, 0) : new Vector3(0, 1, 0);
            x = (guess - z * guess.Dot(z)).Unit;
        }
        return new Frame(origin, x, z.Cross(x), z);
    }

    /// <summary>A plane angle in radians, from the file's own unit.</summary>
    public double Angle(object? value) => AnglesInDegrees ? Number(value) * Math.PI / 180 : Number(value);

    /// <summary>Two or three coordinates; a 2D point lies at z = 0.</summary>
    public static Vector3? Triple(object? value) =>
        value is IReadOnlyList<object?> { Count: >= 2 } list && list.All(item => item is null or double)
            ? new Vector3(Number(list[0]), Number(list[1]), list.Count > 2 ? Number(list[2]) : 0)
            : null;

    /// <summary>A number, also when it is typed, as in POSITIVE_LENGTH_MEASURE(2.5).</summary>
    public static double Number(object? value) => value switch
    {
        double number => number,
        Instance { Args.Count: 1 } typed => Number(typed.Args[0]),
        _ => 0,
    };

    private Instance? ParseInstance(ref int i, int end)
    {
        SkipSpace(ref i, end);
        if (i < end && text[i] == '(')
        {
            // A complex entity: several named parts in one pair of brackets.
            i++;
            var parts = new List<object?>();
            while (true)
            {
                SkipSpace(ref i, end);
                if (i >= end || text[i] == ')')
                {
                    break;
                }
                if (ParseNamed(ref i, end) is not Instance part)
                {
                    return null;
                }
                parts.Add(part);
            }
            return new Instance("", parts);
        }
        return ParseNamed(ref i, end);
    }

    private Instance? ParseNamed(ref int i, int end)
    {
        int nameStart = i;
        while (i < end && (char.IsAsciiLetterOrDigit(text[i]) || text[i] == '_'))
        {
            i++;
        }
        string name = text[nameStart..i];
        SkipSpace(ref i, end);
        if (name.Length == 0 || i >= end || text[i] != '(')
        {
            return null;
        }
        return ParseList(ref i, end) is List<object?> args ? new Instance(name, args) : null;
    }

    /// <summary>A bracketed, comma-separated list; <paramref name="i"/> is on its opening bracket.</summary>
    private List<object?>? ParseList(ref int i, int end)
    {
        if (_depth >= MaxDepth)
        {
            // Lists nest only a few deep in a real model; far deeper would overflow the stack. The instance is dropped.
            i = end;
            return null;
        }
        _depth++;
        try
        {
            return ParseItems(ref i, end);
        }
        finally
        {
            _depth--;
        }
    }

    private List<object?>? ParseItems(ref int i, int end)
    {
        i++;
        var items = new List<object?>();
        while (true)
        {
            SkipSpace(ref i, end);
            if (i >= end)
            {
                return null;
            }
            char c = text[i];
            if (c == ')')
            {
                i++;
                return items;
            }
            if (c == ',')
            {
                i++;
                continue;
            }
            if (items.Count >= MaxListItems)
            {
                i = end;
                return null;
            }
            items.Add(ParseValue(ref i, end));
        }
    }

    private object? ParseValue(ref int i, int end)
    {
        char c = text[i];
        if (c == '(')
        {
            return ParseList(ref i, end);
        }
        if (c == '#')
        {
            int start = ++i;
            while (i < end && char.IsAsciiDigit(text[i]))
            {
                i++;
            }
            return int.TryParse(text.AsSpan(start, i - start), CultureInfo.InvariantCulture, out int id) ? new Ref(id) : null;
        }
        if (c == '\'')
        {
            int start = i;
            i = EndOfString(i);
            return text[(start + 1)..Math.Max(start + 1, i - 1)];
        }
        if (c == '.')
        {
            int close = text.IndexOf('.', i + 1, end - i - 1);
            int stop = close < 0 ? end : close + 1;
            string enumeration = text[i..stop];
            i = stop;
            return enumeration;
        }
        if (char.IsAsciiLetter(c))
        {
            return ParseNamed(ref i, end);
        }
        int valueStart = i;
        while (i < end && text[i] is not (',' or ')') && !char.IsWhiteSpace(text[i]))
        {
            i++;
        }
        if (i == valueStart)
        {
            i++; // a character that starts nothing; skipped so the list moves on
            return null;
        }
        // "$" and "*" (unset and derived) are null, as is anything that is not a number.
        return double.TryParse(text.AsSpan(valueStart, i - valueStart), NumberStyles.Float, CultureInfo.InvariantCulture, out double number)
            ? number
            : null;
    }

    private void SkipSpace(ref int i, int end)
    {
        while (i < end)
        {
            if (char.IsWhiteSpace(text[i]))
            {
                i++;
            }
            else if (text[i] == '/' && i + 1 < end && text[i + 1] == '*')
            {
                int close = text.IndexOf("*/", i + 2, StringComparison.Ordinal);
                i = close < 0 ? end : close + 2;
            }
            else
            {
                return;
            }
        }
    }
}
