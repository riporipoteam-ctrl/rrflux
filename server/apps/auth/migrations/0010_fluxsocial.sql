-- Flux Social account linking (owned by the `auth` worker; consumed by `accounts`).
-- Kept in sync with FLUXSOCIAL_SCHEMA_DDL in apps/accounts/src/fluxsocial-db.ts
-- (and the test seed in apps/accounts/src/test/integration/api.test.ts).
--
-- Pairing codes: the in-game client mints a 6-digit code the player types into the
-- Flux Social website. Only the SHA-256 hash is stored — never the raw code —
-- mirroring the refresh_tokens pattern (apps/auth/migrations/0003). Codes are
-- single-use (redeeming deletes the row) and expire after 10 minutes. A player
-- generating a new code invalidates any previous unconsumed one.
--
-- Website sessions: exchanging a code mints an opaque high-entropy session token
-- (only its SHA-256 hash is stored). One session per game account — relinking
-- replaces the row, so the old token stops working. Unlinking deletes the row,
-- which instantly revokes the website session. Sessions live a year; last_used_at
-- is informational only.
--
-- Privacy: per-account visibility toggles the Flux Social website honors (and that
-- future photo/room endpoints enforce server-side). All default to visible.
--
-- Exchange throttle: per-IP attempt counting stops 6-digit brute force. Codes are
-- 1-in-a-million; 20 attempts per 10-minute window per IP makes guessing useless.

CREATE TABLE IF NOT EXISTS flux_link_codes (
  code_hash TEXT PRIMARY KEY,
  account_id INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_flux_link_codes_account ON flux_link_codes (account_id);
CREATE INDEX IF NOT EXISTS idx_flux_link_codes_expires ON flux_link_codes (expires_at);

CREATE TABLE IF NOT EXISTS flux_social_links (
  account_id INTEGER PRIMARY KEY,
  session_hash TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS flux_social_privacy (
  account_id INTEGER PRIMARY KEY,
  show_profile INTEGER NOT NULL DEFAULT 1,
  show_rooms INTEGER NOT NULL DEFAULT 1,
  show_photos INTEGER NOT NULL DEFAULT 1,
  show_inventions INTEGER NOT NULL DEFAULT 1,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS flux_exchange_attempts (
  ip TEXT PRIMARY KEY,
  attempts INTEGER NOT NULL,
  window_start INTEGER NOT NULL
);
