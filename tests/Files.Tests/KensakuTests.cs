using System.Net;
using System.Net.Sockets;
using System.Text;
using Files.Services;

namespace Files.Tests;

public sealed class KensakuTests
{
    private const string Token = "secret-token";

    /// <summary>A stand-in for <c>kensaku-cli serve</c> on a free loopback port, answering each request with <paramref name="answer"/>.</summary>
    private sealed class FakeServer : IDisposable
    {
        private readonly HttpListener _listener = new();
        private readonly Func<HttpListenerRequest, (int Status, string Body, TimeSpan Delay)> _answer;

        public FakeServer(Func<HttpListenerRequest, (int Status, string Body, TimeSpan Delay)> answer)
        {
            _answer = answer;
            Port = FreePort();
            _listener.Prefixes.Add($"http://127.0.0.1:{Port}/");
            _listener.Start();
            _ = Task.Run(ServeAsync);
        }

        public int Port { get; }

        public List<string> Seen { get; } = [];

        private async Task ServeAsync()
        {
            while (_listener.IsListening)
            {
                HttpListenerContext context;
                try
                {
                    context = await _listener.GetContextAsync();
                }
                catch (Exception error) when (error is HttpListenerException or ObjectDisposedException)
                {
                    return;
                }
                lock (Seen)
                {
                    Seen.Add(context.Request.Url!.PathAndQuery);
                }
                (int status, string body, TimeSpan delay) = context.Request.Headers["Authorization"] == $"Bearer {Token}"
                    ? _answer(context.Request)
                    : (401, """{"error":"missing or invalid bearer token"}""", TimeSpan.Zero);
                await Task.Delay(delay);
                byte[] bytes = Encoding.UTF8.GetBytes(body);
                context.Response.StatusCode = status;
                try
                {
                    await context.Response.OutputStream.WriteAsync(bytes);
                    context.Response.Close();
                }
                catch (Exception error) when (error is HttpListenerException or ObjectDisposedException)
                {
                    // The client gave up first.
                }
            }
        }

        public void Dispose() => _listener.Close();

        public static int FreePort()
        {
            using var socket = new TcpListener(IPAddress.Loopback, 0);
            socket.Start();
            return ((IPEndPoint)socket.LocalEndpoint).Port;
        }
    }

    private static string DataFolder(TempFolder temp, string? token = Token)
    {
        string folder = Directory.CreateDirectory(Path.Combine(temp.Path, "data")).FullName;
        if (token is not null)
        {
            File.WriteAllText(Path.Combine(folder, "http.token"), token + "\n");
        }
        return folder;
    }

    private const string Health = """{"ok":true,"health":{"ok":true,"daemon":false,"db":true,"index":true},"as_of":1}""";

    [Fact]
    public async Task Content_matches_come_back_and_name_only_matches_are_left_to_the_panel()
    {
        using var temp = new TempFolder();
        using var server = new FakeServer(_ => (200, """
            {"results":[
              {"path":"C:\\docs\\report.pdf","score":2.5,"snippet":"the  torque\nlimit is","match_type":"Content"},
              {"path":"C:\\docs\\torque.txt","score":2.0,"snippet":null,"match_type":"FileName"},
              {"path":"C:\\docs\\scan.png","score":1.0,"snippet":null,"match_type":"Visual"}
            ],"total":3,"mode":"ranked","returned":3,"as_of":1}
            """, TimeSpan.Zero));
        var kensaku = new Kensaku(server.Port, DataFolder(temp));

        ContentResult result = await kensaku.SearchAsync("torque limit", CancellationToken.None);

        Assert.Null(result.Problem);
        Assert.Equal([@"C:\docs\report.pdf", @"C:\docs\scan.png"], result.Matches.Select(match => match.Path));
        Assert.Equal("the torque limit is", result.Matches[0].Snippet);
        Assert.Contains("q=torque%20limit", server.Seen[0]);
    }

    [Fact]
    public async Task Kensaku_that_answers_says_so_and_a_wrong_token_is_named()
    {
        using var temp = new TempFolder();
        using var server = new FakeServer(_ => (200, Health, TimeSpan.Zero));

        Assert.Null(await new Kensaku(server.Port, DataFolder(temp)).CheckAsync(CancellationToken.None));
        using var other = new TempFolder();
        string? refused = await new Kensaku(server.Port, DataFolder(other, "wrong")).CheckAsync(CancellationToken.None);
        Assert.Contains("refused the token", refused);
    }

    [Fact]
    public async Task A_kensaku_that_is_not_running_or_too_slow_is_a_problem_not_an_error()
    {
        using var temp = new TempFolder();
        string data = DataFolder(temp);
        // A port nothing listens on: a closed HttpListener's port is still answered by Windows' HTTP service.
        int closedPort = FakeServer.FreePort();
        // Refused at once: Windows' retries of a refused loopback connection are turned off.
        string? closed = (await new Kensaku(closedPort, data).SearchAsync("x", CancellationToken.None)).Problem;
        Assert.Equal("kensaku is not running", closed);

        using var slow = new FakeServer(_ => (200, """{"results":[]}""", TimeSpan.FromSeconds(3)));
        var clock = System.Diagnostics.Stopwatch.StartNew();
        ContentResult late = await new Kensaku(slow.Port, data).SearchAsync("x", CancellationToken.None);
        Assert.Equal("kensaku did not answer in time", late.Problem);
        Assert.True(clock.Elapsed < TimeSpan.FromSeconds(2), $"waited {clock.Elapsed}");
    }

