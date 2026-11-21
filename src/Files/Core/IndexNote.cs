using System.Collections.Immutable;

namespace Files.Core;

/// <summary>A path the panel put somewhere, and whether it is a folder.</summary>
public sealed record NotedPath(string Path, bool IsFolder);

/// <summary>
/// What the panel copied, moved, renamed or deleted in one volume since that volume was last read. The
/// saved lists only change with a read, so until then search results leave out what is gone and add
/// what was put there. Immutable: each change gives a new note.
/// </summary>
public sealed class IndexNote
{
    private readonly ImmutableDictionary<string, DateTime> _removed;
    private readonly ImmutableDictionary<string, (DateTime At, bool IsFolder)> _added;

    private IndexNote(ImmutableDictionary<string, DateTime> removed, ImmutableDictionary<string, (DateTime, bool)> added)
    {
        _removed = removed;
        _added = added;
    }

    public static IndexNote Empty { get; } = new(
        ImmutableDictionary.Create<string, DateTime>(StringComparer.OrdinalIgnoreCase),
        ImmutableDictionary.Create<string, (DateTime, bool)>(StringComparer.OrdinalIgnoreCase));

    public bool IsEmpty => _removed.IsEmpty && _added.IsEmpty;

    /// <summary>This note with more changes, made at <paramref name="at"/>. A path put back where it was is no longer removed.</summary>
    public IndexNote With(IEnumerable<string> removed, IEnumerable<NotedPath> added, DateTime at)
    {
        ImmutableDictionary<string, DateTime> gone = _removed;
        ImmutableDictionary<string, (DateTime, bool)> put = _added;
        foreach (string path in removed)
        {
            // What was put inside a folder goes with it.
            put = put.RemoveRange(put.Keys.Where(key => IsUnder(path, key)));
            gone = gone.SetItem(path, at);
        }
        foreach (NotedPath path in added)
        {
            gone = gone.Remove(path.Path);
            put = put.SetItem(path.Path, (at, path.IsFolder));
        }
        return new IndexNote(gone, put);
    }

    /// <summary>The changes made at or after <paramref name="started"/>, which a read that started then may have missed.</summary>
    public IndexNote Since(DateTime started) => new(
        _removed.RemoveRange(_removed.Where(entry => entry.Value < started).Select(entry => entry.Key)),
        _added.RemoveRange(_added.Where(entry => entry.Value.At < started).Select(entry => entry.Key)));

    /// <summary>Whether a result at <paramref name="path"/> is gone: it, or a folder it was in, was moved, renamed or deleted.</summary>
    public bool Hides(string path)
    {
        if (_removed.IsEmpty || _added.ContainsKey(path))
        {
            return false;
        }
        for (string? at = path; at is not null; at = Path.GetDirectoryName(at))
        {
            if (_removed.ContainsKey(at))
            {
                return true;
            }
        }
        return false;
    }

    /// <summary>A small index of what was put in the volume at <paramref name="root"/>, to search beside its saved list; null if nothing was.</summary>
    public VolumeIndex? AddedIndex(string root)
    {
        // By folder relative to the root, in order, so the root ("") comes first as the builder needs.
        var byFolder = new SortedDictionary<string, List<string>>(StringComparer.Ordinal);
        foreach ((string path, (DateTime _, bool isFolder)) in _added)
        {
            string? folder = isFolder ? path : Path.GetDirectoryName(path);
            if (folder is null || !IsUnder(root, folder))
            {
                continue;
            }
            string relative = folder.TrimEnd('\\')[root.TrimEnd('\\').Length..].Trim('\\');
            if (!byFolder.TryGetValue(relative, out List<string>? files))
            {
                files = [];
                byFolder[relative] = files;
            }
            if (!isFolder)
            {
                files.Add(Path.GetFileName(path));
            }
        }
        if (byFolder.Count == 0)
        {
            return null;
        }
        var builder = new VolumeIndex.Builder(root);
        foreach ((string relative, List<string> files) in byFolder)
        {
            builder.AddFolder(relative, files);
        }
        return builder.Build();
    }

    /// <summary>A search's results without what the notes say is gone, and each path once (a file put back is in both lists).</summary>
    public static SearchResult Apply(SearchResult result, IReadOnlyCollection<IndexNote> notes)
    {
        if (notes.All(note => note.IsEmpty))
        {
            return result;
        }
        return result with { Hits = Kept(result.Hits, notes), Folders = Kept(result.Folders, notes) };
    }

    /// <summary>
    /// Searches the volumes' saved lists together with what the notes say was put in them, then leaves
    /// out what is gone.
    /// </summary>
    public static SearchResult Search(IReadOnlyList<(string Name, string Root, VolumeIndex Index, IndexNote Note)> volumes,
        Query query, IReadOnlySet<string> hiddenExtensions, CancellationToken cancel)
    {
        var indexes = new List<(string Name, VolumeIndex Index)>();
        foreach (var (name, root, index, note) in volumes)
        {
            indexes.Add((name, index));
            if (note.AddedIndex(root) is VolumeIndex added)
            {
                indexes.Add((name, added));
            }
        }
        SearchResult found = Searcher.Search(indexes, query, hiddenExtensions, cancel);
        return Apply(found, volumes.Select(volume => volume.Note).ToList());
    }

    /// <summary>Whether <paramref name="path"/> is <paramref name="folder"/> or inside it, ignoring case.</summary>
    public static bool IsUnder(string folder, string path)
    {
        // A drive root ends with its separator ("C:\"), a folder does not: either way it is checked after the trim.
        string root = folder.TrimEnd('\\');
        return path.StartsWith(root, StringComparison.OrdinalIgnoreCase)
            && (path.Length == root.Length || path[root.Length] == '\\');
    }

    private static List<Hit> Kept(IReadOnlyList<Hit> hits, IReadOnlyCollection<IndexNote> notes)
    {
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        return hits.Where(hit => !notes.Any(note => note.Hides(hit.Path)) && seen.Add(hit.Path)).ToList();
    }
}
