namespace Files.Services;

/// <summary>A face's boundary, in the order that keeps the face on its left as seen from its surface's normal side.</summary>
internal sealed record Loop(Vector3[] Points);

/// <summary>A face: its surface (null when the face has none this app knows) and its boundaries.</summary>
/// <param name="SameSense">False when the face's normal is the surface's turned round, which turns its left and right too.</param>
/// <param name="Edges">The ids of its edges, so a solid lists each of its edges once.</param>
internal sealed record Face(Instance? Surface, IReadOnlyList<Loop> Loops, bool SameSense, IReadOnlyList<int> Edges);

/// <summary>A solid where it was modelled: its faces, and each of its edges once.</summary>
internal sealed record Solid(IReadOnlyList<Face> Faces, IReadOnlyList<Vector3[]> Edges);

/// <summary>A model's solids, and where each copy of one goes (an assembly can place a part many times).</summary>
internal sealed record StepModel(IReadOnlyList<Solid> Solids, IReadOnlyList<Placement> Placements, bool TooLarge = false);

/// <summary>One copy of a solid, where it goes, and the part it belongs to (its representation; −1 for a solid none holds).</summary>
internal readonly record struct Placement(int Solid, Frame At, int Part);

/// <summary>
/// Reads a STEP model's solids face by face, and places them as its assembly does. A representation is a
/// part when it holds solids and an assembly when others are placed in it; SHAPE_REPRESENTATION_RELATIONSHIP
/// makes two representations one, and REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION places one in another.
/// Solids that share their shells are one solid, and a face is read once however many shells hold it, so a
/// file cannot multiply its work by sharing; past <see cref="MaxFaces"/> or <see cref="MaxPoints"/> it is too large.
/// </summary>
internal static class StepShape
{
    public const int MaxPlacements = 2_000;
    public const int MaxFaces = 200_000;
    public const int MaxPoints = 5_000_000;
    private const int MaxNesting = 32;
    private const int MaxLoopPoints = 50_000;

    public static StepModel Read(StepFile file, Func<int, Vector3[]?> edge, CancellationToken cancel)
    {
        var reader = new Reader(file, edge, cancel);
        var solids = new List<Solid>();
        var solidIndex = new Dictionary<int, int>();
        var byShells = new Dictionary<string, int>();
        foreach (int id in file.Named("MANIFOLD_SOLID_BREP").Concat(file.Named("BREP_WITH_VOIDS")).Concat(file.Named("SHELL_BASED_SURFACE_MODEL")))
        {
            cancel.ThrowIfCancellationRequested();
            List<object?> shells = Shells(file.Get(id));
            string key = string.Join(",", shells.OfType<Ref>().Select(shell => shell.Id));
            if (byShells.TryGetValue(key, out int same))
            {
                solidIndex[id] = same;
                continue;
            }
            if (reader.Solid(shells) is Solid solid)
            {
                byShells[key] = solidIndex[id] = solids.Count;
                solids.Add(solid);
            }
            if (reader.TooLarge)
            {
                return new StepModel([], [], TooLarge: true);
            }
        }
        return new StepModel(solids, solids.Count == 0 ? [] : new Assembly(file, solidIndex, cancel).Placements());
    }

    private static List<object?> Shells(Instance? solid) => solid switch
    {
        { Name: "MANIFOLD_SOLID_BREP", Args.Count: >= 2 } => [solid.Args[1]],
        { Name: "BREP_WITH_VOIDS", Args.Count: >= 3 } => [solid.Args[1], .. StepFile.Items(solid.Args[2])],
        { Name: "SHELL_BASED_SURFACE_MODEL", Args.Count: >= 2 } => [.. StepFile.Items(solid.Args[1])],
        _ => [],
    };

    private sealed class Reader(StepFile file, Func<int, Vector3[]?> edge, CancellationToken cancel)
    {
        private readonly Dictionary<int, Face?> _faces = [];
        private long _points;

