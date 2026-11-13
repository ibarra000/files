using System.Windows;
using Files.UI;

namespace Files.Tests;

public sealed class SnapTests
{
    private static readonly Rect Window = new(100, 100, 600, 400);

    [Theory]
    [InlineData(102, 102, Snap.TopLeft)]
    [InlineData(697, 102, Snap.TopRight)]
    [InlineData(102, 497, Snap.BottomLeft)]
    [InlineData(697, 497, Snap.BottomRight)]
    [InlineData(400, 101, Snap.Top)]
    [InlineData(400, 499, Snap.Bottom)]
    [InlineData(101, 300, Snap.Left)]
    [InlineData(699, 300, Snap.Right)]
    public void The_edges_and_corners_resize(double x, double y, int expected)
    {
        Assert.Equal(expected, Snap.HitTest(Window, new Point(x, y), border: 6, overDragArea: true));
    }

    [Fact]
    public void Everything_else_is_the_panel_itself()
    {
        Assert.Equal(Snap.Client, Snap.HitTest(Window, new Point(300, 250), border: 6, overDragArea: false));
    }

    [Theory]
    [InlineData(0x0010_0020, 32, 16)]
    [InlineData(0xFFFF_FFFF, -1, -1)]
    [InlineData(0x0064_F830, -2000, 100)]
    public void Cursor_positions_unpack_with_their_sign(long packed, double x, double y)
    {
        Assert.Equal(new Point(x, y), Snap.CursorFrom(new IntPtr(packed)));
    }

    [Fact]
    public void A_maximized_panel_has_no_resize_edges()
    {
        Assert.Equal(Snap.Client, Snap.HitTest(Window, new Point(101, 300), border: 0, overDragArea: false));
    }

    [Fact]
    public void The_empty_padding_drags_like_a_title_bar()
    {
        Assert.Equal(Snap.Caption, Snap.HitTest(Window, new Point(300, 250), border: 6, overDragArea: true));
    }

    [Fact]
    public void A_maximized_panel_still_drags_from_its_padding()
    {
        Assert.Equal(Snap.Caption, Snap.HitTest(Window, new Point(101, 300), border: 0, overDragArea: true));
    }
}
