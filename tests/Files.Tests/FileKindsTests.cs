using Files.UI;

namespace Files.Tests;

public sealed class FileKindsTests
{
    [Fact]
    public void Each_kind_of_file_gets_its_own_icon()
    {
        string[] names = ["report.pdf", "site.dwg", "costs.xlsx", "photo.png", "backup.zip", "clip.mp4", "memo.mp3", "tool.cs", "notes.docx"];
        string[] glyphs = names
            .Select(FileKinds.Glyph)
            .ToArray();
        Assert.Equal(glyphs.Length, glyphs.Distinct().Count());
    }

    [Fact]
    public void Extensions_match_whatever_their_case()
    {
        Assert.Equal(FileKinds.Glyph("AB-C-0704.pdf"), FileKinds.Glyph("AB-C-0704.PDF"));
    }

    [Fact]
    public void Unknown_or_missing_extensions_look_like_a_document()
    {
        string document = FileKinds.Glyph("notes.docx");
        Assert.Equal(document, FileKinds.Glyph("data.xyz"));
        Assert.Equal(document, FileKinds.Glyph("README"));
        Assert.Equal(document, FileKinds.Glyph(".gitignore"));
    }
}