        public bool TooLarge { get; private set; }

        public Solid? Solid(List<object?> shells)
        {
            var faces = new List<Face>();
            var edgeIds = new HashSet<int>();
            var edges = new List<Vector3[]>();
            foreach (object? reference in shells)
            {
                Instance? shell = file.Get(reference);
                if (shell is { Name: "ORIENTED_CLOSED_SHELL" or "ORIENTED_OPEN_SHELL", Args.Count: >= 3 })
                {
                    shell = file.Get(shell.Args[2]);
                }
                if (shell is not { Name: "CLOSED_SHELL" or "OPEN_SHELL", Args.Count: >= 2 })
                {
                    continue;
                }
                foreach (object? face in StepFile.Items(shell.Args[1]))
                {
                    cancel.ThrowIfCancellationRequested();
                    if (face is not Ref(int faceId) || Face(faceId) is not Face read)
                    {
                        continue;
                    }
                    faces.Add(read);
                    foreach (int id in read.Edges)
                    {
                        if (edgeIds.Add(id) && edge(id) is Vector3[] points && Spend(points.Length))
                        {
                            edges.Add(points);
                        }
                    }
                    if (TooLarge)
                    {
                        return null;
                    }
                }
            }
            return faces.Count > 0 ? new Solid(faces, edges) : null;
        }

        /// <summary>Counts points against the whole file's budget; false (and too large) past it.</summary>
        private bool Spend(long points)
        {
            _points += points;
            TooLarge |= _points > MaxPoints || _faces.Count > MaxFaces;
            return !TooLarge;
        }

        private Face? Face(int id)
        {
            if (!_faces.TryGetValue(id, out Face? face))
            {
                face = Read(file.Get(id));
                _faces[id] = face;
                Spend(face?.Loops.Sum(loop => loop.Points.Length) ?? 0);
            }
            return face;
        }

        private Face? Read(Instance? face)
        {
            if (face is not { Name: "ADVANCED_FACE" or "FACE_SURFACE", Args.Count: >= 4 })
            {
                return null;
            }
            var loops = new List<Loop>();
            var edgeIds = new List<int>();
            foreach (object? reference in StepFile.Items(face.Args[1]))
            {
                if (file.Get(reference) is not { Name: "FACE_BOUND" or "FACE_OUTER_BOUND", Args.Count: >= 3 } bound
                    || file.Get(bound.Args[1]) is not { Name: "EDGE_LOOP", Args.Count: >= 2 } loop)
                {
                    // A vertex loop (the tip of a cone) bounds nothing that shading needs.
                    continue;
                }
                var points = new List<Vector3>();
                foreach (object? item in StepFile.Items(loop.Args[1]))
                {
                    if (file.Get(item) is not { Name: "ORIENTED_EDGE", Args.Count: >= 5 } oriented || oriented.Args[3] is not Ref(int edgeId))
                    {
                        continue;
                    }
                    edgeIds.Add(edgeId);
                    if (edge(edgeId) is not Vector3[] along)
                    {
                        continue;
                    }
                    if (points.Count + along.Length > MaxLoopPoints)
                    {
                        return null;
                    }
                    IEnumerable<Vector3> walked = oriented.Args[4] as string == ".F." ? along.Reverse() : along;
                    // Each edge starts where the one before it ended: that point is not repeated (unless an edge between could not be read).
                    bool follows = points.Count > 0 && (points[^1] - (oriented.Args[4] as string == ".F." ? along[^1] : along[0])).Length < 1e-9;
                    points.AddRange(follows ? walked.Skip(1) : walked);
                }
                if (points.Count < 2)
                {
                    continue;
                }
                if (bound.Args[2] as string == ".F.")
                {
                    points.Reverse();
                }
                loops.Add(new Loop([.. points]));
            }
            return new Face(file.Get(face.Args[2]), loops, face.Args[3] as string != ".F.", edgeIds);
        }
    }
    /// <summary>The assembly tree: which representations hold which solids, and which are placed in which.</summary>
    private sealed class Assembly
    {
        private readonly StepFile _file;
        private readonly Dictionary<int, int> _group = [];
        private readonly Dictionary<int, List<int>> _solids = [];
        private readonly Dictionary<int, List<(int Child, Frame At)>> _children = [];
        private readonly HashSet<int> _placedSolids = [];
        private readonly List<Placement> _placements = [];
        private int _visits;

