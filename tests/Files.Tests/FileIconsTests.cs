using Files.Core;
using Files.UI;

namespace Files.Tests;

public sealed class FileIconsTests
{
    [Fact]
    public void Each_kind_of_file_gets_its_own_icon()
    {
        string[] names = ["report.pdf", "site.dwg", "costs.xlsx", "photo.png", "backup.zip", "clip.mp4", "memo.mp3", "tool.cs", "notes.docx"];
        string[] glyphs = names
            .Select(FileIcons.Glyph)
            .ToArray();
        Assert.Equal(glyphs.Length, glyphs.Distinct().Count());
    }

    [Fact]
    public void Extensions_match_whatever_their_case()
    {
        Assert.Equal(FileIcons.Glyph("AB-C-0704.pdf"), FileIcons.Glyph("AB-C-0704.PDF"));
    }

    [Fact]
    public void Unknown_or_missing_extensions_look_like_a_document()
    {
        string document = FileIcons.Glyph("notes.docx");
        Assert.Equal(document, FileIcons.Glyph("data.xyz"));
        Assert.Equal(document, FileIcons.Glyph("README"));
        Assert.Equal(document, FileIcons.Glyph(".gitignore"));
    }

    [Theory]
    [InlineData("AB-C-0704.pdf", FileKind.Pdf)]
    [InlineData("site.DWG", FileKind.Drawing)]
    [InlineData("notes.docx", FileKind.Document)]
    [InlineData("main.c", FileKind.Code)]
    [InlineData("data.xyz", FileKind.Other)]
    [InlineData(".gitignore", FileKind.Other)]
    [InlineData("file.", FileKind.Other)]
    public void A_file_kind_comes_from_its_extension(string name, FileKind expected)
    {
        Assert.Equal(expected, FileKinds.Of(name));
    }

    [Fact]
    public void The_extensions_of_several_kinds_are_combined()
    {
        IReadOnlySet<string> documents = FileKinds.ExtensionsOf(FileKind.Pdf, FileKind.Document, FileKind.Sheet);
        Assert.Contains("pdf", documents);
        Assert.Contains("docx", documents);
        Assert.Contains("xlsx", documents);
        Assert.DoesNotContain("png", documents);
    }
}
