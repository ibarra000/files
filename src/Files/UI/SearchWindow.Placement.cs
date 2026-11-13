using System.Windows;
using Files.Services;

namespace Files.UI;

/// <summary>Where the panel shows, what it keeps of its place and size, and where it can be dragged from.</summary>
public partial class SearchWindow
{
    /// <summary>Remembers where the panel is and how big, unless it is maximized (that state is kept as it is).</summary>
    private void SavePlacement()
    {
        Settings saved = _app.Settings;
        if (WindowState != WindowState.Normal)
        {
            return;
        }
        bool moved = saved.WindowLeft != Left || saved.WindowTop != Top;
        bool resized = saved.WindowWidth != Width || saved.WindowHeight != Height;
        if (moved || resized)
        {
            _app.ApplySettings(saved with { WindowLeft = Left, WindowTop = Top, WindowWidth = Width, WindowHeight = Height });
        }
    }

    private void PlaceWindow()
    {
        if (WindowState == WindowState.Minimized)
        {
            // Win+Down minimizes it, and with no taskbar button it could never be brought back.
            WindowState = WindowState.Normal;
        }
        if (WindowState == WindowState.Maximized)
        {
            return;
        }
        Rect area = SystemParameters.WorkArea;
        // A size saved on a larger screen still has to fit this one.
        Width = Math.Clamp(_app.Settings.WindowWidth ?? Width, MinWidth, Math.Max(MinWidth, area.Width));
        Height = Math.Clamp(_app.Settings.WindowHeight ?? Height, MinHeight, Math.Max(MinHeight, area.Height));
        double left = _app.Settings.WindowLeft ?? area.Left + (area.Width - Width) / 2;
        double top = _app.Settings.WindowTop ?? area.Top + area.Height * DefaultTopFraction;
        // Pull a position saved on a monitor that is gone back onto the screen.
        bool onScreen = left >= SystemParameters.VirtualScreenLeft
            && top >= SystemParameters.VirtualScreenTop
            && left + Width <= SystemParameters.VirtualScreenLeft + SystemParameters.VirtualScreenWidth
            && top + Height <= SystemParameters.VirtualScreenTop + SystemParameters.VirtualScreenHeight;
        Left = onScreen ? left : area.Left + (area.Width - Width) / 2;
        Top = onScreen ? top : area.Top + area.Height * DefaultTopFraction;
    }

    /// <summary>
    /// Whether <paramref name="screen"/> (in screen pixels) is over the empty padding rather than the box, a result or the footer.
    /// Windows then drags the panel itself, which also works while it is maximized or snapped, where DragMove does nothing.
    /// </summary>
    private bool IsOverDragArea(Point screen)
    {
        if (PresentationSource.FromVisual(this) is null)
        {
            return false;
        }
        try
        {
            return InputHitTest(PointFromScreen(screen)) == DragArea;
        }
        catch (InvalidOperationException)
        {
            // The window is being torn down; this runs on every mouse move, so it must not report an error.
            return false;
        }
    }
}
