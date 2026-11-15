using System.Drawing;
using System.Globalization;
using System.Text;
using Files.Services;

namespace Files.Tests;

/// <summary>STEP models shaded: surfaces, faces laid out and filled, assemblies placed, and the picture.</summary>
public sealed class StepGeometryTests
{
    /// <summary>Writes a STEP file instance by instance, each given the next id.</summary>
    private sealed class StepText
    {
        private readonly StringBuilder _data = new();
        private int _next = 1;

        public int Add(string body)
        {
            int id = _next++;
            _data.Append(CultureInfo.InvariantCulture, $"#{id}={body};\n");
            return id;
        }

        private static string N(double value) => value.ToString("0.0###########", CultureInfo.InvariantCulture);

        public int Point(double x, double y, double z) => Add($"CARTESIAN_POINT('',({N(x)},{N(y)},{N(z)}))");

        public int Direction(double x, double y, double z) => Add($"DIRECTION('',({N(x)},{N(y)},{N(z)}))");

        public int Axes((double X, double Y, double Z) origin, (double X, double Y, double Z) z, (double X, double Y, double Z) x) =>
            Add($"AXIS2_PLACEMENT_3D('',#{Point(origin.X, origin.Y, origin.Z)},#{Direction(z.X, z.Y, z.Z)},#{Direction(x.X, x.Y, x.Z)})");

        public int Vertex(double x, double y, double z) => Add($"VERTEX_POINT('',#{Point(x, y, z)})");

        public int Line(int from, int to, (double X, double Y, double Z) start, (double X, double Y, double Z) end)
        {
            var d = (X: end.X - start.X, Y: end.Y - start.Y, Z: end.Z - start.Z);
            double length = Math.Sqrt(d.X * d.X + d.Y * d.Y + d.Z * d.Z);
            int vector = Add($"VECTOR('',#{Direction(d.X / length, d.Y / length, d.Z / length)},{N(length)})");
            int line = Add($"LINE('',#{Point(start.X, start.Y, start.Z)},#{vector})");
            return Add($"EDGE_CURVE('',#{from},#{to},#{line},.T.)");
        }

