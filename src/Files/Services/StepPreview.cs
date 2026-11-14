using System.Globalization;
using System.Text;
using System.Windows;

namespace Files.Services;

/// <summary>
/// Reads a STEP model (ISO 10303-21) as a wireframe: every edge of its solids, seen from an isometric
/// view. Lines, circles and ellipses are drawn exactly, and B-spline edges along their control points,
/// which is close at this size. Kept small on purpose: no hidden lines, no shading, and an assembly's
/// parts are drawn where each was modelled, without the placements that put them together.
/// </summary>
public static class StepPreview
{
    public const int MaxInstances = 1_000_000;
    private const int SegmentsPerTurn = 48;

    private readonly record struct Ref(int Id);

    /// <summary>One entity, or one part of a complex entity: its name and arguments (numbers, refs, strings, lists, typed values).</summary>
    private sealed record Instance(string Name, IReadOnlyList<object?> Args);

    private readonly record struct Vector3(double X, double Y, double Z)
    {
        public static Vector3 operator +(Vector3 a, Vector3 b) => new(a.X + b.X, a.Y + b.Y, a.Z + b.Z);
        public static Vector3 operator -(Vector3 a, Vector3 b) => new(a.X - b.X, a.Y - b.Y, a.Z - b.Z);
        public static Vector3 operator *(Vector3 a, double s) => new(a.X * s, a.Y * s, a.Z * s);
        public double Dot(Vector3 b) => X * b.X + Y * b.Y + Z * b.Z;
        public Vector3 Cross(Vector3 b) => new(Y * b.Z - Z * b.Y, Z * b.X - X * b.Z, X * b.Y - Y * b.X);
        public double Length => Math.Sqrt(Dot(this));
        public Vector3 Unit => Length > 0 ? this * (1 / Length) : this;
    }

    /// <summary>The model's edges as lines; null when the file is not a STEP model.</summary>
    public static LineWork? Read(string path, CancellationToken cancel)
    {
        string text = Encoding.Latin1.GetString(File.ReadAllBytes(path));
        if (!text.StartsWith("ISO-10303-21", StringComparison.Ordinal))
        {
            return null;
        }
        int data = text.IndexOf("DATA;", StringComparison.Ordinal);
        if (data < 0)
        {
            return null;
        }
        var work = new LineWork();
        var model = new Model(text);
        if (!model.Index(data + 5, cancel))
        {
            work.GiveUp();
            return work;
        }
        foreach (int edge in model.Edges)
        {
            cancel.ThrowIfCancellationRequested();
            if (work.TooLarge)
            {
                break;
            }
            if (model.Edge(edge) is Vector3[] points)
            {
                work.Add(points.Select(Isometric).ToArray());
            }
        }
        return work;
    }

    /// <summary>Seen from the corner where x, y and z are all positive, with z up.</summary>
    private static Point Isometric(Vector3 p) => new((p.Y - p.X) / Math.Sqrt(2), (2 * p.Z - p.X - p.Y) / Math.Sqrt(6));

    private sealed class Model(string text)
    {
        private readonly Dictionary<int, (int Start, int Length)> _spans = [];
        private readonly Dictionary<int, Instance?> _parsed = [];
        private const int MaxDepth = 32;
        private int _depth;

        public List<int> Edges { get; } = [];

