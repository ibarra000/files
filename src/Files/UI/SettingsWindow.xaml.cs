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

    /// <summary>The volume the form is editing, or null while it adds one.</summary>
    private VolumeRow? _editing;
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
        Action onKensakuChanged = () => Dispatcher.BeginInvoke(DispatcherPriority.Background, ShowKensakuStatus);
        app.Kensaku.Changed += onKensakuChanged;
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
        // History may have changed in the panel while this window was behind it, and kensaku may have read more.
        Activated += (_, _) =>
        {
            UpdateClearButtons();
            ShowKensakuStatus();
        };
        Closing += (_, e) =>
        {
            ViewerGrid.CommitEdit(DataGridEditingUnit.Row, exitEditingMode: true);
            // Now, not deferred as while the window is open: a program row that cannot be saved says so while it can be fixed.
            Save();
            if (_viewerProblem && !_closeWarned && !_app.IsQuitting)
            {
                // Once: a second close leaves the row out, as the message says.
                e.Cancel = true;
                _closeWarned = true;
                return;
            }
            SaveTypedText();
            WarnOfRefusedPatterns();
        };
        Closed += (_, _) =>
        {
            app.Indexer.Changed -= onIndexerChanged;
            app.Kensaku.Changed -= onKensakuChanged;
            app.SettingsChanged -= onSettingsChanged;
            // A shortcut being recorded is taken now; a problem with it has no window left to be shown in.
            if (app.ResumeHotkeys() is string problem)
            {
                app.Notify(problem);
            }
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
        // Patterns refused once are not tried again each time focus moves; changing them tries again.
        List<string> removals = RemovalLines();
        bool removalsChanged = !removals.SequenceEqual(saved.ScreenTextRemovals) && !removals.SequenceEqual(_refusedRemovals);
        if (hiddenChanged || folderChanged || removalsChanged)
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
        RemovalsBox.Text = string.Join(Environment.NewLine, settings.ScreenTextRemovals);
        ShowFoldersBox.IsChecked = settings.ShowFolders;
        ContentSearchBox.IsChecked = settings.ContentSearch;
        ContentOcrBox.IsChecked = settings.ContentOcr;
        foreach (Viewer viewer in settings.Viewers)
        {
            _viewers.Add(new ViewerRow { Name = viewer.Name, Program = viewer.Program });
        }
        FillEnterChoices(settings);
        CheckUpdatesBox.IsChecked = settings.CheckForUpdates;
        UpdateFolderBox.Text = settings.UpdateFolder;
        VersionText.Text = $"files {Updater.Current.ToString(3)}";
        SidebarVersion.Text = $"Version {Updater.Current.ToString(3)}";
        PathsText.Text = $"Settings: {AppPaths.Settings}\nVolumes and indexes: {AppPaths.Local}";
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
        Message.Text = "Searches cleared.";
    }

    private void OnClearOpened(object sender, RoutedEventArgs e)
    {
        _app.ClearOpened();
        UpdateClearButtons();
        Message.Text = "Recent files cleared.";
    }

    private void FillEnterChoices(Settings settings)
    {
        EnterBox.Items.Clear();
        EnterBox.Items.Add(new ComboBoxItem { Content = "Windows default", Tag = Settings.DefaultOpen });
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

    /// <summary>The last save left out a program row that is not valid (see ValidViewers).</summary>
    private bool _viewerProblem;

    /// <summary>Closing was stopped once to show a program row that is not valid.</summary>
    private bool _closeWarned;

    /// <summary>Patterns refused when last saved are not kept; closing with them still in the box says so once.</summary>
    private void WarnOfRefusedPatterns()
    {
        if (_refusedRemovals.Count > 0)
        {
            _app.Notify("The removal patterns that are not valid were not saved; the ones before stay in use.");
            _refusedRemovals = [];
        }
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
            ScreenTextRemovals = RemovalLines(),
            ShowFolders = ShowFoldersBox.IsChecked == true,
            ContentSearch = ContentSearchBox.IsChecked == true,
            ContentOcr = ContentOcrBox.IsChecked == true,
            Viewers = ValidViewers(problems),
            EnterOpens = (string?)(EnterBox.SelectedItem as ComboBoxItem)?.Tag ?? Settings.DefaultOpen,
            CheckForUpdates = CheckUpdatesBox.IsChecked == true,
            UpdateFolder = UpdateFolderBox.Text.Trim(),
        };
        _viewerProblem = problems.Count > 0;
        if (!_viewerProblem)
        {
            // Fixed: a row that goes wrong later stops a close again.
            _closeWarned = false;
        }
        string? problem = _app.ApplySettings(settings);
        _refusedRemovals = _app.Settings.ScreenTextRemovals.SequenceEqual(settings.ScreenTextRemovals) ? [] : settings.ScreenTextRemovals.ToList();
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

    /// <summary>Patterns that did not compile when last saved, so leaving the box does not try them again.</summary>
    private List<string> _refusedRemovals = [];

    /// <summary>The patterns typed, one per line, without blank lines.</summary>
    private List<string> RemovalLines() =>
        RemovalsBox.Text.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries)
            .Where(line => line.Trim().Length > 0)
            .ToList();

    /// <summary>Shows what the patterns typed so far would leave of the sample, before they are saved.</summary>
    private void OnTryRemovals(object sender, TextChangedEventArgs e)
    {
        if (RemovalsTry is null || RemovalsResult is null)
        {
            // Still loading the window.
            return;
        }
        (Core.TextCleanup? cleanup, IReadOnlyList<string> problems) = Core.TextCleanup.Compile(RemovalLines());
        if (cleanup is null)
        {
            RemovalsResult.Text = problems[0];
            return;
        }
        if (RemovalsTry.Text.Trim().Length == 0)
        {
            RemovalsResult.Text = "Enter sample text to preview the search.";
            return;
        }
        ScreenRead read = ScreenText.WithRemovals(new ScreenRead(RemovalsTry.Text, null), cleanup);
        RemovalsResult.Text = read.Text is null ? read.Problem : $"Searches for: {read.Text}{(read.Problem is null ? "" : $" ({read.Problem})")}";
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

    /// <summary>The content search status line: what kensaku reads while it is up, else what the host is doing.</summary>
    private async void ShowKensakuStatus()
    {
        // StatusAsync turns every failure into words, so nothing escapes this handler.
        KensakuStatusText.Text = await new Kensaku(_app.Kensaku).StatusAsync(CancellationToken.None);
    }

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
        if (Hotkey.Refused(Keyboard.Modifiers, key) is string refused)
        {
            Message.Text = refused;
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
            // The switch shows what Windows will do, not what was asked for.
            StartWithWindowsBox.IsChecked = StartWithWindows.IsEnabled;
        }
    }

    /// <summary>
    /// Updates the rows in place, so the grid keeps its sort and scroll while volumes are read
    /// (progress arrives several times a second); only a different set of volumes rebuilds it.
    /// </summary>
    private void RefreshVolumes()
    {
        long? selected = (VolumeGrid.SelectedItem as VolumeRow)?.Id;
        // A row replaced while the keyboard is in the grid takes the focus with it; it is given back below.
        bool hadFocus = VolumeGrid.IsKeyboardFocusWithin;
        DataGridColumn? column = VolumeGrid.CurrentCell.Column;
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
        if (hadFocus && !VolumeGrid.IsKeyboardFocusWithin && VolumeGrid.SelectedItem is VolumeRow current)
        {
            VolumeGrid.CurrentCell = new DataGridCellInfo(current, column ?? VolumeGrid.Columns[0]);
            VolumeGrid.UpdateLayout();
            (VolumeGrid.ItemContainerGenerator.ContainerFromItem(current) as DataGridRow)?.Focus();
        }
        if (_editing is VolumeRow editing && !_saving && _volumeRows.All(row => row.Id != editing.Id))
        {
            // The volume being edited was removed: there is nothing left to save the form to.
            ClearVolumeForm();
            Message.Text = $"{editing.Name} was removed.";
        }
        UpdateVolumeButtons();
    }

    /// <summary>A live-only volume is never read in the background, so Read again has nothing to do for it.</summary>
    private void UpdateVolumeButtons()
    {
        bool liveOnly = VolumeGrid.SelectedItem is VolumeRow { LiveOnly: true };
        RescanButton.IsEnabled = !liveOnly;
        RescanButton.ToolTip = liveOnly ? "Live-search-only volumes are not scanned. Live search (Shift+Enter in the panel) finds their files." : null;
    }

    private void OnVolumeSelectionChanged(object sender, SelectionChangedEventArgs e) => UpdateVolumeButtons();

    /// <summary>A double-click on a row edits it; one on a header (to sort) or the empty space does not.</summary>
    private void OnVolumeDoubleClick(object sender, MouseButtonEventArgs e)
    {
        if (!_saving && ElementTree.AncestorOf<DataGridRow>(e.OriginalSource) is not null)
        {
            OnEditVolume(sender, e);
        }
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

    /// <summary>Adds the volume in the form, or saves the one being edited.</summary>
    private async void OnSaveVolume(object sender, RoutedEventArgs e)
    {
        if (_saving)
        {
            return;
        }
        string name = NewVolumeName.Text;
        string folder = NewVolumePath.Text;
        bool liveOnly = NewVolumeLiveOnly.IsChecked == true;
        VolumeRow? editing = _editing;
        // One save at a time: a second click would add the folder twice, or edit a volume the first save replaced.
        SetSaving(true);
        try
        {
            if (editing is not null && !await ConfirmEditAsync(editing, folder, liveOnly))
            {
                return;
            }
            Message.Text = editing is null ? "Adding…" : "Saving…";
            // Off the UI thread: asking a disconnected mapped drive what it is can take seconds,
            // and an edit waits for a read that is writing the volume's file list to finish.
            string? problem = await Task.Run(() => editing is null
                ? _app.Indexer.Add(name, folder, liveOnly)
                : _app.Indexer.Edit(editing.Id, name, folder, liveOnly));
            Message.Text = problem ?? "";
            // Only the form that was saved is cleared, not one started meanwhile.
            if (problem is null && _editing == editing)
            {
                ClearVolumeForm();
            }
        }
        finally
        {
            SetSaving(false);
        }
    }

    private bool _saving;

    private void SetSaving(bool saving)
    {
        _saving = saving;
        VolumeFormButton.IsEnabled = !saving;
        CancelEditButton.IsEnabled = !saving;
    }

    /// <summary>A new folder, or making a volume live-only, drops what has been read; the user agrees first.</summary>
    private async Task<bool> ConfirmEditAsync(VolumeRow editing, string folder, bool liveOnly)
    {
        bool moving;
        try
        {
            // Off the UI thread: asking a disconnected mapped drive what it is can take seconds.
            moving = await Task.Run(() => Data.VolumeRoot.Normalize(folder)) != editing.Root;
        }
        catch (Exception error) when (error is ArgumentException or NotSupportedException or IOException)
        {
            // Not a path; saving says so.
            return true;
        }
        string? consequence = (moving, liveOnly) switch
        {
            (true, false) => $"{editing.Name} will be rescanned. Until the scan finishes, only live search finds its files.",
            (_, true) when moving || !editing.LiveOnly => $"The index of {editing.Name} will be deleted. It will be searched only with live search.",
            _ => null,
        };
        return consequence is null
            || MessageBox.Show(this, consequence, "Change volume", MessageBoxButton.OKCancel, MessageBoxImage.Question) == MessageBoxResult.OK;
    }

    /// <summary>Fills the form with the selected volume, so it can be changed and saved.</summary>
    private void OnEditVolume(object sender, RoutedEventArgs e)
    {
        if (_saving)
        {
            // The volume may be replaced by the save still running.
            return;
        }
        if (VolumeGrid.SelectedItem is not VolumeRow row)
        {
            Message.Text = "Select a volume first.";
            return;
        }
        _editing = row;
        NewVolumeName.Text = row.Name;
        NewVolumePath.Text = row.Root;
        NewVolumeLiveOnly.IsChecked = row.LiveOnly;
        VolumeFormLabel.Text = $"EDIT {row.Name.ToUpperInvariant()}";
        VolumeFormButton.Content = "Save";
        CancelEditButton.Visibility = Visibility.Visible;
        NewVolumeName.Focus();
    }

    private void OnCancelEdit(object sender, RoutedEventArgs e) => ClearVolumeForm();

    private void ClearVolumeForm()
    {
        _editing = null;
        NewVolumeName.Clear();
        NewVolumePath.Clear();
        NewVolumeLiveOnly.IsChecked = false;
        VolumeFormLabel.Text = "ADD VOLUME";
        VolumeFormButton.Content = "Add";
        CancelEditButton.Visibility = Visibility.Collapsed;
    }

    private async void OnRemoveVolume(object sender, RoutedEventArgs e)
    {
        if (VolumeGrid.SelectedItem is not VolumeRow row)
        {
            Message.Text = "Select a volume first.";
            return;
        }
        MessageBoxResult answer = MessageBox.Show(this, $"Remove {row.Name}? Its index is deleted; files on disk are not affected.\n\n{row.Root}", "Remove volume",
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
        CheckButton.IsEnabled = false;
        Message.Text = "Checking…";
        try
        {
            Message.Text = await _app.CheckForUpdateAsync();
        }
        finally
        {
            CheckButton.IsEnabled = true;
        }
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

    public sealed record VolumeRow(long Id, string Name, string Root, string Files, string LastRead, string Status, bool LiveOnly)
    {
        public static VolumeRow From(VolumeState state)
        {
            string files = state.Index?.FileCount.ToString("N0") ?? "–";
            string read = state.Volume.LiveOnly ? "–" : StatusText.Age(state.Volume.LastScannedAt);
            return new VolumeRow(state.Volume.Id, state.Volume.Name, state.Volume.Root, files, read, Describe(state), state.Volume.LiveOnly);
        }

        private static string Describe(VolumeState state)
        {
            if (state.Volume.LiveOnly)
            {
                return "Live search only";
            }
            if (state.ScannedSoFar is int read)
            {
                return $"Scanning… {read:N0} files";
            }
            string? error = state.Volume.LastError switch
            {
                VolumeError.PathNotFound => "Folder not found",
                VolumeError.DriveNotFound => "Drive or server not found",
                VolumeError.PermissionDenied => "Access denied",
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
