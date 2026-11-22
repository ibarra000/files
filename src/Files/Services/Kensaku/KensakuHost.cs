using System.ComponentModel;
using Files.Core;

namespace Files.Services;

/// <summary>What the bundled kensaku is doing.</summary>
public enum KensakuHostState
{
    /// <summary>Content search is off, or kensaku is not built.</summary>
    Stopped,

    /// <summary>kensaku-daemon was started and has not answered on its pipe yet.</summary>
    StartingDaemon,

    /// <summary>The daemon answers; <c>kensaku-cli serve</c> was started and has not answered <c>/health</c> yet.</summary>
    StartingServe,

    /// <summary>Both run and <c>serve</c> answers.</summary>
    Up,

    /// <summary>One of them stopped or never answered; both start again after a back-off.</summary>
    Restarting,

    /// <summary>Another kensaku's daemon holds the pipe, so nothing is started.</summary>
    Blocked,
}

/// <summary>The bundle, port and config the host runs kensaku for; another key is another kensaku to the panel.</summary>
public sealed record KensakuKey(string Bundle, int Port, string Config);

/// <summary>
/// Runs the bundled kensaku (issue #97): kensaku-daemon indexes the volumes' roots, and <c>kensaku-cli serve</c> answers
/// searches on <see cref="Port"/>. <c>serve</c> decides once, on its first request, whether to use the daemon, so it is
/// only started once the daemon answers on its pipe; and when either stops, both start again, after 5 s, 30 s, then
/// every 2 minutes. On quit the daemon is asked to shut down, then whatever is left is ended.
/// </summary>
public sealed class KensakuHost : IDisposable
{
    /// <summary>One above kensaku's own 8765, so a <c>serve</c> the user runs is left alone.</summary>
    public const int Port = 8766;

    public const string NotBuilt = "kensaku is not built · run tools/build_kensaku.ps1";
    public const string AnotherKensaku = "Another kensaku is running";

    public static readonly TimeSpan PingPatience = TimeSpan.FromSeconds(10);
    public static readonly TimeSpan ShutdownGrace = TimeSpan.FromSeconds(2);
    public static readonly IReadOnlyList<TimeSpan> DefaultBackOff = [TimeSpan.FromSeconds(5), TimeSpan.FromSeconds(30), TimeSpan.FromMinutes(2)];

    // Volumes load one by one at start-up, each changing the config; only the last one is run.
    private static readonly TimeSpan DefaultSettle = TimeSpan.FromSeconds(2);
    private static readonly TimeSpan PingInterval = TimeSpan.FromMilliseconds(250);
    private static readonly TimeSpan ServePatience = TimeSpan.FromSeconds(30);

    private readonly string? _bundle;
    private readonly IProcessRunner _runner;
    private readonly IKensakuPipe _pipe;
    private readonly Func<CancellationToken, Task<bool>> _serveAnswers;
    private readonly IReadOnlyList<TimeSpan> _backOff;
    private readonly TimeSpan _settle;
    private readonly object _gate = new();

    // What runs now, guarded by _gate: its key (null when stopped), and how to stop it.
    private KensakuKey? _key;
    private CancellationTokenSource? _stop;
    private Task _run = Task.CompletedTask;
    private bool _disposed;

    /// <param name="bundle">The folder holding kensaku-daemon.exe and kensaku-cli.exe, or null when it is not built.</param>
    /// <param name="serveAnswers">Waits until <c>serve</c> answers <c>/health</c>; false when it never did.</param>
    /// <param name="settle">How long a new config waits before it is run, so a burst of changes starts kensaku once.</param>
    public KensakuHost(string? bundle, string dataFolder, IProcessRunner runner, IKensakuPipe pipe,
        Func<CancellationToken, Task<bool>> serveAnswers, IReadOnlyList<TimeSpan>? backOff = null, TimeSpan? settle = null)
    {
        _bundle = bundle;
        DataFolder = dataFolder;
        _runner = runner;
        _pipe = pipe;
        _serveAnswers = serveAnswers;
        _backOff = backOff ?? DefaultBackOff;
        _settle = settle ?? DefaultSettle;
    }

