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
        // The width the panel has without the preview pane, which adds its own when shown.
        double width = Width - _previewAdded;
        bool moved = saved.WindowLeft != Left || saved.WindowTop != Top;
        bool resized = saved.WindowWidth != width || saved.WindowHeight != Height;
        if (moved || resized)
        {
            _app.ApplySettings(saved with { WindowLeft = Left, WindowTop = Top, WindowWidth = width, WindowHeight = Height });
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
        // The monitor the panel was last on, or the one with the mouse the first time.
        double? savedLeft = _app.Settings.WindowLeft;
        double? savedTop = _app.Settings.WindowTop;
        Rect area = savedLeft is double atLeft && savedTop is double atTop ? WorkAreaAt(atLeft, atTop) : WorkAreaUnderMouse();
        // The saved size is without the preview pane, which WidenForPreview adds back below.
        MinWidth = ListMinWidth;
        // A size saved on a larger screen still has to fit this one.
        Width = Math.Clamp(_app.Settings.WindowWidth ?? Width - _previewAdded, MinWidth, Math.Max(MinWidth, area.Width));
        Height = Math.Clamp(_app.Settings.WindowHeight ?? Height, MinHeight, Math.Max(MinHeight, area.Height));
        _previewAdded = 0;
        double left = savedLeft ?? area.Left + (area.Width - Width) / 2;
        double top = savedTop ?? area.Top + area.Height * DefaultTopFraction;
        // Pull a position saved on a monitor that is gone, or in a gap between monitors, back onto one.
        bool onScreen = area.Contains(left, top)
            && left + Width <= SystemParameters.VirtualScreenLeft + SystemParameters.VirtualScreenWidth
            && top + Height <= SystemParameters.VirtualScreenTop + SystemParameters.VirtualScreenHeight;
        Left = onScreen ? left : area.Left + (area.Width - Width) / 2;
        Top = onScreen ? top : area.Top + area.Height * DefaultTopFraction;
        if (PreviewShown)
        {
            WidenForPreview();
        }
    }

    /// <summary>The work area (the screen less the taskbar), in WPF units, of the monitor the panel is on.</summary>
    private Rect CurrentWorkArea()
    {
        IntPtr handle = new System.Windows.Interop.WindowInteropHelper(this).Handle;
        System.Windows.Forms.Screen screen = handle == IntPtr.Zero
            ? System.Windows.Forms.Screen.PrimaryScreen ?? System.Windows.Forms.Screen.AllScreens[0]
            : System.Windows.Forms.Screen.FromHandle(handle);
        return InWpfUnits(screen.WorkingArea);
    }

    /// <summary>The work area of the monitor holding a point given in WPF units, or the nearest one.</summary>
    private Rect WorkAreaAt(double left, double top)
    {
        DpiScale dpi = System.Windows.Media.VisualTreeHelper.GetDpi(this);
        var pixel = new System.Drawing.Point((int)(left * dpi.DpiScaleX), (int)(top * dpi.DpiScaleY));
        return InWpfUnits(System.Windows.Forms.Screen.FromPoint(pixel).WorkingArea);
    }

    private Rect WorkAreaUnderMouse() =>
        InWpfUnits(System.Windows.Forms.Screen.FromPoint(System.Windows.Forms.Cursor.Position).WorkingArea);

    private Rect InWpfUnits(System.Drawing.Rectangle pixels)
    {
        DpiScale dpi = System.Windows.Media.VisualTreeHelper.GetDpi(this);
        return new Rect(pixels.Left / dpi.DpiScaleX, pixels.Top / dpi.DpiScaleY, pixels.Width / dpi.DpiScaleX, pixels.Height / dpi.DpiScaleY);
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
