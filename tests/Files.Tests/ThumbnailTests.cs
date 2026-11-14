using System.Drawing;
using System.Drawing.Imaging;
using System.Text;
using Files.Services;

namespace Files.Tests;

public sealed class ThumbnailTests
{
    private static readonly byte[] Sentinel = DwgThumbnail.ImageSentinel.ToArray();

    /// <summary>A small picture in <paramref name="format"/>, the way a CAD program saves its preview.</summary>
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

    /// <summary>A drawing file holding only what the preview needs: the version, the image address and the image section.</summary>
    private static byte[] Dwg(byte code, byte[] image, int? address = null, int? declaredSize = null)
    {
        const int Section = 0x40;
        var file = new MemoryStream();
        var writer = new BinaryWriter(file);
        writer.Write(Encoding.ASCII.GetBytes("AC1018"));
        file.Position = 0x0D;
        writer.Write(address ?? Section);
        file.Position = Section;
        writer.Write(Sentinel);
        int start = Section + Sentinel.Length + 4 + 1 + 2 * 9;
        writer.Write(0); // overall size, not needed
        writer.Write((byte)2);
        writer.Write((byte)1); // the header entry, which is skipped
        writer.Write(start);
        writer.Write(0);
        writer.Write(code);
        writer.Write(start);
        writer.Write(declaredSize ?? image.Length);
        writer.Write(image);
        return file.ToArray();
    }

    private static Size SizeOf(byte[] encoded)
    {
        using var image = Image.FromStream(new MemoryStream(encoded));
        return image.Size;
    }

    [Fact]
    public void A_drawing_with_a_png_preview_gives_it_as_it_is()
    {
        byte[] png = Picture(ImageFormat.Png);
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "plan.dwg");
        File.WriteAllBytes(path, Dwg(6, png));

        Assert.Equal(png, DwgThumbnail.Read(path));
    }

    [Fact]
    public void A_drawing_with_a_bitmap_preview_gives_a_whole_bmp_file()
    {
        byte[] bmp = Picture(ImageFormat.Bmp);
        // A drawing stores the bitmap without the 14-byte file header.
        byte[] dib = bmp[14..];
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "plan.dwg");
        File.WriteAllBytes(path, Dwg(2, dib));

        byte[] read = DwgThumbnail.Read(path)!;

        Assert.Equal("BM", Encoding.ASCII.GetString(read, 0, 2));
        Assert.Equal(new Size(8, 4), SizeOf(read));
    }

    [Theory]
    [InlineData(3, null, null)] // only a WMF preview
    [InlineData(6, 0x7FFF_0000, null)] // an image address past the end
    [InlineData(6, null, 0x7FFF_0000)] // an image larger than the file
    [InlineData(6, -5, null)] // a negative address
    public void A_drawing_without_a_usable_preview_gives_none(byte code, int? address, int? size)
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "plan.dwg");
        File.WriteAllBytes(path, Dwg(code, Picture(ImageFormat.Png), address, size));

        Assert.Null(DwgThumbnail.Read(path));
    }

    [Fact]
    public void A_file_too_short_or_not_a_drawing_gives_none()
    {
        using var temp = new TempFolder();
        Assert.Null(DwgThumbnail.Read(temp.File("short.dwg", "AC10")));
        Assert.Null(DwgThumbnail.Read(temp.File("text.dwg", new string('x', 400))));
    }

    [Fact]
    public async Task A_pdf_shows_its_first_page_and_how_many_there_are()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "spec.pdf");
        File.WriteAllBytes(path, MinimalPdf(pages: 3));

        PreviewInfo preview = await Preview.LoadAsync(path, CancellationToken.None);

        Assert.NotNull(preview.Thumbnail);
        Assert.Equal(Preview.ThumbnailWidth, SizeOf(preview.Thumbnail!).Width);
        Assert.Equal("Page 1 of 3", preview.Note);
    }

    [Fact]
    public async Task A_damaged_pdf_says_so()
    {
        using var temp = new TempFolder();
        string path = temp.File("broken.pdf", "%PDF-1.4 this is not a real document");

        PreviewInfo preview = await Preview.LoadAsync(path, CancellationToken.None);

        Assert.Null(preview.Thumbnail);
        Assert.Equal("Could not show this PDF", preview.Note);
        Assert.Equal("PDF document", preview.Kind);
    }

    [Fact]
    public void A_bitmap_preview_with_a_negative_colour_count_gives_none()
    {
        byte[] dib = Picture(ImageFormat.Bmp)[14..];
        BitConverter.TryWriteBytes(dib.AsSpan(32), -1000);
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "plan.dwg");
        File.WriteAllBytes(path, Dwg(2, dib));

        Assert.Null(DwgThumbnail.Read(path));
    }

    [Fact]
    public async Task A_drawing_preview_is_shown_through_the_preview_pane()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "plan.dwg");
        File.WriteAllBytes(path, Dwg(6, Picture(ImageFormat.Png)));
        string bare = temp.File("bare.dwg", "AC1018" + new string('\0', 200));

        Assert.NotNull((await Preview.LoadAsync(path, CancellationToken.None)).Thumbnail);
        Assert.Equal("No preview saved in this drawing", (await Preview.LoadAsync(bare, CancellationToken.None)).Note);
    }

    /// <summary>A valid PDF of blank US-letter pages, with a correct cross-reference table.</summary>
    private static byte[] MinimalPdf(int pages)
    {
        var objects = new List<string>
        {
            "<< /Type /Catalog /Pages 2 0 R >>",
            $"<< /Type /Pages /Kids [{string.Join(" ", Enumerable.Range(0, pages).Select(i => $"{i + 3} 0 R"))}] /Count {pages} >>",
        };
        objects.AddRange(Enumerable.Repeat("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>", pages));
        var pdf = new StringBuilder("%PDF-1.4\n");
        var offsets = new List<int>();
        for (int i = 0; i < objects.Count; i++)
        {
            offsets.Add(pdf.Length);
            pdf.Append($"{i + 1} 0 obj\n{objects[i]}\nendobj\n");
        }
        int xref = pdf.Length;
        pdf.Append($"xref\n0 {objects.Count + 1}\n0000000000 65535 f \n");
        foreach (int offset in offsets)
        {
            pdf.Append($"{offset:D10} 00000 n \n");
        }
        pdf.Append($"trailer\n<< /Size {objects.Count + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n");
        return Encoding.ASCII.GetBytes(pdf.ToString());
    }
}
