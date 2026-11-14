using System.Globalization;
using System.Text;
using System.Windows;

namespace Files.Services;

/// <summary>
/// Reads an HP-GL or HP-GL/2 plot file (.plt) as the lines its pen draws. Plots sent through a printer
/// driver come wrapped in PCL; the wrapper is skipped up to the HP-GL/2 inside it. Labels are not drawn.
/// A plot that is raster or PostScript has no pen moves, so it gives no lines.
/// </summary>
public static class PltPreview
{
    private const char Escape = '\x1B';
    private const char DefaultLabelEnd = '\x03';

    private sealed class Pen(LineWork work)
    {
        private readonly List<Point> _line = [];

        public Point At { get; private set; }
        public bool Down { get; set; }
        public bool Relative { get; set; }

        /// <summary>Moves the pen, drawing the way if it is down.</summary>
        public void MoveTo(Point to)
        {
            if (Down)
            {
                if (_line.Count == 0)
                {
                    _line.Add(At);
                }
                _line.Add(to);
            }
            else
            {
                Lift();
            }
            At = to;
        }

        public void Move(double x, double y) => MoveTo(Relative ? new Point(At.X + x, At.Y + y) : new Point(x, y));

        /// <summary>Ends the line being drawn.</summary>
        public void Lift()
        {
            work.Add([.. _line]);
            _line.Clear();
        }
    }

    /// <summary>The plot's lines; null when the file is not HP-GL (a stray byte where a command should be).</summary>
    public static LineWork? Read(string path, CancellationToken cancel)
    {
        string text = Encoding.Latin1.GetString(File.ReadAllBytes(path));
        var work = new LineWork();
        var pen = new Pen(work);
        char labelEnd = DefaultLabelEnd;
        int i = 0;
        bool sawCommand = false;
        while (i < text.Length && !work.TooLarge)
        {
            char c = text[i];
            if (c == Escape)
            {
                i = SkipPcl(text, i);
                continue;
            }
            // Any other byte where a command should be means this is not HP-GL.
            if (c is ';' or ' ' or ',' or '\r' or '\n' or '\t' or DefaultLabelEnd)
            {
                i++;
                continue;
            }
            if (!IsLetter(c) || i + 1 >= text.Length || !IsLetter(text[i + 1]))
            {
                return sawCommand ? Finish(pen, work) : null;
            }
            sawCommand = true;
            string command = text.Substring(i, 2).ToUpperInvariant();
            i += 2;
            switch (command)
            {
                case "LB":
                    int end = text.IndexOf(labelEnd, i);
                    i = end < 0 ? text.Length : end + 1;
                    continue;
                case "DT":
                    if (i < text.Length && text[i] != ';')
                    {
                        labelEnd = text[i];
                        i++;
                    }
                    break;
                case "PE":
                    i = Encoded(text, i, pen);
                    continue;
            }
            int start = i;
            i = SkipParameters(text, i);
            Run(command, Numbers(text.AsSpan(start, i - start)), pen, work);
            cancel.ThrowIfCancellationRequested();
        }
        return sawCommand ? Finish(pen, work) : null;
    }

    private static LineWork Finish(Pen pen, LineWork work)
    {
        pen.Lift();
        return work;
    }

    private static bool IsLetter(char c) => c is >= 'A' and <= 'Z' or >= 'a' and <= 'z';

    private static void Run(string command, List<double> numbers, Pen pen, LineWork work)
    {
        switch (command)
        {
            case "IN" or "DF":
                pen.Lift();
                pen.Down = false;
                pen.Relative = false;
                break;
            case "PU" or "PD":
                pen.Lift();
                pen.Down = command == "PD";
                MoveThrough(numbers, pen);
                break;
            case "PA" or "PR":
                pen.Relative = command == "PR";
                MoveThrough(numbers, pen);
                break;
            case "SP":
                pen.Lift();
                break;
            case "CI" when numbers.Count >= 1:
                // A circle around where the pen is, drawn whether the pen is up or down.
                work.Add(LineDrawing.Arc(pen.At, numbers[0], numbers[0], 0, 0, 2 * Math.PI));
                break;
            case "AA" or "AR" when numbers.Count >= 3:
                Point centre = command == "AA" ? new Point(numbers[0], numbers[1]) : new Point(pen.At.X + numbers[0], pen.At.Y + numbers[1]);
                double radius = Distance(pen.At, centre);
                double from = Math.Atan2(pen.At.Y - centre.Y, pen.At.X - centre.X);
                foreach (Point point in LineDrawing.Arc(centre, radius, radius, 0, from, numbers[2] * Math.PI / 180).Skip(1))
                {
                    pen.MoveTo(point);
                }
                break;
            case "EA" or "ER" or "RA" or "RR" when numbers.Count >= 2:
                Point corner = command is "EA" or "RA" ? new Point(numbers[0], numbers[1]) : new Point(pen.At.X + numbers[0], pen.At.Y + numbers[1]);
                Point a = pen.At;
                work.Add([a, new Point(corner.X, a.Y), corner, new Point(a.X, corner.Y), a]);
                break;
        }
    }

    private static void MoveThrough(List<double> numbers, Pen pen)
    {
        for (int k = 0; k + 1 < numbers.Count; k += 2)
        {
            pen.Move(numbers[k], numbers[k + 1]);
        }
    }

    private static double Distance(Point a, Point b) => Math.Sqrt((a.X - b.X) * (a.X - b.X) + (a.Y - b.Y) * (a.Y - b.Y));

