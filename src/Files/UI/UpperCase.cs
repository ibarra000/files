using System.Globalization;
using System.Windows.Data;

namespace Files.UI;

/// <summary>Shows text in capitals, for section labels; WPF has no text-transform.</summary>
public sealed class UpperCase : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, CultureInfo culture) =>
        value is string text ? text.ToUpper(culture) : value;

    public object ConvertBack(object value, Type targetType, object parameter, CultureInfo culture) =>
        throw new NotSupportedException("Section labels are only displayed.");
}
