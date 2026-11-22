using Files.Services;

namespace Files.Tests;

public sealed class KensakuHostTests
{
    private const string Config = """{ "watched_dirs": [] }""";

    /// <summary>Everything the fakes saw, in order: "start kensaku-daemon", "ping → Ours", "shutdown", "kill kensaku-cli".</summary>
    private sealed class Log
    {
        private readonly List<string> _lines = [];

        public void Add(string line)
        {
            lock (_lines)
            {
                _lines.Add(line);
            }
        }

        public List<string> Lines
        {
            get
            {
                lock (_lines)
                {
                    return [.. _lines];
                }
            }
        }
    }

    private sealed class FakeProcess(string name, Log log) : IKensakuProcess
    {
        private readonly TaskCompletionSource _exited = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public string Name => name;

        public bool HasExited => _exited.Task.IsCompleted;

        public Task WaitForExitAsync(CancellationToken cancel) => _exited.Task.WaitAsync(cancel);

        public void Exit() => _exited.TrySetResult();

        public void Kill()
        {
            if (!HasExited)
            {
                log.Add($"kill {name}");
            }
            Exit();
        }

        public void Dispose()
        {
        }
    }

    private sealed class FakeRunner(Log log) : IProcessRunner
    {
        private readonly List<FakeProcess> _started = [];

        public List<FakeProcess> Started
        {
            get
            {
                lock (_started)
                {
                    return [.. _started];
                }
            }
        }

        public IKensakuProcess Start(string program, IReadOnlyList<string> arguments, string dataFolder)
        {
            string name = Path.GetFileNameWithoutExtension(program);
            log.Add($"start {name}");
            var process = new FakeProcess(name, log);
            lock (_started)
            {
                _started.Add(process);
            }
            return process;
        }

        public void EndLeftovers(string bundle)
        {
        }
    }

    /// <summary>A daemon's pipe that answers once the newest daemon has been pinged <paramref name="pingsBeforeAnswer"/> times.</summary>
    private sealed class FakePipe(Log log, FakeRunner runner, int pingsBeforeAnswer = 0, PipeAnswer answer = PipeAnswer.Ours) : IKensakuPipe
    {
        private int _pings;
        private FakeProcess? _pinged;

        public Task<PipeAnswer> PingAsync(CancellationToken cancel)
        {
            FakeProcess? daemon = runner.Started.LastOrDefault(process => process.Name == "kensaku-daemon" && !process.HasExited);
            PipeAnswer reply = answer == PipeAnswer.Other ? PipeAnswer.Other : PipeAnswer.Nobody;
            if (daemon is not null)
            {
                _pings = daemon == _pinged ? _pings + 1 : 1;
                _pinged = daemon;
                reply = _pings > pingsBeforeAnswer ? answer : PipeAnswer.Nobody;
            }
            log.Add($"ping → {reply}");
            return Task.FromResult(reply);
        }

        public Task ShutdownAsync(CancellationToken cancel)
        {
            log.Add("shutdown");
            runner.Started.LastOrDefault(process => process.Name == "kensaku-daemon")?.Exit();
            return Task.CompletedTask;
        }
    }

    private static KensakuHost Host(TempFolder temp, FakeRunner runner, IKensakuPipe pipe, string? bundle = "bundle") =>
        new(bundle, temp.Path, runner, pipe, _ => Task.FromResult(true), backOff: [TimeSpan.Zero], settle: TimeSpan.Zero);

    private static async Task WaitFor(Func<bool> condition)
    {
        var clock = System.Diagnostics.Stopwatch.StartNew();
        while (!condition())
        {
            Assert.True(clock.Elapsed < TimeSpan.FromSeconds(5), "timed out");
            await Task.Delay(10);
        }
    }

    [Fact]
    public async Task Serve_starts_only_after_the_daemon_answers_its_pipe()
    {
        using var temp = new TempFolder();
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(temp, runner, new FakePipe(log, runner, pingsBeforeAnswer: 2));

        host.Apply(true, Config);
        await WaitFor(() => host.State == KensakuHostState.Up);

        Assert.Equal(
            ["ping → Nobody", "start kensaku-daemon", "ping → Nobody", "ping → Nobody", "ping → Ours", "start kensaku-cli"],
            log.Lines);
        Assert.Equal(Config, File.ReadAllText(Path.Combine(temp.Path, "config.json")));
    }

    [Fact]
    public async Task A_daemon_that_stops_restarts_both()
    {
        using var temp = new TempFolder();
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(temp, runner, new FakePipe(log, runner));
        host.Apply(true, Config);
        await WaitFor(() => host.State == KensakuHostState.Up);

        runner.Started[0].Exit();
        await WaitFor(() => runner.Started.Count == 4 && host.State == KensakuHostState.Up);

        Assert.Equal(["kensaku-daemon", "kensaku-cli", "kensaku-daemon", "kensaku-cli"], runner.Started.Select(process => process.Name));
        // serve asked the old daemon once and would never ask the new one, so it went too.
        Assert.Contains("kill kensaku-cli", log.Lines);
        Assert.True(runner.Started[1].HasExited);
    }

