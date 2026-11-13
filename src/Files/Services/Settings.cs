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
    public Theme Theme { get; init; } = Theme.System;
    public bool HideOnBlur { get; init; } = true;
    public bool HideAfterOpening { get; init; } = true;
    /// <summary>Records new searches and opened files; off pauses history without deleting it.</summary>
    public bool RememberSearches { get; init; } = true;

    /// <summary>Lists recently opened files under the favorites while the box is empty.</summary>
    public bool ShowRecentlyOpened { get; init; } = true;

    /// <summary>Shows each result's folder on a second line; off keeps one line per result, the folder on hover.</summary>
    public bool ShowFolders { get; init; } = true;

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
            HiddenExtensions = loaded.HiddenExtensions ?? defaults.HiddenExtensions,
            EnterOpens = loaded.EnterOpens ?? defaults.EnterOpens,
            Viewers = (loaded.Viewers ?? [])
                .Where(viewer => viewer?.Name is not null && viewer.Program is not null)
                .ToList(),
            UpdateFolder = loaded.UpdateFolder ?? "",
        };
    }

    public void Save(string path) => AtomicFile.WriteAllText(path, JsonSerializer.Serialize(this, Json));
}
