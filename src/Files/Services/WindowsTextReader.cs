using System.Runtime.InteropServices.WindowsRuntime;
using System.Windows;
using Windows.Graphics.Imaging;
using Windows.Media.Ocr;
using Windows.Storage.Streams;

namespace Files.Services;

/// <summary>The text recognition built into Windows: always there, good with digits, weaker with letters.</summary>
/// <param name="language">The language to read (Settings.OcrLanguage), or empty for the user's languages.</param>
public sealed class WindowsTextReader(Func<string> language) : ITextReader
{
    public WindowsTextReader()
        : this(() => "")
    {
    }

    /// <summary>The languages Windows can read on this PC, as tags and display names.</summary>
    public static IReadOnlyList<(string Tag, string Name)> Languages() =>
        OcrEngine.AvailableRecognizerLanguages.Select(l => (l.LanguageTag, l.DisplayName)).ToList();

    /// <summary>Windows OCR misreads letters at screen size; three times larger it reads them far better (1800×600, well under its limit).</summary>
    public int Scale => 3;

    public async Task<IReadOnlyList<ReadWord>> ReadWordsAsync(byte[] png, CancellationToken cancel)
    {
        string tag = language();
        OcrEngine engine = (tag.Length > 0 ? TryCreate(tag) : null)
            ?? OcrEngine.TryCreateFromUserProfileLanguages()
            ?? throw new TextRecognitionUnavailableException("Windows has no text recognition for your languages; add one in Settings → Time & language");
        using var stream = new InMemoryRandomAccessStream();
        await stream.WriteAsync(png.AsBuffer());
        stream.Seek(0);
        BitmapDecoder decoder = await BitmapDecoder.CreateAsync(stream);
        using SoftwareBitmap bitmap = await decoder.GetSoftwareBitmapAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied);
        cancel.ThrowIfCancellationRequested();
        Windows.Media.Ocr.OcrResult result = await engine.RecognizeAsync(bitmap);
        var words = new List<ReadWord>();
        foreach (OcrLine line in result.Lines)
        {
            foreach (OcrWord word in line.Words)
            {
                words.Add(new ReadWord(word.Text, new Rect(word.BoundingRect.X, word.BoundingRect.Y, word.BoundingRect.Width, word.BoundingRect.Height)));
            }
        }
        return words;
    }

    /// <summary>An engine for the language chosen in Settings; null when Windows no longer has it, so the user's languages read instead.</summary>
    private static OcrEngine? TryCreate(string tag)
    {
        try
        {
            return OcrEngine.TryCreateFromLanguage(new Windows.Globalization.Language(tag));
        }
        catch (ArgumentException)
        {
            return null;
        }
    }
}