        public Assembly(StepFile file, Dictionary<int, int> solidIndex, CancellationToken cancel)
        {
            _file = file;
            _cancel = cancel;
            foreach (int id in file.Named("SHAPE_REPRESENTATION_RELATIONSHIP"))
            {
                Check();
                if (file.Get(id) is { Args.Count: >= 4 } link && link.Args[2] is Ref(int a) && link.Args[3] is Ref(int b))
                {
                    Join(a, b);
                }
            }
            foreach (int rep in file.Representations)
            {
                Check();
                foreach (object? item in StepFile.Items(file.Get(rep)?.Args.ElementAtOrDefault(1)))
                {
                    if (item is Ref(int id) && solidIndex.TryGetValue(id, out int solid))
                    {
                        Add(_solids, Group(rep), solid);
                    }
                }
            }
            ReadPlacements();
            AllSolids = solidIndex.Values.Distinct();
        }

        private readonly CancellationToken _cancel;
        private int _checks;

        private void Check()
        {
            if ((++_checks & 0xFFF) == 0)
            {
                _cancel.ThrowIfCancellationRequested();
            }
        }

        private IEnumerable<int> AllSolids { get; }

        public List<Placement> Placements()
        {
            HashSet<int> children = [.. _children.Values.SelectMany(list => list).Select(child => child.Child)];
            foreach (int root in _solids.Keys.Concat(_children.Keys).Distinct().Where(group => !children.Contains(group)))
            {
                Place(root, Frame.Identity, 0);
            }
            // A solid no representation holds, or one only a cycle of placements reaches, is drawn where it was modelled.
            foreach (int solid in AllSolids)
            {
                if (!_placedSolids.Contains(solid) && _placements.Count < MaxPlacements)
                {
                    _placements.Add(new Placement(solid, Frame.Identity, Part: -1));
                }
            }
            return _placements;
        }

        private void Place(int group, Frame at, int depth)
        {
            // Both bounds matter: a cycle of empty assemblies places nothing, but would branch without end.
            if (depth > MaxNesting || ++_visits > MaxPlacements * 4)
            {
                return;
            }
            foreach (int solid in _solids.GetValueOrDefault(group) ?? [])
            {
                if (_placements.Count >= MaxPlacements)
                {
                    return;
                }
                _placements.Add(new Placement(solid, at, group));
                _placedSolids.Add(solid);
            }
            foreach ((int child, Frame move) in _children.GetValueOrDefault(group) ?? [])
            {
                Place(child, at.After(move), depth + 1);
            }
        }

