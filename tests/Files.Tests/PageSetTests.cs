using Files.Core;

namespace Files.Tests;

public sealed class PageSetTests
{
    [Theory]
    [InlineData("AB-C-0704.pdf", 0)]
    [InlineData("AB-C-0704_Page2.pdf", 2)]
    [InlineData("ab-c-0704-p3.PDF", 3)]
    [InlineData("AB-C-0704 page 4.pdf", 4)]
    [InlineData("AB-C-0704pg12.pdf", 12)]
    public void Page_markers_are_recognised(string name, int page)
    {
        Assert.Equal(page, PageSet.PageOf("AB-C-0704", name));
    }

    [Theory]
    [InlineData("AB-C-0704-2.pdf")]
    [InlineData("AB-C-0704 revision notes.pdf")]
    [InlineData("AB-C-0704_Page2.txt")]
    [InlineData("AB-C-07045.pdf")]
    public void Other_files_are_not_pages(string name)
    {
        Assert.Null(PageSet.PageOf("AB-C-0704", name));
    }

    [Fact]
    public void The_document_name_drops_the_page_marker()
    {
        Assert.Equal("AB-C-0704", PageSet.BaseOf("AB-C-0704_Page10.pdf"));
        Assert.Equal("AB-C-0704", PageSet.BaseOf("AB-C-0704.pdf"));
        Assert.Null(PageSet.BaseOf("AB-C-0704.dwg"));
    }

    [Fact]
    public void Pages_are_collected_from_the_same_folder_in_reading_order()
    {
        VolumeIndex index = SearchTests.Index(@"C:\v",
            ("", ["AB_Page1.pdf"]),
            ("set", ["AB_Page10.pdf", "AB_Page2.pdf", "AB.pdf", "ABC.pdf", "other.pdf"]));
        Hit hit = Searcher.Search([("v", index)], Query.Parse("AB_Page2"), new HashSet<string>(), CancellationToken.None).Hits[0];

        IReadOnlyList<string> pages = PageSet.Collect(hit);

        Assert.Equal([@"C:\v\set\AB.pdf", @"C:\v\set\AB_Page2.pdf", @"C:\v\set\AB_Page10.pdf"], pages);
    }
}
