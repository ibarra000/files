-- Migration 1: one volume's file names.
-- Each volume has its own shard file, named after its uid. A scan writes a
-- whole new shard and swaps it in, so rows are never updated in place.

PRAGMA application_id = 0x46494C45;  -- 'FILE': marks the file as ours

-- Exactly one row: which volume this shard belongs to, and when it was read.
CREATE TABLE meta (
    -- The catalog's volumes.uid. A shard whose uid does not match is ignored.
    volume_uid BLOB NOT NULL CHECK (length(volume_uid) = 16),

    -- UTC, ISO 8601 with a Z suffix, like the catalog.
    scanned_at TEXT NOT NULL
) STRICT;

-- Folders relative to the volume root. The root itself is id 0, path ''.
CREATE TABLE folders (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE
) STRICT;

-- File names, grouped by folder. The primary key is also the load order.
CREATE TABLE files (
    folder_id INTEGER NOT NULL REFERENCES folders (id),
    name TEXT NOT NULL CHECK (name <> ''),
    PRIMARY KEY (folder_id, name)
) STRICT, WITHOUT ROWID;