    /// <summary>The host the app uses: the bundle found beside it, its own data folder, real processes and the real pipe.</summary>
    public static KensakuHost ForApp(string dataFolder)
    {
        var kensaku = new Kensaku(Port, dataFolder);
        return new KensakuHost(FindBundle(AppContext.BaseDirectory), dataFolder, new ProcessRunner(), new KensakuPipe(dataFolder),
            cancel => kensaku.WaitUntilUpAsync(ServePatience, cancel));
    }

    /// <summary>kensaku's data folder, which only this app's kensaku uses.</summary>
    public string DataFolder { get; }

    public KensakuHostState State { get; private set; }

    /// <summary>Why kensaku is stopped, blocked or restarting, in words; null when nothing is wrong.</summary>
    public string? Problem { get; private set; }

    /// <summary>The state in words, for the status line in Settings; while up, kensaku's own health says more.</summary>
    public string Status => State switch
    {
        KensakuHostState.StartingDaemon or KensakuHostState.StartingServe => "Starting kensaku…",
        KensakuHostState.Up => "Running",
        KensakuHostState.Restarting => $"Restarting · {Problem}",
        _ => Problem ?? "Off",
    };

    /// <summary>What it runs for, or null when stopped.</summary>
    public KensakuKey? Key
    {
        get
        {
            lock (_gate)
            {
                return _key;
            }
        }
    }

    /// <summary>Raised on a worker thread when <see cref="State"/> changes.</summary>
    public event Action? Changed;

    /// <summary>
    /// The installed bundle (<c>kensaku</c> beside files.exe), or for a build run from source the nearest
    /// <c>artifacts\kensaku</c> above it; null when neither holds kensaku.
    /// </summary>
    public static string? FindBundle(string appFolder)
    {
        string beside = Path.Combine(appFolder, "kensaku");
        if (IsBundle(beside))
        {
            return beside;
        }
        for (DirectoryInfo? folder = new(appFolder); folder is not null; folder = folder.Parent)
        {
            string built = Path.Combine(folder.FullName, "artifacts", "kensaku");
            if (IsBundle(built))
            {
                return built;
            }
        }
        return null;
    }

    private static bool IsBundle(string folder) =>
        File.Exists(Path.Combine(folder, "kensaku-daemon.exe")) && File.Exists(Path.Combine(folder, "kensaku-cli.exe"));

