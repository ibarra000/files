namespace Files.Core;

/// <summary>
/// How far a walk has got, readable from any thread while it runs: folders
/// finished (read or not), what was passed over so far, and the folder a
/// worker started last, so a live search can show it is moving even when
/// nothing matches.
/// </summary>
public sealed class WalkProgress
{
    private Counts _counts = new(0, WalkSkips.None);
    private string? _reading;

    /// <summary>Folders finished so far, the root included, whether they could be read or not.</summary>
    public int Folders => Volatile.Read(ref _counts).Folders;

    /// <summary>What the walk has passed over so far.</summary>
    public WalkSkips Skipped => Volatile.Read(ref _counts).Skipped;

    /// <summary>The folder, relative to the root, a worker started reading last; null before the first one.</summary>
    public string? Reading => Volatile.Read(ref _reading);

    internal void Started(string relative) => Volatile.Write(ref _reading, relative);

    internal void Report(int folders, WalkSkips skipped) => Volatile.Write(ref _counts, new Counts(folders, skipped));

    private sealed record Counts(int Folders, WalkSkips Skipped);
}

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
    public static LiveWalkOptions Default { get; } = new(Walker.Workers, [], null);
}
