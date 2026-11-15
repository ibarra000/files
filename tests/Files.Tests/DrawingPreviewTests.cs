using System.Drawing;
using System.Drawing.Imaging;
using System.Text;
using Files.Core;
using Files.Services;
using WpfPoint = System.Windows.Point;

namespace Files.Tests;

public sealed class DrawingPreviewTests
{
    private const double Close = 1e-6;

    /// <summary>A DXF from its group codes and values, as pairs of lines.</summary>
    private static string Dxf(params (int Code, string Value)[] pairs) =>
        string.Concat(pairs.Select(pair => $"{pair.Code,3}\n{pair.Value}\n"));

    private static (int, string)[] Section(string name, params (int, string)[] body) =>
        [(0, "SECTION"), (2, name), .. body, (0, "ENDSEC")];

    private static (int, string)[] Line(double x1, double y1, double x2, double y2) =>
        [(0, "LINE"), (8, "0"), (10, $"{x1}"), (20, $"{y1}"), (30, "0"), (11, $"{x2}"), (21, $"{y2}"), (31, "0")];

    private static bool Near(WpfPoint a, double x, double y) => Math.Abs(a.X - x) < 1e-6 && Math.Abs(a.Y - y) < 1e-6;

    private static bool HasInk(byte[] png)
    {
        using var picture = new Bitmap(new MemoryStream(png));
        for (int y = 0; y < picture.Height; y++)
        {
            for (int x = 0; x < picture.Width; x++)
            {
                if (picture.GetPixel(x, y).R < 128)
                {
                    return true;
                }
            }
        }
        return false;
    }

    [Fact]
    public void A_dxf_draws_its_lines_arcs_and_the_blocks_it_inserts()
    {
        using var temp = new TempFolder();
        string path = temp.File("plan.dxf", Dxf(
        [
            .. Section("BLOCKS", [(0, "BLOCK"), (2, "BOLT"), (10, "0"), (20, "0"), .. Line(0, 0, 1, 0), (0, "ENDBLK")]),
            .. Section("ENTITIES",
            [
                .. Line(0, 0, 100, 50),
                (0, "ARC"), (10, "50"), (20, "25"), (40, "10"), (50, "0"), (51, "90"),
                (0, "INSERT"), (2, "BOLT"), (10, "10"), (20, "10"), (41, "2"), (42, "2"), (50, "90"),
            ]),
            (0, "EOF"),
        ]));

        DxfContent dxf = DxfPreview.Read(path, CancellationToken.None)!;

        Assert.Equal(3, dxf.Work.Lines.Count);
        Assert.Contains(dxf.Work.Lines, line => Near(line[0], 0, 0) && Near(line[^1], 100, 50));
        Assert.Contains(dxf.Work.Lines, line => Near(line[0], 60, 25) && Near(line[^1], 50, 35));
        // The block's unit line, doubled, turned a quarter and moved to (10, 10).
        Assert.Contains(dxf.Work.Lines, line => Near(line[0], 10, 10) && Near(line[^1], 10, 12));
        byte[] png = LineDrawing.Render(dxf.Work, 360)!;
        Assert.True(HasInk(png));
    }

    [Fact]
    public void A_dxf_polyline_follows_its_bulges_and_closes()
    {
        using var temp = new TempFolder();
        // A half circle from (0,0) to (10,0), bulging below (bulge 1 = a half turn counter-clockwise), then closed with a straight line.
        string path = temp.File("slot.dxf", Dxf(
        [
            .. Section("ENTITIES", [(0, "LWPOLYLINE"), (90, "2"), (70, "1"), (10, "0"), (20, "0"), (42, "1"), (10, "10"), (20, "0")]),
            (0, "EOF"),
        ]));

        WpfPoint[] line = Assert.Single(DxfPreview.Read(path, CancellationToken.None)!.Work.Lines);

        Assert.True(Near(line[0], 0, 0) && Near(line[^1], 0, 0));
        Assert.Contains(line, point => Near(point, 5, -5));
    }

