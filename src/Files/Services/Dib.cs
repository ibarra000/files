namespace Files.Services;

/// <summary>Device-independent bitmaps, as CAD programs save their previews (a DWG's, a DXF's).</summary>
public static class Dib
{
    private const int BmpFileHeaderBytes = 14;

    /// <summary>Puts the BMP file header in front of a DIB, so the image decoder can read it; null when the DIB is not whole.</summary>
    public static byte[]? AsBmpFile(byte[] dib)
    {
        const int InfoHeaderBytes = 40;
        const int BitFields = 3;
        if (dib.Length < InfoHeaderBytes)
        {
            return null;
        }
        int infoBytes = BitConverter.ToInt32(dib, 0);
        int bitCount = BitConverter.ToInt16(dib, 14);
        int compression = BitConverter.ToInt32(dib, 16);
        int colorsUsed = BitConverter.ToInt32(dib, 32);
        int colors = colorsUsed != 0 ? colorsUsed : bitCount <= 8 ? 1 << bitCount : 0;
        int masks = compression == BitFields && infoBytes == InfoHeaderBytes ? 12 : 0;
        long pixelsAt = BmpFileHeaderBytes + (long)infoBytes + masks + colors * 4L;
        if (infoBytes < InfoHeaderBytes || colorsUsed < 0 || pixelsAt > BmpFileHeaderBytes + dib.Length)
        {
            return null;
        }
        var file = new byte[BmpFileHeaderBytes + dib.Length];
        file[0] = (byte)'B';
        file[1] = (byte)'M';
        BitConverter.TryWriteBytes(file.AsSpan(2), file.Length);
        BitConverter.TryWriteBytes(file.AsSpan(10), (int)pixelsAt);
        dib.CopyTo(file, BmpFileHeaderBytes);
        return file;
    }
}
