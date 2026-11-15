using System.Collections.Concurrent;
using System.IO.Enumeration;
using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;

namespace Files.Core;

/// <summary>What a walk passed over, so a search that ends early says why.</summary>
/// <param name="Unreadable">Folders that could not be listed (no access, deleted meanwhile, name too long).</param>
/// <param name="Links">Folder links (junctions, DFS links, mount points) that were not followed.</param>
/// <param name="TooDeep">Folders below <see cref="Walker.MaxDepth"/>, which were not read.</param>
public sealed record WalkSkips(int Unreadable, int Links, int TooDeep)
{
    public static WalkSkips None { get; } = new(0, 0, 0);

    public WalkSkips Plus(WalkSkips other) => new(Unreadable + other.Unreadable, Links + other.Links, TooDeep + other.TooDeep);

    /// <summary>The skips in words, or null when nothing was skipped.</summary>
    public string? Describe()
    {
        var parts = new List<string>();
        if (Unreadable > 0)
        {
            parts.Add(Unreadable == 1 ? "1 folder could not be read" : $"{Unreadable:N0} folders could not be read");
        }
        if (Links > 0)
        {
            parts.Add(Links == 1 ? "1 link not followed" : $"{Links:N0} links not followed");
        }
        if (TooDeep > 0)
        {
            parts.Add(TooDeep == 1 ? "1 folder too deep" : $"{TooDeep:N0} folders too deep");
        }
        return parts.Count == 0 ? null : string.Join(" · ", parts);
    }
}

/// <param name="Truncated">The walk stopped because the names no longer fit in memory.</param>
public sealed record WalkResult(VolumeIndex Index, int Folders, WalkSkips Skipped, bool Truncated);

/// <summary>
/// Reads every file name under a root with several folders listed at once.
/// On a network share each listing is mostly waiting for the server, so the
/// worker count is above the core count.
/// </summary>
public static class Walker
{
    public const int Workers = 8;
    public const int MaxDepth = 32;

    /// <summary>
    /// After this many folders in a row fail with network errors, the root is
    /// listed again: if it still reads, the walk goes on (a broken corner of a
    /// share), otherwise the share is treated as gone. Folders the user may not
    /// read do not count, so a share full of restricted folders is still read.
    /// </summary>
    public const int MaxConsecutiveFailures = 32;

    private const int BufferBytes = 64 * 1024;

    /// <summary>
    /// Memory a walk needs per character of names, as a safe estimate: the
    /// listed string, both arenas, the copy made while an arena grows, and
    /// per-name overhead, at two bytes a character.
    /// </summary>
    private const int BytesPerNameChar = 16;

    /// <param name="First">Read before the rest of the tree because a match is likely there (see <see cref="LiveWalkOptions.FirstFolders"/>).</param>
    private sealed record Folder(string Relative, int Depth, bool First = false);

    /// <param name="Links">Folders that are links (reparse points): junctions, symbolic links, DFS links, mount points.</param>
    private sealed record Listing(List<string> Files, List<string> Folders, List<string> Links);

    /// <summary>
    /// Walks <paramref name="root"/>. Throws when the root itself cannot be
    /// read; a subfolder that cannot be read is counted and skipped. Folder
    /// links are followed only on a network share (where DFS links and the
    /// server's mount points hold whole subtrees), each target once; on a
    /// local disk they are counted and skipped.
    /// There is no fixed file count: the walk stops only when the names would
    /// no longer fit in an arena or in memory (see <see cref="CharLimit"/>).
    /// </summary>
    public static WalkResult Walk(string root, bool skipHidden, CancellationToken cancel, Action<int>? progress = null)
    {
        return Walk(root, skipHidden, cancel, progress, CharLimit());
    }

    /// <summary>
    /// The most characters of names one walk may hold: what fits in an arena,
    /// and what this machine's memory can take, so a huge volume stops with a
    /// clear message instead of running out of memory.
    /// </summary>
    public static long CharLimit()
    {
        long available = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes;
        return Math.Min(Names.MaxChars, available / BytesPerNameChar);
    }

