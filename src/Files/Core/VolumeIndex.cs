namespace Files.Core;

/// <summary>One search result.</summary>
public sealed record Hit(string Volume, VolumeIndex Index, int File, string Name, string Folder, int MatchStart, int MatchLength, ulong Key)
{
    public string Path => System.IO.Path.Join(Folder, Name);

    public bool Inherited => RankKey.IsInherited(Key);

    /// <summary>A folder found by its own name: <see cref="File"/> is then its folder ordinal and <see cref="Folder"/> its parent.</summary>
    public bool IsFolder { get; init; }
}

/// <summary>The file names under one volume root, held in memory and immutable.</summary>
/// <remarks>
/// Files are grouped by folder: folder d owns files
/// [FolderFirstFile[d], FolderFirstFile[d + 1]). Folder names are paths
/// relative to the root, the root itself being "".
/// </remarks>
public sealed class VolumeIndex
{
    public const int MaxResults = 300;
    private const int ChunkEntries = 8192;
    private const int MaxFilesPerFolder = 64;

    private VolumeIndex(string root, Names files, Names folders, int[] folderFirstFile)
    {
        Root = root;
        Files = files;
        Folders = folders;
        FolderFirstFile = folderFirstFile;
        _lowerRoot = Names.Fold(root.TrimEnd('\\'));
        _folderIds = new(() =>
        {
            var ids = new Dictionary<string, int>(Folders.Count, StringComparer.OrdinalIgnoreCase);
            for (int folder = 0; folder < Folders.Count; folder++)
            {
                ids.TryAdd(Folders[folder], folder);
            }
            return ids;
        });
    }

    // Folders by relative path, built the first time a file is looked up by its path.
    private readonly Lazy<Dictionary<string, int>> _folderIds;

