namespace Files.Core;

/// <summary>
/// Searches volumes by reading them now instead of using their saved lists,
/// so files added since the last scan are found. Only matching names are
/// kept, so memory stays small however large the volumes are. Results can be
/// taken with <see cref="Snapshot"/> at any time while the search runs.
/// </summary>
public sealed class LiveSearch
{
    public const int MaxVolumesAtOnce = 2;

    /// <summary>
    /// The most names kept. A short term on a large share can match millions;
    /// past this the rest are only counted, so memory and each snapshot stay small.
    /// </summary>
    public const int MaxKept = 100_000;

    /// <summary>The most files kept from a matching folder that do not match by name, as indexed search shows.</summary>
    private const int MaxInheritedPerFolder = 64;

    private readonly IReadOnlyList<(string Name, string Root)> _volumes;
    private readonly Query _query;
    private readonly IReadOnlySet<string> _hiddenExtensions;
    private readonly bool _skipHidden;
    private readonly int _maxKept;
    private readonly object _gate = new();
    private readonly List<(string Relative, IReadOnlyList<string> Files)>[] _found;
    private readonly List<string> _problems = [];
    private int _foldersRead;
    private int _kept;
    private int _notKept;

    public LiveSearch(IReadOnlyList<(string Name, string Root)> volumes, Query query, IReadOnlySet<string> hiddenExtensions, bool skipHidden, int maxKept = MaxKept)
    {
        _volumes = volumes;
        _query = query;
        _hiddenExtensions = hiddenExtensions;
        _skipHidden = skipHidden;
        _maxKept = maxKept;
        _found = volumes.Select(_ => new List<(string, IReadOnlyList<string>)>()).ToArray();
    }

    public int FoldersRead => Volatile.Read(ref _foldersRead);

    /// <summary>More names matched than <see cref="MaxKept"/>; only the first ones found are ranked.</summary>
    public bool IsCapped => Volatile.Read(ref _notKept) > 0;

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

    /// <summary>Reads every volume. A volume that cannot be read is noted in <see cref="Problems"/>.</summary>
    public async Task RunAsync(CancellationToken cancel)
    {
        if (!_query.IsValid)
        {
            return;
        }
        var options = new ParallelOptions { MaxDegreeOfParallelism = MaxVolumesAtOnce, CancellationToken = cancel };
        await Parallel.ForEachAsync(Enumerable.Range(0, _volumes.Count), options, (volume, token) =>
        {
            Read(volume, token);
            return ValueTask.CompletedTask;
        });
    }

    /// <summary>The matches found so far, ranked as indexed search ranks them.</summary>
    public SearchResult Snapshot(CancellationToken cancel = default)
    {
        var indexes = new List<(string Name, VolumeIndex Index)>();
        for (int volume = 0; volume < _volumes.Count; volume++)
        {
            List<(string Relative, IReadOnlyList<string> Files)> folders;
            lock (_gate)
            {
                folders = [.. _found[volume]];
            }
            folders.Sort((a, b) => string.CompareOrdinal(a.Relative, b.Relative));
            var builder = new VolumeIndex.Builder(_volumes[volume].Root);
            foreach (var (relative, files) in folders)
            {
                builder.AddFolder(relative, files);
            }
            indexes.Add((_volumes[volume].Name, builder.Build()));
        }
        SearchResult result = Searcher.Search(indexes, _query, _hiddenExtensions, cancel);
        return result with { Matched = result.Matched + Volatile.Read(ref _notKept) };
    }

    private void Read(int volume, CancellationToken cancel)
    {
        var (name, root) = _volumes[volume];
        try
        {
            Walker.Walk(root, _skipHidden, cancel, Keep, (relative, files) =>
            {
                lock (_gate)
                {
                    _found[volume].Add((relative, files));
                }
            });
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException
            or ArgumentException or System.Security.SecurityException)
        {
            lock (_gate)
            {
                _problems.Add($"{name} could not be read · {error.Message}");
            }
        }
    }

    /// <summary>
    /// Keeps the files that match by name. In a folder whose path matches,
    /// also keeps up to 64 others, which indexed search lists as inherited
    /// (indexed search counts all of them, so its match count can be higher).
    /// </summary>
    private List<string> Keep(string relative, List<string> files)
    {
        Interlocked.Increment(ref _foldersRead);
        bool folderMatches = relative.Length > 0 && VolumeIndex.FolderMatch(_query, Names.Fold(relative)) >= 0;
        var kept = new List<string>();
        int inherited = 0;
        foreach (string file in files)
        {
            string lower = Names.Fold(file);
            bool matches = _query.MatchAt(lower, lower.IndexOf(_query.Needle, StringComparison.Ordinal)) >= 0;
            if (matches || (folderMatches && inherited++ < MaxInheritedPerFolder))
            {
                kept.Add(file);
            }
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
