using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Media3D;

namespace Files.UI;

/// <summary>Walks up from whatever a mouse event came from to the element that owns it.</summary>
public static class ElementTree
{
    /// <summary>
    /// The nearest <typeparamref name="T"/> at or above <paramref name="source"/>, or null. Text in a result's
    /// name is a Run, which is not a Visual; asking the visual tree for its parent throws, so content
    /// elements and elements not yet in the visual tree are walked through the logical tree instead.
    /// </summary>
    public static T? AncestorOf<T>(object? source) where T : DependencyObject
    {
        for (var node = source as DependencyObject; node is not null; node = ParentOf(node))
        {
            if (node is T found)
            {
                return found;
            }
        }
        return null;
    }

    private static DependencyObject? ParentOf(DependencyObject node)
    {
        if (node is Visual or Visual3D)
        {
            return VisualTreeHelper.GetParent(node) ?? LogicalTreeHelper.GetParent(node);
        }
        return LogicalTreeHelper.GetParent(node);
    }
}
