using Files.Core;
using Microsoft.Data.Sqlite;

namespace Files.Data;

/// <summary>
/// One volume's file names on disk: <c>volumes\&lt;uid&gt;.db</c>. A scan
/// writes a complete new shard beside the old one and renames it into place,
/// so a crash mid-write leaves the previous shard intact.
/// </summary>
public static class Shard
{
    public static string PathFor(string folder, Guid uid) => Path.Combine(folder, $"{uid:N}.db");

    public static void Write(string path, Guid uid, VolumeIndex index)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        string temporary = path + ".tmp";
        File.Delete(temporary);
        using (SqliteConnection connection = Connect(temporary, SqliteOpenMode.ReadWriteCreate))
        {
            // The file is renamed into place only once complete, so a journal buys nothing.
            Migrations.Execute(connection, "PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF");
            Migrations.Run(connection, "shard");
            using SqliteTransaction transaction = connection.BeginTransaction();
            Insert(connection, transaction, "INSERT INTO meta (volume_uid, scanned_at) VALUES ($a, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))", uid.ToByteArray());
            InsertFolders(connection, transaction, index);
            InsertFiles(connection, transaction, index);
            transaction.Commit();
        }
        File.Move(temporary, path, overwrite: true);
    }

    /// <summary>Loads a shard, or returns null when it is missing or belongs to another volume.</summary>
    public static VolumeIndex? Load(string path, Guid uid, string root)
    {
        if (!File.Exists(path))
        {
            return null;
        }
        using SqliteConnection connection = Connect(path, SqliteOpenMode.ReadOnly);
        using (SqliteCommand meta = connection.CreateCommand())
        {
            meta.CommandText = "SELECT volume_uid FROM meta";
            if (meta.ExecuteScalar() is not byte[] stored || new Guid(stored) != uid)
            {
                return null;
            }
        }

        var builder = new VolumeIndex.Builder(root);
        using SqliteCommand command = connection.CreateCommand();
        command.CommandText = "SELECT folders.path, files.name FROM folders LEFT JOIN files ON files.folder_id = folders.id ORDER BY folders.id, files.name";
        using SqliteDataReader reader = command.ExecuteReader();
        string? folder = null;
        var names = new List<string>();
        while (reader.Read())
        {
            string relative = reader.GetString(0);
            if (folder is not null && relative != folder)
            {
                builder.AddFolder(folder, names);
                names.Clear();
            }
            folder = relative;
            if (!reader.IsDBNull(1))
            {
                names.Add(reader.GetString(1));
            }
        }
        if (folder is not null)
        {
            builder.AddFolder(folder, names);
        }
        return builder.Build();
    }

    // OR IGNORE: Windows allows names that are not valid UTF-16. SQLite stores them
    // with a replacement character, so two such names can collide on the key.
    private static void InsertFolders(SqliteConnection connection, SqliteTransaction transaction, VolumeIndex index)
    {
        using SqliteCommand command = Prepare(connection, transaction, "INSERT OR IGNORE INTO folders (id, path) VALUES ($a, $b)");
        for (int folder = 0; folder < index.Folders.Count; folder++)
        {
            command.Parameters[0].Value = folder;
            command.Parameters[1].Value = index.Folders[folder];
            command.ExecuteNonQuery();
        }
    }

    private static void InsertFiles(SqliteConnection connection, SqliteTransaction transaction, VolumeIndex index)
    {
        using SqliteCommand command = Prepare(connection, transaction, "INSERT OR IGNORE INTO files (folder_id, name) VALUES ($a, $b)");
        for (int folder = 0; folder < index.Folders.Count; folder++)
        {
            foreach (int file in index.FilesIn(folder))
            {
                command.Parameters[0].Value = folder;
                command.Parameters[1].Value = index.Files[file];
                command.ExecuteNonQuery();
            }
        }
    }

    private static void Insert(SqliteConnection connection, SqliteTransaction transaction, string sql, object value)
    {
        using SqliteCommand command = connection.CreateCommand();
        command.Transaction = transaction;
        command.CommandText = sql;
        command.Parameters.AddWithValue("$a", value);
        command.ExecuteNonQuery();
    }

    private static SqliteCommand Prepare(SqliteConnection connection, SqliteTransaction transaction, string sql)
    {
        SqliteCommand command = connection.CreateCommand();
        command.Transaction = transaction;
        command.CommandText = sql;
        command.Parameters.Add("$a", SqliteType.Integer);
        command.Parameters.Add("$b", SqliteType.Text);
        command.Prepare();
        return command;
    }

    private static SqliteConnection Connect(string path, SqliteOpenMode mode)
    {
        // No pooling, so the file is closed (and can be renamed) when disposed.
        var connection = new SqliteConnection(new SqliteConnectionStringBuilder
        {
            DataSource = path,
            Mode = mode,
            Pooling = false,
        }.ToString());
        connection.Open();
        return connection;
    }
}
