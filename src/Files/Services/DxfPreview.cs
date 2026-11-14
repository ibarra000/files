using System.Globalization;
using System.Text;
using System.Windows;
using System.Windows.Media;

namespace Files.Services;

/// <summary>What a DXF gives the preview: its geometry as lines, and the preview picture a CAD program may have saved in it.</summary>
/// <param name="Saved">The saved preview as BMP bytes, or null.</param>
public sealed record DxfContent(LineWork Work, byte[]? Saved);

/// <summary>
/// Reads an ASCII DXF without a CAD library. A DXF is pairs of lines, a group code and a value; the
/// model's lines, arcs, circles, ellipses, polylines and splines are drawn, with block references
/// expanded. Text, hatches, dimensions and 3D solids are not drawn; for those the preview picture an
/// AutoCAD-family program saves (the THUMBNAILIMAGE section) is used instead.
/// </summary>
public static class DxfPreview
{
    public const int MaxEntities = 500_000;
    private const int MaxNesting = 8;

    /// <summary>The most values one entity may hold: a polyline of a million points is already too large to draw.</summary>
    private const int MaxPairs = 3 * LineWork.MaxPoints / 2;

    /// <summary>
    /// The most entities drawn, counting each time a block is inserted: blocks that insert each other
    /// many times over multiply, so a small file could otherwise ask for billions.
    /// </summary>
    private const int MaxVisits = 4 * MaxEntities;
    private const string BinarySentinel = "AutoCAD Binary DXF";

    /// <summary>Only these group codes are kept: the rest (layers, colours, text) is not drawn.</summary>
    private static readonly HashSet<int> KeptCodes = [1, 2, 3, 10, 11, 20, 21, 40, 41, 42, 43, 50, 51, 67, 70, 71, 230];

    private sealed record Entity(string Type, List<(int Code, string Value)> Pairs)
    {
        public double Number(int code, double fallback = 0)
        {
            foreach (var (c, value) in Pairs)
            {
                if (c == code)
                {
                    return double.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out double number) ? number : fallback;
                }
            }
            return fallback;
        }

        public string? Text(int code) => Pairs.FirstOrDefault(pair => pair.Code == code).Value?.Trim();

        public Point At(int xCode = 10) => new(Number(xCode), Number(xCode + 10));

