using System.Windows;
using System.Windows.Threading;
using Files.Core;
using Files.Data;
using Files.Services;
using Files.UI;

namespace Files;

/// <summary>Starts the app, owns the services, and applies settings changes.</summary>
public partial class App : Application
{
    private const string SingleInstanceName = @"Local\files-single-instance";
    private const string SummonName = @"Local\files-summon";
    private const string PersonalizeKey = @"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
    private static readonly TimeSpan FirstUpdateCheck = TimeSpan.FromMinutes(1);
    private static readonly TimeSpan UpdateCheckInterval = TimeSpan.FromHours(4);
    private static readonly TimeSpan PanelHideDelay = TimeSpan.FromMilliseconds(200);

    private Mutex? _singleInstance;
    private EventWaitHandle? _summon;
    private Hotkey? _hotkey;
    private Hotkey? _ocrHotkey;
    private bool _readingScreen;
    private Tray? _tray;
    private SearchWindow? _panel;
    private SettingsWindow? _settingsWindow;
    private DispatcherTimer? _updateTimer;

    // Saves run one at a time, and each writes the newest list, so a quick second change is never lost.
    private readonly object _saveGate = new();

    public Settings Settings { get; private set; } = new();
    public Indexer Indexer { get; private set; } = null!;
    public Updater Updater { get; private set; } = null!;

    /// <summary>Searches that led to an opened file, newest first.</summary>
    public IReadOnlyList<string> History { get; private set; } = [];

    /// <summary>Files opened or shown in their folder, newest first.</summary>
    public IReadOnlyList<string> Opened { get; private set; } = [];

    public IReadOnlyList<Favorite> Favorites { get; private set; } = [];

    /// <summary>The newer release found by the last check, if any.</summary>
    public Manifest? UpdateOffer { get; private set; }

    /// <summary>Why the hotkey could not be registered, if it could not.</summary>
    public string? HotkeyProblem { get; private set; }

    /// <summary>Raised on the UI thread after settings or the update offer change.</summary>
    public event Action? SettingsChanged;

    public static new App Current => (App)Application.Current;

