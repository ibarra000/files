using System.Windows.Input;

namespace Files.Services;

/// <summary>
/// What a key in the panel can do, apart from moving and typing, which keep their keys. A member's name
/// is the key Settings saves its shortcut under, so a renamed member would drop saved shortcuts.
/// </summary>
public enum PanelCommand
{
    Reveal,
    LiveSearch,
    Favorite,
    // Shown as "Copy": Ctrl+C copies the file itself once file operations land.
    CopyPath,
    CopyName,
    Preview,
    Rescan,
    Settings,
    Quit,
    Filter1,
    Filter2,
    Filter3,
    Filter4,
    Filter5,
}

/// <summary>
/// The panel's shortcuts, which Settings can change (<see cref="Settings.PanelKeys"/>). The keys for
/// moving and typing (arrows, Enter, Esc, Tab, Delete, Ctrl+V and the like) are not among them, so no
/// choice can leave the panel unusable.
/// </summary>
public sealed class PanelKeys
{
    // Find runs on every key press in the panel: the bindings are parsed once, here.
    private readonly Dictionary<(ModifierKeys, Key), PanelCommand> _byKeys = [];

    public PanelKeys(IReadOnlyDictionary<PanelCommand, string> bindings)
    {
        Bindings = bindings;
        foreach ((PanelCommand command, string keys) in bindings)
        {
            if (Parse(keys) is { } parsed)
            {
                _byKeys.TryAdd(parsed, command);
            }
        }
    }

    public IReadOnlyDictionary<PanelCommand, string> Bindings { get; }

    public static PanelKeys Defaults { get; } = new(new Dictionary<PanelCommand, string>
    {
        [PanelCommand.Reveal] = "Ctrl+Enter",
        [PanelCommand.LiveSearch] = "Shift+Enter",
        [PanelCommand.Favorite] = "Ctrl+D",
        [PanelCommand.CopyPath] = "Ctrl+C",
        [PanelCommand.CopyName] = "Ctrl+Shift+C",
        [PanelCommand.Preview] = "Ctrl+P",
        [PanelCommand.Rescan] = "F5",
        [PanelCommand.Settings] = "Ctrl+OemComma",
        [PanelCommand.Quit] = "Ctrl+Q",
        [PanelCommand.Filter1] = "Ctrl+D1",
        [PanelCommand.Filter2] = "Ctrl+D2",
        [PanelCommand.Filter3] = "Ctrl+D3",
        [PanelCommand.Filter4] = "Ctrl+D4",
        [PanelCommand.Filter5] = "Ctrl+D5",
    });

    /// <summary>The command on these keys, or null.</summary>
    public PanelCommand? Find(ModifierKeys modifiers, Key key) =>
        _byKeys.TryGetValue((modifiers, key), out PanelCommand command) ? command : null;

    /// <summary>A command's keys as a key cap shows them ("Ctrl ↵"), or joined for a sentence with "+" ("Ctrl+,").</summary>
    public string Label(PanelCommand command, string separator = " ")
    {
        if (Parse(Bindings[command]) is not (ModifierKeys modifiers, Key key))
        {
            return Bindings[command];
        }
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
        parts.Add(KeyCap(key));
        return string.Join(separator, parts);
    }

    /// <summary>What Settings calls a command.</summary>
    public static string Name(PanelCommand command) => command switch
    {
        PanelCommand.Reveal => "Show in folder",
        PanelCommand.LiveSearch => "Search live",
        PanelCommand.Favorite => "Pin to favorites",
        PanelCommand.CopyPath => "Copy",
        PanelCommand.CopyName => "Copy name",
        PanelCommand.Preview => "Preview",
        PanelCommand.Rescan => "Rescan volumes",
        PanelCommand.Settings => "Settings",
        PanelCommand.Quit => "Quit",
        PanelCommand.Filter1 => "Filter: All",
        PanelCommand.Filter2 => "Filter: Files",
        PanelCommand.Filter3 => "Filter: Folders",
        PanelCommand.Filter4 => "Filter: Code",
        PanelCommand.Filter5 => "Filter: Documents",
        _ => command.ToString(),
    };

    /// <summary>
    /// Parses keys such as "Ctrl+P". Unlike a system-wide shortcut a key may stand alone ("F5"); which
    /// lone keys may be a shortcut is for <see cref="From"/> to say.
    /// </summary>
    public static (ModifierKeys, Key)? Parse(string text)
    {
        string last = text.Split('+')[^1].Trim();
        // Enum.TryParse also takes numbers ("Ctrl+6" would be Ctrl+Enter): a key is written by its name.
        if (last.Length == 0 || !char.IsLetter(last[0]))
        {
            return null;
        }
        if (!text.Contains('+') && Enum.TryParse(last, ignoreCase: true, out Key key))
        {
            return (ModifierKeys.None, key);
        }
        return Hotkey.Parse(text);
    }

