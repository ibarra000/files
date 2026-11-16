using System.Collections.Concurrent;
using System.Diagnostics;

namespace Files.Core;

/// <summary>
/// How far a walk has got, readable from any thread while it runs: folders
/// finished (read or not), what was passed over so far, the folder a worker
/// started last, and the folders being listed now with how long each has
/// waited, so a live search can show it is moving, or what it is waiting on.
/// </summary>
public sealed class WalkProgress
{
    private readonly ConcurrentDictionary<string, long> _inFlight = new(StringComparer.OrdinalIgnoreCase);
    private Counts _counts = new(0, WalkSkips.None);
    private string? _reading;

    /// <summary>Called on the reading thread as each folder is done with: for tracing a slow walk.</summary>
    public Action<FolderListed>? Listed { get; init; }

    /// <summary>Folders finished so far, the root included, whether they could be read or not.</summary>
    public int Folders => Volatile.Read(ref _counts).Folders;

    /// <summary>What the walk has passed over so far.</summary>
    public WalkSkips Skipped => Volatile.Read(ref _counts).Skipped;

    /// <summary>The folder, relative to the root, a worker started reading last; null before the first one.</summary>
    public string? Reading => Volatile.Read(ref _reading);

    /// <summary>The folders being listed now, relative to the root, longest waiting first.</summary>
    public IReadOnlyList<(string Relative, TimeSpan Waited)> InFlight
    {
        get
        {
            long now = Stopwatch.GetTimestamp();
            return _inFlight
                .Select(folder => (folder.Key, Stopwatch.GetElapsedTime(folder.Value, now)))
                .OrderByDescending(folder => folder.Item2)
                .ToList();
        }
    }

    /// <summary>A folder is being read; one already in flight keeps the time it started.</summary>
    internal void Started(string relative)
    {
        _inFlight.TryAdd(relative, Stopwatch.GetTimestamp());
        Volatile.Write(ref _reading, relative);
    }

    /// <param name="entries">The names the folder held; 0 when it could not be read.</param>
    /// <param name="error">Why it could not be read, or null.</param>
    internal void Finished(string relative, int entries, Exception? error)
    {
        if (_inFlight.TryRemove(relative, out long started))
        {
            try
            {
                Listed?.Invoke(new FolderListed(relative, Stopwatch.GetElapsedTime(started), entries, error));
            }
            catch (Exception)
            {
                // Only a trace listens here; a failing one must not end the walk it is watching.
            }
        }
    }

    internal void Report(int folders, WalkSkips skipped) => Volatile.Write(ref _counts, new Counts(folders, skipped));

    private sealed record Counts(int Folders, WalkSkips Skipped);
}

/// <summary>One folder a walk is done with: how long its listing took, the names it held, and why it could not be read.</summary>
public sealed record FolderListed(string Relative, TimeSpan Took, int Entries, Exception? Error);

/// <summary>How a live walk reads, beyond what it keeps.</summary>
/// <param name="Workers">How many folders are listed at once.</param>
/// <param name="FirstFolders">
/// Folders, relative to the root, read before the rest of the tree (where a match
/// is likely). One that no longer exists is passed over without being counted.
/// Each folder is still read only once.
/// </param>
/// <param name="Progress">Where the walk reports how far it has got; null for none.</param>
public sealed record LiveWalkOptions(int Workers, IReadOnlyList<string> FirstFolders, WalkProgress? Progress)
{
    /// <summary>Names listed before a folder's listing hands them to the walk; a huge folder is walked while it lists.</summary>
    public const int DefaultStreamEvery = 2_000;

    public static LiveWalkOptions Default { get; } = new(Walker.Workers, [], null);

    /// <inheritdoc cref="DefaultStreamEvery"/>
    internal int StreamEvery { get; init; } = DefaultStreamEvery;

    /// <summary>For tests: called with each folder, relative to the root, just before it is listed; it can wait or throw as a slow or failing server would.</summary>
    internal Action<string>? BeforeList { get; init; }
}