    /// <summary>
    /// Walks <paramref name="root"/> for a live search: stores only the files
    /// <paramref name="keep"/> returns for each folder, and skips folders where it
    /// keeps none. Each stored folder is also handed to <paramref name="onFolder"/>
    /// as soon as it is read, so a caller can show results before the walk ends.
    /// Folder links are never followed (a DFS link can lead to another server, and
    /// even asking where it leads waits on that server); they are only counted.
    /// </summary>
    public static WalkResult Walk(
        string root,
        bool skipHidden,
        CancellationToken cancel,
        Func<string, List<string>, List<string>> keep,
        Action<string, IReadOnlyList<string>> onFolder,
        LiveWalkOptions? live = null)
    {
        return Walk(root, skipHidden, cancel, progress: null, CharLimit(), keep, onFolder, followLinks: false, live);
    }

    /// <param name="followLinks">Whether to read folder links; by default, only on a network share.</param>
    /// <param name="live">How many folders to list at once, which to read first, and where to report progress.</param>
    internal static WalkResult Walk(
        string root,
        bool skipHidden,
        CancellationToken cancel,
        Action<int>? progress,
        long maxChars,
        Func<string, List<string>, List<string>>? keep = null,
        Action<string, IReadOnlyList<string>>? onFolder = null,
        bool? followLinks = null,
        LiveWalkOptions? live = null)
    {
        live ??= LiveWalkOptions.Default;
        bool follow = followLinks ?? IsOnNetwork(root);
        // Link targets already read, by volume serial and file id, so a loop or two links to one place are read once.
        var linkTargets = new ConcurrentDictionary<(uint Volume, ulong File), byte>();
        // The links of a listing that are read like folders; the others are skipped.
        List<string> LinksToFollow(string relative, Listing listing) => follow
            ? listing.Links.Where(link => FolderId(Path.Join(root, relative, link)) is { } id && linkTargets.TryAdd(id, 0)).ToList()
            : [];

        Listing rootListing = Filter(List(root, skipHidden), "", keep);
        onFolder?.Invoke("", rootListing.Files);
        var found = new List<(string Relative, List<string> Files)> { ("", rootListing.Files) };
        List<string> rootLinks = LinksToFollow("", rootListing);
        var pending = new Stack<Folder>(rootListing.Folders.Concat(rootLinks).Select(name => new Folder(name, 1)));
        // Folders to read first are all taken before the rest; what they hold goes on the stack with everything else.
        // The folders read so far keep each to one reading.
        var firstPending = new Queue<Folder>(FirstFoldersOf(live.FirstFolders));
        HashSet<string>? read = firstPending.Count > 0 ? new HashSet<string>(StringComparer.OrdinalIgnoreCase) { "" } : null;
        // Whether each folder on the way to a first folder is a plain folder, not a link, by its path relative to the root.
        var plainFolders = new ConcurrentDictionary<string, bool>(StringComparer.OrdinalIgnoreCase);
        bool PassesThroughLink(string relative)
        {
            string[] segments = relative.Split('\\');
            for (int depth = 1; depth <= segments.Length; depth++)
            {
                string prefix = string.Join('\\', segments[..depth]);
                if (!plainFolders.GetOrAdd(prefix, at => IsPlainFolder(Path.Join(root, at))))
                {
                    return true;
                }
            }
            return false;
        }
        int folders = 1;
        int busy = 0;
        int entries = rootListing.Files.Count;
        long fileChars = CharsOf(rootListing.Files);
        long folderChars = 1;
        int unreadable = 0;
        int linksSkipped = rootListing.Links.Count - rootLinks.Count;
        int tooDeep = 0;
        int failuresInARow = 0;
        bool checkingShare = false;
        bool shareLost = false;
        bool truncated = false;
        // Set when Walk has returned: a worker still listing when it was cancelled then reports nothing more.
        bool stopped = false;
        Exception? failure = null;
        var gate = new object();

        // Called under the gate, after each folder.
        void ReportProgress() => live.Progress?.Report(folders, new WalkSkips(unreadable, linksSkipped, tooDeep));
        ReportProgress();

        // The next folder to read, or null when the walk is over; called under the gate.
        Folder? Take()
        {
            while (true)
            {
                while (firstPending.Count + pending.Count == 0 && busy > 0 && !cancel.IsCancellationRequested)
                {
                    Monitor.Wait(gate);
                }
                bool stop = cancel.IsCancellationRequested || shareLost || failure is not null;
                if (firstPending.Count + pending.Count == 0 || stop)
                {
                    Monitor.PulseAll(gate);
                    return null;
                }
                Folder next = firstPending.Count > 0 ? firstPending.Dequeue() : pending.Pop();
                if (read is null || read.Add(next.Relative))
                {
                    busy++;
                    return next;
                }
            }
        }

        void Work()
        {
            while (true)
            {
                Folder? taken;
                lock (gate)
                {
                    taken = Take();
                }
                if (taken is not Folder folder)
                {
                    return;
                }

                Listing? listing = null;
                List<string> links = [];
                bool lostContact = false;
                bool missing = false;
                Exception? crash = null;
                try
                {
                    live.Progress?.Started(folder.Relative);
                    // The saved list was read following links: a first folder may lie behind one, which a live walk never enters.
                    missing = folder.First && PassesThroughLink(folder.Relative);
                    listing = missing ? null : TryList(Path.Join(root, folder.Relative), skipHidden, out lostContact, out missing);
                    listing = listing is null ? null : Filter(listing, folder.Relative, keep);
                    links = listing is not null && folder.Depth < MaxDepth ? LinksToFollow(folder.Relative, listing) : [];
                }
                catch (Exception error)
                {
                    // Anything unexpected (out of memory, a failing filter) ends the walk
                    // and is rethrown by Walk; letting it escape would end the process.
                    crash = error;
                }
                bool store = listing is not null && (keep is null || listing.Files.Count > 0);
                int? report = null;
                bool checkShare = false;

                lock (gate)
                {
                    busy--;
                    if (stopped)
                    {
                        Monitor.PulseAll(gate);
                        return;
                    }
                    if (crash is not null)
                    {
                        failure ??= crash;
                    }
                    else if (listing is null && folder.First && missing)
                    {
                        // A folder the saved list knew that is gone, or behind a link: passed over, not counted.
                    }
                    else if (listing is null)
                    {
                        folders++;
                        unreadable++;
                        failuresInARow = lostContact ? failuresInARow + 1 : failuresInARow;
                        // One worker at a time checks whether the share is really gone; the others carry on meanwhile.
                        checkShare = lostContact && failuresInARow >= MaxConsecutiveFailures && !checkingShare && !shareLost;
                        checkingShare |= checkShare;
                    }
                    else if (fileChars + CharsOf(listing.Files) > maxChars || folderChars + folder.Relative.Length + 1 > maxChars)
                    {
                        folders++;
                        truncated = true;
                        store = false;
                    }
                    else
                    {
                        folders++;
                        failuresInARow = 0;
                        if (store)
                        {
                            entries += listing.Files.Count;
                            fileChars += CharsOf(listing.Files);
                            folderChars += folder.Relative.Length + 1;
                            found.Add((folder.Relative, listing.Files));
                        }
                        if (folder.Depth < MaxDepth)
                        {
                            foreach (string child in listing.Folders.Concat(links))
                            {
                                pending.Push(new Folder(Path.Join(folder.Relative, child), folder.Depth + 1));
                            }
                            linksSkipped += listing.Links.Count - links.Count;
                        }
                        else
                        {
                            tooDeep += listing.Folders.Count + listing.Links.Count;
                        }
                        report = entries;
                    }
                    ReportProgress();
                    Monitor.PulseAll(gate);
                }
                if (checkShare)
                {
                    // A run of failures can be one broken corner of a share that is still there.
                    bool reachable = TryList(root, skipHidden, out _, out _) is not null;
                    lock (gate)
                    {
                        checkingShare = false;
                        // Lost only if nothing was read while the root was tried; once lost, it stays lost.
                        shareLost |= !reachable && failuresInARow >= MaxConsecutiveFailures;
                        failuresInARow = reachable ? 0 : failuresInARow;
                        Monitor.PulseAll(gate);
                    }
                }
                // Reported outside the lock, so a slow listener never holds up the other workers.
                try
                {
                    if (report is int count)
                    {
                        progress?.Invoke(count);
                    }
                    if (store)
                    {
                        onFolder?.Invoke(folder.Relative, listing!.Files);
                    }
                }
                catch (Exception error)
                {
                    lock (gate)
                    {
                        failure ??= error;
                        Monitor.PulseAll(gate);
                    }
                }
            }
        }

        int workers = Math.Max(1, live.Workers);
        // Not disposed: after a cancel, a worker still waiting on a slow server signals it when its call returns.
        var done = new CountdownEvent(workers);
        void Run()
        {
            try
            {
                Work();
            }
            finally
            {
                done.Signal();
            }
        }
        // A cancel wakes the idle workers, and the walk returns at once instead of waiting on folders still being listed:
        // those workers are background threads and stop when their listing returns.
        try
        {
            using (cancel.Register(() =>
            {
                lock (gate)
                {
                    Monitor.PulseAll(gate);
                }
            }))
            {
                for (int i = 0; i < workers; i++)
                {
                    new Thread(Run) { IsBackground = true }.Start();
                }
                done.Wait(cancel);
            }
        }
        finally
        {
            lock (gate)
            {
                stopped = true;
            }
        }
        // The last worker can finish just as the cancel comes: a cancelled walk never returns what it had read so far.
        cancel.ThrowIfCancellationRequested();
        if (failure is not null)
        {
            ExceptionDispatchInfo.Throw(failure);
        }
        if (shareLost)
        {
            throw new IOException($"{MaxConsecutiveFailures} folders in a row could not be read, and neither could the volume itself");
        }

        // Sorted so the same tree always gives the same index.
        found.Sort((a, b) => string.CompareOrdinal(a.Relative, b.Relative));
        var builder = new VolumeIndex.Builder(root);
        foreach (var (relative, files) in found)
        {
            builder.AddFolder(relative, files);
        }
        return new WalkResult(builder.Build(), found.Count, new WalkSkips(unreadable, linksSkipped, tooDeep), truncated);
    }

