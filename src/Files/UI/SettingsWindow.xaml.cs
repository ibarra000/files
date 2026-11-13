using System.Collections.ObjectModel;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Threading;
using Files.Core;
using Files.Data;
using Files.Services;
using Microsoft.Win32;

namespace Files.UI;

/// <summary>Every setting, saved as soon as it changes.</summary>
public partial class SettingsWindow : Window
{
    public enum Page
    {
        General,
        Volumes,
        Search,
        Opening,
        About,
    }

    private readonly App _app;
    private readonly ObservableCollection<ViewerRow> _viewers = [];
    private readonly ObservableCollection<VolumeRow> _volumeRows = [];
    private bool _loading = true;

    public SettingsWindow(App app)
    {
        InitializeComponent();
        _app = app;
        ViewerGrid.ItemsSource = _viewers;
        VolumeGrid.ItemsSource = _volumeRows;
        Load(app.Settings);
        RefreshVolumes();
        Action onIndexerChanged = () => Dispatcher.BeginInvoke(DispatcherPriority.Background, RefreshVolumes);
        app.Indexer.Changed += onIndexerChanged;
        // The title bar and body take the panel's colors, and follow a change of Colors at once;
        // the Install button follows an update found while the window is open.
        Action onSettingsChanged = () =>
        {
            Backdrop.Apply(this, app.IsDark);
            InstallButton.IsEnabled = app.UpdateOffer is not null;
        };
        SourceInitialized += (_, _) => onSettingsChanged();
        app.SettingsChanged += onSettingsChanged;
        // A deleted program row raises no edit event, so the list itself says when one goes.
        _viewers.CollectionChanged += (_, e) =>
        {
            if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Remove)
            {
                Dispatcher.BeginInvoke(DispatcherPriority.Background, Save);
            }
        };
        foreach (TextBox box in new[] { HotkeyBox, OcrHotkeyBox })
        {
            // While a shortcut is being recorded, the current ones are let go so pressing them reaches the box.
            box.GotKeyboardFocus += (_, _) => app.SuspendHotkeys();
            box.LostKeyboardFocus += (_, _) => ResumeHotkeys();
        }
        // Typing in a text field is saved even if the window is left or closed without moving focus.
        Deactivated += (_, _) => SaveTypedText();
        Closing += (_, _) =>
        {
            ViewerGrid.CommitEdit(DataGridEditingUnit.Row, exitEditingMode: true);
            SaveTypedText();
        };
        Closed += (_, _) =>
        {
            app.Indexer.Changed -= onIndexerChanged;
            app.SettingsChanged -= onSettingsChanged;
            app.ResumeHotkeys();
        };
    }

    private void ResumeHotkeys()
    {
        string? problem = _app.ResumeHotkeys();
        ShowShortcuts();
        if (problem is not null)
        {
            Message.Text = problem;
        }
    }

    /// <summary>The shortcut boxes show the combinations that are kept, which may be the old ones after a problem.</summary>
    private void ShowShortcuts()
    {
        HotkeyBox.Text = _app.Settings.Hotkey;
        OcrHotkeyBox.Text = _app.Settings.OcrHotkey;
    }

    /// <summary>Saves the two typed fields when they differ from what is saved; they otherwise save only when focus leaves them.</summary>
    private void SaveTypedText()
    {
        Settings saved = _app.Settings;
        bool hiddenChanged = !SplitList(HiddenBox.Text).SequenceEqual(saved.HiddenExtensions);
        bool folderChanged = UpdateFolderBox.Text.Trim() != saved.UpdateFolder;
        if (hiddenChanged || folderChanged)
        {
            Save();
        }
    }

    /// <summary>
    /// A grid inside the page takes the mouse wheel even when it has nothing to scroll; then the page scrolls instead.
    /// </summary>
    private void OnGridWheel(object sender, MouseWheelEventArgs e)
    {
        ScrollViewer? own = FindScrollViewer((DependencyObject)sender);
        bool down = e.Delta < 0;
        bool canScroll = own is not null && (down ? own.VerticalOffset < own.ScrollableHeight : own.VerticalOffset > 0);
        if (canScroll)
        {
            return;
        }
        e.Handled = true;
        PageScroll.ScrollToVerticalOffset(PageScroll.VerticalOffset - e.Delta);
    }

    private static ScrollViewer? FindScrollViewer(DependencyObject node)
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(node); i++)
        {
            DependencyObject child = VisualTreeHelper.GetChild(node, i);
            if (child is ScrollViewer viewer)
            {
                return viewer;
            }
            if (FindScrollViewer(child) is ScrollViewer nested)
            {
                return nested;
            }
        }
        return null;
    }

    /// <summary>Shows the page picked in the sidebar; the pages sit in PageHost in the order of <see cref="Page"/>.</summary>
    private void OnPageChanged(object sender, SelectionChangedEventArgs e)
    {
        int shown = Pages.SelectedIndex;
        if (shown < 0 || PageHost is null)
        {
            return;
        }
        for (int i = 0; i < PageHost.Children.Count; i++)
        {
            PageHost.Children[i].Visibility = i == shown ? Visibility.Visible : Visibility.Collapsed;
        }
        PageTitle.Text = ((Page)shown).ToString();
        PageScroll.ScrollToTop();
    }

    public void ShowPage(Page page)
    {
        Pages.SelectedIndex = (int)page;
        // Files may have been opened since the window was last shown.
        UpdateClearButtons();
        if (WindowState == WindowState.Minimized)
        {
            WindowState = WindowState.Normal;
        }
        Show();
        // Activate alone does not raise a window already open behind others; a moment on top does.
        Topmost = true;
        Topmost = false;
        Activate();
    }

    private void Load(Settings settings)
    {
        _loading = true;
        HotkeyBox.Text = settings.Hotkey;
        OcrHotkeyBox.Text = settings.OcrHotkey;
        OcrEngineBox.SelectedIndex = (int)settings.OcrEngine;
        StartWithWindowsBox.IsChecked = StartWithWindows.IsEnabled;
        HideOnBlurBox.IsChecked = settings.HideOnBlur;
        HideAfterOpeningBox.IsChecked = settings.HideAfterOpening;
        RememberBox.IsChecked = settings.RememberSearches;
        ShowOpenedBox.IsChecked = settings.ShowRecentlyOpened;
        UpdateClearButtons();
        ThemeBox.SelectedIndex = (int)settings.Theme;
        RescanBox.SelectedItem = RescanBox.Items.Cast<ComboBoxItem>()
            .FirstOrDefault(item => (string)item.Tag == settings.RescanHours.ToString()) ?? RescanBox.Items[0];
        HideSystemBox.IsChecked = settings.HideSystemFiles;
        HiddenBox.Text = string.Join(", ", settings.HiddenExtensions);
        ShowFoldersBox.IsChecked = settings.ShowFolders;
        foreach (Viewer viewer in settings.Viewers)
        {
            _viewers.Add(new ViewerRow { Name = viewer.Name, Program = viewer.Program });
        }
        FillEnterChoices(settings);
        CheckUpdatesBox.IsChecked = settings.CheckForUpdates;
        UpdateFolderBox.Text = settings.UpdateFolder;
        VersionText.Text = $"files {Updater.Current.ToString(3)}";
        SidebarVersion.Text = $"Version {Updater.Current.ToString(3)}";
        PathsText.Text = $"Settings: {AppPaths.Settings}\nVolumes and file lists: {AppPaths.Local}";
        InstallButton.IsEnabled = _app.UpdateOffer is not null;
        _loading = false;
    }

    private void UpdateClearButtons()
    {
        ClearSearchesButton.IsEnabled = _app.History.Count > 0;
        ClearOpenedButton.IsEnabled = _app.Opened.Count > 0;
    }

    private void OnClearSearches(object sender, RoutedEventArgs e)
    {
        _app.ClearSearches();
        UpdateClearButtons();
        Message.Text = "Recent searches cleared.";
    }

    private void OnClearOpened(object sender, RoutedEventArgs e)
    {
        _app.ClearOpened();
        UpdateClearButtons();
        Message.Text = "Recently opened files cleared.";
    }

    private void FillEnterChoices(Settings settings)
    {
        EnterBox.Items.Clear();
        EnterBox.Items.Add(new ComboBoxItem { Content = "The Windows default for the file type", Tag = Settings.DefaultOpen });
        foreach (Viewer viewer in settings.Viewers)
        {
            EnterBox.Items.Add(new ComboBoxItem { Content = viewer.Name, Tag = viewer.Name });
        }
        bool missing = settings.EnterOpens != Settings.DefaultOpen && settings.Viewers.All(viewer => viewer.Name != settings.EnterOpens);
        if (missing)
        {
            // Its program row is being edited or renamed: the choice is kept rather than quietly becoming the default.
            EnterBox.Items.Add(new ComboBoxItem { Content = $"{settings.EnterOpens} (not set up)", Tag = settings.EnterOpens });
        }
        EnterBox.SelectedItem = EnterBox.Items.Cast<ComboBoxItem>()
            .FirstOrDefault(item => (string)item.Tag == settings.EnterOpens) ?? EnterBox.Items[0];
    }

    private void Save()
    {
        if (_loading)
        {
            return;
        }
        var problems = new List<string>();
        Settings settings = _app.Settings with
        {
            Hotkey = HotkeyBox.Text,
            OcrHotkey = OcrHotkeyBox.Text,
            OcrEngine = (OcrChoice)Math.Max(0, OcrEngineBox.SelectedIndex),
            HideOnBlur = HideOnBlurBox.IsChecked == true,
            HideAfterOpening = HideAfterOpeningBox.IsChecked == true,
            RememberSearches = RememberBox.IsChecked == true,
            ShowRecentlyOpened = ShowOpenedBox.IsChecked == true,
            Theme = (Theme)Math.Max(0, ThemeBox.SelectedIndex),
            RescanHours = int.Parse((string)((ComboBoxItem)RescanBox.SelectedItem).Tag),
            HideSystemFiles = HideSystemBox.IsChecked == true,
            HiddenExtensions = SplitList(HiddenBox.Text),
            ShowFolders = ShowFoldersBox.IsChecked == true,
            Viewers = ValidViewers(problems),
            EnterOpens = (string?)(EnterBox.SelectedItem as ComboBoxItem)?.Tag ?? Settings.DefaultOpen,
            CheckForUpdates = CheckUpdatesBox.IsChecked == true,
            UpdateFolder = UpdateFolderBox.Text.Trim(),
        };
        string? problem = _app.ApplySettings(settings);
        if (problem is not null)
        {
            problems.Insert(0, problem);
        }
        Message.Text = string.Join("\n", problems);

        // The viewer list feeds the Enter choices; a shortcut that could not be had shows the one kept.
        _loading = true;
        FillEnterChoices(_app.Settings);
        ShowShortcuts();
        _loading = false;
    }

    private List<Viewer> ValidViewers(List<string> problems)
    {
        var valid = new List<Viewer>();
        foreach (ViewerRow row in _viewers)
        {
            string name = row.Name.Trim();
            bool reserved = name == Settings.DefaultOpen;
            if (name.Length == 0 || row.Program.Trim().Length == 0 || reserved || valid.Any(v => v.Name == name))
            {
                problems.Add($"Program \"{row.Name}\" is not saved: it needs a unique name and a program");
                continue;
            }
            valid.Add(new Viewer(name, row.Program.Trim()));
        }
        return valid;
    }

    private static List<string> SplitList(string text) =>
        text.Split(',', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries)
            .Select(item => item.TrimStart('.').ToLowerInvariant())
            .Distinct()
            .ToList();

    private void OnChanged(object sender, RoutedEventArgs e) => Save();

    private void OnGridEdited(object? sender, EventArgs e)
    {
        // Edit events fire before the row is committed, so save once it is.
        Dispatcher.BeginInvoke(DispatcherPriority.Background, Save);
    }

    private void OnHotkeyKey(object sender, KeyEventArgs e)
    {
        var box = (TextBox)sender;
        Key key = e.Key == Key.System ? e.SystemKey : e.Key;
        if (key == Key.Tab && Keyboard.Modifiers is ModifierKeys.None or ModifierKeys.Shift)
        {
            // Tab and Shift+Tab move on, so the boxes can be left with the keyboard.
            return;
        }
        e.Handled = true;
        if (key is Key.Delete or Key.Back)
        {
            box.Text = Hotkey.Off;
            Save();
            return;
        }
        bool modifierOnly = key is Key.LeftCtrl or Key.RightCtrl or Key.LeftAlt or Key.RightAlt
            or Key.LeftShift or Key.RightShift or Key.LWin or Key.RWin;
        if (modifierOnly || Keyboard.Modifiers == ModifierKeys.None)
        {
            return;
        }
        box.Text = Hotkey.Format(Keyboard.Modifiers, key);
        Save();
    }

    private void OnStartWithWindows(object sender, RoutedEventArgs e)
    {
        try
        {
            StartWithWindows.Set(StartWithWindowsBox.IsChecked == true);
            Message.Text = "";
        }
        catch (Exception error) when (error is UnauthorizedAccessException or System.Security.SecurityException or IOException)
        {
            Message.Text = $"Could not change Start with Windows: {error.Message}";
        }
    }

    /// <summary>
    /// Updates the rows in place, so the grid keeps its sort and scroll while volumes are read
    /// (progress arrives several times a second); only a different set of volumes rebuilds it.
    /// </summary>
    private void RefreshVolumes()
    {
        long? selected = (VolumeGrid.SelectedItem as VolumeRow)?.Id;
        List<VolumeRow> rows = _app.Indexer.Volumes.Select(VolumeRow.From).ToList();
        bool sameVolumes = rows.Select(row => row.Id).Order().SequenceEqual(_volumeRows.Select(row => row.Id).Order());
        if (!sameVolumes)
        {
            _volumeRows.Clear();
            rows.ForEach(_volumeRows.Add);
        }
        else
        {
            foreach (VolumeRow row in rows)
            {
                int at = _volumeRows.ToList().FindIndex(shown => shown.Id == row.Id);
                if (_volumeRows[at] != row)
                {
                    _volumeRows[at] = row;
                }
            }
        }
        VolumeGrid.SelectedItem = _volumeRows.FirstOrDefault(row => row.Id == selected);
    }

    private void OnBrowse(object sender, RoutedEventArgs e)
    {
        var dialog = new OpenFolderDialog { Title = "Choose a folder to search" };
        if (dialog.ShowDialog(this) != true)
        {
            return;
        }
        NewVolumePath.Text = dialog.FolderName;
        if (NewVolumeName.Text.Trim().Length == 0)
        {
            NewVolumeName.Text = Path.GetFileName(dialog.FolderName.TrimEnd('\\')) is { Length: > 0 } name ? name : dialog.FolderName;
        }
    }

    private async void OnAddVolume(object sender, RoutedEventArgs e)
    {
        string name = NewVolumeName.Text;
        string folder = NewVolumePath.Text;
        Message.Text = "Adding…";
        // Off the UI thread: asking a disconnected mapped drive what it is can take seconds.
        string? problem = await Task.Run(() => _app.Indexer.Add(name, folder));
        Message.Text = problem ?? "";
        if (problem is null)
        {
            NewVolumeName.Clear();
            NewVolumePath.Clear();
        }
    }

    private async void OnRemoveVolume(object sender, RoutedEventArgs e)
    {
        if (VolumeGrid.SelectedItem is not VolumeRow row)
        {
            Message.Text = "Select a volume first";
            return;
        }
        MessageBoxResult answer = MessageBox.Show(this, $"Stop searching {row.Name}?\n\n{row.Root}", "Remove volume",
            MessageBoxButton.OKCancel, MessageBoxImage.Question);
        if (answer == MessageBoxResult.OK)
        {
            // Off the UI thread: it waits for a read that is writing the volume's file list to finish.
            Message.Text = "Removing…";
            Message.Text = await Task.Run(() => _app.Indexer.Remove(row.Id)) ?? "";
        }
    }

    private void OnRescanVolume(object sender, RoutedEventArgs e)
    {
        if (VolumeGrid.SelectedItem is VolumeRow row)
        {
            _app.Indexer.Rescan(row.Id);
        }
        else
        {
            _app.Indexer.RescanAll();
        }
    }

    private async void OnCheckUpdate(object sender, RoutedEventArgs e)
    {
        Message.Text = "Checking…";
        Message.Text = await _app.CheckForUpdateAsync();
        InstallButton.IsEnabled = _app.UpdateOffer is not null;
    }

    private async void OnInstallUpdate(object sender, RoutedEventArgs e)
    {
        Message.Text = "Downloading the update…";
        Message.Text = await _app.InstallUpdateAsync() ?? "";
    }

    public sealed class ViewerRow
    {
        public string Name { get; set; } = "";
        public string Program { get; set; } = "";
    }

    public sealed record VolumeRow(long Id, string Name, string Root, string Files, string LastRead, string Status)
    {
        public static VolumeRow From(VolumeState state)
        {
            string files = state.Index?.FileCount.ToString("N0") ?? "–";
            return new VolumeRow(state.Volume.Id, state.Volume.Name, state.Volume.Root, files,
                StatusText.Age(state.Volume.LastScannedAt), Describe(state));
        }

        private static string Describe(VolumeState state)
        {
            if (state.ScannedSoFar is int read)
            {
                return $"Reading… {read:N0} files";
            }
            string? error = state.Volume.LastError switch
            {
                VolumeError.PathNotFound => "Folder not found",
                VolumeError.DriveNotFound => "Drive or server not found",
                VolumeError.PermissionDenied => "No permission to read it",
                _ => null,
            };
            if (error is not null)
            {
                return error;
            }
            if (state.Problem is not null)
            {
                return state.Problem;
            }
            return state.Index is null ? "Waiting to be read" : "OK";
        }
    }
}