    /// <summary>The file at <paramref name="path"/> in this index, ignoring case, or -1.</summary>
    internal int Find(string path)
    {
        string relative = Path.GetRelativePath(Root, path);
        if (relative == ".." || relative.StartsWith(@"..\", StringComparison.Ordinal) || Path.IsPathRooted(relative)
            || !_folderIds.Value.TryGetValue(Path.GetDirectoryName(relative) ?? "", out int folder))
        {
            return -1;
        }
        string name = Path.GetFileName(relative);
        foreach (int file in FilesIn(folder))
        {
            if (Files.OrigSpan(file).Equals(name, StringComparison.OrdinalIgnoreCase))
            {
                return file;
            }
        }
        return -1;
    }

    /// <summary>The hit for the file at <paramref name="path"/> when it is in this index, matches and passes the filters; else null.</summary>
    public Hit? HitFor(string volume, Query query, IReadOnlySet<string> hiddenExtensions, string path)
    {
        // The name is tested first, so most opened files cost no lookup at all.
        string fileName = Path.GetFileName(path);
        if (query.Rank(Names.Fold(fileName), fileName, out _, out _) < 0)
        {
            return null;
        }
        int file;
        try
        {
            file = Find(path);
        }
        catch (ArgumentException)
        {
            // A path the counts file should not have held; it simply gets no boost.
            return null;
        }
        if (file < 0)
        {
            return null;
        }
        ReadOnlySpan<char> name = Files.LowerSpan(file);
        int at = query.Rank(name, Files.OrigSpan(file), out _, out MatchClass kind);
        if (at < 0 || !new Filter(this, query, hiddenExtensions).Admits(file, name))
        {
            return null;
        }
        return ToHit(volume, query, RankKey.Make(false, kind, at, name.Length, file));
    }

    // For path: terms, which may name part of the root (a share or drive).
    private readonly string _lowerRoot;

    public string Root { get; }
    public Names Files { get; }
    public Names Folders { get; }
    internal int[] FolderFirstFile { get; }

    public int FileCount => Files.Count;

    public int FolderOf(int file)
    {
        // The last folder whose first file is at or before this one.
        int lo = 0;
        int hi = Folders.Count - 1;
        while (lo < hi)
        {
            int mid = (lo + hi + 1) / 2;
            if (FolderFirstFile[mid] <= file)
            {
                lo = mid;
            }
            else
            {
                hi = mid - 1;
            }
        }
        return lo;
    }

    public string FolderPath(int folder) => Path.Join(Root, Folders[folder]);

    public IEnumerable<int> FilesIn(int folder)
    {
        for (int file = FolderFirstFile[folder]; file < FolderFirstFile[folder + 1]; file++)
        {
            yield return file;
        }
    }

    /// <summary>
    /// Returns the best <see cref="MaxResults"/> hits, ranked, and how many
    /// files matched in total. Files inside a folder whose path matches are
    /// included as "inherited" hits, ranked below direct ones.
    /// </summary>
    public (List<Hit> Hits, int Matched) Search(string volume, Query query, IReadOnlySet<string> hiddenExtensions, CancellationToken cancel)
    {
        var filter = new Filter(this, query, hiddenExtensions);
        var top = new TopK(MaxResults);
        int matched = SweepFiles(query, filter, top, cancel);
        cancel.ThrowIfCancellationRequested();
        matched += SweepFolders(query, filter, top);

        var hits = new List<Hit>();
        foreach (ulong key in top.Sorted())
        {
            hits.Add(ToHit(volume, query, key));
        }
        return (hits, matched);
    }

    /// <summary>
    /// The best <paramref name="limit"/> folders whose own name matches, ranked the way files are.
    /// A folder's subfolders do not match through it, and a type filter (<c>ext:</c>) leaves folders out.
    /// </summary>
    public List<Hit> SearchFolders(string volume, Query query, int limit, CancellationToken cancel)
    {
        var folders = new List<Hit>();
        if (query.Extensions.Count > 0)
        {
            return folders;
        }
        var top = new TopK(limit);
        // Folder 0 is the volume root, which has no name of its own.
        for (int folder = 1; folder < Folders.Count; folder++)
        {
            if ((folder & 0xFFFF) == 0)
            {
                cancel.ThrowIfCancellationRequested();
            }
            ReadOnlySpan<char> relative = Folders.LowerSpan(folder);
            int at = RankOwnName(query, folder, out _, out MatchClass kind);
            if (at >= 0 && AdmitsFolder(query, relative))
            {
                int ownLength = relative.Length - (relative.LastIndexOf('\\') + 1);
                top.Add(RankKey.Make(false, kind, at, ownLength, folder));
            }
        }
        foreach (ulong key in top.Sorted())
        {
            folders.Add(ToFolderHit(volume, query, key));
        }
        return folders;
    }

    /// <summary>Where the term matches a folder's own name (the last part of its lowercased relative path), or -1.</summary>
    internal static int OwnNameMatch(Query query, ReadOnlySpan<char> lowerRelative) => OwnNameMatch(query, lowerRelative, out _);

    private static int OwnNameMatch(Query query, ReadOnlySpan<char> lowerRelative, out int length) =>
        query.Match(lowerRelative[(lowerRelative.LastIndexOf('\\') + 1)..], out length);

    /// <summary>A folder found by its own name is listed unless that name holds an excluded word or its path misses a <c>path:</c> term.</summary>
    internal bool AdmitsFolder(Query query, ReadOnlySpan<char> lowerRelative)
    {
        ReadOnlySpan<char> own = lowerRelative[(lowerRelative.LastIndexOf('\\') + 1)..];
        return !query.HoldsExcludedWord(own) && AdmitsPath(query, lowerRelative);
    }

    /// <summary>Every <c>path:</c> term is in the folder's path below the root, or in the root itself.</summary>
    private bool AdmitsPath(Query query, ReadOnlySpan<char> lowerRelative) =>
        query.PathTerms.Count == 0
        || query.AdmitsPath(lowerRelative)
        || query.AdmitsPath(lowerRelative.Length == 0 ? _lowerRoot : $"{_lowerRoot}\\{lowerRelative}");

    private Hit ToFolderHit(string volume, Query query, ulong key)
    {
        int folder = RankKey.File(key);
        string relative = Folders[folder];
        int slash = relative.LastIndexOf('\\');
        string name = relative[(slash + 1)..];
        string parent = Path.Join(Root, slash < 0 ? "" : relative[..slash]);
        int start = RankOwnName(query, folder, out int length, out _);
        return new Hit(volume, this, folder, name, parent, start, length, key) { IsFolder = true };
    }

    /// <summary>How a folder's own name (the last part of its path) matches, ranked like a file's name.</summary>
    private int RankOwnName(Query query, int folder, out int length, out MatchClass kind)
    {
        ReadOnlySpan<char> lower = Folders.LowerSpan(folder);
        int start = lower.LastIndexOf('\\') + 1;
        return query.Rank(lower[start..], Folders.OrigSpan(folder)[start..], out length, out kind);
    }

    private int SweepFiles(Query query, Filter filter, TopK top, CancellationToken cancel)
    {
        int chunks = (Files.Count + ChunkEntries - 1) / ChunkEntries;
        int matched = 0;
        var gate = new object();
        var options = new ParallelOptions { CancellationToken = cancel };
        Parallel.For(0, chunks, options, chunk =>
        {
            int lo = chunk * ChunkEntries;
            int hi = Math.Min(lo + ChunkEntries, Files.Count);
            var local = new TopK(MaxResults);
            int count = query.Alternatives.Count == 1
                ? SweepChunk(query.Alternatives[0], filter, local, lo, hi)
                : SweepChunkAlternatives(query, filter, local, lo, hi);
            lock (gate)
            {
                matched += count;
                top.AddRange(local);
            }
        });
        return matched;
    }

    // The hot loop, kept free of allocations: most searches are one phrase.
    private int SweepChunk(Alternative alternative, Filter filter, TopK top, int lo, int hi)
    {
        ReadOnlySpan<char> arena = Files.Lower;
        int end = Files.Offsets[hi];
        int position = Files.Offsets[lo];
        int matched = 0;
        while (position < end)
        {
            int found = arena[position..end].IndexOf(alternative.Needle, StringComparison.Ordinal);
            if (found < 0)
            {
                break;
            }
            int file = Files.EntryAt(position + found);
            ReadOnlySpan<char> name = Files.LowerSpan(file);
            int at = alternative.MatchAt(name, Files.OrigSpan(file), position + found - Files.Offsets[file], out MatchClass kind);
            if (at >= 0 && filter.Admits(file, name))
            {
                matched++;
                top.Add(RankKey.Make(false, kind, at, name.Length, file));
            }
            // Each name gives at most its leftmost hit.
            position = Files.Offsets[file + 1];
        }
        return matched;
    }

    /// <summary>A search with several alternatives: each name is ranked once, by the alternative that matches earliest.</summary>
    private int SweepChunkAlternatives(Query query, Filter filter, TopK top, int lo, int hi)
    {
        var seen = new HashSet<int>();
        int matched = 0;
        foreach (Alternative alternative in query.Alternatives)
        {
            foreach ((int file, _) in Candidates(alternative.Needle, lo, hi))
            {
                if (!seen.Add(file))
                {
                    continue;
                }
                ReadOnlySpan<char> name = Files.LowerSpan(file);
                int at = query.Rank(name, Files.OrigSpan(file), out _, out MatchClass kind);
                if (at >= 0 && filter.Admits(file, name))
                {
                    matched++;
                    top.Add(RankKey.Make(false, kind, at, name.Length, file));
                }
            }
        }
        return matched;
    }

    /// <summary>The files from <paramref name="lo"/> to <paramref name="hi"/> whose name holds <paramref name="needle"/>, each once, with the leftmost place it does.</summary>
    private List<(int File, int FirstHit)> Candidates(string needle, int lo, int hi)
    {
        var found = new List<(int, int)>();
        ReadOnlySpan<char> arena = Files.Lower;
        int end = Files.Offsets[hi];
        int position = Files.Offsets[lo];
        while (position < end)
        {
            int at = arena[position..end].IndexOf(needle, StringComparison.Ordinal);
            if (at < 0)
            {
                break;
            }
            int file = Files.EntryAt(position + at);
            found.Add((file, position + at - Files.Offsets[file]));
            // Each name gives at most its leftmost hit.
            position = Files.Offsets[file + 1];
        }
        return found;
    }

    private int SweepFolders(Query query, Filter filter, TopK top)
    {
        int matched = 0;
        // Every file in a matching folder is checked: one phrase skips the loop over alternatives.
        Alternative? single = query.Alternatives.Count == 1 ? query.Alternatives[0] : null;
        for (int folder = 1; folder < Folders.Count; folder++)
        {
            int at = FolderMatch(query, Folders.LowerSpan(folder));
            if (at < 0)
            {
                continue;
            }
            int added = 0;
            foreach (int file in FilesIn(folder))
            {
                ReadOnlySpan<char> name = Files.LowerSpan(file);
                if ((single?.Match(name) ?? query.Match(name)) >= 0 || !filter.Admits(file, name))
                {
                    continue;
                }
                matched++;
                if (added++ < MaxFilesPerFolder)
                {
                    top.Add(RankKey.Make(true, at, name.Length, file));
                }
            }
        }
        return matched;
    }

    /// <summary>
    /// Contains searches match anywhere in the folder's relative path, so files
    /// in subfolders of a matching folder are found too. Prefix and suffix
    /// searches look at the folder's own name.
    /// </summary>
    internal static int FolderMatch(Query query, ReadOnlySpan<char> relative)
    {
        int best = -1;
        ReadOnlySpan<char> own = relative[(relative.LastIndexOf('\\') + 1)..];
        for (int i = 0; i < query.Alternatives.Count; i++)
        {
            Alternative alternative = query.Alternatives[i];
            int at = alternative.Mode == MatchMode.Contains
                ? relative.IndexOf(alternative.Needle, StringComparison.Ordinal)
                : alternative.Match(own);
            if (at >= 0 && (best < 0 || at < best))
            {
                best = at;
            }
        }
        return best;
    }

    private Hit ToHit(string volume, Query query, ulong key)
    {
        int file = RankKey.File(key);
        int start = -1;
        int length = 0;
        if (!RankKey.IsInherited(key))
        {
            start = query.Rank(Files.LowerSpan(file), Files.OrigSpan(file), out length, out _);
        }
        return new Hit(volume, this, file, Files[file], FolderPath(FolderOf(file)), start, length, key);
    }

    /// <summary>Whether a matching file may be listed: hidden types, then the query's types, excluded words and <c>path:</c> terms.</summary>
    private sealed class Filter(VolumeIndex index, Query query, IReadOnlySet<string> hidden)
    {
        private readonly NameFilter _names = new(query, hidden);

        // Admits runs once per matching name, up to millions of times: what does not apply is skipped.
        private readonly bool _byPath = query.PathTerms.Count > 0;

        public bool Admits(int file, ReadOnlySpan<char> lowerName) =>
            _names.Admits(lowerName)
            && (!_byPath || index.AdmitsPath(query, index.Folders.LowerSpan(index.FolderOf(file))));
    }

    /// <summary>Builds an index one folder at a time. The root folder must come first.</summary>
    public sealed class Builder(string root)
    {
        private readonly Names.Builder _files = new();
        private readonly Names.Builder _folders = new();
        private readonly List<int> _firstFile = [];

        public int FileCount => _files.Count;

        public void AddFolder(string relative, IEnumerable<string> files)
        {
            if (_folders.Count == 0 && relative.Length != 0)
            {
                AddFolder("", []);
            }
            _folders.Add(relative);
            _firstFile.Add(_files.Count);
            foreach (string file in files)
            {
                _files.Add(file);
            }
        }

        public VolumeIndex Build()
        {
            if (_folders.Count == 0)
            {
                AddFolder("", []);
            }
            int[] firstFile = [.. _firstFile, _files.Count];
            return new VolumeIndex(root, _files.Build(), _folders.Build(), firstFile);
        }
    }
}
