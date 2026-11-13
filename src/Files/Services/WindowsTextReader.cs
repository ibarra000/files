using System.Runtime.InteropServices.WindowsRuntime;
using System.Windows;
using Windows.Graphics.Imaging;
using Windows.Media.Ocr;
using Windows.Storage.Streams;

namespace Files.Services;

/// <summary>The text recognition built into Windows: always there, good with digits, weaker with letters.</summary>
public sealed class WindowsTextReader : ITextReader
{
    /// <summary>Windows OCR misreads letters at screen size; three times larger it reads them far better (1800×600, well under its limit).</summary>
    public int Scale => 3;

    public async Task<IReadOnlyList<(string Text, Rect Box)>> ReadWordsAsync(byte[] png, CancellationToken cancel)
    {
        OcrEngine engine = OcrEngine.TryCreateFromUserProfileLanguages()
            ?? throw new TextRecognitionUnavailableException("Windows has no text recognition for your languages; add one in Settings → Time & language");
        using var stream = new InMemoryRandomAccessStream();
        await stream.WriteAsync(png.AsBuffer());
        stream.Seek(0);
        BitmapDecoder decoder = await BitmapDecoder.CreateAsync(stream);
        using SoftwareBitmap bitmap = await decoder.GetSoftwareBitmapAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied);
        cancel.ThrowIfCancellationRequested();
        Windows.Media.Ocr.OcrResult result = await engine.RecognizeAsync(bitmap);
        var words = new List<(string Text, Rect Box)>();
        foreach (OcrLine line in result.Lines)
        {
            foreach (OcrWord word in line.Words)
            {
                words.Add((word.Text, new Rect(word.BoundingRect.X, word.BoundingRect.Y, word.BoundingRect.Width, word.BoundingRect.Height)));
            }
        }
        return words;
    }
}
