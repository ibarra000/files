using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Interop;
using System.Windows.Media;

namespace Files.UI;

/// <summary>
/// Gives a frameless window the Windows 11 look: rounded corners, a shadow and
/// the acrylic backdrop that transient windows such as Start search use.
/// Older Windows ignores the attributes it does not know and shows a plain window.
/// </summary>
internal static class Backdrop
{
    private const int UseImmersiveDarkMode = 20;
    private const int WindowCornerPreference = 33;
    private const int SystemBackdropType = 38;
    private const int RoundCorners = 2;
    private const int TransientWindowBackdrop = 3;

    public static void Apply(Window window, bool dark)
    {
        IntPtr handle = new WindowInteropHelper(window).Handle;
        if (handle == IntPtr.Zero)
        {
            // Not created yet (the panel before it is first shown); SourceInitialized applies it later.
            return;
        }
        if (HwndSource.FromHwnd(handle) is not HwndSource source)
        {
            return;
        }
        // Let DWM's backdrop show through WPF's own background.
        window.Background = Brushes.Transparent;
        source.CompositionTarget.BackgroundColor = Colors.Transparent;
        var margins = new Margins { Left = -1, Right = -1, Top = -1, Bottom = -1 };
        DwmExtendFrameIntoClientArea(handle, ref margins);
        SetAttribute(handle, UseImmersiveDarkMode, dark ? 1 : 0);
        SetAttribute(handle, WindowCornerPreference, RoundCorners);
        SetAttribute(handle, SystemBackdropType, TransientWindowBackdrop);
    }

    private static void SetAttribute(IntPtr handle, int attribute, int value)
    {
        DwmSetWindowAttribute(handle, attribute, ref value, sizeof(int));
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Margins
    {
        public int Left;
        public int Right;
        public int Top;
        public int Bottom;
    }

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);

    [DllImport("dwmapi.dll")]
    private static extern int DwmExtendFrameIntoClientArea(IntPtr hwnd, ref Margins margins);
}
