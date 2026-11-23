using System.Text.Json;
using System.Text.Json.Serialization;
using Files.Core;

namespace Files.Services;

public enum Theme
{
    System,
    Light,
    Dark,
}

/// <summary>The engine that reads the text under the mouse when the app does not offer it.</summary>
public enum OcrChoice
{
    /// <summary>Built into Windows; good with digits, weaker with letters.</summary>
    Windows,

    /// <summary>PaddleOCR on this PC (Services/PaddleTextReader.cs); better with letters.</summary>
    Paddle,
}

/// <summary>What happens to the text read under the mouse.</summary>
public enum ScreenTextAction
{
    /// <summary>The panel searches for it.</summary>
    Search,

    /// <summary>It goes on the clipboard; the panel does not open.</summary>
    Copy,
}

/// <summary>A program to open files with.</summary>
public sealed record Viewer(string Name, string Program);

/// <summary>Everything the Settings window edits, saved as <c>settings.json</c>. Volumes live in the catalog.</summary>
public sealed record Settings
{
    public const string DefaultOpen = "default";

    public static readonly string[] DefaultHiddenExtensions =
        ["db", "js", "lnk", "ini", "tmp", "bak", "url", "exe", "dll", "bat", "cmd", "ps1", "vbs"];

    private static readonly JsonSerializerOptions Json = new()
    {
        WriteIndented = true,
        // Keeps "Ctrl+Shift+Space" readable instead of "Ctrl+Shift...". The file is never served as HTML.
        Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
        Converters = { new JsonStringEnumConverter() },
    };

    public string Hotkey { get; init; } = "Ctrl+Shift+Space";

    /// <summary>Searches for the word or code under the mouse.</summary>
    public string OcrHotkey { get; init; } = "Ctrl+Alt+Space";

    /// <summary>
    /// The panel's shortcuts the user changed, by command name (Services/PanelKeys.cs), such as
    /// "Preview": "F6"; a command not listed keeps its default.
    /// </summary>
    public IReadOnlyDictionary<string, string> PanelKeys { get; init; } = new Dictionary<string, string>();

    /// <summary>
    /// The panel's shortcuts in effect: <see cref="PanelKeys"/> over the defaults, or the defaults if they cannot
    /// all be had. A system-wide shortcut on the same keys is not checked here: it only keeps that key from the
    /// panel, and changing it must not drop the others. Settings warns of it when a panel key is recorded.
    /// </summary>
    [JsonIgnore]
    public PanelKeys Shortcuts
    {
        get
        {
            // Read on every key press in the panel: built once for each saved list (records copy the reference on "with").
            ShortcutsCache cache = _shortcuts;
            if (!ReferenceEquals(cache.Source, PanelKeys))
            {
                PanelKeys keys = Services.PanelKeys.From(PanelKeys, Services.Hotkey.Off, Services.Hotkey.Off).Keys ?? Services.PanelKeys.Defaults;
                cache = new ShortcutsCache(PanelKeys, keys);
                _shortcuts = cache;
            }
            return cache.Keys;
        }
    }

    private sealed record ShortcutsCache(IReadOnlyDictionary<string, string>? Source, PanelKeys Keys);

    private static ShortcutsCache _shortcuts = new(null, Services.PanelKeys.Defaults);

    /// <summary>The engine for the text under the mouse; Windows OCR is the fallback if it cannot run.</summary>
    public OcrChoice OcrEngine { get; init; } = OcrChoice.Windows;

    /// <summary>Whether the text under the mouse is searched for or only copied.</summary>
    public ScreenTextAction AfterReading { get; init; } = ScreenTextAction.Search;

    /// <summary>
    /// The text corrections (Core/TextCleanup.cs), applied to the text under the mouse before it is searched:
    /// a regular expression whose matches are removed, or "pattern => replacement". The name is older than replacements.
    /// </summary>
    public IReadOnlyList<string> ScreenTextRemovals { get; init; } = [];
    public Theme Theme { get; init; } = Theme.System;
    public bool HideOnBlur { get; init; } = true;
    public bool HideAfterOpening { get; init; } = true;
    /// <summary>Records new searches and opened files; off pauses history without deleting it.</summary>
    public bool RememberSearches { get; init; } = true;

