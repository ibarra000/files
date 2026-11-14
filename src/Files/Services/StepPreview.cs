using System.Text;
using System.Windows;

namespace Files.Services;

/// <summary>A STEP model's picture, or why there is none.</summary>
public sealed record StepPicture(byte[]? Png, bool TooLarge);

/// <summary>
/// Previews a STEP model (ISO 10303-21) from an isometric view: shaded, with its visible edges drawn over
/// it, and an assembly's parts each where the assembly places it. A model whose faces cannot be shaded is
/// drawn as a wireframe of its edges. Lines, circles and ellipses are exact; B-spline edges follow their
/// control points, which is close at this size.
/// </summary>
public static class StepPreview
{
    private const int SegmentsPerTurn = 48;

    /// <summary>A B-spline edge with more control points than this is drawn straight between its ends.</summary>
    private const int MaxEdgePoints = 10_000;

    /// <summary>Larger files are not read (Preview checks the size first; this holds if the file grew since).</summary>
    private const long MaxBytes = 64L * 1024 * 1024;

    /// <summary>The model's picture, <paramref name="width"/> pixels wide; null when the file is not a STEP model.</summary>
    public static StepPicture? Render(string path, int width, CancellationToken cancel)
    {
        if (Open(path, cancel) is not (StepFile file, bool complete))
        {
            return null;
        }
        if (!complete)
        {
            return new StepPicture(null, TooLarge: true);
        }
        var edges = new EdgeReader(file);
        StepModel shape = StepShape.Read(file, edges.Points, cancel);
        if (shape.TooLarge)
        {
            return new StepPicture(null, TooLarge: true);
        }
        if (shape.Solids.Count > 0 && Mesher.Mesh(file, shape, cancel) is { } mesh && ShadedDrawing.Render(mesh, width, cancel) is byte[] shaded)
        {
            return new StepPicture(shaded, TooLarge: false);
        }
        LineWork work = Wireframe(file, shape, edges, cancel);
        return work.TooLarge
            ? new StepPicture(null, TooLarge: true)
            : new StepPicture(LineDrawing.Render(work, width), TooLarge: false);
    }

    /// <summary>The model's edges as lines (each part where its assembly places it); null when the file is not a STEP model.</summary>
    public static LineWork? Read(string path, CancellationToken cancel)
    {
        if (Open(path, cancel) is not (StepFile file, bool complete))
        {
            return null;
        }
        if (!complete)
        {
            var tooLarge = new LineWork();
            tooLarge.GiveUp();
            return tooLarge;
        }
        var edges = new EdgeReader(file);
        StepModel shape = StepShape.Read(file, edges.Points, cancel);
        if (shape.TooLarge)
        {
            var tooLarge = new LineWork();
            tooLarge.GiveUp();
            return tooLarge;
        }
        return Wireframe(file, shape, edges, cancel);
    }

    /// <summary>The file indexed, and whether all of it was (false: too many instances); null when it is not STEP.</summary>
    private static (StepFile File, bool Complete)? Open(string path, CancellationToken cancel)
    {
        byte[] bytes;
        using (FileStream stream = File.OpenRead(path))
        {
            if (stream.Length > MaxBytes)
            {
                return (new StepFile(""), false);
            }
            bytes = new byte[stream.Length];
            stream.ReadExactly(bytes);
        }
        cancel.ThrowIfCancellationRequested();
        string text = Encoding.Latin1.GetString(bytes);
        if (!text.StartsWith("ISO-10303-21", StringComparison.Ordinal))
        {
            return null;
        }
        int data = text.IndexOf("DATA;", StringComparison.Ordinal);
        if (data < 0)
        {
            return null;
        }
        var file = new StepFile(text);
        return (file, file.Index(data + 5, cancel));
    }

    /// <summary>The solids' edges where they are placed; a file with no solids gives every edge it holds, where it was modelled.</summary>
    private static LineWork Wireframe(StepFile file, StepModel shape, EdgeReader edges, CancellationToken cancel)
    {
        var work = new LineWork();
        if (shape.Solids.Count == 0)
        {
            foreach (int edge in file.Named("EDGE_CURVE"))
            {
                cancel.ThrowIfCancellationRequested();
                if (work.TooLarge)
                {
                    break;
                }
                if (edges.Points(edge) is Vector3[] points)
                {
                    work.Add(points.Select(p => Isometric(p)).ToArray());
                }
            }
            return work;
        }
        foreach ((int solid, Frame at, _) in shape.Placements)
        {
            foreach (Vector3[] edge in shape.Solids[solid].Edges)
            {
                cancel.ThrowIfCancellationRequested();
                if (work.TooLarge)
                {
                    return work;
                }
                work.Add(edge.Select(p => Isometric(at.Point(p))).ToArray());
            }
        }
        return work;
    }

