using System.Windows.Input;

namespace Files.Services;

/// <summary>
/// The rules for one system-wide shortcut, apart from Windows so they can be tested: a new
/// combination is registered before the old one is let go, so a combination another program
/// holds never leaves no shortcut at all; and the shortcut can be set aside while it is recorded.
/// </summary>
/// <param name="register">RegisterHotKey for this slot's window: an id, the modifiers and the virtual key.</param>
/// <param name="unregister">UnregisterHotKey for an id.</param>
public sealed class HotkeySlot(Func<int, uint, uint, bool> register, Func<int, bool> unregister)
{
    private const uint ModNoRepeat = 0x4000;

    // Two ids, so the new combination can be held alongside the old one until it is known to work.
    private int _id;
    private string? _suspended;

    /// <summary>The combination that works now, or null when there is none.</summary>
    public string? Active { get; private set; }

    /// <summary>Whether the id belongs to the combination that works now.</summary>
    public bool Owns(int id) => id != 0 && id == _id;

    /// <summary>Makes <paramref name="text"/> the shortcut. On a problem, the old one keeps working; returns the problem in words, or null.</summary>
    public string? Register(string text)
    {
        if (text.Equals(Hotkey.Off, StringComparison.OrdinalIgnoreCase))
        {
            Release();
            return null;
        }
        if (Hotkey.Parse(text) is not (ModifierKeys modifiers, Key key))
        {
            return $"\"{text}\" is not a shortcut";
        }
        if (Active == text)
        {
            return null;
        }
        int next = _id == 1 ? 2 : 1;
        if (!register(next, (uint)modifiers | ModNoRepeat, (uint)KeyInterop.VirtualKeyFromKey(key)))
        {
            return $"{text} is already used by another program";
        }
        Release();
        _id = next;
        Active = text;
        return null;
    }

    /// <summary>Lets the combination go while a shortcut box records keys, so pressing it reaches the box.</summary>
    public void Suspend()
    {
        _suspended ??= Active ?? "";
        Release();
    }

    /// <summary>Takes the shortcut back: <paramref name="wanted"/> if it can be had, else the one held before. Returns the one kept.</summary>
    public (string Kept, string? Problem) Resume(string wanted)
    {
        string before = _suspended ?? Active ?? "";
        _suspended = null;
        string? problem = Register(wanted);
        if (problem is null)
        {
            return (wanted, null);
        }
        if (before.Length > 0)
        {
            Register(before);
        }
        return (Active ?? wanted, problem);
    }

    private void Release()
    {
        if (_id != 0)
        {
            unregister(_id);
            _id = 0;
        }
        Active = null;
    }
}