    /// <summary>
    /// The defaults with <paramref name="saved"/> laid over them; entries that name no command are ignored.
    /// Returns null and the problems if any binding cannot be had, so none are applied half.
    /// </summary>
    /// <param name="hotkey">The system-wide shortcut that opens the panel, which the panel never sees.</param>
    /// <param name="ocrHotkey">The system-wide shortcut for the text under the pointer.</param>
    public static (PanelKeys? Keys, IReadOnlyList<string> Problems) From(IReadOnlyDictionary<string, string>? saved, string hotkey, string ocrHotkey)
    {
        var bindings = new Dictionary<PanelCommand, string>(Defaults.Bindings);
        foreach ((string name, string keys) in saved ?? new Dictionary<string, string>())
        {
            if (Enum.TryParse(name, ignoreCase: false, out PanelCommand command) && Enum.IsDefined(command))
            {
                bindings[command] = keys;
            }
        }

        var problems = new List<string>();
        var taken = new Dictionary<(ModifierKeys, Key), PanelCommand>();
        foreach ((PanelCommand command, string keys) in bindings.OrderBy(binding => binding.Key))
        {
            if (Problem(keys, hotkey, ocrHotkey) is string problem)
            {
                problems.Add($"{Name(command)}: {problem}");
                continue;
            }
            (ModifierKeys, Key) parsed = Parse(keys)!.Value;
            if (taken.TryGetValue(parsed, out PanelCommand other))
            {
                problems.Add($"{keys} is set for both {Name(other)} and {Name(command)}");
                continue;
            }
            taken[parsed] = command;
        }
        return problems.Count > 0 ? (null, problems) : (new PanelKeys(bindings), problems);
    }

    private static string? Problem(string keys, string hotkey, string ocrHotkey)
    {
        if (Parse(keys) is not (ModifierKeys modifiers, Key key))
        {
            return $"\"{keys}\" is not a shortcut";
        }
        if (Hotkey.Refused(modifiers, key) is string refused)
        {
            return refused;
        }
        if (IsNeeded(modifiers, key))
        {
            return $"{keys} is needed to move and type in the panel";
        }
        // Shift alone would take a capital letter or a symbol from the search box; only Shift+Enter is free.
        bool typing = modifiers is ModifierKeys.None or ModifierKeys.Shift && !IsFunctionKey(key) && key != Key.Enter;
        if (typing)
        {
            return $"{keys} needs Ctrl or Alt, or use a function key";
        }
        if (Hotkey.SameKeys(keys, hotkey))
        {
            return $"{keys} is the Open search shortcut";
        }
        if (Hotkey.SameKeys(keys, ocrHotkey))
        {
            return $"{keys} is the Search text under pointer shortcut";
        }
        return null;
    }

    /// <summary>
    /// The keys the panel keeps for itself: moving, opening, clearing, and editing the search. They match
    /// SearchWindow.OnFixedKey, which takes them before any shortcut, so a shortcut on them could never run.
    /// </summary>
    private static bool IsNeeded(ModifierKeys modifiers, Key key) => (modifiers, key) switch
    {
        (ModifierKeys.None, Key.Enter) => true,
        (_, Key.Escape) => true,
        (_, Key.Tab) when !modifiers.HasFlag(ModifierKeys.Control) => true,
        (_, Key.Up or Key.PageUp or Key.PageDown) when !modifiers.HasFlag(ModifierKeys.Alt) => true,
        (_, Key.Down) when modifiers == ModifierKeys.Alt || !modifiers.HasFlag(ModifierKeys.Alt) => true,
        (ModifierKeys.None or ModifierKeys.Shift, Key.Left or Key.Right or Key.Home or Key.End or Key.Back or Key.Delete) => true,
        (ModifierKeys.Control, Key.A or Key.V or Key.X or Key.Z or Key.Y or Key.O or Key.Back or Key.Delete
            or Key.Left or Key.Right or Key.Home or Key.End) => true,
        (ModifierKeys.Alt, Key.Down) => true,
        _ => false,
    };

    private static bool IsFunctionKey(Key key) => key is >= Key.F1 and <= Key.F24;

    private static string KeyCap(Key key) => key switch
    {
        Key.Enter => "↵",
        Key.OemComma => ",",
        Key.OemPeriod => ".",
        >= Key.D0 and <= Key.D9 => ((int)(key - Key.D0)).ToString(),
        _ => key.ToString(),
    };
}
