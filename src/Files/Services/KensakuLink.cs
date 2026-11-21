using System.ComponentModel;
using System.Diagnostics;

namespace Files.Services;

/// <summary>The settings a <see cref="KensakuLink"/> works for: kensaku-cli (empty to never start it), the port, and the data folder as set (empty for kensaku's default).</summary>
public sealed record KensakuKey(string Cli, int Port, string Data)
{
    public static KensakuKey Of(Settings settings) => new(settings.KensakuCli.Trim(), settings.KensakuPort, settings.KensakuData.Trim());
}

/// <summary>What the panel knows of kensaku for the settings in use.</summary>
public enum KensakuState
{
    /// <summary>Not asked yet since these settings were set.</summary>
    Unknown,

    /// <summary>It answered the last time it was asked.</summary>
    Up,

    /// <summary>Nothing answers, and kensaku-cli is not to be started (not set, or already started once).</summary>
    Absent,

    /// <summary>kensaku-cli was started and has not answered yet.</summary>
    Starting,

    /// <summary>Starting kensaku-cli failed, or it quit before answering; tried again after <see cref="KensakuLink.RetryAfter"/>.</summary>
    Failed,
}

/// <summary>
/// The panel's link to kensaku for the app's life (issue #87): it asks whether kensaku is up before each content
/// search, and starts kensaku-cli at most once per <see cref="KensakuKey"/>, again only after a failed start and
/// a minute's wait. A process it started is left running when the app quits, as kensaku's own <c>serve</c> is.
/// Used from the UI thread; only file and process work runs off it.
/// </summary>
public sealed class KensakuLink(Func<DateTime> now)
{
    public static readonly TimeSpan RetryAfter = TimeSpan.FromSeconds(60);

    /// <summary>How long a search waits for a kensaku-cli it started; one not ready by then says it is starting.</summary>
    public static readonly TimeSpan StartupPatience = TimeSpan.FromSeconds(5);

    /// <summary>How long a kensaku-cli it started may run without answering before the start counts as failed.</summary>
    public static readonly TimeSpan StartupGrace = TimeSpan.FromSeconds(60);

    public const string StillStarting = "kensaku is starting";

    /// <summary>Said by a search whose settings changed while it waited; the newer search says what is so.</summary>
    private const string SettingsChanged = "the content search settings changed";

    private KensakuKey? _key;
    private string? _dataFolder;
    private bool _started;
    private Process? _process;
    private DateTime _startedAt;
    private DateTime _failedAt;
    private string? _startProblem;

    public KensakuLink() : this(() => DateTime.UtcNow)
    {
    }

    public KensakuState State { get; private set; }

    /// <summary>Files whose contents match <paramref name="line"/>, starting kensaku-cli first when it should be.</summary>
    public async Task<ContentResult> SearchAsync(KensakuKey key, string line, CancellationToken cancel)
    {
        Use(key);
        var kensaku = new Kensaku(key.Port, await DataFolderAsync(key, cancel));
        KensakuReach reach = await kensaku.ProbeAsync(cancel);
        if (key != _key)
        {
            // A newer search, for other settings, ran meanwhile; what was learned here is not about them.
            return ContentResult.Unavailable(SettingsChanged);
        }
        if (reach == KensakuReach.TooSlow)
        {
            return ContentResult.Unavailable(Kensaku.TooSlow);
        }
        if (reach == KensakuReach.Answers)
        {
            State = KensakuState.Up;
            return await kensaku.SearchAsync(line, cancel);
        }
        if (State == KensakuState.Starting)
        {
            // No process yet means it is being started by a search before this one.
            bool running = _process is null || IsRunning(_process);
            if (running && now() - _startedAt < StartupGrace)
            {
                return ContentResult.Unavailable(StillStarting);
            }
            Fail(running ? "kensaku-cli did not answer after it was started" : "kensaku-cli quit before it answered");
        }
        if (!MayStart())
        {
            State = State == KensakuState.Failed ? KensakuState.Failed : KensakuState.Absent;
            return ContentResult.Unavailable(State == KensakuState.Failed ? _startProblem! : Kensaku.NotRunning);
        }
        return await StartAsync(kensaku, key, line, cancel);
    }

    /// <summary>Other settings start over: nothing known, and kensaku-cli may be started for them.</summary>
    private void Use(KensakuKey key)
    {
        if (key == _key)
        {
            return;
        }
        _key = key;
        _dataFolder = null;
        _started = false;
        // Only the handle: the process itself keeps running.
        _process?.Dispose();
        _process = null;
        State = KensakuState.Unknown;
    }

    /// <summary>The data folder, looked up once per settings: the default is found on disk, maybe a slow share, so off the UI thread.</summary>
    private async Task<string> DataFolderAsync(KensakuKey key, CancellationToken cancel)
    {
        if (_dataFolder is not null)
        {
            return _dataFolder;
        }
        string folder = key.Data.Length > 0 ? key.Data : await Task.Run(() => Kensaku.DefaultDataFolder(key.Cli), cancel);
        if (key == _key)
        {
            _dataFolder = folder;
        }
        return folder;
    }

    private bool MayStart()
    {
        if (_key is not { Cli.Length: > 0 })
        {
            return false;
        }
        return !_started || (State == KensakuState.Failed && now() - _failedAt >= RetryAfter);
    }

    private async Task<ContentResult> StartAsync(Kensaku kensaku, KensakuKey key, string line, CancellationToken cancel)
    {
        _started = true;
        _startedAt = now();
        State = KensakuState.Starting;
        (Process? process, string? problem) started;
        try
        {
            // Not cancelled by typing: once a start is asked for, its outcome is recorded.
            started = await Task.Run(() => kensaku.Start(key.Cli));
        }
        catch (Exception error)
        {
            // Anything Start did not expect still ends the start, or the link would say "starting" for good.
            if (key == _key)
            {
                Fail($"kensaku-cli could not be started: {error.Message}");
            }
            return ContentResult.Unavailable($"kensaku-cli could not be started: {error.Message}");
        }
        (Process? process, string? problem) = started;
        if (key != _key)
        {
            // The settings changed while it started; the next search sees what is on the new port.
            process?.Dispose();
            return ContentResult.Unavailable(SettingsChanged);
        }
        if (problem is not null)
        {
            Fail(problem);
            return ContentResult.Unavailable(problem);
        }
        _process = process;
        if (!await kensaku.WaitUntilUpAsync(StartupPatience, cancel))
        {
            return ContentResult.Unavailable(StillStarting);
        }
        State = KensakuState.Up;
        return await kensaku.SearchAsync(line, cancel);
    }

    private void Fail(string problem)
    {
        State = KensakuState.Failed;
        _failedAt = now();
        _startProblem = problem;
        _process?.Dispose();
        _process = null;
    }

    private static bool IsRunning(Process process)
    {
        try
        {
            return !process.HasExited;
        }
        catch (Exception error) when (error is InvalidOperationException or Win32Exception)
        {
            return false;
        }
    }
}
