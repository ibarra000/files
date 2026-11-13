using Files.Services;

namespace Files.Tests;

public sealed class PreviewTests
{
    [Fact]
    public void A_text_file_shows_its_first_lines_size_and_kind()
    {
        using var temp = new TempFolder();
        string path = temp.File("notes.txt", string.Join("\n", Enumerable.Range(1, 100).Select(i => $"line {i}")));

        PreviewInfo preview = Preview.Load(path, CancellationToken.None);

        Assert.Equal("notes.txt", preview.Name);
        Assert.Equal(temp.Path, preview.Folder);
        Assert.Equal(Preview.MaxLines, preview.Lines!.Count);
        Assert.Equal("line 1", preview.Lines[0]);
        Assert.Equal(new FileInfo(path).Length, preview.Size);
        Assert.NotNull(preview.Modified);
        Assert.Equal("Document", preview.Kind);
        Assert.Null(preview.Note);
    }

    [Fact]
    public void Tabs_show_as_spaces_and_long_lines_are_cut()
    {
        using var temp = new TempFolder();
        string path = temp.File("tool.cs", "\tint x;\n" + new string('a', 500));
        IReadOnlyList<string> lines = Preview.Load(path, CancellationToken.None).Lines!;
        Assert.Equal("    int x;", lines[0]);
        Assert.Equal(Preview.MaxLineLength, lines[1].Length);
    }

    [Fact]
    public void A_text_type_holding_binary_says_there_is_no_text_preview()
    {
        using var temp = new TempFolder();
        string path = temp.File("data.json", "{\"a\":\0\0\0}");
        PreviewInfo preview = Preview.Load(path, CancellationToken.None);
        Assert.Null(preview.Lines);
        Assert.Equal("Binary file: no text preview", preview.Note);
    }

    [Fact]
    public void Other_types_have_no_text_preview_but_keep_their_details()
    {
        using var temp = new TempFolder();
        string path = temp.File("site.dwg", "x");
        PreviewInfo preview = Preview.Load(path, CancellationToken.None);
        Assert.Null(preview.Lines);
        Assert.Equal("Drawing", preview.Kind);
        Assert.Equal(1, preview.Size);
        Assert.Equal("No preview for this type", preview.Note);
    }

    [Fact]
    public void An_image_is_marked_for_the_panel_to_draw()
    {
        using var temp = new TempFolder();
        Assert.True(Preview.Load(temp.File("photo.png", "x"), CancellationToken.None).IsImage);
    }

    [Fact]
    public void A_folder_lists_its_first_entries_folders_first()
    {
        using var temp = new TempFolder();
        for (int i = 0; i < 20; i++)
        {
            temp.File($"file{i:00}.txt");
        }
        Directory.CreateDirectory(Path.Combine(temp.Path, "zz sub"));

        PreviewInfo preview = Preview.Load(temp.Path, CancellationToken.None);

        Assert.Equal("Folder", preview.Kind);
        Assert.Equal(Preview.MaxEntries, preview.Entries!.Count);
        Assert.Equal(@"zz sub\", preview.Entries[0]);
        Assert.Null(preview.Size);
    }

    [Fact]
    public void A_file_that_is_gone_says_so()
    {
        using var temp = new TempFolder();
        PreviewInfo preview = Preview.Load(Path.Combine(temp.Path, "gone.pdf"), CancellationToken.None);
        Assert.Equal("Not found", preview.Note);
        Assert.Null(preview.Size);
    }

    [Fact]
    public void Sizes_read_in_the_largest_whole_unit()
    {
        Assert.Equal("0 B", Preview.FormatSize(0));
        Assert.Equal("512 B", Preview.FormatSize(512));
        Assert.Equal($"{2.4:N1} KB", Preview.FormatSize(2458));
        Assert.Equal($"{1.5:N1} MB", Preview.FormatSize(1_572_864));
        Assert.Equal($"{2.0:N1} GB", Preview.FormatSize(2L * 1024 * 1024 * 1024));
    }
}
