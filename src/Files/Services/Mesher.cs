namespace Files.Services;

/// <summary>A solid's triangles where it was modelled: nine numbers (three corners) per triangle, with a normal at each corner.</summary>
internal sealed record SolidMesh(float[] Points, float[] Normals, IReadOnlyList<Vector3[]> Edges)
{
    public int Triangles => Points.Length / 9;
}

/// <summary>Every solid's triangles, and where each copy of one is placed.</summary>
internal sealed record Mesh(IReadOnlyList<SolidMesh> Solids, IReadOnlyList<Placement> Placements);

/// <summary>
/// Turns a model's faces into triangles. Each face's boundary is laid out on its surface's (u, v), filled
/// there (<see cref="Triangulation"/>), split until it follows the surface's curve, and lifted back. A face
/// this cannot do (an unknown surface, a boundary that will not lay out) is left unfilled; its edges still show.
/// </summary>
internal static class Mesher
{
    /// <summary>More triangles than this, counting each placed copy, take too long to draw: the preview is a wireframe.</summary>
    public const int MaxTriangles = 600_000;

    /// <summary>How many times one triangle may be halved to follow a curved surface.</summary>
    private const int MaxSplits = 12;

    /// <summary>The shaded model, or null when there is nothing to fill or too much.</summary>
    public static Mesh? Mesh(StepFile file, StepModel shape, CancellationToken cancel)
    {
        var copies = new int[shape.Solids.Count];
        foreach ((int solid, _, _) in shape.Placements)
        {
            copies[solid]++;
        }
        var meshes = new SolidMesh[shape.Solids.Count];
        long total = 0;
        for (int i = 0; i < shape.Solids.Count; i++)
        {
            if (copies[i] == 0)
            {
                meshes[i] = new SolidMesh([], [], []);
                continue;
            }
            var mesh = new MeshBuilder((MaxTriangles - total) / copies[i], cancel);
            foreach (Face face in shape.Solids[i].Faces)
            {
                cancel.ThrowIfCancellationRequested();
                if (!AddFace(file, face, mesh, cancel))
                {
                    return null;
                }
            }
            meshes[i] = mesh.Done(shape.Solids[i].Edges);
            total += (long)meshes[i].Triangles * copies[i];
        }
        return total == 0 ? null : new Mesh(meshes, shape.Placements);
    }

    /// <summary>Adds a face's triangles; false only when the model has gone past <see cref="MaxTriangles"/>.</summary>
    private static bool AddFace(StepFile file, Face face, MeshBuilder mesh, CancellationToken cancel)
    {
        if (Surface.From(file, face.Surface) is not Surface surface)
        {
            return true;
        }
        var layout = new Layout(surface, cancel);
        if (!layout.Build(face))
        {
            return true;
        }
        List<(int A, int B, int C)>? triangles = Triangulation.Fill(layout.Flat, layout.Outer, layout.Holes, cancel);
        if (triangles is null)
        {
            return true;
        }
        foreach ((int a, int b, int c) in triangles)
        {
            if (!Split(surface, layout.Corner(a), layout.Corner(b), layout.Corner(c), 0, mesh))
            {
                return false;
            }
        }
        return true;
    }

    /// <summary>Halves a triangle across its longest side (in steps of the surface's curve) until each side is short enough, then adds it.</summary>
    private static bool Split(Surface surface, Corner a, Corner b, Corner c, int depth, MeshBuilder mesh)
    {
        double ab = Steps(surface, a, b), bc = Steps(surface, b, c), ca = Steps(surface, c, a);
        double longest = Math.Max(ab, Math.Max(bc, ca));
        if (longest <= 1 || depth >= MaxSplits)
        {
            return mesh.Add(surface, a, b, c);
        }
        // Rotated so the side to halve is a–b.
        (a, b, c) = longest == ab ? (a, b, c) : longest == bc ? (b, c, a) : (c, a, b);
        var middle = new Corner((a.U + b.U) / 2, (a.V + b.V) / 2, null);
        return Split(surface, a, middle, c, depth + 1, mesh) && Split(surface, middle, b, c, depth + 1, mesh);
    }

    private static double Steps(Surface surface, Corner a, Corner b) =>
        Math.Max(Math.Abs(a.U - b.U) / surface.StepU, Math.Abs(a.V - b.V) / surface.StepV);

    /// <summary>A corner in (u, v), with its point in space when it came from the face's boundary (kept, so faces meet exactly).</summary>
    private readonly record struct Corner(double U, double V, Vector3? Point);

    private sealed class MeshBuilder(long limit, CancellationToken cancel)
    {
        private readonly List<float> _points = [];
        private readonly List<float> _normals = [];
        private long _tried;

