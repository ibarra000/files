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
}
