namespace Files.Services;

/// <summary>
/// The preview Creo saves inside its parts, assemblies and drawings: a JPEG (256 pixels square) after a
/// <c>#THMB_IMG_MAIN</c> marker near the end of the file, stored as it is or compressed with Unix compress.
/// Read here, a Creo file is previewed without Creo installed, and assemblies too, which Creo's own thumbnail
/// handler does not draw. The file is untrusted: the JPEG's end is found by walking its segments, each
/// checked against what was read.
/// </summary>
public static class CreoThumbnail
{
    private static ReadOnlySpan<byte> Header => "#UGC:"u8;
    private static ReadOnlySpan<byte> Marker => "#THMB_IMG_MAIN"u8;
    private static ReadOnlySpan<byte> StartOfImage => [0xFF, 0xD8, 0xFF];

    /// <summary>How much is read at a time while looking for the marker.</summary>
    internal const int ChunkBytes = 1024 * 1024;

    /// <summary>
    /// How much of the end of the file is searched. Creo writes the preview within a few hundred KB of the
    /// end; an old save has none, and reading all of a large one over a share for each selection would be slow.
    /// </summary>
    internal const long TailBytes = 8L * 1024 * 1024;

    /// <summary>How far after the marker the JPEG may start.</summary>
    private const int MaxGap = 512;

    /// <summary>A 256-pixel preview is tens of KB.</summary>
    private const int MaxImageBytes = 1024 * 1024;

    /// <summary>Whether a file is Creo's: each starts with "#UGC:".</summary>
    public static bool IsCreoFile(string path)
    {
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
        return IsCreoFile(stream);
    }

    /// <summary>The saved preview as a JPEG file's bytes, or null when the file is not Creo's or has none.</summary>
    public static byte[]? Read(string path, CancellationToken cancel)
    {
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite);
        if (!IsCreoFile(stream))
        {
            return null;
        }
        long length = stream.Length;
        if (FindLast(stream, Math.Max(0, length - TailBytes), length, cancel) is not long at)
        {
            return null;
        }
        long after = at + Marker.Length;
        stream.Position = after;
        var window = new byte[(int)Math.Min(MaxGap + MaxImageBytes, length - after)];
        int read = stream.ReadAtLeast(window, window.Length, throwOnEndOfStream: false);
        return Picture(window.AsSpan(0, read));
    }

    /// <summary>The JPEG at the start of a preview section: stored as it is, or compressed (Unix compress).</summary>
    internal static byte[]? Picture(ReadOnlySpan<byte> section)
    {
        ReadOnlySpan<byte> near = section[..Math.Min(section.Length, MaxGap + StartOfImage.Length)];
        int plain = near.IndexOf(StartOfImage);
        int packed = near.IndexOf(UnixCompress.Signature);
        if (packed >= 0 && (plain < 0 || packed < plain))
        {
            byte[]? expanded = UnixCompress.Expand(section[packed..], MaxImageBytes);
            if (expanded is not null && expanded.AsSpan().StartsWith(StartOfImage) && JpegEnd(expanded, 0) is int length)
            {
                return expanded[..length];
            }
            // Those two bytes were not a compressed picture; a stored one may still follow.
        }
        return plain >= 0 && JpegEnd(section, plain) is int end ? section[plain..end].ToArray() : null;
    }

    private static bool IsCreoFile(FileStream stream)
    {
        Span<byte> head = stackalloc byte[Header.Length];
        return stream.ReadAtLeast(head, head.Length, throwOnEndOfStream: false) == head.Length && head.SequenceEqual(Header);
    }

    /// <summary>
    /// Where the last marker between two offsets starts, read a chunk at a time from the end; null when there is none.
    /// The last one, as Creo writes the preview near the end and an earlier match may be model data.
    /// </summary>
    private static long? FindLast(FileStream stream, long from, long to, CancellationToken cancel)
    {
        var chunk = new byte[(int)Math.Min(ChunkBytes, to - from)];
        // Each read reaches into the start of the one after it, so a marker split between them is found.
        int overlap = Marker.Length - 1;
        for (long end = to; end > from; end -= chunk.Length - overlap)
        {
            cancel.ThrowIfCancellationRequested();
            long start = Math.Max(from, end - chunk.Length);
            int wanted = (int)(end - start);
            stream.Position = start;
            int read = stream.ReadAtLeast(chunk.AsSpan(0, wanted), wanted, throwOnEndOfStream: false);
            int found = chunk.AsSpan(0, read).LastIndexOf(Marker);
            if (found >= 0)
            {
                return start + found;
            }
            if (start == from || chunk.Length <= overlap)
            {
                break;
            }
        }
        return null;
    }

    /// <summary>
    /// Where the JPEG starting at <paramref name="start"/> ends (just past its end-of-image marker), found by
    /// stepping over its segments and scan data; null when it runs past what was read or is not a JPEG.
    /// </summary>
    internal static int? JpegEnd(ReadOnlySpan<byte> data, int start)
    {
        int at = start + 2;
        while (true)
        {
            if (at + 1 >= data.Length || data[at] != 0xFF)
            {
                return null;
            }
            // Fill bytes: any number of 0xFF before a marker's code.
            while (at + 1 < data.Length && data[at + 1] == 0xFF)
            {
                at++;
            }
            if (at + 1 >= data.Length)
            {
                return null;
            }
            byte code = data[at + 1];
            at += 2;
            if (code == 0xD9)
            {
                return at;
            }
            if (code is 0x01 or (>= 0xD0 and <= 0xD7))
            {
                // A marker with no segment.
                continue;
            }
            if (at + 2 > data.Length)
            {
                return null;
            }
            int size = (data[at] << 8) | data[at + 1];
            if (size < 2 || at + size > data.Length)
            {
                return null;
            }
            at += size;
            if (code == 0xDA && SkipScan(data, at) is int next)
            {
                at = next;
            }
            else if (code == 0xDA)
            {
                return null;
            }
        }
    }

    /// <summary>Past a scan's coded data to the next marker: a 0xFF there is followed by 0x00, or is a restart marker.</summary>
    private static int? SkipScan(ReadOnlySpan<byte> data, int at)
    {
        for (; at + 1 < data.Length; at++)
        {
            if (data[at] == 0xFF && data[at + 1] != 0x00 && data[at + 1] is not (>= 0xD0 and <= 0xD7))
            {
                return at;
            }
        }
        return null;
    }
}
