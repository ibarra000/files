namespace Files.Services;

/// <summary>
/// A face's surface, parametrised by (u, v) so that ∂u × ∂v points along STEP's normal for it. A face is
/// shaded by laying its boundary out in (u, v), filling that, and lifting the fill back onto the surface.
/// </summary>
internal abstract class Surface
{
    protected const double AngleStep = Math.PI / 18; // 10°: a triangle's edge spans no more than this of a turn

    /// <summary>The period in u (2π for a turn), or 0 when u does not come round.</summary>
    public virtual double PeriodU => 0;

    public virtual double PeriodV => 0;

    /// <summary>The most of u one triangle may span before it is split; infinite where the surface is straight across u.</summary>
    public virtual double StepU => double.PositiveInfinity;

    public virtual double StepV => double.PositiveInfinity;

    public abstract Vector3 At(double u, double v);

    /// <summary>The unit normal at (u, v); its sign does not matter for shading.</summary>
    public abstract Vector3 Normal(double u, double v);

    /// <summary>(u, v) of a point on the surface; u is NaN where every u meets (at a pole).</summary>
    public abstract (double U, double V) Find(Vector3 point);

    /// <summary>
    /// For a face bounded by one loop that goes once round in u: the v it closes at, on the side
    /// <paramref name="side"/> (+1: larger v) — a sphere's pole or a cone's tip. Null when none.
    /// </summary>
    public virtual double? Pole(int side) => null;

    /// <summary>The (u, v) range of the whole surface, for a face with no boundary (a whole sphere); null when it has none.</summary>
    public virtual (double U0, double U1, double V0, double V1)? Whole => null;

    /// <summary>The surface a STEP surface entity describes, or null for a kind this app does not shade.</summary>
    public static Surface? From(StepFile file, Instance? surface)
    {
        if (surface is null)
        {
            return null;
        }
        if (surface.Name is "B_SPLINE_SURFACE_WITH_KNOTS" or "")
        {
            return BSplineSurface.Read(file, surface);
        }
        if (surface.Args.Count < 2 || file.Placement(surface.Args[1]) is not Frame at)
        {
            return null;
        }
        double Length(int index) => StepFile.Number(surface.Args.ElementAtOrDefault(index));
        static bool Size(double value) => double.IsFinite(value) && value > 0;
        return surface.Name switch
        {
            "PLANE" => new Plane(at),
            "CYLINDRICAL_SURFACE" when Size(Length(2)) => new Cylinder(at, Length(2)),
            "CONICAL_SURFACE" => Cone.From(at, Length(2), file.Angle(surface.Args.ElementAtOrDefault(3))),
            "SPHERICAL_SURFACE" when Size(Length(2)) => new Sphere(at, Length(2)),
            "TOROIDAL_SURFACE" or "DEGENERATE_TOROIDAL_SURFACE" when Size(Length(2)) && Size(Length(3)) => new Torus(at, Length(2), Length(3)),
            _ => null,
        };
    }

    /// <summary>The direction at angle <paramref name="u"/> round the frame's axis.</summary>
    protected static Vector3 Radial(Frame at, double u) => at.X * Math.Cos(u) + at.Y * Math.Sin(u);

    /// <summary>The angle round the frame's axis of a point given in its coordinates; NaN on the axis.</summary>
    protected static double AngleOf(Vector3 local) =>
        local.X * local.X + local.Y * local.Y < 1e-24 ? double.NaN : Math.Atan2(local.Y, local.X);
}

internal sealed class Plane(Frame at) : Surface
{
    public override Vector3 At(double u, double v) => at.Point(new Vector3(u, v, 0));

    public override Vector3 Normal(double u, double v) => at.Z;

    public override (double U, double V) Find(Vector3 point)
    {
        Vector3 local = at.Local(point);
        return (local.X, local.Y);
    }
}

