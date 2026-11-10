namespace Files.Core;

/// <summary>One search result.</summary>
public sealed record Hit(string Volume, VolumeIndex Index, int File, string Name, string Folder, int MatchStart, int MatchLength, ulong Key)
{
    public string Path => System.IO.Path.Join(Folder, Name);

    public bool Inherited => RankKey.IsInherited(Key);
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
    }

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
        var filter = new Filter(query, hiddenExtensions);
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
            int count = SweepChunk(query, filter, local, lo, hi);
            lock (gate)
            {
                matched += count;
                top.AddRange(local);
            }
        });
        return matched;
    }

    private int SweepChunk(Query query, Filter filter, TopK top, int lo, int hi)
    {
        ReadOnlySpan<char> arena = Files.Lower;
        int end = Files.Offsets[hi];
        int position = Files.Offsets[lo];
        int matched = 0;
        while (position < end)
        {
            int found = arena[position..end].IndexOf(query.Needle, StringComparison.Ordinal);
            if (found < 0)
            {
                break;
            }
            int file = Files.EntryAt(position + found);
            ReadOnlySpan<char> name = Files.LowerSpan(file);
            int at = query.MatchAt(name, position + found - Files.Offsets[file]);
            if (at >= 0 && filter.Admits(name))
            {
                matched++;
                top.Add(RankKey.Make(false, at, name.Length, file));
            }
            // Each name gives at most its leftmost hit.
            position = Files.Offsets[file + 1];
        }
        return matched;
    }

    private int SweepFolders(Query query, Filter filter, TopK top)
    {
        int matched = 0;
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
                bool matchedByName = query.MatchAt(name, name.IndexOf(query.Needle, StringComparison.Ordinal)) >= 0;
                if (matchedByName || !filter.Admits(name))
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
    private static int FolderMatch(Query query, ReadOnlySpan<char> relative)
    {
        if (query.Mode == MatchMode.Contains)
        {
            return relative.IndexOf(query.Needle, StringComparison.Ordinal);
        }
        int start = relative.LastIndexOf('\\') + 1;
        ReadOnlySpan<char> own = relative[start..];
        return query.MatchAt(own, own.IndexOf(query.Needle, StringComparison.Ordinal));
    }

    private Hit ToHit(string volume, Query query, ulong key)
    {
        int file = RankKey.File(key);
        int start = -1;
        if (!RankKey.IsInherited(key))
        {
            ReadOnlySpan<char> name = Files.LowerSpan(file);
            start = query.MatchAt(name, name.IndexOf(query.Needle, StringComparison.Ordinal));
        }
        int length = start < 0 ? 0 : query.Needle.Length;
        return new Hit(volume, this, file, Files[file], FolderPath(FolderOf(file)), start, length, key);
    }

    private sealed class Filter(Query query, IReadOnlySet<string> hidden)
    {
        public bool Admits(ReadOnlySpan<char> lowerName)
        {
            string ext = Query.ExtensionOf(lowerName);
            if (hidden.Contains(ext))
            {
                return false;
            }
            return query.Extensions.Count == 0 || query.Extensions.Contains(ext);
        }
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
