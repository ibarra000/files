using System.Windows.Input;
using Files.Data;
using Files.Services;

namespace Files.Tests;

public sealed class ServiceTests
{
    private const string Digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    [Fact]
    public void Settings_survive_a_save_and_load()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "settings.json");
        var settings = new Settings
        {
            Hotkey = "Alt+Space",
            Theme = Theme.Dark,
            Viewers = [new Viewer("sketch", "sketch.exe")],
        };

        settings.Save(path);
        (Settings loaded, string? problem) = Settings.Load(path);

        Assert.Null(problem);
        Assert.Equal("Alt+Space", loaded.Hotkey);
        Assert.Equal(Theme.Dark, loaded.Theme);
        Assert.Equal("sketch.exe", Assert.Single(loaded.Viewers).Program);
    }

    [Fact]
    public void Unreadable_settings_fall_back_to_defaults_and_say_so()
    {
        using var temp = new TempFolder();
        string path = temp.File("settings.json", "{ not json");
        (Settings loaded, string? problem) = Settings.Load(path);
        Assert.Equal(new Settings().Hotkey, loaded.Hotkey);
        Assert.NotNull(problem);
    }

    [Fact]
    public void Nulls_in_a_hand_edited_file_get_the_defaults()
    {
        using var temp = new TempFolder();
        string path = temp.File("settings.json", """{ "Hotkey": null, "Viewers": [ { "Name": "x", "Program": "x.exe" }, { "Name": null, "Program": "y.exe" } ] }""");
        (Settings loaded, _) = Settings.Load(path);
        Assert.Equal(new Settings().Hotkey, loaded.Hotkey);
        Assert.Equal("x", Assert.Single(loaded.Viewers).Name);
    }

    [Fact]
    public void Settings_from_older_versions_still_load()
    {
        using var temp = new TempFolder();
        string path = temp.File("settings.json", """{ "Hotkey": "Alt+Space", "Aliases": [ { "Name": "a", "Code": "b" } ], "PdfViewer": "x.exe", "EnterOpens": "pdf" }""");
        (Settings loaded, string? problem) = Settings.Load(path);
        Assert.Null(problem);
        Assert.Equal("Alt+Space", loaded.Hotkey);
        Assert.IsType<OpenWith.Default>(OpenWith.ForEnter(loaded));
    }

    [Fact]
    public void Older_settings_keep_history_paused_and_show_recently_opened()
    {
        using var temp = new TempFolder();
        string path = temp.File("settings.json", """{ "RememberSearches": false }""");
        (Settings loaded, string? problem) = Settings.Load(path);
        Assert.Null(problem);
        Assert.False(loaded.RememberSearches);
        Assert.True(loaded.ShowRecentlyOpened);
    }

    [Fact]
    public void Folders_show_under_results_unless_turned_off()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "settings.json");
        Assert.True(new Settings().ShowFolders);

        new Settings { ShowFolders = false }.Save(path);
        (Settings loaded, _) = Settings.Load(path);

        Assert.False(loaded.ShowFolders);
    }

    [Fact]
    public void The_preview_pane_starts_closed_and_its_choice_is_kept()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "settings.json");
        Assert.False(new Settings().ShowPreview);

        new Settings { ShowPreview = true }.Save(path);
        (Settings loaded, _) = Settings.Load(path);

        Assert.True(loaded.ShowPreview);
    }

    [Fact]
    public void Settings_without_the_text_shortcut_get_its_default()
    {
        using var temp = new TempFolder();
        string path = temp.File("settings.json", """{ "Hotkey": "Alt+Space", "OcrHotkey": null }""");
        (Settings loaded, _) = Settings.Load(path);
        Assert.Equal(new Settings().OcrHotkey, loaded.OcrHotkey);
    }

    [Fact]
    public void Hidden_extensions_are_normalized()
    {
        var settings = new Settings { HiddenExtensions = [".TMP", " bak ", ""] };
        Assert.Equal(new HashSet<string> { "tmp", "bak" }, settings.HiddenExtensionSet);
    }

    [Fact]
    public void A_valid_manifest_is_accepted_and_compared_by_version()
    {
        Manifest manifest = Manifest.Parse($$"""{"version":"1.2.0","msi":"files-1.2.0-x64.msi","sha256":"{{Digest}}"}""")!;
        Assert.True(manifest.IsNewerThan(new Version(1, 1, 9)));
        Assert.False(manifest.IsNewerThan(new Version(1, 2, 0)));
    }

    [Theory]
    [InlineData("""{"version":"x","msi":"a.msi","sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}""")]
    [InlineData("""{"version":"1.0.0","msi":"..\\a.msi","sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}""")]
    [InlineData("""{"version":"1.0.0","msi":"a.msi","sha256":"short"}""")]
    [InlineData("not json")]
    [InlineData("""{"version":"1.0.0"}""")]
    public void A_bad_manifest_is_refused(string json)
    {
        Assert.Null(Manifest.Parse(json));
    }

    [Fact]
    public void An_installer_is_checked_against_its_digest()
    {
        using var temp = new TempFolder();
        string msi = temp.File("a.msi", "abc");
        Assert.True(Updater.HasDigest(msi, "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"));
        Assert.False(Updater.HasDigest(msi, Digest));
    }

    [Theory]
    [InlineData("Ctrl+Shift+Space", ModifierKeys.Control | ModifierKeys.Shift, Key.Space)]
    [InlineData("alt+f1", ModifierKeys.Alt, Key.F1)]
    [InlineData("Shift+Win+F23", ModifierKeys.Shift | ModifierKeys.Windows, Key.F23)]
    public void Shortcuts_parse_and_format(string text, ModifierKeys modifiers, Key key)
    {
        Assert.Equal((modifiers, key), Hotkey.Parse(text));
        Assert.Equal((modifiers, key), Hotkey.Parse(Hotkey.Format(modifiers, key)));
    }

    [Theory]
    [InlineData("Space")]
    [InlineData("Hyper+K")]
    [InlineData("Ctrl+NotAKey")]
    public void Bad_shortcuts_do_not_parse(string text)
    {
        Assert.Null(Hotkey.Parse(text));
    }

    [Fact]
    public void Scan_failures_map_to_the_catalog_errors()
    {
        Assert.Equal(VolumeError.PermissionDenied, Indexer.Classify(@"C:\x", new UnauthorizedAccessException()));
        Assert.Equal(VolumeError.PathNotFound, Indexer.Classify(@"C:\x", new DirectoryNotFoundException()));
        Assert.Equal(VolumeError.DriveNotFound, Indexer.Classify(@"Q:\x", new DirectoryNotFoundException()));
        Assert.Null(Indexer.Classify(@"C:\x", new IOException("other")));
    }
}
