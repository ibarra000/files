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

    // One engine, loaded once. Loading, reading and disposing all hold this lock, so a warm-up racing
    // the first read never builds a second engine, and closing the app never frees one mid-read.
    // A failed load leaves _ocr null, so the next read tries again (and the caller falls back meanwhile).
    private readonly object _gate = new();
    private RapidOcr? _ocr;
    private bool _disposed;

    public PaddleTextReader(string modelFolder)
    {
        _modelFolder = modelFolder;
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
            lock (_gate)
            {
                Engine();
            }
        }
        catch (TextReaderLoadException error)
        {
            System.Diagnostics.Trace.WriteLine($"PaddleOCR could not be loaded: {error}");
        }
        catch (ObjectDisposedException)
        {
            // The app closed before the warm-up ran.
        }
    });

    public void Dispose()
    {
        lock (_gate)
        {
            _disposed = true;
            _ocr?.Dispose();
            _ocr = null;
        }
    }

    /// <summary>The loaded engine, loading it first if needed. Call with <see cref="_gate"/> held.</summary>
    private RapidOcr Engine()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        return _ocr ??= Load();
    }

    private IReadOnlyList<(string Text, Rect Box)> Read(byte[] png, CancellationToken cancel)
    {
        using SKBitmap image = SKBitmap.Decode(png) ?? throw new InvalidDataException("The screen capture could not be decoded");
        RapidOcrNet.OcrResult result;
        lock (_gate)
        {
            result = Engine().Detect(image, Options, cancel);
        }
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
            throw new TextReaderLoadException($"A PaddleOCR model is missing: {missing}", new FileNotFoundException(null, missing));
        }
        RapidOcr? ocr = null;
        try
        {
            ocr = new RapidOcr();
            ocr.InitModels(paths[0], paths[1], paths[2], paths[3]);
            return ocr;
        }
        catch (Exception error) when (error is Microsoft.ML.OnnxRuntime.OnnxRuntimeException or DllNotFoundException
            or BadImageFormatException or TypeInitializationException or IOException or UnauthorizedAccessException)
        {
            // A damaged model, or ONNX Runtime or Skia that cannot load on this PC.
            ocr?.Dispose();
            throw new TextReaderLoadException($"PaddleOCR could not be loaded: {error.Message}", error);
        }
    }

    private static Rect Bounds(SKPointI[] points)
    {
        int left = points.Min(p => p.X);
        int top = points.Min(p => p.Y);
        return new Rect(left, top, points.Max(p => p.X) - left, points.Max(p => p.Y) - top);
    }
}
