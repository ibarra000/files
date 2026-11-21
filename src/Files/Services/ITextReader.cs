using System.Windows;

namespace Files.Services;

/// <summary>A word read from the screen, its box in the read image's pixels, and how sure the engine is (0 to 1) when it says.</summary>
public sealed record ReadWord(string Text, Rect Box, float? Confidence = null)
{
    public static implicit operator ReadWord((string Text, Rect Box) word) => new(word.Text, word.Box);
}

/// <summary>A text-recognition engine that reads the words in the crop around the mouse.</summary>
public interface ITextReader
{
    /// <summary>How much the crop is enlarged before it is read; the engine reads best at this size.</summary>
    int Scale { get; }

    /// <summary>The words in <paramref name="png"/> with their boxes, in the enlarged image's pixels.</summary>
    Task<IReadOnlyList<ReadWord>> ReadWordsAsync(byte[] png, CancellationToken cancel);
}

/// <summary>An engine that cannot read at all on this PC, with what the user can do about it.</summary>
public sealed class TextRecognitionUnavailableException(string message) : Exception(message);

/// <summary>An engine whose models or native runtime could not be loaded; another engine reads instead.</summary>
public sealed class TextReaderLoadException(string message, Exception? inner = null) : Exception(message, inner);
