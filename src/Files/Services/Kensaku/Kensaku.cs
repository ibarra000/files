using System.Net;
using System.Net.Http.Headers;
using System.Net.Sockets;
using System.Text.Json;

namespace Files.Services;

/// <summary>A file kensaku found by what is inside it, with the text around the match when it has one.</summary>
public sealed record ContentMatch(string Path, double Score, string? Snippet, string MatchType);

/// <summary>What a content search gave: the matches, or why there are none.</summary>
public sealed record ContentResult(IReadOnlyList<ContentMatch> Matches, string? Problem)
{
    public static ContentResult Unavailable(string problem) => new([], problem);
}

/// <summary>
/// Searches inside files through the bundled kensaku, which indexes file contents (issues #87, #97):
/// its local HTTP server, <c>kensaku-cli serve</c> started by <see cref="KensakuHost"/>, on 127.0.0.1 with
/// the bearer token it keeps in <c>http.token</c> in its data folder. Name search never waits on it; every failure comes back as
/// a problem in words, and nothing returned is stored.
/// </summary>
public sealed class Kensaku(int port, string dataFolder)
{
    /// <summary>How long a content search may take before the panel goes on without it.</summary>
    public static readonly TimeSpan SearchTimeout = TimeSpan.FromMilliseconds(800);

    /// <summary>The most content matches asked for; kensaku's own limit is 50.</summary>
    public const int MaxMatches = 20;

    private static readonly TimeSpan HealthTimeout = TimeSpan.FromSeconds(2);

    // One client for the app's life; each call sets its own time limit. No proxy and no redirects,
    // so the token is only ever sent to this PC, never to a proxy or wherever a squatter on the port points.
    private static readonly HttpClient Http = new(new SocketsHttpHandler { UseProxy = false, AllowAutoRedirect = false, ConnectCallback = ConnectAsync })
    {
        Timeout = Timeout.InfiniteTimeSpan,
    };

    // SIO_TCP_INITIAL_RTO with TCP_INITIAL_RTO_PARAMETERS { Rtt = unspecified, MaxSynRetransmissions = none }.
    private const int InitialRto = unchecked((int)0x98000011);
    private static readonly byte[] NoSynRetransmissions = [0xFF, 0xFF, 0xFE, 0x00];

