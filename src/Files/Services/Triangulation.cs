namespace Files.Services;

/// <summary>
/// Fills a polygon with holes with triangles. A port of mapbox's earcut (ISC licence, github.com/mapbox/earcut):
/// ear clipping over a linked ring, holes joined by bridges, a z-order hash so large outlines stay fast, and
/// fallbacks that cure self-crossings and split what is left. Corners are given as indices into one list of points.
/// Bounded for untrusted files: a work budget, a recursion limit and the cancellation token all end it, with no fill.
/// </summary>
internal static class Triangulation
{
    /// <summary>More corners than this are not filled; the face is left to its edges.</summary>
    public const int MaxCorners = 50_000;

    /// <summary>Corners visited, all steps together, before the fill is given up (a damaged outline can cost far more than a real one).</summary>
    private const long WorkPerCorner = 2_000;
    private const long MinWork = 2_000_000;
    private const int MaxDepth = 64;

    /// <summary>
    /// Triangles, as corner index triples, covering <paramref name="outer"/> less <paramref name="holes"/>; null when the
    /// outline is not a polygon, too large, or too costly to fill.
    /// </summary>
    public static List<(int A, int B, int C)>? Fill(IReadOnlyList<(double X, double Y)> points, IReadOnlyList<int> outer,
        IEnumerable<IReadOnlyList<int>> holes, CancellationToken cancel = default)
    {
        var used = new List<int>();
        var holeStarts = new List<int>();
        foreach (IReadOnlyList<int> ring in holes.Prepend(outer))
        {
            if (used.Count > 0)
            {
                holeStarts.Add(used.Count);
            }
            foreach (int i in ring)
            {
                if (double.IsFinite(points[i].X) && double.IsFinite(points[i].Y))
                {
                    used.Add(i);
                }
            }
            if (used.Count > MaxCorners)
            {
                return null;
            }
        }
        // A hole left with no corners ends where the next begins; earcut skips it.
        holeStarts.RemoveAll(start => start >= used.Count);
        var clipper = new Earcut([.. used.Select(i => points[i])], Math.Max(MinWork, WorkPerCorner * used.Count), cancel);
        List<int>? found = clipper.Run([.. holeStarts]);
        if (found is null)
        {
            return null;
        }
        var triangles = new List<(int, int, int)>(found.Count / 3);
        for (int t = 0; t + 2 < found.Count; t += 3)
        {
            triangles.Add((used[found[t]], used[found[t + 1]], used[found[t + 2]]));
        }
        return triangles;
    }

    private sealed class Node(int i, double x, double y)
    {
        public readonly int I = i;
        public readonly double X = x, Y = y;
        public Node Prev = null!, Next = null!;
        public int Z;
        public Node? PrevZ, NextZ;
        public bool Steiner;
    }

    /// <summary>Thrown inside the clipper when its budget, depth or token ends it; Run turns it into null.</summary>
    private sealed class GiveUp : Exception;

    private sealed class Earcut((double X, double Y)[] data, long budget, CancellationToken cancel)
    {
        private readonly List<int> _triangles = [];
        private long _work;
        private int _depth;
        private double _minX, _minY, _invSize;

        public List<int>? Run(int[] holeStarts)
        {
            try
            {
                int outerEnd = holeStarts.Length > 0 ? holeStarts[0] : data.Length;
                Node? outer = LinkedList(0, outerEnd, clockwise: true);
                if (outer is null || outer.Next == outer.Prev)
                {
                    return _triangles;
                }
                if (holeStarts.Length > 0)
                {
                    outer = EliminateHoles(holeStarts, outer);
                }
                if (data.Length > 80)
                {
                    _minX = data.Min(p => p.X);
                    _minY = data.Min(p => p.Y);
                    double size = Math.Max(data.Max(p => p.X) - _minX, data.Max(p => p.Y) - _minY);
                    _invSize = size != 0 ? 32767 / size : 0;
                }
                EarcutLinked(outer, 0);
                return _triangles;
            }
            catch (GiveUp)
            {
                return null;
            }
        }

