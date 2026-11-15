using System.Drawing;
using System.Drawing.Imaging;
using System.Text;
using Files.Services;

namespace Files.Tests;

public sealed class CreoThumbnailTests
{
    private const string Marker = "#THMB_IMG_MAIN";

    /// <summary>A real JPEG with some detail, so its coded data is not trivial.</summary>
    private static byte[] Jpeg()
    {
        using var image = new Bitmap(64, 64);
        using (var graphics = Graphics.FromImage(image))
        {
            graphics.Clear(Color.LightGray);
            graphics.FillEllipse(Brushes.SteelBlue, 8, 8, 48, 40);
            graphics.DrawLine(Pens.Black, 0, 63, 63, 0);
        }
        using var stream = new MemoryStream();
        image.Save(stream, ImageFormat.Jpeg);
        return stream.ToArray();
    }

    /// <summary>A Creo file: its header, <paramref name="before"/> bytes of model data, the marker and the preview, then more data.</summary>
    private static byte[] Creo(byte[] image, long before, int after = 2000, string marker = Marker)
    {
        var file = new MemoryStream();
        file.Write(Encoding.ASCII.GetBytes("#UGC:2 ASSEMBLY 2552 2120 800 1 1 15 4300 2025102 00000285 \n"));
        file.Write(Filler(before));
        file.Write(Encoding.ASCII.GetBytes(marker));
        // As Creo writes it: a few bytes of the section's own before the JPEG.
        file.Write([0x00, 0x01, 0x00, 0x00]);
        file.Write(image);
        file.Write(Filler(after));
        return file.ToArray();
    }

    /// <summary>Bytes that hold neither the marker nor a JPEG's start.</summary>
    private static byte[] Filler(long count)
    {
        var bytes = new byte[count];
        for (long i = 0; i < count; i++)
        {
            bytes[i] = (byte)('a' + (i % 26));
        }
        return bytes;
    }

    private static string Write(TempFolder temp, string name, byte[] content)
    {
        string path = Path.Combine(temp.Path, name);
        File.WriteAllBytes(path, content);
        return path;
    }

    [Fact]
    public void An_assembly_gives_the_preview_it_saved()
    {
        byte[] jpeg = Jpeg();
        using var temp = new TempFolder();
        string path = Write(temp, "robot.asm.4", Creo(jpeg, before: 80_000));

        Assert.Equal(jpeg, CreoThumbnail.Read(path, CancellationToken.None));
    }

    [Fact]
    public void A_compressed_preview_gives_the_jpeg_without_what_follows_it()
    {
        // Creo compresses some previews with Unix compress; model data follows them in the file.
        byte[] jpeg = Jpeg();
        using var temp = new TempFolder();
        string path = Write(temp, "robot.asm.14", Creo(UnixCompressTests.Compress(jpeg), before: 5000));

        Assert.Equal(jpeg, CreoThumbnail.Read(path, CancellationToken.None));
    }

    [Fact]
    public void A_preview_further_from_the_end_than_is_searched_is_not_looked_for()
    {
        using var temp = new TempFolder();
        string path = Write(temp, "big.prt.1", Creo(Jpeg(), before: 1000, after: (int)CreoThumbnail.TailBytes + 1000));

        Assert.Null(CreoThumbnail.Read(path, CancellationToken.None));
    }

    [Theory]
    [InlineData(1)]
    [InlineData(13)]
    public void A_marker_split_between_two_reads_is_found(int straddle)
    {
        byte[] jpeg = Jpeg();
        using var temp = new TempFolder();
        // Reads go back from the end a chunk at a time; the marker starts this many bytes before the first one.
        // After the marker come 4 section bytes, the JPEG, then the filler.
        int after = CreoThumbnail.ChunkBytes + straddle - Marker.Length - 4 - jpeg.Length;
        string path = Write(temp, "split.prt.2", Creo(jpeg, before: 100, after));

        Assert.Equal(jpeg, CreoThumbnail.Read(path, CancellationToken.None));
    }

    [Fact]
    public void The_last_marker_is_the_preview()
    {
        byte[] jpeg = Jpeg();
        byte[] decoy = Encoding.ASCII.GetBytes(Marker + " but no picture follows ");
        byte[] real = Creo(jpeg, before: 100);
        byte[] content = [.. real[..61], .. decoy, .. real[61..]];
        using var temp = new TempFolder();

        Assert.Equal(jpeg, CreoThumbnail.Read(Write(temp, "decoy.asm.2", content), CancellationToken.None));
    }

    [Fact]
    public void A_stray_compress_signature_before_a_stored_picture_is_passed_over()
    {
        byte[] jpeg = Jpeg();
        byte[] section = [0x1F, 0x9D, 0x10, 0x00, 0x00, .. jpeg, 0x61, 0x62];

        Assert.Equal(jpeg, CreoThumbnail.Picture(section));
    }

