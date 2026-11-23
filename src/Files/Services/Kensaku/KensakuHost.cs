using System.ComponentModel;
using System.Text.Json;

namespace Files.Services;

/// <summary>What the bundled kensaku is doing.</summary>
public enum KensakuHostState
{
    /// <summary>Content search is off, kensaku is not fetched, or there is no volume to read.</summary>
    Stopped,

    /// <summary>The daemon was started and has not answered or been set up yet.</summary>
    Starting,

    /// <summary>The daemon answers and has the volumes and OCR setting.</summary>
    Up,

    /// <summary>The daemon stopped or never answered; it starts again after a back-off.</summary>
    Restarting,

    /// <summary>kensaku cannot run here, or refuses this app; starting it again would not help.</summary>
    Failed,
}

/// <summary>
/// Runs the bundled kensaku (issue #97): <c>kensaku daemon run</c> for this app's own data folder, set up with the
/// roots of the indexed volumes. A change of volumes or OCR is sent to the running daemon; when the daemon stops it
/// starts again after 5 s, 30 s, then every 2 minutes. When content search is turned off, and on quit, the daemon is
/// asked to shut down and whatever is left after 2 s is ended.
/// </summary>
public sealed class KensakuHost : IDisposable
{
    public const string NotFetched = "kensaku is not fetched · run tools/fetch_kensaku.ps1";

    public static readonly TimeSpan PingPatience = TimeSpan.FromSeconds(10);
    public static readonly TimeSpan ShutdownGrace = TimeSpan.FromSeconds(2);

    /// <summary>How long a daemon must run before a crash counts as a fresh start rather than one more failure.</summary>
    public static readonly TimeSpan StableUptime = TimeSpan.FromSeconds(60);
    public static readonly IReadOnlyList<TimeSpan> DefaultBackOff = [TimeSpan.FromSeconds(5), TimeSpan.FromSeconds(30), TimeSpan.FromMinutes(2)];

    // Volumes load one by one at start-up, each changing what is wanted; only the last one is sent.
    private static readonly TimeSpan DefaultSettle = TimeSpan.FromSeconds(2);
    private static readonly TimeSpan PingInterval = TimeSpan.FromMilliseconds(250);

    private readonly string? _program;
    private readonly IProcessRunner _runner;
    private readonly IReadOnlyList<TimeSpan> _backOff;
    private readonly TimeSpan _settle;
    private readonly object _gate = new();

    // Guarded by _gate: what is wanted, the run in progress (null _stop when stopped), and its wake-up for a change.
    private KensakuWanted? _wanted;
    private CancellationTokenSource? _stop;
    private Task _run = Task.CompletedTask;
    private SemaphoreSlim _changed = new(0);
    private bool _disposed;

    /// <param name="program">kensaku.exe, or null when it is not fetched.</param>
    /// <param name="settle">How long a change waits before it is sent, so a burst of changes is sent once.</param>
    public KensakuHost(string? program, string dataFolder, IProcessRunner runner, IKensakuPipe pipe,
        IReadOnlyList<TimeSpan>? backOff = null, TimeSpan? settle = null)
    {
        _program = program;
        DataFolder = dataFolder;
        _runner = runner;
        Pipe = pipe;
        _backOff = backOff ?? DefaultBackOff;
        _settle = settle ?? DefaultSettle;
    }

    /// <summary>The host the app uses: kensaku.exe found beside it, its own data folder, real processes and the real pipe.</summary>
    public static KensakuHost ForApp(string dataFolder) =>
        new(FindProgram(AppContext.BaseDirectory), dataFolder, new ProcessRunner(), new KensakuPipe(dataFolder));

    /// <summary>kensaku's data folder, which only this app's kensaku uses.</summary>
    public string DataFolder { get; }

    /// <summary>The daemon's methods, for searches; ask only while <see cref="State"/> is Up.</summary>
    public IKensakuPipe Pipe { get; }

    public KensakuHostState State { get; private set; }

    /// <summary>Why kensaku is stopped, restarting or failed, in words; null when nothing is wrong.</summary>
    public string? Problem { get; private set; }

