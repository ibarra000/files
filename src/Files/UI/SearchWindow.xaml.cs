using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Threading;
using Files.Core;
using Files.Services;

namespace Files.UI;

/// <summary>The search panel the hotkey shows.</summary>
public partial class SearchWindow : Window
{
    private static readonly TimeSpan TypingDelay = TimeSpan.FromMilliseconds(150);
    private static readonly TimeSpan ToastDuration = TimeSpan.FromSeconds(4);
    private static readonly TimeSpan LiveRefresh = TimeSpan.FromMilliseconds(250);
    private const double DefaultTopFraction = 0.18;
    private const int PageRows = 8;

    private readonly App _app;
    private readonly DispatcherTimer _typing;
    private readonly DispatcherTimer _toastTimer;
    private readonly DispatcherTimer _liveTimer;
    private CancellationTokenSource? _searchCancel;
    private int _epoch;
    private SearchResult? _result;
    private string? _queryProblem;
    private string? _toast;
    private bool _pasted;
    private bool _indexerUpdatePending;
    private Point? _dragStart;
    private Snap? _snap;
    private HashSet<VolumeIndex> _searchedIndexes = [];

    // The live search on screen (running or finished), and its cancel while it runs.
    private LiveSearch? _live;
    private CancellationTokenSource? _liveCancel;
    private bool _liveStopped;
    private bool _liveSnapshotPending;
    private bool _liveSnapshotAgain;

    public SearchWindow(App app)
    {
        InitializeComponent();
        _app = app;
        _typing = new DispatcherTimer { Interval = TypingDelay };
        _typing.Tick += async (_, _) => await SearchAsync();
        _toastTimer = new DispatcherTimer { Interval = ToastDuration };
        _toastTimer.Tick += (_, _) => ShowToast(null);
        _liveTimer = new DispatcherTimer { Interval = LiveRefresh };
        _liveTimer.Tick += async (_, _) => await ShowLiveAsync();
        DataObject.AddPastingHandler(Input, (_, _) => _pasted = true);
        PreviewKeyDown += OnKey;
        Deactivated += (_, _) => OnDeactivated();
        app.Indexer.Changed += () => Dispatcher.BeginInvoke(OnIndexerChanged);
        SourceInitialized += (_, _) =>
        {
            Backdrop.Apply(this, app.IsDark);
            _snap = Snap.Attach(this, SnapButton, ShowSnapHover);
        };
        app.SettingsChanged += async () =>
        {
            Backdrop.Apply(this, app.IsDark);
            await SearchAsync();
        };
    }

