using System.Runtime.InteropServices;
using System.Windows.Input;
using System.Windows.Interop;

namespace Files.Services;

/// <summary>A system-wide shortcut, registered with RegisterHotKey on a hidden message window.</summary>
public sealed class Hotkey : IDisposable
{
    public const string Off = "Off";
    private const int WmHotkey = 0x0312;
    private const int Id = 1;
    private const uint ModNoRepeat = 0x4000;

    private readonly HwndSource _window;
    private bool _registered;

    public Hotkey()
    {
        // HWND_MESSAGE (-3): a message-only window, never shown.
        _window = new HwndSource(new HwndSourceParameters("files-hotkey") { ParentWindow = new IntPtr(-3) });
        _window.AddHook(OnMessage);
    }

    public event Action? Pressed;

    /// <summary>Registers <paramref name="text"/>, replacing any earlier shortcut. Returns a problem in words, or null.</summary>
    public string? Register(string text)
    {
        if (_registered)
        {
            UnregisterHotKey(_window.Handle, Id);
            _registered = false;
        }
        if (text.Equals(Off, StringComparison.OrdinalIgnoreCase))
        {
            return null;
        }
        if (Parse(text) is not (ModifierKeys modifiers, Key key))
        {
            return $"\"{text}\" is not a shortcut";
        }
        int virtualKey = KeyInterop.VirtualKeyFromKey(key);
        _registered = RegisterHotKey(_window.Handle, Id, (uint)modifiers | ModNoRepeat, (uint)virtualKey);
        return _registered ? null : $"{text} is already used by another program";
    }

    /// <summary>Parses text such as "Ctrl+Shift+Space". A shortcut needs at least one modifier.</summary>
    public static (ModifierKeys, Key)? Parse(string text)
    {
        string[] parts = text.Split('+', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length < 2)
        {
            return null;
        }
        var modifiers = ModifierKeys.None;
        foreach (string part in parts[..^1])
        {
            modifiers |= part.ToLowerInvariant() switch
            {
                "ctrl" or "control" => ModifierKeys.Control,
                "alt" => ModifierKeys.Alt,
                "shift" => ModifierKeys.Shift,
                "win" => ModifierKeys.Windows,
                _ => (ModifierKeys)(-1),
            };
        }
        bool validModifiers = !modifiers.HasFlag((ModifierKeys)(-1));
        return validModifiers && Enum.TryParse(parts[^1], ignoreCase: true, out Key key) ? (modifiers, key) : null;
    }

    public static string Format(ModifierKeys modifiers, Key key)
    {
        var parts = new List<string>();
        if (modifiers.HasFlag(ModifierKeys.Control))
        {
            parts.Add("Ctrl");
        }
        if (modifiers.HasFlag(ModifierKeys.Alt))
        {
            parts.Add("Alt");
        }
        if (modifiers.HasFlag(ModifierKeys.Shift))
        {
            parts.Add("Shift");
        }
        if (modifiers.HasFlag(ModifierKeys.Windows))
        {
            parts.Add("Win");
        }
        parts.Add(key.ToString());
        return string.Join('+', parts);
    }

    public void Dispose()
    {
        Register(Off);
        _window.Dispose();
    }

    private IntPtr OnMessage(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (message == WmHotkey && wParam.ToInt32() == Id)
        {
            handled = true;
            Pressed?.Invoke();
        }
        return IntPtr.Zero;
    }

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool RegisterHotKey(IntPtr hwnd, int id, uint modifiers, uint virtualKey);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnregisterHotKey(IntPtr hwnd, int id);
}