        /// <summary>Adds a triangle; false past the limit, which counts triangles left out as damaged too (they cost as much).</summary>
        public bool Add(Surface surface, Corner a, Corner b, Corner c)
        {
            if (++_tried > 2 * limit || _points.Count / 9 >= limit)
            {
                return false;
            }
            if ((_tried & 0xFFF) == 0)
            {
                cancel.ThrowIfCancellationRequested();
            }
            Span<Vector3> points = [a.Point ?? surface.At(a.U, a.V), b.Point ?? surface.At(b.U, b.V), c.Point ?? surface.At(c.U, c.V)];
            Span<Vector3> normals = [surface.Normal(a.U, a.V), surface.Normal(b.U, b.V), surface.Normal(c.U, c.V)];
            for (int i = 0; i < 3; i++)
            {
                if (!points[i].IsFinite || !normals[i].IsFinite)
                {
                    return true; // a damaged surface: this triangle is left out
                }
            }
            for (int i = 0; i < 3; i++)
            {
                _points.AddRange([(float)points[i].X, (float)points[i].Y, (float)points[i].Z]);
                _normals.AddRange([(float)normals[i].X, (float)normals[i].Y, (float)normals[i].Z]);
            }
            return true;
        }

        public SolidMesh Done(IReadOnlyList<Vector3[]> edges) => new([.. _points], [.. _normals], edges);
    }

    /// <summary>
    /// A face's boundary laid out flat on its surface's (u, v). A surface that comes round (a cylinder) is cut
    /// open: a boundary that goes once round it is joined to the other one that does into a band, or, alone, is
    /// closed at the surface's pole (a sphere's, a cone's tip).
    /// </summary>
    private sealed class Layout(Surface surface, CancellationToken cancel)
    {
        private readonly List<Corner> _corners = [];
        private bool _swapped;

        /// <summary>The corners as plane points; when the surface comes round in v rather than u, u and v are swapped.</summary>
        public List<(double X, double Y)> Flat { get; } = [];

        public List<int> Outer { get; private set; } = [];

        public List<List<int>> Holes { get; } = [];

        public Corner Corner(int index) => _corners[index];

        public bool Build(Face face)
        {
            var rings = face.Loops.Select(Ring).Where(ring => ring.Count >= 2).ToList();
            if (rings.Count == 0)
            {
                return Whole();
            }
            var aroundU = rings.Where(ring => Turns(ring, surface.PeriodU, u: true) != 0).ToList();
            var aroundV = rings.Where(ring => Turns(ring, surface.PeriodV, u: false) != 0).ToList();
            if (aroundU.Count > 0 && aroundV.Count > 0)
            {
                return false; // round both ways (a torus cut only one way): not laid out
            }
            if (aroundV.Count > 0)
            {
                _swapped = true;
                (aroundU, aroundV) = (aroundV, aroundU);
            }
            double period = _swapped ? surface.PeriodV : surface.PeriodU;
            List<List<Corner>> closed = [.. rings.Except(aroundU)];
            List<Corner>? outside = aroundU.Count switch
            {
                0 => Largest(closed),
                1 when !_swapped => Capped(aroundU[0], period, face.SameSense),
                2 => Band(aroundU[0], aroundU[1], period),
                _ => null,
            };
            if (outside is null)
            {
                return false;
            }
            Outer = Add(outside);
            foreach (List<Corner> hole in closed.Where(ring => ring != outside))
            {
                Holes.Add(Add(Beside(hole, outside, period)));
            }
            return true;
        }

        /// <summary>A face with no boundary (a whole sphere) covers its surface's whole range, where it has one.</summary>
        private bool Whole()
        {
            if (surface.Whole is not { } whole)
            {
                return false;
            }
            (double u0, double u1, double v0, double v1) = whole;
            Outer = Add([new(u0, v0, null), new(u1, v0, null), new(u1, v1, null), new(u0, v1, null)]);
            return true;
        }

        private List<int> Add(List<Corner> ring)
        {
            var indices = new List<int>(ring.Count);
            foreach (Corner corner in ring)
            {
                indices.Add(_corners.Count);
                _corners.Add(corner);
                Flat.Add(_swapped ? (corner.V, corner.U) : (corner.U, corner.V));
            }
            return indices;
        }

        /// <summary>
        /// A loop's points in (u, v), each angle unwrapped to follow on from the one before. A point at a pole,
        /// where every u meets, is laid out twice: at the u it is reached from and the u it is left by.
        /// </summary>
        private List<Corner> Ring(Loop loop)
        {
            Vector3[] points = loop.Points;
            int count = points.Length > 1 && (points[^1] - points[0]).Length < 1e-9 ? points.Length - 1 : points.Length;
            var found = new (double U, double V)[count];
            for (int i = 0; i < count; i++)
            {
                if ((i & 0x3FF) == 0x3FF)
                {
                    cancel.ThrowIfCancellationRequested();
                }
                found[i] = surface.Find(points[i]);
            }
            if (count == 0 || Known(found, -1) is not { } before || Known(found, +1) is not { } after)
            {
                return []; // nothing but the pole
            }
            var ring = new List<Corner>(count + 4);
            for (int i = 0; i < count; i++)
            {
                (double u, double v) = found[i];
                if (!double.IsNaN(u))
                {
                    ring.Add(new(u, v, points[i]));
                    continue;
                }
                ring.Add(new(before[i], v, points[i]));
                ring.Add(new(after[i], v, points[i]));
            }
            Unwrap(ring, surface.PeriodU, u: true);
            Unwrap(ring, surface.PeriodV, u: false);
            // A loop that goes round ends where it began, a whole period on: that last step is a side of the face too
            // (without it a band or a cap would stop one step short of the full turn).
            (double U, double V) closing = (Image(ring[0].U, ring[^1].U, surface.PeriodU), Image(ring[0].V, ring[^1].V, surface.PeriodV));
            if (closing != (ring[0].U, ring[0].V))
            {
                ring.Add(new(closing.U, closing.V, ring[0].Point));
            }
            return ring;
        }

