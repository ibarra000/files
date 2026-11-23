using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using Files.Services;

namespace Files.Tests;

public sealed class KensakuPipeTests
{
    private const string Token = "test-token";

    /// <summary>A stand-in for kensaku's daemon on the pipe for a data folder, answering each request line with <c>answer</c>; a null answer drops the connection.</summary>
    private sealed class FakeDaemon : IDisposable
    {
        private readonly CancellationTokenSource _stop = new();
        private readonly Func<JsonObject, Task<string?>> _answer;
        private readonly List<JsonObject> _requests = [];
        private int _connections;

        public FakeDaemon(string dataFolder, Func<JsonObject, Task<string?>> answer)
        {
            _answer = answer;
            string name = KensakuPipe.PipeName(dataFolder);
            _ = Task.Run(() => ServeAsync(name));
        }

        public int Connections => Volatile.Read(ref _connections);

        public List<JsonObject> Requests
        {
            get
            {
                lock (_requests)
                {
                    return [.. _requests];
                }
            }
        }

        private async Task ServeAsync(string name)
        {
            while (!_stop.IsCancellationRequested)
            {
                var server = new NamedPipeServerStream(name, PipeDirection.InOut, NamedPipeServerStream.MaxAllowedServerInstances,
                    PipeTransmissionMode.Byte, PipeOptions.Asynchronous);
                try
                {
                    await server.WaitForConnectionAsync(_stop.Token);
                }
                catch (OperationCanceledException)
                {
                    await server.DisposeAsync();
                    return;
                }
                Interlocked.Increment(ref _connections);
                // Each connection answered on its own, as kensaku does, so a slow answer does not hold up the next client.
                _ = Task.Run(() => AnswerAsync(server));
            }
        }

        private async Task AnswerAsync(NamedPipeServerStream server)
        {
            await using (server)
            {
                try
                {
                    using var reader = new StreamReader(server, Encoding.UTF8, false, 1024, leaveOpen: true);
                    while (await reader.ReadLineAsync(_stop.Token) is string line)
                    {
                        JsonObject request = JsonNode.Parse(line)!.AsObject();
                        lock (_requests)
                        {
                            _requests.Add(request);
                        }
                        if (await _answer(request) is not string reply)
                        {
                            return;
                        }
                        await server.WriteAsync(Encoding.UTF8.GetBytes(reply + "\n"), _stop.Token);
                        await server.FlushAsync(_stop.Token);
                    }
                }
                catch (Exception error) when (error is OperationCanceledException or IOException)
                {
                    // Stopped, or the client went.
                }
            }
        }

        public void Dispose() => _stop.Cancel();
    }

    private static string DataFolder(TempFolder temp, bool token = true)
    {
        string folder = Directory.CreateDirectory(Path.Combine(temp.Path, "data")).FullName;
        if (token)
        {
            File.WriteAllText(Path.Combine(folder, "daemon.token"), Token + "\n");
        }
        return folder;
    }

    private static string Ok(JsonObject request, string result = """{"ok":true}""") =>
        $$"""{"jsonrpc":"2.0","id":{{request["id"]!.ToJsonString()}},"result":{{result}}}""";

    [Fact]
    public void The_pipe_is_named_from_the_lowercased_data_folder_as_kensaku_names_it()
    {
        // The first 12 hex digits of SHA-256("c:\users\me\appdata\local\files\kensaku"), worked out apart from this code.
        Assert.Equal("kensaku-43a453a9475a", KensakuPipe.PipeName(@"C:\Users\Me\AppData\Local\files\kensaku"));
        Assert.Equal(KensakuPipe.PipeName(@"C:\DATA"), KensakuPipe.PipeName(@"c:\data"));
    }