        /// <summary>
        /// Each placement of one representation in another. Exporters disagree on which of the two is the
        /// parent, so it is taken from the product structure (the usage's relating product) when the file has it.
        /// </summary>
        private void ReadPlacements()
        {
            Dictionary<int, int> usageOf = UsagesByRelationship();
            Dictionary<int, HashSet<int>> groupsOfProduct = GroupsByProduct();
            foreach (int id in _file.Named(""))
            {
                Check();
                Instance? complex = _file.Get(id);
                if (complex?.Part("REPRESENTATION_RELATIONSHIP") is not { Args.Count: >= 4 } relation
                    || complex.Part("REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION") is not { Args.Count: >= 1 } moved
                    || relation.Args[2] is not Ref(int rep1) || relation.Args[3] is not Ref(int rep2)
                    || _file.Get(moved.Args[0]) is not { Name: "ITEM_DEFINED_TRANSFORMATION", Args.Count: >= 4 } transform
                    || _file.Placement(transform.Args[2]) is not Frame frame1 || _file.Placement(transform.Args[3]) is not Frame frame2)
                {
                    continue;
                }
                int group1 = Group(rep1), group2 = Group(rep2);
                if (group1 == group2)
                {
                    continue;
                }
                HashSet<int>? parents = usageOf.TryGetValue(id, out int parentProduct) ? groupsOfProduct.GetValueOrDefault(parentProduct) : null;
                // Without the product structure (or when it names neither side): a side that holds solids is a part,
                // so the other is the assembly; otherwise rep_2 is, as recommended.
                bool firstIsParent = parents?.Contains(group1) == true
                    || (parents?.Contains(group2) != true && _solids.ContainsKey(group2) && !_solids.ContainsKey(group1));
                // Transform item 1 is in rep_1, item 2 in rep_2: the child's frame is moved onto the parent's.
                (int parent, int child, Frame move) = firstIsParent
                    ? (group1, group2, frame1.After(frame2.Inverse))
                    : (group2, group1, frame2.After(frame1.Inverse));
                Add(_children, parent, (child, move));
            }
        }

        /// <summary>Placement relationship id → the product its usage is relating (the assembly), through CONTEXT_DEPENDENT_SHAPE_REPRESENTATION.</summary>
        private Dictionary<int, int> UsagesByRelationship()
        {
            var usages = new Dictionary<int, int>();
            foreach (int id in _file.Named("CONTEXT_DEPENDENT_SHAPE_REPRESENTATION"))
            {
                Check();
                if (_file.Get(id) is { Args.Count: >= 2 } context && context.Args[0] is Ref(int relation)
                    && _file.Get(context.Args[1]) is { Name: "PRODUCT_DEFINITION_SHAPE", Args.Count: >= 3 } shape
                    && _file.Get(shape.Args[2]) is { Name: "NEXT_ASSEMBLY_USAGE_OCCURRENCE", Args.Count: >= 5 } usage
                    && usage.Args[3] is Ref(int relating))
                {
                    usages[relation] = relating;
                }
            }
            return usages;
        }

        /// <summary>Product definition id → the representation groups that are its shape.</summary>
        private Dictionary<int, HashSet<int>> GroupsByProduct()
        {
            var groups = new Dictionary<int, HashSet<int>>();
            foreach (int id in _file.Named("SHAPE_DEFINITION_REPRESENTATION"))
            {
                Check();
                if (_file.Get(id) is { Args.Count: >= 2 } definition && definition.Args[1] is Ref(int rep)
                    && _file.Get(definition.Args[0]) is { Name: "PRODUCT_DEFINITION_SHAPE", Args.Count: >= 3 } shape
                    && shape.Args[2] is Ref(int product))
                {
                    if (!groups.TryGetValue(product, out HashSet<int>? set))
                    {
                        groups[product] = set = [];
                    }
                    set.Add(Group(rep));
                }
            }
            return groups;
        }

        private static void Add<T>(Dictionary<int, List<T>> lists, int key, T value)
        {
            if (!lists.TryGetValue(key, out List<T>? list))
            {
                lists[key] = list = [];
            }
            list.Add(value);
        }

        /// <summary>The representation that stands for every one joined to <paramref name="rep"/> (union-find).</summary>
        private int Group(int rep)
        {
            int root = rep;
            while (_group.TryGetValue(root, out int up) && up != root)
            {
                root = up;
            }
            // Point every one on the way straight at the root, so later finds are short.
            while (_group.TryGetValue(rep, out int up) && up != root)
            {
                _group[rep] = root;
                rep = up;
            }
            return root;
        }

        private void Join(int a, int b)
        {
            int rootA = Group(a), rootB = Group(b);
            if (rootA != rootB)
            {
                _group[rootA] = rootB;
            }
        }
    }
}