        private void Work(int amount = 1)
        {
            _work += amount;
            if (_work > budget)
            {
                throw new GiveUp();
            }
            if ((_work & 0xFFF) < amount)
            {
                if (cancel.IsCancellationRequested)
                {
                    throw new GiveUp();
                }
            }
        }

        private Node? LinkedList(int start, int end, bool clockwise)
        {
            Node? last = null;
            if (clockwise == (SignedArea(start, end) > 0))
            {
                for (int i = start; i < end; i++)
                {
                    last = Insert(i, last);
                }
            }
            else
            {
                for (int i = end - 1; i >= start; i--)
                {
                    last = Insert(i, last);
                }
            }
            if (last is not null && SamePlace(last, last.Next))
            {
                Remove(last);
                last = last.Next;
            }
            return last;
        }

        private double SignedArea(int start, int end)
        {
            double sum = 0;
            for (int i = start, j = end - 1; i < end; j = i++)
            {
                sum += (data[j].X - data[i].X) * (data[i].Y + data[j].Y);
            }
            return sum;
        }

        /// <summary>Removes repeated and in-line corners between <paramref name="start"/> and <paramref name="end"/>.</summary>
        private Node? FilterPoints(Node? start, Node? end = null)
        {
            if (start is null)
            {
                return null;
            }
            end ??= start;
            Node p = start;
            bool again;
            do
            {
                Work();
                again = false;
                if (!p.Steiner && (SamePlace(p, p.Next) || Area(p.Prev, p, p.Next) == 0))
                {
                    Remove(p);
                    p = end = p.Prev;
                    if (p == p.Next)
                    {
                        break;
                    }
                    again = true;
                }
                else
                {
                    p = p.Next;
                }
            }
            while (again || p != end);
            return end;
        }

        private void EarcutLinked(Node? ear, int pass)
        {
            if (ear is null)
            {
                return;
            }
            if (++_depth > MaxDepth)
            {
                throw new GiveUp();
            }
            try
            {
                if (pass == 0 && _invSize != 0)
                {
                    IndexCurve(ear);
                }
                Node stop = ear;
                while (ear.Prev != ear.Next)
                {
                    Node prev = ear.Prev, next = ear.Next;
                    if (_invSize != 0 ? IsEarHashed(ear) : IsEar(ear))
                    {
                        _triangles.Add(prev.I);
                        _triangles.Add(ear.I);
                        _triangles.Add(next.I);
                        Remove(ear);
                        ear = next.Next;
                        stop = next.Next;
                        continue;
                    }
                    ear = next;
                    if (ear == stop)
                    {
                        if (pass == 0)
                        {
                            EarcutLinked(FilterPoints(ear), 1);
                        }
                        else if (pass == 1)
                        {
                            EarcutLinked(CureLocalIntersections(FilterPoints(ear)!), 2);
                        }
                        else
                        {
                            SplitEarcut(ear);
                        }
                        break;
                    }
                }
            }
            finally
            {
                _depth--;
            }
        }

        private bool IsEar(Node ear)
        {
            Node a = ear.Prev, b = ear, c = ear.Next;
            if (Area(a, b, c) >= 0)
            {
                return false;
            }
            (double x0, double y0, double x1, double y1) = Box(a, b, c);
            for (Node p = c.Next; p != a; p = p.Next)
            {
                Work();
                if (p.X >= x0 && p.X <= x1 && p.Y >= y0 && p.Y <= y1 && InTriangle(a, b, c, p) && Area(p.Prev, p, p.Next) >= 0)
                {
                    return false;
                }
            }
            return true;
        }

        private bool IsEarHashed(Node ear)
        {
            Node a = ear.Prev, b = ear, c = ear.Next;
            if (Area(a, b, c) >= 0)
            {
                return false;
            }
            (double x0, double y0, double x1, double y1) = Box(a, b, c);
            int minZ = ZOrder(x0, y0), maxZ = ZOrder(x1, y1);
            Node? p = ear.PrevZ, n = ear.NextZ;
            bool Blocks(Node q) => q.X >= x0 && q.X <= x1 && q.Y >= y0 && q.Y <= y1 && q != a && q != c
                && InTriangle(a, b, c, q) && Area(q.Prev, q, q.Next) >= 0;
            while (p is not null && p.Z >= minZ && n is not null && n.Z <= maxZ)
            {
                Work(2);
                if (Blocks(p) || Blocks(n))
                {
                    return false;
                }
                p = p.PrevZ;
                n = n.NextZ;
            }
            for (; p is not null && p.Z >= minZ; p = p.PrevZ)
            {
                Work();
                if (Blocks(p))
                {
                    return false;
                }
            }
            for (; n is not null && n.Z <= maxZ; n = n.NextZ)
            {
                Work();
                if (Blocks(n))
                {
                    return false;
                }
            }
            return true;
        }

