using System.Diagnostics;
using System.Net;
using System.Net.Http.Headers;
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
/// Searches inside files through kensaku, a separate app that indexes file contents (issue #87):
/// its local HTTP server, <c>kensaku-cli serve</c>, on 127.0.0.1 with the bearer token it keeps in
/// <c>http.token</c> in its data folder. Name search never waits on it; every failure comes back as
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
    private static readonly HttpClient Http = new(new SocketsHttpHandler { UseProxy = false, AllowAutoRedirect = false })
    {
        Timeout = Timeout.InfiniteTimeSpan,
    };

    private Uri Base => new($"http://127.0.0.1:{port}/");

    /// <summary>
    /// The data folder kensaku uses: <c>KENSAKU_DATA_DIR</c> when set, else <c>data</c> beside a portable
    /// kensaku-cli (one with a <c>portable</c> marker), else <c>%APPDATA%\kensaku</c>, as kensaku decides.
    /// </summary>
    public static string DefaultDataFolder(string? cliPath)
    {
        string? fromEnvironment = Environment.GetEnvironmentVariable("KENSAKU_DATA_DIR");
        if (!string.IsNullOrWhiteSpace(fromEnvironment))
        {
            return fromEnvironment;
        }
        string? cliFolder = string.IsNullOrWhiteSpace(cliPath) ? null : Path.GetDirectoryName(cliPath);
        if (cliFolder is not null && (File.Exists(Path.Combine(cliFolder, "portable")) || File.Exists(Path.Combine(cliFolder, "kensaku.portable"))))
        {
            return Path.Combine(cliFolder, "data");
        }
        return Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "kensaku");
    }

    /// <summary>The kensaku app: beside the CLI, beside a portable data folder, or where its installer puts it; null when not found.</summary>
    public static string? AppPath(string? cliPath, string dataFolder)
    {
        string?[] folders =
        [
            string.IsNullOrWhiteSpace(cliPath) ? null : Path.GetDirectoryName(cliPath),
            PortableFolder(dataFolder),
            Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "kensaku"),
        ];
        return folders.OfType<string>().Select(folder => Path.Combine(folder, "kensaku.exe")).FirstOrDefault(File.Exists);
    }

    /// <summary>The folder of a portable kensaku whose data folder this is (its <c>data</c>, beside the <c>portable</c> marker), or null.</summary>
    private static string? PortableFolder(string dataFolder)
    {
        string? parent = Path.GetDirectoryName(dataFolder.TrimEnd('\\', '/'));
        bool portable = parent is not null && Path.GetFileName(dataFolder.TrimEnd('\\', '/')).Equals("data", StringComparison.OrdinalIgnoreCase)
            && (File.Exists(Path.Combine(parent, "portable")) || File.Exists(Path.Combine(parent, "kensaku.portable")));
        return portable ? parent : null;
    }

    /// <summary>
    /// Brings the kensaku app forward with <paramref name="text"/> on the clipboard to paste into it;
    /// kensaku takes no search on its command line. Returns what to tell the user.
    /// </summary>
    public static string OpenApp(string? appPath, string text)
    {
        if (appPath is null)
        {
            return "kensaku.exe was not found beside kensaku-cli or its data folder";
        }
        try
        {
            System.Windows.Clipboard.SetText(text);
        }
        catch (System.Runtime.InteropServices.COMException)
        {
            // Still opened; the text just has to be typed.
        }
        try
        {
            Process.Start(new ProcessStartInfo(appPath) { ArgumentList = { "--toggle" }, UseShellExecute = false })?.Dispose();
            return $"Opened kensaku · \"{text}\" is on the clipboard to paste";
        }
        catch (Exception error) when (error is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            return $"kensaku could not be opened: {error.Message}";
        }
    }

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

    /// <summary>Starts <c>kensaku-cli serve</c> hidden, on this port and data folder. Returns a problem in words, or null.</summary>
    public string? Start(string cliPath)
    {
        if (!Path.IsPathRooted(cliPath) || !File.Exists(cliPath))
        {
            return "kensaku-cli was not found; set its full path in Settings → Search";
        }
        try
        {
            var start = new ProcessStartInfo(cliPath)
            {
                ArgumentList = { "serve", "--port", port.ToString(System.Globalization.CultureInfo.InvariantCulture) },
                // A hidden console of its own: pipes to this app would break when it closes, and could stop kensaku.
                CreateNoWindow = true,
                UseShellExecute = false,
            };
            start.Environment["KENSAKU_DATA_DIR"] = dataFolder;
            using Process? process = Process.Start(start);
            return process is null ? "kensaku-cli could not be started" : null;
        }
        catch (Exception error) when (error is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            return $"kensaku-cli could not be started: {error.Message}";
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
        HttpStatusCode.Unauthorized => "kensaku refused the token; check its data folder in Settings → Search",
        HttpStatusCode.ServiceUnavailable => "kensaku is busy",
        _ => $"kensaku answered {(int)response.StatusCode}",
    };

    /// <summary>Errors that mean kensaku is not there or too slow, as opposed to the search being cancelled by typing.</summary>
    private static bool Unreachable(Exception error, CancellationToken cancel) =>
        !cancel.IsCancellationRequested && error is HttpRequestException or OperationCanceledException or IOException or JsonException;

    private const string NotRunning = "kensaku is not running";
    private const string TooSlow = "kensaku did not answer in time";

    /// <summary>Whether a problem means nothing answered at all, so starting kensaku-cli may help.</summary>
    public static bool IsAbsent(string? problem) => problem is NotRunning;

    private static string Described(Exception error) => error switch
    {
        TokenMissingException missing => $"{missing.Message} was not found; is the data folder right (Settings → Search)?",
        OperationCanceledException => TooSlow,
        HttpRequestException => NotRunning,
        _ => $"kensaku could not be reached: {error.Message}",
    };

    /// <summary>No token to send: kensaku has not run with this data folder, or the folder is wrong.</summary>
    private sealed class TokenMissingException(string path) : IOException(path);

    /// <summary>Whether kensaku answers within a few seconds of being started, asking every quarter second.</summary>
    public async Task<bool> WaitUntilUpAsync(TimeSpan patience, CancellationToken cancel)
    {
        DateTime giveUp = DateTime.UtcNow + patience;
        while (DateTime.UtcNow < giveUp)
        {
            if (await CheckAsync(cancel) is null)
            {
                return true;
            }
            await Task.Delay(TimeSpan.FromMilliseconds(250), cancel);
        }
        return false;
    }

    /// <summary>A snippet on one line, as the second line of a result shows it.</summary>
    private static string Tidy(string snippet) => string.Join(' ', snippet.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
}