    public void ShowPanel()
    {
        PlaceWindow();
        Show();
        Activate();
        Input.Focus();
        Input.SelectAll();
        _recall.Reset(_app.History);
        if (Input.Text.Length == 0)
        {
            _showAllFavorites = false;
            ShowHome();
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
        SavePlacement();
        // The pointer may be on the snap button now and elsewhere next time; no leave message arrives while hidden.
        _snap?.Reset();
        Hide();
    }

    /// <summary>Remembers where the panel is and how big, unless it is maximized (that state is kept as it is).</summary>
    private void SavePlacement()
    {
        Settings saved = _app.Settings;
        if (WindowState != WindowState.Normal)
        {
            return;
        }
        bool moved = saved.WindowLeft != Left || saved.WindowTop != Top;
        bool resized = saved.WindowWidth != Width || saved.WindowHeight != Height;
        if (moved || resized)
        {
            _app.ApplySettings(saved with { WindowLeft = Left, WindowTop = Top, WindowWidth = Width, WindowHeight = Height });
        }
    }

    private void PlaceWindow()
    {
        if (WindowState == WindowState.Minimized)
        {
            // Win+Down minimizes it, and with no taskbar button it could never be brought back.
            WindowState = WindowState.Normal;
        }
        if (WindowState == WindowState.Maximized)
        {
            return;
        }
        Rect area = SystemParameters.WorkArea;
        // A size saved on a larger screen still has to fit this one.
        Width = Math.Clamp(_app.Settings.WindowWidth ?? Width, MinWidth, Math.Max(MinWidth, area.Width));
        Height = Math.Clamp(_app.Settings.WindowHeight ?? Height, MinHeight, Math.Max(MinHeight, area.Height));
        double left = _app.Settings.WindowLeft ?? area.Left + (area.Width - Width) / 2;
        double top = _app.Settings.WindowTop ?? area.Top + area.Height * DefaultTopFraction;
        // Pull a position saved on a monitor that is gone back onto the screen.
        bool onScreen = left >= SystemParameters.VirtualScreenLeft
            && top >= SystemParameters.VirtualScreenTop
            && left + Width <= SystemParameters.VirtualScreenLeft + SystemParameters.VirtualScreenWidth
            && top + Height <= SystemParameters.VirtualScreenTop + SystemParameters.VirtualScreenHeight;
        Left = onScreen ? left : area.Left + (area.Width - Width) / 2;
        Top = onScreen ? top : area.Top + area.Height * DefaultTopFraction;
    }

    private void OnDeactivated()
    {
        bool menuOpen = Results.ContextMenu?.IsOpen == true;
        // The snap-layout flyout takes focus for a moment; hiding then would make it impossible to use.
        bool snapping = _snap?.IsOverButton == true;
        if (_app.Settings.HideOnBlur && !menuOpen && !snapping)
        {
            HidePanel();
        }
    }

    private void OnInputChanged(object sender, TextChangedEventArgs e)
    {
        Placeholder.Visibility = Input.Text.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        _typing.Stop();
        StopLiveSearch();
        if (!_recalling)
        {
            // Typing leaves the recalled search behind.
            _recall.Reset(_app.History);
        }
        if (Input.Text.Trim().Length == 0)
        {
            _searchCancel?.Cancel();
            ShowHome();
            return;
        }
        _showAllFavorites = false;
        // A paste, a recalled search or a saved one is complete already, so it searches at once.
        if (_pasted || _recalling)
        {
            _pasted = false;
            _ = SearchAsync();
            return;
        }
        _typing.Start();
    }

    private async Task SearchAsync()
    {
        _typing.Stop();
        string line = Input.Text;
        if (line.Trim().Length == 0)
        {
            return;
        }
        StopLiveSearch();
        Query query = Query.Parse(line);
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
        var volumes = _app.Indexer.Volumes
            .Where(state => state.Index is not null)
            .Select(state => (state.Volume.Name, Index: state.Index!))
            .ToList();
        IReadOnlySet<string> hidden = _app.Settings.HiddenExtensionSet;
        SearchResult result;
        try
        {
            result = await Task.Run(() => Searcher.Search(volumes, query, hidden, cancel.Token), cancel.Token);
        }
        catch (OperationCanceledException)
        {
            return;
        }
        if (epoch != _epoch)
        {
            return;
        }
        _searchedIndexes = volumes.Select(v => v.Index).ToHashSet();
        ShowResults(result);
    }

    /// <summary>
    /// Reads every volume now instead of searching the saved lists, so files
    /// added since the last scan are found. Results stream in as folders are read.
    /// </summary>
    private async Task StartLiveSearchAsync()
    {
        _typing.Stop();
        string line = Input.Text;
        if (line.Trim().Length == 0)
        {
            return;
        }
        StopLiveSearch();
        Query query = Query.Parse(line);
        _epoch++;
        _searchCancel?.Cancel();
        _queryProblem = query.Problem;
        if (!query.IsValid)
        {
            _result = null;
            Results.ItemsSource = null;
            UpdateStatus();
            return;
        }

        var volumes = _app.Indexer.Volumes.Select(state => (state.Volume.Name, state.Volume.Root)).ToList();
        var live = new LiveSearch(volumes, query, _app.Settings.HiddenExtensionSet, _app.Settings.HideSystemFiles);
        var cancel = new CancellationTokenSource();
        _live = live;
        _liveCancel = cancel;
        _liveStopped = false;
        _result = SearchResult.Empty;
        Results.ItemsSource = null;
        UpdateStatus();
        _liveTimer.Start();
        try
        {
            await Task.Run(() => live.RunAsync(cancel.Token));
        }
        catch (OperationCanceledException)
        {
            // Stopped with Esc, by typing, or by hiding the panel.
        }
        catch (Exception error)
        {
            if (_live == live)
            {
                ShowToast($"Live search stopped: {error.Message}");
            }
        }
        bool stopped = cancel.IsCancellationRequested;
        cancel.Dispose();
        if (_live != live)
        {
            // Replaced or cleared meanwhile; StopLiveSearch has tidied up.
            return;
        }
        _liveStopped = stopped;
        _liveCancel = null;
        _liveTimer.Stop();
        try
        {
            await ShowLiveAsync();
        }
        catch (Exception error)
        {
            ShowToast($"Live results could not be shown: {error.Message}");
        }
    }

    /// <summary>Cancels a running live search and takes its results off the panel's state.</summary>
    private void StopLiveSearch()
    {
        _liveCancel?.Cancel();
        _liveCancel = null;
        _liveTimer.Stop();
        _live = null;
    }

    private async Task ShowLiveAsync()
    {
        if (_live is not LiveSearch live)
        {
            return;
        }
        if (_liveSnapshotPending)
        {
            // One is being built; take another when it is done, so the last folders read are shown.
            _liveSnapshotAgain = true;
            return;
        }
        _liveSnapshotPending = true;
        SearchResult result;
        try
        {
            result = await Task.Run(() => live.Snapshot());
        }
        finally
        {
            _liveSnapshotPending = false;
        }
        if (_live == live)
        {
            ShowResults(result);
        }
        if (_liveSnapshotAgain)
        {
            _liveSnapshotAgain = false;
            await ShowLiveAsync();
        }
    }

    private void ShowResults(SearchResult result)
    {
        // Keep the same file selected when the rows move.
        string? selectedPath = (Results.SelectedItem as ResultRow)?.Hit?.Path;
        _result = result;
        List<ResultRow> rows = result.Hits.Select(ResultRow.ForHit).ToList();
        if (rows.Count == 0 && _live is null)
        {
            rows.Add(ResultRow.ForLiveHint());
        }
        Results.ItemsSource = rows;
        int keep = rows.FindIndex(row => row.Hit?.Path == selectedPath);
        Results.SelectedIndex = keep >= 0 ? keep : (rows.Count > 0 ? 0 : -1);
        UpdateStatus();
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
            UpdateStatus();
            bool indexesChanged = !_app.Indexer.Volumes
                .Where(state => state.Index is not null)
                .Select(state => state.Index!)
                .ToHashSet()
                .SetEquals(_searchedIndexes);
            if (IsVisible && indexesChanged && _live is null && Input.Text.Trim().Length > 0)
            {
                _ = SearchAsync();
            }
        });
    }

    private void UpdateStatus()
    {
        if (_toast is not null)
        {
            Status.Text = _toast;
        }
        else if (_live is LiveSearch live)
        {
            Status.Text = StatusText.ForLive(_result, live.FoldersRead, running: _liveCancel is not null, _liveStopped, live.IsCapped, live.Problems);
        }
        else
        {
            Status.Text = StatusText.For(_app.Indexer.Volumes, _result, _queryProblem);
        }
    }

    private void ShowToast(string? message)
    {
        _toastTimer.Stop();
        _toast = message;
        if (message is not null)
        {
            _toastTimer.Start();
        }
        UpdateStatus();
    }

    private void OnKey(object sender, KeyEventArgs e)
    {
        bool ctrl = Keyboard.Modifiers.HasFlag(ModifierKeys.Control);
        bool shift = Keyboard.Modifiers.HasFlag(ModifierKeys.Shift);
        e.Handled = true;
        switch (e.Key)
        {
            case Key.Down when _recall.IsActive:
                ShowRecalled(_recall.Newer());
                break;
            case Key.Down:
                Move(1);
                break;
            case Key.Up when CanRecall():
                ShowRecalled(_recall.Older());
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
            case Key.C when ctrl && Input.SelectionLength == 0:
                CopySelected(path: true);
                break;
            case Key.Escape when _liveCancel is not null:
                _liveCancel.Cancel();
                break;
            case Key.Escape when _showAllFavorites && Input.Text.Length == 0:
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
                _app.Indexer.RescanAll();
                ShowToast("Reading every volume again");
                break;
            case Key.OemComma when ctrl:
                _app.ShowSettings();
                break;
            case Key.Q when ctrl:
                _app.Quit();
                break;
            default:
                e.Handled = false;
                break;
        }
    }

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
        if (_typing.IsEnabled)
        {
            // Enter before the typing pause ended: search now, then open the best match.
            await SearchAsync();
        }
        switch (SelectedRow)
        {
            case { Kind: RowKind.LiveHint }:
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

    private async void Open(OpenWith with, ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Path is not string path)
        {
            ShowToast("Nothing to open");
            return;
        }
        string? problem;
        try
        {
            problem = await Task.Run(() => Opener.Open(path, with));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidOperationException)
        {
            problem = $"Could not open {path}: {error.Message}";
        }
        AfterOpening(row, problem);
    }

    private void Reveal(ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Path is not string path)
        {
            ShowToast("Nothing to show");
            return;
        }
        AfterOpening(row, Opener.Reveal(path));
    }

    /// <summary>Remembers an opened file and the search that found it, then hides if the settings say so.</summary>
    private void AfterOpening(ResultRow row, string? problem)
    {
        ShowToast(problem);
        if (problem is not null)
        {
            return;
        }
        if (row.Hit is Hit hit)
        {
            _app.RecordOpened(hit.Path, Input.Text);
        }
        else if (row.Item is { Kind: FavoriteKind.File } item)
        {
            _app.RecordOpened(item.Target, "");
        }
        _recall.Reset(_app.History);
        if (_app.Settings.HideAfterOpening)
        {
            HidePanel();
        }
    }

    private void CopySelected(bool path, ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Path is not string full)
        {
            ShowToast("Nothing to copy");
            return;
        }
        string text = path ? full : Path.GetFileName(full.TrimEnd('\\'));
        try
        {
            Clipboard.SetText(text);
            ShowToast($"Copied {text}");
        }
        catch (COMException)
        {
            ShowToast("The clipboard is busy · try again");
        }
    }

    private void OnResultDoubleClick(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource is FrameworkElement { DataContext: ResultRow })
        {
            Accept();
        }
    }

    private void OnResultMenuOpening(object sender, ContextMenuEventArgs e)
    {
        if (SelectedRow is not ResultRow row || (row.Path is null && row.Item is null))
        {
            e.Handled = true;
            return;
        }
        var menu = Results.ContextMenu!;
        menu.Items.Clear();
        if (row.Path is not null)
        {
            AddMenuItem(menu, "Open", "Enter", () => Open(OpenWith.ForEnter(_app.Settings), row));
            AddMenuItem(menu, "Open with default app", "", () => Open(new OpenWith.Default(), row));
            if (row.Item?.Kind != FavoriteKind.Folder)
            {
                foreach (Viewer viewer in _app.Settings.Viewers)
                {
                    AddMenuItem(menu, $"Open with {viewer.Name}", "", () => Open(new OpenWith.Program(viewer), row));
                }
            }
            menu.Items.Add(new Separator());
            AddMenuItem(menu, "Show in folder", "Ctrl+O", () => Reveal(row));
            AddMenuItem(menu, "Copy path", "Ctrl+C", () => CopySelected(path: true, row));
            AddMenuItem(menu, "Copy name", "Ctrl+Shift+C", () => CopySelected(path: false, row));
            menu.Items.Add(new Separator());
        }
        AddFavoriteItems(menu, row);
    }

    private static void AddMenuItem(ContextMenu menu, string header, string gesture, Action action)
    {
        var item = new MenuItem { Header = header, InputGestureText = gesture };
        item.Click += (_, _) => action();
        menu.Items.Add(item);
    }

    private void OnResultMouseDown(object sender, MouseButtonEventArgs e) => _dragStart = e.GetPosition(Results);

    /// <summary>Dragging a result out drops the file itself into Explorer, mail or any other app.</summary>
    private void OnResultMouseMove(object sender, MouseEventArgs e)
    {
        if (e.LeftButton != MouseButtonState.Pressed || _dragStart is not Point start || SelectedRow?.Path is not string path)
        {
            return;
        }
        Vector moved = e.GetPosition(Results) - start;
        bool farEnough = Math.Abs(moved.X) > SystemParameters.MinimumHorizontalDragDistance
            || Math.Abs(moved.Y) > SystemParameters.MinimumVerticalDragDistance;
        if (farEnough)
        {
            _dragStart = null;
            DragDrop.DoDragDrop(Results, new DataObject(DataFormats.FileDrop, new[] { path }), DragDropEffects.Copy);
        }
    }

    private void ShowSnapHover(bool over)
    {
        if (over)
        {
            SnapButton.SetResourceReference(Border.BackgroundProperty, "SubtleFillColorSecondaryBrush");
        }
        else
        {
            SnapButton.ClearValue(Border.BackgroundProperty);
        }
    }

    private void OnDragWindow(object sender, MouseButtonEventArgs e)
    {
        if (e.OriginalSource == sender && e.ButtonState == MouseButtonState.Pressed)
        {
            DragMove();
        }
    }
}
