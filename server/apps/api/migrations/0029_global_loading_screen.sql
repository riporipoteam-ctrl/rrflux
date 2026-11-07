-- The shared room service reads this operator-managed loading-screen singleton
-- while constructing room metadata. Keep the migration here too so API-only
-- development deployments create the table before the bot command is used.
CREATE TABLE IF NOT EXISTS global_loading_screen (
	id INTEGER PRIMARY KEY CHECK (id = 1),
	data TEXT NOT NULL
);
