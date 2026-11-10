namespace Files.Core;

/// <summary>
/// A list of names stored in two NUL-separated character arenas: one lowercased
/// for matching, one in original case for display. One offsets table serves
/// both, because lowercasing a UTF-16 char never changes the length.
/// Name i occupies [Offsets[i], Offsets[i + 1] - 1); the char after it is NUL,
/// so a substring hit can never span two names.
/// </summary>
public sealed class Names
{
    public const char Separator = '\0';

    private Names(char[] lower, char[] orig, int[] offsets)
    {
        Lower = lower;
        Orig = orig;
        Offsets = offsets;
    }

    public static Names Empty { get; } = new([], [], [0]);

    internal char[] Lower { get; }
    internal char[] Orig { get; }
    internal int[] Offsets { get; }

    public int Count => Offsets.Length - 1;

    public string this[int i] => new(Orig, Offsets[i], Length(i));

    public int Length(int i) => Offsets[i + 1] - Offsets[i] - 1;

    internal ReadOnlySpan<char> LowerSpan(int i) => Lower.AsSpan(Offsets[i], Length(i));

    /// <summary>Lowercases one string exactly as the arenas are lowercased.</summary>
    public static string Fold(string text) => text.ToLowerInvariant();

    /// <summary>Finds the entry that contains arena position <paramref name="position"/>.</summary>
    internal int EntryAt(int position)
    {
        int found = Array.BinarySearch(Offsets, position);
        return found >= 0 ? found : ~found - 1;
    }

    public sealed class Builder
    {
        private readonly List<int> _offsets = [0];
        private char[] _orig = new char[1024];
        private int _length;

        public int Count => _offsets.Count - 1;

        public void Add(string name)
        {
            int needed = _length + name.Length + 1;
            if (needed > _orig.Length)
            {
                Array.Resize(ref _orig, Math.Max(needed, _orig.Length * 2));
            }
            name.CopyTo(_orig.AsSpan(_length));
            _length += name.Length;
            _orig[_length++] = Separator;
            _offsets.Add(_length);
        }

        public Names Build()
        {
            char[] orig = _orig[.._length];
            char[] lower = new char[_length];
            new ReadOnlySpan<char>(orig).ToLowerInvariant(lower);
            return new Names(lower, orig, [.. _offsets]);
        }
    }
}