/// <summary>u round the axis, v along it.</summary>
internal sealed class Cylinder(Frame at, double radius) : Surface
{
    public override double PeriodU => 2 * Math.PI;

    public override double StepU => AngleStep;

    public override Vector3 At(double u, double v) => at.Origin + Radial(at, u) * radius + at.Z * v;

    public override Vector3 Normal(double u, double v) => Radial(at, u);

    public override (double U, double V) Find(Vector3 point)
    {
        Vector3 local = at.Local(point);
        return (AngleOf(local), local.Z);
    }
}

/// <summary>u round the axis, v along it; the radius is <paramref name="radius"/> where v is 0 and grows by tan(semi-angle) per unit of v.</summary>
internal sealed class Cone(Frame at, double radius, double semiAngle) : Surface
{
    private readonly double _slope = Math.Tan(semiAngle);

    /// <summary>Null for a cone that is flat or a cylinder (a semi-angle of 0 or a right angle), or of no size.</summary>
    public static Cone? From(Frame at, double radius, double semiAngle) =>
        radius >= 0 && double.IsFinite(radius) && Math.Abs(semiAngle) > 1e-6 && Math.Abs(semiAngle) < Math.PI / 2 - 1e-6
            ? new Cone(at, radius, semiAngle)
            : null;

    public override double PeriodU => 2 * Math.PI;

    public override double StepU => AngleStep;

    public override Vector3 At(double u, double v) => at.Origin + Radial(at, u) * (radius + v * _slope) + at.Z * v;

    public override Vector3 Normal(double u, double v) => (Radial(at, u) * Math.Cos(semiAngle) - at.Z * Math.Sin(semiAngle)).Unit;

    public override (double U, double V) Find(Vector3 point)
    {
        Vector3 local = at.Local(point);
        return (AngleOf(local), local.Z);
    }

    /// <summary>The tip, where the radius comes to nothing.</summary>
    public override double? Pole(int side) => -radius / _slope;
}

/// <summary>u round the axis (longitude), v from the equator (latitude, −π/2 to π/2).</summary>
internal sealed class Sphere(Frame at, double radius) : Surface
{
    public override double PeriodU => 2 * Math.PI;

    public override double StepU => AngleStep;

    public override double StepV => AngleStep;

    public override Vector3 At(double u, double v) => at.Origin + Normal(u, v) * radius;

    public override Vector3 Normal(double u, double v) => Radial(at, u) * Math.Cos(v) + at.Z * Math.Sin(v);

    public override (double U, double V) Find(Vector3 point)
    {
        Vector3 local = at.Local(point);
        double length = local.Length;
        double v = length > 0 ? Math.Asin(Math.Clamp(local.Z / length, -1, 1)) : 0;
        return (Math.Abs(Math.Cos(v)) < 1e-9 ? double.NaN : AngleOf(local), v);
    }

    public override double? Pole(int side) => side > 0 ? Math.PI / 2 : -Math.PI / 2;

    public override (double U0, double U1, double V0, double V1)? Whole => (0, 2 * Math.PI, -Math.PI / 2, Math.PI / 2);
}

/// <summary>u round the main axis, v round the tube.</summary>
internal sealed class Torus(Frame at, double major, double minor) : Surface
{
    public override double PeriodU => 2 * Math.PI;

    public override double PeriodV => 2 * Math.PI;

    public override double StepU => AngleStep;

    public override double StepV => AngleStep;

    public override Vector3 At(double u, double v) => at.Origin + Radial(at, u) * major + Normal(u, v) * minor;

    public override Vector3 Normal(double u, double v) => Radial(at, u) * Math.Cos(v) + at.Z * Math.Sin(v);

    public override (double U, double V) Find(Vector3 point)
    {
        Vector3 local = at.Local(point);
        double u = AngleOf(local);
        double across = Math.Sqrt(local.X * local.X + local.Y * local.Y) - major;
        return (u, Math.Atan2(local.Z, across));
    }

