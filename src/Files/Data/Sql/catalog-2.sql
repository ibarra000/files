-- Migration 2: live-only volumes.
-- A live-only volume is never read in the background and has no shard;
-- it is searched only by a live search (Shift+Enter).

ALTER TABLE volumes ADD COLUMN live_only INTEGER NOT NULL DEFAULT 0 CHECK (live_only IN (0, 1));