    [Fact]
    public async Task Another_kensaku_on_the_pipe_blocks_the_start()
    {
        using var temp = new TempFolder();
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(temp, runner, new FakePipe(log, runner, answer: PipeAnswer.Other));

        host.Apply(true, Config);
        await WaitFor(() => host.State == KensakuHostState.Blocked);

        Assert.Empty(runner.Started);
        Assert.Equal(KensakuHost.AnotherKensaku, host.Problem);
    }

    [Fact]
    public async Task Turning_content_search_off_shuts_the_daemon_down_and_ends_serve()
    {
        using var temp = new TempFolder();
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(temp, runner, new FakePipe(log, runner));
        host.Apply(true, Config);
        await WaitFor(() => host.State == KensakuHostState.Up);

        host.Apply(false, Config);
        await WaitFor(() => runner.Started.All(process => process.HasExited));

        Assert.Equal(KensakuHostState.Stopped, host.State);
        Assert.Equal(["shutdown", "kill kensaku-cli"], log.Lines.SkipWhile(line => line != "shutdown"));
    }

    [Fact]
    public async Task The_same_config_changes_nothing_and_another_restarts_kensaku()
    {
        using var temp = new TempFolder();
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(temp, runner, new FakePipe(log, runner));
        host.Apply(true, Config);
        await WaitFor(() => host.State == KensakuHostState.Up);

        host.Apply(true, Config);
        Assert.Equal(2, runner.Started.Count);

        string other = """{ "watched_dirs": ["C:\\docs"] }""";
        host.Apply(true, other);
        await WaitFor(() => runner.Started.Count == 4 && host.State == KensakuHostState.Up);
        Assert.Equal(other, File.ReadAllText(Path.Combine(temp.Path, "config.json")));
    }

    [Fact]
    public async Task A_serve_that_quits_at_once_is_a_failed_start_without_the_whole_wait()
    {
        using var temp = new TempFolder();
        var log = new Log();
        var runner = new FakeRunner(log);
        // /health never answers, as when another program holds the port and serve quit.
        using var host = new KensakuHost("bundle", temp.Path, runner, new FakePipe(log, runner),
            cancel => Task.Delay(Timeout.Infinite, cancel).ContinueWith(_ => false, TaskScheduler.Default),
            backOff: [TimeSpan.FromMinutes(1)], settle: TimeSpan.Zero);
        host.Apply(true, Config);
        await WaitFor(() => host.State == KensakuHostState.StartingServe);

        runner.Started[1].Exit();
        await WaitFor(() => host.State == KensakuHostState.Restarting);

        Assert.Equal("kensaku-cli serve did not answer", host.Problem);
        Assert.True(runner.Started[0].HasExited);
    }

    [Fact]
    public void Without_a_built_kensaku_content_search_says_how_to_build_it()
    {
        using var temp = new TempFolder();
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(temp, runner, new FakePipe(log, runner), bundle: null);

        host.Apply(true, Config);

        Assert.Equal((KensakuHostState.Stopped, KensakuHost.NotBuilt), (host.State, host.Problem));
        Assert.Empty(runner.Started);
    }

    [Fact]
    public async Task The_panel_says_kensaku_is_starting_or_blocked_rather_than_search()
    {
        using var temp = new TempFolder();
        var log = new Log();
        var runner = new FakeRunner(log);
        // The daemon never answers, so the host stays starting it.
        using KensakuHost starting = Host(temp, runner, new FakePipe(log, runner, pingsBeforeAnswer: int.MaxValue));
        starting.Apply(true, Config);
        await WaitFor(() => starting.State == KensakuHostState.StartingDaemon);
        var link = new KensakuLink(starting);

        Assert.Equal(KensakuLink.StillStarting, (await link.SearchAsync("x", CancellationToken.None)).Problem);
        Assert.Equal(KensakuState.Starting, link.State);

        using KensakuHost blocked = Host(temp, runner, new FakePipe(log, runner, answer: PipeAnswer.Other));
        blocked.Apply(true, Config);
        await WaitFor(() => blocked.State == KensakuHostState.Blocked);
        Assert.Equal(KensakuHost.AnotherKensaku, (await new KensakuLink(blocked).SearchAsync("x", CancellationToken.None)).Problem);
    }

    [Fact]
    public void The_bundle_is_found_beside_the_app_then_in_artifacts_above_it()
    {
        using var temp = new TempFolder();
        string app = Directory.CreateDirectory(Path.Combine(temp.Path, "src", "bin")).FullName;
        Assert.Null(KensakuHost.FindBundle(app));

        temp.File(@"artifacts\kensaku\kensaku-daemon.exe");
        temp.File(@"artifacts\kensaku\kensaku-cli.exe");
        Assert.Equal(Path.Combine(temp.Path, "artifacts", "kensaku"), KensakuHost.FindBundle(app));

        temp.File(@"src\bin\kensaku\kensaku-daemon.exe");
        temp.File(@"src\bin\kensaku\kensaku-cli.exe");
        Assert.Equal(Path.Combine(app, "kensaku"), KensakuHost.FindBundle(app));
    }
}
