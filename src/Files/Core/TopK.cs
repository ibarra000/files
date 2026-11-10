namespace Files.Core;

/// <summary>
/// Ranking keys, packed into one ulong where lower is better:
/// <code>
/// bit  63      0 = the name matched, 1 = its folder did
/// bits 62..48  match position within the name   (earlier wins)
/// bits 47..32  name length                      (shorter wins)
/// bits 31..0   file ordinal                     (keeps keys unique)
/// </code>
/// The order is strict and total, so parallel and serial searches agree.
/// </summary>
public static class RankKey
{
    private const ulong InheritedBit = 1UL << 63;
    private const uint MaxPosition = 0x7FFF;
    private const uint MaxLength = 0xFFFF;

    public static ulong Make(bool inherited, int position, int length, int file)
    {
        ulong key = (ulong)Math.Min((uint)position, MaxPosition) << 48
            | (ulong)Math.Min((uint)length, MaxLength) << 32
            | (uint)file;
        return inherited ? key | InheritedBit : key;
    }

    public static bool IsInherited(ulong key) => (key & InheritedBit) != 0;

    public static int File(ulong key) => (int)(uint)key;
}

/// <summary>Keeps the <see cref="Capacity"/> lowest keys seen.</summary>
public sealed class TopK(int capacity)
{
    // A max-heap: the root is the worst key kept, the first to be replaced.
    private readonly PriorityQueue<ulong, ulong> _heap =
        new(Comparer<ulong>.Create((a, b) => b.CompareTo(a)));

    public int Capacity { get; } = capacity;

    public void Add(ulong key)
    {
        if (_heap.Count < Capacity)
        {
            _heap.Enqueue(key, key);
        }
        else if (key < _heap.Peek())
        {
            _heap.DequeueEnqueue(key, key);
        }
    }

    public void AddRange(TopK other)
    {
        foreach (var (key, _) in other._heap.UnorderedItems)
        {
            Add(key);
        }
    }

    public ulong[] Sorted()
    {
        ulong[] keys = _heap.UnorderedItems.Select(item => item.Element).ToArray();
        Array.Sort(keys);
        return keys;
    }
}
