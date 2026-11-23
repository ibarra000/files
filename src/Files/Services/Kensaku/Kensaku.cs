using System.Text.Json;
using System.Text.Json.Nodes;

namespace Files.Services;

/// <summary>A file kensaku found by what is inside it, with the text around the match when it has one.</summary>
public sealed record ContentMatch(string Path, double Score, string? Snippet, string MatchType);

/// <summary>What a content search gave: the matches, or why there are none.</summary>
public sealed record ContentResult(IReadOnlyList<ContentMatch> Matches, string? Problem)
{
    public static ContentResult Unavailable(string problem) => new([], problem);
}

/// <summary>
/// Searches inside files through the bundled kensaku that <see cref="KensakuHost"/> runs (issues #87, #97), over its
/// daemon's pipe. Name search never waits on it; every failure comes back as a problem in words, and nothing returned
/// is stored.
/// </summary>
public sealed class Kensaku(KensakuHost host)
{
    /// <summary>The most content matches shown.</summary>
    public const int MaxMatches = 20;

    // Asked for when matches are kept to several extensions here, so enough are left after.
    private const int FilteredMatches = 100;

    public const string NotRunning = "kensaku is not running";
    public const string TooSlow = "kensaku did not answer in time";
    public const string StillStarting = "kensaku is starting";
    public const string SettingUp = "kensaku is still setting up";

    /// <summary>Files whose contents match <paramref name="query"/>, best first; files kensaku found only by name are left out, the panel lists those.</summary>
    public async Task<ContentResult> SearchAsync(KensakuQuery query, CancellationToken cancel)
    {
        if (NotUp() is string problem)
        {
            return ContentResult.Unavailable(problem);
        }
        var parameters = new JsonObject
        {
            ["query"] = query.Text,
            ["limit"] = query.Extensions.Count > 1 ? FilteredMatches : MaxMatches,
            ["snippets"] = true,
        };
        try
        {
            JsonElement result = await host.Pipe.CallAsync("search", parameters, cancel);
            return Parse(result, query);
        }
        catch (TimeoutException)
        {
            return ContentResult.Unavailable(TooSlow);
        }
        catch (KensakuException error)
        {
            return ContentResult.Unavailable(Described(error));
        }
    }

    /// <summary>What kensaku is reading, for the status line in Settings, or what the host is doing.</summary>
    public async Task<string> StatusAsync(CancellationToken cancel)
    {
        if (host.State != KensakuHostState.Up)
        {
            return host.Status;
        }
        try
        {
            JsonElement status = await host.Pipe.CallAsync("get_status", null, cancel);
            JsonElement state = await host.Pipe.CallAsync("index_state", null, cancel);
            return Describe(status, state);
        }
        catch (TimeoutException)
        {
            return TooSlow;
        }
        catch (KensakuException error)
        {
            return Described(error);
        }
    }

    /// <summary>Tells kensaku a file it found was opened, so its ranking follows what is opened. Never waited on.</summary>
    public void RecordOpen(string path)
    {
        if (host.State != KensakuHostState.Up)
        {
            return;
        }
        _ = Task.Run(async () =>
        {
            try
            {
                await host.Pipe.CallAsync("record_open", new JsonObject { ["path"] = path }, CancellationToken.None);
            }
            catch (Exception error) when (error is KensakuException or TimeoutException)
            {
                // Only ranking is lost.
            }
        });
    }

    /// <summary>Why the panel does not ask kensaku now, or null while it is up.</summary>
    private string? NotUp() => host.State switch
    {
        KensakuHostState.Up => null,
        KensakuHostState.Starting or KensakuHostState.Restarting => StillStarting,
        _ => host.Problem ?? NotRunning,
    };

    private static string Described(KensakuException error) => error.Code switch
    {
        KensakuError.NotSetup or KensakuError.NoIndex => SettingUp,
        // The host sees the daemon exit and starts it again.
        KensakuError.DaemonUnavailable => NotRunning,
        KensakuError.Busy => "kensaku is busy",
        _ => $"kensaku said: {error.Message}",
    };

    /// <summary>
    /// "Reading 3 volumes · 12,400 files · 3,100 with text", from <c>get_status</c>'s <c>is_indexing</c> and
    /// <c>index_state</c>'s <c>roots</c>, <c>files</c> and <c>content_docs</c>.
    /// </summary>
    public static string Describe(JsonElement status, JsonElement state)
    {
        if (state.ValueKind != JsonValueKind.Object)
        {
            return "kensaku is running; its index is not open yet";
        }
        int roots = state.TryGetProperty("roots", out JsonElement r) && r.ValueKind == JsonValueKind.Array ? r.GetArrayLength() : 0;
        long files = Number(state, "files");
        long text = Number(state, "content_docs");
        bool indexing = status.ValueKind == JsonValueKind.Object
            && status.TryGetProperty("is_indexing", out JsonElement i) && i.ValueKind == JsonValueKind.True;
        string volumes = roots == 1 ? "1 volume" : $"{roots:N0} volumes";
        string read = files == 1 ? "1 file" : $"{files:N0} files";
        return indexing
            ? $"Indexing {volumes} · {read} so far · {text:N0} with text"
            : $"Reading {volumes} · {read} · {text:N0} with text";
    }

    private static long Number(JsonElement element, string name) =>
        element.TryGetProperty(name, out JsonElement value) && value.ValueKind == JsonValueKind.Number && value.TryGetInt64(out long number) ? number : 0;

    /// <summary>Reads <c>search</c>'s result, <c>{ results: [{ path, score, snippet, match_type }], … }</c>, kept to <paramref name="query"/>'s extensions.</summary>
    public static ContentResult Parse(JsonElement root, KensakuQuery query)
    {
        if (root.ValueKind != JsonValueKind.Object || !root.TryGetProperty("results", out JsonElement results) || results.ValueKind != JsonValueKind.Array)
        {
            return ContentResult.Unavailable("this kensaku version is not supported");
        }
        var matches = new List<ContentMatch>();
        foreach (JsonElement result in results.EnumerateArray())
        {
            if (matches.Count == MaxMatches)
            {
                break;
            }
            if (result.ValueKind != JsonValueKind.Object)
            {
                continue;
            }
            string? path = result.TryGetProperty("path", out JsonElement p) && p.ValueKind == JsonValueKind.String ? p.GetString() : null;
            string type = result.TryGetProperty("match_type", out JsonElement t) && t.ValueKind == JsonValueKind.String ? t.GetString()! : "";
            bool folder = result.TryGetProperty("is_dir", out JsonElement d) && d.ValueKind == JsonValueKind.True;
            if (path is null || folder || type == "FileName" || !query.Keeps(path))
            {
                continue;
            }
            double score = result.TryGetProperty("score", out JsonElement s) && s.ValueKind == JsonValueKind.Number ? s.GetDouble() : 0;
            string? snippet = result.TryGetProperty("snippet", out JsonElement n) && n.ValueKind == JsonValueKind.String ? Tidy(n.GetString()!) : null;
            matches.Add(new ContentMatch(path, score, snippet, type));
        }
        return new ContentResult(matches, null);
    }

    /// <summary>A snippet on one line, as the second line of a result shows it.</summary>
    private static string Tidy(string snippet) => string.Join(' ', snippet.Split((char[]?)null, StringSplitOptions.RemoveEmptyEntries));
}
