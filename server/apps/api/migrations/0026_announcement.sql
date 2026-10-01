-- Custom announcements created by players via `POST /api/announcement/v1/create`.
-- These are merged with the static announcements.json at read time.
CREATE TABLE IF NOT EXISTS announcement (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	announcement_id INTEGER NOT NULL UNIQUE,
	title TEXT NOT NULL,
	body TEXT NOT NULL,
	image_name TEXT,
	link_uri TEXT,
	link_label TEXT,
	created_by INTEGER NOT NULL,
	created_at TEXT NOT NULL DEFAULT (datetime('now')),
	expires_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_announcement_created ON announcement(created_at DESC);