    /// <summary>Whether windows are drawn dark: chosen in Settings, or following Windows' app setting.</summary>
    public bool IsDark => Settings.Theme switch
    {
        Theme.Dark => true,
        Theme.Light => false,
        _ => Microsoft.Win32.Registry.GetValue(PersonalizeKey, "AppsUseLightTheme", 1) is 0,
    };

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);
        if (e.Args is [Updater.ApplyFlag, string msi, string pid, string relaunch] && int.TryParse(pid, out int waitFor))
        {
            Shutdown(Updater.Apply(msi, waitFor, relaunch));
            return;
        }
        DispatcherUnhandledException += OnUnexpectedError;
        if (!ClaimSingleInstance())
        {
            Shutdown();
            return;
        }

        (Settings, string? settingsProblem) = Settings.Load(AppPaths.Settings);
        History = LoadOrEmpty(() => Core.History.Load(AppPaths.History));
        Opened = LoadOrEmpty(() => Core.History.Load(AppPaths.Opened));
        Favorites = LoadOrEmpty(() => Core.Favorites.Load(AppPaths.Favorites));
        Catalog catalog;
        try
        {
            catalog = Catalog.Open(AppPaths.Catalog);
        }
        catch (Exception error) when (error is Microsoft.Data.Sqlite.SqliteException or IOException or UnauthorizedAccessException)
        {
            MessageBox.Show($"The volume list ({AppPaths.Catalog}) could not be opened:\n\n{error.Message}", "files", MessageBoxButton.OK, MessageBoxImage.Error);
            Shutdown(1);
            return;
        }

        Indexer = new Indexer(catalog, AppPaths.Shards, () => Settings);
        Updater = new Updater(() => Settings, AppPaths.Updates);
        ApplyTheme();

        _panel = new SearchWindow(this);
        _tray = new Tray(this);
        _hotkey = new Hotkey();
        _hotkey.Pressed += TogglePanel;
        HotkeyProblem = _hotkey.Register(Settings.Hotkey);
        _ocrHotkey = new Hotkey();
        _ocrHotkey.Pressed += () => _ = SearchUnderCursorAsync();
        string? ocrHotkeyProblem = _ocrHotkey.Register(Settings.OcrHotkey);
        Indexer.Start();
        // Combined PDFs were removed in 1.2; their cache is no longer needed.
        SaveInBackground(() => DeleteFolder(Path.Combine(AppPaths.Local, "pdf")));
        StartUpdateChecks();

        var startProblems = new List<string>();
        if (settingsProblem is not null)
        {
            startProblems.Add(settingsProblem);
        }
        if (HotkeyProblem is not null)
        {
            startProblems.Add($"Hotkey: {HotkeyProblem}");
        }
        if (ocrHotkeyProblem is not null)
        {
            startProblems.Add($"Text-under-the-mouse shortcut: {ocrHotkeyProblem}");
        }
        if (startProblems.Count > 0)
        {
            _tray.Notify(string.Join("\n", startProblems));
        }
        if (catalog.All().Count == 0)
        {
            ShowSettings(SettingsWindow.Page.Volumes);
        }
    }

    public void ShowPanel() => _panel?.ShowPanel();

    public void TogglePanel()
    {
        if (_panel is { IsVisible: true, IsActive: true })
        {
            _panel.HidePanel();
        }
        else
        {
            ShowPanel();
        }
    }

    /// <summary>Reads the word or code under the mouse and searches for it.</summary>
    private async Task SearchUnderCursorAsync()
    {
        if (_readingScreen)
        {
            // A slow app is still being read; a second press would only queue up behind it.
            return;
        }
        _readingScreen = true;
        ScreenRead read;
        try
        {
            if (_panel is { IsVisible: true })
            {
                // The panel itself must not be what is read.
                _panel.HidePanel();
                await Task.Delay(PanelHideDelay);
            }
            read = await ScreenText.UnderCursorAsync();
        }
        catch (Exception error)
        {
            // Nothing awaits this task, so every failure ends in the panel saying what happened.
            System.Diagnostics.Trace.WriteLine(error.ToString());
            read = new ScreenRead(null, $"The screen could not be read: {error.Message}");
        }
        finally
        {
            _readingScreen = false;
        }
        _panel?.ShowWithSearch(read.Text, read.Problem);
    }

    public void ShowSettings(SettingsWindow.Page page = SettingsWindow.Page.General)
    {
        if (_settingsWindow is null)
        {
            _settingsWindow = new SettingsWindow(this);
            _settingsWindow.Closed += (_, _) => _settingsWindow = null;
        }
        _settingsWindow.ShowPage(page);
    }

    /// <summary>Saves and applies new settings. Returns a problem in words, or null.</summary>
    public string? ApplySettings(Settings settings)
    {
        Settings previous = Settings;
        Settings = settings;
        string? problem = null;
        try
        {
            settings.Save(AppPaths.Settings);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            problem = $"Settings could not be saved: {error.Message}";
        }
        if (settings.Hotkey != previous.Hotkey && _hotkey is not null)
        {
            HotkeyProblem = _hotkey.Register(settings.Hotkey);
            problem ??= HotkeyProblem;
        }
        if (settings.OcrHotkey != previous.OcrHotkey && _ocrHotkey is not null)
        {
            problem ??= _ocrHotkey.Register(settings.OcrHotkey);
        }
        if (settings.Theme != previous.Theme)
        {
            ApplyTheme();
        }
        SettingsChanged?.Invoke();
        return problem;
    }

    /// <summary>Remembers a file opened or shown in its folder, and the search that found it, unless history is paused.</summary>
    public void RecordOpened(string path, string search)
    {
        if (!Settings.RememberSearches)
        {
            return;
        }
        IReadOnlyList<string> history = Core.History.Record(History, search);
        if (!ReferenceEquals(history, History))
        {
            History = history;
            SaveInBackground(() => Core.History.Save(AppPaths.History, History));
        }
        IReadOnlyList<string> opened = Core.History.Record(Opened, path);
        if (!ReferenceEquals(opened, Opened))
        {
            Opened = opened;
            SaveInBackground(() => Core.History.Save(AppPaths.Opened, Opened));
        }
    }

    /// <summary>Adds the favorite, or removes it if it is one already. Returns whether it is a favorite now.</summary>
    public bool ToggleFavorite(Favorite favorite)
    {
        IReadOnlyList<Favorite> favorites = Core.Favorites.Toggle(Favorites, favorite);
        Favorites = favorites;
        SaveInBackground(() => Core.Favorites.Save(AppPaths.Favorites, Favorites));
        return Core.Favorites.Contains(favorites, favorite);
    }

    public async Task<string?> CheckForUpdateAsync()
    {
        try
        {
            UpdateOffer = await Updater.CheckAsync(CancellationToken.None);
            SettingsChanged?.Invoke();
            return UpdateOffer is null ? "files is up to date" : $"Version {UpdateOffer.Version} is available";
        }
        catch (Exception error) when (error is IOException or HttpRequestException or TaskCanceledException or UnauthorizedAccessException or ArgumentException or NotSupportedException)
        {
            return $"Could not check for updates: {error.Message}";
        }
    }

    /// <summary>Installs the offered update and quits. Returns a problem in words if it could not start.</summary>
    public async Task<string?> InstallUpdateAsync()
    {
        if (UpdateOffer is null)
        {
            return "No update is available";
        }
        try
        {
            await Updater.InstallAsync(UpdateOffer, CancellationToken.None);
        }
        catch (Exception error) when (error is IOException or HttpRequestException or TaskCanceledException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            return $"The update could not be installed: {error.Message}";
        }
        Quit();
        return null;
    }

    public void Quit()
    {
        _panel?.HidePanel();
        _hotkey?.Dispose();
        _ocrHotkey?.Dispose();
        _tray?.Dispose();
        Indexer.Dispose();
        _summon?.Dispose();
        _singleInstance?.Dispose();
        Shutdown();
    }

    private bool ClaimSingleInstance()
    {
        // The event exists before the mutex is claimed, so a second launch can always signal it.
        _summon = new EventWaitHandle(false, EventResetMode.AutoReset, SummonName);
        _singleInstance = new Mutex(initiallyOwned: true, SingleInstanceName, out bool first);
        if (!first)
        {
            // Already running: ask that instance to show its panel.
            _summon.Set();
            return false;
        }
        ThreadPool.RegisterWaitForSingleObject(_summon, (_, _) => Dispatcher.BeginInvoke(ShowPanel), null, Timeout.Infinite, executeOnlyOnce: false);
        return true;
    }

    /// <summary>
    /// The last resort for a bug that escapes a UI handler: say what happened
    /// and keep running, rather than vanishing from the tray.
    /// </summary>
    private void OnUnexpectedError(object sender, DispatcherUnhandledExceptionEventArgs e)
    {
        System.Diagnostics.Trace.WriteLine(e.Exception.ToString());
        _tray?.Notify($"Something went wrong: {e.Exception.Message}");
        e.Handled = true;
    }

    private void ApplyTheme()
    {
        ThemeMode = Settings.Theme switch
        {
            Theme.Light => ThemeMode.Light,
            Theme.Dark => ThemeMode.Dark,
            _ => ThemeMode.System,
        };
    }

    private void StartUpdateChecks()
    {
        _updateTimer = new DispatcherTimer { Interval = FirstUpdateCheck };
        _updateTimer.Tick += async (_, _) =>
        {
            _updateTimer.Interval = UpdateCheckInterval;
            if (!Settings.CheckForUpdates || UpdateOffer is not null)
            {
                return;
            }
            await CheckForUpdateAsync();
            if (UpdateOffer is not null)
            {
                _tray?.Notify($"Version {UpdateOffer.Version} is available. Install it from the tray menu.");
            }
        };
        _updateTimer.Start();
    }

    /// <summary>Forgets the searches Up and Down recall. Favorites are kept.</summary>
    public void ClearSearches()
    {
        History = [];
        // Saves the current list rather than deleting the file, so whichever save runs last writes the newest state.
        SaveInBackground(() => Core.History.Save(AppPaths.History, History));
    }

    /// <summary>Forgets one recently opened file.</summary>
    public void RemoveOpened(string path)
    {
        IReadOnlyList<string> opened = Core.History.Remove(Opened, path);
        if (!ReferenceEquals(opened, Opened))
        {
            Opened = opened;
            SaveInBackground(() => Core.History.Save(AppPaths.Opened, Opened));
        }
    }

    /// <summary>Forgets the recently opened files. Favorites are kept.</summary>
    public void ClearOpened()
    {
        Opened = [];
        SaveInBackground(() => Core.History.Save(AppPaths.Opened, Opened));
    }

    private static void DeleteFolder(string path)
    {
        if (Directory.Exists(path))
        {
            Directory.Delete(path, recursive: true);
        }
    }

    private static IReadOnlyList<T> LoadOrEmpty<T>(Func<IReadOnlyList<T>> load)
    {
        try
        {
            return load();
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            System.Diagnostics.Trace.WriteLine($"not loaded: {error.Message}");
            return [];
        }
    }

    private void SaveInBackground(Action save)
    {
        Task.Run(() =>
        {
            try
            {
                lock (_saveGate)
                {
                    save();
                }
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                // Losing one entry is not worth interrupting the user for.
                System.Diagnostics.Trace.WriteLine($"not saved: {error.Message}");
            }
        });
    }
}