    [Fact]
    public async Task Every_call_carries_the_token_and_protocol_version_2_without_changing_the_callers_parameters()
    {
        using var temp = new TempFolder();
        string data = DataFolder(temp);
        using var daemon = new FakeDaemon(data, request => Task.FromResult<string?>(Ok(request, """{"results":[],"total":0}""")));
        using var pipe = new KensakuPipe(data);
        var parameters = new JsonObject { ["query"] = "torque", ["limit"] = 5 };

        JsonElement result = await pipe.CallAsync("search", parameters, CancellationToken.None);

        Assert.Equal(0, result.GetProperty("total").GetInt32());
        JsonObject sent = daemon.Requests.Single();
        Assert.Equal(("2.0", "search"), (sent["jsonrpc"]!.GetValue<string>(), sent["method"]!.GetValue<string>()));
        Assert.Equal(Token, sent["params"]!["token"]!.GetValue<string>());
        Assert.Equal(2, sent["params"]!["protocol_version"]!.GetValue<int>());
        Assert.Equal("torque", sent["params"]!["query"]!.GetValue<string>());
        Assert.False(parameters.ContainsKey("token"));
    }

    [Theory]
    [InlineData("not_setup", KensakuError.NotSetup)]
    [InlineData("no_index", KensakuError.NoIndex)]
    [InlineData("busy", KensakuError.Busy)]
    [InlineData("forbidden", KensakuError.Forbidden)]
    [InlineData("invalid_params", KensakuError.InvalidParams)]
    [InlineData("something_new", KensakuError.Internal)]
    public async Task An_error_code_from_kensaku_becomes_its_kind(string code, KensakuError kind)
    {
        using var temp = new TempFolder();
        string data = DataFolder(temp);
        using var daemon = new FakeDaemon(data, request => Task.FromResult<string?>(
            """{"jsonrpc":"2.0","id":1,"error":{"code":-32011,"message":"setup is not complete","data":{"code":""" + JsonSerializer.Serialize(code) + "}}}"));
        using var pipe = new KensakuPipe(data);

        var error = await Assert.ThrowsAsync<KensakuException>(() => pipe.CallAsync("search", null, CancellationToken.None));

        Assert.Equal(kind, error.Code);
        Assert.Equal("setup is not complete", error.Message);
    }

    [Fact]
    public async Task A_search_that_takes_too_long_times_out_and_the_next_call_still_works()
    {
        using var temp = new TempFolder();
        string data = DataFolder(temp);
        using var daemon = new FakeDaemon(data, async request =>
        {
            if (request["method"]!.GetValue<string>() == "search")
            {
                await Task.Delay(TimeSpan.FromSeconds(3));
            }
            return Ok(request);
        });
        using var pipe = new KensakuPipe(data);
        var clock = System.Diagnostics.Stopwatch.StartNew();

        await Assert.ThrowsAsync<TimeoutException>(() => pipe.CallAsync("search", null, CancellationToken.None));

        Assert.True(clock.Elapsed < TimeSpan.FromSeconds(2), $"waited {clock.Elapsed}");
        // The slow connection was dropped, so the late answer is not read as the ping's.
        JsonElement ping = await pipe.CallAsync("ping", null, CancellationToken.None);
        Assert.True(ping.GetProperty("ok").GetBoolean());
        Assert.Equal(2, daemon.Connections);
    }

    [Fact]
    public async Task A_connection_the_daemon_dropped_is_opened_again()
    {
        using var temp = new TempFolder();
        string data = DataFolder(temp);
        int calls = 0;
        // Answers the first call, then drops the connection on the second, as a daemon that restarted between them.
        using var daemon = new FakeDaemon(data, request => Task.FromResult(Interlocked.Increment(ref calls) == 2 ? null : Ok(request)));
        using var pipe = new KensakuPipe(data);

        await pipe.CallAsync("ping", null, CancellationToken.None);
        JsonElement second = await pipe.CallAsync("ping", null, CancellationToken.None);

        Assert.True(second.GetProperty("ok").GetBoolean());
        Assert.Equal(2, daemon.Connections);
    }

