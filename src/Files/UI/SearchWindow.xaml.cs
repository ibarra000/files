using System.Collections.ObjectModel;
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
    private HwndSourceHook? _snapHook;
    private ResourceDictionary? _themeColors;

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
        _liveTimer.Tick += async (_, _) => await ShowLiveAsync();
        DataObject.AddPastingHandler(Input, (_, _) => _pasted = true);
        PreviewKeyDown += OnKey;
        Deactivated += (_, _) => OnDeactivated();
        // The footer keeps as many key hints as the width allows.
        SizeChanged += (_, _) => UpdateStatus();
        app.Indexer.Changed += () => Dispatcher.BeginInvoke(OnIndexerChanged);
        ApplyTheme();
        ApplyRowLayout();
        SourceInitialized += (_, _) =>
        {
            Backdrop.Apply(this, app.IsDark);
            _snapHook = Snap.Attach(this, IsOverDragArea);
        };
        app.SettingsChanged += async () =>
        {
            ApplyTheme();
            Backdrop.Apply(this, app.IsDark);
            ApplyRowLayout();
            await SearchAsync();
        };
    }

    /// <summary>Puts the light or dark panel colors (UI/PanelTheme.*.xaml) in front of the styles that use them.</summary>
    private void ApplyTheme()
    {
        string name = _app.IsDark ? "PanelTheme.Dark.xaml" : "PanelTheme.Light.xaml";
        var colors = new ResourceDictionary { Source = new Uri($"/files;component/UI/{name}", UriKind.Relative) };
        Collection<ResourceDictionary> merged = Resources.MergedDictionaries;
        if (_themeColors is not null)
        {
            merged.Remove(_themeColors);
        }
        merged.Insert(0, colors);
        _themeColors = colors;
    }

    /// <summary>The folder on its own line under each name, or after it on the same line.</summary>
    private void ApplyRowLayout()
    {
        string template = _app.Settings.ShowFolders ? "TwoLineRow" : "OneLineRow";
        Results.ItemTemplate = (DataTemplate)FindResource(template);
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
        SetSpinning(false);
        SavePlacement();
        Hide();
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

    private void ShowResults(SearchResult result)
    {
        // Keep the same file selected when the rows move.
        string? selectedPath = (Results.SelectedItem as ResultRow)?.Hit?.Path;
        _result = result;
        bool showVolume = _app.Indexer.Volumes.Count > 1;
        List<ResultRow> rows = result.Hits.Select(hit => ResultRow.ForHit(hit, showVolume)).ToList();
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
                Rescan();
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
            case null when HasNoHits && _liveCancel is not null:
                // A live search is still reading; Enter must not restart it.
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

    private void OnSettingsClick(object sender, RoutedEventArgs e) => _app.ShowSettings();

}
