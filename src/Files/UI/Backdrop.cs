using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Interop;
using System.Windows.Media;

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
    private const int CaptionColor = 35;
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
        // Solid, following light and dark: no translucent backdrop shows through. The color is the panel's (UI/PanelTheme.*.xaml).
        window.SetResourceReference(Control.BackgroundProperty, "PanelBackground");
        SetAttribute(handle, SystemBackdropType, NoBackdrop);
        SetAttribute(handle, UseImmersiveDarkMode, dark ? 1 : 0);
        SetAttribute(handle, WindowCornerPreference, RoundCorners);
        SetAttribute(handle, BorderColor, NoBorderColor);
        // A window with a title bar (Settings) gets it in the same color as its body; the frameless panel has none.
        if (window.TryFindResource("PanelBackground") is SolidColorBrush { Color: var color })
        {
            SetAttribute(handle, CaptionColor, color.R | color.G << 8 | color.B << 16);
        }
    }

    private static void SetAttribute(IntPtr handle, int attribute, int value)
    {
        DwmSetWindowAttribute(handle, attribute, ref value, sizeof(int));
    }

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);
}
