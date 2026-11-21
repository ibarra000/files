using System.Reflection;
using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using Microsoft.VisualBasic.FileIO;

namespace Files.Services;

/// <summary>
/// Sends files to the Recycle Bin and brings them back. Sending uses the shell's own delete (through
/// Microsoft.VisualBasic), which keeps what Explorer's Restore needs. Restoring goes through the shell's
/// Recycle Bin folder (Shell.Application), as .NET has no managed way to it.
/// </summary>
public static class RecycleBin
{
    // ShellSpecialFolderConstants.ssfBITBUCKET.
    private const int RecycleBinFolder = 10;
    private const string RestoreVerb = "undelete";
    private static readonly TimeSpan RestoreWait = TimeSpan.FromSeconds(5);
    private static readonly TimeSpan RestorePoll = TimeSpan.FromMilliseconds(100);

    /// <summary>Whether Windows' shell is there to restore from (it is not on a server core or in some containers).</summary>
    public static bool IsAvailable => Type.GetTypeFromProgID("Shell.Application") is not null;

    /// <summary>
    /// Sends a local file or folder to the Recycle Bin. The shell shows its own message if one cannot go,
    /// as Explorer would, and the error is passed on.
    /// </summary>
    public static void Send(string path, bool isFolder)
    {
        if (isFolder)
        {
            FileSystem.DeleteDirectory(path, UIOption.OnlyErrorDialogs, RecycleOption.SendToRecycleBin);
        }
        else
        {
            FileSystem.DeleteFile(path, UIOption.OnlyErrorDialogs, RecycleOption.SendToRecycleBin);
        }
    }

    /// <summary>
    /// Restores the newest deletion of <paramref name="originalPath"/> from the Recycle Bin. Returns a
    /// problem in words, or null. Nothing is restored over a file that took its place.
    /// </summary>
    public static string? Restore(string originalPath)
    {
        string name = Path.GetFileName(originalPath);
        if (File.Exists(originalPath) || Directory.Exists(originalPath))
        {
            return $"{name} was not restored: something else is at {originalPath} now";
        }
        try
        {
            return OnShellThread(() => RestoreNewest(originalPath));
        }
        catch (Exception error) when (error is COMException or TargetInvocationException or InvalidCastException
            or MissingMethodException or InvalidComObjectException)
        {
            return $"{name} could not be restored from the Recycle Bin: {(error.InnerException ?? error).Message}";
        }
    }

    private static string? RestoreNewest(string originalPath)
    {
        string name = Path.GetFileName(originalPath);
        if (Type.GetTypeFromProgID("Shell.Application") is not Type shellType || Activator.CreateInstance(shellType) is not object shell)
        {
            return $"{name} could not be restored: Windows' shell is not available";
        }
        object? newest = null;
        DateTime newestAt = DateTime.MinValue;
        object? bin = Call(shell, "NameSpace", RecycleBinFolder);
        object? items = Call(bin, "Items");
        int count = (int)Get(items, "Count")!;
        for (int i = 0; i < count; i++)
        {
            object? item = Call(items, "Item", i);
            if (item is null || !IsDeletionOf(bin, item, originalPath))
            {
                continue;
            }
            DateTime at = Call(item, "ExtendedProperty", "System.Recycle.DateDeleted") as DateTime? ?? DateTime.MinValue;
            if (newest is null || at > newestAt)
            {
                newest = item;
                newestAt = at;
            }
        }
        if (newest is null)
        {
            return $"{name} is not in the Recycle Bin any more";
        }
        Call(newest, "InvokeVerb", RestoreVerb);
        return WaitFor(originalPath) ? null : $"{name} could not be restored from the Recycle Bin";
    }

    /// <summary>
    /// Whether a Recycle Bin item was deleted from this path. Its shown name can leave the extension out
    /// (Explorer's "hide extensions"); the file it is kept as in the Recycle Bin has the original's.
    /// </summary>
    private static bool IsDeletionOf(object? bin, object item, string originalPath)
    {
        string from = Call(item, "ExtendedProperty", "System.Recycle.DeletedFrom") as string
            ?? Call(bin, "GetDetailsOf", item, 1) as string
            ?? "";
        string folder = Path.GetDirectoryName(originalPath) ?? "";
        if (!string.Equals(Path.TrimEndingDirectorySeparator(from), Path.TrimEndingDirectorySeparator(folder), StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }
        string shown = Get(item, "Name") as string ?? "";
        string kept = Get(item, "Path") as string ?? "";
        string name = Path.GetFileName(originalPath);
        if (string.Equals(shown, name, StringComparison.OrdinalIgnoreCase))
        {
            return true;
        }
        return string.Equals(shown, Path.GetFileNameWithoutExtension(name), StringComparison.OrdinalIgnoreCase)
            && string.Equals(Path.GetExtension(kept), Path.GetExtension(name), StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>The shell restores on its own time: waits a little for the file to be back.</summary>
    private static bool WaitFor(string path)
    {
        DateTime until = DateTime.UtcNow + RestoreWait;
        while (!File.Exists(path) && !Directory.Exists(path))
        {
            if (DateTime.UtcNow > until)
            {
                return false;
            }
            Thread.Sleep(RestorePoll);
        }
        return true;
    }

    private static object? Call(object? target, string method, params object?[] args) =>
        target?.GetType().InvokeMember(method, BindingFlags.InvokeMethod, null, target, args);

    private static object? Get(object? target, string property) =>
        target?.GetType().InvokeMember(property, BindingFlags.GetProperty, null, target, null);

    /// <summary>Shell objects belong on a single-threaded apartment; the work runs on one of its own and its error comes back here.</summary>
    private static T OnShellThread<T>(Func<T> work)
    {
        if (Thread.CurrentThread.GetApartmentState() == ApartmentState.STA)
        {
            return work();
        }
        T result = default!;
        ExceptionDispatchInfo? failed = null;
        var thread = new Thread(() =>
        {
            try
            {
                result = work();
            }
            catch (Exception error)
            {
                // Not handled here: rethrown on the calling thread below, where the caller catches what it knows.
                failed = ExceptionDispatchInfo.Capture(error);
            }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.IsBackground = true;
        thread.Start();
        thread.Join();
        failed?.Throw();
        return result;
    }
}
