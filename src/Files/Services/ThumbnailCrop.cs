using System.Windows;

namespace Files.Services;

/// <summary>
/// Where a thumbnail handler actually drew. Some (Creo's) draw a picture of their own size into one
/// corner of the larger bitmap asked for, and fill the rest with white or leave it transparent.
/// </summary>
internal static class ThumbnailCrop
{
    /// <summary>How far a channel may differ from the background and still count as background.</summary>
    private const int Tolerance = 6;

    /// <summary>
    /// The bounds of what differs from the background, in 32-bit BGRA pixels. The background is a
    /// transparent pixel, or the colour at least three corners share. Null when there is no such
    /// background, nothing but background, the drawing covers more than half the bitmap, or it is a speck.
    /// </summary>
    public static Int32Rect? ContentBounds(byte[] pixels, int width, int height, int stride)
    {
        if (width <= 0 || height <= 0 || Background(pixels, width, height, stride) is not int background)
        {
            return null;
        }
        int left = width;
        int top = height;
        int right = -1;
        int bottom = -1;
        for (int y = 0; y < height; y++)
        {
            int row = y * stride;
            for (int x = 0; x < width; x++)
            {
                if (IsBackground(pixels, row + 4 * x, background))
                {
                    continue;
                }
                left = Math.Min(left, x);
                right = Math.Max(right, x);
                top = Math.Min(top, y);
                bottom = Math.Max(bottom, y);
            }
        }
        if (right < 0)
        {
            return null;
        }
        var bounds = new Int32Rect(left, top, right - left + 1, bottom - top + 1);
        // Only a picture drawn small inside a bigger canvas is cut out. A thumbnail that mostly fills its
        // bitmap keeps its margins (a page's white border), and a speck is noise, not a picture.
        bool small = (long)bounds.Width * bounds.Height <= (long)width * height / 2;
        bool speck = bounds.Width < MinSide && bounds.Height < MinSide;
        return small && !speck ? bounds : null;
    }

    /// <summary>Drawn areas narrower and shorter than this are left uncropped.</summary>
    private const int MinSide = 32;

    /// <summary>The offset of a corner pixel whose colour at least two other corners share, or null.</summary>
    private static int? Background(byte[] pixels, int width, int height, int stride)
    {
        int[] corners = [0, 4 * (width - 1), (height - 1) * stride, (height - 1) * stride + 4 * (width - 1)];
        foreach (int corner in corners)
        {
            if (corners.Count(other => IsBackground(pixels, other, corner)) >= 3)
            {
                return corner;
            }
        }
        return null;
    }

    private static bool IsBackground(byte[] pixels, int pixel, int background)
    {
        if (pixels[pixel + 3] == 0)
        {
            return true;
        }
        for (int channel = 0; channel < 4; channel++)
        {
            if (Math.Abs(pixels[pixel + channel] - pixels[background + channel]) > Tolerance)
            {
                return false;
            }
        }
        return true;
    }
}
