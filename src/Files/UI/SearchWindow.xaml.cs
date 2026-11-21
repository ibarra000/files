using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Threading;
using Files.Core;
using Files.Services;

namespace Files.UI;

/// <summary>The search panel the hotkey shows.</summary>
public partial class SearchWindow : Window
{
    private static readonly TimeSpan TypingDelay = TimeSpan.FromMilliseconds(150);
    private static readonly TimeSpan ToastDuration = TimeSpan.FromSeconds(4);
    private static readonly TimeSpan MinScanShown = TimeSpan.FromSeconds(1);
    private static readonly TimeSpan LiveRefresh = TimeSpan.FromMilliseconds(250);
    private static readonly Duration SpinDuration = new(TimeSpan.FromSeconds(1));
    private const double DefaultTopFraction = 0.18;
    private const int PageRows = 8;

    private readonly App _app;
    private readonly DispatcherTimer _typing;
    private readonly DispatcherTimer _toastTimer;
    private readonly DispatcherTimer _liveTimer;
    private readonly DispatcherTimer _rescanTimer;
    private DateTime _rescanShownUntil;
    private CancellationTokenSource? _searchCancel;
    private Task? _searching;
    private bool _accepting;

    /// <summary>The box text is selected only because the panel was just shown, not by the user.</summary>
    private bool _selectedOnShow;
    private bool _hiding;
    private int _epoch;
    private SearchResult? _result;
    private string? _queryProblem;
    private string? _toast;
    private bool _pasted;
    private bool _indexerUpdatePending;
    private Point? _dragStart;
    private HashSet<VolumeIndex> _searchedIndexes = [];

    // The live search on screen (running or finished), and its cancel while it runs.
    private LiveSearch? _live;
    private CancellationTokenSource? _liveCancel;
    private bool _liveStopped;
    private bool _liveSnapshotPending;
    private bool _liveSnapshotAgain;
    // When the next snapshot of a running live search may be taken: spaced by how long the last one took.
    private long _liveSnapshotDue;
    private HwndSourceHook? _snapHook;

    public SearchWindow(App app)
    {
        InitializeComponent();
        _app = app;
        _typing = new DispatcherTimer { Interval = TypingDelay };
        _typing.Tick += async (_, _) => await SearchAsync();
        _toastTimer = new DispatcherTimer { Interval = ToastDuration };
        _toastTimer.Tick += (_, _) => ShowToast(null);
        _rescanTimer = new DispatcherTimer { Interval = MinScanShown };
        _rescanTimer.Tick += (_, _) =>
        {
            _rescanTimer.Stop();
            UpdateStatus();
        };
        _liveTimer = new DispatcherTimer { Interval = LiveRefresh };
        _liveTimer.Tick += async (_, _) =>
        {
            try
            {
                await OnLiveTickAsync();
            }
            catch (Exception error) when (error is InvalidOperationException or ArgumentException or IOException)
            {
                // A timer callback must not let an exception end the process.
                ShowToast($"Live results could not be shown: {error.Message}");
            }
        };
        DataObject.AddPastingHandler(Input, (_, _) => _pasted = true);
        PreviewKeyDown += OnKey;
        PreviewTextInput += OnActionText;
        Deactivated += (_, _) => OnDeactivated();
        Closing += OnClosing;
        // The footer keeps as many key hints as the width allows.
        SizeChanged += (_, _) => UpdateStatus();
        // A click selects a row by focusing it; the keyboard goes straight back to the search box,
        // so typing, Backspace and Ctrl+C keep working after a click.
        Results.IsKeyboardFocusWithinChanged += (_, e) =>
        {
            if (e.NewValue is true)
            {
                Dispatcher.BeginInvoke(DispatcherPriority.Input, () => Input.Focus());
            }
        };
        // The preview pane's width is added or taken off once a maximized panel is restored.
        StateChanged += (_, _) =>
        {
            CloseMenus();
            FitPreviewWidth();
        };
        // A popup does not follow its window, so the menus close rather than float where the panel was.
        LocationChanged += (_, _) => CloseMenus();
        app.Indexer.Changed += () => Dispatcher.BeginInvoke(OnIndexerChanged);
        ApplyRowLayout();
        ApplyPreview(app.Settings.ShowPreview);
        SourceInitialized += (_, _) =>
        {
            Backdrop.Apply(this, app.IsDark);
            _snapHook = Snap.Attach(this, IsOverDragArea);
        };
        _searchSettings = SearchSettingsOf(app.Settings);
        app.SettingsChanged += OnSettingsChanged;
    }