    /// <summary>Lists recently opened files under the favorites while the box is empty.</summary>
    public bool ShowRecentlyOpened { get; init; } = true;

    /// <summary>Shows each result's folder on a second line; off keeps one line per result, the folder on hover.</summary>
    public bool ShowFolders { get; init; } = true;

    /// <summary>Files opened from the panel rank first among results that match as well (Core/OpenCounts.cs); off also stops counting.</summary>
    public bool RankOpenedHigher { get; init; } = true;

    /// <summary>The preview pane beside the results (Ctrl+P); the window is that much wider while it shows.</summary>
    public bool ShowPreview { get; init; }

    /// <summary>Hours between automatic rescans of each volume; 0 means only on F5.</summary>
    public int RescanHours { get; init; } = 1;
    public bool HideSystemFiles { get; init; } = true;
    public IReadOnlyList<string> HiddenExtensions { get; init; } = DefaultHiddenExtensions;

    /// <summary>What Enter does: <see cref="DefaultOpen"/> or a viewer's name.</summary>
    public string EnterOpens { get; init; } = DefaultOpen;
    public IReadOnlyList<Viewer> Viewers { get; init; } = [];

    public bool CheckForUpdates { get; init; } = true;

    /// <summary>A shared folder holding latest.json and the installer, or empty for GitHub releases.</summary>
    public string UpdateFolder { get; init; } = "";

    public double? WindowLeft { get; init; }
    public double? WindowTop { get; init; }
    public double? WindowWidth { get; init; }
    public double? WindowHeight { get; init; }

    [JsonIgnore]
    public IReadOnlySet<string> HiddenExtensionSet =>
        HiddenExtensions.Select(e => e.Trim().TrimStart('.').ToLowerInvariant()).Where(e => e.Length > 0).ToHashSet();

    /// <summary>Loads settings, falling back to defaults when the file is missing or unreadable.</summary>
    public static (Settings Settings, string? Problem) Load(string path)
    {
        if (!File.Exists(path))
        {
            return (new Settings(), null);
        }
        try
        {
            Settings? loaded = JsonSerializer.Deserialize<Settings>(File.ReadAllText(path), Json);
            return (WithDefaultsForNulls(loaded ?? new Settings()), null);
        }
        catch (Exception error) when (error is JsonException or IOException or UnauthorizedAccessException)
        {
            return (new Settings(), $"Settings could not be read, so defaults are in use: {error.Message}");
        }
    }

    /// <summary>A hand-edited file can hold null where a value is required; those get the default.</summary>
    private static Settings WithDefaultsForNulls(Settings loaded)
    {
        var defaults = new Settings();
        return loaded with
        {
            Hotkey = loaded.Hotkey ?? defaults.Hotkey,
            OcrHotkey = loaded.OcrHotkey ?? defaults.OcrHotkey,
            PanelKeys = (loaded.PanelKeys ?? new Dictionary<string, string>())
                .Where(binding => binding.Value is not null)
                .ToDictionary(binding => binding.Key, binding => binding.Value),
            HiddenExtensions = loaded.HiddenExtensions ?? defaults.HiddenExtensions,
            ScreenTextRemovals = (loaded.ScreenTextRemovals ?? []).Where(pattern => pattern is not null).ToList(),
            EnterOpens = loaded.EnterOpens ?? defaults.EnterOpens,
            Viewers = (loaded.Viewers ?? [])
                .Where(viewer => viewer?.Name is not null && viewer.Program is not null)
                .ToList(),
            UpdateFolder = loaded.UpdateFolder ?? "",
        };
    }

    public void Save(string path) => AtomicFile.WriteAllText(path, JsonSerializer.Serialize(this, Json));
}
