-- Checklist completion ledger for New Player challenges.
-- Records which checklist rows (ItemIndex) each account has completed,
-- making the 25 XP + 25 token grant once-only (idempotent).
-- Mirrors the reward_status pattern.

CREATE TABLE IF NOT EXISTS checklist_status (
	account_id INTEGER NOT NULL,
	item_index INTEGER NOT NULL,
	completed_at TEXT NOT NULL,
	PRIMARY KEY (account_id, item_index)
);