    public override (double U0, double U1, double V0, double V1)? Whole => (0, 2 * Math.PI, 0, 2 * Math.PI);
}

/// <summary>
/// A B-spline surface (rational or not), evaluated by its basis functions (The NURBS Book, A2.1 and A2.2).
/// A point is found on it by the nearest of a grid of samples, then of finer grids round that one.
/// </summary>
internal sealed class BSplineSurface : Surface
{
    private const int MaxDegree = 15;
    private const int MaxControlPoints = 40_000;
    private const int Grid = 24;
    private const int Refine = 4;

    private readonly int _degreeU, _degreeV;
    private readonly Vector3[,] _points;
    private readonly double[,]? _weights;
    private readonly double[] _knotsU, _knotsV;
    private readonly double _u0, _u1, _v0, _v1;
    private Vector3[,]? _samples;

    private BSplineSurface(int degreeU, int degreeV, Vector3[,] points, double[,]? weights, double[] knotsU, double[] knotsV)
    {
        (_degreeU, _degreeV, _points, _weights, _knotsU, _knotsV) = (degreeU, degreeV, points, weights, knotsU, knotsV);
        (_u0, _u1) = (knotsU[degreeU], knotsU[points.GetLength(0)]);
        (_v0, _v1) = (knotsV[degreeV], knotsV[points.GetLength(1)]);
    }

    /// <summary>A triangle spans at most about one of the surface's spans (between knots), which is enough to follow its curve at preview size.</summary>
    public override double StepU => (_u1 - _u0) / Math.Clamp(_points.GetLength(0) - _degreeU + 1, 2, 12);

    public override double StepV => (_v1 - _v0) / Math.Clamp(_points.GetLength(1) - _degreeV + 1, 2, 12);

    public override (double U0, double U1, double V0, double V1)? Whole => (_u0, _u1, _v0, _v1);

    /// <summary>From B_SPLINE_SURFACE_WITH_KNOTS, or a complex entity of B_SPLINE_SURFACE, its knots and maybe weights.</summary>
    public static BSplineSurface? Read(StepFile file, Instance surface)
    {
        // The simple entity carries a name first and the knots after the surface's own arguments; a complex one splits them into parts.
        (Instance? shape, int offset) = surface.Name == "" ? (surface.Part("B_SPLINE_SURFACE"), 0) : (surface, 1);
        Instance? knots = surface.Name == "" ? surface.Part("B_SPLINE_SURFACE_WITH_KNOTS") : surface;
        int knotsAt = surface.Name == "" ? 0 : 8;
        if (shape is null || knots is null || shape.Args.Count < offset + 3 || knots.Args.Count < knotsAt + 4)
        {
            return null;
        }
        int degreeU = (int)StepFile.Number(shape.Args[offset]), degreeV = (int)StepFile.Number(shape.Args[offset + 1]);
        if (degreeU is < 1 or > MaxDegree || degreeV is < 1 or > MaxDegree || Grid2(shape.Args[offset + 2], file.Point3) is not Vector3[,] points)
        {
            return null;
        }
        double[,]? weights = null;
        if (surface.Name == "" && surface.Part("RATIONAL_B_SPLINE_SURFACE") is { Args.Count: >= 1 } rational)
        {
            weights = Grid2(rational.Args[0], value => value is double weight && weight > 0 && double.IsFinite(weight) ? (double?)weight : null);
            if (weights is null || weights.GetLength(0) != points.GetLength(0) || weights.GetLength(1) != points.GetLength(1))
            {
                return null;
            }
        }
        double[]? knotsU = Expand(knots.Args[knotsAt], knots.Args[knotsAt + 2], points.GetLength(0) + degreeU + 1);
        double[]? knotsV = Expand(knots.Args[knotsAt + 1], knots.Args[knotsAt + 3], points.GetLength(1) + degreeV + 1);
        if (knotsU is null || knotsV is null || points.GetLength(0) <= degreeU || points.GetLength(1) <= degreeV)
        {
            return null;
        }
        var made = new BSplineSurface(degreeU, degreeV, points, weights, knotsU, knotsV);
        return made._u1 > made._u0 && made._v1 > made._v0 ? made : null;
    }

