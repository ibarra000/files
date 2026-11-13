using System.Windows;
using RapidOcrNet;
using SkiaSharp;

namespace Files.Services;

/// <summary>
/// PaddleOCR's PP-OCRv5 Latin models run on this PC through ONNX Runtime (RapidOcrNet):
/// far better than Windows OCR with letters, and nothing leaves the PC. The models are
/// loaded once, on first use or when warmed, and kept until the app closes.
/// </summary>
public sealed class PaddleTextReader : ITextReader, IDisposable
{
    /// <summary>Where the package puts the models: <c>models\v5</c> beside files.exe.</summary>
    public static string DefaultModelFolder => Path.Combine(AppContext.BaseDirectory, RapidOcr.ModelsFolderName, RapidOcr.ModelsVersion);

    private static readonly RapidOcrOptions Options = RapidOcrOptions.Default with
    {
        // Word boxes, so the word under the mouse can be picked; screen text is upright, so no rotation check.
        ReturnWordBox = true,
        DoAngle = false,
    };

    private readonly string _modelFolder;

    // Not cached when loading fails, so a later read tries again (and the caller falls back meanwhile).
    private readonly Lazy<RapidOcr> _ocr;

    public PaddleTextReader(string modelFolder)
    {
        _modelFolder = modelFolder;
        _ocr = new Lazy<RapidOcr>(Load, LazyThreadSafetyMode.PublicationOnly);
    }

    /// <summary>Twice the screen's size is enough for its text detector, and faster than more.</summary>
    public int Scale => 2;

    public Task<IReadOnlyList<(string Text, Rect Box)>> ReadWordsAsync(byte[] png, CancellationToken cancel) =>
        Task.Run(() => Read(png, cancel), cancel);

    /// <summary>Loads the models in the background, so the first read is not slowed by it. Failures wait for that read.</summary>
    public Task WarmAsync() => Task.Run(() =>
    {
        try
        {
            _ = _ocr.Value;
        }
        catch (Exception error) when (IsLoadFailure(error))
        {
            System.Diagnostics.Trace.WriteLine($"PaddleOCR could not be loaded: {error}");
        }
    });

    /// <summary>The ways the models can fail to load or run: missing or damaged files, or the native runtime.</summary>
    public static bool IsLoadFailure(Exception error) =>
        error is IOException or UnauthorizedAccessException or DllNotFoundException or BadImageFormatException
            or InvalidOperationException or Microsoft.ML.OnnxRuntime.OnnxRuntimeException or TypeInitializationException;

    public void Dispose()
    {
        if (_ocr.IsValueCreated)
        {
            _ocr.Value.Dispose();
        }
    }

    private IReadOnlyList<(string Text, Rect Box)> Read(byte[] png, CancellationToken cancel)
    {
        RapidOcr ocr = _ocr.Value;
        using SKBitmap image = SKBitmap.Decode(png) ?? throw new InvalidOperationException("The screen capture could not be decoded");
        RapidOcrNet.OcrResult result = ocr.Detect(image, Options, cancel);
        var words = new List<(string Text, Rect Box)>();
        foreach (TextBlock block in result.TextBlocks)
        {
            foreach (WordBox word in block.WordResults ?? [])
            {
                words.Add((word.Text, Bounds(word.BoxPoints)));
            }
        }
        return words;
    }

    private RapidOcr Load()
    {
        string[] files = [RapidOcr.DefaultDetModelPath, RapidOcr.DefaultClsModelPath, RapidOcr.DefaultRecModelPath, RapidOcr.DefaultKeysFilePath];
        string[] paths = files.Select(file => Path.Combine(_modelFolder, file)).ToArray();
        string? missing = paths.FirstOrDefault(path => !File.Exists(path));
        if (missing is not null)
        {
            throw new FileNotFoundException("A PaddleOCR model is missing", missing);
        }
        var ocr = new RapidOcr();
        try
        {
            ocr.InitModels(paths[0], paths[1], paths[2], paths[3]);
            return ocr;
        }
        catch
        {
            ocr.Dispose();
            throw;
        }
    }

    private static Rect Bounds(SKPointI[] points)
    {
        int left = points.Min(p => p.X);
        int top = points.Min(p => p.Y);
        return new Rect(left, top, points.Max(p => p.X) - left, points.Max(p => p.Y) - top);
    }
}