    /// <summary>The state in words, for the status line in Settings; while up, kensaku's own status says more.</summary>
    public string Status => State switch
    {
        KensakuHostState.Starting => "Starting kensaku…",
        KensakuHostState.Up => "Running",
        KensakuHostState.Restarting => $"Restarting · {Problem}",
        _ => Problem ?? "Off",
    };

    /// <summary>Raised on a worker thread when <see cref="State"/> changes.</summary>
    public event Action? Changed;

    /// <summary>
    /// The installed kensaku (<c>kensaku\kensaku.exe</c> beside files.exe), or for a build run from source the nearest
    /// <c>artifacts\kensaku\kensaku.exe</c> above it; null when neither exists.
    /// </summary>
    public static string? FindProgram(string appFolder)
    {
        string beside = Path.Combine(appFolder, "kensaku", "kensaku.exe");
        if (File.Exists(beside))
        {
            return beside;
        }
        for (DirectoryInfo? folder = new(appFolder); folder is not null; folder = folder.Parent)
        {
            string fetched = Path.Combine(folder.FullName, "artifacts", "kensaku", "kensaku.exe");
            if (File.Exists(fetched))
            {
                return fetched;
            }
        }
        return null;
    }

    /// <summary>
    /// Runs kensaku for <paramref name="wanted"/> when <paramref name="enabled"/>, else stops it. A running daemon is
    /// sent what changed; it is not restarted. Called from any thread, as often as volumes change.
    /// </summary>
    public void Apply(bool enabled, KensakuWanted wanted)
    {
        lock (_gate)
        {
            if (_disposed)
            {
                return;
            }
            string? problem = !enabled ? null
                : _program is null ? NotFetched
                : wanted.WatchedDirs.Count == 0 ? KensakuSetup.NoVolumes
                : null;
            if (!enabled || problem is not null)
            {
                _wanted = null;
                StopLocked();
                Set(KensakuHostState.Stopped, problem);
                return;
            }
            bool changed = wanted != _wanted;
            _wanted = wanted;
            if (_stop is not null)
            {
                if (changed)
                {
                    _changed.Release();
                }
                return;
            }
            Task previous = _run;
            var stop = new CancellationTokenSource();
            _stop = stop;
            _changed = new SemaphoreSlim(0);
            _run = Task.Run(async () =>
            {
                await Settled(previous);
                await RunAsync(stop.Token);
            });
        }
    }

    /// <summary>Stops kensaku and waits for it, for at most the shutdown grace and a second.</summary>
    public void Dispose()
    {
        Task run;
        lock (_gate)
        {
            _disposed = true;
            run = StopLocked();
            Set(KensakuHostState.Stopped, null);
        }
        // On quit: a run that went wrong must not keep the app from closing.
        Settled(run).Wait(ShutdownGrace + TimeSpan.FromSeconds(1));
    }

    /// <summary><paramref name="run"/>, finished however it ended: RunAsync handles its own failures, this is the last guard.</summary>
    private static async Task Settled(Task run)
    {
        try
        {
            await run;
        }
        catch (Exception)
        {
            // Nothing is left to stop; the next run starts afresh.
        }
    }

    private Task StopLocked()
    {
        _stop?.Cancel();
        _stop = null;
        return _run;
    }

    private void Set(KensakuHostState state, string? problem)
    {
        if (State == state && Problem == problem)
        {
            return;
        }
        State = state;
        Problem = problem;
        Changed?.Invoke();
    }

    /// <summary>Sets the state for the run that <paramref name="cancel"/> belongs to, unless that run was stopped.</summary>
    private void SetFor(CancellationToken cancel, KensakuHostState state, string? problem = null)
    {
        lock (_gate)
        {
            if (!cancel.IsCancellationRequested)
            {
                Set(state, problem);
            }
        }
    }

    private KensakuWanted? Wanted
    {
        get
        {
            lock (_gate)
            {
                return _wanted;
            }
        }
    }