    /// <summary>The settings a search depends on; only a change to these runs the search again.</summary>
    private string _searchSettings;

    private static string SearchSettingsOf(Settings settings) => string.Join(",", settings.HiddenExtensionSet.Order(StringComparer.Ordinal));

    /// <summary>
    /// Settings change for many reasons (Ctrl+P, saving the panel's place on hide, the update check, an edit in
    /// Settings). The look always follows; the list is only rebuilt when what it shows depends on the change,
    /// and a live search is never interrupted.
    /// </summary>
    private async void OnSettingsChanged()
    {
        Backdrop.Apply(this, _app.IsDark);
        ApplyRowLayout();
        ApplyPreview(_app.Settings.ShowPreview);
        string searchSettings = SearchSettingsOf(_app.Settings);
        bool searchChanged = searchSettings != _searchSettings;
        _searchSettings = searchSettings;
        if (!IsVisible || _hiding)
        {
            // ShowPanel rebuilds the list; a hidden or hiding panel does no work for it.
            return;
        }
        if (IsHome)
        {
            // Show recently opened, or favorites, may have changed.
            ShowHome(keepSelection: true);
            return;
        }
        if (searchChanged && _live is null)
        {
            await SearchAsync();
            return;
        }
        UpdateStatus();
    }

    /// <summary>The folder on its own line under each name, or after it on the same line.</summary>
    private void ApplyRowLayout()
    {
        string template = _app.Settings.ShowFolders ? "TwoLineRow" : "OneLineRow";
        Results.ItemTemplate = (DataTemplate)FindResource(template);
    }

    public void ShowPanel()
    {
        // Only a hidden panel goes back to its saved place; a visible one stays where the user put it.
        bool wasHidden = !IsVisible;
        if (wasHidden)
        {
            PlaceWindow();
        }
        // Win+Down can minimize a panel that stays open; it has no taskbar button to restore it from.
        if (WindowState == WindowState.Minimized)
        {
            WindowState = WindowState.Normal;
        }
        Show();
        Activate();
        Input.Focus();
        Input.SelectAll();
        // After SelectAll, whose selection change (raised at once) would otherwise clear it.
        _selectedOnShow = Input.Text.Length > 0;
        _recall.Reset(_app.History);
        if (IsHome)
        {
            _showAllFavorites = false;
            ShowHome();
        }
        else if (wasHidden)
        {
            // What was found before may come from a live search that hiding stopped, or from lists read since: search again (the selection is kept).
            // A panel that was already showing keeps what it shows, a live search included.
            _ = SearchAsync();
        }
        UpdateStatus();
    }

    public void HidePanel()
    {
        if (!IsVisible)
        {
            return;
        }
        StopLiveSearch();
        CloseMenus();
        StopPreview();
        // A search typed just before hiding would otherwise finish, and fill the list and read a preview, while hidden.
        // Showing the panel again searches for what is in the box.
        _typing.Stop();
        _searchCancel?.Cancel();
        // Saving the place raises a settings change while the panel is still visible; it must not rebuild the list,
        // restart a preview read or check favorites for a panel about to hide.
        _hiding = true;
        try
        {
            SavePlacement();
        }
        finally
        {
            _hiding = false;
        }
        SetSpinning(false);
        Hide();
    }

    /// <summary>The action menu and the volume menu belong to what is on screen; they never outlive it.</summary>
    private void CloseMenus()
    {
        ActionMenu.IsOpen = false;
        if (_scopeMenu is not null)
        {
            _scopeMenu.IsOpen = false;
        }
    }

    private void OnDeactivated()
    {
        // The menus are popups that never take activation, so a click elsewhere means the user moved on.
        CloseMenus();
        if (_app.Settings.HideOnBlur)
        {
            HidePanel();
        }
    }

    /// <summary>Alt+F4 or Close in the system menu would destroy the panel for good; it only hides, until the app quits.</summary>
    private void OnClosing(object? sender, System.ComponentModel.CancelEventArgs e)
    {
        if (_app.IsQuitting)
        {
            return;
        }
        e.Cancel = true;
        HidePanel();
    }

    /// <summary>The user chose or moved the selection in the box, so Ctrl+C copies what is selected there.</summary>
    private void OnInputSelectionChanged(object sender, RoutedEventArgs e) => _selectedOnShow = false;

