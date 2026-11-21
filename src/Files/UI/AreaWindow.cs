using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Shapes;
using System.Windows.Threading;
using Files.Services;

namespace Files.UI;

/// <summary>
/// A dimmed layer over every screen to drag out the area whose text is read. It never takes
/// focus, so the window being read keeps looking the way it did. The corners are taken from
/// the pointer in physical pixels, the space the screen is captured in, so the area read is
/// exactly the one dragged on any monitor at any scale; the rectangle drawn only shows it.
/// Esc or a right-click cancels.
/// </summary>
public sealed class AreaWindow : Window
{
    private const int GwlExStyle = -20;
    private const int WsExNoActivate = 0x08000000;
    private const int WsExToolWindow = 0x00000080;
    private const int VkEscape = 0x1B;
    private static readonly TimeSpan EscapeCheck = TimeSpan.FromMilliseconds(30);

    private readonly TaskCompletionSource<System.Drawing.Rectangle?> _picked = new();
    private readonly Canvas _canvas = new();
    private readonly Rectangle _box = new() { Stroke = Brushes.White, StrokeThickness = 1.5, Fill = new SolidColorBrush(Color.FromArgb(0x30, 0xFF, 0xFF, 0xFF)), Visibility = Visibility.Collapsed };
    private readonly DispatcherTimer _escape;
    private readonly TextBlock _hint = new() { Text = "Drag over the text to read · Esc or a right-click cancels", Foreground = Brushes.White, FontSize = 14 };
    private System.Drawing.Point? _from;
    private bool _closing;

    private AreaWindow()
    {
        WindowStyle = WindowStyle.None;
        AllowsTransparency = true;
        Background = new SolidColorBrush(Color.FromArgb(0x55, 0, 0, 0));
        Topmost = true;
        ShowInTaskbar = false;
        ShowActivated = false;
        Cursor = Cursors.Cross;
        ResizeMode = ResizeMode.NoResize;
        WindowStartupLocation = WindowStartupLocation.Manual;
        _canvas.Children.Add(_box);
        _canvas.Children.Add(_hint);
        Content = _canvas;
        _escape = new DispatcherTimer(EscapeCheck, DispatcherPriority.Input, (_, _) => CancelOnEscape(), Dispatcher);
        SourceInitialized += (_, _) => CoverScreens();
        // Moved onto a screen of another scale, Windows resizes the layer by that scale; it is put back over every screen.
        DpiChanged += (_, _) => Dispatcher.BeginInvoke(CoverScreens);
        // A screen plugged in, removed or rescaled while picking changes the area to cover.
        Microsoft.Win32.SystemEvents.DisplaySettingsChanged += OnDisplaySettingsChanged;
        Loaded += (_, _) => PlaceHint();
        MouseLeftButtonDown += (_, _) => Start();
        MouseMove += (_, _) => Track();
        MouseLeftButtonUp += (_, _) => Finish();
        MouseRightButtonDown += (_, _) => CloseOnce();
        // Another window taking the mouse mid-drag (a UAC prompt, Alt+Tab) would leave a box no button-up ends; the drag starts over.
        LostMouseCapture += (_, _) => ForgetDrag();
        Closed += (_, _) =>
        {
            // SystemEvents holds its handlers for the life of the app, so a closed layer must leave it.
            Microsoft.Win32.SystemEvents.DisplaySettingsChanged -= OnDisplaySettingsChanged;
            // Closed by quitting too, not only by CloseOnce; a screen change queued before then has nothing to cover.
            _closing = true;
            _escape.Stop();
            _picked.TrySetResult(null);
        };
    }

    /// <summary>Shows the layer. <see cref="Picked"/> gives the area dragged.</summary>
    public static AreaWindow Open()
    {
        var window = new AreaWindow();
        window.Show();
        window._escape.Start();
        return window;
    }

    /// <summary>The area dragged, in physical screen pixels, or null when cancelled or too small.</summary>
    public Task<System.Drawing.Rectangle?> Picked => _picked.Task;

    /// <summary>Closes the layer with nothing picked.</summary>
    public void Cancel() => CloseOnce();

    private void Start()
    {
        _from = ScreenText.CursorPosition();
        CaptureMouse();
        Track();
    }

    private void Track()
    {
        if (_from is not System.Drawing.Point from)
        {
            return;
        }
        System.Drawing.Point to = ScreenText.CursorPosition();
        Point a = PointFromScreen(new Point(from.X, from.Y));
        Point b = PointFromScreen(new Point(to.X, to.Y));
        Canvas.SetLeft(_box, Math.Min(a.X, b.X));
        Canvas.SetTop(_box, Math.Min(a.Y, b.Y));
        _box.Width = Math.Abs(a.X - b.X);
        _box.Height = Math.Abs(a.Y - b.Y);
        _box.Visibility = Visibility.Visible;
    }

    private void Finish()
    {
        if (_from is not System.Drawing.Point from)
        {
            return;
        }
        ReleaseMouseCapture();
        _picked.TrySetResult(ScreenText.Area(from, ScreenText.CursorPosition(), System.Windows.Forms.SystemInformation.VirtualScreen));
        CloseOnce();
    }

    private void ForgetDrag()
    {
        _from = null;
        _box.Visibility = Visibility.Collapsed;
    }

    private void OnDisplaySettingsChanged(object? sender, EventArgs e)
    {
        // Raised on a system thread.
        Dispatcher.BeginInvoke(() =>
        {
            if (_closing)
            {
                return;
            }
            CoverScreens();
            PlaceHint();
        });
    }

    /// <summary>Esc, a right-click and the end of a drag can come together; the layer closes once.</summary>
    private void CloseOnce()
    {
        if (_closing)
        {
            return;
        }
        _closing = true;
        Close();
    }

    /// <summary>The hint goes at the top of the main screen, which the middle of all screens together may not be on.</summary>
    private void PlaceHint()
    {
        System.Drawing.Rectangle main = System.Windows.Forms.Screen.PrimaryScreen?.Bounds ?? System.Windows.Forms.SystemInformation.VirtualScreen;
        Point top = PointFromScreen(new Point(main.Left + main.Width / 2.0, main.Top + 24));
        _hint.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        Canvas.SetLeft(_hint, top.X - _hint.DesiredSize.Width / 2);
        Canvas.SetTop(_hint, top.Y);
    }

    private void CancelOnEscape()
    {
        // The layer never has focus, so Esc is watched rather than received.
        if ((GetAsyncKeyState(VkEscape) & 0x8000) != 0)
        {
            CloseOnce();
        }
    }

    /// <summary>Spreads the layer over every monitor in physical pixels and keeps it from taking focus.</summary>
    private void CoverScreens()
    {
        nint handle = new WindowInteropHelper(this).Handle;
        SetWindowLong(handle, GwlExStyle, GetWindowLong(handle, GwlExStyle) | WsExNoActivate | WsExToolWindow);
        System.Drawing.Rectangle screen = System.Windows.Forms.SystemInformation.VirtualScreen;
        const uint NoActivate = 0x0010;
        SetWindowPos(handle, -1, screen.X, screen.Y, screen.Width, screen.Height, NoActivate);
    }

    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int key);

    [DllImport("user32.dll")]
    private static extern int GetWindowLong(nint window, int index);

    [DllImport("user32.dll")]
    private static extern int SetWindowLong(nint window, int index, int value);

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(nint window, nint after, int x, int y, int width, int height, uint flags);
}