    /// <summary>A rectangular list of lists, each item read by <paramref name="read"/>; null when ragged, too large or unreadable.</summary>
    private static T[,]? Grid2<T>(object? rows, Func<object?, T?> read) where T : struct
    {
        if (rows is not IReadOnlyList<object?> { Count: > 0 } list || list[0] is not IReadOnlyList<object?> { Count: > 0 } first
            || (long)list.Count * first.Count > MaxControlPoints)
        {
            return null;
        }
        var grid = new T[list.Count, first.Count];
        for (int i = 0; i < list.Count; i++)
        {
            if (list[i] is not IReadOnlyList<object?> row || row.Count != first.Count)
            {
                return null;
            }
            for (int j = 0; j < row.Count; j++)
            {
                if (read(row[j]) is not T value)
                {
                    return null;
                }
                grid[i, j] = value;
            }
        }
        return grid;
    }

    /// <summary>Knot values repeated by their multiplicities, which must make <paramref name="count"/> in rising order.</summary>
    private static double[]? Expand(object? multiplicities, object? values, int count)
    {
        if (multiplicities is not IReadOnlyList<object?> times || values is not IReadOnlyList<object?> knots || times.Count != knots.Count)
        {
            return null;
        }
        var expanded = new List<double>(count);
        for (int i = 0; i < knots.Count; i++)
        {
            double knot = StepFile.Number(knots[i]);
            int repeat = (int)StepFile.Number(times[i]);
            if (repeat < 1 || !double.IsFinite(knot) || repeat > count - expanded.Count || (expanded.Count > 0 && knot < expanded[^1]))
            {
                return null;
            }
            expanded.AddRange(Enumerable.Repeat(knot, repeat));
        }
        return expanded.Count == count ? [.. expanded] : null;
    }

    public override Vector3 At(double u, double v)
    {
        u = Math.Clamp(u, _u0, _u1);
        v = Math.Clamp(v, _v0, _v1);
        int spanU = KnotSpan(_knotsU, _degreeU, _points.GetLength(0) - 1, u);
        int spanV = KnotSpan(_knotsV, _degreeV, _points.GetLength(1) - 1, v);
        Span<double> basisU = stackalloc double[_degreeU + 1];
        Span<double> basisV = stackalloc double[_degreeV + 1];
        Basis(_knotsU, _degreeU, spanU, u, basisU);
        Basis(_knotsV, _degreeV, spanV, v, basisV);
        Vector3 sum = default;
        double weight = 0;
        for (int k = 0; k <= _degreeU; k++)
        {
            for (int l = 0; l <= _degreeV; l++)
            {
                int i = spanU - _degreeU + k, j = spanV - _degreeV + l;
                double w = basisU[k] * basisV[l] * (_weights?[i, j] ?? 1);
                sum += _points[i, j] * w;
                weight += w;
            }
        }
        return weight > 0 ? sum * (1 / weight) : sum;
    }

    /// <summary>From the two tangents, found by small steps either way; a collapsed side (a point at one end) is stepped off.</summary>
    public override Vector3 Normal(double u, double v)
    {
        double du = (_u1 - _u0) * 1e-3, dv = (_v1 - _v0) * 1e-3;
        double uc = Math.Clamp(u, _u0 + du, _u1 - du), vc = Math.Clamp(v, _v0 + dv, _v1 - dv);
        Vector3 normal = (At(uc + du, vc) - At(uc - du, vc)).Cross(At(uc, vc + dv) - At(uc, vc - dv));
        if (normal.Length == 0)
        {
            uc = Math.Clamp(uc, _u0 + 20 * du, _u1 - 20 * du);
            vc = Math.Clamp(vc, _v0 + 20 * dv, _v1 - 20 * dv);
            normal = (At(uc + du, vc) - At(uc - du, vc)).Cross(At(uc, vc + dv) - At(uc, vc - dv));
        }
        return normal.Unit;
    }