    [Fact]
    public async Task A_search_cancelled_by_typing_is_given_up_quietly()
    {
        using var temp = new TempFolder();
        using var slow = new FakeServer(_ => (200, """{"results":[]}""", TimeSpan.FromSeconds(3)));
        using var typing = new CancellationTokenSource(TimeSpan.FromMilliseconds(100));

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => new Kensaku(slow.Port, DataFolder(temp)).SearchAsync("x", typing.Token));
    }

    [Theory]
    [InlineData("""{"error":"index locked"}""", "kensaku said: index locked")]
    [InlineData("""{"hits":[]}""", "this kensaku version is not supported")]
    [InlineData("not json", "kensaku's answer could not be read")]
    public void An_answer_that_is_not_results_is_a_problem(string json, string problem)
    {
        Assert.Equal(problem, Kensaku.Parse(json).Problem);
    }

    [Fact]
    public async Task A_health_answer_of_another_shape_is_an_unsupported_version()
    {
        using var temp = new TempFolder();
        using var server = new FakeServer(_ => (200, """{"status":"up","version":"2.0"}""", TimeSpan.Zero));

        Assert.Equal("this kensaku version is not supported", await new Kensaku(server.Port, DataFolder(temp)).CheckAsync(CancellationToken.None));
    }

    [Fact]
    public void Content_matches_come_after_the_name_results_without_repeating_them()
    {
        Files.Core.VolumeIndex index = new Files.Core.VolumeIndex.Builder(@"C:\docs").Build();
        var result = new Files.Core.SearchResult([new Files.Core.Hit("v", index, 0, "torque.txt", @"C:\docs", 0, 6, 0)], 1);
        ContentMatch[] contents =
        [
            new(@"C:\docs\torque.txt", 2, null, "Both"),
            new(@"C:\docs\report.pdf", 1, "the torque limit", "Content"),
        ];

        List<Files.UI.ResultRow> rows = Files.UI.SearchRows.Build(result, showVolume: false, contents: contents);

        Assert.Equal(["Best match", "torque.txt", "Contents", "report.pdf"], rows.Select(row => row.Before + row.Match + row.After));
        Assert.Equal("…the torque limit…", rows[^1].Detail);
        Assert.Equal(@"C:\docs\report.pdf", rows[^1].Path);
    }

    [Fact]
    public async Task Waiting_for_kensaku_to_come_up_keeps_to_one_deadline()
    {
        using var temp = new TempFolder();
        using var slow = new FakeServer(_ => (200, Health, TimeSpan.FromSeconds(3)));
        var clock = System.Diagnostics.Stopwatch.StartNew();

        bool up = await new Kensaku(slow.Port, DataFolder(temp)).WaitUntilUpAsync(TimeSpan.FromMilliseconds(500), CancellationToken.None);

        Assert.False(up);
        Assert.True(clock.Elapsed < TimeSpan.FromSeconds(1.5), $"waited {clock.Elapsed}");
    }

    [Fact]
    public async Task A_missing_token_is_named()
    {
        using var temp = new TempFolder();
        using var server = new FakeServer(_ => (200, Health, TimeSpan.Zero));

        string? problem = await new Kensaku(server.Port, DataFolder(temp, token: null)).CheckAsync(CancellationToken.None);

        Assert.Contains("http.token was not found", problem);
    }

    [Theory]
    [InlineData("""{"ok":true,"health":{"state":{"roots":["a","b","c"],"files":12400,"is_indexing":false}}}""", "Reading 3 volumes · 12,400 files")]
    [InlineData("""{"ok":true,"health":{"state":{"roots":["a"],"files":5,"is_indexing":true}}}""", "Indexing 1 volume · 5 files so far")]
    [InlineData("""{"ok":false,"health":{"error":"no index"}}""", "kensaku is running; its index is not open yet")]
    public void The_status_line_says_what_kensaku_reads(string json, string status)
    {
        using var culture = new CultureScope("en-US");

        Assert.Equal(status, Kensaku.Describe(json));
    }

    [Fact]
    public void Content_search_and_reading_images_survive_a_save_and_the_old_kensaku_keys_are_ignored()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "settings.json");
        new Settings { ContentSearch = true, ContentOcr = true }.Save(path);
        (Settings loaded, _) = Settings.Load(path);
        Assert.Equal((true, true), (loaded.ContentSearch, loaded.ContentOcr));

        string old = temp.File("old.json", """{ "ContentSearch": true, "KensakuCli": "kensaku-cli.exe", "KensakuData": "data", "KensakuPort": 9000 }""");
        (Settings migrated, string? problem) = Settings.Load(old);
        Assert.Null(problem);
        Assert.Equal((true, false), (migrated.ContentSearch, migrated.ContentOcr));
    }

    /// <summary>Sets the current culture for a test, so numbers are written the same on every PC.</summary>
    private sealed class CultureScope : IDisposable
    {
        private readonly System.Globalization.CultureInfo _previous = System.Globalization.CultureInfo.CurrentCulture;

        public CultureScope(string name) => System.Globalization.CultureInfo.CurrentCulture = new System.Globalization.CultureInfo(name);

        public void Dispose() => System.Globalization.CultureInfo.CurrentCulture = _previous;
    }
}