        private static (double X0, double Y0, double X1, double Y1) Box(Node a, Node b, Node c) =>
            (Math.Min(a.X, Math.Min(b.X, c.X)), Math.Min(a.Y, Math.Min(b.Y, c.Y)), Math.Max(a.X, Math.Max(b.X, c.X)), Math.Max(a.Y, Math.Max(b.Y, c.Y)));

        /// <summary>Cuts off the small triangles where the outline crosses itself next door.</summary>
        private Node CureLocalIntersections(Node start)
        {
            Node p = start;
            do
            {
                Work();
                Node a = p.Prev, b = p.Next.Next;
                if (!SamePlace(a, b) && Intersects(a, p, p.Next, b) && LocallyInside(a, b) && LocallyInside(b, a))
                {
                    _triangles.Add(a.I);
                    _triangles.Add(p.I);
                    _triangles.Add(b.I);
                    Remove(p);
                    Remove(p.Next);
                    p = start = b;
                }
                p = p.Next;
            }
            while (p != start);
            return FilterPoints(p)!;
        }

        /// <summary>Splits what is left along a diagonal inside it, and fills each half.</summary>
        private void SplitEarcut(Node start)
        {
            Node a = start;
            do
            {
                for (Node b = a.Next.Next; b != a.Prev; b = b.Next)
                {
                    Work();
                    if (a.I != b.I && IsValidDiagonal(a, b))
                    {
                        Node c = SplitPolygon(a, b);
                        Node? first = FilterPoints(a, a.Next), second = FilterPoints(c, c.Next);
                        EarcutLinked(first, 0);
                        EarcutLinked(second, 0);
                        return;
                    }
                }
                a = a.Next;
            }
            while (a != start);
        }

        private Node EliminateHoles(int[] holeStarts, Node outer)
        {
            var queue = new List<Node>();
            for (int h = 0; h < holeStarts.Length; h++)
            {
                int end = h < holeStarts.Length - 1 ? holeStarts[h + 1] : data.Length;
                if (LinkedList(holeStarts[h], end, clockwise: false) is not Node list)
                {
                    continue;
                }
                if (list == list.Next)
                {
                    list.Steiner = true;
                }
                queue.Add(Leftmost(list));
            }
            queue.Sort((a, b) => a.X.CompareTo(b.X));
            foreach (Node hole in queue)
            {
                outer = EliminateHole(hole, outer);
            }
            return outer;
        }

        private Node EliminateHole(Node hole, Node outer)
        {
            if (FindHoleBridge(hole, outer) is not Node bridge)
            {
                return outer;
            }
            Node reverse = SplitPolygon(bridge, hole);
            FilterPoints(reverse, reverse.Next);
            return FilterPoints(bridge, bridge.Next)!;
        }

        /// <summary>David Eberly's way: the nearest outline corner a ray left from the hole's leftmost corner can see.</summary>
        private Node? FindHoleBridge(Node hole, Node outer)
        {
            Node p = outer;
            double hx = hole.X, hy = hole.Y, qx = double.NegativeInfinity;
            Node? m = null;
            do
            {
                Work();
                if (hy <= p.Y && hy >= p.Next.Y && p.Next.Y != p.Y)
                {
                    double x = p.X + (hy - p.Y) * (p.Next.X - p.X) / (p.Next.Y - p.Y);
                    if (x <= hx && x > qx)
                    {
                        qx = x;
                        m = p.X < p.Next.X ? p : p.Next;
                        if (x == hx)
                        {
                            return m;
                        }
                    }
                }
                p = p.Next;
            }
            while (p != outer);
            if (m is null)
            {
                return null;
            }
            Node stop = m;
            double mx = m.X, my = m.Y, tanMin = double.PositiveInfinity;
            p = m;
            do
            {
                Work();
                if (hx >= p.X && p.X >= mx && hx != p.X
                    && InTriangle(hy < my ? hx : qx, hy, mx, my, hy < my ? qx : hx, hy, p.X, p.Y))
                {
                    double tan = Math.Abs(hy - p.Y) / (hx - p.X);
                    if (LocallyInside(p, hole) && (tan < tanMin || (tan == tanMin && (p.X > m.X || (p.X == m.X && SectorContainsSector(m, p))))))
                    {
                        m = p;
                        tanMin = tan;
                    }
                }
                p = p.Next;
            }
            while (p != stop);
            return m;
        }