        /// <summary>Arcs drawn with their normal pointing down (a mirrored arc) run the other way across x.</summary>
        public bool Mirrored => Number(230, 1) < 0;
    }

    private sealed record Block(Point Base, List<Entity> Entities);

    /// <summary>The drawing's lines and saved preview; null when it is not an ASCII DXF (a binary DXF, or a damaged file).</summary>
    public static DxfContent? Read(string path, CancellationToken cancel)
    {
        using var reader = new StreamReader(path, Encoding.Latin1);
        var entities = new List<Entity>();
        var blocks = new List<(Entity Header, List<Entity> Entities)>();
        var saved = new StringBuilder();
        var work = new LineWork();
        string? section = null;
        List<Entity>? target = null;
        Entity? current = null;
        int count = 0;
        bool sawSection = false;
        while (reader.ReadLine() is string codeLine)
        {
            if (reader.ReadLine() is not string value)
            {
                break;
            }
            if (codeLine.StartsWith(BinarySentinel, StringComparison.Ordinal) || !int.TryParse(codeLine.Trim(), CultureInfo.InvariantCulture, out int code))
            {
                return sawSection ? Finish(work, entities, blocks, saved, cancel) : null;
            }
            value = value.TrimEnd();
            if (code != 0)
            {
                if (section == "THUMBNAILIMAGE" && code == 310)
                {
                    saved.Append(value);
                }
                else if (code == 2 && current is null && section is not null && target is null && section.Length == 0)
                {
                    section = value;
                    target = section == "ENTITIES" ? entities : null;
                }
                else if (current is not null && KeptCodes.Contains(code))
                {
                    if (current.Pairs.Count >= MaxPairs)
                    {
                        work.GiveUp();
                        return new DxfContent(work, null);
                    }
                    current.Pairs.Add((code, value));
                }
                continue;
            }
            // Code 0 starts the next record and ends the one before.
            current = null;
            switch (value)
            {
                case "SECTION":
                    sawSection = true;
                    section = "";
                    target = null;
                    break;
                case "ENDSEC":
                    section = null;
                    target = null;
                    break;
                case "EOF":
                    return Finish(work, entities, blocks, saved, cancel);
                case "BLOCK" when section == "BLOCKS":
                    // Its own pairs (the name and base point) are read into the header; the entities after it are the block's.
                    current = new Entity(value, []);
                    target = [];
                    blocks.Add((current, target));
                    break;
                case "ENDBLK" when section == "BLOCKS":
                    target = null;
                    break;
                default:
                    if (target is not null)
                    {
                        if (++count > MaxEntities)
                        {
                            work.GiveUp();
                            return new DxfContent(work, null);
                        }
                        current = new Entity(value, []);
                        target.Add(current);
                    }
                    break;
            }
            cancel.ThrowIfCancellationRequested();
        }
        return sawSection ? Finish(work, entities, blocks, saved, cancel) : null;
    }

    private static DxfContent Finish(LineWork work, List<Entity> entities, List<(Entity Header, List<Entity> Entities)> blocks, StringBuilder saved, CancellationToken cancel)
    {
        var named = new Dictionary<string, Block>(StringComparer.OrdinalIgnoreCase);
        foreach (var (header, members) in blocks)
        {
            if (header.Text(2) is { Length: > 0 } name)
            {
                named[name] = new Block(header.At(), members);
            }
        }
        // Model space first; a drawing laid out only on paper (code 67 = 1) is drawn from paper space.
        var model = entities.Where(entity => entity.Number(67) == 0).ToList();
        var visits = new Visits(cancel);
        foreach (Entity entity in Draw(model.Count > 0 ? model : entities))
        {
            if (work.TooLarge)
            {
                break;
            }
            AddEntity(work, entity, Matrix.Identity, named, 0, visits);
        }
        return new DxfContent(work, SavedPicture(saved.ToString()));
    }

    /// <summary>Joins each POLYLINE to the VERTEX records after it, so the rest can treat every entity alone.</summary>
    private static IEnumerable<Entity> Draw(List<Entity> entities)
    {
        for (int i = 0; i < entities.Count; i++)
        {
            Entity entity = entities[i];
            if (entity.Type != "POLYLINE")
            {
                if (entity.Type is not ("VERTEX" or "SEQEND"))
                {
                    yield return entity;
                }
                continue;
            }
            var pairs = new List<(int, string)>(entity.Pairs.Where(pair => pair.Code is 70 or 230));
            for (i++; i < entities.Count && entities[i].Type == "VERTEX"; i++)
            {
                pairs.AddRange(entities[i].Pairs.Where(pair => pair.Code is 10 or 20 or 42));
            }
            i--;
            // A polyface or a mesh (flags 64 and 16) is a 3D surface, not a line through its vertices.
            if (((int)entity.Number(70) & (16 | 64)) == 0)
            {
                yield return new Entity("LWPOLYLINE", pairs);
            }
        }
    }

    /// <summary>How many entities have been drawn, so block expansion stops in time, and can be cancelled.</summary>
    private sealed class Visits(CancellationToken cancel)
    {
        private int _count;

        /// <summary>Counts one more; false once there have been too many.</summary>
        public bool Next()
        {
            if (++_count % 4096 == 0)
            {
                cancel.ThrowIfCancellationRequested();
            }
            return _count <= MaxVisits;
        }
    }

    private static void AddEntity(LineWork work, Entity entity, Matrix transform, Dictionary<string, Block> blocks, int depth, Visits visits)
    {
        if (!visits.Next())
        {
            work.GiveUp();
            return;
        }
        Point[]? line = entity.Type switch
        {
            "LINE" => [entity.At(10), entity.At(11)],
            "CIRCLE" => LineDrawing.Arc(entity.At(), entity.Number(40), entity.Number(40), 0, 0, 2 * Math.PI),
            "ARC" => Arc(entity),
            "ELLIPSE" => Ellipse(entity),
            "LWPOLYLINE" => Polyline(entity),
            "SPLINE" => Spline(entity),
            _ => null,
        };
        if (entity.Type is "TEXT" or "ATTRIB" or "MTEXT")
        {
            AddText(work, entity, transform);
            return;
        }
        if (line is not null)
        {
            if (entity.Mirrored && entity.Type is "CIRCLE" or "ARC" or "LWPOLYLINE")
            {
                line = line.Select(point => new Point(-point.X, point.Y)).ToArray();
            }
            work.Add(line.Select(transform.Transform).ToArray());
            return;
        }
        if (entity.Type == "INSERT" && depth < MaxNesting && entity.Text(2) is string name && blocks.TryGetValue(name, out Block? block))
        {
            Matrix inner = Matrix.Identity;
            inner.Translate(-block.Base.X, -block.Base.Y);
            inner.Scale(entity.Number(41, 1) * (entity.Mirrored ? -1 : 1), entity.Number(42, 1));
            // Mirrored, the x flip comes before the rotation, which then turns the other way.
            inner.Rotate(entity.Mirrored ? -entity.Number(50) : entity.Number(50));
            Point at = entity.At();
            inner.Translate(entity.Mirrored ? -at.X : at.X, at.Y);
            Matrix combined = inner * transform;
            foreach (Entity part in Draw(block.Entities))
            {
                if (work.TooLarge)
                {
                    return;
                }
                AddEntity(work, part, combined, blocks, depth + 1, visits);
            }
        }
    }

    /// <summary>
    /// TEXT and ATTRIB start at their baseline; MTEXT is placed by its top-left corner (attachment 1, the
    /// usual one) and only its first line is drawn, with formatting codes taken out.
    /// </summary>
    private static void AddText(LineWork work, Entity entity, Matrix transform)
    {
        double height = entity.Number(40);
        string text = entity.Type == "MTEXT"
            ? MText(string.Concat(entity.Pairs.Where(pair => pair.Code == 3).Select(pair => pair.Value)) + entity.Text(1))
            : entity.Text(1) ?? "";
        Point at = entity.At();
        if (entity.Type == "MTEXT" && (int)entity.Number(71, 1) is 1 or 2 or 3)
        {
            at = new Point(at.X, at.Y - height);
        }
        // The drawing's own scale and turn apply to the text in a block.
        Vector across = transform.Transform(new Vector(1, 0));
        double size = Math.Sqrt(Math.Abs(transform.Determinant));
        double turn = Math.Atan2(across.Y, across.X) * 180 / Math.PI;
        work.Add(new Label(transform.Transform(at), height * size, entity.Number(50) + turn, text));
    }

    /// <summary>The first line of MTEXT, without its codes: \P breaks a line, \f...; and the like set fonts, braces group.</summary>
    private static string MText(string value)
    {
        string first = value.Split(@"\P", 2)[0];
        string plain = System.Text.RegularExpressions.Regex.Replace(first, @"\\[A-Za-z][^;\\{}]*;|\\[A-Za-z]|[{}]", "");
        return plain.Replace("%%c", "Ø", StringComparison.OrdinalIgnoreCase).Replace("%%d", "°", StringComparison.OrdinalIgnoreCase)
            .Replace("%%p", "±", StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>A sweep brought into (0, 2π], counter-clockwise; one that is not a number stays so, and draws nothing.</summary>
    private static double Turn(double sweep)
    {
        sweep %= 2 * Math.PI;
        return sweep <= 0 ? sweep + 2 * Math.PI : sweep;
    }

    private static Point[] Arc(Entity entity)
    {
        double start = entity.Number(50) * Math.PI / 180, end = entity.Number(51) * Math.PI / 180;
        double sweep = end - start;
        sweep = Turn(sweep);
        double radius = entity.Number(40);
        return LineDrawing.Arc(entity.At(), radius, radius, 0, start, sweep);
    }

    /// <summary>An ellipse is its centre, the end of its major axis (relative to the centre), the minor-to-major ratio, and start and end parameters.</summary>
    private static Point[] Ellipse(Entity entity)
    {
        Point major = entity.At(11);
        double radius = Math.Sqrt(major.X * major.X + major.Y * major.Y);
        double start = entity.Number(41), end = entity.Number(42, 2 * Math.PI);
        double sweep = end - start;
        sweep = Turn(sweep);
        return LineDrawing.Arc(entity.At(), radius, radius * entity.Number(40, 1), Math.Atan2(major.Y, major.X), start, sweep);
    }

    /// <summary>The vertices in order; a bulge (code 42) after a vertex curves the segment that leaves it.</summary>
    private static Point[] Polyline(Entity entity)
    {
        var vertices = new List<(Point At, double Bulge)>();
        double x = 0;
        foreach (var (code, value) in entity.Pairs)
        {
            double number = double.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out double parsed) ? parsed : 0;
            if (code == 10)
            {
                x = number;
            }
            else if (code == 20)
            {
                vertices.Add((new Point(x, number), 0));
            }
            else if (code == 42 && vertices.Count > 0)
            {
                vertices[^1] = (vertices[^1].At, number);
            }
        }
        bool closed = ((int)entity.Number(70) & 1) == 1;
        if (closed && vertices.Count > 1)
        {
            vertices.Add((vertices[0].At, 0));
        }
        var points = new List<Point>();
        for (int i = 0; i < vertices.Count; i++)
        {
            var (at, bulge) = vertices[i];
            if (bulge == 0 || !double.IsFinite(bulge) || i == vertices.Count - 1)
            {
                points.Add(at);
                continue;
            }
            points.AddRange(BulgeArc(at, vertices[i + 1].At, bulge).SkipLast(1));
        }
        return [.. points];
    }

    /// <summary>A bulge is the tangent of a quarter of the arc's angle; negative runs clockwise.</summary>
    private static Point[] BulgeArc(Point from, Point to, double bulge)
    {
        double angle = 4 * Math.Atan(bulge);
        double chord = Math.Sqrt((to.X - from.X) * (to.X - from.X) + (to.Y - from.Y) * (to.Y - from.Y));
        if (chord == 0)
        {
            return [from, to];
        }
        double radius = chord / (2 * Math.Sin(Math.Abs(angle) / 2));
        double towardsCentre = Math.Atan2(to.Y - from.Y, to.X - from.X) + Math.Sign(angle) * (Math.PI / 2 - Math.Abs(angle) / 2);
        var centre = new Point(from.X + radius * Math.Cos(towardsCentre), from.Y + radius * Math.Sin(towardsCentre));
        double start = Math.Atan2(from.Y - centre.Y, from.X - centre.X);
        return LineDrawing.Arc(centre, radius, radius, 0, start, angle);
    }

    /// <summary>Through the fit points when the spline has them, else along its control points: close enough at this size.</summary>
    private static Point[] Spline(Entity entity)
    {
        bool hasFit = entity.Pairs.Any(pair => pair.Code == 11);
        int xCode = hasFit ? 11 : 10, yCode = xCode + 10;
        var points = new List<Point>();
        double x = 0;
        foreach (var (code, value) in entity.Pairs)
        {
            if (code != xCode && code != yCode)
            {
                continue;
            }
            double number = double.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out double parsed) ? parsed : 0;
            if (code == xCode)
            {
                x = number;
            }
            else
            {
                points.Add(new Point(x, number));
            }
        }
        return [.. points];
    }

    /// <summary>The saved preview is a DIB written as hex; null when there is none or it does not decode.</summary>
    private static byte[]? SavedPicture(string hex)
    {
        if (hex.Length < 2)
        {
            return null;
        }
        try
        {
            return Dib.AsBmpFile(Convert.FromHexString(hex.Length % 2 == 0 ? hex : hex[..^1]));
        }
        catch (FormatException)
        {
            return null;
        }
    }
}