        /// <summary>The first value moved by whole periods to lie nearest the last.</summary>
        private static double Image(double first, double last, double period) =>
            period > 0 ? first + period * Math.Round((last - first) / period) : first;

        /// <summary>
        /// For each point, the u of the nearest point not at a pole in that direction round the loop (−1: before it,
        /// +1: after it); null when every point is at a pole. Two passes round, so it costs no more than the loop.
        /// </summary>
        private static double[]? Known((double U, double V)[] found, int step)
        {
            int n = found.Length;
            var known = new double[n];
            double? last = null;
            for (int k = 0; k < 2 * n; k++)
            {
                int i = step < 0 ? k % n : (2 * n - 1 - k) % n;
                if (k >= n)
                {
                    known[i] = last ?? double.NaN;
                }
                if (!double.IsNaN(found[i].U))
                {
                    last = found[i].U;
                }
            }
            return last is null ? null : known;
        }

        private static void Unwrap(List<Corner> ring, double period, bool u)
        {
            if (period <= 0)
            {
                return;
            }
            for (int i = 1; i < ring.Count; i++)
            {
                double before = u ? ring[i - 1].U : ring[i - 1].V;
                double value = u ? ring[i].U : ring[i].V;
                double moved = value + period * Math.Round((before - value) / period);
                ring[i] = u ? ring[i] with { U = moved } : ring[i] with { V = moved };
            }
        }

        /// <summary>How many times the loop goes round (its last point to its first steps back over a whole period), with its sign.</summary>
        private static int Turns(List<Corner> ring, double period, bool u)
        {
            if (period <= 0)
            {
                return 0;
            }
            double first = u ? ring[0].U : ring[0].V, last = u ? ring[^1].U : ring[^1].V;
            return (int)Math.Round((last - first) / period);
        }

        private double Along(Corner corner) => _swapped ? corner.V : corner.U;

        private List<Corner>? Largest(List<List<Corner>> rings) =>
            rings.Count == 0 ? null : rings.MaxBy(ring => Math.Abs(SignedArea(ring)));

        private double SignedArea(List<Corner> ring)
        {
            double area = 0;
            for (int i = 0, j = ring.Count - 1; i < ring.Count; j = i++)
            {
                area += (ring[j].U * ring[i].V) - (ring[i].U * ring[j].V);
            }
            return area;
        }

        /// <summary>The loop as one that goes round forwards (u rising by a period).</summary>
        private List<Corner> Forwards(List<Corner> ring, double period)
        {
            if (Turns(ring, period, !_swapped) > 0)
            {
                return ring;
            }
            List<Corner> reversed = [.. ring];
            reversed.Reverse();
            return reversed;
        }

        /// <summary>
        /// Two loops that each go once round (the two rims of a cylinder with no seam): the first forwards, then
        /// the second backwards, moved round so its start meets the first's end. The band they bound covers one turn.
        /// </summary>
        private List<Corner> Band(List<Corner> one, List<Corner> other, double period)
        {
            List<Corner> forwards = Forwards(one, period);
            List<Corner> backwards = Forwards(other, period);
            backwards.Reverse();
            double shift = period * Math.Round((Along(forwards[^1]) - Along(backwards[0])) / period);
            return [.. forwards, .. backwards.Select(corner => Moved(corner, shift))];
        }

        /// <summary>
        /// One loop that goes once round (a sphere's rim, a cone's base), closed at the pole on the face's side:
        /// the face lies left of its boundary as seen from the normal, and left of travel towards larger u is larger v.
        /// </summary>
        private List<Corner>? Capped(List<Corner> ring, double period, bool sameSense)
        {
            int side = Turns(ring, period, u: true) * (sameSense ? 1 : -1);
            if (surface.Pole(side) is not double pole)
            {
                return null;
            }
            List<Corner> forwards = Forwards(ring, period);
            return [.. forwards, new(forwards[^1].U, pole, null), new(forwards[0].U, pole, null)];
        }

        /// <summary>A hole moved round by whole turns so that it lies within the outline (each was unwrapped on its own).</summary>
        private List<Corner> Beside(List<Corner> hole, List<Corner> outside, double period)
        {
            if (period <= 0)
            {
                return hole;
            }
            double middle = (outside.Min(Along) + outside.Max(Along)) / 2;
            double centre = (hole.Min(Along) + hole.Max(Along)) / 2;
            double shift = period * Math.Round((middle - centre) / period);
            return shift == 0 ? hole : [.. hole.Select(corner => Moved(corner, shift))];
        }

        private Corner Moved(Corner corner, double shift) =>
            _swapped ? corner with { V = corner.V + shift } : corner with { U = corner.U + shift };
    }
}
