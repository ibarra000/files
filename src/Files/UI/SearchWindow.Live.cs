using System.Diagnostics;
using Files.Core;

namespace Files.UI;

/// <summary>Live search: reading the volumes directly, with results shown as folders are read.</summary>
public partial class SearchWindow
{
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
        Query query = SearchFilters.Narrow(Query.Parse(line), _filter);
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

        // A volume's saved list, when it has one, points the walk at the folders where matches were last time.
        var volumes = ScopedVolumes().Select(state => new LiveVolume(state.Volume.Name, state.Volume.Root, state.Index)).ToList();
        _searchTook = null;
        var live = new LiveSearch(volumes, query, _app.Settings.HiddenExtensionSet, _app.Settings.HideSystemFiles,
            opened: _app.Settings.RankOpenedHigher ? _app.OpenCounts : null);
        var cancel = new CancellationTokenSource();
        _live = live;
        _liveCancel = cancel;
        _liveStopped = false;
        _result = SearchResult.Empty;
        Results.ItemsSource = null;
        _liveSnapshotDue = 0;
        UpdateStatus();
        _liveTimer.Start();
        // Background reads of the volumes wait while this one runs, so they do not compete for the same servers.
        IDisposable scansHeld = _app.Indexer.HoldScans();
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
        finally
        {
            scansHeld.Dispose();
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

    /// <summary>
    /// Each refresh moves the footer (folders read, the folder being read or waited on).
    /// The results are taken again only once the last snapshot is done and as long again
    /// has passed twice over: on a share where much matches a snapshot takes seconds,
    /// and taking them back to back would hold up the footer and the reading.
    /// </summary>
    private async Task OnLiveTickAsync()
    {
        UpdateStatus();
        if (!_liveSnapshotPending && Stopwatch.GetTimestamp() >= _liveSnapshotDue)
        {
            await ShowLiveAsync();
        }
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
        long started = Stopwatch.GetTimestamp();
        try
        {
            result = await Task.Run(() => live.Snapshot());
        }
        finally
        {
            _liveSnapshotPending = false;
            // A snapshot of a search replaced meanwhile leaves the new one's timing alone.
            if (_live == live)
            {
                long took = Stopwatch.GetTimestamp() - started;
                _liveSnapshotDue = Stopwatch.GetTimestamp() + 2 * took;
            }
        }
        if (_live == live)
        {
            // Nothing new found: only the footer moves (folders read, the folder being read), and the rows stay as they are.
            if (ReferenceEquals(result, _result))
            {
                UpdateStatus();
            }
            else
            {
                ShowResults(result);
            }
        }
        if (_liveSnapshotAgain)
        {
            _liveSnapshotAgain = false;
            await ShowLiveAsync();
        }
    }
}
