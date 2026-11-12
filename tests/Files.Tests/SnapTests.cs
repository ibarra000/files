using System.Windows;
using Files.UI;

namespace Files.Tests;

public sealed class SnapTests
{
    private static readonly Rect Window = new(100, 100, 600, 400);
    private static readonly Rect Button = new(650, 115, 36, 32);

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
        Assert.Equal(expected, Snap.HitTest(Window, Button, new Point(x, y), border: 6));
    }

    [Fact]
    public void The_snap_button_answers_as_the_maximize_button()
    {
        Assert.Equal(Snap.MaxButton, Snap.HitTest(Window, Button, new Point(660, 130), border: 6));
    }

    [Fact]
    public void Everything_else_is_the_panel_itself()
    {
        Assert.Equal(Snap.Client, Snap.HitTest(Window, Button, new Point(300, 250), border: 6));
    }

    [Fact]
    public void A_maximized_panel_has_no_resize_edges()
    {
        Assert.Equal(Snap.Client, Snap.HitTest(Window, Button, new Point(101, 300), border: 0));
    }
}