    private async Task RunAsync(CancellationToken cancel)
    {
        int failures = 0;
        try
        {
            await Task.Delay(_settle, cancel);
            while (true)
            {
                (bool stable, bool failed, string problem) = await RunGuardedAsync(cancel);
                cancel.ThrowIfCancellationRequested();
                if (failed)
                {
                    SetFor(cancel, KensakuHostState.Failed, problem);
                    return;
                }
                // A kensaku that ran a good while starts over from the shortest wait; one that keeps crashing waits longer each time.
                failures = stable ? 1 : failures + 1;
                SetFor(cancel, KensakuHostState.Restarting, problem);
                await Task.Delay(_backOff[Math.Min(failures, _backOff.Count) - 1], cancel);
            }
        }
        catch (Exception) when (cancel.IsCancellationRequested)
        {
            // Stopped: the daemon was shut down on the way out.
        }
    }

    /// <summary>One run; an error nobody foresaw is a restart, never a run that dies with the state left at Up.</summary>
    private async Task<(bool Stable, bool Failed, string Problem)> RunGuardedAsync(CancellationToken cancel)
    {
        try
        {
            return await RunOnceAsync(cancel);
        }
        catch (Exception error) when (!cancel.IsCancellationRequested)
        {
            return (false, false, $"kensaku failed: {error.Message}");
        }
    }

    /// <summary>Starts the daemon, sets it up, sends changes while it runs, and returns once it stops or fails.</summary>
    private async Task<(bool Stable, bool Failed, string Problem)> RunOnceAsync(CancellationToken cancel)
    {
        IKensakuProcess? daemon = null;
        System.Diagnostics.Stopwatch? up = null;
        bool Stable() => up?.Elapsed >= StableUptime;
        try
        {
            SetFor(cancel, KensakuHostState.Starting);
            if (!await ShutDownLeftoverAsync(cancel))
            {
                // Starting another now would find the old one answering and take it for the new.
                return (false, false, "an earlier kensaku did not stop");
            }
            string ort = Path.Combine(Path.GetDirectoryName(_program!)!, "onnxruntime.dll");
            daemon = _runner.Start(_program!, ["daemon", "run", "--data-dir", DataFolder], new Dictionary<string, string> { ["ORT_DYLIB_PATH"] = ort });
            if (!await WaitForPingAsync(daemon, cancel))
            {
                return (false, false, "kensaku did not answer");
            }
            KensakuWanted? applied = await SendAsync(null, cancel);
            SetFor(cancel, KensakuHostState.Up);
            up = System.Diagnostics.Stopwatch.StartNew();
            while (await ChangedOrExitedAsync(daemon, cancel))
            {
                await Task.Delay(_settle, cancel);
                applied = await SendAsync(applied, cancel);
            }
            return (Stable(), false, "kensaku stopped");
        }
        catch (KensakuException error) when (error.Code == KensakuError.Forbidden)
        {
            return (false, true, $"kensaku refused this app: {error.Message}");
        }
        catch (IncompatibleException error)
        {
            return (false, true, error.Message);
        }
        catch (Exception error) when (error is KensakuException or TimeoutException)
        {
            // Stopping ends the run in RunAsync; otherwise the daemon starts again.
            return (Stable(), false, error.Message);
        }
        catch (Exception error) when (error is Win32Exception or InvalidOperationException)
        {
            return (false, true, $"kensaku could not be started: {error.Message}");
        }
        finally
        {
            await EndAsync(daemon);
        }
    }

    /// <summary>
    /// Asks a daemon left by an earlier run of the app (one that crashed, say) to shut down, so a new one can take the
    /// data folder. False when it still answers after <see cref="ShutdownGrace"/>.
    /// </summary>
    private async Task<bool> ShutDownLeftoverAsync(CancellationToken cancel)
    {
        if (!await AnswersAsync(cancel))
        {
            return true;
        }
        await Pipe.CallAsync("shutdown", null, cancel);
        DateTime deadline = DateTime.UtcNow + ShutdownGrace;
        while (await AnswersAsync(cancel))
        {
            if (DateTime.UtcNow >= deadline)
            {
                return false;
            }
            await Task.Delay(PingInterval, cancel);
        }
        return true;
    }

