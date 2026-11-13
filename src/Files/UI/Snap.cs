using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;

namespace Files.UI;

/// <summary>
/// Lets the frameless panel behave like a normal window for Windows snapping:
/// Win+Arrow, Win+Z for the snap layouts, dragging to a screen edge, and
/// resizing from its edges. Its empty padding is a title bar, so it drags
/// out of maximized or snapped like any window.
/// </summary>
/// <remarks>
/// The window keeps the resizable styles (ResizeMode CanResize) so Windows
/// treats it as snappable, but answers WM_NCCALCSIZE so the whole window is
/// client area and no frame is drawn. WM_NCHITTEST then tells Windows where
/// the edges and the title bar are.
/// </remarks>
internal static class Snap
{
    public const int Client = 1;
    public const int Caption = 2;
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
    private const int WmNcActivate = 0x0086;
    private const int MonitorDefaultToNearest = 2;
    private const double ResizeBorder = 6;

    private const uint SwpNoSize = 0x0001;
    private const uint SwpNoMove = 0x0002;
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoActivate = 0x0010;
    private const uint SwpFrameChanged = 0x0020;

    /// <summary>Hooks the window's messages. Keep the returned hook for the window's lifetime so it is never collected.</summary>
    /// <param name="isDragArea">Whether a point, in screen pixels, is over the part of the window that moves it.</param>
    public static HwndSourceHook? Attach(Window window, Func<Point, bool> isDragArea)
    {
        IntPtr handle = new WindowInteropHelper(window).Handle;
        if (HwndSource.FromHwnd(handle) is not HwndSource source)
        {
            return null;
        }
        HwndSourceHook hook = (IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled) =>
            OnMessage(window, isDragArea, hwnd, message, wParam, lParam, ref handled);
        source.AddHook(hook);
        // Windows sized the frame when the window was created, before this hook;
        // asking again drops it, or a light resize border shows around the panel.
        SetWindowPos(handle, IntPtr.Zero, 0, 0, 0, 0, SwpNoMove | SwpNoSize | SwpNoZOrder | SwpNoActivate | SwpFrameChanged);
        return hook;
    }

    /// <summary>What part of the window <paramref name="cursor"/> is over, as a WM_NCHITTEST code. All in screen pixels.</summary>
    /// <param name="overDragArea">The cursor is over empty padding, which drags the window like a title bar.</param>
    public static int HitTest(Rect window, Point cursor, double border, bool overDragArea)
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
        return overDragArea ? Caption : Client;
    }

    /// <summary>Screen coordinates come packed in lParam as two signed 16-bit values; left of or above the main screen they are negative.</summary>
    public static Point CursorFrom(IntPtr lParam)
    {
        long packed = lParam.ToInt64();
        short x = (short)(packed & 0xFFFF);
        short y = (short)((packed >> 16) & 0xFFFF);
        return new Point(x, y);
    }

    private static IntPtr OnMessage(Window window, Func<Point, bool> isDragArea, IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        switch (message)
        {
            case WmNcCalcSize when wParam != IntPtr.Zero:
                // The whole window is client area: Windows draws no frame or caption.
                handled = true;
                return IntPtr.Zero;
            case WmNcActivate:
                // Windows repaints the frame on focus changes; -1 tells it not to,
                // or the inactive frame shows as a light border around the panel.
                handled = true;
                return DefWindowProc(hwnd, message, wParam, new IntPtr(-1));
            case WmGetMinMaxInfo:
                // Not handled: WPF still applies MinWidth and MinHeight after this.
                MaximizeToWorkArea(hwnd, lParam);
                return IntPtr.Zero;
            case WmNcHitTest:
                int hit = HitTestAt(window, isDragArea, hwnd, lParam);
                if (hit == Client)
                {
                    return IntPtr.Zero;
                }
                handled = true;
                return hit;
            default:
                return IntPtr.Zero;
        }
    }

    private static int HitTestAt(Window window, Func<Point, bool> isDragArea, IntPtr hwnd, IntPtr lParam)
    {
        GetWindowRect(hwnd, out Rect32 frame);
        var bounds = new Rect(frame.Left, frame.Top, frame.Right - frame.Left, frame.Bottom - frame.Top);
        double scale = VisualTreeHelper.GetDpi(window).DpiScaleX;
        double border = window.WindowState == WindowState.Maximized ? 0 : ResizeBorder * scale;
        Point cursor = CursorFrom(lParam);
        return HitTest(bounds, cursor, border, isDragArea(cursor));
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
    private static extern IntPtr DefWindowProc(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hwnd, IntPtr insertAfter, int x, int y, int width, int height, uint flags);

    [DllImport("user32.dll")]
    private static extern IntPtr MonitorFromWindow(IntPtr hwnd, int flags);

    [DllImport("user32.dll")]
    private static extern bool GetMonitorInfo(IntPtr monitor, ref MonitorInfo info);
}
