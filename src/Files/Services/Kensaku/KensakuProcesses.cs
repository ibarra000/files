using System.ComponentModel;
using System.Diagnostics;

namespace Files.Services;

/// <summary>A kensaku process <see cref="KensakuHost"/> started.</summary>
public interface IKensakuProcess : IDisposable
{
    bool HasExited { get; }

    Task WaitForExitAsync(CancellationToken cancel);

    void Kill();
}

/// <summary>Starts kensaku; tests put fake processes behind it.</summary>
public interface IProcessRunner
{
    /// <summary>Starts <paramref name="program"/> hidden, with <paramref name="environment"/> added to this app's.</summary>
    IKensakuProcess Start(string program, IReadOnlyList<string> arguments, IReadOnlyDictionary<string, string> environment);
}

public sealed class ProcessRunner : IProcessRunner
{
    private static readonly TimeSpan KillWait = TimeSpan.FromSeconds(2);

    public IKensakuProcess Start(string program, IReadOnlyList<string> arguments, IReadOnlyDictionary<string, string> environment)
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
        foreach ((string name, string value) in environment)
        {
            start.Environment[name] = value;
        }
        Process process = Process.Start(start) ?? throw new InvalidOperationException($"{Path.GetFileName(program)} did not start");
        return new RunningProcess(process);
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
