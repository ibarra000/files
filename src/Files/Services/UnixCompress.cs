namespace Files.Services;

/// <summary>
/// Expands data in the Unix <c>compress</c> (.Z, LZW) format, which Creo uses for some saved previews.
/// A port of Mark Adler's decoder in pigz (unlzw), bounded for untrusted input: it stops at the end of
/// the input, at the first code that cannot be valid, or once <c>maxOutput</c> bytes are written.
/// What follows the compressed data in a file decodes as noise; the caller finds where its content ends.
/// </summary>
internal static class UnixCompress
{
    public static ReadOnlySpan<byte> Signature => [0x1F, 0x9D];

    /// <summary>The expanded bytes, up to where the input ends or stops making sense; null when it is not this format.</summary>
    public static byte[]? Expand(ReadOnlySpan<byte> input, int maxOutput)
    {
        if (input.Length < 3 || !input.StartsWith(Signature) || (input[2] & 0x60) != 0)
        {
            return null;
        }
        int max = input[2] & 0x1F;
        if (max is < 9 or > 16)
        {
            return null;
        }
        if (max == 9)
        {
            // As compress writes it, 9 means 10.
            max = 10;
        }
        bool block = (input[2] & 0x80) != 0;
        var reader = new Reader(input[3..]);
        var output = new List<byte>(Math.Min(maxOutput, 64 * 1024));
        var prefix = new ushort[65536];
        var suffix = new byte[65536];
        var match = new byte[65536];

        int bits = 9;
        int mask = 0x1FF;
        int end = block ? 256 : 255;
        if (!reader.Next(out byte first))
        {
            return [];
        }
        int prev = first;
        int final = first;
        if (!reader.Next(out byte second) || (second & 1) != 0)
        {
            return [first];
        }
        int rem = second >> 1;
        int left = 7;
        // The first code took two bytes of a group of `bits` bytes.
        int chunk = bits - 2;
        output.Add(first);

        while (output.Count < maxOutput)
        {
            if (end >= mask && bits < max)
            {
                // Codes come in groups of `bits` bytes; a wider code starts a new group.
                reader.Skip(chunk);
                chunk = 0;
                left = 0;
                rem = 0;
                bits++;
                mask = (mask << 1) | 1;
            }
            if (chunk == 0)
            {
                chunk = bits;
            }
            int code = rem;
            if (!reader.Next(out byte last))
            {
                break;
            }
            code += last << left;
            left += 8;
            chunk--;
            if (bits > left)
            {
                if (!reader.Next(out last))
                {
                    break;
                }
                code += last << left;
                left += 8;
                chunk--;
            }
            code &= mask;
            left -= bits;
            rem = last >> (8 - left);

            if (code == 256 && block)
            {
                reader.Skip(chunk);
                chunk = 0;
                left = 0;
                rem = 0;
                bits = 9;
                mask = 0x1FF;
                end = 255;
                continue;
            }

            int stack = 0;
            int current = code;
            if (code > end)
            {
                // Only the code about to be made may be used early, and only after a valid one.
                if (code != end + 1 || prev > end)
                {
                    break;
                }
                match[stack++] = (byte)final;
                code = prev;
            }
            while (code >= 256)
            {
                match[stack++] = suffix[code];
                code = prefix[code];
            }
            match[stack++] = (byte)code;
            final = code;
            if (end < mask)
            {
                end++;
                prefix[end] = (ushort)prev;
                suffix[end] = (byte)final;
            }
            prev = current;
            while (stack > 0 && output.Count < maxOutput)
            {
                output.Add(match[--stack]);
            }
        }
        return [.. output];
    }

    private ref struct Reader(ReadOnlySpan<byte> data)
    {
        private readonly ReadOnlySpan<byte> _data = data;
        private int _at;

        public bool Next(out byte value)
        {
            if (_at >= _data.Length)
            {
                value = 0;
                return false;
            }
            value = _data[_at++];
            return true;
        }

        public void Skip(int count) => _at = Math.Min(_data.Length, _at + Math.Max(0, count));
    }
}
