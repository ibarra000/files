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

    private sealed record Folder(string Relative, int Depth);

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
    /// Walks <paramref name="root"/> but stores only the files <paramref name="keep"/>
    /// returns for each folder, and skips folders where it keeps none. Each
    /// stored folder is also handed to <paramref name="onFolder"/> as soon as it
    /// is read, so a caller can show results before the walk ends.
    /// </summary>
    public static WalkResult Walk(
        string root,
        bool skipHidden,
        CancellationToken cancel,
        Func<string, List<string>, List<string>> keep,
        Action<string, IReadOnlyList<string>> onFolder)
    {
        return Walk(root, skipHidden, cancel, progress: null, CharLimit(), keep, onFolder);
    }

    /// <param name="followLinks">Whether to read folder links; by default, only on a network share.</param>
    internal static WalkResult Walk(
        string root,
        bool skipHidden,
        CancellationToken cancel,
        Action<int>? progress,
        long maxChars,
        Func<string, List<string>, List<string>>? keep = null,
        Action<string, IReadOnlyList<string>>? onFolder = null,
        bool? followLinks = null)
    {
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
        int busy = 0;
        int entries = rootListing.Files.Count;
        long fileChars = CharsOf(rootListing.Files);
        long folderChars = 1;
        int unreadable = 0;
        int linksSkipped = rootListing.Links.Count - rootLinks.Count;
        int tooDeep = 0;
        int failuresInARow = 0;
        bool shareLost = false;
        bool truncated = false;
        Exception? failure = null;
        var gate = new object();

        void Work()
        {
            while (true)
            {
                Folder folder;
                lock (gate)
                {
                    while (pending.Count == 0 && busy > 0)
                    {
                        Monitor.Wait(gate);
                    }
                    bool stop = cancel.IsCancellationRequested || shareLost || failure is not null;
                    if (pending.Count == 0 || stop)
                    {
                        Monitor.PulseAll(gate);
                        return;
                    }
                    folder = pending.Pop();
                    busy++;
                }

                Listing? listing = null;
                List<string> links = [];
                bool lostContact = false;
                Exception? crash = null;
                try
                {
                    listing = TryList(Path.Join(root, folder.Relative), skipHidden, out lostContact);
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
                    if (crash is not null)
                    {
                        failure ??= crash;
                    }
                    else if (listing is null)
                    {
                        unreadable++;
                        failuresInARow = lostContact ? failuresInARow + 1 : failuresInARow;
                        // One worker checks whether the share is really gone; the others carry on meanwhile.
                        checkShare = lostContact && failuresInARow == MaxConsecutiveFailures;
                    }
                    else if (fileChars + CharsOf(listing.Files) > maxChars || folderChars + folder.Relative.Length + 1 > maxChars)
                    {
                        truncated = true;
                        store = false;
                    }
                    else
                    {
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
                    Monitor.PulseAll(gate);
                }
                if (checkShare)
                {
                    // A run of failures can be one broken corner of a share that is still there.
                    bool reachable = TryList(root, skipHidden, out _) is not null;
                    lock (gate)
                    {
                        shareLost = !reachable;
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

        var threads = Enumerable.Range(0, Workers).Select(_ => new Thread(Work) { IsBackground = true }).ToList();
        threads.ForEach(thread => thread.Start());
        threads.ForEach(thread => thread.Join());
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
    private static Listing? TryList(string path, bool skipHidden, out bool lostContact)
    {
        lostContact = false;
        try
        {
            return List(path, skipHidden);
        }
        catch (Exception error) when (error is DirectoryNotFoundException or UnauthorizedAccessException
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
    /// The volume serial and file id of the folder a link leads to, or null when
    /// it cannot be opened. .NET has no managed way to ask for a folder's file id.
    /// </summary>
    private static (uint Volume, ulong File)? FolderId(string path)
    {
        const uint BackupSemantics = 0x02000000;
        const uint OpenExisting = 3;
        IntPtr handle = CreateFileW(path, 0, FileShare.ReadWrite | FileShare.Delete, IntPtr.Zero, OpenExisting, BackupSemantics, IntPtr.Zero);
        if (handle == new IntPtr(-1))
        {
            return null;
        }
        try
        {
            return GetFileInformationByHandle(handle, out FileInformation info)
                ? (info.VolumeSerialNumber, ((ulong)info.FileIndexHigh << 32) | info.FileIndexLow)
                : null;
        }
        finally
        {
            CloseHandle(handle);
        }
    }

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
