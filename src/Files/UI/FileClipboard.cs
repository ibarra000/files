using System.Collections.Specialized;
using System.Runtime.InteropServices;
using System.Windows;

namespace Files.UI;

/// <summary>
/// Files on the clipboard as Explorer puts them there: Explorer pastes the files, a text box pastes the
/// path, and "Preferred DropEffect" says whether the paste copies or moves (cut).
/// </summary>
public static class FileClipboard
{
    public const string DropEffectFormat = "Preferred DropEffect";

    // DROPEFFECT_COPY | DROPEFFECT_LINK, as Explorer's Copy writes it; DROPEFFECT_MOVE for Cut.
    private const int CopyEffect = 5;
    private const int MoveEffect = 2;
    private const int CopyBit = 1;

    public static DataObject Make(IReadOnlyList<string> paths, bool cut)
    {
        var data = new DataObject();
        var list = new StringCollection();
        list.AddRange(paths.ToArray());
        data.SetFileDropList(list);
        data.SetText(string.Join(Environment.NewLine, paths), TextDataFormat.UnicodeText);
        data.SetData(DropEffectFormat, new MemoryStream(BitConverter.GetBytes(cut ? MoveEffect : CopyEffect)));
        return data;
    }

    /// <summary>The files on <paramref name="data"/>, or none.</summary>
    public static IReadOnlyList<string> Files(IDataObject? data) =>
        data?.GetData(DataFormats.FileDrop) as string[] ?? [];

    /// <summary>Whether the files were cut, so pasting moves them. Without a drop effect (most programs) a paste copies.</summary>
    public static bool IsCut(IDataObject? data)
    {
        if (data?.GetData(DropEffectFormat) is not MemoryStream stream || stream.Length < sizeof(int))
        {
            return false;
        }
        byte[] bytes = stream.ToArray();
        int effect = BitConverter.ToInt32(bytes, 0);
        return (effect & MoveEffect) != 0 && (effect & CopyBit) == 0;
    }

    /// <summary>
    /// A number Windows changes whenever anything is put on the clipboard, so the panel can tell that what
    /// it put there is still there. Clipboard.IsCurrent cannot: the panel's copy is flushed so it outlives
    /// the app, and after that the clipboard no longer holds the panel's own object.
    /// </summary>
    public static uint Version => GetClipboardSequenceNumber();

    [DllImport("user32.dll")]
    private static extern uint GetClipboardSequenceNumber();
}
