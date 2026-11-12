using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;

namespace Files.UI;

/// <summary>
/// Lets the frameless panel behave like a normal window for Windows snapping:
/// Win+Arrow, dragging to a screen edge, resizing from its edges, and the
/// Windows 11 snap-layout flyout over a maximize button the panel draws itself.
/// </summary>
/// <remarks>
/// The window keeps the resizable styles (ResizeMode CanResize) so Windows
/// treats it as snappable, but answers WM_NCCALCSIZE so the whole window is
/// client area and no frame is drawn. WM_NCHITTEST then tells Windows where
/// the edges and the maximize button are; answering HTMAXBUTTON is what makes
/// Windows 11 show the flyout.
/// </remarks>
internal sealed class Snap
{
    public const int Client = 1;
    public const int MaxButton = 9;
    public const int Left = 10;
    public const int Right = 11;
    public const int Top = 12;
    public const int TopLeft = 13;
    public const int TopRight = 14;
    public const int Bottom = 15;
    public const int BottomLeft = 16;
    public const int BottomRight = 17;

    private const int WmGetMinMaxInfo = 0x0024;
    private const int WmNcCalcSize = 0x0083;
    private const int WmNcHitTest = 0x0084;
    private const int WmNcLButtonDown = 0x00A1;
    private const int WmNcMouseLeave = 0x02A2;
    private const int MonitorDefaultToNearest = 2;
    private const double ResizeBorder = 6;

    private readonly Window _window;
    private readonly FrameworkElement _button;
    private readonly Action<bool> _hover;

    private Snap(Window window, FrameworkElement button, Action<bool> hover)
    {
        _window = window;
        _button = button;
        _hover = hover;
    }

    /// <summary>The pointer is over the snap button, where the flyout may take focus for a moment.</summary>
    public bool IsOverButton { get; private set; }

    /// <param name="hover">Called with true and false as the pointer enters and leaves the button.</param>
    public static Snap Attach(Window window, FrameworkElement button, Action<bool> hover)
    {
        var snap = new Snap(window, button, hover);
        IntPtr handle = new WindowInteropHelper(window).Handle;
        HwndSource.FromHwnd(handle)?.AddHook(snap.OnMessage);
        return snap;
    }

    /// <summary>What part of the window <paramref name="cursor"/> is over, as a WM_NCHITTEST code. All in screen pixels.</summary>
    public static int HitTest(Rect window, Rect button, Point cursor, double border)
    {
        bool left = cursor.X < window.Left + border;
        bool right = cursor.X >= window.Right - border;
        bool top = cursor.Y < window.Top + border;
        bool bottom = cursor.Y >= window.Bottom - border;
        if (top && left)
        {
            return TopLeft;
        }
        if (top && right)
        {
            return TopRight;
        }
        if (bottom && left)
        {
            return BottomLeft;
        }
        if (bottom && right)
        {
            return BottomRight;
        }
        if (top)
        {
            return Top;
        }
        if (bottom)
        {
            return Bottom;
        }
        if (left)
        {
            return Left;
        }
        if (right)
        {
            return Right;
        }
        return button.Contains(cursor) ? MaxButton : Client;
    }

    private IntPtr OnMessage(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        switch (message)
        {
            case WmNcCalcSize when wParam != IntPtr.Zero:
                // The whole window is client area: Windows draws no frame or caption.
                handled = true;
                return IntPtr.Zero;
            case WmGetMinMaxInfo:
                // Not handled: WPF still applies MinWidth and MinHeight after this.
                MaximizeToWorkArea(hwnd, lParam);
                return IntPtr.Zero;
            case WmNcHitTest:
                int hit = HitTestAt(hwnd, lParam);
                SetOverButton(hit == MaxButton);
                if (hit == Client)
                {
                    return IntPtr.Zero;
                }
                handled = true;
                return hit;
            case WmNcLButtonDown when wParam == MaxButton:
                // Handled on press: Windows does not send the release for a button it is not drawing.
                handled = true;
                ToggleMaximized();
                return IntPtr.Zero;
            case WmNcMouseLeave:
                SetOverButton(false);
                return IntPtr.Zero;
            default:
                return IntPtr.Zero;
        }
    }

    /// <summary>Screen coordinates come packed in lParam as two signed 16-bit values; left of or above the main screen they are negative.</summary>
    public static Point CursorFrom(IntPtr lParam)
    {
        long packed = lParam.ToInt64();
        short x = (short)(packed & 0xFFFF);
        short y = (short)((packed >> 16) & 0xFFFF);
        return new Point(x, y);
    }

    /// <summary>Forgets that the pointer was over the button, for when the panel hides.</summary>
    public void Reset()
    {
        SetOverButton(false);
    }

    private int HitTestAt(IntPtr hwnd, IntPtr lParam)
    {
        Point cursor = CursorFrom(lParam);
        GetWindowRect(hwnd, out Rect32 frame);
        var window = new Rect(frame.Left, frame.Top, frame.Right - frame.Left, frame.Bottom - frame.Top);
        Point topLeft = _button.PointToScreen(new Point(0, 0));
        Point bottomRight = _button.PointToScreen(new Point(_button.ActualWidth, _button.ActualHeight));
        var button = new Rect(topLeft, bottomRight);
        double scale = VisualTreeHelper.GetDpi(_window).DpiScaleX;
        double border = _window.WindowState == WindowState.Maximized ? 0 : ResizeBorder * scale;
        return HitTest(window, button, cursor, border);
    }

    private void ToggleMaximized()
    {
        if (_window.WindowState == WindowState.Maximized)
        {
            _window.WindowState = WindowState.Normal;
        }
        else
        {
            _window.WindowState = WindowState.Maximized;
        }
    }

    private void SetOverButton(bool over)
    {
        if (over != IsOverButton)
        {
            IsOverButton = over;
            _hover(over);
        }
    }

    /// <summary>A maximized window would otherwise hang past the screen by its frame width and cover the taskbar.</summary>
    private static void MaximizeToWorkArea(IntPtr hwnd, IntPtr lParam)
    {
        var monitor = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
        if (!GetMonitorInfo(MonitorFromWindow(hwnd, MonitorDefaultToNearest), ref monitor))
        {
            return;
        }
        MinMaxInfo info = Marshal.PtrToStructure<MinMaxInfo>(lParam);
        // The position is relative to the monitor, the size is the work area's.
        info.MaxPosition = new Point32 { X = monitor.Work.Left - monitor.Monitor.Left, Y = monitor.Work.Top - monitor.Monitor.Top };
        info.MaxSize = new Point32 { X = monitor.Work.Right - monitor.Work.Left, Y = monitor.Work.Bottom - monitor.Work.Top };
        Marshal.StructureToPtr(info, lParam, fDeleteOld: false);
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Rect32
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Point32
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MinMaxInfo
    {
        public Point32 Reserved;
        public Point32 MaxSize;
        public Point32 MaxPosition;
        public Point32 MinTrackSize;
        public Point32 MaxTrackSize;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MonitorInfo
    {
        public int Size;
        public Rect32 Monitor;
        public Rect32 Work;
        public int Flags;
    }

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hwnd, out Rect32 rect);

    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromWindow(IntPtr hwnd, int flags);

    [DllImport("user32.dll")]
    private static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);
}
