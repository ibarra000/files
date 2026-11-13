using System.Drawing;
using System.Drawing.Imaging;
using System.Drawing.Text;
using Files.Services;
using Point = System.Windows.Point;
using Rect = System.Windows.Rect;

namespace Files.Tests;

public sealed class TextReaderTests
{
    private const string Line = "see AB-C-0704_Page1.pdf for details";
    private const string Code = "AB-C-0704_Page1.pdf";

    /// <summary>A screen-like crop: <see cref="Line"/> in 13 px Segoe UI, enlarged as the reader asks.</summary>
    private static byte[] Render(int scale, Color text, Color background)
    {
        using var image = new Bitmap(600 * scale, 200 * scale);
        using (var graphics = Graphics.FromImage(image))
        {
            graphics.Clear(background);
            graphics.TextRenderingHint = TextRenderingHint.ClearTypeGridFit;
            using var font = new Font("Segoe UI", 13 * scale, GraphicsUnit.Pixel);
            using var brush = new SolidBrush(text);
            graphics.DrawString(Line, font, brush, 40 * scale, 90 * scale);
        }
        using var png = new MemoryStream();
        image.Save(png, ImageFormat.Png);
        return png.ToArray();
    }

    private static string ModelFolder => Path.Combine(AppContext.BaseDirectory, "models", "v5");

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task PaddleOCR_reads_a_whole_code_in_light_and_dark(bool dark)
    {
        using var reader = new PaddleTextReader(ModelFolder);
        byte[] png = Render(reader.Scale, dark ? Color.White : Color.Black, dark ? Color.FromArgb(30, 30, 32) : Color.White);

        IReadOnlyList<(string Text, Rect Box)> words = await reader.ReadWordsAsync(png, CancellationToken.None);

        (string text, Rect box) = Assert.Single(words, word => ScreenText.Clean(word.Text) == Code);
        Point middle = new(box.X + box.Width / 2, box.Y + box.Height / 2);
        Assert.Equal(Code, ScreenText.Nearest(words, middle));
        Assert.NotEmpty(text);
    }

    [Fact]
    public async Task PaddleOCR_without_its_models_says_which_file_is_missing()
    {
        using var reader = new PaddleTextReader(Path.Combine(ModelFolder, "missing"));
        await Assert.ThrowsAsync<FileNotFoundException>(() => reader.ReadWordsAsync(Render(1, Color.Black, Color.White), CancellationToken.None));
    }

    [Fact]
    public async Task An_engine_that_cannot_run_falls_back_to_the_other()
    {
        var broken = new StubReader(2, new FileNotFoundException("no model"));
        var windows = new StubReader(3, words: [("plan.pdf", new Rect(0, 0, 10, 10))]);
        var scalesGrabbed = new List<int>();

        (IReadOnlyList<(string Text, Rect Box)> words, int scale) = await ScreenText.ReadWithFallbackAsync(broken, windows, s =>
        {
            scalesGrabbed.Add(s);
            return Task.FromResult(Array.Empty<byte>());
        }, CancellationToken.None);

        Assert.Equal("plan.pdf", Assert.Single(words).Text);
        Assert.Equal(3, scale);
        Assert.Equal([2, 3], scalesGrabbed);
    }

    [Fact]
    public async Task An_unexpected_error_is_not_hidden_by_the_fallback()
    {
        var broken = new StubReader(2, new ArgumentNullException("png"));
        var windows = new StubReader(3, words: []);
        await Assert.ThrowsAsync<ArgumentNullException>(() =>
            ScreenText.ReadWithFallbackAsync(broken, windows, _ => Task.FromResult(Array.Empty<byte>()), CancellationToken.None));
    }

    private sealed class StubReader(int scale, Exception? error = null, IReadOnlyList<(string Text, Rect Box)>? words = null) : ITextReader
    {
        public int Scale => scale;

        public Task<IReadOnlyList<(string Text, Rect Box)>> ReadWordsAsync(byte[] png, CancellationToken cancel) =>
            error is null ? Task.FromResult(words ?? []) : Task.FromException<IReadOnlyList<(string Text, Rect Box)>>(error);
    }
}
