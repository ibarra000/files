using System.Runtime.InteropServices;
using System.Runtime.InteropServices.WindowsRuntime;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Automation.Text;
using Windows.Graphics.Imaging;
using Windows.Media.Ocr;
using Windows.Storage.Streams;

namespace Files.Services;

/// <summary>What was read under the mouse, or why nothing was.</summary>
public sealed record ScreenRead(string? Text, string? Problem);

/// <summary>
/// Reads the word or code under the mouse: from the app's accessibility text
/// when it offers one, else with the OCR engine built into Windows on the
/// screen around the pointer. Follows ibarra000/hover (Capture.cs, Ocr.cs).
/// </summary>
public static class ScreenText
{
    private static readonly TimeSpan AutomationTimeout = TimeSpan.FromMilliseconds(400);
    private const int CropHalfWidth = 300;
    private const int CropHalfHeight = 100;
    private const int MaxLine = 1000;

    /// <summary>Windows OCR misreads letters at screen size; three times larger it reads them far better (1800×600, well under its limit).</summary>
    private const int OcrScale = 3;

    /// <summary>Punctuation that surrounds a code in running text but is never part of it.</summary>
    private static readonly char[] Surrounding = ['(', ')', '[', ']', '{', '}', '<', '>', ',', ';', ':', '"', '\'', '`'];

    /// <summary>Sentence punctuation after a code; a leading dot stays (".gitignore").</summary>
    private static readonly char[] Trailing = ['.', '!', '?'];

    public static async Task<ScreenRead> UnderCursorAsync()
    {
        GetCursorPos(out Point32 cursor);
        string? text = await FromAutomationAsync(cursor);
        if (text is not null)
        {
            return new ScreenRead(text, null);
        }
        OcrEngine? engine = OcrEngine.TryCreateFromUserProfileLanguages();
        if (engine is null)
        {
            return new ScreenRead(null, "Windows has no text recognition for your languages; add one in Settings → Time & language");
        }
        text = await FromOcrAsync(engine, cursor);
        return text is null ? new ScreenRead(null, "No text under the mouse") : new ScreenRead(text, null);
    }

