using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Windows;

namespace Files.Services;

/// <summary>A line of text in a drawing: where its baseline starts, how tall its capitals are, and its angle in degrees.</summary>
public sealed record Label(Point At, double Height, double Rotation, string Text);

/// <summary>
/// The lines read from a drawing file (a DXF's geometry, a plot's pen moves, a STEP model's edges),
/// gathered up to <see cref="MaxPoints"/>; past that the file is too large to preview.
/// </summary>
public sealed class LineWork
{
    public const int MaxPoints = 2_000_000;

    /// <summary>Longer text is cut: a preview cannot show more of one line.</summary>
    public const int MaxLabelLength = 200;

    /// <summary>More labels than this make the picture slow to draw and unreadable; past it the file is too large.</summary>
    public const int MaxLabels = 20_000;

    private readonly List<Point[]> _lines = [];
    private readonly List<Label> _labels = [];

    public IReadOnlyList<Point[]> Lines => _lines;

    public IReadOnlyList<Label> Labels => _labels;

    public int Points { get; private set; }

    /// <summary>The file holds more than can be drawn in time; nothing more is added.</summary>
    public bool TooLarge { get; private set; }

    /// <summary>Adds a line of two or more points; a point that is not a finite number drops the line, as from a damaged file.</summary>
    public void Add(Point[] line)
    {
        if (TooLarge || line.Length < 2 || line.Any(point => !double.IsFinite(point.X) || !double.IsFinite(point.Y)))
        {
            return;
        }
        if (Points + line.Length > MaxPoints)
        {
            TooLarge = true;
            return;
        }
        _lines.Add(line);
        Points += line.Length;
    }

    /// <summary>Adds a line of text; it is drawn over the lines but does not widen the picture.</summary>
    public void Add(Label label)
    {
        if (TooLarge || string.IsNullOrWhiteSpace(label.Text) || !double.IsFinite(label.At.X) || !double.IsFinite(label.At.Y)
            || !(label.Height > 0) || !double.IsFinite(label.Height) || !double.IsFinite(label.Rotation))
        {
            return;
        }
        if (label.Text.Length > MaxLabelLength)
        {
            label = label with { Text = label.Text[..MaxLabelLength] };
        }
        if (++Points > MaxPoints || _labels.Count >= MaxLabels)
        {
            TooLarge = true;
            return;
        }
        _labels.Add(label);
    }

    public void GiveUp() => TooLarge = true;
}

/// <summary>Draws <see cref="LineWork"/> as dark lines on white, fitted to the picture, with y pointing up as in CAD.</summary>
public static class LineDrawing
{
    private const int Margin = 16;
    private const float LineWidth = 1.2f;
    private static readonly System.Drawing.Color Ink = System.Drawing.Color.FromArgb(32, 32, 32);

    /// <summary>
    /// PNG bytes <paramref name="width"/> pixels wide, as tall as the drawing's shape needs (a quarter to
    /// one and a quarter of the width); null when there is nothing with a size to draw.
    /// </summary>
    public static byte[]? Render(LineWork work, int width) => Render(work.Lines, width, work.Labels);

    /// <inheritdoc cref="Render(LineWork, int)"/>
    public static byte[]? Render(IReadOnlyList<Point[]> lines, int width, IReadOnlyList<Label>? labels = null)
    {
        var points = lines.SelectMany(line => line).ToList();
        if (points.Count == 0)
        {
            return null;
        }
        double left = points.Min(p => p.X), right = points.Max(p => p.X);
        double bottom = points.Min(p => p.Y), top = points.Max(p => p.Y);
        double across = right - left, up = top - bottom;
        if (across <= 0 && up <= 0)
        {
            return null;
        }
        int height = (int)Math.Clamp(across > 0 ? width * up / across : width, width / 4.0, width * 1.25);
        double scale = Math.Min(
            across > 0 ? (width - 2 * Margin) / across : double.MaxValue,
            up > 0 ? (height - 2 * Margin) / up : double.MaxValue);
        if (!double.IsFinite(scale) || scale <= 0)
        {
            return null; // too small to measure
        }
        // Centred both ways: one of the two directions has room to spare.
        double offsetX = (width - across * scale) / 2, offsetY = (height - up * scale) / 2;
        System.Drawing.PointF OnPicture(Point p) =>
            new((float)(offsetX + (p.X - left) * scale), (float)(height - offsetY - (p.Y - bottom) * scale));

        using var picture = new System.Drawing.Bitmap(width, height);
        using (var graphics = System.Drawing.Graphics.FromImage(picture))
        using (var pen = new System.Drawing.Pen(Ink, LineWidth) { LineJoin = LineJoin.Round })
        {
            graphics.Clear(System.Drawing.Color.White);
            graphics.SmoothingMode = SmoothingMode.AntiAlias;
            foreach (Point[] line in lines)
            {
                graphics.DrawLines(pen, line.Select(OnPicture).ToArray());
            }
            DrawLabels(graphics, labels ?? [], scale, OnPicture);
        }
        using var png = new MemoryStream();
        picture.Save(png, ImageFormat.Png);
        return png.ToArray();
    }