    [Fact]
    public void Another_programs_prt_file_is_left_to_its_windows_thumbnail()
    {
        using var temp = new TempFolder();
        byte[] content = Creo(Jpeg(), before: 100);
        content[0] = (byte)'X';
        string path = Write(temp, "bracket.prt", content);

        Assert.Equal(Preview.NoDrawingPreview, Preview.Load(path, CancellationToken.None).Note);
    }

    [Fact]
    public void A_file_that_is_not_creos_gives_none()
    {
        using var temp = new TempFolder();
        byte[] content = Creo(Jpeg(), before: 100);
        content[0] = (byte)'X';

        Assert.Null(CreoThumbnail.Read(Write(temp, "other.prt", content), CancellationToken.None));
    }

    [Fact]
    public void An_old_save_without_a_preview_gives_none()
    {
        using var temp = new TempFolder();
        string path = Write(temp, "old.prt.1", Creo(Jpeg(), before: 100, marker: "#OTHER_SECTION"));

        Assert.Null(CreoThumbnail.Read(path, CancellationToken.None));
    }

    [Fact]
    public void A_marker_with_no_picture_after_it_gives_none()
    {
        using var temp = new TempFolder();
        string path = Write(temp, "empty.asm.1", Creo(Encoding.ASCII.GetBytes("no picture here"), before: 100));

        Assert.Null(CreoThumbnail.Read(path, CancellationToken.None));
    }

    [Fact]
    public void A_cut_off_preview_gives_none()
    {
        byte[] jpeg = Jpeg();
        using var temp = new TempFolder();
        string path = Write(temp, "cut.asm.1", Creo(jpeg[..(jpeg.Length / 2)], before: 100, after: 0));

        Assert.Null(CreoThumbnail.Read(path, CancellationToken.None));
    }

    [Fact]
    public void A_segment_longer_than_the_file_gives_none()
    {
        // Start of image, then a segment that claims 64 KB in a file far shorter.
        byte[] jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 0x01, 0x02];
        using var temp = new TempFolder();
        string path = Write(temp, "long.asm.1", Creo(jpeg, before: 100, after: 50));

        Assert.Null(CreoThumbnail.Read(path, CancellationToken.None));
    }

    [Fact]
    public void Stuffed_and_restart_bytes_in_the_coded_data_do_not_end_the_picture()
    {
        byte[] jpeg =
        [
            0xFF, 0xD8,
            0xFF, 0xDA, 0x00, 0x02, // start of scan, no parameters
            0x11, 0xFF, 0x00, 0xD9, 0xFF, 0xD3, 0x22, // a stuffed 0xFF before a D9 byte, and a restart marker
            0xFF, 0xD9,
        ];

        Assert.Equal(jpeg.Length, CreoThumbnail.JpegEnd(jpeg, 0));
    }

    [Fact]
    public void A_cancelled_read_stops()
    {
        using var temp = new TempFolder();
        string path = Write(temp, "robot.asm.4", Creo(Jpeg(), before: 100));
        using var cancel = new CancellationTokenSource();
        cancel.Cancel();

        Assert.Throws<OperationCanceledException>(() => CreoThumbnail.Read(path, cancel.Token));
    }

    [Fact]
    public async Task Creo_files_show_their_preview_and_assembly_source_still_shows_its_lines()
    {
        using var temp = new TempFolder();
        string numbered = Write(temp, "robot.asm.3", Creo(Jpeg(), before: 100));
        string plain = Write(temp, "robot.asm", Creo(Jpeg(), before: 100));
        string source = temp.File("boot.asm", "mov ax, 1\nint 0x10\n");

        PreviewInfo assembly = await Preview.LoadAsync(numbered, CancellationToken.None);
        PreviewInfo unnumbered = await Preview.LoadAsync(plain, CancellationToken.None);
        PreviewInfo code = await Preview.LoadAsync(source, CancellationToken.None);

        Assert.NotNull(assembly.Thumbnail);
        Assert.Null(assembly.Note);
        Assert.NotNull(unnumbered.Thumbnail);
        Assert.Equal("Drawing", unnumbered.Kind);
        Assert.Equal(["mov ax, 1", "int 0x10"], code.Lines);
    }

    [Fact]
    public async Task A_creo_file_saved_without_a_preview_says_so()
    {
        using var temp = new TempFolder();
        string path = Write(temp, "old.prt.1", Creo(Jpeg(), before: 100, marker: "#OTHER_SECTION"));

        PreviewInfo preview = await Preview.LoadAsync(path, CancellationToken.None);

        Assert.Null(preview.Thumbnail);
        Assert.Equal(Preview.NoCreoPreview, preview.Note);
    }
}