        /// <summary>Finds every instance (#id = body;) after <paramref name="from"/>; false when there are too many.</summary>
        public bool Index(int from, CancellationToken cancel)
        {
            int i = from;
            while ((i = text.IndexOf('#', i)) >= 0)
            {
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
                if (text.AsSpan(start, i - start).TrimStart().StartsWith("EDGE_CURVE", StringComparison.Ordinal))
                {
                    Edges.Add(id);
                }
                if (_spans.Count % 4096 == 0)
                {
                    cancel.ThrowIfCancellationRequested();
                }
            }
            return true;
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

        private Instance? Get(object? reference)
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

        /// <summary>The points along an edge, or null when it does not lead to two points.</summary>
        public Vector3[]? Edge(int id)
        {
            Instance? edge = Get(new Ref(id));
            if (edge is not { Args.Count: >= 5 } || Vertex(edge.Args[1]) is not Vector3 from || Vertex(edge.Args[2]) is not Vector3 to)
            {
                return null;
            }
            bool sameSense = edge.Args[4] as string != ".F.";
            Instance? curve = Get(edge.Args[3]);
            for (int hops = 0; hops < 4 && curve is { Name: "SURFACE_CURVE" or "SEAM_CURVE" or "BOUNDED_SURFACE_CURVE" or "TRIMMED_CURVE", Args.Count: >= 2 }; hops++)
            {
                curve = Get(curve.Args[1]);
            }
            return curve switch
            {
                { Name: "CIRCLE", Args.Count: >= 3 } => Conic(curve.Args[1], Number(curve.Args[2]), Number(curve.Args[2]), from, to, sameSense),
                { Name: "ELLIPSE", Args.Count: >= 4 } => Conic(curve.Args[1], Number(curve.Args[2]), Number(curve.Args[3]), from, to, sameSense),
                _ when ControlPoints(curve) is { Length: >= 2 } controls => controls,
                _ => [from, to],
            };
        }

        /// <summary>A B-spline's control points: the third argument of a simple one, the second of B_SPLINE_CURVE inside a complex one.</summary>
        private Vector3[]? ControlPoints(Instance? curve)
        {
            object? points = curve switch
            {
                { Name: "" } => curve.Args.OfType<Instance>().FirstOrDefault(part => part.Name == "B_SPLINE_CURVE")?.Args.ElementAtOrDefault(1),
                { Name: var name, Args.Count: >= 3 } when name.StartsWith("B_SPLINE_CURVE", StringComparison.Ordinal) => curve.Args[2],
                _ => null,
            };
            return points is IReadOnlyList<object?> list
                ? list.Select(Point3).OfType<Vector3>().ToArray()
                : null;
        }

        /// <summary>
        /// The arc of a circle or ellipse between the edge's vertices, counter-clockwise about the placement's
        /// axis; an edge whose curve runs the other way (same_sense false) goes from its end to its start.
        /// </summary>
        private Vector3[]? Conic(object? placement, double radiusX, double radiusY, Vector3 from, Vector3 to, bool sameSense)
        {
            if (Get(placement) is not { Name: "AXIS2_PLACEMENT_3D", Args.Count: >= 2 } axes || Point3(axes.Args[1]) is not Vector3 centre || radiusX <= 0 || radiusY <= 0)
            {
                return null;
            }
            Vector3 z = (Direction(axes.Args.ElementAtOrDefault(2)) ?? new Vector3(0, 0, 1)).Unit;
            Vector3 guess = Direction(axes.Args.ElementAtOrDefault(3)) ?? (Math.Abs(z.X) < 0.9 ? new Vector3(1, 0, 0) : new Vector3(0, 1, 0));
            Vector3 x = (guess - z * guess.Dot(z)).Unit;
            Vector3 y = z.Cross(x);
            double Angle(Vector3 p) => Math.Atan2((p - centre).Dot(y) / radiusY, (p - centre).Dot(x) / radiusX);
            (Vector3 first, Vector3 last) = sameSense ? (from, to) : (to, from);
            double start = Angle(first);
            double sweep = Angle(last) - start;
            while (sweep <= 1e-9)
            {
                sweep += 2 * Math.PI;
            }
            int segments = Math.Max(2, (int)Math.Ceiling(sweep / (2 * Math.PI) * SegmentsPerTurn));
            var points = new Vector3[segments + 1];
            for (int i = 0; i <= segments; i++)
            {
                double t = start + sweep * i / segments;
                points[i] = centre + x * (radiusX * Math.Cos(t)) + y * (radiusY * Math.Sin(t));
            }
            return points;
        }

        private Vector3? Vertex(object? reference) =>
            Get(reference) is { Name: "VERTEX_POINT", Args.Count: >= 2 } vertex ? Point3(vertex.Args[1]) : null;

        private Vector3? Point3(object? reference) =>
            Get(reference) is { Name: "CARTESIAN_POINT", Args.Count: >= 2 } point ? Triple(point.Args[1]) : null;

        private Vector3? Direction(object? reference) =>
            Get(reference) is { Name: "DIRECTION", Args.Count: >= 2 } direction ? Triple(direction.Args[1]) : null;

        /// <summary>Two or three coordinates; a 2D point lies at z = 0.</summary>
        private static Vector3? Triple(object? value) =>
            value is IReadOnlyList<object?> { Count: >= 2 } list && list.All(item => item is null or double)
                ? new Vector3(Number(list[0]), Number(list[1]), list.Count > 2 ? Number(list[2]) : 0)
                : null;

        /// <summary>A number, also when it is typed, as in POSITIVE_LENGTH_MEASURE(2.5).</summary>
        private static double Number(object? value) => value switch
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
}
