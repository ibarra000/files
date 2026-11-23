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
/// The pipe of the kensaku daemon for one data folder (issue #97): newline-delimited JSON-RPC 2.0, one request and one
/// answer at a time on one connection, each request carrying the token from <c>daemon.token</c> and the protocol
/// version. A connection that breaks is opened again once. The token is never logged or put in a message.
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

    private readonly SemaphoreSlim _gate = new(1, 1);
    private readonly string _name = PipeName(dataFolder);

    // The open connection, guarded by _gate; null until the first call and after one breaks.
    private NamedPipeClientStream? _pipe;
    private StreamReader? _reader;
    private long _id;

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
        var request = new JsonObject
        {
            ["jsonrpc"] = "2.0",
            ["id"] = Interlocked.Increment(ref _id),
            ["method"] = method,
            ["params"] = WithToken(parameters, token),
        };
        using var limit = CancellationTokenSource.CreateLinkedTokenSource(cancel);
        limit.CancelAfter(LimitFor(method));
        try
        {
            await _gate.WaitAsync(limit.Token);
        }
        catch (OperationCanceledException) when (!cancel.IsCancellationRequested)
        {
            throw new TimeoutException($"kensaku did not answer {method} in time");
        }
        try
        {
            string reply = await ExchangeAsync(request.ToJsonString() + "\n", limit.Token);
            return Result(reply);
        }
        catch (OperationCanceledException) when (!cancel.IsCancellationRequested)
        {
            // A late answer would be read as the next call's, so the connection goes.
            Drop();
            throw new TimeoutException($"kensaku did not answer {method} in time");
        }
        catch (OperationCanceledException)
        {
            Drop();
            throw;
        }
        catch (IOException) when (method == "shutdown")
        {
            // The daemon may exit before its answer is read: it is shutting down either way.
            Drop();
            return default;
        }
        catch (IOException error)
        {
            Drop();
            throw new KensakuException(KensakuError.DaemonUnavailable, $"kensaku is not running ({error.Message})");
        }
        finally
        {
            _gate.Release();
        }
    }

    public void Dispose() => Drop();

    private static JsonObject WithToken(JsonObject? parameters, string token)
    {
        // A copy, so the caller's parameters never hold the token.
        var copy = parameters?.DeepClone().AsObject() ?? [];
        copy["token"] = token;
        copy["protocol_version"] = ProtocolVersion;
        return copy;
    }

    /// <summary>Writes one line and reads one; a connection that was open before and broke is opened again once.</summary>
    private async Task<string> ExchangeAsync(string line, CancellationToken cancel)
    {
        byte[] bytes = Encoding.UTF8.GetBytes(line);
        while (true)
        {
            bool fresh = _pipe is null;
            if (fresh)
            {
                await ConnectAsync(cancel);
            }
            try
            {
                await _pipe!.WriteAsync(bytes, cancel);
                await _pipe.FlushAsync(cancel);
                return await _reader!.ReadLineAsync(cancel) ?? throw new IOException("kensaku closed the pipe");
            }
            catch (IOException) when (!fresh)
            {
                // The daemon was restarted since the last call.
                Drop();
            }
        }
    }

    private async Task ConnectAsync(CancellationToken cancel)
    {
        var pipe = new NamedPipeClientStream(".", _name, PipeDirection.InOut, PipeOptions.Asynchronous);
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
        _pipe = pipe;
        _reader = new StreamReader(pipe, Encoding.UTF8, detectEncodingFromByteOrderMarks: false, leaveOpen: true);
    }

    private void Drop()
    {
        _reader?.Dispose();
        _pipe?.Dispose();
        _reader = null;
        _pipe = null;
    }

    /// <summary>The <c>result</c> of an answer, or its <c>error</c> as a <see cref="KensakuException"/>.</summary>
    internal static JsonElement Result(string reply)
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
        if (answer.ValueKind == JsonValueKind.Object && answer.TryGetProperty("error", out JsonElement error))
        {
            throw Refused(error);
        }
        if (answer.ValueKind != JsonValueKind.Object || !answer.TryGetProperty("result", out JsonElement result))
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
