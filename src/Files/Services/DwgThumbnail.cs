namespace Files.Services;

/// <summary>
/// The preview picture a CAD program saves inside a DWG drawing (R13 and later), so a
/// drawing can be previewed without a CAD library. The file says where the image section
/// is (at byte 0x0D); the section starts with a fixed sentinel and lists its images by
/// type: a bitmap (a DIB, without the BMP file header), a WMF, or, since 2013, a PNG.
/// Every offset is checked against the file, which may be damaged or not a drawing.
/// </summary>
public static class DwgThumbnail
{
    /// <summary>The 16 bytes that open the image section.</summary>
    public static ReadOnlySpan<byte> ImageSentinel =>
        [0x1F, 0x25, 0x6D, 0x07, 0xD4, 0x36, 0x28, 0x28, 0x9D, 0x57, 0xCA, 0x3F, 0x9D, 0x44, 0x10, 0x2B];

    private const int ImageAddressAt = 0x0D;
    private const byte BitmapCode = 2;
    private const byte PngCode = 6;
    private const int MaxEntries = 8;
    private const int MaxImageBytes = 16 * 1024 * 1024;

    /// <summary>The drawing's preview as a PNG or BMP file's bytes, or null when it has none that can be shown.</summary>
    public static byte[]? Read(string path)
    {
        using FileStream stream = File.OpenRead(path);
        using var reader = new BinaryReader(stream);
        long length = stream.Length;
        if (length < ImageAddressAt + 4 || !IsDrawing(reader))
        {
            return null;
        }
        stream.Position = ImageAddressAt;
        long section = reader.ReadInt32();
        int headerBytes = ImageSentinel.Length + 4 + 1;
        if (section <= 0 || section + headerBytes > length)
        {
            return null;
        }
        stream.Position = section;
        if (!reader.ReadBytes(ImageSentinel.Length).AsSpan().SequenceEqual(ImageSentinel))
        {
            return null;
        }
        reader.ReadInt32(); // the section's overall size
        int count = reader.ReadByte();
        if (count > MaxEntries || stream.Position + count * 9L > length)
        {
            return null;
        }
        var images = new List<(byte Code, long Start, int Size)>();
        for (int i = 0; i < count; i++)
        {
            images.Add((reader.ReadByte(), reader.ReadInt32(), reader.ReadInt32()));
        }
        // A PNG when there is one: it is the sharper of the two.
        foreach (byte code in new[] { PngCode, BitmapCode })
        {
            var (_, start, size) = images.FirstOrDefault(image => image.Code == code);
            if (size <= 0 || size > MaxImageBytes || start <= 0 || start + size > length)
            {
                continue;
            }
            stream.Position = start;
            byte[] image = reader.ReadBytes(size);
            return code == PngCode ? image : Dib.AsBmpFile(image);
        }
        return null;
    }

    /// <summary>Every DWG starts with its version, "AC10" and two digits.</summary>
    private static bool IsDrawing(BinaryReader reader)
    {
        byte[] version = reader.ReadBytes(6);
        return version.Length == 6 && version[0] == 'A' && version[1] == 'C' && version.Skip(2).All(b => b is >= (byte)'0' and <= (byte)'9');
    }
}