    private void OnInputChanged(object sender, TextChangedEventArgs e)
    {
        Placeholder.Visibility = Input.Text.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        _typing.Stop();
        StopLiveSearch();
        // A paste, a recalled search or a saved one is complete already, so it searches at once.
        // Read and cleared on every change, so it can never carry over to the next keystroke.
        bool complete = _pasted || _recalling;
        _pasted = false;
        if (!_recalling)
        {
            // Typing leaves the recalled search behind.
            _recall.Reset(_app.History);
        }
        if (IsHome)
        {
            _searchCancel?.Cancel();
            ShowHome();
            return;
        }
        _showAllFavorites = false;
        if (complete)
        {
            _ = SearchAsync();
            return;
        }
        _typing.Start();
        // The chips, count and footer follow at once, not when the results arrive.
        UpdateStatus();
    }

    private Task SearchAsync()
    {
        // Kept so Enter can wait for the search that is still running for what is in the box.
        Task search = SearchCoreAsync();
        _searching = search;
        return search;
    }

    private async Task SearchCoreAsync()
    {
        _typing.Stop();
        string line = Input.Text;
        if (line.Trim().Length == 0)
        {
            return;
        }
        StopLiveSearch();
        Query query = SearchFilters.Narrow(Query.Parse(line), _filter);
        int epoch = ++_epoch;
        _searchCancel?.Cancel();
        _queryProblem = query.Problem;
        if (!query.IsValid)
        {
            _result = null;
            Results.ItemsSource = null;
            UpdateStatus();
            return;
        }

        var cancel = new CancellationTokenSource();
        _searchCancel = cancel;
        var volumes = ScopedVolumes()
            .Where(state => state.Index is not null)
            .Select(state => (state.Volume.Name, Index: state.Index!))
            .ToList();
        IReadOnlySet<string> hidden = _app.Settings.HiddenExtensionSet;
        SearchResult result;
        var clock = System.Diagnostics.Stopwatch.StartNew();
        try
        {
            result = await Task.Run(() => Searcher.Search(volumes, query, hidden, cancel.Token), cancel.Token);
        }
        catch (OperationCanceledException)
        {
            return;
        }
        catch (Exception error) when (epoch == _epoch)
        {
            // This runs from timers and settings changes, where an exception would end the app: the footer says so instead.
            ShowToast($"The search failed: {error.Message}");
            return;
        }
        if (epoch != _epoch)
        {
            return;
        }
        _searchedIndexes = volumes.Select(v => v.Index).ToHashSet();
        _searchTook = clock.Elapsed;
        ShowResults(result);
    }

    /// <summary>By volume id, the marks on the rows on screen, so a volume going offline or coming back redraws them.</summary>
    private Dictionary<long, string>? _shownStale;

    private bool StaleMarksChanged(IReadOnlyList<VolumeState> volumes)
    {
        Dictionary<long, string>? now = StaleMarks(volumes);
        return now?.Count != _shownStale?.Count || (now is not null && now.Any(mark => _shownStale!.GetValueOrDefault(mark.Key) != mark.Value));
    }

    /// <summary>By volume id, the mark for results from each volume that cannot be reached; none after a live search, which read them just now.</summary>
    private Dictionary<long, string>? StaleMarks(IReadOnlyList<VolumeState> volumes)
    {
        if (_live is not null)
        {
            return null;
        }
        var marks = new Dictionary<long, string>();
        foreach (VolumeState state in volumes)
        {
            if (StatusText.Stale(state) is string mark)
            {
                marks.Add(state.Volume.Id, mark);
            }
        }
        return marks;
    }

    /// <summary>The mark for a result, found by the list it came from, since two volumes may share a name.</summary>
    private static string? StaleMark(Hit hit, IReadOnlyList<VolumeState> volumes, Dictionary<long, string>? marks)
    {
        VolumeState? from = volumes.FirstOrDefault(state => ReferenceEquals(state.Index, hit.Index));
        return from is null ? null : marks?.GetValueOrDefault(from.Volume.Id);
    }

