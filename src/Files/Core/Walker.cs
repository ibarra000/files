using System.IO.Enumeration;
using System.Runtime.ExceptionServices;

namespace Files.Core;

/// <param name="Truncated">The walk stopped because the names no longer fit in memory.</param>
public sealed record WalkResult(VolumeIndex Index, int Folders, int Unreadable, bool Truncated);

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
    /// After this many folders in a row fail with I/O errors, the share is
    /// treated as gone. Folders the user may not read do not count, so a share
    /// full of restricted folders is still read.
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

    private sealed record Listing(List<string> Files, List<string> Folders);

    /// <summary>
    /// Walks <paramref name="root"/>. Throws when the root itself cannot be
    /// read; a subfolder that cannot be read is counted and skipped. Folder
    /// junctions and other reparse points are never followed.
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

    internal static WalkResult Walk(
        string root,
        bool skipHidden,
        CancellationToken cancel,
        Action<int>? progress,
        long maxChars,
        Func<string, List<string>, List<string>>? keep = null,
        Action<string, IReadOnlyList<string>>? onFolder = null)
    {
        Listing rootListing = Filter(List(root, skipHidden), "", keep);
        onFolder?.Invoke("", rootListing.Files);
        var found = new List<(string Relative, List<string> Files)> { ("", rootListing.Files) };
        var pending = new Stack<Folder>(rootListing.Folders.Select(name => new Folder(name, 1)));
        int busy = 0;
        int entries = rootListing.Files.Count;
        long fileChars = CharsOf(rootListing.Files);
        long folderChars = 1;
        int unreadable = 0;
        int failuresInARow = 0;
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
                    bool stop = cancel.IsCancellationRequested || failuresInARow >= MaxConsecutiveFailures || failure is not null;
                    if (pending.Count == 0 || stop)
                    {
                        Monitor.PulseAll(gate);
                        return;
                    }
                    folder = pending.Pop();
                    busy++;
                }

                Listing? listing = null;
                bool lostContact = false;
                Exception? crash = null;
                try
                {
                    listing = TryList(Path.Join(root, folder.Relative), skipHidden, out lostContact);
                    listing = listing is null ? null : Filter(listing, folder.Relative, keep);
                }
                catch (Exception error)
                {
                    // Anything unexpected (out of memory, a failing filter) ends the walk
                    // and is rethrown by Walk; letting it escape would end the process.
                    crash = error;
                }
                bool store = listing is not null && (keep is null || listing.Files.Count > 0);
                int? report = null;

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
                            foreach (string child in listing.Folders)
                            {
                                pending.Push(new Folder(Path.Join(folder.Relative, child), folder.Depth + 1));
                            }
                        }
                        report = entries;
                    }
                    Monitor.PulseAll(gate);
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
        if (failuresInARow >= MaxConsecutiveFailures)
        {
            throw new IOException($"{MaxConsecutiveFailures} folders in a row could not be read");
        }

        // Sorted so the same tree always gives the same index.
        found.Sort((a, b) => string.CompareOrdinal(a.Relative, b.Relative));
        var builder = new VolumeIndex.Builder(root);
        foreach (var (relative, files) in found)
        {
            builder.AddFolder(relative, files);
        }
        return new WalkResult(builder.Build(), found.Count, unreadable, truncated);
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
        catch (IOException)
        {
            lostContact = true;
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

        var listing = new Listing([], []);
        foreach (var (name, attributes) in entries)
        {
            if (attributes.HasFlag(FileAttributes.Directory))
            {
                // Hidden and system folders are still walked; junctions never are.
                if (!attributes.HasFlag(FileAttributes.ReparsePoint))
                {
                    listing.Folders.Add(name);
                }
            }
            else if (!skipHidden || (attributes & (FileAttributes.Hidden | FileAttributes.System)) == 0)
            {
                listing.Files.Add(name);
            }
        }
        listing.Files.Sort(string.CompareOrdinal);
        return listing;
    }
}
