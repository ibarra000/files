using System.Reflection;
using Microsoft.Data.Sqlite;

namespace Files.Data;

/// <summary>
/// Brings a database up to date by running the embedded scripts
/// <c>{kind}-1.sql</c>, <c>{kind}-2.sql</c>, ... above its <c>user_version</c>,
/// each in its own transaction.
/// </summary>
internal static class Migrations
{
    public static void Run(SqliteConnection connection, string kind)
    {
        long version = Scalar<long>(connection, "PRAGMA user_version");
        for (long next = version + 1; Script(kind, next) is string script; next++)
        {
            using SqliteTransaction transaction = connection.BeginTransaction();
            Execute(connection, script, transaction);
            // PRAGMA takes no parameters; `next` is a number we produced.
            Execute(connection, $"PRAGMA user_version = {next}", transaction);
            transaction.Commit();
        }
    }

    public static T Scalar<T>(SqliteConnection connection, string sql)
    {
        using SqliteCommand command = connection.CreateCommand();
        command.CommandText = sql;
        return (T)Convert.ChangeType(command.ExecuteScalar()!, typeof(T));
    }

    public static void Execute(SqliteConnection connection, string sql, SqliteTransaction? transaction = null)
    {
        using SqliteCommand command = connection.CreateCommand();
        command.Transaction = transaction;
        command.CommandText = sql;
        command.ExecuteNonQuery();
    }

    private static string? Script(string kind, long version)
    {
        using Stream? stream = Assembly.GetExecutingAssembly().GetManifestResourceStream($"{kind}-{version}.sql");
        if (stream is null)
        {
            return null;
        }
        using var reader = new StreamReader(stream);
        return reader.ReadToEnd();
    }
}
