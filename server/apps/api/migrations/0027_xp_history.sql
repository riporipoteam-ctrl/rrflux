-- XP grant history for `GET /api/players/v1/progression/xpEarnedToday`.
-- Every positive XP grant is logged here so "XP earned today" is computed
-- from real grants, not invented.
CREATE TABLE IF NOT EXISTS xp_history (
	id INTEGER PRIMARY KEY AUTOINCREMENT,
	account_id INTEGER NOT NULL,
	xp_delta INTEGER NOT NULL,
	created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_xp_history_account_day ON xp_history(account_id, created_at);