    [Fact]
    public void A_dxf_with_only_text_and_a_saved_preview_gives_the_preview()
    {
        using var temp = new TempFolder();
        byte[] bmp = Picture(ImageFormat.Bmp);
        string hex = Convert.ToHexString(bmp[14..]); // the DIB, without the BMP file header
        string path = temp.File("sheet.dxf", Dxf(
        [
            .. Section("ENTITIES", [(0, "TEXT"), (10, "0"), (20, "0"), (40, "2.5"), (1, "TITLE")]),
            .. Section("THUMBNAILIMAGE", [(90, $"{hex.Length / 2}"), (310, hex[..40]), (310, hex[40..])]),
            (0, "EOF"),
        ]));

        DxfContent dxf = DxfPreview.Read(path, CancellationToken.None)!;

        Assert.Equal(bmp, dxf.Saved);
        Assert.Empty(dxf.Work.Lines);
        Assert.Equal("TITLE", Assert.Single(dxf.Work.Labels).Text);
    }

    [Theory]
    [InlineData("not a drawing at all")]
    [InlineData("AutoCAD Binary DXF\r\n\u001a\0\0\0")]
    [InlineData("  0\nSECTION\n  2\nENTI")]
    public void A_dxf_that_is_binary_damaged_or_not_one_draws_nothing(string contents)
    {
        using var temp = new TempFolder();
        string path = temp.File("broken.dxf", contents);

        DxfContent? dxf = DxfPreview.Read(path, CancellationToken.None);

        Assert.True(dxf is null || dxf.Work.Lines.Count == 0);
    }

    [Fact]
    public void A_dxf_with_impossible_numbers_still_finishes()
    {
        using var temp = new TempFolder();
        string path = temp.File("hostile.dxf", Dxf(
        [
            .. Section("ENTITIES",
            [
                (0, "ARC"), (10, "0"), (20, "0"), (40, "1"), (50, "0"), (51, "-1e999"),
                (0, "ELLIPSE"), (10, "0"), (20, "0"), (11, "1"), (21, "0"), (40, "0.5"), (41, "0"), (42, "-1e30"),
                (0, "LWPOLYLINE"), (10, "0"), (20, "0"), (42, "NaN"), (10, "1"), (20, "1"),
                (0, "TEXT"), (10, "0"), (20, "0"), (40, "1"), (50, "NaN"), (1, "TURNED NOWHERE"),
            ]),
            (0, "EOF"),
        ]));

        DxfContent dxf = DxfPreview.Read(path, CancellationToken.None)!;

        Assert.Empty(dxf.Work.Labels);
        Assert.All(dxf.Work.Lines, line => Assert.True(line.Length <= 49));
    }

    [Fact]
    public void Blocks_that_insert_each_other_over_and_over_are_too_large_not_endless()
    {
        using var temp = new TempFolder();
        // Eight levels of a block inserting the next one 200 times: 200^8 entities from a few thousand lines.
        var blocks = new List<(int, string)>();
        for (int level = 0; level < 8; level++)
        {
            blocks.AddRange([(0, "BLOCK"), (2, $"L{level}"), (10, "0"), (20, "0")]);
            for (int copy = 0; copy < 200; copy++)
            {
                blocks.AddRange([(0, "INSERT"), (2, $"L{level + 1}"), (10, $"{copy}"), (20, "0")]);
            }
            blocks.Add((0, "ENDBLK"));
        }
        blocks.AddRange([(0, "BLOCK"), (2, "L8"), (10, "0"), (20, "0"), (0, "HATCH"), (0, "ENDBLK")]);
        string path = temp.File("bomb.dxf", Dxf(
        [
            .. Section("BLOCKS", [.. blocks]),
            .. Section("ENTITIES", [(0, "INSERT"), (2, "L0"), (10, "0"), (20, "0")]),
            (0, "EOF"),
        ]));

        Assert.True(DxfPreview.Read(path, CancellationToken.None)!.Work.TooLarge);
    }

    [Fact]
    public void A_step_file_nested_too_deep_draws_nothing_instead_of_crashing()
    {
        using var temp = new TempFolder();
        string deep = new string('(', 100_000) + new string(')', 100_000);
        string path = temp.File("deep.step", $"ISO-10303-21;\nDATA;\n#1=EDGE_CURVE('',{deep},#2,#3,.T.);\nENDSEC;\n");

        Assert.Empty(StepPreview.Read(path, CancellationToken.None)!.Lines);
    }