        private static bool SectorContainsSector(Node m, Node p) => Area(m.Prev, m, p.Prev) < 0 && Area(p.Next, m, m.Next) < 0;

        private void IndexCurve(Node start)
        {
            Node p = start;
            do
            {
                Work();
                if (p.Z == 0)
                {
                    p.Z = ZOrder(p.X, p.Y);
                }
                p.PrevZ = p.Prev;
                p.NextZ = p.Next;
                p = p.Next;
            }
            while (p != start);
            p.PrevZ!.NextZ = null;
            p.PrevZ = null;
            SortLinked(p);
        }

        /// <summary>Simon Tatham's merge sort of the z-order links.</summary>
        private void SortLinked(Node? list)
        {
            int inSize = 1, merges;
            do
            {
                Node? p = list, tail = null;
                list = null;
                merges = 0;
                while (p is not null)
                {
                    merges++;
                    Node? q = p;
                    int pSize = 0;
                    for (int i = 0; i < inSize; i++)
                    {
                        pSize++;
                        q = q.NextZ;
                        if (q is null)
                        {
                            break;
                        }
                    }
                    int qSize = inSize;
                    while (pSize > 0 || (qSize > 0 && q is not null))
                    {
                        Work();
                        Node e;
                        if (pSize != 0 && (qSize == 0 || q is null || p!.Z <= q.Z))
                        {
                            e = p!;
                            p = p!.NextZ;
                            pSize--;
                        }
                        else
                        {
                            e = q!;
                            q = q!.NextZ;
                            qSize--;
                        }
                        if (tail is not null)
                        {
                            tail.NextZ = e;
                        }
                        else
                        {
                            list = e;
                        }
                        e.PrevZ = tail;
                        tail = e;
                    }
                    p = q;
                }
                tail!.NextZ = null;
                inSize *= 2;
            }
            while (merges > 1);
        }

        private int ZOrder(double px, double py)
        {
            int x = (int)((px - _minX) * _invSize), y = (int)((py - _minY) * _invSize);
            x = (x | (x << 8)) & 0x00FF00FF;
            x = (x | (x << 4)) & 0x0F0F0F0F;
            x = (x | (x << 2)) & 0x33333333;
            x = (x | (x << 1)) & 0x55555555;
            y = (y | (y << 8)) & 0x00FF00FF;
            y = (y | (y << 4)) & 0x0F0F0F0F;
            y = (y | (y << 2)) & 0x33333333;
            y = (y | (y << 1)) & 0x55555555;
            return x | (y << 1);
        }

        private Node Leftmost(Node start)
        {
            Node p = start, leftmost = start;
            do
            {
                Work();
                if (p.X < leftmost.X || (p.X == leftmost.X && p.Y < leftmost.Y))
                {
                    leftmost = p;
                }
                p = p.Next;
            }
            while (p != start);
            return leftmost;
        }

        private static bool InTriangle(Node a, Node b, Node c, Node p) => InTriangle(a.X, a.Y, b.X, b.Y, c.X, c.Y, p.X, p.Y);

        private static bool InTriangle(double ax, double ay, double bx, double by, double cx, double cy, double px, double py) =>
            (cx - px) * (ay - py) >= (ax - px) * (cy - py)
            && (ax - px) * (by - py) >= (bx - px) * (ay - py)
            && (bx - px) * (cy - py) >= (cx - px) * (by - py);