        public int Face(int surface, params (int Loop, bool Outer)[] bounds) =>
            Add($"ADVANCED_FACE('',({string.Join(",", bounds.Select(b => "#" + Add($"{(b.Outer ? "FACE_OUTER_BOUND" : "FACE_BOUND")}('',#{b.Loop},.T.)")))}),#{surface},.T.)");

        public int Loop(params (int Edge, bool Forward)[] edges) =>
            Add($"EDGE_LOOP('',({string.Join(",", edges.Select(e => "#" + Add($"ORIENTED_EDGE('',*,*,#{e.Edge},{(e.Forward ? ".T." : ".F.")})")))}))");

        public int Solid(params int[] faces) =>
            Add($"MANIFOLD_SOLID_BREP('',#{Add($"CLOSED_SHELL('',({string.Join(",", faces.Select(f => "#" + f))}))")})");

        public string Text => $"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n{_data}ENDSEC;\nEND-ISO-10303-21;\n";
    }

    /// <summary>A cube of side <paramref name="size"/> from the origin, each face counter-clockwise seen from outside.</summary>
    private static int Cube(StepText step, double size)
    {
        (double, double, double)[] corners = [.. Enumerable.Range(0, 8).Select(i => ((i & 1) * size, ((i >> 1) & 1) * size, ((i >> 2) & 1) * size))];
        int[] vertices = [.. corners.Select(c => step.Vertex(c.Item1, c.Item2, c.Item3))];
        var edges = new Dictionary<(int, int), int>();
        int[][] faces = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        var made = new List<int>();
        foreach (int[] face in faces)
        {
            var loop = new List<(int, bool)>();
            for (int k = 0; k < 4; k++)
            {
                int a = face[k], b = face[(k + 1) % 4];
                if (edges.TryGetValue((b, a), out int back))
                {
                    loop.Add((back, false));
                    continue;
                }
                int edge = step.Line(vertices[a], vertices[b], corners[a], corners[b]);
                edges[(a, b)] = edge;
                loop.Add((edge, true));
            }
            var (o, p, q) = (corners[face[0]], corners[face[1]], corners[face[3]]);
            var x = (p.Item1 - o.Item1, p.Item2 - o.Item2, p.Item3 - o.Item3);
            var y = (q.Item1 - o.Item1, q.Item2 - o.Item2, q.Item3 - o.Item3);
            var z = ((x.Item2 * y.Item3) - (x.Item3 * y.Item2), (x.Item3 * y.Item1) - (x.Item1 * y.Item3), (x.Item1 * y.Item2) - (x.Item2 * y.Item1));
            int plane = step.Add($"PLANE('',#{step.Axes(o, z, x)})");
            made.Add(step.Face(plane, (step.Loop([.. loop]), true)));
        }
        return step.Solid([.. made]);
    }

    /// <summary>A cylinder's side, radius <paramref name="radius"/> round z from 0 to <paramref name="height"/>, cut open by a seam.</summary>
    private static int CylinderSide(StepText step, double radius, double height)
    {
        int axes = step.Axes((0, 0, 0), (0, 0, 1), (1, 0, 0));
        int bottomVertex = step.Vertex(radius, 0, 0), topVertex = step.Vertex(radius, 0, height);
        int bottom = step.Add($"EDGE_CURVE('',#{bottomVertex},#{bottomVertex},#{step.Add($"CIRCLE('',#{axes},{radius})")},.T.)");
        int topAxes = step.Axes((0, 0, height), (0, 0, 1), (1, 0, 0));
        int top = step.Add($"EDGE_CURVE('',#{topVertex},#{topVertex},#{step.Add($"CIRCLE('',#{topAxes},{radius})")},.T.)");
        int seam = step.Line(bottomVertex, topVertex, (radius, 0, 0), (radius, 0, height));
        int surface = step.Add($"CYLINDRICAL_SURFACE('',#{axes},{radius})");
        // Round the bottom, up the seam, back round the top, down the seam: the side is left of this, seen from outside.
        return step.Face(surface, (step.Loop((bottom, true), (seam, true), (top, false), (seam, false)), true));
    }

    private static Mesh MeshOf(string text)
    {
        var file = new StepFile(text);
        Assert.True(file.Index(text.IndexOf("DATA;", StringComparison.Ordinal) + 5, CancellationToken.None));
        StepModel model = StepShape.Read(file, Edges(file), CancellationToken.None);
        return Assert.IsType<Mesh>(Mesher.Mesh(file, model, CancellationToken.None));
    }

    private static Func<int, Vector3[]?> Edges(StepFile file) => new StepPreview.EdgeReader(file).Points;

    private static double Area(SolidMesh mesh)
    {
        double area = 0;
        float[] p = mesh.Points;
        for (int t = 0; t + 8 < p.Length; t += 9)
        {
            var a = new Vector3(p[t], p[t + 1], p[t + 2]);
            var b = new Vector3(p[t + 3], p[t + 4], p[t + 5]);
            var c = new Vector3(p[t + 6], p[t + 7], p[t + 8]);
            area += (b - a).Cross(c - a).Length / 2;
        }
        return area;
    }

    [Fact]
    public void A_square_with_a_square_hole_is_filled_round_the_hole()
    {
        (double, double)[] points = [(0, 0), (10, 0), (10, 10), (0, 10), (3, 3), (3, 7), (7, 7), (7, 3)];

        var triangles = Triangulation.Fill(points, [0, 1, 2, 3], [[4, 5, 6, 7]])!;

        Assert.Equal(100 - 16, triangles.Sum(t => Math.Abs(Cross(points, t)) / 2), 6);
    }

    [Fact]
    public void A_concave_outline_is_filled_without_its_notch()
    {
        // An L: a 10 square less its top right 5 square, given clockwise (the fill turns it round).
        (double, double)[] points = [(0, 0), (0, 10), (5, 10), (5, 5), (10, 5), (10, 0)];

        var triangles = Triangulation.Fill(points, [0, 1, 2, 3, 4, 5], [])!;

        Assert.Equal(75, triangles.Sum(t => Math.Abs(Cross(points, t)) / 2), 6);
    }

    /// <summary>The comb's and grid's areas are what their outlines hold: nothing is filled over a notch or a hole.</summary>
    [Theory]
    [InlineData(40, 0)]
    [InlineData(0, 3)]
    [InlineData(0, 4)]
    public void Notches_and_rows_of_square_holes_are_left_unfilled(int teeth, int grid)
    {
        var points = new List<(double, double)>();
        var outer = new List<int>();
        var holes = new List<IReadOnlyList<int>>();
        double expected;
        if (teeth > 0)
        {
            // A comb: a 10-high spine with teeth 10 high and 1 wide, a gap of 1 between them.
            int Add(double x, double y) { points.Add((x, y)); return points.Count - 1; }
            outer.Add(Add(0, 0));
            outer.Add(Add(2 * teeth, 0));
            for (int k = teeth - 1; k >= 0; k--)
            {
                outer.AddRange([Add(2 * k + 2, 10), Add(2 * k + 2, 20), Add(2 * k + 1, 20), Add(2 * k + 1, 10)]);
            }
            outer.Add(Add(0, 10));
            expected = 2 * teeth * 10 + teeth * 10;
        }
        else
        {
            points.AddRange([(0, 0), (100, 0), (100, 60), (0, 60)]);
            outer.AddRange([0, 1, 2, 3]);
            for (int i = 0; i < grid; i++)
            {
                for (int j = 0; j < grid; j++)
                {
                    double x = 10 + i * 25, y = 5 + j * 15;
                    int first = points.Count;
                    points.AddRange([(x, y), (x + 10, y), (x + 10, y + 5), (x, y + 5)]);
                    holes.Add([first, first + 1, first + 2, first + 3]);
                }
            }
            expected = 6000 - grid * grid * 50;
        }
        (double X, double Y)[] all = [.. points];

        var triangles = Triangulation.Fill(all, outer, holes)!;

        Assert.Equal(expected, triangles.Sum(t => Math.Abs(Cross(all, t)) / 2), 6);
    }

    [Fact]
    public void An_outline_that_crosses_itself_everywhere_ends_quickly()
    {
        var random = new Random(7);
        (double X, double Y)[] points = [.. Enumerable.Range(0, 10_000).Select(_ => (random.NextDouble(), random.NextDouble()))];
        var clock = System.Diagnostics.Stopwatch.StartNew();

        Triangulation.Fill(points, [.. Enumerable.Range(0, points.Length)], []);

        // Well under a second here; CI's coverage instrumentation makes it many times slower.
        Assert.True(clock.Elapsed < TimeSpan.FromSeconds(30), $"took {clock.Elapsed}");
    }

    /// <summary>Every 30° of the turn round z has some of the mesh in it: no wedge is left out.</summary>
    private static void AssertAllRound(SolidMesh mesh)
    {
        var seen = new bool[12];
        float[] p = mesh.Points;
        for (int t = 0; t + 8 < p.Length; t += 9)
        {
            double x = (p[t] + p[t + 3] + p[t + 6]) / 3, y = (p[t + 1] + p[t + 4] + p[t + 7]) / 3;
            seen[(int)((Math.Atan2(y, x) + Math.PI) / (2 * Math.PI) * 12) % 12] = true;
        }
        Assert.All(seen, Assert.True);
    }

    [Fact]
    public void A_cylinder_between_two_rims_with_no_seam_is_filled_all_the_way_round()
    {
        var step = new StepText();
        int axes = step.Axes((0, 0, 0), (0, 0, 1), (1, 0, 0));
        int topAxes = step.Axes((0, 0, 10), (0, 0, 1), (1, 0, 0));
        int bottomVertex = step.Vertex(5, 0, 0), topVertex = step.Vertex(5, 0, 10);
        int bottom = step.Add($"EDGE_CURVE('',#{bottomVertex},#{bottomVertex},#{step.Add($"CIRCLE('',#{axes},5.)")},.T.)");
        int top = step.Add($"EDGE_CURVE('',#{topVertex},#{topVertex},#{step.Add($"CIRCLE('',#{topAxes},5.)")},.T.)");
        int surface = step.Add($"CYLINDRICAL_SURFACE('',#{axes},5.)");
        step.Solid(step.Face(surface, (step.Loop((bottom, true)), true), (step.Loop((top, false)), false)));

        Mesh mesh = MeshOf(step.Text);

        double prism = 48 * 2 * 5 * Math.Sin(Math.PI / 48) * 10;
        Assert.InRange(Area(mesh.Solids[0]), prism - 1e-6, 2 * Math.PI * 5 * 10 * 1.02);
        AssertAllRound(mesh.Solids[0]);
    }

    [Fact]
    public void A_cone_bounded_by_its_base_is_filled_to_its_tip()
    {
        var step = new StepText();
        int axes = step.Axes((0, 0, 0), (0, 0, 1), (1, 0, 0));
        int vertex = step.Vertex(5, 0, 0);
        int rim = step.Add($"EDGE_CURVE('',#{vertex},#{vertex},#{step.Add($"CIRCLE('',#{axes},5.)")},.T.)");
        // Radius 5 at the base, narrowing upwards (a negative semi-angle) to a tip 5 up.
        int cone = step.Add($"CONICAL_SURFACE('',#{axes},5.,{(-Math.PI / 4).ToString(CultureInfo.InvariantCulture)})");
        step.Solid(step.Face(cone, (step.Loop((rim, true)), true)));

        Mesh mesh = MeshOf(step.Text);

        // The side of a cone of radius 5 and slant 5√2, a little less for its 48-sided base.
        Assert.InRange(Area(mesh.Solids[0]), Math.PI * 5 * 5 * Math.Sqrt(2) * 0.99, Math.PI * 5 * 5 * Math.Sqrt(2) * 1.02);
        AssertAllRound(mesh.Solids[0]);
    }

    [Fact]
    public void Solids_that_share_a_shell_are_read_once()
    {
        var step = new StepText();
        int first = Cube(step, 10);
        string text = step.Text;
        string shell = System.Text.RegularExpressions.Regex.Match(text, $@"#{first}=MANIFOLD_SOLID_BREP\('',(#\d+)\)").Groups[1].Value;
        var copies = new StringBuilder();
        for (int k = 0; k < 3000; k++)
        {
            copies.Append(CultureInfo.InvariantCulture, $"#{100_000 + k}=MANIFOLD_SOLID_BREP('',{shell});\n");
        }
        text = text.Replace("ENDSEC;\nEND-ISO", copies + "ENDSEC;\nEND-ISO", StringComparison.Ordinal);
        var file = new StepFile(text);
        file.Index(text.IndexOf("DATA;", StringComparison.Ordinal) + 5, CancellationToken.None);

        StepModel model = StepShape.Read(file, Edges(file), CancellationToken.None);

        // One solid, and drawn once: the copies are all in the same place.
        Assert.Single(model.Solids);
        Assert.Single(model.Placements);
    }

    private static double Cross((double X, double Y)[] p, (int A, int B, int C) t) =>
        ((p[t.B].X - p[t.A].X) * (p[t.C].Y - p[t.A].Y)) - ((p[t.B].Y - p[t.A].Y) * (p[t.C].X - p[t.A].X));

    public static TheoryData<string> Surfaces => ["cylinder", "cone", "sphere", "torus"];

    [Theory]
    [MemberData(nameof(Surfaces))]
    public void A_point_on_a_surface_is_found_where_it_was_put(string kind)
    {
        var at = new Frame(new Vector3(1, 2, 3), new Vector3(0, 1, 0), new Vector3(0, 0, 1), new Vector3(1, 0, 0));
        Surface surface = kind switch
        {
            "cylinder" => new Cylinder(at, 4),
            "cone" => Cone.From(at, 4, 0.3)!,
            "sphere" => new Sphere(at, 4),
            _ => new Torus(at, 6, 2),
        };

        (double u, double v) = surface.Find(surface.At(1.1, 0.7));

        Assert.Equal(1.1, u, 9);
        Assert.Equal(0.7, v, 9);
    }

    [Fact]
    public void A_bilinear_b_spline_patch_meets_its_corners_and_averages_them_in_the_middle()
    {
        var step = new StepText();
        int[] p = [step.Point(0, 0, 0), step.Point(0, 10, 0), step.Point(10, 0, 0), step.Point(10, 10, 4)];
        int id = step.Add($"B_SPLINE_SURFACE_WITH_KNOTS('',1,1,((#{p[0]},#{p[1]}),(#{p[2]},#{p[3]})),.UNSPECIFIED.,.F.,.F.,.F.,(2,2),(2,2),(0.,1.),(0.,1.),.UNSPECIFIED.)");
        var file = new StepFile(step.Text);
        file.Index(0, CancellationToken.None);

        Surface surface = Surface.From(file, file.Get(id))!;

        Assert.Equal(new Vector3(10, 10, 4), surface.At(1, 1));
        Assert.Equal(new Vector3(5, 5, 1), surface.At(0.5, 0.5));
        (double u, double v) = surface.Find(new Vector3(5, 5, 1));
        Assert.Equal(0.5, u, 2);
        Assert.Equal(0.5, v, 2);
    }

    [Fact]
    public void A_cube_is_filled_face_by_face()
    {
        var step = new StepText();
        Cube(step, 10);

        Mesh mesh = MeshOf(step.Text);

        Assert.Equal(600, Area(Assert.Single(mesh.Solids)), 6);
        Assert.Equal(12, mesh.Solids[0].Edges.Count);
    }

    [Fact]
    public void A_cylinder_cut_open_by_a_seam_is_filled_all_the_way_round()
    {
        var step = new StepText();
        step.Solid(CylinderSide(step, 5, 10));

        Mesh mesh = MeshOf(step.Text);

        // From the side of the 48-sided prism its rims are drawn as to just over the round side: triangles split onto
        // the surface zigzag a little, which adds a sliver of area (as Schwarz's lantern does). Two turns would double it.
        double prism = 48 * 2 * 5 * Math.Sin(Math.PI / 48) * 10;
        Assert.InRange(Area(Assert.Single(mesh.Solids)), prism - 1e-6, 2 * Math.PI * 5 * 10 * 1.02);
    }

    [Fact]
    public void A_whole_sphere_and_a_hemisphere_are_filled_and_the_hemisphere_on_its_side()
    {
        var step = new StepText();
        int axes = step.Axes((0, 0, 0), (0, 0, 1), (1, 0, 0));
        int sphere = step.Add($"SPHERICAL_SURFACE('',#{axes},3.)");
        step.Add($"MANIFOLD_SOLID_BREP('',#{step.Add($"CLOSED_SHELL('',(#{step.Add($"ADVANCED_FACE('',(),#{sphere},.T.)")}))")})");
        int vertex = step.Vertex(3, 0, 0);
        int equator = step.Add($"EDGE_CURVE('',#{vertex},#{vertex},#{step.Add($"CIRCLE('',#{axes},3.)")},.T.)");
        // Counter-clockwise seen from above: the face on its left, seen from outside, is the top half.
        step.Solid(step.Face(sphere, (step.Loop((equator, true)), true)));

        Mesh mesh = MeshOf(step.Text);

        Assert.Equal(4 * Math.PI * 9, Area(mesh.Solids[0]), 0);
        // All the way round: within a little of the round half (its rim has 48 straight sides; split triangles zigzag).
        Assert.InRange(Area(mesh.Solids[1]), 2 * Math.PI * 9 * 0.99, 2 * Math.PI * 9 * 1.02);
        AssertAllRound(mesh.Solids[1]);
        float[] points = mesh.Solids[1].Points;
        Assert.All(Enumerable.Range(0, points.Length / 3), i => Assert.True(points[3 * i + 2] > -1e-4));
    }

    [Fact]
    public void An_assembly_places_each_copy_of_a_part_where_it_goes()
    {
        var step = new StepText();
        int cube = Cube(step, 10);
        int partOrigin = step.Axes((0, 0, 0), (0, 0, 1), (1, 0, 0));
        int part = step.Add($"ADVANCED_BREP_SHAPE_REPRESENTATION('',(#{cube},#{partOrigin}),$)");
        int first = step.Axes((0, 0, 0), (0, 0, 1), (1, 0, 0));
        // The second copy 20 along x and turned a quarter round z.
        int second = step.Axes((20, 0, 0), (0, 0, 1), (0, 1, 0));
        int assembly = step.Add($"SHAPE_REPRESENTATION('',(#{first},#{second}),$)");
        foreach (int place in new[] { first, second })
        {
            // As SOLIDWORKS writes it: the assembly first. With no product structure, the side holding a solid is the part.
            int move = step.Add($"ITEM_DEFINED_TRANSFORMATION('','',#{place},#{partOrigin})");
            step.Add($"(REPRESENTATION_RELATIONSHIP('','',#{assembly},#{part})REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#{move})SHAPE_REPRESENTATION_RELATIONSHIP())");
        }

        Mesh mesh = MeshOf(step.Text);

        Assert.Equal(2, mesh.Placements.Count);
        Assert.Contains(mesh.Placements, placed => placed.At.Point(new Vector3(10, 0, 0)) == new Vector3(10, 0, 0));
        Assert.Contains(mesh.Placements, placed => (placed.At.Point(new Vector3(10, 0, 0)) - new Vector3(20, 10, 0)).Length < 1e-9);
        Assert.Single(mesh.Placements.Select(placed => placed.Part).Distinct());
    }

    [Fact]
    public void Placements_that_go_round_in_a_circle_end()
    {
        var step = new StepText();
        int cube = Cube(step, 1);
        int a = step.Axes((0, 0, 0), (0, 0, 1), (1, 0, 0));
        int part = step.Add($"ADVANCED_BREP_SHAPE_REPRESENTATION('',(#{cube},#{a}),$)");
        int one = step.Add($"SHAPE_REPRESENTATION('',(#{a}),$)");
        int two = step.Add($"SHAPE_REPRESENTATION('',(#{a}),$)");
        foreach ((int parent, int child) in new[] { (one, two), (two, one), (one, part) })
        {
            int move = step.Add($"ITEM_DEFINED_TRANSFORMATION('','',#{a},#{a})");
            step.Add($"(REPRESENTATION_RELATIONSHIP('','',#{child},#{parent})REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#{move})SHAPE_REPRESENTATION_RELATIONSHIP())");
        }

        Mesh mesh = MeshOf(step.Text);

        Assert.InRange(mesh.Placements.Count, 1, StepShape.MaxPlacements);
    }

    [Fact]
    public void A_cube_is_drawn_shaded_with_its_faces_told_apart_and_its_edges_inked()
    {
        using var temp = new TempFolder();
        var step = new StepText();
        Cube(step, 10);
        string path = temp.File("cube.step", step.Text);

        byte[] png = StepPreview.Render(path, 300, CancellationToken.None)!.Png!;

        using var picture = new Bitmap(new MemoryStream(png));
        // The top face's middle: the edge behind it, which a wireframe draws there, is hidden.
        Color top = picture.GetPixel(picture.Width / 2, picture.Height / 4);
        Assert.NotEqual(Color.White.ToArgb(), top.ToArgb());
        Assert.True(top.R > 60, "a face, not an edge");
        // Seen from the corner, the top face and the two sides are three shades.
        var shades = new HashSet<int>
        {
            picture.GetPixel(picture.Width / 2, picture.Height / 4).ToArgb(),
            picture.GetPixel(picture.Width / 3, picture.Height * 2 / 3).ToArgb(),
            picture.GetPixel(picture.Width * 2 / 3, picture.Height * 2 / 3).ToArgb(),
        };
        Assert.Equal(3, shades.Count);
        Assert.True(Dark(picture), "the edges are drawn");
    }

    private static bool Dark(Bitmap picture)
    {
        for (int y = 0; y < picture.Height; y++)
        {
            for (int x = 0; x < picture.Width; x++)
            {
                Color c = picture.GetPixel(x, y);
                if (c.R < 70 && c.G < 70 && c.B < 80)
                {
                    return true;
                }
            }
        }
        return false;
    }

    [Fact]
    public void A_model_with_no_surface_this_app_knows_is_drawn_as_a_wireframe()
    {
        using var temp = new TempFolder();
        var step = new StepText();
        Cube(step, 10);
        string text = step.Text.Replace("PLANE(", "SURFACE_OF_LINEAR_EXTRUSION(", StringComparison.Ordinal);
        string path = temp.File("odd.step", text);

        StepPicture picture = StepPreview.Render(path, 300, CancellationToken.None)!;

        using var drawn = new Bitmap(new MemoryStream(picture.Png!));
        // Inside the top face, which a shaded cube would fill.
        Assert.Equal(Color.White.ToArgb(), drawn.GetPixel(drawn.Width * 58 / 100, drawn.Height * 3 / 10).ToArgb());
        Assert.True(Dark(drawn));
    }
}