    [Fact]
    public void A_plot_draws_its_pen_moves_circles_and_rectangles()
    {
        using var temp = new TempFolder();
        string path = temp.File("sheet.plt", "IN;SP1;PU0,0;PD100,0,100,100;PU;PR10,10;PD10,0;PA200,200;CI50;PU0,0;ER20,30;LBNOT DRAWN\u0003;");

        LineWork work = PltPreview.Read(path, CancellationToken.None)!;

        Assert.Contains(work.Lines, line => line.Length == 3 && Near(line[0], 0, 0) && Near(line[1], 100, 0) && Near(line[2], 100, 100));
        // Relative from (110, 110): to (120, 110), then absolute to (200, 200).
        Assert.Contains(work.Lines, line => Near(line[0], 110, 110) && Near(line[1], 120, 110) && Near(line[2], 200, 200));
        Assert.Contains(work.Lines, line => line.Length == 49 && Near(line[0], 250, 200));
        Assert.Contains(work.Lines, line => line.Length == 5 && Near(line[2], 20, 30));
        Assert.Equal(4, work.Lines.Count);
    }

    [Fact]
    public void A_plot_decodes_hpgl2_encoded_polylines()
    {
        using var temp = new TempFolder();
        // Pen up to (100, 200) absolute, then pen down 50 across and 30 down.
        string encoded = "PE<=" + Encode(100) + Encode(200) + Encode(50) + Encode(-30) + ";";
        string path = Path.Combine(temp.Path, "encoded.plt");
        File.WriteAllBytes(path, Encoding.Latin1.GetBytes("IN;" + encoded));

        WpfPoint[] line = Assert.Single(PltPreview.Read(path, CancellationToken.None)!.Lines);

        Assert.Equal(2, line.Length);
        Assert.True(Near(line[0], 100, 200) && Near(line[1], 150, 170));
    }

    [Fact]
    public void A_plot_wrapped_in_pcl_by_a_printer_driver_still_draws()
    {
        using var temp = new TempFolder();
        const string Esc = "\u001b";
        // The job header, a reset, raster data that must be skipped whole (5 bytes, one of them a letter pair), then HP-GL/2.
        string pcl = $"{Esc}%-12345X@PJL ENTER LANGUAGE=PCL\r\n{Esc}E{Esc}*b5WPD9,9{Esc}%1BIN;PU0,0;PD10,10;{Esc}%0A{Esc}E";
        string path = Path.Combine(temp.Path, "driver.plt");
        File.WriteAllBytes(path, Encoding.Latin1.GetBytes(pcl));

        WpfPoint[] line = Assert.Single(PltPreview.Read(path, CancellationToken.None)!.Lines);

        Assert.True(Near(line[0], 0, 0) && Near(line[1], 10, 10));
    }

