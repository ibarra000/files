using System.Collections.Concurrent;

namespace Files.Core;

/// <summary>A volume a live search reads, with its saved list when it has one (used to read likely folders first).</summary>
public sealed record LiveVolume(string Name, string Root, VolumeIndex? Saved = null)
{
    public static implicit operator LiveVolume((string Name, string Root) volume) => new(volume.Name, volume.Root);
}

/// <summary>
/// Searches volumes by reading them now instead of using their saved lists,
/// so files added since the last scan are found. Only matching names are
/// kept, so memory stays small however large the volumes are. Results can be
/// taken with <see cref="Snapshot"/> at any time while the search runs.
/// </summary>
/// <remarks>
/// Every volume is read at once. Folders where a match is likely are read
/// first: the typed text as a path under the root, and the folders the saved
/// list found matches in. Folder links are never followed, so a search stays
/// inside the volume chosen.
/// </remarks>
public sealed class LiveSearch
{
    /// <summary>
    /// The most names kept. A short term on a large share can match millions;
    /// past this the rest are only counted, so memory and each snapshot stay small.
    /// </summary>
    public const int MaxKept = 100_000;

    /// <summary>The most folders listed at once on one share: a listing mostly waits for the server, so many run side by side.</summary>
    public const int NetworkWorkers = 32;

    /// <summary>The folders listed at once shared out over the shares of one search; each share still gets at least <see cref="Walker.Workers"/>.</summary>
    public const int TotalNetworkWorkers = 64;

    /// <summary>The most files kept from a matching folder that do not match by name, as indexed search shows.</summary>
    private const int MaxInheritedPerFolder = 64;

    /// <summary>The most matching folders remembered per volume; the panel ranks and shows far fewer.</summary>
    private const int MaxMatchingFolders = 1000;

    /// <summary>How long a folder is listed before the footer names it as the one waited on.</summary>
    public static readonly TimeSpan SlowFolder = TimeSpan.FromSeconds(5);

    /// <summary>The most folders read first per volume.</summary>
    private const int MaxFirstFolders = 200;

    /// <summary>The most folders found by their own name in the saved list that are read first.</summary>
    private const int SavedFolderHits = 50;

    private readonly IReadOnlyList<LiveVolume> _volumes;
    private readonly Query _query;
    private readonly IReadOnlySet<string> _hiddenExtensions;
    private readonly NameFilter _names;
    private readonly bool _skipHidden;
    private readonly int _maxKept;
    private readonly object _gate = new();
    private readonly List<(string Relative, IReadOnlyList<string> Files)>[] _found;
    private readonly List<string> _problems = [];
    private readonly WalkProgress[] _progress;
    private readonly bool[] _finished;

    /// <summary>Folders whose own name matches, kept even when they hold no matching file (the walk only keeps those that do).</summary>
    private readonly List<string>[] _matchingFolders;

    /// <summary>
    /// Per volume, the folders whose path matches, with the files kept from each so far that do not
    /// match by name: a huge folder is kept batch by batch, and its limit is for the whole folder.
    /// </summary>
    private readonly ConcurrentDictionary<string, int>[] _inheritedSoFar;

    // Goes up with every folder kept, so a snapshot of nothing new is the one before. Guarded by _gate.
    private int _version;
    private (int Version, int NotKept, SearchResult Result)? _lastSnapshot;
    private int _kept;
    private int _notKept;

    public LiveSearch(IReadOnlyList<LiveVolume> volumes, Query query, IReadOnlySet<string> hiddenExtensions, bool skipHidden, int maxKept = MaxKept)
    {
        _volumes = volumes;
        _query = query;
        _hiddenExtensions = hiddenExtensions;
        _names = new NameFilter(query, hiddenExtensions);
        _skipHidden = skipHidden;
        _maxKept = maxKept;
        _found = volumes.Select(_ => new List<(string, IReadOnlyList<string>)>()).ToArray();
        _matchingFolders = volumes.Select(_ => new List<string>()).ToArray();
        _inheritedSoFar = volumes.Select(_ => new ConcurrentDictionary<string, int>(StringComparer.Ordinal)).ToArray();
        _progress = volumes.Select(_ => new WalkProgress()).ToArray();
        _finished = new bool[volumes.Count];
    }

    /// <summary>For tests: names listed before a folder's listing hands them on (see <see cref="LiveWalkOptions.StreamEvery"/>).</summary>
    internal int StreamEvery { get; init; } = LiveWalkOptions.DefaultStreamEvery;

    /// <summary>Folders finished so far over every volume, whether they could be read or not.</summary>
    public int FoldersRead => _progress.Sum(progress => progress.Folders);

    /// <summary>More names matched than <see cref="MaxKept"/>; only the first ones found are ranked.</summary>
    public bool IsCapped => Volatile.Read(ref _notKept) > 0;

    /// <summary>What the walks have passed over so far, added up over every volume.</summary>
    public WalkSkips Skipped => _progress.Aggregate(WalkSkips.None, (sum, progress) => sum.Plus(progress.Skipped));