    /// <summary>Seen from the corner where x, y and z are all positive, with z up.</summary>
    internal static Point Isometric(Vector3 p) => new((p.Y - p.X) / Math.Sqrt(2), (2 * p.Z - p.X - p.Y) / Math.Sqrt(6));

    /// <summary>How near a point is to the eye in that view: larger is nearer.</summary>
    internal static double Depth(Vector3 p) => (p.X + p.Y + p.Z) / Math.Sqrt(3);

    /// <summary>The points along each edge, read once.</summary>
    internal sealed class EdgeReader(StepFile file)
    {
        private readonly Dictionary<int, Vector3[]?> _read = [];

        /// <summary>The points along an edge from its start vertex to its end, or null when it does not lead to two points.</summary>
        public Vector3[]? Points(int id)
        {
            if (!_read.TryGetValue(id, out Vector3[]? points))
            {
                points = Read(id);
                _read[id] = points;
            }
            return points;
        }

        private Vector3[]? Read(int id)
        {
            Instance? edge = file.Get(id);
            if (edge is not { Name: "EDGE_CURVE", Args.Count: >= 5 } || Vertex(edge.Args[1]) is not Vector3 from || Vertex(edge.Args[2]) is not Vector3 to)
            {
                return null;
            }
            bool sameSense = edge.Args[4] as string != ".F.";
            Instance? curve = file.Get(edge.Args[3]);
            for (int hops = 0; hops < 4 && curve is { Name: "SURFACE_CURVE" or "SEAM_CURVE" or "BOUNDED_SURFACE_CURVE" or "TRIMMED_CURVE", Args.Count: >= 2 }; hops++)
            {
                curve = file.Get(curve.Args[1]);
            }
            // Each comes in the curve's own direction, which runs from the edge's end to its start when the sense is not the same.
            Vector3[]? points = curve switch
            {
                { Name: "CIRCLE", Args.Count: >= 3 } => Conic(curve.Args[1], StepFile.Number(curve.Args[2]), StepFile.Number(curve.Args[2]), from, to, sameSense),
                { Name: "ELLIPSE", Args.Count: >= 4 } => Conic(curve.Args[1], StepFile.Number(curve.Args[2]), StepFile.Number(curve.Args[3]), from, to, sameSense),
                _ => ControlPoints(curve) is { Length: >= 2 } controls ? controls : null,
            };
            if (points is null)
            {
                return [from, to];
            }
            if (!sameSense)
            {
                Array.Reverse(points);
            }
            bool closed = (from - to).Length < 1e-9;
            if (!closed && (points[0] - from).Length > (points[0] - to).Length)
            {
                Array.Reverse(points);
            }
            return points;
        }

        /// <summary>A B-spline's control points: the third argument of a simple one, the second of B_SPLINE_CURVE inside a complex one.</summary>
        private Vector3[]? ControlPoints(Instance? curve)
        {
            object? points = curve switch
            {
                { Name: "" } => curve.Part("B_SPLINE_CURVE")?.Args.ElementAtOrDefault(1),
                { Name: var name, Args.Count: >= 3 } when name.StartsWith("B_SPLINE_CURVE", StringComparison.Ordinal) => curve.Args[2],
                _ => null,
            };
            return points is IReadOnlyList<object?> { Count: <= MaxEdgePoints } list
                ? list.Select(file.Point3).OfType<Vector3>().ToArray()
                : null;
        }

        /// <summary>
        /// The arc of a circle or ellipse between the edge's vertices, counter-clockwise about the placement's
        /// axis, in the curve's direction: from the edge's end to its start when the sense is not the same.
        /// </summary>
        private Vector3[]? Conic(object? placement, double radiusX, double radiusY, Vector3 from, Vector3 to, bool sameSense)
        {
            if (file.Placement(placement) is not Frame axes || radiusX <= 0 || radiusY <= 0)
            {
                return null;
            }
            double Angle(Vector3 p)
            {
                Vector3 local = axes.Local(p);
                return Math.Atan2(local.Y / radiusY, local.X / radiusX);
            }
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
                points[i] = axes.Point(new Vector3(radiusX * Math.Cos(t), radiusY * Math.Sin(t), 0));
            }
            return points;
        }

        private Vector3? Vertex(object? reference) =>
            file.Get(reference) is { Name: "VERTEX_POINT", Args.Count: >= 2 } vertex ? file.Point3(vertex.Args[1]) : null;
    }
}
