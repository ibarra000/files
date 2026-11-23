using System.Windows;

namespace Files.Services;

/// <summary>A text-recognition engine that reads the words in the crop around the mouse.</summary>
public interface ITextReader
{
    /// <summary>How much the crop is enlarged before it is read; the engine reads best at this size.</summary>
    int Scale { get; }

    /// <summary>The words in <paramref name="png"/> with their boxes, in the enlarged image's pixels.</summary>
    Task<IReadOnlyList<(string Text, Rect Box)>> ReadWordsAsync(byte[] png, CancellationToken cancel);
}

/// <summary>An engine that cannot read at all on this PC, with what the user can do about it.</summary>
public sealed class TextRecognitionUnavailableException(string message) : Exception(message);

/// <summary>An engine whose models or native runtime could not be loaded; another engine reads instead.</summary>
public sealed class TextReaderLoadException(string message, Exception? inner = null) : Exception(message, inner);