    /// <summary>A folder being read now, as the volume name and its path in the volume; null when none is.</summary>
    public string? Reading
    {
        get
        {
            for (int volume = 0; volume < _volumes.Count; volume++)
            {
                if (!Volatile.Read(ref _finished[volume]) && _progress[volume].Reading is string relative)
                {
                    return Path.Join(_volumes[volume].Name, relative);
                }
            }
            return null;
        }
    }

    /// <summary>
    /// The folder listed longest, as the volume name and its path in the volume, once it has
    /// waited <see cref="SlowFolder"/> or more: a server slow to answer; null when none has.
    /// </summary>
    public (string Path, TimeSpan Waited)? Waiting
    {
        get
        {
            (string Path, TimeSpan Waited)? longest = null;
            for (int volume = 0; volume < _volumes.Count; volume++)
            {
                if (Volatile.Read(ref _finished[volume]) || _progress[volume].InFlight is not [var (relative, waited), ..])
                {
                    continue;
                }
                if (waited >= SlowFolder && (longest is null || waited > longest.Value.Waited))
                {
                    longest = (Path.Join(_volumes[volume].Name, relative), waited);
                }
            }
            return longest;
        }
    }

    /// <summary>Volumes that could not be read, in words.</summary>
    public IReadOnlyList<string> Problems
    {
        get
        {
            lock (_gate)
            {
                return _problems.ToList();
            }
        }
    }

    /// <summary>Reads every volume at once. A volume that cannot be read is noted in <see cref="Problems"/>.</summary>
    public async Task RunAsync(CancellationToken cancel)
    {
        if (!_query.IsValid)
        {
            return;
        }
        cancel.ThrowIfCancellationRequested();
        bool[] onNetwork = _volumes.Select(volume => Walker.IsOnNetwork(volume.Root)).ToArray();
        int shares = Math.Max(1, onNetwork.Count(network => network));
        int shareWorkers = Math.Clamp(TotalNetworkWorkers / shares, Walker.Workers, NetworkWorkers);
        // Each walk waits on its own reading threads, so each gets a thread of its own rather than one from the pool.
        IEnumerable<Task> reads = Enumerable.Range(0, _volumes.Count).Select(volume => Task.Factory.StartNew(
            () => Read(volume, onNetwork[volume] ? shareWorkers : Walker.Workers, cancel),
            cancel,
            TaskCreationOptions.LongRunning,
            TaskScheduler.Default));
        // A cancel ends the search at once, even while a read waits on a server that does not answer
        // (a root or a first folder being checked); that read stops by itself when the server replies.
        await Task.WhenAll(reads).WaitAsync(cancel);
    }

    /// <summary>The matches found so far, ranked as indexed search ranks them; the same result while nothing new was found.</summary>
    public SearchResult Snapshot(CancellationToken cancel = default)
    {
        int notKept = Volatile.Read(ref _notKept);
        int version;
        var copies = new List<List<(string Relative, IReadOnlyList<string> Files)>>();
        lock (_gate)
        {
            if (_lastSnapshot is { } last && last.Version == _version && last.NotKept == notKept)
            {
                return last.Result;
            }
            version = _version;
            for (int volume = 0; volume < _volumes.Count; volume++)
            {
                // A folder whose own name matches but that holds no match is listed with no files.
                List<(string Relative, IReadOnlyList<string> Files)> folders = [.. _found[volume]];
                folders.AddRange(_matchingFolders[volume].Select(relative => (relative, (IReadOnlyList<string>)[])));
                copies.Add(folders);
            }
        }
        var indexes = new List<(string Name, VolumeIndex Index)>();
        for (int volume = 0; volume < _volumes.Count; volume++)
        {
            // A huge folder comes in several batches: each folder once, its batches together.
            List<(string Relative, List<string> Files)> folders = Walker.Merge(copies[volume]);
            var builder = new VolumeIndex.Builder(_volumes[volume].Root);
            foreach (var (relative, files) in folders)
            {
                builder.AddFolder(relative, files);
            }
            indexes.Add((_volumes[volume].Name, builder.Build()));
        }
        SearchResult searched = Searcher.Search(indexes, _query, _hiddenExtensions, cancel);
        SearchResult result = searched with { Matched = searched.Matched + notKept };
        lock (_gate)
        {
            _lastSnapshot = (version, notKept, result);
        }
        return result;
    }

    private void Read(int volume, int workers, CancellationToken cancel)
    {
        LiveVolume read = _volumes[volume];
        // The root is in flight from the start: the checks below and its listing can each wait on the server.
        _progress[volume].Started("");
        try
        {
            var options = new LiveWalkOptions(workers, FirstFolders(read, cancel), _progress[volume]) { StreamEvery = StreamEvery };
            WalkResult walk = Walker.Walk(read.Root, _skipHidden, cancel, (relative, files) => Keep(volume, relative, files), (relative, files) =>
            {
                lock (_gate)
                {
                    _found[volume].Add((relative, files));
                    _version++;
                }
            }, options);
            if (walk.RootError is Exception rootError)
            {
                lock (_gate)
                {
                    _problems.Add($"{read.Name}'s top folder could not be listed, so only part of it was searched · {rootError.Message}");
                }
            }
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException
            or ArgumentException or System.Security.SecurityException)
        {
            lock (_gate)
            {
                _problems.Add($"{read.Name} could not be read · {error.Message}");
            }
        }
        finally
        {
            // The root stays in flight if the walk ended before it was listed (a cancel, a failed check).
            _progress[volume].Finished("", 0, null);
            Volatile.Write(ref _finished[volume], true);
        }
    }