    /// <summary>
    /// Whether a daemon answers a ping. A refused token is thrown, and so is a daemon that does not take the ping as
    /// this app sends it (another protocol), since starting again would meet the same.
    /// </summary>
    private async Task<bool> AnswersAsync(CancellationToken cancel)
    {
        try
        {
            await Pipe.CallAsync("ping", null, cancel);
            return true;
        }
        catch (KensakuException error) when (error.Code is KensakuError.InvalidParams or KensakuError.MethodNotFound)
        {
            throw new IncompatibleException($"this kensaku does not speak the protocol files uses: {error.Message}");
        }
        catch (Exception error) when (error is TimeoutException || error is KensakuException { Code: not KensakuError.Forbidden })
        {
            return false;
        }
    }

    /// <summary>A daemon of a version this app cannot talk to.</summary>
    private sealed class IncompatibleException(string message) : Exception(message);

    /// <summary>Pings the daemon until it answers, it exits, or <see cref="PingPatience"/> passes.</summary>
    private async Task<bool> WaitForPingAsync(IKensakuProcess daemon, CancellationToken cancel)
    {
        DateTime deadline = DateTime.UtcNow + PingPatience;
        while (!await AnswersAsync(cancel))
        {
            if (daemon.HasExited || DateTime.UtcNow >= deadline)
            {
                return false;
            }
            await Task.Delay(PingInterval, cancel);
        }
        return true;
    }

    /// <summary>Sends what changed since <paramref name="applied"/> and returns what the daemon now has.</summary>
    private async Task<KensakuWanted?> SendAsync(KensakuWanted? applied, CancellationToken cancel)
    {
        if (Wanted is not KensakuWanted wanted)
        {
            return applied;
        }
        JsonElement health = await Pipe.CallAsync("health", null, cancel);
        bool setupComplete = health.ValueKind == JsonValueKind.Object
            && health.TryGetProperty("setup_complete", out JsonElement complete) && complete.ValueKind == JsonValueKind.True;
        foreach (KensakuCall call in KensakuSetup.Calls(setupComplete, applied, wanted))
        {
            try
            {
                await Pipe.CallAsync(call.Method, call.Parameters, cancel);
            }
            catch (KensakuException error) when (call.Method == "pack_download" && error.Code is KensakuError.Busy or KensakuError.Internal)
            {
                // A download already running; kensaku reports a failed one in its own status.
            }
        }
        return wanted;
    }

    /// <summary>Waits for a change to send (true) or the daemon to exit (false).</summary>
    private async Task<bool> ChangedOrExitedAsync(IKensakuProcess daemon, CancellationToken cancel)
    {
        SemaphoreSlim changed;
        lock (_gate)
        {
            changed = _changed;
        }
        // Its own token, so a wait left over when the daemon exits does not take the next change.
        using var waiting = CancellationTokenSource.CreateLinkedTokenSource(cancel);
        Task exited = daemon.WaitForExitAsync(waiting.Token);
        Task<bool> wake = changed.WaitAsync(Timeout.Infinite, waiting.Token);
        Task first = await Task.WhenAny(exited, wake);
        waiting.Cancel();
        cancel.ThrowIfCancellationRequested();
        return first == wake && !daemon.HasExited;
    }

    /// <summary>Asks the daemon to shut down, gives it <see cref="ShutdownGrace"/>, then ends it.</summary>
    private async Task EndAsync(IKensakuProcess? daemon)
    {
        if (daemon is null)
        {
            return;
        }
        if (!daemon.HasExited)
        {
            using var grace = new CancellationTokenSource(ShutdownGrace);
            try
            {
                await Pipe.CallAsync("shutdown", null, grace.Token);
                await daemon.WaitForExitAsync(grace.Token);
            }
            catch (Exception error) when (error is OperationCanceledException or KensakuException or TimeoutException)
            {
                // Not gone in time: ended below.
            }
        }
        daemon.Kill();
        daemon.Dispose();
    }
}
