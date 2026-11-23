using System.Text.Json;
using System.Text.Json.Nodes;
using Files.Services;

namespace Files.Tests;

public sealed class KensakuHostTests
{
    private const string Program = @"C:\app\kensaku\kensaku.exe";
    private static readonly KensakuWanted Docs = new([@"C:\docs"], Ocr: false);

    /// <summary>Everything the fakes saw, in order: "start", "ping → none", "setup ["C:\\docs"]", "shutdown", "kill".</summary>
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

    private sealed class FakeProcess(Log log, IReadOnlyList<string> arguments, IReadOnlyDictionary<string, string> environment) : IKensakuProcess
    {
        private readonly TaskCompletionSource _exited = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public IReadOnlyList<string> Arguments => arguments;

        public IReadOnlyDictionary<string, string> Environment => environment;

        public bool HasExited => _exited.Task.IsCompleted;

        public Task WaitForExitAsync(CancellationToken cancel) => _exited.Task.WaitAsync(cancel);

        public void Exit() => _exited.TrySetResult();

        public void Kill()
        {
            if (!HasExited)
            {
                log.Add("kill");
            }
            Exit();
        }

        public void Dispose()
        {
        }
    }

    /// <param name="exitAtOnce">Daemons started after the first this many exit at once, as one that cannot take the data folder.</param>
    private sealed class FakeRunner(Log log, int exitAtOnceAfter = int.MaxValue) : IProcessRunner
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

        public FakeProcess? Running => Started.LastOrDefault(process => !process.HasExited);

        /// <summary>Thrown by the next start, then cleared.</summary>
        public Exception? FailNextStart { get; set; }

