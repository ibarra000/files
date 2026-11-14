using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace Files.Services;

/// <summary>
/// The thumbnail Windows shows for a file in Explorer, drawn by whichever program installed a preview
/// handler for its type: CAD programs (Creo, SOLIDWORKS, Inventor, MicroStation, Revit, SketchUp) do.
/// Handlers are other vendors' code and can be slow on a share, or hang, so each request runs on its
/// own thread and is given up after <see cref="Limit"/>.
/// </summary>
public static class ShellThumbnail
{
    public static readonly TimeSpan Limit = TimeSpan.FromSeconds(5);

    /// <summary>Only a thumbnail, never the type's icon; a larger one is fine.</summary>
    private const int ThumbnailOnly = 0x8;
    private const int BiggerSizeOk = 0x1;
    private static readonly Guid ImageFactoryId = new("bcc18b79-ba16-442f-80c4-8a59c30c463b");

    [StructLayout(LayoutKind.Sequential)]
    private struct Size
    {
        public int Width;
        public int Height;
    }

    [ComImport]
    [Guid("bcc18b79-ba16-442f-80c4-8a59c30c463b")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItemImageFactory
    {
        [PreserveSig]
        int GetImage(Size size, int flags, out IntPtr bitmap);
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern int SHCreateItemFromParsingName(string path, IntPtr bindContext, in Guid id, [MarshalAs(UnmanagedType.Interface)] out object item);

    [DllImport("gdi32.dll")]
    private static extern int DeleteObject(IntPtr gdiObject);

    [DllImport("gdi32.dll", EntryPoint = "GetObjectW")]
    private static extern int GetObject(IntPtr gdiObject, int bytes, out DibSection section);

    /// <summary>A DIB section's description (DIBSECTION): where its pixels are, and which way up.</summary>
    [StructLayout(LayoutKind.Sequential)]
    private struct DibSection
    {
        public int Type;
        public int Width;
        public int Height;
        public int RowBytes;
        public ushort Planes;
        public ushort BitsPerPixel;
        public IntPtr Bits;
        public int HeaderSize;
        public int HeaderWidth;
        public int HeaderHeight;
        public short HeaderPlanes;
        public short HeaderBitCount;
        public int Compression;
        public int ImageSize;
        public int PixelsPerMeterX;
        public int PixelsPerMeterY;
        public int ColorsUsed;
        public int ColorsImportant;
        public uint RedMask;
        public uint GreenMask;
        public uint BlueMask;
        public IntPtr Section;
        public uint Offset;
    }

    /// <summary>
    /// The thumbnail as PNG bytes, about <paramref name="size"/> pixels on its longer side; null when no
    /// handler draws one, or it took longer than <see cref="Limit"/>.
    /// </summary>
    /// <param name="typedAs">
    /// The extension the file's handler is registered for, when its own differs: Creo numbers its saves
    /// (part.prt.3), and Windows finds no handler for ".3". The file is then read from a copy.
    /// </param>
    public static async Task<byte[]?> ReadAsync(string path, int size, string? typedAs, CancellationToken cancel)
    {
        // A handler that hangs keeps its thread; past a few of those, no more are started.
        if (!Running.Wait(0, CancellationToken.None))
        {
            return null;
        }
        var done = new TaskCompletionSource<byte[]?>(TaskCreationOptions.RunContinuationsAsynchronously);
        var thread = new Thread(() =>
        {
            try
            {
                done.TrySetResult(ReadOnThisThread(path, size, typedAs));
            }
            catch (Exception)
            {
                // Vendor code runs on this thread: whatever it throws must not end the app, which an
                // exception escaping a thread of its own would. No thumbnail is shown instead.
                done.TrySetResult(null);
            }
            finally
            {
                Running.Release();
            }
        })
        {
            IsBackground = true,
            Name = "Shell thumbnail",
        };
        // Handlers are COM objects that expect a single-threaded apartment, as in Explorer.
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        try
        {
            return await done.Task.WaitAsync(Limit, cancel);
        }
        catch (TimeoutException)
        {
            // The thread is left to finish (or hang) on its own; it is a background thread.
            return null;
        }
    }

    /// <summary>How many handlers may run at once, counting any that hang.</summary>
    private static readonly SemaphoreSlim Running = new(3);

    /// <summary>The longest file copied to give a numbered Creo save its type.</summary>
    private const long MaxCopyBytes = 64L * 1024 * 1024;

    /// <summary>Copies older than this are left from a handler that hung or an app that closed; they are deleted.</summary>
    private static readonly TimeSpan StaleCopy = TimeSpan.FromHours(1);

    private static string CopyFolder => Path.Combine(Path.GetTempPath(), "files-preview");

    /// <summary>The largest thumbnail kept, in pixels; a handler may give any size.</summary>
    private const int MaxPixels = 4096 * 4096;

    private static byte[]? ReadOnThisThread(string path, int size, string? typedAs)
    {
        string? copy = null;
        try
        {
            if (typedAs is not null)
            {
                if (new FileInfo(path).Length > MaxCopyBytes)
                {
                    return null;
                }
                Directory.CreateDirectory(CopyFolder);
                DeleteStaleCopies();
                copy = Path.Combine(CopyFolder, $"{Guid.NewGuid():N}.{typedAs}");
                File.Copy(path, copy);
            }
            return Thumbnail(copy ?? path, size);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or COMException
            or InvalidCastException or ArgumentException or NotSupportedException)
        {
            return null;
        }
        finally
        {
            if (copy is not null)
            {
                TryDelete(copy);
            }
        }
    }

    private static void DeleteStaleCopies()
    {
        foreach (FileInfo old in new DirectoryInfo(CopyFolder).EnumerateFiles())
        {
            if (DateTime.UtcNow - old.CreationTimeUtc > StaleCopy)
            {
                TryDelete(old.FullName);
            }
        }
    }

    private static void TryDelete(string path)
    {
        try
        {
            File.Delete(path);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // Still open in a handler; deleted as stale on a later copy.
        }
    }

    private static byte[]? Thumbnail(string path, int size)
    {
        // The shell parses only backslashes; GetFullPath turns any forward ones round.
        if (SHCreateItemFromParsingName(Path.GetFullPath(path), IntPtr.Zero, ImageFactoryId, out object item) < 0)
        {
            return null;
        }
        try
        {
            var factory = (IShellItemImageFactory)item;
            if (factory.GetImage(new Size { Width = size, Height = size }, ThumbnailOnly | BiggerSizeOk, out IntPtr bitmap) < 0 || bitmap == IntPtr.Zero)
            {
                return null;
            }
            try
            {
                // GetObject fills a BITMAP, the start of a DIBSECTION, for any bitmap; its size is checked before it is copied.
                if (GetObject(bitmap, Marshal.SizeOf<DibSection>(), out DibSection about) == 0 || about.Width <= 0 || about.Height <= 0
                    || (long)about.Width * about.Height > MaxPixels)
                {
                    return null;
                }
                BitmapSource picture = WithAlpha(bitmap) ?? System.Windows.Interop.Imaging.CreateBitmapSourceFromHBitmap(
                    bitmap, IntPtr.Zero, Int32Rect.Empty, BitmapSizeOptions.FromEmptyOptions());
                return Png(OnWhite(picture));
            }
            finally
            {
                DeleteObject(bitmap);
            }
        }
        finally
        {
            Marshal.ReleaseComObject(item);
        }
    }

    /// <summary>
    /// The pixels of a 32-bit thumbnail with their transparency, which CreateBitmapSourceFromHBitmap drops
    /// (turning a transparent background black); null for any other kind of bitmap.
    /// </summary>
    private static BitmapSource? WithAlpha(IntPtr bitmap)
    {
        if (GetObject(bitmap, Marshal.SizeOf<DibSection>(), out DibSection dib) == 0 || dib.BitsPerPixel != 32 || dib.Bits == IntPtr.Zero
            || dib.Width <= 0 || dib.Height <= 0 || (long)dib.Width * dib.Height > MaxPixels || dib.RowBytes < 4 * dib.Width)
        {
            return null;
        }
        int stride = dib.RowBytes;
        var pixels = new byte[stride * dib.Height];
        Marshal.Copy(dib.Bits, pixels, 0, pixels.Length);
        if (dib.HeaderHeight > 0)
        {
            // Stored bottom row first.
            var flipped = new byte[pixels.Length];
            for (int row = 0; row < dib.Height; row++)
            {
                Array.Copy(pixels, row * stride, flipped, (dib.Height - 1 - row) * stride, stride);
            }
            pixels = flipped;
        }
        bool anyAlpha = false;
        for (int i = 3; i < pixels.Length && !anyAlpha; i += 4)
        {
            anyAlpha = pixels[i] != 0;
        }
        if (!anyAlpha)
        {
            // No transparency at all means the alpha byte is unused, not that every pixel is clear.
            for (int i = 3; i < pixels.Length; i += 4)
            {
                pixels[i] = 255;
            }
        }
        return BitmapSource.Create(dib.Width, dib.Height, 96, 96, PixelFormats.Pbgra32, null, pixels, stride);
    }

    /// <summary>Thumbnails may be transparent around the part; on white they read as Explorer shows them.</summary>
    private static BitmapSource OnWhite(BitmapSource picture)
    {
        var visual = new DrawingVisual();
        using (DrawingContext context = visual.RenderOpen())
        {
            var area = new Rect(0, 0, picture.PixelWidth, picture.PixelHeight);
            context.DrawRectangle(Brushes.White, null, area);
            context.DrawImage(picture, area);
        }
        var flat = new RenderTargetBitmap(picture.PixelWidth, picture.PixelHeight, 96, 96, PixelFormats.Pbgra32);
        flat.Render(visual);
        return flat;
    }

    private static byte[] Png(BitmapSource picture)
    {
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(picture));
        using var png = new MemoryStream();
        encoder.Save(png);
        return png.ToArray();
    }
}
