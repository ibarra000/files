using System.Text;
using Files.Services;

namespace Files.Tests;

public sealed class UnixCompressTests
{
    /// <summary>
    /// Compresses as the compress program does: LZW codes from 9 bits wide, written low bit first, in groups
    /// of as many bytes as the code width, a group padded out when the width grows.
    /// </summary>
    internal static byte[] Compress(byte[] data, int maxBits = 16, bool block = false)
    {
        var writer = new BitWriter();
        foreach (byte b in new byte[] { 0x1F, 0x9D, (byte)(maxBits | (block ? 0x80 : 0)) })
        {
            writer.Write(b, 8);
        }
        var table = new Dictionary<int, int>();
        int free = block ? 257 : 256;
        int bits = 9;
        int maxCode = (1 << bits) - 1;
        int maxMaxCode = 1 << maxBits;
        int groupBits = 0;

        void Emit(int code)
        {
            writer.Write(code, bits);
            groupBits = (groupBits + bits) % (bits * 8);
            if (free > maxCode)
            {
                if (groupBits > 0)
                {
                    writer.Write(0, bits * 8 - groupBits);
                }
                groupBits = 0;
                bits++;
                maxCode = bits == maxBits ? maxMaxCode : (1 << bits) - 1;
            }
        }

        int entry = data[0];
        foreach (byte c in data.AsSpan(1))
        {
            int key = (entry << 8) | c;
            if (table.TryGetValue(key, out int code))
            {
                entry = code;
                continue;
            }
            Emit(entry);
            if (free < maxMaxCode)
            {
                table[key] = free++;
            }
            entry = c;
        }
        Emit(entry);
        return writer.ToArray();
    }

    private sealed class BitWriter
    {
        private readonly List<byte> _bytes = [];
        private int _bit;

        public void Write(int value, int count)
        {
            for (int i = 0; i < count; i++)
            {
                if (_bit == 0)
                {
                    _bytes.Add(0);
                }
                if (((value >> i) & 1) != 0)
                {
                    _bytes[^1] |= (byte)(1 << _bit);
                }
                _bit = (_bit + 1) % 8;
            }
        }

        public byte[] ToArray() => [.. _bytes];
    }

    /// <summary>Repetitive enough to make long codes, varied enough to fill the table.</summary>
    private static byte[] Sample(int length)
    {
        var random = new Random(11);
        var bytes = new byte[length];
        for (int i = 0; i < length; i++)
        {
            bytes[i] = (byte)(i % 7 == 0 ? random.Next(256) : 'a' + random.Next(6));
        }
        return bytes;
    }

    [Theory]
    [InlineData(16, false)]
    [InlineData(12, false)]
    [InlineData(16, true)]
    public void Compressed_data_expands_to_what_it_was(int maxBits, bool block)
    {
        byte[] data = Sample(300_000);

        byte[]? expanded = UnixCompress.Expand(Compress(data, maxBits, block), int.MaxValue);

        Assert.Equal(data, expanded);
    }

    [Fact]
    public void A_run_of_one_byte_expands_to_what_it_was()
    {
        // Each new code is used as soon as it is made, the one case that refers to a code not yet in the table.
        byte[] data = Enumerable.Repeat((byte)'a', 100_000).ToArray();

        Assert.Equal(data, UnixCompress.Expand(Compress(data), int.MaxValue));
    }

    [Fact]
    public void Expanding_stops_at_the_most_it_may_write()
    {
        byte[] data = Sample(100_000);

        byte[]? expanded = UnixCompress.Expand(Compress(data), 1000);

        Assert.Equal(data[..1000], expanded);
    }

    [Fact]
    public void Data_that_is_not_compressed_gives_none()
    {
        Assert.Null(UnixCompress.Expand(Encoding.ASCII.GetBytes("plain text"), 1000));
        Assert.Null(UnixCompress.Expand([0x1F, 0x9D, 0x05], 1000)); // fewer than 9 bits
        Assert.Null(UnixCompress.Expand([0x1F, 0x9D, 0x70], 1000)); // reserved flags
    }

    [Fact]
    public void Noise_after_the_signature_ends_without_error()
    {
        var random = new Random(5);
        var noise = new byte[50_000];
        random.NextBytes(noise);
        byte[] input = [0x1F, 0x9D, 0x10, .. noise];

        // Noise soon gives a code that cannot be valid, so it stops long before the limit.
        byte[]? expanded = UnixCompress.Expand(input, 1_000_000);

        Assert.NotNull(expanded);
        Assert.InRange(expanded!.Length, 1, 100_000);
    }
}