        public IKensakuProcess Start(string program, IReadOnlyList<string> arguments, IReadOnlyDictionary<string, string> environment)
        {
            log.Add("start");
            if (FailNextStart is Exception failure)
            {
                FailNextStart = null;
                throw failure;
            }
            var process = new FakeProcess(log, arguments, environment);
            lock (_started)
            {
                _started.Add(process);
                if (_started.Count > exitAtOnceAfter)
                {
                    process.Exit();
                }
            }
            return process;
        }
    }

    /// <summary>
    /// The running fake daemon's methods: it answers pings after <c>pingsBeforeAnswer</c> of them, remembers whether it
    /// was set up, and exits on shutdown. <c>answer</c> gives any other method's result.
    /// </summary>
    private sealed class FakePipe(Log log, FakeRunner runner) : IKensakuPipe
    {
        public int PingsBeforeAnswer { get; init; }

        public bool SetupComplete { get; set; }

        /// <summary>A daemon an earlier run of the app left, answering until it is shut down.</summary>
        public bool Leftover { get; set; }

        /// <summary>A leftover that ignores shutdown.</summary>
        public bool LeftoverStays { get; init; }

        public KensakuError? Refuse { get; init; }

        public Func<string, JsonObject?, string>? Answer { get; init; }

        private int _pings;

        public Task<JsonElement> CallAsync(string method, JsonObject? parameters, CancellationToken cancel)
        {
            if (Refuse is KensakuError refused)
            {
                throw new KensakuException(refused, "bad token");
            }
            if (!Leftover && runner.Running is null)
            {
                Note(method, " → none");
                throw new KensakuException(KensakuError.DaemonUnavailable, "kensaku is not running");
            }
            if (method == "ping" && !Leftover && ++_pings <= PingsBeforeAnswer)
            {
                Note(method, " → none");
                throw new KensakuException(KensakuError.DaemonUnavailable, "kensaku is not running");
            }
            Note(method, method is "setup" or "patch_config" or "pack_download" ? " " + parameters!.ToJsonString() : "");
            string result = method switch
            {
                "health" => $$"""{"setup_complete":{{(SetupComplete ? "true" : "false")}}}""",
                "shutdown" => Shutdown(),
                "setup" => Setup(),
                _ => Answer?.Invoke(method, parameters) ?? "{}",
            };
            return Task.FromResult(JsonDocument.Parse(result).RootElement.Clone());
        }

        private void Note(string method, string detail) => log.Add(method + detail);

        private string Setup()
        {
            SetupComplete = true;
            return "{}";
        }

        private string Shutdown()
        {
            Leftover = LeftoverStays;
            runner.Running?.Exit();
            return "{}";
        }
    }

    private static KensakuHost Host(FakeRunner runner, IKensakuPipe pipe, string? program = Program, IReadOnlyList<TimeSpan>? backOff = null) =>
        new(program, @"C:\data\kensaku", runner, pipe, backOff ?? [TimeSpan.Zero], settle: TimeSpan.Zero);

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
    public async Task A_new_daemon_is_started_for_the_data_folder_pinged_until_it_answers_then_set_up()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(runner, new FakePipe(log, runner) { PingsBeforeAnswer = 2 });

        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Up);

        Assert.Equal(
            [
                "ping → none", "start", "ping → none", "ping → none", "ping", "health",
                """setup {"watched_dirs":["C:\\docs"]}""",
                """patch_config {"patch":{"ocr_enabled":false}}""",
            ],
            log.Lines);
        FakeProcess daemon = runner.Started.Single();
        Assert.Equal(["daemon", "run", "--data-dir", @"C:\data\kensaku"], daemon.Arguments);
        Assert.Equal(@"C:\app\kensaku\onnxruntime.dll", daemon.Environment["ORT_DYLIB_PATH"]);
    }

    [Fact]
    public async Task A_daemon_set_up_before_is_sent_the_volumes_and_a_change_goes_to_the_running_daemon()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(runner, new FakePipe(log, runner) { SetupComplete = true });
        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Up);
        Assert.Contains("""patch_config {"patch":{"watched_dirs":["C:\\docs"],"ocr_enabled":false}}""", log.Lines);

        host.Apply(true, Docs);
        host.Apply(true, new KensakuWanted([@"C:\docs", @"\\server\share"], Ocr: false));
        await WaitFor(() => log.Lines.Count(line => line.StartsWith("patch_config", StringComparison.Ordinal)) == 2);

        Assert.Equal("""patch_config {"patch":{"watched_dirs":["C:\\docs","\\\\server\\share"]}}""", log.Lines[^1]);
        Assert.Single(runner.Started);
    }

    [Fact]
    public async Task A_daemon_that_stops_starts_again_and_a_restart_that_fails_waits_longer()
    {
        var log = new Log();
        // The second daemon exits at once, as one that could not start.
        var runner = new FakeRunner(log, exitAtOnceAfter: 1);
        using KensakuHost host = Host(runner, new FakePipe(log, runner), backOff: [TimeSpan.Zero, TimeSpan.FromMinutes(1)]);
        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Up);

        runner.Started[0].Exit();
        await WaitFor(() => runner.Started.Count == 2 && host.State == KensakuHostState.Restarting);

        // Up once, so the first restart came after the shortest wait; the failed one waits a minute.
        await Task.Delay(100);
        Assert.Equal(2, runner.Started.Count);
        Assert.Equal("Restarting · kensaku did not answer", host.Status);
    }

    [Fact]
    public async Task Turning_content_search_off_shuts_the_daemon_down()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(runner, new FakePipe(log, runner));
        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Up);

        host.Apply(false, Docs);
        await WaitFor(() => runner.Started.All(process => process.HasExited));

        Assert.Equal(KensakuHostState.Stopped, host.State);
        Assert.Equal("shutdown", log.Lines[^1]);
    }

    [Fact]
    public async Task A_daemon_left_by_an_earlier_run_is_shut_down_before_a_new_one_starts()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(runner, new FakePipe(log, runner) { Leftover = true });

        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Up);

        Assert.Equal(["ping", "shutdown", "ping → none", "start"], log.Lines.Take(4));
    }

    [Fact]
    public async Task A_daemon_that_refuses_the_token_fails_without_starting_again()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(runner, new FakePipe(log, runner) { Refuse = KensakuError.Forbidden });

        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Failed);

        Assert.Empty(runner.Started);
        Assert.Equal("kensaku refused this app: bad token", host.Problem);
    }

    [Fact]
    public void Without_kensaku_or_an_indexed_volume_nothing_starts_and_the_status_says_why()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost unfetched = Host(runner, new FakePipe(log, runner), program: null);
        unfetched.Apply(true, Docs);
        Assert.Equal((KensakuHostState.Stopped, KensakuHost.NotFetched), (unfetched.State, unfetched.Problem));

        using KensakuHost empty = Host(runner, new FakePipe(log, runner));
        empty.Apply(true, new KensakuWanted([], Ocr: false));
        Assert.Equal("No indexed volumes to read inside", empty.Status);
        Assert.Empty(runner.Started);
    }

    [Fact]
    public async Task The_panel_says_kensaku_is_starting_and_searches_once_it_is_up()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        string answer = """{"results":[{"path":"C:\\docs\\report.pdf","score":2,"snippet":"torque","match_type":"Content","is_dir":false}],"total":1}""";
        using KensakuHost host = Host(runner, new FakePipe(log, runner)
        {
            PingsBeforeAnswer = int.MaxValue,
            Answer = (method, _) => method == "search" ? answer : "{}",
        });
        var query = new KensakuQuery("torque", new HashSet<string>());
        Assert.Equal(Kensaku.NotRunning, (await new Kensaku(host).SearchAsync(query, CancellationToken.None)).Problem);

        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Starting && runner.Started.Count == 1);
        Assert.Equal(Kensaku.StillStarting, (await new Kensaku(host).SearchAsync(query, CancellationToken.None)).Problem);

        using KensakuHost up = Host(runner, new FakePipe(log, runner) { Answer = (method, _) => method == "search" ? answer : "{}" });
        up.Apply(true, Docs);
        await WaitFor(() => up.State == KensakuHostState.Up);
        ContentResult result = await new Kensaku(up).SearchAsync(query, CancellationToken.None);
        Assert.Equal([@"C:\docs\report.pdf"], result.Matches.Select(match => match.Path));
    }

    [Fact]
    public async Task A_search_kensaku_cannot_answer_yet_says_it_is_setting_up()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        var pipe = new FakePipe(log, runner)
        {
            Answer = (method, _) => method == "search" ? throw new KensakuException(KensakuError.NotSetup, "setup is not complete") : "{}",
        };
        using KensakuHost host = Host(runner, pipe);
        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Up);

        ContentResult result = await new Kensaku(host).SearchAsync(new KensakuQuery("x", new HashSet<string>()), CancellationToken.None);

        Assert.Equal(Kensaku.SettingUp, result.Problem);
    }

    [Fact]
    public async Task An_error_nobody_foresaw_restarts_kensaku_instead_of_leaving_the_host_stuck()
    {
        var log = new Log();
        var runner = new FakeRunner(log) { FailNextStart = new ArgumentException("an unexpected path") };
        var host = Host(runner, new FakePipe(log, runner));
        var problems = new System.Collections.Concurrent.ConcurrentQueue<string?>();
        host.Changed += () => problems.Enqueue(host.Problem);

        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Up);

        Assert.Contains("kensaku failed: an unexpected path", problems);
        // Quitting after such a run still ends at once rather than throwing.
        host.Dispose();
        Assert.Equal(KensakuHostState.Stopped, host.State);
    }

    [Fact]
    public async Task A_daemon_that_crashes_soon_after_coming_up_waits_longer_each_time()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(runner, new FakePipe(log, runner), backOff: [TimeSpan.Zero, TimeSpan.FromMinutes(1)]);
        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Up);

        runner.Started[0].Exit();
        await WaitFor(() => runner.Started.Count == 2 && host.State == KensakuHostState.Up);
        runner.Started[1].Exit();
        await WaitFor(() => host.State == KensakuHostState.Restarting);

        // Up for well under a minute both times, so the second crash waits the longer back-off.
        await Task.Delay(100);
        Assert.Equal(2, runner.Started.Count);
    }

    [Fact]
    public async Task A_leftover_daemon_that_does_not_stop_is_not_taken_for_a_new_one()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(runner, new FakePipe(log, runner) { Leftover = true, LeftoverStays = true }, backOff: [TimeSpan.FromMinutes(1)]);

        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Restarting);

        Assert.Empty(runner.Started);
        Assert.Equal("an earlier kensaku did not stop", host.Problem);
    }

    [Fact]
    public async Task A_daemon_that_does_not_take_this_apps_ping_fails_without_starting_again()
    {
        var log = new Log();
        var runner = new FakeRunner(log);
        using KensakuHost host = Host(runner, new FakePipe(log, runner) { Refuse = KensakuError.InvalidParams });

        host.Apply(true, Docs);
        await WaitFor(() => host.State == KensakuHostState.Failed);

        Assert.Empty(runner.Started);
        Assert.StartsWith("this kensaku does not speak the protocol files uses", host.Problem);
    }

    [Fact]
    public void Kensaku_is_found_beside_the_app_then_in_artifacts_above_it()
    {
        using var temp = new TempFolder();
        string app = Directory.CreateDirectory(Path.Combine(temp.Path, "src", "bin")).FullName;
        Assert.Null(KensakuHost.FindProgram(app));

        temp.File(@"artifacts\kensaku\kensaku.exe");
        Assert.Equal(Path.Combine(temp.Path, "artifacts", "kensaku", "kensaku.exe"), KensakuHost.FindProgram(app));

        temp.File(@"src\bin\kensaku\kensaku.exe");
        Assert.Equal(Path.Combine(app, "kensaku", "kensaku.exe"), KensakuHost.FindProgram(app));
    }
}