        private bool IsValidDiagonal(Node a, Node b) =>
            a.Next.I != b.I && a.Prev.I != b.I && !IntersectsPolygon(a, b)
            && ((LocallyInside(a, b) && LocallyInside(b, a) && MiddleInside(a, b) && (Area(a.Prev, a, b.Prev) != 0 || Area(a, b.Prev, b) != 0))
                || (SamePlace(a, b) && Area(a.Prev, a, a.Next) > 0 && Area(b.Prev, b, b.Next) > 0));

        private static double Area(Node p, Node q, Node r) => ((q.Y - p.Y) * (r.X - q.X)) - ((q.X - p.X) * (r.Y - q.Y));

        private static bool SamePlace(Node a, Node b) => a.X == b.X && a.Y == b.Y;

        private static bool Intersects(Node p1, Node q1, Node p2, Node q2)
        {
            int o1 = Math.Sign(Area(p1, q1, p2)), o2 = Math.Sign(Area(p1, q1, q2));
            int o3 = Math.Sign(Area(p2, q2, p1)), o4 = Math.Sign(Area(p2, q2, q1));
            return (o1 != o2 && o3 != o4)
                || (o1 == 0 && OnSegment(p1, p2, q1))
                || (o2 == 0 && OnSegment(p1, q2, q1))
                || (o3 == 0 && OnSegment(p2, p1, q2))
                || (o4 == 0 && OnSegment(p2, q1, q2));
        }

        private static bool OnSegment(Node p, Node q, Node r) =>
            q.X <= Math.Max(p.X, r.X) && q.X >= Math.Min(p.X, r.X) && q.Y <= Math.Max(p.Y, r.Y) && q.Y >= Math.Min(p.Y, r.Y);

        private bool IntersectsPolygon(Node a, Node b)
        {
            Node p = a;
            do
            {
                Work();
                if (p.I != a.I && p.Next.I != a.I && p.I != b.I && p.Next.I != b.I && Intersects(p, p.Next, a, b))
                {
                    return true;
                }
                p = p.Next;
            }
            while (p != a);
            return false;
        }

        private static bool LocallyInside(Node a, Node b) =>
            Area(a.Prev, a, a.Next) < 0
                ? Area(a, b, a.Next) >= 0 && Area(a, a.Prev, b) >= 0
                : Area(a, b, a.Prev) < 0 || Area(a, a.Next, b) < 0;

        private bool MiddleInside(Node a, Node b)
        {
            Node p = a;
            bool inside = false;
            double px = (a.X + b.X) / 2, py = (a.Y + b.Y) / 2;
            do
            {
                Work();
                if ((p.Y > py) != (p.Next.Y > py) && p.Next.Y != p.Y && px < ((p.Next.X - p.X) * (py - p.Y) / (p.Next.Y - p.Y)) + p.X)
                {
                    inside = !inside;
                }
                p = p.Next;
            }
            while (p != a);
            return inside;
        }

        /// <summary>Joins a and b by a diagonal, splitting the ring in two; the second ring starts at the returned copy of b.</summary>
        private static Node SplitPolygon(Node a, Node b)
        {
            var a2 = new Node(a.I, a.X, a.Y);
            var b2 = new Node(b.I, b.X, b.Y);
            Node an = a.Next, bp = b.Prev;
            a.Next = b;
            b.Prev = a;
            a2.Next = an;
            an.Prev = a2;
            b2.Next = a2;
            a2.Prev = b2;
            bp.Next = b2;
            b2.Prev = bp;
            return b2;
        }

        private Node Insert(int i, Node? last)
        {
            var p = new Node(i, data[i].X, data[i].Y);
            if (last is null)
            {
                p.Prev = p;
                p.Next = p;
            }
            else
            {
                p.Next = last.Next;
                p.Prev = last;
                last.Next.Prev = p;
                last.Next = p;
            }
            return p;
        }

        private static void Remove(Node p)
        {
            p.Next.Prev = p.Prev;
            p.Prev.Next = p.Next;
            if (p.PrevZ is not null)
            {
                p.PrevZ.NextZ = p.NextZ;
            }
            if (p.NextZ is not null)
            {
                p.NextZ.PrevZ = p.PrevZ;
            }
        }
    }
}