    /// <summary>
    /// Connects without Windows' SYN retries, so a port nothing listens on is refused at once rather than after
    /// about two seconds: that refusal is how the panel tells kensaku is not running.
    /// </summary>
    private static async ValueTask<Stream> ConnectAsync(SocketsHttpConnectionContext context, CancellationToken cancel)
    {
        var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp) { NoDelay = true };
        try
        {
            socket.IOControl(InitialRto, NoSynRetransmissions, null);
        }
        catch (SocketException)
        {
            // An older Windows: the refusal just takes longer.
        }
        try
        {
            await socket.ConnectAsync(context.DnsEndPoint, cancel);
            return new NetworkStream(socket, ownsSocket: true);
        }
        catch (Exception)
        {
            socket.Dispose();
            throw;
        }
    }

    private Uri Base => new($"http://127.0.0.1:{port}/");

    /// <summary>Whether kensaku answers, as a problem in words or null when it does.</summary>
    public async Task<string?> CheckAsync(CancellationToken cancel)
    {
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancel);
        timeout.CancelAfter(HealthTimeout);
        try
        {
            using HttpResponseMessage response = await SendAsync("health", timeout.Token);
            if (Problem(response) is string problem)
            {
                return problem;
            }
            using JsonDocument health = JsonDocument.Parse(await response.Content.ReadAsStringAsync(timeout.Token));
            // The shape this was written against (kensaku 1.0); another one is a version this panel does not know.
            if (!health.RootElement.TryGetProperty("health", out JsonElement details) || details.ValueKind != JsonValueKind.Object)
            {
                return "this kensaku version is not supported";
            }
            return health.RootElement.TryGetProperty("ok", out JsonElement ok) && ok.ValueKind == JsonValueKind.True
                ? null
                : "kensaku is running but its index is not ready";
        }
        catch (Exception error) when (Unreachable(error, cancel))
        {
            return Described(error);
        }
    }

    /// <summary>What kensaku is reading, for the status line in Settings, or a problem in words.</summary>
    public async Task<string> StatusAsync(CancellationToken cancel)
    {
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancel);
        timeout.CancelAfter(HealthTimeout);
        try
        {
            using HttpResponseMessage response = await SendAsync("health", timeout.Token);
            return Problem(response) ?? Describe(await response.Content.ReadAsStringAsync(timeout.Token));
        }
        catch (Exception error) when (Unreachable(error, cancel))
        {
            return Described(error);
        }
    }

    /// <summary>
    /// Reads <c>/health</c>'s <c>{ health: { state: { roots, files, is_indexing } } }</c> as
    /// "Reading 3 volumes · 12,400 files".
    /// </summary>
    public static string Describe(string json)
    {
        try
        {
            using JsonDocument document = JsonDocument.Parse(json);
            if (!document.RootElement.TryGetProperty("health", out JsonElement health)
                || !health.TryGetProperty("state", out JsonElement state) || state.ValueKind != JsonValueKind.Object)
            {
                return "kensaku is running; its index is not open yet";
            }
            int roots = state.TryGetProperty("roots", out JsonElement r) && r.ValueKind == JsonValueKind.Array ? r.GetArrayLength() : 0;
            long files = state.TryGetProperty("files", out JsonElement f) && f.ValueKind == JsonValueKind.Number ? f.GetInt64() : 0;
            bool indexing = state.TryGetProperty("is_indexing", out JsonElement i) && i.ValueKind == JsonValueKind.True;
            string volumes = roots == 1 ? "1 volume" : $"{roots:N0} volumes";
            string read = files == 1 ? "1 file" : $"{files:N0} files";
            return indexing ? $"Indexing {volumes} · {read} so far" : $"Reading {volumes} · {read}";
        }
        catch (JsonException)
        {
            return "kensaku's answer could not be read";
        }
    }

    /// <summary>Files whose contents match <paramref name="query"/>, best first; files kensaku found only by name are left out, the panel lists those.</summary>
    public async Task<ContentResult> SearchAsync(string query, CancellationToken cancel)
    {
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancel);
        timeout.CancelAfter(SearchTimeout);
        try
        {
            string path = $"search?q={Uri.EscapeDataString(query)}&limit={MaxMatches}";
            using HttpResponseMessage response = await SendAsync(path, timeout.Token);
            if (Problem(response) is string problem)
            {
                return ContentResult.Unavailable(problem);
            }
            return Parse(await response.Content.ReadAsStringAsync(timeout.Token));
        }
        catch (Exception error) when (Unreachable(error, cancel))
        {
            return ContentResult.Unavailable(Described(error));
        }
    }

    /// <summary>Reads <c>/search</c>'s answer: <c>{ results: [{ path, score, snippet, match_type }], … }</c> or <c>{ error }</c>.</summary>
    public static ContentResult Parse(string json)
    {
        try
        {
            using JsonDocument document = JsonDocument.Parse(json);
            JsonElement root = document.RootElement;
            if (root.TryGetProperty("error", out JsonElement error))
            {
                return ContentResult.Unavailable($"kensaku said: {(error.ValueKind == JsonValueKind.String ? error.GetString() : error.GetRawText())}");
            }
            if (!root.TryGetProperty("results", out JsonElement results) || results.ValueKind != JsonValueKind.Array)
            {
                return ContentResult.Unavailable("this kensaku version is not supported");
            }
            var matches = new List<ContentMatch>();
            foreach (JsonElement result in results.EnumerateArray())
            {
                string? path = result.TryGetProperty("path", out JsonElement p) && p.ValueKind == JsonValueKind.String ? p.GetString() : null;
                string type = result.TryGetProperty("match_type", out JsonElement t) && t.ValueKind == JsonValueKind.String ? t.GetString()! : "";
                if (path is null || type == "FileName")
                {
                    continue;
                }
                double score = result.TryGetProperty("score", out JsonElement s) && s.ValueKind == JsonValueKind.Number ? s.GetDouble() : 0;
                string? snippet = result.TryGetProperty("snippet", out JsonElement n) && n.ValueKind == JsonValueKind.String ? Tidy(n.GetString()!) : null;
                matches.Add(new ContentMatch(path, score, snippet, type));
            }
            return new ContentResult(matches, null);
        }
        catch (JsonException)
        {
            return ContentResult.Unavailable("kensaku's answer could not be read");
        }
    }

    private async Task<HttpResponseMessage> SendAsync(string path, CancellationToken cancel)
    {
        // Read each time (kensaku makes a new token when the file is deleted), and off the UI thread,
        // since the data folder may be on a share that is slow to answer.
        string token = await Task.Run(ReadToken, cancel) ?? throw new TokenMissingException(Path.Combine(dataFolder, "http.token"));
        var request = new HttpRequestMessage(HttpMethod.Get, new Uri(Base, path));
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
        return await Http.SendAsync(request, HttpCompletionOption.ResponseContentRead, cancel);
    }

    private string? ReadToken()
    {
        try
        {
            string token = File.ReadAllText(Path.Combine(dataFolder, "http.token")).Trim();
            return token.Length > 0 ? token : null;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            return null;
        }
    }

    private static string? Problem(HttpResponseMessage response) => response.StatusCode switch
    {
        HttpStatusCode.OK => null,
        HttpStatusCode.Unauthorized => "kensaku refused the token",
        HttpStatusCode.ServiceUnavailable => "kensaku is busy",
        _ => $"kensaku answered {(int)response.StatusCode}",
    };

    /// <summary>Errors that mean kensaku is not there or too slow, as opposed to the search being cancelled by typing.</summary>
    private static bool Unreachable(Exception error, CancellationToken cancel) =>
        !cancel.IsCancellationRequested && error is HttpRequestException or OperationCanceledException or IOException or JsonException;

    public const string NotRunning = "kensaku is not running";
    public const string TooSlow = "kensaku did not answer in time";

    private static string Described(Exception error) => error switch
    {
        TokenMissingException missing => $"{missing.Message} was not found",
        OperationCanceledException => TooSlow,
        HttpRequestException => NotRunning,
        _ => $"kensaku could not be reached: {error.Message}",
    };

    /// <summary>No token to send: <c>serve</c> has not run with this data folder yet.</summary>
    private sealed class TokenMissingException(string path) : IOException(path);

    /// <summary>
    /// Whether kensaku answers within <paramref name="patience"/> of being started, asking every quarter second.
    /// One deadline covers every check, so a check that hangs cannot stretch the wait.
    /// </summary>
    public async Task<bool> WaitUntilUpAsync(TimeSpan patience, CancellationToken cancel)
    {
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancel);
        deadline.CancelAfter(patience);
        try
        {
            while (await CheckAsync(deadline.Token) is not null)
            {
                await Task.Delay(TimeSpan.FromMilliseconds(250), deadline.Token);
            }
            return true;
        }
        catch (Exception) when (deadline.IsCancellationRequested && !cancel.IsCancellationRequested)
        {
            // Whatever a check was doing when the deadline passed.
            return false;
        }
    }

    /// <summary>A snippet on one line, as the second line of a result shows it.</summary>
    private static string Tidy(string snippet) => string.Join(' ', snippet.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
}
