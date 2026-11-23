using System.IO.Pipes;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Files.Services;

/// <summary>kensaku's stable error codes (<c>error.data.code</c> on its pipe).</summary>
public enum KensakuError
{
    Internal,
    InvalidParams,
    MethodNotFound,
    NoIndex,
    NotSetup,
    DaemonUnavailable,
    Forbidden,
    Busy,
}

/// <summary>kensaku refused a call, or there was no daemon to ask.</summary>
public sealed class KensakuException(KensakuError code, string message) : Exception(message)
{
    public KensakuError Code { get; } = code;
}

/// <summary>The kensaku daemon's methods; tests put a fake behind it.</summary>
public interface IKensakuPipe
{
    /// <summary>
    /// Calls <paramref name="method"/> and returns its result. Throws <see cref="KensakuException"/> when kensaku
    /// refuses it or nobody answers, and <see cref="TimeoutException"/> when the answer takes too long.
    /// </summary>
    Task<JsonElement> CallAsync(string method, JsonObject? parameters, CancellationToken cancel);
}

/// <summary>
/// The pipe of the kensaku daemon for one data folder (issue #97): newline-delimited JSON-RPC 2.0, each request
/// carrying the token from <c>daemon.token</c> and the protocol version. Searches have a connection of their own, so a
/// slow setup call never holds one up; on each connection calls go one at a time. A connection that breaks is opened
/// again once, for calls that are safe to send twice. The token is never logged or put in a message.
/// </summary>
public sealed class KensakuPipe(string dataFolder) : IKensakuPipe, IDisposable
{
    /// <summary>The protocol kensaku-headless 2 speaks; a daemon of another version refuses the request.</summary>
    public const int ProtocolVersion = 2;

    public static readonly TimeSpan SearchLimit = TimeSpan.FromMilliseconds(800);

    // Setup checks every root, which can take a while on a share.
    private static readonly TimeSpan SetupLimit = TimeSpan.FromSeconds(30);
    private static readonly TimeSpan DefaultLimit = TimeSpan.FromSeconds(5);
    private static readonly TimeSpan ConnectTimeout = TimeSpan.FromMilliseconds(500);
    private static readonly TimeSpan DisposeWait = TimeSpan.FromSeconds(1);

    // Calls that read only, so sending one again after a broken connection does no harm.
    private static readonly HashSet<string> Repeatable = ["ping", "health", "get_status", "index_state", "search"];

    private readonly string _name = PipeName(dataFolder);
    private readonly Connection _searches = new();
    private readonly Connection _control = new();
    private long _id;

    /// <summary>One connection to the daemon, used by one call at a time; null pipe until the first call and after one breaks.</summary>
    private sealed class Connection
    {
        public SemaphoreSlim Gate { get; } = new(1, 1);

        public NamedPipeClientStream? Pipe { get; set; }

        public StreamReader? Reader { get; set; }

        public void Drop()
        {
            Reader?.Dispose();
            Pipe?.Dispose();
            Reader = null;
            Pipe = null;
        }
    }

    /// <summary>
    /// <c>kensaku-</c> and the first 12 hex digits of the SHA-256 of the lowercased data folder, as kensaku names it
    /// (<c>daemon::endpoint_id</c>), so each data folder has its own daemon.
    /// </summary>
    public static string PipeName(string dataFolder)
    {
        byte[] digest = SHA256.HashData(Encoding.UTF8.GetBytes(dataFolder.ToLowerInvariant()));
        return "kensaku-" + Convert.ToHexStringLower(digest, 0, 6);
    }

    private static TimeSpan LimitFor(string method) => method switch
    {
        "search" => SearchLimit,
        "setup" or "patch_config" => SetupLimit,
        _ => DefaultLimit,
    };

    public async Task<JsonElement> CallAsync(string method, JsonObject? parameters, CancellationToken cancel)
    {
        // Read each time, off the UI thread: kensaku makes a new token when the file is deleted.
        string token = await Task.Run(ReadToken, cancel)
            ?? throw new KensakuException(KensakuError.DaemonUnavailable, "kensaku has not made its token yet");
        long id = Interlocked.Increment(ref _id);
        var request = new JsonObject
        {
            ["jsonrpc"] = "2.0",
            ["id"] = id,
            ["method"] = method,
            ["params"] = WithToken(parameters, token),
        };
        Connection connection = method == "search" ? _searches : _control;
        // The time limit starts once it is this call's turn, so waiting behind another call does not use it up.
        await connection.Gate.WaitAsync(cancel);
        try
        {
            using var limit = CancellationTokenSource.CreateLinkedTokenSource(cancel);
            limit.CancelAfter(LimitFor(method));
            string reply = await ExchangeAsync(connection, request.ToJsonString() + "\n", Repeatable.Contains(method), limit.Token);
            return Result(reply, id);
        }
        catch (OperationCanceledException) when (!cancel.IsCancellationRequested)
        {
            // A late answer would be read as the next call's, so the connection goes.
            connection.Drop();
            throw new TimeoutException($"kensaku did not answer {method} in time");
        }
        catch (OperationCanceledException)
        {
            connection.Drop();
            throw;
        }
        catch (Exception error) when (method == "shutdown" && Gone(error))
        {
            // The daemon may exit before its answer is read, or be gone already: it is shut down either way.
            connection.Drop();
            return default;
        }
        catch (IOException error)
        {
            connection.Drop();
            throw new KensakuException(KensakuError.DaemonUnavailable, $"kensaku is not running ({error.Message})");
        }
        catch (KensakuException error) when (error.Code == KensakuError.Internal)
        {
            // An answer that could not be read or was for another request: the connection may be out of step.
            connection.Drop();
            throw;
        }
        finally
        {
            connection.Gate.Release();
        }
    }

