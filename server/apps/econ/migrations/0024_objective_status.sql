-- Daily objective progress, owned by the `econ` worker. One row per (account, group,
-- index): the client reports objective progress to `/api/objectives/v1/updateobjective`,
-- which upserts here; `/api/objectives/v1/myprogress` reads the rows back to stamp
-- per-player progress.
--
-- `group` is the objective group (0-6 for the 7 daily groups). `index` is the objective
-- index within the group. When all objectives in a group are complete, the group is
-- marked completed and the reward can be claimed via `/api/objectives/v1/completegroup`.
--
-- Kept in sync with OBJECTIVE_STATUS_SCHEMA_DDL in src/objective-db.ts.

CREATE TABLE IF NOT EXISTS objective_status (
  account_id INTEGER NOT NULL,
  objective_group INTEGER NOT NULL,
  objective_index INTEGER NOT NULL,
  progress INTEGER NOT NULL DEFAULT 0,
  is_completed INTEGER NOT NULL DEFAULT 0,
  has_claimed_reward INTEGER NOT NULL DEFAULT 0,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (account_id, objective_group, objective_index)
);

CREATE TABLE IF NOT EXISTS objective_group_status (
  account_id INTEGER NOT NULL,
  objective_group INTEGER NOT NULL,
  is_completed INTEGER NOT NULL DEFAULT 0,
  cleared_at TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (account_id, objective_group)
);

CREATE INDEX IF NOT EXISTS idx_objective_status_account_group ON objective_status (account_id, objective_group);
CREATE INDEX IF NOT EXISTS idx_objective_group_status_account ON objective_group_status (account_id);