    /// <summary>The whitespace-separated token of <paramref name="line"/> that holds <paramref name="word"/>, so AB-C-0704_Page1 stays whole.</summary>
    public static string? TokenAround(string line, string word)
    {
        string trimmed = word.Trim();
        if (trimmed.Length == 0)
        {
            return null;
        }
        string[] tokens = line.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries);
        // Prefer a token where the word starts at a boundary, so "AB" finds "AB-C" rather than "TAB".
        string? token = tokens.FirstOrDefault(t => StartsWordAt(t, trimmed));
        token ??= tokens.FirstOrDefault(t => t.Contains(trimmed, StringComparison.Ordinal));
        return Clean(token ?? trimmed);
    }

    /// <summary>Whether <paramref name="word"/> appears in <paramref name="token"/> not preceded by a letter or digit.</summary>
    private static bool StartsWordAt(string token, string word)
    {
        int at = token.IndexOf(word, StringComparison.Ordinal);
        while (at >= 0)
        {
            if (at == 0 || !char.IsLetterOrDigit(token[at - 1]))
            {
                return true;
            }
            at = token.IndexOf(word, at + 1, StringComparison.Ordinal);
        }
        return false;
    }

    /// <summary>The word whose box holds <paramref name="point"/>, else the closest one, or null when there are none.</summary>
    public static string? Nearest(IReadOnlyList<(string Text, Rect Box)> words, Point point)
    {
        string? best = null;
        double bestDistance = double.MaxValue;
        foreach (var (text, box) in words)
        {
            double dx = Math.Max(0, Math.Max(box.Left - point.X, point.X - box.Right));
            double dy = Math.Max(0, Math.Max(box.Top - point.Y, point.Y - box.Bottom));
            double distance = dx * dx + dy * dy;
            if (distance < bestDistance)
            {
                best = text;
                bestDistance = distance;
            }
        }
        return best is null ? null : Clean(best);
    }

    /// <summary>Strips surrounding punctuation; null when nothing is left.</summary>
    public static string? Clean(string token)
    {
        string cleaned = token.Trim().TrimStart(Surrounding).TrimEnd([.. Surrounding, .. Trailing]);
        return cleaned.Length > 0 ? cleaned : null;
    }

    /// <summary>UI Automation can hang on a busy app, so it gets a time limit.</summary>
    private static async Task<string?> FromAutomationAsync(Point32 cursor)
    {
        Task<string?> read = Task.Run(() => ReadAutomation(cursor));
        Task finished = await Task.WhenAny(read, Task.Delay(AutomationTimeout));
        return finished == read ? await read : null;
    }

    private static string? ReadAutomation(Point32 cursor)
    {
        try
        {
            var point = new Point(cursor.X, cursor.Y);
            AutomationElement element = AutomationElement.FromPoint(point);
            if (!element.TryGetCurrentPattern(TextPattern.Pattern, out object pattern))
            {
                return null;
            }
            TextPatternRange word = ((TextPattern)pattern).RangeFromPoint(point);
            TextPatternRange line = word.Clone();
            word.ExpandToEnclosingUnit(TextUnit.Word);
            // Over blank space an app returns the nearest word; let OCR look instead.
            Rect[] boxes = word.GetBoundingRectangles();
            if (!boxes.Any(box => box.Contains(point)))
            {
                return null;
            }
            line.ExpandToEnclosingUnit(TextUnit.Line);
            return TokenAround(line.GetText(MaxLine), word.GetText(MaxLine));
        }
        catch (Exception error) when (error is ElementNotAvailableException or InvalidOperationException
            or COMException or ArgumentException or UnauthorizedAccessException or NotSupportedException or TimeoutException)
        {
            // Apps without accessibility text; OCR reads them instead.
            return null;
        }
    }

    private static async Task<string?> FromOcrAsync(OcrEngine engine, Point32 cursor)
    {
        // The crop around the pointer, moved back onto the screen near its edges.
        System.Drawing.Rectangle screen = System.Windows.Forms.SystemInformation.VirtualScreen;
        var area = new System.Drawing.Rectangle(cursor.X - CropHalfWidth, cursor.Y - CropHalfHeight, 2 * CropHalfWidth, 2 * CropHalfHeight);
        area.X = Math.Max(screen.Left, Math.Min(area.X, screen.Right - area.Width));
        area.Y = Math.Max(screen.Top, Math.Min(area.Y, screen.Bottom - area.Height));
        int left = area.X;
        int top = area.Y;

        // Capturing and encoding off the UI thread keeps the panel responsive.
        using SoftwareBitmap bitmap = await Task.Run(() => GrabAsync(area));
        OcrResult result = await engine.RecognizeAsync(bitmap);
        var words = new List<(string Text, Rect Box)>();
        foreach (OcrLine line in result.Lines)
        {
            foreach (OcrWord word in line.Words)
            {
                var box = new Rect(word.BoundingRect.X, word.BoundingRect.Y, word.BoundingRect.Width, word.BoundingRect.Height);
                words.Add((word.Text, box));
            }
        }
        // The word boxes are in the enlarged image.
        return Nearest(words, new Point((cursor.X - left) * OcrScale, (cursor.Y - top) * OcrScale));
    }

    private static async Task<SoftwareBitmap> GrabAsync(System.Drawing.Rectangle area)
    {
        using var screen = new System.Drawing.Bitmap(area.Width, area.Height);
        using (var graphics = System.Drawing.Graphics.FromImage(screen))
        {
            graphics.CopyFromScreen(area.Location, System.Drawing.Point.Empty, area.Size);
        }
        using var image = new System.Drawing.Bitmap(area.Width * OcrScale, area.Height * OcrScale);
        using (var graphics = System.Drawing.Graphics.FromImage(image))
        {
            graphics.InterpolationMode = System.Drawing.Drawing2D.InterpolationMode.HighQualityBicubic;
            graphics.PixelOffsetMode = System.Drawing.Drawing2D.PixelOffsetMode.HighQuality;
            graphics.DrawImage(screen, 0, 0, image.Width, image.Height);
        }
        using var png = new MemoryStream();
        image.Save(png, System.Drawing.Imaging.ImageFormat.Png);
        using var stream = new InMemoryRandomAccessStream();
        await stream.WriteAsync(png.ToArray().AsBuffer());
        stream.Seek(0);
        BitmapDecoder decoder = await BitmapDecoder.CreateAsync(stream);
        return await decoder.GetSoftwareBitmapAsync(BitmapPixelFormat.Bgra8, BitmapAlphaMode.Premultiplied);
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Point32
    {
        public int X;
        public int Y;
    }

    [DllImport("user32.dll")]
    private static extern bool GetCursorPos(out Point32 point);
}
