using Files.Core;
using Files.Services;

namespace Files.UI;

/// <summary>Files found by what is inside them, from kensaku, listed after the name results once they come back.</summary>
public partial class SearchWindow
{
    private static readonly TimeSpan KensakuStartup = TimeSpan.FromSeconds(5);

    private CancellationTokenSource? _contentCancel;

    /// <summary>The content matches for <see cref="_contentsFor"/>, the line they were searched for.</summary>
    private IReadOnlyList<ContentMatch>? _contents;
    private string? _contentsFor;

    /// <summary>Why the last content search gave nothing, said once until it changes.</summary>
    private string? _contentProblem;

    /// <summary>kensaku-cli is started at most once while the app runs.</summary>
    private static bool _kensakuStarted;

    /// <summary>The content matches for what is in the box now, or none (also when content search was turned off, or Folders is chosen).</summary>
    private IReadOnlyList<ContentMatch>? ContentsShown =>
        _app.Settings.ContentSearch && _filter != SearchFilter.Folders && _contentsFor == Input.Text ? _contents : null;

    /// <summary>Stops a content search in flight and forgets what it found, when the search changes or the panel closes.</summary>
    private void StopContentSearch()
    {
        _contentCancel?.Cancel();
        _contentCancel?.Dispose();
        _contentCancel = null;
    }

    /// <summary>Asks kensaku for <paramref name="line"/> after the name results are on screen; a newer search cancels it.</summary>
    private async Task SearchContentsAsync(string line, int epoch)
    {
        StopContentSearch();
        Settings settings = _app.Settings;
        if (!settings.ContentSearch)
        {
            return;
        }
        var cancel = new CancellationTokenSource();
        _contentCancel = cancel;
        ContentResult result;
        try
        {
            // The default data folder is looked up on disk, which may be a slow share: off the UI thread.
            string dataFolder = await Task.Run(() => settings.KensakuDataFolder, cancel.Token);
            var kensaku = new Kensaku(settings.KensakuPort, dataFolder);
            result = await kensaku.SearchAsync(line, cancel.Token);
            if (Kensaku.IsAbsent(result.Problem) && !_kensakuStarted && settings.KensakuCli.Trim().Length > 0)
            {
                _kensakuStarted = true;
                string cli = settings.KensakuCli.Trim();
                string? startProblem = await Task.Run(() => kensaku.Start(cli), cancel.Token);
                if (startProblem is not null)
                {
                    result = ContentResult.Unavailable(startProblem);
                }
                else if (await kensaku.WaitUntilUpAsync(KensakuStartup, cancel.Token))
                {
                    result = await kensaku.SearchAsync(line, cancel.Token);
                }
            }
        }
        catch (OperationCanceledException)
        {
            return;
        }
        if (epoch != _epoch || Input.Text != line)
        {
            return;
        }
        // Rows are redrawn when what is listed changes, also when matches shown before are gone now.
        bool changed = !(_contents ?? []).Select(match => match.Path).SequenceEqual(result.Matches.Select(match => match.Path));
        _contents = result.Matches;
        _contentsFor = line;
        if (result.Problem != _contentProblem)
        {
            _contentProblem = result.Problem;
            if (result.Problem is not null)
            {
                ShowToast($"Content search unavailable · {result.Problem}");
            }
        }
        if (_live is null && _result is SearchResult shown && changed)
        {
            ShowResults(shown);
        }
    }

    /// <summary>Brings kensaku forward with <paramref name="text"/> on the clipboard (Open in kensaku, Find in kensaku).</summary>
    private void OpenInKensaku(string text)
    {
        Settings settings = _app.Settings;
        ShowToast(Kensaku.OpenApp(Kensaku.AppPath(settings.KensakuCli, settings.KensakuDataFolder), text));
    }
}