    private static bool Gone(Exception error) =>
        error is IOException || error is KensakuException { Code: KensakuError.DaemonUnavailable };

    public void Dispose()
    {
        foreach (Connection connection in new[] { _searches, _control })
        {
            // A call in progress ends within its limit; one that does not is cut off.
            bool held = connection.Gate.Wait(DisposeWait);
            connection.Drop();
            if (held)
            {
                connection.Gate.Release();
            }
        }
    }

    private static JsonObject WithToken(JsonObject? parameters, string token)
    {
        // A copy, so the caller's parameters never hold the token.
        var copy = parameters?.DeepClone().AsObject() ?? [];
        copy["token"] = token;
        copy["protocol_version"] = ProtocolVersion;
        return copy;
    }

    /// <summary>
    /// Writes one line and reads one. A connection that was open before and broke is opened again once when
    /// <paramref name="repeatable"/>; otherwise the daemon may have acted on it, so it is not sent twice.
    /// </summary>
    private async Task<string> ExchangeAsync(Connection connection, string line, bool repeatable, CancellationToken cancel)
    {
        byte[] bytes = Encoding.UTF8.GetBytes(line);
        while (true)
        {
            bool fresh = connection.Pipe is null;
            if (fresh)
            {
                await ConnectAsync(connection, cancel);
            }
            try
            {
                await connection.Pipe!.WriteAsync(bytes, cancel);
                await connection.Pipe.FlushAsync(cancel);
                return await connection.Reader!.ReadLineAsync(cancel) ?? throw new IOException("kensaku closed the pipe");
            }
            catch (IOException) when (!fresh && repeatable)
            {
                // The daemon was restarted since the last call.
                connection.Drop();
            }
        }
    }

    private async Task ConnectAsync(Connection connection, CancellationToken cancel)
    {
        // CurrentUserOnly: a pipe of this name made by another user is never sent the token.
        var pipe = new NamedPipeClientStream(".", _name, PipeDirection.InOut, PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
        try
        {
            await pipe.ConnectAsync(ConnectTimeout, cancel);
        }
        catch (TimeoutException)
        {
            await pipe.DisposeAsync();
            throw new KensakuException(KensakuError.DaemonUnavailable, "kensaku is not running");
        }
        catch (UnauthorizedAccessException)
        {
            await pipe.DisposeAsync();
            throw new KensakuException(KensakuError.Forbidden, "kensaku's pipe belongs to another user");
        }
        catch (Exception error) when (error is IOException or OperationCanceledException)
        {
            await pipe.DisposeAsync();
            throw;
        }
        connection.Pipe = pipe;
        connection.Reader = new StreamReader(pipe, Encoding.UTF8, detectEncodingFromByteOrderMarks: false, leaveOpen: true);
    }

    /// <summary>The <c>result</c> of the answer to request <paramref name="id"/>, or its <c>error</c> as a <see cref="KensakuException"/>.</summary>
    internal static JsonElement Result(string reply, long id)
    {
        JsonElement answer;
        try
        {
            using JsonDocument document = JsonDocument.Parse(reply);
            answer = document.RootElement.Clone();
        }
        catch (JsonException)
        {
            throw new KensakuException(KensakuError.Internal, "kensaku's answer could not be read");
        }
        if (answer.ValueKind != JsonValueKind.Object)
        {
            throw new KensakuException(KensakuError.Internal, "kensaku's answer could not be read");
        }
        if (answer.TryGetProperty("error", out JsonElement error) && error.ValueKind != JsonValueKind.Null)
        {
            throw Refused(error);
        }
        // An answer to another request means the connection is out of step; dropping it puts it right.
        if (!answer.TryGetProperty("id", out JsonElement answered) || !answered.TryGetInt64(out long number) || number != id)
        {
            throw new KensakuException(KensakuError.Internal, "kensaku answered another request");
        }
        if (!answer.TryGetProperty("result", out JsonElement result))
        {
            throw new KensakuException(KensakuError.Internal, "kensaku's answer has no result");
        }
        return result;
    }

    private static KensakuException Refused(JsonElement error)
    {
        string message = error.ValueKind == JsonValueKind.Object && error.TryGetProperty("message", out JsonElement m) && m.ValueKind == JsonValueKind.String
            ? m.GetString()!
            : "kensaku refused the request";
        string? code = error.ValueKind == JsonValueKind.Object
            && error.TryGetProperty("data", out JsonElement data) && data.ValueKind == JsonValueKind.Object
            && data.TryGetProperty("code", out JsonElement c) && c.ValueKind == JsonValueKind.String
            ? c.GetString()
            : null;
        return new KensakuException(ErrorFor(code), message);
    }

    private static KensakuError ErrorFor(string? code) => code switch
    {
        "invalid_params" => KensakuError.InvalidParams,
        "method_not_found" => KensakuError.MethodNotFound,
        "no_index" => KensakuError.NoIndex,
        "not_setup" => KensakuError.NotSetup,
        "daemon_unavailable" => KensakuError.DaemonUnavailable,
        "forbidden" => KensakuError.Forbidden,
        "busy" => KensakuError.Busy,
        _ => KensakuError.Internal,
    };

    private string? ReadToken()
    {
        try
        {
            string token = File.ReadAllText(Path.Combine(dataFolder, "daemon.token")).Trim();
            return token.Length > 0 ? token : null;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            return null;
        }
    }
}