    public override (double U, double V) Find(Vector3 point)
    {
        _samples ??= Sample();
        int bestI = 0, bestJ = 0;
        double best = double.MaxValue;
        for (int i = 0; i <= Grid; i++)
        {
            for (int j = 0; j <= Grid; j++)
            {
                double distance = (_samples[i, j] - point).Dot(_samples[i, j] - point);
                if (distance < best)
                {
                    (best, bestI, bestJ) = (distance, i, j);
                }
            }
        }
        double u = _u0 + (_u1 - _u0) * bestI / Grid, v = _v0 + (_v1 - _v0) * bestJ / Grid;
        double spanU = (_u1 - _u0) / Grid, spanV = (_v1 - _v0) / Grid;
        for (int round = 0; round < 3; round++)
        {
            (u, v) = Nearest(point, u, v, spanU, spanV);
            spanU /= Refine / 2.0;
            spanV /= Refine / 2.0;
        }
        return (u, v);
    }

    /// <summary>The nearest of a <see cref="Refine"/>-step grid spanning one step either side of (u, v).</summary>
    private (double U, double V) Nearest(Vector3 point, double u, double v, double spanU, double spanV)
    {
        (double U, double V) bestAt = (u, v);
        double best = double.MaxValue;
        for (int i = 0; i <= Refine; i++)
        {
            for (int j = 0; j <= Refine; j++)
            {
                double tu = Math.Clamp(u - spanU + 2 * spanU * i / Refine, _u0, _u1);
                double tv = Math.Clamp(v - spanV + 2 * spanV * j / Refine, _v0, _v1);
                Vector3 d = At(tu, tv) - point;
                if (d.Dot(d) < best)
                {
                    (best, bestAt) = (d.Dot(d), (tu, tv));
                }
            }
        }
        return bestAt;
    }

    private Vector3[,] Sample()
    {
        var samples = new Vector3[Grid + 1, Grid + 1];
        for (int i = 0; i <= Grid; i++)
        {
            for (int j = 0; j <= Grid; j++)
            {
                samples[i, j] = At(_u0 + (_u1 - _u0) * i / Grid, _v0 + (_v1 - _v0) * j / Grid);
            }
        }
        return samples;
    }

    /// <summary>The knot span holding <paramref name="t"/> (A2.1); <paramref name="last"/> is the last control point's index.</summary>
    private static int KnotSpan(double[] knots, int degree, int last, double t)
    {
        if (t >= knots[last + 1])
        {
            return last;
        }
        int low = degree, high = last + 1;
        while (high - low > 1)
        {
            int middle = (low + high) / 2;
            if (t < knots[middle])
            {
                high = middle;
            }
            else
            {
                low = middle;
            }
        }
        return low;
    }

    /// <summary>The degree + 1 basis functions that are not zero in that span (A2.2).</summary>
    private static void Basis(double[] knots, int degree, int span, double t, Span<double> basis)
    {
        Span<double> left = stackalloc double[degree + 1];
        Span<double> right = stackalloc double[degree + 1];
        basis[0] = 1;
        for (int j = 1; j <= degree; j++)
        {
            left[j] = t - knots[span + 1 - j];
            right[j] = knots[span + j] - t;
            double saved = 0;
            for (int r = 0; r < j; r++)
            {
                double denominator = right[r + 1] + left[j - r];
                double share = denominator == 0 ? 0 : basis[r] / denominator;
                basis[r] = saved + right[r + 1] * share;
                saved = left[j - r] * share;
            }
            basis[j] = saved;
        }
    }
}
