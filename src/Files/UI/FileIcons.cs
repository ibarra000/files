using Files.Core;

namespace Files.UI;

/// <summary>The Segoe Fluent Icons glyph for each kind of file, chosen from its extension alone.</summary>
public static class FileIcons
{
    public const string Document = "\uE8A5";
    public const string Folder = "\uE8B7";

    public static string Glyph(string name) => FileKinds.Of(name) switch
    {
        FileKind.Pdf => "\uEA90", // PDF
        FileKind.Image => "\uE8B9", // Picture
        FileKind.Drawing => "\uEB3C", // Design
        FileKind.Sheet => "\uE80A", // ViewAll
        FileKind.Archive => "\uF012", // ZipFolder
        FileKind.Video => "\uE714", // Video
        FileKind.Audio => "\uE8D6", // Audio
        FileKind.Code => "\uE943", // Code
        _ => Document,
    };
}