    /// <summary>Parameters run to the next command's letters or a semicolon; a quoted string may hold either.</summary>
    private static int SkipParameters(string text, int i)
    {
        while (i < text.Length && text[i] != ';' && text[i] != Escape && !IsLetter(text[i]))
        {
            if (text[i] == '"')
            {
                int close = text.IndexOf('"', i + 1);
                i = close < 0 ? text.Length : close;
            }
            i++;
        }
        return i;
    }

    private static List<double> Numbers(ReadOnlySpan<char> parameters)
    {
        var numbers = new List<double>();
        foreach (Range range in parameters.SplitAny(", \t\r\n"))
        {
            if (double.TryParse(parameters[range], NumberStyles.Float, CultureInfo.InvariantCulture, out double number))
            {
                numbers.Add(number);
            }
        }
        return numbers;
    }

    /// <summary>
    /// Skips a PCL escape sequence, and any binary data it announces (a "W" command carries that many
    /// bytes), up to the next printable command. Inside PCL, everything up to the switch into HP-GL/2
    /// (ESC % n B) is skipped too.
    /// </summary>
    private static int SkipPcl(string text, int i)
    {
        bool inPcl = true;
        while (i < text.Length)
        {
            if (text[i] != Escape)
            {
                if (!inPcl)
                {
                    return i;
                }
                i++;
                continue;
            }
            i++;
            if (i >= text.Length)
            {
                return i;
            }
            char group = text[i];
            if (group is < '!' or > '/')
            {
                // A two-character escape, such as ESC E (reset).
                i++;
                continue;
            }
            i++;
            if (group == '%' && text.IndexOf("-12345X", i, Math.Min(7, text.Length - i), StringComparison.Ordinal) == i)
            {
                // The job's PJL header: text lines up to the next escape.
                int next = text.IndexOf(Escape, i);
                i = next < 0 ? text.Length : next;
                continue;
            }
            if (i < text.Length && group != '%' && text[i] is >= '`' and <= '~')
            {
                i++; // the group character, as in ESC * b
            }
            while (i < text.Length)
            {
                int numberStart = i;
                while (i < text.Length && text[i] is >= '0' and <= '9' or '+' or '-' or '.')
                {
                    i++;
                }
                int.TryParse(text.AsSpan(numberStart, i - numberStart), NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out int value);
                if (i >= text.Length)
                {
                    return i;
                }
                char letter = text[i++];
                if (group == '%' && letter is 'B' or 'b')
                {
                    inPcl = false;
                    break;
                }
                if (group == '%' && letter is 'A' or 'a')
                {
                    inPcl = true;
                    break;
                }
                if (letter is 'W' or 'w' && value > 0)
                {
                    i = (int)Math.Min(text.Length, (long)i + value);
                }
                if (letter is >= 'A' and <= 'Z' or '@')
                {
                    break; // an uppercase letter ends the sequence; a lowercase one joins the next command to it
                }
            }
            if (!inPcl)
            {
                return i;
            }
        }
        return i;
    }

    /// <summary>
    /// HP-GL/2's encoded polyline: numbers in base 64 (or base 32 after "7"), the lowest digit first, the
    /// sign in the lowest bit, each ended by a terminating digit. Points are relative unless "=" comes
    /// before them; "&lt;" lifts the pen for the next point; "&gt;" sets how many bits are fraction; ":" picks a pen.
    /// </summary>
    private static int Encoded(string text, int i, Pen pen)
    {
        bool sevenBit = false, absolute = false, liftNext = false;
        double fraction = 1;
        double? x = null;
        bool wasDown = pen.Down;
        bool wasRelative = pen.Relative;
        pen.Lift();
        while (i < text.Length && text[i] != ';')
        {
            char c = text[i];
            switch (c)
            {
                case '7':
                    sevenBit = true;
                    i++;
                    continue;
                case '=':
                    absolute = true;
                    i++;
                    continue;
                case '<':
                    liftNext = true;
                    i++;
                    continue;
                case ':':
                    i = Number(text, i + 1, sevenBit, out _);
                    continue;
                case '>':
                    i = Number(text, i + 1, sevenBit, out long bits);
                    fraction = Math.Pow(2, Math.Clamp(bits, 0, 26));
                    continue;
            }
            if (c < '?')
            {
                i++;
                continue;
            }
            i = Number(text, i, sevenBit, out long value);
            if (x is null)
            {
                x = value / fraction;
                continue;
            }
            pen.Relative = !absolute;
            pen.Down = !liftNext;
            pen.Move(x.Value, value / fraction);
            x = null;
            absolute = false;
            liftNext = false;
        }
        pen.Lift();
        pen.Down = wasDown;
        pen.Relative = wasRelative;
        return i;
    }

    private static int Number(string text, int i, bool sevenBit, out long value)
    {
        int digitBase = sevenBit ? 32 : 64;
        int lastStart = sevenBit ? 95 : 191;
        long magnitude = 0, place = 1;
        while (i < text.Length && place < (1L << 40))
        {
            int b = text[i++];
            bool last = b >= lastStart && b < lastStart + digitBase;
            if (b is < 63 || (!last && b >= 63 + digitBase))
            {
                break;
            }
            magnitude += (last ? b - lastStart : b - 63) * place;
            place *= digitBase;
            if (last)
            {
                break;
            }
        }
        value = (magnitude & 1) == 1 ? -(magnitude >> 1) : magnitude >> 1;
        return i;
    }
}