    [Fact]
    public async Task No_daemon_or_no_token_means_kensaku_is_not_running()
    {
        using var temp = new TempFolder();
        using var pipe = new KensakuPipe(DataFolder(temp));
        var nobody = await Assert.ThrowsAsync<KensakuException>(() => pipe.CallAsync("ping", null, CancellationToken.None));
        Assert.Equal(KensakuError.DaemonUnavailable, nobody.Code);

        using var other = new TempFolder();
        using var tokenless = new KensakuPipe(DataFolder(other, token: false));
        var missing = await Assert.ThrowsAsync<KensakuException>(() => tokenless.CallAsync("ping", null, CancellationToken.None));
        Assert.Equal(KensakuError.DaemonUnavailable, missing.Code);
    }

    [Fact]
    public async Task A_shutdown_with_no_daemon_left_is_done()
    {
        using var temp = new TempFolder();
        using var pipe = new KensakuPipe(DataFolder(temp));

        await pipe.CallAsync("shutdown", null, CancellationToken.None);
    }

    [Fact]
    public async Task A_change_is_not_sent_twice_when_the_connection_breaks_under_it()
    {
        using var temp = new TempFolder();
        string data = DataFolder(temp);
        int calls = 0;
        using var daemon = new FakeDaemon(data, request => Task.FromResult(Interlocked.Increment(ref calls) == 2 ? null : Ok(request)));
        using var pipe = new KensakuPipe(data);
        await pipe.CallAsync("ping", null, CancellationToken.None);

        var error = await Assert.ThrowsAsync<KensakuException>(() => pipe.CallAsync("patch_config", new JsonObject { ["patch"] = new JsonObject() }, CancellationToken.None));

        // The daemon may have applied it before the connection broke.
        Assert.Equal(KensakuError.DaemonUnavailable, error.Code);
        Assert.Equal(2, daemon.Requests.Count);
    }

    [Fact]
    public async Task A_search_is_not_held_up_by_a_slow_setup()
    {
        using var temp = new TempFolder();
        string data = DataFolder(temp);
        using var daemon = new FakeDaemon(data, async request =>
        {
            if (request["method"]!.GetValue<string>() == "setup")
            {
                await Task.Delay(TimeSpan.FromSeconds(3));
            }
            return Ok(request, """{"results":[]}""");
        });
        using var pipe = new KensakuPipe(data);
        Task setup = pipe.CallAsync("setup", null, CancellationToken.None);
        await WaitUntil(() => daemon.Requests.Count == 1);

        JsonElement result = await pipe.CallAsync("search", null, CancellationToken.None);

        Assert.Equal(0, result.GetProperty("results").GetArrayLength());
        Assert.False(setup.IsCompleted);
    }

    [Fact]
    public void A_null_error_is_no_error_and_an_answer_to_another_request_is_refused()
    {
        JsonElement result = KensakuPipe.Result("""{"jsonrpc":"2.0","id":7,"error":null,"result":{"ok":true}}""", 7);
        Assert.True(result.GetProperty("ok").GetBoolean());

        var error = Assert.Throws<KensakuException>(() => KensakuPipe.Result("""{"jsonrpc":"2.0","id":6,"result":{}}""", 7));
        Assert.Equal(KensakuError.Internal, error.Code);
    }

    private static async Task WaitUntil(Func<bool> condition)
    {
        var clock = System.Diagnostics.Stopwatch.StartNew();
        while (!condition())
        {
            Assert.True(clock.Elapsed < TimeSpan.FromSeconds(5), "timed out");
            await Task.Delay(10);
        }
    }

    [Fact]
    public async Task A_shutdown_the_daemon_does_not_answer_is_still_done()
    {
        using var temp = new TempFolder();
        string data = DataFolder(temp);
        using var daemon = new FakeDaemon(data, _ => Task.FromResult<string?>(null));
        using var pipe = new KensakuPipe(data);

        await pipe.CallAsync("shutdown", null, CancellationToken.None);

        Assert.Equal("shutdown", daemon.Requests.Single()["method"]!.GetValue<string>());
    }
}
