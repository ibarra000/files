using System.Windows;
using Files.Services;
using Xunit;

namespace Files.Tests;

public sealed class ThumbnailCropTests
{
    private const int Size = 720;
    private const int Stride = Size * 4;

    /// <summary>A BGRA canvas of one colour (alpha 0 for transparent).</summary>
    private static byte[] Canvas(byte grey, byte alpha)
    {
        var pixels = new byte[Size * Stride];
        for (int i = 0; i < pixels.Length; i += 4)
        {
            pixels[i] = pixels[i + 1] = pixels[i + 2] = grey;
            pixels[i + 3] = alpha;
        }
        return pixels;
    }

    private static void Paint(byte[] pixels, Int32Rect area, byte grey)
    {
        for (int y = area.Y; y < area.Y + area.Height; y++)
        {
            for (int x = area.X; x < area.X + area.Width; x++)
            {
                int i = y * Stride + x * 4;
                pixels[i] = pixels[i + 1] = pixels[i + 2] = grey;
                pixels[i + 3] = 255;
            }
        }
    }

    [Fact]
    public void A_picture_drawn_in_a_corner_of_a_white_canvas_is_cropped_to_that_picture()
    {
        // As Creo's handler does: its 256-pixel picture in the bottom-left, the rest opaque white.
        byte[] pixels = Canvas(255, 255);
        var drawn = new Int32Rect(0, Size - 256, 256, 256);
        Paint(pixels, drawn, 0xE8);

        Assert.Equal(drawn, ThumbnailCrop.ContentBounds(pixels, Size, Size, Stride));
    }

    [Fact]
    public void A_picture_on_a_transparent_canvas_is_cropped_to_that_picture()
    {
        byte[] pixels = Canvas(0, 0);
        var drawn = new Int32Rect(200, 300, 120, 100);
        Paint(pixels, drawn, 0x40);

        Assert.Equal(drawn, ThumbnailCrop.ContentBounds(pixels, Size, Size, Stride));
    }

    [Fact]
    public void Shades_close_to_the_background_count_as_background()
    {
        byte[] pixels = Canvas(255, 255);
        Paint(pixels, new Int32Rect(0, 0, 50, 50), 252);
        var drawn = new Int32Rect(300, 300, 40, 40);
        Paint(pixels, drawn, 0x80);

        Assert.Equal(drawn, ThumbnailCrop.ContentBounds(pixels, Size, Size, Stride));
    }

    [Fact]
    public void A_picture_that_fills_the_bitmap_is_left_whole()
    {
        byte[] pixels = Canvas(255, 255);
        Paint(pixels, new Int32Rect(1, 1, Size - 2, Size - 2), 0x80);
        // The corners are still white, but the drawing reaches every edge.
        Paint(pixels, new Int32Rect(0, Size / 2, Size, 1), 0x80);
        Paint(pixels, new Int32Rect(Size / 2, 0, 1, Size), 0x80);

        Assert.Null(ThumbnailCrop.ContentBounds(pixels, Size, Size, Stride));
    }

    [Fact]
    public void A_page_with_white_margins_that_mostly_fills_the_bitmap_keeps_its_margins()
    {
        byte[] pixels = Canvas(255, 255);
        Paint(pixels, new Int32Rect(60, 40, 600, 640), 0x30);

        Assert.Null(ThumbnailCrop.ContentBounds(pixels, Size, Size, Stride));
    }

    [Fact]
    public void A_speck_is_not_cropped_to()
    {
        byte[] pixels = Canvas(255, 255);
        Paint(pixels, new Int32Rect(10, 10, 20, 20), 0x30);

        Assert.Null(ThumbnailCrop.ContentBounds(pixels, Size, Size, Stride));
    }

    [Fact]
    public void A_bitmap_of_only_background_is_left_whole()
    {
        Assert.Null(ThumbnailCrop.ContentBounds(Canvas(255, 255), Size, Size, Stride));
    }

    [Fact]
    public void A_bitmap_whose_corners_share_no_colour_is_left_whole()
    {
        byte[] pixels = Canvas(255, 255);
        Paint(pixels, new Int32Rect(0, 0, 1, 1), 0x10);
        Paint(pixels, new Int32Rect(Size - 1, 0, 1, 1), 0x50);
        Paint(pixels, new Int32Rect(0, Size - 1, 1, 1), 0x90);

        Assert.Null(ThumbnailCrop.ContentBounds(pixels, Size, Size, Stride));
    }
}