    /// <summary>
    /// Runs kensaku with <paramref name="config"/> when <paramref name="enabled"/>, else stops it. The same config again
    /// changes nothing; another one restarts it. Called from any thread, as often as volumes change.
    /// </summary>
    public void Apply(bool enabled, string config)
    {
        lock (_gate)
        {
            KensakuKey? key = enabled && _bundle is not null ? new KensakuKey(_bundle, Port, KensakuConfig.Hash(config)) : null;
            if (_disposed || key is not null && key == _key)
            {
                // Quit, or running, starting or blocked for this config already.
                return;
            }
            Task previous = StopLocked();
            _key = key;
            Set(KensakuHostState.Stopped, enabled && _bundle is null ? NotBuilt : null);
            if (key is null)
            {
                return;
            }
            var stop = new CancellationTokenSource();
            _stop = stop;
            _run = Task.Run(async () =>
            {
                await previous;
                await RunAsync(config, stop.Token);
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
            _key = null;
            run = StopLocked();
            Set(KensakuHostState.Stopped, null);
        }
        run.Wait(ShutdownGrace + TimeSpan.FromSeconds(1));
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

    private async Task RunAsync(string config, CancellationToken cancel)
    {
        int failures = 0;
        try
        {
            await Task.Delay(_settle, cancel);
            WriteConfig(config);
            while (!cancel.IsCancellationRequested)
            {
                (bool blocked, bool wasUp, string problem) = await RunOnceAsync(cancel);
                if (blocked)
                {
                    SetFor(cancel, KensakuHostState.Blocked, AnotherKensaku);
                    return;
                }
                // A kensaku that came up and later stopped starts over from the shortest wait.
                failures = wasUp ? 1 : failures + 1;
                SetFor(cancel, KensakuHostState.Restarting, problem);
                await Task.Delay(_backOff[Math.Min(failures, _backOff.Count) - 1], cancel);
            }
        }
        catch (OperationCanceledException) when (cancel.IsCancellationRequested)
        {
            // Stopped: the processes were ended on the way out.
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            SetFor(cancel, KensakuHostState.Stopped, $"kensaku's config could not be written: {error.Message}");
        }
    }

    /// <summary>Writes config.json, unless what this app wrote last is the same: kensaku rewrites the file as it indexes.</summary>
    private void WriteConfig(string config)
    {
        string written = Path.Combine(DataFolder, "config.files-hash");
        string hash = KensakuConfig.Hash(config);
        if (File.Exists(written) && File.ReadAllText(written).Trim() == hash)
        {
            return;
        }
        AtomicFile.WriteAllText(Path.Combine(DataFolder, "config.json"), config);
        AtomicFile.WriteAllText(written, hash);
    }

    /// <summary>Starts the daemon, then <c>serve</c>, and returns once either stops, or one never answers.</summary>
    private async Task<(bool Blocked, bool WasUp, string Problem)> RunOnceAsync(CancellationToken cancel)
    {
        IKensakuProcess? daemon = null;
        IKensakuProcess? serve = null;
        try
        {
            _runner.EndLeftovers(_bundle!);
            if (await _pipe.PingAsync(cancel) == PipeAnswer.Other)
            {
                return (true, false, AnotherKensaku);
            }
            SetFor(cancel, KensakuHostState.StartingDaemon);
            daemon = _runner.Start(Path.Combine(_bundle!, "kensaku-daemon.exe"), [], DataFolder);
            PipeAnswer answer = await WaitForPipeAsync(daemon, cancel);
            if (answer != PipeAnswer.Ours)
            {
                return (answer == PipeAnswer.Other, false, "kensaku-daemon did not answer");
            }
            SetFor(cancel, KensakuHostState.StartingServe);
            serve = _runner.Start(Path.Combine(_bundle!, "kensaku-cli.exe"), ["serve", "--port", Port.ToString(System.Globalization.CultureInfo.InvariantCulture)], DataFolder);
            // A serve that quits at once (its port taken, say) fails now rather than after the whole wait.
            Task<bool> answers = _serveAnswers(cancel);
            if (await Task.WhenAny(answers, serve.WaitForExitAsync(cancel)) != answers || !await answers || serve.HasExited)
            {
                cancel.ThrowIfCancellationRequested();
                return (false, false, "kensaku-cli serve did not answer");
            }
            SetFor(cancel, KensakuHostState.Up);
            await Task.WhenAny(daemon.WaitForExitAsync(cancel), serve.WaitForExitAsync(cancel));
            cancel.ThrowIfCancellationRequested();
            return (false, true, daemon.HasExited ? "kensaku-daemon stopped" : "kensaku-cli serve stopped");
        }
        catch (Exception error) when (error is Win32Exception or InvalidOperationException or IOException)
        {
            return (false, false, $"kensaku could not be started: {error.Message}");
        }
        finally
        {
            await EndAsync(daemon, serve);
        }
    }

    /// <summary>Pings the daemon until it answers, it exits, or <see cref="PingPatience"/> passes.</summary>
    private async Task<PipeAnswer> WaitForPipeAsync(IKensakuProcess daemon, CancellationToken cancel)
    {
        DateTime deadline = DateTime.UtcNow + PingPatience;
        while (true)
        {
            PipeAnswer answer = await _pipe.PingAsync(cancel);
            if (answer != PipeAnswer.Nobody || daemon.HasExited || DateTime.UtcNow >= deadline)
            {
                return answer;
            }
            await Task.Delay(PingInterval, cancel);
        }
    }

    /// <summary>Asks the daemon to shut down, gives it <see cref="ShutdownGrace"/>, then ends both. <c>serve</c> has no shutdown of its own.</summary>
    private async Task EndAsync(IKensakuProcess? daemon, IKensakuProcess? serve)
    {
        if (daemon is { HasExited: false })
        {
            using var grace = new CancellationTokenSource(ShutdownGrace);
            try
            {
                await _pipe.ShutdownAsync(grace.Token);
                await daemon.WaitForExitAsync(grace.Token);
            }
            catch (OperationCanceledException)
            {
                // Not gone in time: ended below.
            }
        }
        foreach (IKensakuProcess? process in new[] { serve, daemon })
        {
            process?.Kill();
            process?.Dispose();
        }
    }
}
