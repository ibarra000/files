using System.Buffers.Binary;
using System.Reflection;
using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using Microsoft.VisualBasic.FileIO;

namespace Files.Services;

/// <summary>
/// Sends files to the Recycle Bin and brings them back. Sending uses the shell's own delete (through
/// Microsoft.VisualBasic), which keeps what Explorer's Restore needs. Finding and restoring go through the
/// shell's Recycle Bin folder (Shell.Application), as .NET has no managed way to it.
/// </summary>
public static class RecycleBin
{
    // ShellSpecialFolderConstants.ssfBITBUCKET.
    private const int RecycleBinFolder = 10;
    private const string RestoreVerb = "undelete";
    private const int MaxLinks = 16;
    private static readonly TimeSpan RestoreWait = TimeSpan.FromSeconds(5);
    private static readonly TimeSpan RestorePoll = TimeSpan.FromMilliseconds(100);

    // The shell keeps the deletion time to the second, in local time; a little slack either side.
    private static readonly TimeSpan Slack = TimeSpan.FromSeconds(2);

    /// <summary>Whether Windows' shell is there to restore from (it is not on a server core or in some containers).</summary>
    public static bool IsAvailable => Type.GetTypeFromProgID("Shell.Application") is not null;

    /// <summary>
    /// Whether <paramref name="path"/> is on a local fixed disk, which has a Recycle Bin. A share, a mapped or
    /// removable drive, a drive made with subst, and a folder linked to any of those do not: Windows would
    /// delete from them for good. Following links touches each folder above the path once.
    /// </summary>
    public static bool HasBin(string path)
    {
        try
        {
            string? real = RealPath(path);
            if (real is null || real.StartsWith(@"\\", StringComparison.Ordinal))
            {
                return false;
            }
            return Path.GetPathRoot(real) is string root && new DriveInfo(root).DriveType == DriveType.Fixed;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentException)
        {
            // Unsure means no Recycle Bin: the panel then asks before deleting.
            return false;
        }
    }

    /// <summary>
    /// Sends a local file or folder to the Recycle Bin and says whether it is there now. The shell asks
    /// before deleting anything for good (too big for the bin, the bin turned off), and asks the user's
    /// usual question, if they have one; saying no throws <see cref="OperationCanceledException"/>.
    /// </summary>
    public static bool Send(string path, bool isFolder)
    {
        DateTime since = DateTime.UtcNow;
        long? size = isFolder ? null : new FileInfo(path).Length;
        if (isFolder)
        {
            FileSystem.DeleteDirectory(path, UIOption.AllDialogs, RecycleOption.SendToRecycleBin);
        }
        else
        {
            FileSystem.DeleteFile(path, UIOption.AllDialogs, RecycleOption.SendToRecycleBin);
        }
        try
        {
            return OnShellThread(() => Find(path, since, DateTime.UtcNow, size) is not null);
        }
        catch (Exception error) when (error is COMException or TargetInvocationException or InvalidCastException
            or MissingMethodException or InvalidComObjectException)
        {
            // Not found means not restorable: it is reported as deleted for good.
            return false;
        }
    }

    /// <summary>
    /// Restores <paramref name="originalPath"/> from the Recycle Bin: the newest item deleted from that path
    /// between <paramref name="fromUtc"/> and <paramref name="toUtc"/>, of <paramref name="size"/> if given.
    /// Returns a problem in words, or null. Nothing is restored over a file that took its place.
    /// </summary>
    public static string? Restore(string originalPath, DateTime fromUtc, DateTime toUtc, long? size)
    {
        string name = Path.GetFileName(originalPath);
        if (File.Exists(originalPath) || Directory.Exists(originalPath))
        {
            return $"{name} was not restored: something else is at {originalPath} now";
        }
        try
        {
            return OnShellThread(() =>
            {
                if (Find(originalPath, fromUtc, toUtc, size) is not object item)
                {
                    return $"{name} is not in the Recycle Bin any more";
                }
                Call(item, "InvokeVerb", RestoreVerb);
                return WaitFor(originalPath) ? null : $"{name} could not be restored from the Recycle Bin";
            });
        }
        catch (Exception error) when (error is COMException or TargetInvocationException or InvalidCastException
            or MissingMethodException or InvalidComObjectException)
        {
            return $"{name} could not be restored from the Recycle Bin: {(error.InnerException ?? error).Message}";
        }
    }