    /// <summary>
    /// Where a match is likely, read before the rest: the typed text as a path
    /// under the root (one quick check), then the folders the saved list found
    /// matches in, best first.
    /// </summary>
    internal List<string> FirstFolders(LiveVolume volume, CancellationToken cancel)
    {
        var first = new List<string>();
        if (ExactFolder(volume.Root) is string exact)
        {
            first.Add(exact);
        }
        if (volume.Saved is VolumeIndex saved)
        {
            first.AddRange(SavedFolders(volume.Name, saved, cancel));
        }
        return first.Distinct(StringComparer.OrdinalIgnoreCase).Take(MaxFirstFolders).ToList();
    }

    /// <summary>
    /// The folder the typed text names under <paramref name="root"/> (itself, or
    /// the folder of the file it names), or null when it names nothing there.
    /// </summary>
    private string? ExactFolder(string root)
    {
        string term = _query.Term.Trim().Trim('\\', '/');
        bool plain = _query.IsPlain && term.Length > 0 && term.IndexOfAny(Path.GetInvalidPathChars()) < 0
            && !Path.IsPathRooted(term) && !term.Split('\\', '/').Contains("..");
        if (!plain)
        {
            return null;
        }
        string path = Path.Join(root, term);
        if (Directory.Exists(path))
        {
            return term;
        }
        return File.Exists(path) && Path.GetDirectoryName(term) is { Length: > 0 } parent ? parent : null;
    }

    /// <summary>The folders, relative to the root, of the best matches in the saved list, files first, then folders by their own name.</summary>
    private IEnumerable<string> SavedFolders(string name, VolumeIndex saved, CancellationToken cancel)
    {
        var (hits, _) = saved.Search(name, _query, _hiddenExtensions, cancel);
        IEnumerable<string> fileFolders = hits.Select(hit => hit.Folder);
        IEnumerable<string> ownFolders = saved.SearchFolders(name, _query, SavedFolderHits, cancel).Select(hit => hit.Path);
        return fileFolders.Concat(ownFolders)
            .Select(full => Path.GetRelativePath(saved.Root, full))
            .Where(relative => relative != ".");
    }

    /// <summary>Remembers a folder found by its own name, up to <see cref="MaxMatchingFolders"/> per volume.</summary>
    private void NoteMatchingFolder(int volume, string relative)
    {
        lock (_gate)
        {
            // A huge folder is kept batch by batch: it is remembered once.
            if (_matchingFolders[volume].Count < MaxMatchingFolders && !_matchingFolders[volume].Contains(relative))
            {
                _matchingFolders[volume].Add(relative);
                _version++;
            }
        }
    }

    /// <summary>
    /// Keeps the files that match by name. In a folder whose path matches,
    /// also keeps up to 64 others, which indexed search lists as inherited
    /// (indexed search counts all of them, so its match count can be higher).
    /// </summary>
    private List<string> Keep(int volume, string relative, List<string> files)
    {
        string lowerRelative = Names.Fold(relative);
        bool folderMatches = relative.Length > 0 && VolumeIndex.FolderMatch(_query, lowerRelative) >= 0;
        if (relative.Length > 0 && VolumeIndex.OwnNameMatch(_query, lowerRelative) >= 0)
        {
            NoteMatchingFolder(volume, relative);
        }
        // Files outside every path: term are dropped from the results; keeping them would use up the room.
        bool inPath = _query.PathTerms.Count == 0 || _query.AdmitsPath(lowerRelative)
            || _query.AdmitsPath(Names.Fold(Path.Join(_volumes[volume].Root, relative)));
        if (!inPath)
        {
            return [];
        }
        var kept = new List<string>();
        // The batches of one folder are kept one after another by one reader, so this count is not raced.
        int inherited = folderMatches ? _inheritedSoFar[volume].GetValueOrDefault(relative) : 0;
        foreach (string file in files)
        {
            string lower = Names.Fold(file);
            // Hidden types are dropped here too, so they neither use up the room nor add to the count.
            if (_names.Admits(lower) && (_query.Match(lower) >= 0 || (folderMatches && inherited++ < MaxInheritedPerFolder)))
            {
                kept.Add(file);
            }
        }
        if (folderMatches)
        {
            _inheritedSoFar[volume][relative] = inherited;
        }
        // Reserve room for this folder's names; whatever does not fit is only counted.
        int room = _maxKept - Interlocked.Add(ref _kept, kept.Count) + kept.Count;
        if (room < kept.Count)
        {
            int fits = Math.Max(room, 0);
            Interlocked.Add(ref _notKept, kept.Count - fits);
            kept.RemoveRange(fits, kept.Count - fits);
        }
        return kept;
    }
}
