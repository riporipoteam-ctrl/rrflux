-- Optional operator-managed loading screen shown while any room loads.
-- The room worker overlays this onto room metadata at read time; clearing the
-- singleton restores each room's saved LoadScreens without rewriting rooms.
CREATE TABLE IF NOT EXISTS global_loading_screen (
	id INTEGER PRIMARY KEY CHECK (id = 1),
	data TEXT NOT NULL
);
