using System.ComponentModel;
using System.Diagnostics;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;

namespace Files.Services;

/// <summary>A kensaku process <see cref="KensakuHost"/> started.</summary>
public interface IKensakuProcess : IDisposable
{
    bool HasExited { get; }

    Task WaitForExitAsync(CancellationToken cancel);

    void Kill();
}

/// <summary>Starts kensaku's programs; tests put fake processes behind it.</summary>
public interface IProcessRunner
{
    /// <summary>Starts <paramref name="program"/> hidden, with <c>KENSAKU_DATA_DIR</c> set to <paramref name="dataFolder"/>.</summary>
    IKensakuProcess Start(string program, IReadOnlyList<string> arguments, string dataFolder);

    /// <summary>Ends kensaku processes run from <paramref name="bundle"/> that an earlier run of the app left behind.</summary>
    void EndLeftovers(string bundle);
}

/// <summary>Who answers on kensaku-daemon's pipe, <c>\\.\pipe\kensaku</c>.</summary>
public enum PipeAnswer
{
    Nobody,

    /// <summary>A daemon that took this data folder's token: the one this app started.</summary>
    Ours,

    /// <summary>Another kensaku's daemon, which holds the pipe name every daemon uses.</summary>
    Other,
}

/// <summary>kensaku-daemon's pipe: newline-delimited JSON-RPC carrying the token from <c>daemon.token</c>.</summary>
public interface IKensakuPipe
{
    Task<PipeAnswer> PingAsync(CancellationToken cancel);

    /// <summary>Asks the daemon to quit; it exits without answering.</summary>
    Task ShutdownAsync(CancellationToken cancel);
}

public sealed class ProcessRunner : IProcessRunner
{
    private static readonly TimeSpan KillWait = TimeSpan.FromSeconds(2);

    public IKensakuProcess Start(string program, IReadOnlyList<string> arguments, string dataFolder)
    {
        var start = new ProcessStartInfo(program)
        {
            // A hidden console of its own: pipes to this app would break when it closes, and could stop kensaku.
            CreateNoWindow = true,
            UseShellExecute = false,
            WorkingDirectory = Path.GetDirectoryName(program)!,
        };
        foreach (string argument in arguments)
        {
            start.ArgumentList.Add(argument);
        }
        start.Environment["KENSAKU_DATA_DIR"] = dataFolder;
        Process process = Process.Start(start) ?? throw new InvalidOperationException($"{Path.GetFileName(program)} did not start");
        return new RunningProcess(process);
    }

    public void EndLeftovers(string bundle)
    {
        string folder = Path.GetFullPath(bundle).TrimEnd('\\');
        foreach (string name in new[] { "kensaku-cli", "kensaku-daemon" })
        {
            foreach (Process process in Process.GetProcessesByName(name))
            {
                using (process)
                {
                    EndIfFrom(process, folder);
                }
            }
        }
    }

    private static void EndIfFrom(Process process, string folder)
    {
        try
        {
            string? program = process.MainModule?.FileName;
            if (program is not null && string.Equals(Path.GetDirectoryName(program), folder, StringComparison.OrdinalIgnoreCase))
            {
                process.Kill();
                // Gone before the next daemon starts, so a ping never reaches the old one.
                process.WaitForExit(KillWait);
            }
        }
        catch (Exception error) when (error is Win32Exception or InvalidOperationException or NotSupportedException)
        {
            // Another user's process, or one that ended meanwhile.
        }
    }

    private sealed class RunningProcess(Process process) : IKensakuProcess
    {
        public bool HasExited
        {
            get
            {
                try
                {
                    return process.HasExited;
                }
                catch (Exception error) when (error is InvalidOperationException or Win32Exception)
                {
                    return true;
                }
            }
        }

        public Task WaitForExitAsync(CancellationToken cancel) => process.WaitForExitAsync(cancel);

        public void Kill()
        {
            try
            {
                process.Kill();
                process.WaitForExit(KillWait);
            }
            catch (Exception error) when (error is InvalidOperationException or Win32Exception)
            {
                // Already gone.
            }
        }