    /// <summary>A UNC path, or a drive Windows reports as a network drive.</summary>
    internal static bool IsOnNetwork(string root)
    {
        if (root.StartsWith(@"\\", StringComparison.Ordinal))
        {
            return true;
        }
        try
        {
            return new DriveInfo(root).DriveType == DriveType.Network;
        }
        catch (Exception error) when (error is IOException or ArgumentException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    /// <summary>
    /// Whether a failed listing looks like the share going away (the network or
    /// the server) rather than one folder that cannot be read: a path too long,
    /// a name the server refuses, a broken link.
    /// </summary>
    internal static bool LooksLikeLostContact(IOException error)
    {
        const int PathNotFound = unchecked((int)0x80070003);
        const int InvalidName = unchecked((int)0x8007007B);
        const int FilenameTooLong = unchecked((int)0x800700CE);
        const int CantAccessFile = unchecked((int)0x80070780);
        return error is not PathTooLongException
            && error.HResult is not (PathNotFound or InvalidName or FilenameTooLong or CantAccessFile);
    }

    /// <summary>
    /// The folders to read first, as folders below the root: the root itself, paths
    /// that leave the root, and folders past <see cref="MaxDepth"/> are left out.
    /// </summary>
    private static IEnumerable<Folder> FirstFoldersOf(IReadOnlyList<string> relatives)
    {
        foreach (string relative in relatives)
        {
            string trimmed = relative.Trim('\\', '/');
            string[] segments = trimmed.Split('\\', '/');
            bool inside = trimmed.Length > 0 && !Path.IsPathRooted(trimmed) && !segments.Any(segment => segment is "" or "." or "..");
            if (inside && segments.Length <= MaxDepth)
            {
                yield return new Folder(string.Join('\\', segments), segments.Length, First: true);
            }
        }
    }

    private static Listing Filter(Listing listing, string relative, Func<string, List<string>, List<string>>? keep)
    {
        return keep is null ? listing : listing with { Files = keep(relative, listing.Files) };
    }

    /// <summary>The arena space a folder's names take, one separator each.</summary>
    private static long CharsOf(List<string> names)
    {
        long chars = 0;
        foreach (string name in names)
        {
            chars += name.Length + 1;
        }
        return chars;
    }

    /// <summary>
    /// Lists one folder, or returns null when it cannot be read.
    /// <paramref name="lostContact"/> says whether that looks like the share
    /// going away, rather than a folder that is restricted or was just deleted.
    /// </summary>
    /// <param name="missing">The folder is not there (deleted, or its parent is).</param>
    private static Listing? TryList(string path, bool skipHidden, out bool lostContact, out bool missing)
    {
        lostContact = false;
        missing = false;
        try
        {
            return List(path, skipHidden);
        }
        catch (DirectoryNotFoundException)
        {
            missing = true;
            return null;
        }
        catch (Exception error) when (error is UnauthorizedAccessException
            or System.Security.SecurityException or ArgumentException)
        {
            return null;
        }
        catch (IOException error)
        {
            lostContact = LooksLikeLostContact(error);
            return null;
        }
    }

    private static Listing List(string path, bool skipHidden)
    {
        var options = new EnumerationOptions
        {
            AttributesToSkip = 0,
            IgnoreInaccessible = false,
            RecurseSubdirectories = false,
            BufferSize = BufferBytes,
        };
        var entries = new FileSystemEnumerable<(string Name, FileAttributes Attributes)>(
            path,
            (ref FileSystemEntry entry) => (entry.FileName.ToString(), entry.Attributes),
            options);

        var listing = new Listing([], [], []);
        foreach (var (name, attributes) in entries)
        {
            if (attributes.HasFlag(FileAttributes.Directory))
            {
                // Hidden and system folders are still walked; links only when the walk follows them.
                (attributes.HasFlag(FileAttributes.ReparsePoint) ? listing.Links : listing.Folders).Add(name);
            }
            else if (!skipHidden || (attributes & (FileAttributes.Hidden | FileAttributes.System)) == 0)
            {
                listing.Files.Add(name);
            }
        }
        listing.Files.Sort(string.CompareOrdinal);
        return listing;
    }

    /// <summary>
    /// The volume serial and file id of the folder a link leads to, or null when it
    /// cannot be opened or the server gives no file id (some report 0), so the link
    /// is counted as not followed rather than risk reading one place twice.
    /// .NET has no managed way to ask for a folder's file id.
    /// </summary>
    private static (uint Volume, ulong File)? FolderId(string path)
    {
        const uint BackupSemantics = 0x02000000;
        const uint OpenExisting = 3;
        IntPtr handle = CreateFileW(LongPath(path), 0, FileShare.ReadWrite | FileShare.Delete, IntPtr.Zero, OpenExisting, BackupSemantics, IntPtr.Zero);
        if (handle == new IntPtr(-1))
        {
            return null;
        }
        try
        {
            if (!GetFileInformationByHandle(handle, out FileInformation info))
            {
                return null;
            }
            ulong file = ((ulong)info.FileIndexHigh << 32) | info.FileIndexLow;
            return file == 0 ? null : (info.VolumeSerialNumber, file);
        }
        finally
        {
            CloseHandle(handle);
        }
    }

    /// <summary>
    /// Whether <paramref name="path"/> is a folder and not a link, as its parent lists it:
    /// asking for the entry by name reads the parent's listing, so a link is never entered.
    /// False when it cannot be found.
    /// </summary>
    private static bool IsPlainFolder(string path)
    {
        const int InfoBasic = 1;
        const int NameMatch = 0;
        IntPtr find = FindFirstFileExW(LongPath(path), InfoBasic, out FindData data, NameMatch, IntPtr.Zero, 0);
        if (find == new IntPtr(-1))
        {
            return false;
        }
        FindClose(find);
        var attributes = (FileAttributes)data.Attributes;
        return attributes.HasFlag(FileAttributes.Directory) && !attributes.HasFlag(FileAttributes.ReparsePoint);
    }

    /// <summary>The path with the long-path prefix past MAX_PATH, which the Windows API needs and .NET's own calls add by themselves.</summary>
    private static string LongPath(string path)
    {
        const int MaxPath = 260;
        return path.Length < MaxPath ? path
            : path.StartsWith(@"\\", StringComparison.Ordinal) ? @"\\?\UNC\" + path[2..] : @"\\?\" + path;
    }

    /// <summary>WIN32_FIND_DATAW.</summary>
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct FindData
    {
        public uint Attributes;
        public uint CreationLow, CreationHigh, AccessLow, AccessHigh, WriteLow, WriteHigh;
        public uint SizeHigh;
        public uint SizeLow;
        public uint Reserved0;
        public uint Reserved1;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
        public string FileName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 14)]
        public string AlternateFileName;
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr FindFirstFileExW(string name, int infoLevel, out FindData data, int searchOp, IntPtr filter, uint flags);

    [DllImport("kernel32.dll")]
    private static extern bool FindClose(IntPtr find);

    /// <summary>BY_HANDLE_FILE_INFORMATION; each FILETIME is two 32-bit halves.</summary>
    [StructLayout(LayoutKind.Sequential)]
    private struct FileInformation
    {
        public uint Attributes;
        public uint CreationLow, CreationHigh, AccessLow, AccessHigh, WriteLow, WriteHigh;
        public uint VolumeSerialNumber;
        public uint SizeHigh;
        public uint SizeLow;
        public uint Links;
        public uint FileIndexHigh;
        public uint FileIndexLow;
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateFileW(string name, uint access, FileShare share, IntPtr security, uint disposition, uint flags, IntPtr template);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool GetFileInformationByHandle(IntPtr handle, out FileInformation info);

    [DllImport("kernel32.dll")]
    private static extern bool CloseHandle(IntPtr handle);
}
