using System.Runtime.InteropServices;
using Windows.Data.Pdf;
using Windows.Storage.Streams;

namespace Files.Services;

/// <summary>
/// The first page of a PDF as a picture, rendered by Windows' own PDF engine
/// (<c>Windows.Data.Pdf</c>), so no PDF library ships with the app.
/// </summary>
public static class PdfThumbnail
{
    /// <summary>The first page as PNG bytes <paramref name="width"/> pixels wide, and the page count; null when it cannot be rendered.</summary>
    public static async Task<(byte[] Png, uint Pages)?> RenderAsync(string path, uint width, CancellationToken cancel)
    {
        try
        {
            await using FileStream file = new(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete, 4096, useAsync: true);
            PdfDocument document = await PdfDocument.LoadFromStreamAsync(file.AsRandomAccessStream()).AsTask(cancel);
            if (document.PageCount == 0)
            {
                return null;
            }
            using PdfPage page = document.GetPage(0);
            using var output = new InMemoryRandomAccessStream();
            var options = new PdfPageRenderOptions { DestinationWidth = width, BitmapEncoderId = Windows.Graphics.Imaging.BitmapEncoder.PngEncoderId };
            await page.RenderToStreamAsync(output, options).AsTask(cancel);
            using var png = new MemoryStream();
            await output.AsStreamForRead().CopyToAsync(png, cancel);
            return (png.ToArray(), document.PageCount);
        }
        catch (Exception error) when (error is COMException or IOException or UnauthorizedAccessException or ArgumentException
            or InvalidOperationException or NotSupportedException)
        {
            // Damaged, password-protected, or a form Windows cannot draw. WinRT reports most of these as COM errors.
            return null;
        }
    }
}