        public void Dispose() => process.Dispose();
    }
}

public sealed class KensakuPipe(string dataFolder) : IKensakuPipe
{
    // kensaku 1.0's pipe protocol; a daemon of another version refuses the request.
    private const int ProtocolVersion = 1;
    private const int BadToken = -32001;
    private static readonly TimeSpan ConnectTimeout = TimeSpan.FromMilliseconds(500);
    private static readonly TimeSpan AnswerTimeout = TimeSpan.FromSeconds(2);

    public async Task<PipeAnswer> PingAsync(CancellationToken cancel)
    {
        NamedPipeClientStream? connected;
        try
        {
            connected = await ConnectAsync(cancel);
        }
        catch (UnauthorizedAccessException)
        {
            // A pipe this user may not open: another user's kensaku.
            return PipeAnswer.Other;
        }
        using NamedPipeClientStream? pipe = connected;
        if (pipe is null)
        {
            return PipeAnswer.Nobody;
        }
        // Our daemon writes its token before it takes the pipe, so a pipe without one is another kensaku's.
        if (ReadToken() is not string token)
        {
            return PipeAnswer.Other;
        }
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancel);
        timeout.CancelAfter(AnswerTimeout);
        try
        {
            await SendAsync(pipe, "ping", token, timeout.Token);
            using var reader = new StreamReader(pipe, Encoding.UTF8, leaveOpen: true);
            string? line = await reader.ReadLineAsync(timeout.Token);
            return line is null ? PipeAnswer.Nobody : Answer(line);
        }
        catch (Exception error) when (error is IOException or OperationCanceledException && !cancel.IsCancellationRequested)
        {
            // It broke off or did not answer: not a daemon to start beside.
            return PipeAnswer.Nobody;
        }
    }

    public async Task ShutdownAsync(CancellationToken cancel)
    {
        try
        {
            using NamedPipeClientStream? pipe = await ConnectAsync(cancel);
            if (pipe is not null && ReadToken() is string token)
            {
                await SendAsync(pipe, "shutdown", token, cancel);
            }
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // It quit as it read the request, or the pipe is not ours: the process is ended anyway.
        }
    }

    /// <summary>A reply with a result is our daemon's; a refused token is another's.</summary>
    private static PipeAnswer Answer(string line)
    {
        try
        {
            using JsonDocument reply = JsonDocument.Parse(line);
            if (reply.RootElement.TryGetProperty("result", out _))
            {
                return PipeAnswer.Ours;
            }
            bool refused = reply.RootElement.TryGetProperty("error", out JsonElement error)
                && error.TryGetProperty("code", out JsonElement code) && code.ValueKind == JsonValueKind.Number
                && code.TryGetInt32(out int number) && number == BadToken;
            return refused ? PipeAnswer.Other : PipeAnswer.Nobody;
        }
        catch (JsonException)
        {
            return PipeAnswer.Other;
        }
    }

    private static async Task<NamedPipeClientStream?> ConnectAsync(CancellationToken cancel)
    {
        var pipe = new NamedPipeClientStream(".", "kensaku", PipeDirection.InOut, PipeOptions.Asynchronous);
        try
        {
            await pipe.ConnectAsync(ConnectTimeout, cancel);
            return pipe;
        }
        catch (Exception error) when (error is TimeoutException or IOException)
        {
            await pipe.DisposeAsync();
            return null;
        }
        catch (Exception error) when (error is OperationCanceledException or UnauthorizedAccessException)
        {
            await pipe.DisposeAsync();
            throw;
        }
    }

    private static async Task SendAsync(Stream pipe, string method, string token, CancellationToken cancel)
    {
        var request = new { jsonrpc = "2.0", id = 1, method, @params = new { token, protocol_version = ProtocolVersion } };
        byte[] line = Encoding.UTF8.GetBytes(JsonSerializer.Serialize(request) + "\n");
        await pipe.WriteAsync(line, cancel);
        await pipe.FlushAsync(cancel);
    }

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
