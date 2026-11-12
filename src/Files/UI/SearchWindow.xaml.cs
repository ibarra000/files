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
        SourceInitialized += (_, _) => Backdrop.Apply(this, app.IsDark);
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
        if (Input.Text.Length == 0)
        {
            ShowRecent();
        }
        UpdateStatus();
    }

    public void HidePanel()
    {
        if (!IsVisible)
        {
            return;
        }
        _app.RecordSearch(Input.Text);
        StopLiveSearch();
        if (_app.Settings.WindowLeft != Left || _app.Settings.WindowTop != Top)
        {
            _app.ApplySettings(_app.Settings with { WindowLeft = Left, WindowTop = Top });
        }
        Hide();
    }

    private void PlaceWindow()
    {
        Rect area = SystemParameters.WorkArea;
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
        if (_app.Settings.HideOnBlur && !menuOpen)
        {
            HidePanel();
        }
    }

    private void OnInputChanged(object sender, TextChangedEventArgs e)
    {
        Placeholder.Visibility = Input.Text.Length == 0 ? Visibility.Visible : Visibility.Collapsed;
        Alias? alias = Alias.Resolve(_app.Settings.Aliases, Input.Text);
        AliasHint.Text = alias is null ? "" : $"→ {alias.Code}";
        _typing.Stop();
        StopLiveSearch();
        if (Input.Text.Trim().Length == 0)
        {
            _searchCancel?.Cancel();
            ShowRecent();
            return;
        }
        // A paste or an alias is complete already, so it searches at once.
        if (_pasted || alias is not null)
        {
            _pasted = false;
            _ = SearchAsync();
            return;
        }
        _typing.Start();
    }

    private void ShowRecent()
    {
        // A search still running must not replace this list when it finishes.
        _epoch++;
        _result = null;
        _queryProblem = null;
        Results.ItemsSource = _app.History.Take(20).Select(ResultRow.ForRecent).ToList();
        UpdateStatus();
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
        Query query = ParseInput(line);
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

    private Query ParseInput(string line)
    {
        Alias? alias = Alias.Resolve(_app.Settings.Aliases, line);
        return Query.Parse(alias?.Code ?? line);
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
        Query query = ParseInput(line);
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
            case Key.Down:
                Move(1);
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
                Open(new OpenWith.CombinedPdf());
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
        int count = Results.Items.Count;
        if (count == 0)
        {
            return;
        }
        Results.SelectedIndex = Math.Clamp(Results.SelectedIndex + by, 0, count - 1);
        Results.ScrollIntoView(Results.SelectedItem);
    }

    private async void Accept()
    {
        if (Results.SelectedItem is ResultRow { RecentSearch: string search })
        {
            Input.Text = search;
            Input.CaretIndex = search.Length;
            await SearchAsync();
            return;
        }
        if (_typing.IsEnabled)
        {
            // Enter before the typing pause ended: search now, then open the best match.
            await SearchAsync();
        }
        if (Results.SelectedItem is ResultRow { IsLiveHint: true })
        {
            await StartLiveSearchAsync();
            return;
        }
        Open(OpenWith.ForEnter(_app.Settings));
    }

    private Hit? Selected => (Results.SelectedItem as ResultRow)?.Hit;

    private async void Open(OpenWith with, Hit? hit = null)
    {
        hit ??= Selected;
        if (hit is null)
        {
            ShowToast("Nothing to open");
            return;
        }
        if (with is OpenWith.CombinedPdf && PageSet.BaseOf(hit.Name) is not null)
        {
            ShowToast("Opening…");
        }
        string? problem;
        try
        {
            problem = await Task.Run(() => _app.Opener.Open(hit, with));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidOperationException)
        {
            problem = $"Could not open {hit.Name}: {error.Message}";
        }
        _app.RecordSearch(Input.Text);
        ShowToast(problem);
        if (problem is null && _app.Settings.HideAfterOpening)
        {
            HidePanel();
        }
    }

    private void Reveal(Hit? hit = null)
    {
        hit ??= Selected;
        if (hit is null)
        {
            ShowToast("Nothing to show");
            return;
        }
        string? problem = Opener.Reveal(hit.Path);
        ShowToast(problem);
        if (problem is null && _app.Settings.HideAfterOpening)
        {
            HidePanel();
        }
    }

    private void CopySelected(bool path, Hit? hit = null)
    {
        hit ??= Selected;
        if (hit is null)
        {
            ShowToast("Nothing to copy");
            return;
        }
        string text = path ? hit.Path : hit.Name;
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
        if (Selected is not Hit hit)
        {
            e.Handled = true;
            return;
        }
        var menu = Results.ContextMenu!;
        menu.Items.Clear();
        AddMenuItem(menu, "Open", "Enter", () => Open(OpenWith.ForEnter(_app.Settings), hit));
        AddMenuItem(menu, "Open with default app", "", () => Open(new OpenWith.Default(), hit));
        if (PageSet.BaseOf(hit.Name) is not null)
        {
            AddMenuItem(menu, "Open as combined PDF", "Ctrl+D", () => Open(new OpenWith.CombinedPdf(), hit));
        }
        foreach (Viewer viewer in _app.Settings.Viewers)
        {
            AddMenuItem(menu, $"Open with {viewer.Name}", "", () => Open(new OpenWith.Program(viewer), hit));
        }
        menu.Items.Add(new Separator());
        AddMenuItem(menu, "Show in folder", "Ctrl+O", () => Reveal(hit));
        AddMenuItem(menu, "Copy path", "Ctrl+C", () => CopySelected(path: true, hit));
        AddMenuItem(menu, "Copy name", "Ctrl+Shift+C", () => CopySelected(path: false, hit));
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
        if (e.LeftButton != MouseButtonState.Pressed || _dragStart is not Point start || Selected is not Hit hit)
        {
            return;
        }
        Vector moved = e.GetPosition(Results) - start;
        bool farEnough = Math.Abs(moved.X) > SystemParameters.MinimumHorizontalDragDistance
            || Math.Abs(moved.Y) > SystemParameters.MinimumVerticalDragDistance;
        if (farEnough)
        {
            _dragStart = null;
            DragDrop.DoDragDrop(Results, new DataObject(DataFormats.FileDrop, new[] { hit.Path }), DragDropEffects.Copy);
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