    private void ShowResults(SearchResult result)
    {
        // Keep the same file selected when the rows move.
        string? selectedPath = (Results.SelectedItem as ResultRow)?.Hit?.Path;
        // Results for the same search (a live search's progress, a volume read again) keep the list where it was scrolled;
        // another chip or volume is another list, which starts at the top.
        string shownFor = $"{Input.Text}\n{_filter}\n{_scope}";
        bool sameSearch = shownFor == _shownFor;
        double scrolled = sameSearch && ListScroller() is ScrollViewer before ? before.VerticalOffset : 0;
        _shownFor = shownFor;
        _result = result;
        IReadOnlyList<VolumeState> volumes = _app.Indexer.Volumes;
        bool showVolume = volumes.Count > 1;
        Dictionary<long, string>? marks = StaleMarks(volumes);
        _shownStale = marks;
        List<ResultRow> rows = SearchRows.Build(result, showVolume, _filter, hit => StaleMark(hit, volumes, marks));
        Results.ItemsSource = rows;
        KeepActionsFor(rows);
        // A recalled search starts at the top row, where Up and Down keep stepping through past searches.
        int keep = _recall.IsActive && !sameSearch ? -1 : rows.FindIndex(row => row.Hit is not null && row.Hit.Path == selectedPath);
        // The first row is a section label, so the first result is the next one that can be selected.
        Results.SelectedIndex = keep >= 0 ? keep : NextSelectable(0, 1) ?? -1;
        if (scrolled > 0)
        {
            Results.UpdateLayout();
            ListScroller()?.ScrollToVerticalOffset(scrolled);
        }
        UpdateStatus();
    }

    /// <summary>The search whose results are on screen, so a refresh of them can keep the scroll; null on the home list.</summary>
    private string? _shownFor;

    private ScrollViewer? ListScroller() => FirstScroller(Results);

