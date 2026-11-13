using System.Runtime.InteropServices;
using System.Windows.Input;
using System.Windows.Interop;

namespace Files.Services;

/// <summary>A system-wide shortcut, registered with RegisterHotKey on a hidden message window.</summary>
public sealed class Hotkey : IDisposable
{
    public const string Off = "Off";
    private const int WmHotkey = 0x0312;

    private readonly HwndSource _window;
    private readonly HotkeySlot _slot;

    public Hotkey()
    {
        // HWND_MESSAGE (-3): a message-only window, never shown.
        _window = new HwndSource(new HwndSourceParameters("files-hotkey") { ParentWindow = new IntPtr(-3) });
        _window.AddHook(OnMessage);
        _slot = new HotkeySlot(
            (id, modifiers, key) => RegisterHotKey(_window.Handle, id, modifiers, key),
            id => UnregisterHotKey(_window.Handle, id));
    }

    public event Action? Pressed;

    /// <summary>The combination that works now, or null.</summary>
    public string? Active => _slot.Active;

    /// <summary>Makes <paramref name="text"/> the shortcut; on a problem the old one keeps working. Returns the problem in words, or null.</summary>
    public string? Register(string text) => _slot.Register(text);

    /// <summary>Lets the combination go while it is being recorded in Settings.</summary>
    public void Suspend() => _slot.Suspend();

    /// <summary>Takes <paramref name="wanted"/>, or the combination held before if it cannot. Returns the one kept.</summary>
    public (string Kept, string? Problem) Resume(string wanted) => _slot.Resume(wanted);

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
        if (message == WmHotkey && _slot.Owns(wParam.ToInt32()))
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
