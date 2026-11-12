using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;

namespace Files.UI;

/// <summary>
/// Gives the frameless panel a plain, solid Windows 11 look: the theme's own
/// background, rounded corners with their shadow, and no light outline around a
/// dark panel. Older Windows ignores the attributes it does not know.
/// </summary>
internal static class Backdrop
{
    private const int UseImmersiveDarkMode = 20;
    private const int WindowCornerPreference = 33;
    private const int BorderColor = 34;
    private const int SystemBackdropType = 38;
    private const int RoundCorners = 2;
    private const int NoBackdrop = 1;
    private const int NoBorderColor = unchecked((int)0xFFFFFFFE);

    public static void Apply(Window window, bool dark)
    {
        IntPtr handle = new WindowInteropHelper(window).Handle;
        if (handle == IntPtr.Zero)
        {
            // Not created yet (the panel before it is first shown); SourceInitialized applies it later.
            return;
        }
        // Solid, following light and dark: no translucent backdrop shows through.
        window.SetResourceReference(Control.BackgroundProperty, "ApplicationBackgroundBrush");
        SetAttribute(handle, SystemBackdropType, NoBackdrop);
        SetAttribute(handle, UseImmersiveDarkMode, dark ? 1 : 0);
        SetAttribute(handle, WindowCornerPreference, RoundCorners);
        SetAttribute(handle, BorderColor, NoBorderColor);
    }

    private static void SetAttribute(IntPtr handle, int attribute, int value)
    {
        DwmSetWindowAttribute(handle, attribute, ref value, sizeof(int));
    }

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);
}