    /// <summary>Text too small to read is left out; it would only grey the picture.</summary>
    private const float MinTextPixels = 3;

    private static void DrawLabels(System.Drawing.Graphics graphics, IReadOnlyList<Label> labels, double scale, Func<Point, System.Drawing.PointF> onPicture)
    {
        using var ink = new System.Drawing.SolidBrush(Ink);
        // Text in a drawing comes in a few sizes, so one font is made per size, not per label.
        var fonts = new Dictionary<int, System.Drawing.Font>();
        try
        {
            foreach (Label label in labels)
            {
                DrawLabel(graphics, label, scale, onPicture, ink, fonts);
            }
        }
        finally
        {
            foreach (System.Drawing.Font font in fonts.Values)
            {
                font.Dispose();
            }
        }
    }

    private static void DrawLabel(System.Drawing.Graphics graphics, Label label, double scale, Func<Point, System.Drawing.PointF> onPicture,
        System.Drawing.Brush ink, Dictionary<int, System.Drawing.Font> fonts)
    {
        float pixels = (float)(label.Height * scale);
        if (pixels < MinTextPixels || pixels > graphics.VisibleClipBounds.Height)
        {
            return;
        }
        // CAD text height is its capitals'; a font's em is about a third taller. The baseline sits at the point.
        int em = (int)Math.Round(pixels * 1.4f);
        if (!fonts.TryGetValue(em, out System.Drawing.Font? font))
        {
            font = new System.Drawing.Font("Arial", em, System.Drawing.GraphicsUnit.Pixel);
            fonts[em] = font;
        }
        System.Drawing.PointF at = onPicture(label.At);
        var state = graphics.Save();
        graphics.TranslateTransform(at.X, at.Y);
        graphics.RotateTransform((float)-label.Rotation);
        float ascent = font.Size * font.FontFamily.GetCellAscent(font.Style) / font.FontFamily.GetEmHeight(font.Style);
        graphics.DrawString(label.Text, font, ink, 0, -ascent, System.Drawing.StringFormat.GenericTypographic);
        graphics.Restore(state);
    }

    /// <summary>The points of an arc around <paramref name="centre"/>, counter-clockwise from <paramref name="start"/> through <paramref name="sweep"/> radians.</summary>
    public static Point[] Arc(Point centre, double radiusX, double radiusY, double rotation, double start, double sweep)
    {
        const int SegmentsPerTurn = 48;
        if (!double.IsFinite(sweep) || !double.IsFinite(start))
        {
            return [];
        }
        // Past a whole turn an arc only retraces itself; a file asking for more is not believed.
        sweep = Math.Clamp(sweep, -2 * Math.PI, 2 * Math.PI);
        int segments = Math.Max(2, (int)Math.Ceiling(Math.Abs(sweep) / (2 * Math.PI) * SegmentsPerTurn));
        var points = new Point[segments + 1];
        double cos = Math.Cos(rotation), sin = Math.Sin(rotation);
        for (int i = 0; i <= segments; i++)
        {
            double angle = start + sweep * i / segments;
            double x = radiusX * Math.Cos(angle), y = radiusY * Math.Sin(angle);
            points[i] = new Point(centre.X + x * cos - y * sin, centre.Y + x * sin + y * cos);
        }
        return points;
    }
}