    private static ScrollViewer? FirstScroller(DependencyObject parent)
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(parent); i++)
        {
            DependencyObject child = VisualTreeHelper.GetChild(parent, i);
            if ((child as ScrollViewer ?? FirstScroller(child)) is ScrollViewer found)
            {
                return found;
            }
        }
        return null;
    }

    private void OnIndexerChanged()
    {
        // Indexer events can come in bursts during a scan; one update per frame is enough.
        if (_indexerUpdatePending)
        {
            return;
        }
        _indexerUpdatePending = true;
        Dispatcher.BeginInvoke(DispatcherPriority.Background, () =>
        {
            _indexerUpdatePending = false;
            // Resolved first, so a volume removed in Settings also leaves the scope label.
            bool indexesChanged = !ScopedVolumes()
                .Where(state => state.Index is not null)
                .Select(state => state.Index!)
                .ToHashSet()
                .SetEquals(_searchedIndexes);
            UpdateStatus();
            if (IsVisible && indexesChanged && _live is null && !IsHome)
            {
                // A volume read again may have changed the selected file: the preview reads it again.
                _previewedPath = null;
                _ = SearchAsync();
            }
            else if (IsVisible && !IsHome && _result is SearchResult shown && StaleMarksChanged(_app.Indexer.Volumes))
            {
                ShowResults(shown);
            }
        });
    }

    private void OnKey(object sender, KeyEventArgs e)
    {
        if (_scopeMenu?.IsOpen == true)
        {
            // The volume menu handles its own keys, F5 among them.
            return;
        }
        bool ctrl = Keyboard.Modifiers.HasFlag(ModifierKeys.Control);
        bool shift = Keyboard.Modifiers.HasFlag(ModifierKeys.Shift);
        if (ActionMenu.IsOpen)
        {
            // A shortcut the menu lists beside one of its actions runs that action for the menu's row.
            // Other shortcuts, such as Ctrl+V, would change the search under the open menu, so they do nothing here.
            if (MenuGesture(e.Key, ctrl, shift) is string gesture && _actions.FirstOrDefault(action => action.Gesture == gesture) is RowAction listed)
            {
                RunAction(listed);
                e.Handled = true;
                return;
            }
            bool chord = ctrl || Keyboard.Modifiers.HasFlag(ModifierKeys.Alt);
            e.Handled = chord || e.Key == Key.Delete || OnActionKey(e.Key);
            return;
        }
        e.Handled = true;
        switch (e.Key)
        {
            case Key.Tab when !ctrl:
                OpenActions();
                break;
            case Key.Delete when IsHome && SelectedRow is { Kind: RowKind.Opened } recent:
                RemoveRecent(recent);
                break;
            case Key.System when e.SystemKey == Key.Down && Keyboard.Modifiers == ModifierKeys.Alt:
                // Alt+Down opens the volume menu, as it opens a dropdown.
                OpenScopeMenu();
                break;
            case Key.Down when CanRecallNewer():
                ShowRecalled(_recall.Newer());
                break;
            case Key.Down:
                Move(1);
                break;
            case Key.Up when CanRecall():
                ShowRecalled(_recall.Older(Input.Text));
                break;
            case Key.Up:
                Move(-1);
                break;
            case Key.PageDown:
                Move(PageRows);
                break;
            case Key.PageUp:
                Move(-PageRows);
                break;
            case Key.Enter when ctrl:
            case Key.O when ctrl:
                Reveal();
                break;
            case Key.Enter when shift:
                _ = StartLiveSearchAsync();
                break;
            case Key.Enter:
                Accept();
                break;
            case Key.D when ctrl:
                ToggleFavorite(FavoriteFor(SelectedRow));
                break;
            case Key.C when ctrl && shift:
                CopySelected(path: false);
                break;
            case Key.C when ctrl && (Input.SelectionLength == 0 || _selectedOnShow):
                // The box text is selected only because the panel was shown: Ctrl+C copies the path, as the footer says.
                CopySelected(path: true);
                break;
            case Key.Escape when _liveCancel is not null:
                _liveCancel.Cancel();
                break;
            case Key.Escape when _showAllFavorites && IsHome:
                _showAllFavorites = false;
                ShowHome();
                break;
            case Key.Escape when Input.Text.Length > 0:
                Input.Clear();
                break;
            case Key.Escape:
                HidePanel();
                break;
            case Key.F5:
                Rescan();
                break;
            case Key.OemComma when ctrl:
                _app.ShowSettings();
                break;
            case Key.Q when ctrl:
                _app.Quit();
                break;
            case Key.P when ctrl:
                TogglePreview();
                break;
            case >= Key.D1 and <= Key.D5 when ctrl && !IsHome:
                // Only while the chips show: on the home list a hidden chip would narrow the next search unseen.
                SetFilter((SearchFilter)(e.Key - Key.D1));
                break;
            default:
                e.Handled = false;
                break;
        }
    }

    /// <summary>A key as the action menu writes it beside an action (RowAction.Gesture), or null for any other.</summary>
    private static string? MenuGesture(Key key, bool ctrl, bool shift) => (key, ctrl, shift) switch
    {
        (Key.Delete, false, false) => "Del",
        (Key.D, true, false) => "Ctrl D",
        (Key.Enter, true, false) => "Ctrl ↵",
        (Key.C, true, false) => "Ctrl C",
        (Key.C, true, true) => "Ctrl Shift C",
        _ => null,
    };

    private void Move(int by)
    {
        if (Results.Items.Count == 0)
        {
            return;
        }
        int step = by > 0 ? 1 : -1;
        int target = Math.Clamp(Results.SelectedIndex + by, 0, Results.Items.Count - 1);
        // Section headers cannot be selected: keep going past them, or turn back at the end of the list.
        int? index = NextSelectable(target, step) ?? NextSelectable(target, -step);
        if (index is int found)
        {
            Results.SelectedIndex = found;
            Results.ScrollIntoView(Results.SelectedItem);
        }
    }

    private async void Accept()
    {
        if (_accepting)
        {
            // Already waiting for the search to finish; a second Enter must not open another file.
            return;
        }
        _accepting = true;
        try
        {
            await AcceptAsync();
        }
        finally
        {
            _accepting = false;
        }
    }

    private async Task AcceptAsync()
    {
        // What Enter was pressed for: if the box changes or the panel hides while the search finishes, nothing opens.
        string pressedFor = Input.Text;
        if (_typing.IsEnabled)
        {
            // Enter before the typing pause ended: search now, then open the best match.
            await SearchAsync();
        }
        else
        {
            // A paste, recall, saved search or screen read is still being searched: open its best match, not the old row.
            // Waits for the newest one, since a search started meanwhile replaces the one awaited.
            try
            {
                while (_searching is { IsCompleted: false } running)
                {
                    await running;
                }
            }
            catch (Exception error) when (error is InvalidOperationException or ArgumentException or IOException)
            {
                ShowToast($"The search failed: {error.Message}");
                return;
            }
        }
        if (_queryProblem is not null || Input.Text != pressedFor || !IsVisible)
        {
            // The footer already says what is wrong with the search; or Esc, typing or hiding has moved on from it.
            return;
        }
        switch (SelectedRow)
        {
            case null when HasNoHits && _live is not null:
                // A live search is reading, or has read everything and found nothing; Enter does not start another.
                return;
            case null when HasNoHits:
                // Nothing matched: Enter reads the volumes directly, as Shift+Enter does.
                await StartLiveSearchAsync();
                return;
            case { Kind: RowKind.SeeAll }:
                _showAllFavorites = true;
                ShowHome();
                return;
            case { Item.Kind: FavoriteKind.Search } row:
                RunSearch(row.Item.Target);
                return;
            case { Missing: true } row:
                ShowToast($"{row.Before} was not found · Ctrl+D removes it from favorites");
                return;
        }
        Open(OpenWith.ForEnter(_app.Settings));
    }

    private void OnSettingsClick(object sender, RoutedEventArgs e) => _app.ShowSettings();

}
