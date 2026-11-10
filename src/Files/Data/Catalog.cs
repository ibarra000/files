using Microsoft.Data.Sqlite;

namespace Files.Data;

public enum VolumeStatus
{
    Offline,
    Online,
    Scanning,
}

/// <summary>Why the last check or scan of a volume went wrong.</summary>
public enum VolumeError
{
    PathNotFound,
    DriveNotFound,
    PermissionDenied,
}

public sealed record Volume(
    long Id,
    Guid Uid,
    string Root,
    string Type,
    string Name,
    VolumeStatus Status,
    VolumeError? LastError,
    DateTime? LastScannedAt);

/// <summary>The list of volumes, in <c>catalog.db</c>. Each volume's names live in its own shard.</summary>
public sealed class Catalog(string path)
{
    private readonly string _connectionString = new SqliteConnectionStringBuilder
    {
        DataSource = path,
        ForeignKeys = true,
    }.ToString();

    public string Path { get; } = path;

    public static Catalog Open(string path)
    {
        Directory.CreateDirectory(System.IO.Path.GetDirectoryName(System.IO.Path.GetFullPath(path))!);
        var catalog = new Catalog(path);
        using SqliteConnection connection = catalog.Connect();
        Migrations.Run(connection, "catalog");
        return catalog;
    }

    public IReadOnlyList<Volume> All()
    {
        using SqliteConnection connection = Connect();
        using SqliteCommand command = connection.CreateCommand();
        command.CommandText = "SELECT id, uid, root, vtype, name, status, last_error, last_scanned_at FROM volumes ORDER BY name, id";
        using SqliteDataReader reader = command.ExecuteReader();
        var volumes = new List<Volume>();
        while (reader.Read())
        {
            volumes.Add(new Volume(
                reader.GetInt64(0),
                new Guid((byte[])reader[1]),
                reader.GetString(2),
                reader.GetString(3),
                reader.GetString(4),
                Enum.Parse<VolumeStatus>(reader.GetString(5), ignoreCase: true),
                reader.IsDBNull(6) ? null : ErrorFromText(reader.GetString(6)),
                reader.IsDBNull(7) ? null : DateTime.Parse(reader.GetString(7), null, System.Globalization.DateTimeStyles.AdjustToUniversal)));
        }
        return volumes;
    }

    /// <summary>Adds a volume. Throws <see cref="SqliteException"/> if the root is already a volume.</summary>
    public Volume Add(string name, string folder)
    {
        string root = VolumeRoot.Normalize(folder);
        using SqliteConnection connection = Connect();
        using SqliteCommand command = connection.CreateCommand();
        command.CommandText = "INSERT INTO volumes (uid, root, vtype, name) VALUES ($uid, $root, $vtype, $name)";
        command.Parameters.AddWithValue("$uid", Guid.NewGuid().ToByteArray());
        command.Parameters.AddWithValue("$root", root);
        command.Parameters.AddWithValue("$vtype", VolumeRoot.TypeOf(root));
        command.Parameters.AddWithValue("$name", name.Trim());
        command.ExecuteNonQuery();
        return All().Single(volume => volume.Root == root);
    }

    public void Remove(long id) => Update("DELETE FROM volumes WHERE id = $id", id);

    public void SetStatus(long id, VolumeStatus status, VolumeError? error)
    {
        Update("UPDATE volumes SET status = $status, last_error = $error WHERE id = $id", id,
            ("$status", status.ToString().ToLowerInvariant()),
            ("$error", error is VolumeError e ? ErrorToText(e) : DBNull.Value));
    }

    public void MarkScanned(long id)
    {
        Update("UPDATE volumes SET last_scanned_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = $id", id);
    }

    private void Update(string sql, long id, params (string Name, object Value)[] parameters)
    {
        using SqliteConnection connection = Connect();
        using SqliteCommand command = connection.CreateCommand();
        command.CommandText = sql;
        command.Parameters.AddWithValue("$id", id);
        foreach (var (name, value) in parameters)
        {
            command.Parameters.AddWithValue(name, value);
        }
        command.ExecuteNonQuery();
    }

    private SqliteConnection Connect()
    {
        var connection = new SqliteConnection(_connectionString);
        connection.Open();
        return connection;
    }

    private static string ErrorToText(VolumeError error) => error switch
    {
        VolumeError.PathNotFound => "path_not_found",
        VolumeError.DriveNotFound => "drive_not_found",
        _ => "permission_denied",
    };

    private static VolumeError ErrorFromText(string text) => text switch
    {
        "path_not_found" => VolumeError.PathNotFound,
        "drive_not_found" => VolumeError.DriveNotFound,
        _ => VolumeError.PermissionDenied,
    };
}
