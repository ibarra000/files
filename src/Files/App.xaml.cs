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

    private Mutex? _singleInstance;
    private EventWaitHandle? _summon;
    private Hotkey? _hotkey;
    private Tray? _tray;
    private SearchWindow? _panel;
    private SettingsWindow? _settingsWindow;
    private DispatcherTimer? _updateTimer;

    public Settings Settings { get; private set; } = new();
    public Indexer Indexer { get; private set; } = null!;
    public Opener Opener { get; private set; } = null!;
    public Updater Updater { get; private set; } = null!;
    public IReadOnlyList<string> History { get; private set; } = [];

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
        History = Settings.RememberSearches ? Core.History.Load(AppPaths.History) : [];
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
        Opener = new Opener(() => Settings, AppPaths.PdfCache);
        Updater = new Updater(() => Settings, AppPaths.Updates);
        ApplyTheme();

        _panel = new SearchWindow(this);
        _tray = new Tray(this);
        _hotkey = new Hotkey();
        _hotkey.Pressed += TogglePanel;
        HotkeyProblem = _hotkey.Register(Settings.Hotkey);
        Indexer.Start();
        Task.Run(() => PdfMerger.CleanUp(AppPaths.PdfCache));
        StartUpdateChecks();

        if (settingsProblem is not null || HotkeyProblem is not null)
        {
            _tray.Notify(settingsProblem ?? $"Hotkey: {HotkeyProblem}");
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
        if (settings.Theme != previous.Theme)
        {
            ApplyTheme();
        }
        if (!settings.RememberSearches)
        {
            ForgetHistory();
        }
        SettingsChanged?.Invoke();
        return problem;
    }

    public void RecordSearch(string line)
    {
        if (!Settings.RememberSearches)
        {
            return;
        }
        IReadOnlyList<string> updated = Core.History.Record(History, line);
        if (ReferenceEquals(updated, History))
        {
            return;
        }
        History = updated;
        Task.Run(() => SaveHistory(updated));
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

    private void ForgetHistory()
    {
        History = [];
        try
        {
            File.Delete(AppPaths.History);
        }
        catch (IOException)
        {
            // In use; it is overwritten next time searches are remembered.
        }
    }

    private static void SaveHistory(IReadOnlyList<string> entries)
    {
        try
        {
            Core.History.Save(AppPaths.History, entries);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // Losing a history entry is not worth interrupting the user for.
            System.Diagnostics.Trace.WriteLine($"history not saved: {error.Message}");
        }
    }
}