    [Fact]
    public void A_plot_that_is_not_hpgl_draws_nothing()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "raster.plt");
        File.WriteAllBytes(path, [0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF, 0x10, 0x20]);

        Assert.Null(PltPreview.Read(path, CancellationToken.None));
    }

    private const string StepPart = """
        ISO-10303-21;
        HEADER;
        FILE_NAME('part.step','',(''),(''),'','','');
        ENDSEC;
        DATA;
        #1=CARTESIAN_POINT('',(0.,0.,0.));
        #2=CARTESIAN_POINT('',(10.,0.,0.));
        #3=VERTEX_POINT('',#1);
        #4=VERTEX_POINT('',#2);
        #5=DIRECTION('',(1.,0.,0.));
        #6=VECTOR('',#5,10.);
        #7=LINE('a ; in a name',#1,#6);
        #8=EDGE_CURVE('',#3,#4,#7,.T.);
        /* a full circle of radius 5 around the z axis, 5 up */
        #9=CARTESIAN_POINT('',(0.,0.,5.));
        #10=DIRECTION('',(0.,0.,1.));
        #11=DIRECTION('',(1.,0.,0.));
        #12=AXIS2_PLACEMENT_3D('',#9,#10,#11);
        #13=CIRCLE('',#12,5.);
        #14=CARTESIAN_POINT('',(5.,0.,5.));
        #15=VERTEX_POINT('',#14);
        #16=EDGE_CURVE('',#15,#15,
          #13,.T.);
        ENDSEC;
        END-ISO-10303-21;
        """;

    [Fact]
    public void A_step_model_draws_its_edges_in_an_isometric_view()
    {
        using var temp = new TempFolder();
        string path = temp.File("part.step", StepPart);

        LineWork work = StepPreview.Read(path, CancellationToken.None)!;

        Assert.Equal(2, work.Lines.Count);
        WpfPoint[] edge = work.Lines.Single(line => line.Length == 2);
        Assert.True(Near(edge[0], 0, 0));
        Assert.True(Near(edge[1], -10 / Math.Sqrt(2), -10 / Math.Sqrt(6)));
        WpfPoint[] circle = work.Lines.Single(line => line.Length > 2);
        Assert.True(Near(circle[0], circle[^1].X, circle[^1].Y));
        // Seen from the (1, 1, 1) corner, the circle reaches 5 to either side.
        Assert.Equal(5, circle.Max(point => point.X), Close);
        Assert.Equal(-5, circle.Min(point => point.X), Close);
    }

    [Theory]
    [InlineData("ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=EDGE_CURVE('',#2,#3,#4,.T.);\n#2=VERTEX_POINT('',")]
    [InlineData("not a model")]
    public void A_step_file_cut_short_or_not_one_draws_nothing(string contents)
    {
        using var temp = new TempFolder();
        string path = temp.File("cut.step", contents);

        LineWork? work = StepPreview.Read(path, CancellationToken.None);

        Assert.True(work is null || work.Lines.Count == 0);
    }

    [Theory]
    [InlineData("part.prt.12", "prt", FileKind.Drawing)]
    [InlineData("sheet.DRW.3", "DRW", FileKind.Drawing)]
    [InlineData("gearbox.asm.7", "asm", FileKind.Drawing)]
    [InlineData("boot.asm", "asm", FileKind.Code)]
    [InlineData("notes.txt.2", "2", FileKind.Other)]
    [InlineData("flat.dxf", "dxf", FileKind.Drawing)]
    [InlineData("plot.plt", "plt", FileKind.Drawing)]
    public void Creo_numbered_saves_count_as_their_type(string name, string extension, FileKind kind)
    {
        Assert.Equal(extension, FileKinds.ExtensionOf(name));
        Assert.Equal(kind, FileKinds.Of(name));
    }

    [Fact]
    public async Task The_preview_pane_draws_a_dxf_and_says_when_a_drawing_has_no_preview()
    {
        using var temp = new TempFolder();
        string dxf = temp.File("plan.dxf", Dxf([.. Section("ENTITIES", Line(0, 0, 10, 5)), (0, "EOF")]));
        string plot = temp.File("empty.plt", "IN;SP1;");

        Assert.NotNull((await Preview.LoadAsync(dxf, CancellationToken.None)).Thumbnail);
        PreviewInfo none = await Preview.LoadAsync(plot, CancellationToken.None);
        Assert.Null(none.Thumbnail);
        Assert.Equal(Preview.NoDrawingPreview, none.Note);
    }

    [Fact]
    public async Task Windows_gives_the_thumbnail_its_handler_draws_and_none_without_one()
    {
        using var temp = new TempFolder();
        string png = Path.Combine(temp.Path, "picture.png");
        File.WriteAllBytes(png, Picture(ImageFormat.Png));
        string unknown = temp.File("thing.nohandlerhere", "nothing to draw");

        Assert.NotNull(await ShellThumbnail.ReadAsync(png, 64, CancellationToken.None));
        Assert.Null(await ShellThumbnail.ReadAsync(unknown, 64, CancellationToken.None));
    }

    /// <summary>A number in HP-GL/2's 8-bit encoding: base 64, lowest digit first, the sign in the lowest bit.</summary>
    private static string Encode(long value)
    {
        long n = value >= 0 ? value << 1 : ((-value) << 1) | 1;
        var digits = new StringBuilder();
        while (n >= 64)
        {
            digits.Append((char)(63 + n % 64));
            n /= 64;
        }
        return digits.Append((char)(191 + n)).ToString();
    }

    private static byte[] Picture(ImageFormat format)
    {
        using var image = new Bitmap(8, 4);
        using (var graphics = Graphics.FromImage(image))
        {
            graphics.Clear(Color.SteelBlue);
        }
        using var stream = new MemoryStream();
        image.Save(stream, format);
        return stream.ToArray();
    }
}
