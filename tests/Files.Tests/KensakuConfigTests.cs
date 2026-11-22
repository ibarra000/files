using System.Text.Json;
using Files.Data;
using Files.Services;

namespace Files.Tests;

public sealed class KensakuConfigTests
{
    private static Volume Volume(string root, bool liveOnly = false) =>
        new(1, Guid.NewGuid(), root, "local", Path.GetFileName(root), VolumeStatus.Online, null, null, liveOnly);

    private static JsonElement Parse(string text) => JsonDocument.Parse(text).RootElement;

    [Fact]
    public void Every_field_kensaku_requires_is_written_and_setup_is_complete()
    {
        JsonElement config = Parse(KensakuConfig.Text([Volume(@"C:\docs")], ocr: false));

        foreach (string field in new[] { "watched_dirs", "excluded_dirs", "excluded_extensions", "shortcut", "max_results", "is_setup_complete" })
        {
            Assert.True(config.TryGetProperty(field, out _), field);
        }
        Assert.True(config.GetProperty("is_setup_complete").GetBoolean());
        Assert.Equal(7, config.GetProperty("config_version").GetInt32());
    }

    [Fact]
    public void Only_the_roots_of_volumes_read_in_the_background_are_watched()
    {
        string text = KensakuConfig.Text([Volume(@"\\server\drawings"), Volume(@"\\server\archive", liveOnly: true), Volume(@"C:\docs")], ocr: false);

        string[] watched = Parse(text).GetProperty("watched_dirs").EnumerateArray().Select(root => root.GetString()!).ToArray();

        Assert.Equal([@"C:\docs", @"\\server\drawings"], watched);
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void Reading_images_is_written_as_set(bool ocr)
    {
        Assert.Equal(ocr, Parse(KensakuConfig.Text([], ocr)).GetProperty("ocr_enabled").GetBoolean());
    }

    [Fact]
    public void The_same_volumes_in_another_order_give_the_same_config_and_hash()
    {
        string one = KensakuConfig.Text([Volume(@"C:\a"), Volume(@"C:\b")], ocr: false);
        string other = KensakuConfig.Text([Volume(@"C:\b"), Volume(@"C:\a")], ocr: false);

        Assert.Equal(one, other);
        Assert.Equal(KensakuConfig.Hash(one), KensakuConfig.Hash(other));
        Assert.NotEqual(KensakuConfig.Hash(one), KensakuConfig.Hash(KensakuConfig.Text([Volume(@"C:\a")], ocr: false)));
    }
}