    /// <summary>The newest Recycle Bin item deleted from this path in this time (and of this size), or null.</summary>
    private static object? Find(string originalPath, DateTime fromUtc, DateTime toUtc, long? size)
    {
        if (Type.GetTypeFromProgID("Shell.Application") is not Type shellType || Activator.CreateInstance(shellType) is not object shell)
        {
            return null;
        }
        object? bin = Call(shell, "NameSpace", RecycleBinFolder);
        object? items = Call(bin, "Items");
        int count = (int)Get(items, "Count")!;
        object? newest = null;
        DateTime newestAt = DateTime.MinValue;
        for (int i = 0; i < count; i++)
        {
            object? item = Call(items, "Item", i);
            if (item is null || !IsDeletionOf(bin, item, originalPath) || DeletionRecord(item) is not var (at, keptSize))
            {
                continue;
            }
            bool inTime = at >= fromUtc - Slack && at <= toUtc + Slack;
            bool sameSize = size is null || keptSize == size;
            if (inTime && sameSize && (newest is null || at > newestAt))
            {
                newest = item;
                newestAt = at;
            }
        }
        return newest;
    }

    /// <summary>
    /// When the item was deleted (UTC) and its size, as Windows wrote them in its "$I" record: the item is
    /// kept as "$R…" and the record as "$I…" beside it, with the size at byte 8 and the time at byte 16.
    /// Null if the record cannot be read, and the item is then never taken.
    /// </summary>
    private static (DateTime At, long Size)? DeletionRecord(object item)
    {
        const int RecordStart = 24;
        if (Get(item, "Path") is not string kept || Path.GetFileName(kept) is not { Length: > 2 } name
            || !name.StartsWith("$R", StringComparison.OrdinalIgnoreCase))
        {
            return null;
        }
        string record = Path.Combine(Path.GetDirectoryName(kept)!, "$I" + name[2..]);
        try
        {
            using FileStream stream = File.OpenRead(record);
            Span<byte> head = stackalloc byte[RecordStart];
            stream.ReadExactly(head);
            long size = BinaryPrimitives.ReadInt64LittleEndian(head[8..16]);
            long deleted = BinaryPrimitives.ReadInt64LittleEndian(head[16..24]);
            return (DateTime.FromFileTimeUtc(deleted), size);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentOutOfRangeException)
        {
            return null;
        }
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

    /// <summary>
    /// Where <paramref name="path"/> really is: through subst drives, and through any linked folder above it
    /// (the item itself may be a link: deleting a link removes only the link). Null if links go round too often.
    /// </summary>
    private static string? RealPath(string path)
    {
        string current = Path.GetFullPath(path);
        for (int hops = 0; hops < MaxLinks; hops++)
        {
            string? next = SubstTarget(current) ?? ThroughLink(current);
            if (next is null)
            {
                return current;
            }
            current = next;
        }
        return null;
    }

    /// <summary>The path on the folder a subst drive stands for, or null if the drive is not one.</summary>
    private static string? SubstTarget(string path)
    {
        const string SubstPrefix = @"\??\";
        if (path.Length < 2 || path[1] != ':')
        {
            return null;
        }
        var target = new char[1024];
        if (QueryDosDeviceW(path[..2], target, target.Length) == 0)
        {
            return null;
        }
        string device = new string(target).Split('\0')[0];
        if (!device.StartsWith(SubstPrefix, StringComparison.Ordinal))
        {
            return null;
        }
        string folder = device[SubstPrefix.Length..];
        // A subst of a share shows as \??\UNC\server\share.
        if (folder.StartsWith(@"UNC\", StringComparison.OrdinalIgnoreCase))
        {
            folder = @"\" + folder[3..];
        }
        return Path.Join(folder, path[2..]);
    }

    /// <summary>The path through the nearest linked folder above it, or null if there is none.</summary>
    private static string? ThroughLink(string path)
    {
        for (string? folder = Path.GetDirectoryName(path); folder is not null; folder = Path.GetDirectoryName(folder))
        {
            var info = new DirectoryInfo(folder);
            if (!info.Exists || !info.Attributes.HasFlag(FileAttributes.ReparsePoint))
            {
                continue;
            }
            FileSystemInfo target = info.ResolveLinkTarget(returnFinalTarget: true)
                ?? throw new IOException($"{folder} is a link that cannot be followed");
            return Path.Join(target.FullName, Path.GetRelativePath(folder, path));
        }
        return null;
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

    // .NET has no way to tell a subst drive from the disk it stands for.
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint QueryDosDeviceW(string deviceName, char[] targetPath, int max);
}
