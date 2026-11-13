using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Automation.Text;

namespace Files.Services;

/// <summary>What was read under the mouse, or why nothing was.</summary>
public sealed record ScreenRead(string? Text, string? Problem);

/// <summary>
/// Reads the word or code under the mouse: from the app's accessibility text
/// when it offers one, else with a text-recognition engine (Windows OCR, or
/// PaddleOCR when chosen in Settings) on the screen around the pointer.
/// Follows ibarra000/hover (Capture.cs, Ocr.cs).
/// </summary>
public static class ScreenText
{
    private static readonly TimeSpan AutomationTimeout = TimeSpan.FromMilliseconds(400);
    private const int CropHalfWidth = 300;
    private const int CropHalfHeight = 100;
    private const int MaxLine = 1000;

    /// <summary>Punctuation that surrounds a code in running text but is never part of it.</summary>
    private static readonly char[] Surrounding = ['(', ')', '[', ']', '{', '}', '<', '>', ',', ';', ':', '"', '\'', '`'];

    /// <summary>Sentence punctuation after a code; a leading dot stays (".gitignore").</summary>
    private static readonly char[] Trailing = ['.', '!', '?'];

    /// <param name="reader">The engine chosen in Settings.</param>
    /// <param name="fallback">Windows OCR, used when <paramref name="reader"/> cannot run on this PC.</param>
    public static async Task<ScreenRead> UnderCursorAsync(ITextReader reader, ITextReader fallback)
    {
        GetCursorPos(out Point32 cursor);
        string? text = await FromAutomationAsync(cursor);
        if (text is not null)
        {
            return new ScreenRead(text, null);
        }
        System.Drawing.Rectangle area = CropAround(cursor);
        string? note;
        try
        {
            // Capturing and encoding off the UI thread keeps the panel responsive.
            var (words, scale, fellBack) = await ReadWithFallbackAsync(reader, fallback, s => Task.Run(() => Grab(area, s)), CancellationToken.None);
            // The word boxes are in the enlarged image.
            text = Nearest(words, new Point((cursor.X - area.X) * scale, (cursor.Y - area.Y) * scale));
            note = fellBack ? FellBackNote : null;
        }
        catch (TextRecognitionUnavailableException error)
        {
            return new ScreenRead(null, error.Message);
        }
        return text is null ? new ScreenRead(null, note ?? "No text under the mouse") : new ScreenRead(text, note);
    }

    /// <summary>Said when the engine chosen in Settings could not load, so the user knows why reading is weaker.</summary>
    public const string FellBackNote = "PaddleOCR could not load, so Windows text recognition read this";

    /// <summary>
    /// Reads with <paramref name="reader"/>, grabbing the crop at its scale; if its models or runtime
    /// cannot load, reads with <paramref name="fallback"/> instead. Returns the words, the scale they
    /// were read at, and whether the fallback read them. Any other error is not hidden.
    /// </summary>
    public static async Task<(IReadOnlyList<(string Text, Rect Box)> Words, int Scale, bool FellBack)> ReadWithFallbackAsync(
        ITextReader reader, ITextReader fallback, Func<int, Task<byte[]>> grab, CancellationToken cancel)
    {
        try
        {
            return (await reader.ReadWordsAsync(await grab(reader.Scale), cancel), reader.Scale, false);
        }
        catch (TextReaderLoadException error) when (!ReferenceEquals(reader, fallback))
        {
            System.Diagnostics.Trace.WriteLine($"Reading with {reader.GetType().Name} failed, so Windows OCR reads instead: {error}");
        }
        return (await fallback.ReadWordsAsync(await grab(fallback.Scale), cancel), fallback.Scale, true);
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

    /// <summary>The crop around the pointer, moved back onto the screen near its edges.</summary>
    private static System.Drawing.Rectangle CropAround(Point32 cursor)
    {
        System.Drawing.Rectangle screen = System.Windows.Forms.SystemInformation.VirtualScreen;
        var area = new System.Drawing.Rectangle(cursor.X - CropHalfWidth, cursor.Y - CropHalfHeight, 2 * CropHalfWidth, 2 * CropHalfHeight);
        area.X = Math.Max(screen.Left, Math.Min(area.X, screen.Right - area.Width));
        area.Y = Math.Max(screen.Top, Math.Min(area.Y, screen.Bottom - area.Height));
        return area;
    }

    /// <summary>The screen in <paramref name="area"/>, enlarged <paramref name="scale"/> times with bicubic scaling, as PNG.</summary>
    private static byte[] Grab(System.Drawing.Rectangle area, int scale)
    {
        using var screen = new System.Drawing.Bitmap(area.Width, area.Height);
        using (var graphics = System.Drawing.Graphics.FromImage(screen))
        {
            graphics.CopyFromScreen(area.Location, System.Drawing.Point.Empty, area.Size);
        }
        using var image = new System.Drawing.Bitmap(area.Width * scale, area.Height * scale);
        using (var graphics = System.Drawing.Graphics.FromImage(image))
        {
            graphics.InterpolationMode = System.Drawing.Drawing2D.InterpolationMode.HighQualityBicubic;
            graphics.PixelOffsetMode = System.Drawing.Drawing2D.PixelOffsetMode.HighQuality;
            graphics.DrawImage(screen, 0, 0, image.Width, image.Height);
        }
        using var png = new MemoryStream();
        image.Save(png, System.Drawing.Imaging.ImageFormat.Png);
        return png.ToArray();
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
