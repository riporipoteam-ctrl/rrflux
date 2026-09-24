/**
 * Flux Social account-linking storage on the shared `recflare` D1 database
 * (owned by the `auth` worker, migration 0010). Consumed by the `accounts`
 * worker's `/account/me/fluxsocial/*` (game client) and `/fluxsocial/*`
 * (Flux Social website) routes.
 *
 * Two credential kinds, both following the refresh_tokens pattern
 * (apps/auth/src/refresh-db.ts): only a SHA-256 hash is ever stored, never the
 * raw value.
 *
 * - Pairing codes: 6-digit numeric codes minted in-game, typed into the website.
 *   Single-use (redeeming deletes the row), 10-minute expiry. Minting a new code
 *   invalidates any previous unconsumed code for the account.
 * - Website sessions: opaque high-entropy tokens minted on code exchange. One
 *   row per account — relinking replaces it (old token dies), unlinking deletes
 *   it (instant revocation). A year of life; `last_used_at` is informational.
 *
 * Privacy toggles live in `flux_social_privacy`; all default to visible (1) and
 * are created lazily on first read.
 */

/** Pairing codes live this long (s) before the website must ask for a fresh one. */
export const LINK_CODE_TTL_SECONDS = 10 * 60 // 10 minutes

/** Website sessions live this long (s); unlinking revokes them immediately. */
export const WEBSITE_SESSION_TTL_SECONDS = 365 * 24 * 60 * 60 // 1 year

/** Exchange attempts allowed per IP per throttle window before 429. */
export const EXCHANGE_ATTEMPTS_PER_WINDOW = 20

/** Throttle window (s) for code-exchange attempts. */
export const EXCHANGE_THROTTLE_WINDOW_SECONDS = 10 * 60 // 10 minutes

/** Schema DDL (mirror of apps/auth/migrations/0010_fluxsocial.sql). */
export const FLUXSOCIAL_SCHEMA_DDL: string[] = [
	`CREATE TABLE IF NOT EXISTS flux_link_codes (
		code_hash TEXT PRIMARY KEY,
		account_id INTEGER NOT NULL,
		created_at INTEGER NOT NULL,
		expires_at INTEGER NOT NULL
	)`,
	`CREATE INDEX IF NOT EXISTS idx_flux_link_codes_account ON flux_link_codes (account_id)`,
	`CREATE INDEX IF NOT EXISTS idx_flux_link_codes_expires ON flux_link_codes (expires_at)`,
	`CREATE TABLE IF NOT EXISTS flux_social_links (
		account_id INTEGER PRIMARY KEY,
		session_hash TEXT NOT NULL,
		created_at INTEGER NOT NULL,
		last_used_at INTEGER NOT NULL
	)`,
	`CREATE TABLE IF NOT EXISTS flux_social_privacy (
		account_id INTEGER PRIMARY KEY,
		show_profile INTEGER NOT NULL DEFAULT 1,
		show_rooms INTEGER NOT NULL DEFAULT 1,
		show_photos INTEGER NOT NULL DEFAULT 1,
		show_inventions INTEGER NOT NULL DEFAULT 1,
		updated_at INTEGER NOT NULL
	)`,
	`CREATE TABLE IF NOT EXISTS flux_exchange_attempts (
		ip TEXT PRIMARY KEY,
		attempts INTEGER NOT NULL,
		window_start INTEGER NOT NULL
	)`,
]

/** SHA-256 hex of a code or token. Both are high-entropy random, so no salt. */
async function sha256hex(value: string): Promise<string> {
	const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value))
	return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('')
}

/** A 6-digit numeric pairing code, zero-padded (`crypto.getRandomValues`, not Math.random). */
function randomLinkCode(): string {
	const buf = new Uint32Array(1)
	crypto.getRandomValues(buf)
	return String(buf[0] % 1_000_000).padStart(6, '0')
}

/**
 * Mint a pairing code for the account, returning the RAW code — the only moment
 * it exists in plaintext (only its hash is stored). Any previous unconsumed code
 * for the account is invalidated first, so only the newest code on screen works.
 */
export async function issueLinkCode(db: D1Database, accountId: number): Promise<string> {
	const code = randomLinkCode()
	const now = Math.floor(Date.now() / 1000)
	await db.batch([
		db.prepare(`DELETE FROM flux_link_codes WHERE account_id = ?1`).bind(accountId),
		db
			.prepare(
				`INSERT INTO flux_link_codes (code_hash, account_id, created_at, expires_at)
				 VALUES (?1, ?2, ?3, ?4)`
			)
			.bind(await sha256hex(code), accountId, now, now + LINK_CODE_TTL_SECONDS),
	])
	return code
}

/** Why a code redemption failed — the caller maps these to status codes. */
export type RedeemResult = { ok: true; accountId: number } | { ok: false; reason: 'invalid' | 'expired' }

/**
 * Redeem a pairing code: single-use (the row is deleted whether it succeeds or
 * is expired, so a code can never be tried twice). Returns the account that
 * minted it — the ownership binding the website session inherits.
 */
export async function redeemLinkCode(db: D1Database, code: string): Promise<RedeemResult> {
	const now = Math.floor(Date.now() / 1000)
	const row = await db
		.prepare(`SELECT account_id, expires_at FROM flux_link_codes WHERE code_hash = ?1`)
		.bind(await sha256hex(code.trim()))
		.first<{ account_id: number; expires_at: number }>()
	if (!row) return { ok: false, reason: 'invalid' }
	// Burn the code even when it is expired — a dead code stays dead.
	await db.prepare(`DELETE FROM flux_link_codes WHERE code_hash = ?1`).bind(await sha256hex(code.trim())).run()
	if (row.expires_at <= now) return { ok: false, reason: 'expired' }
	return { ok: true, accountId: row.account_id }
}

/**
 * Mint a website session token for the account, returning the RAW token — the
 * only moment it exists in plaintext (only its hash is stored). One session per
 * account: relinking replaces the row, so a previous website token stops
 * working the moment a new code is exchanged.
 */
export async function issueWebsiteSession(db: D1Database, accountId: number): Promise<string> {
	const token = `${crypto.randomUUID()}${crypto.randomUUID()}`.replace(/-/g, '')
	const now = Math.floor(Date.now() / 1000)
	await db
		.prepare(
			`INSERT INTO flux_social_links (account_id, session_hash, created_at, last_used_at)
			 VALUES (?1, ?2, ?3, ?4)
			 ON CONFLICT (account_id) DO UPDATE SET
			   session_hash = excluded.session_hash,
			   created_at = excluded.created_at,
			   last_used_at = excluded.last_used_at`
		)
		.bind(accountId, await sha256hex(token), now, now)
		.run()
	return token
}

/**
 * Resolve a website session token to its account id, or `null` when the token
 * is unknown, revoked, or expired. Touches `last_used_at` on success.
 */
export async function getAccountIdBySession(db: D1Database, token: string): Promise<number | null> {
	const now = Math.floor(Date.now() / 1000)
	const row = await db
		.prepare(
			`SELECT account_id, created_at FROM flux_social_links WHERE session_hash = ?1`
		)
		.bind(await sha256hex(token))
		.first<{ account_id: number; created_at: number }>()
	if (!row) return null
	if (row.created_at + WEBSITE_SESSION_TTL_SECONDS <= now) {
		await db.prepare(`DELETE FROM flux_social_links WHERE account_id = ?1`).bind(row.account_id).run()
		return null
	}
	await db
		.prepare(`UPDATE flux_social_links SET last_used_at = ?1 WHERE account_id = ?2`)
		.bind(now, row.account_id)
		.run()
	return row.account_id
}

/** Revoke the account's website session (unlink). Idempotent — no row, no-op. */
export async function revokeWebsiteSession(db: D1Database, accountId: number): Promise<void> {
	await db.prepare(`DELETE FROM flux_social_links WHERE account_id = ?1`).bind(accountId).run()
}

/** Whether the account currently has a live website session. */
export async function hasWebsiteSession(db: D1Database, accountId: number): Promise<boolean> {
	const row = await db
		.prepare(`SELECT 1 AS ok FROM flux_social_links WHERE account_id = ?1`)
		.bind(accountId)
		.first<{ ok: number }>()
	return row !== null
}

/** The Flux Social visibility toggles for an account. */
export interface FluxSocialPrivacy {
	showProfile: boolean
	showRooms: boolean
	showPhotos: boolean
	showInventions: boolean
}

/** Read the account's privacy toggles, creating the all-visible default row lazily. */
export async function getPrivacy(db: D1Database, accountId: number): Promise<FluxSocialPrivacy> {
	const row = await db
		.prepare(
			`SELECT show_profile, show_rooms, show_photos, show_inventions
			 FROM flux_social_privacy WHERE account_id = ?1`
		)
		.bind(accountId)
		.first<{ show_profile: number; show_rooms: number; show_photos: number; show_inventions: number }>()
	if (!row) {
		const now = Math.floor(Date.now() / 1000)
		await db
			.prepare(`INSERT OR IGNORE INTO flux_social_privacy (account_id, updated_at) VALUES (?1, ?2)`)
			.bind(accountId, now)
			.run()
		return { showProfile: true, showRooms: true, showPhotos: true, showInventions: true }
	}
	return {
		showProfile: row.show_profile === 1,
		showRooms: row.show_rooms === 1,
		showPhotos: row.show_photos === 1,
		showInventions: row.show_inventions === 1,
	}
}

/** Replace the account's privacy toggles wholesale. */
export async function setPrivacy(
	db: D1Database,
	accountId: number,
	privacy: FluxSocialPrivacy
): Promise<void> {
	const now = Math.floor(Date.now() / 1000)
	await db
		.prepare(
			`INSERT INTO flux_social_privacy
			   (account_id, show_profile, show_rooms, show_photos, show_inventions, updated_at)
			 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
			 ON CONFLICT (account_id) DO UPDATE SET
			   show_profile = excluded.show_profile,
			   show_rooms = excluded.show_rooms,
			   show_photos = excluded.show_photos,
			   show_inventions = excluded.show_inventions,
			   updated_at = excluded.updated_at`
		)
		.bind(
			accountId,
			privacy.showProfile ? 1 : 0,
			privacy.showRooms ? 1 : 0,
			privacy.showPhotos ? 1 : 0,
			privacy.showInventions ? 1 : 0,
			now
		)
		.run()
}

/**
 * Brute-force throttle for code exchange: at most EXCHANGE_ATTEMPTS_PER_WINDOW
 * attempts per IP per EXCHANGE_THROTTLE_WINDOW_SECONDS. Returns `true` when the
 * attempt is allowed (and counts it), `false` when the IP is throttled.
 */
export async function checkExchangeThrottle(db: D1Database, ip: string): Promise<boolean> {
	const now = Math.floor(Date.now() / 1000)
	const windowStart = now - EXCHANGE_THROTTLE_WINDOW_SECONDS
	const row = await db
		.prepare(`SELECT attempts, window_start FROM flux_exchange_attempts WHERE ip = ?1`)
		.bind(ip)
		.first<{ attempts: number; window_start: number }>()
	if (!row || row.window_start < windowStart) {
		// New window (or first sighting): reset the counter and allow.
		await db
			.prepare(
				`INSERT INTO flux_exchange_attempts (ip, attempts, window_start) VALUES (?1, 1, ?2)
				 ON CONFLICT (ip) DO UPDATE SET attempts = 1, window_start = excluded.window_start`
			)
			.bind(ip, now)
			.run()
		return true
	}
	if (row.attempts >= EXCHANGE_ATTEMPTS_PER_WINDOW) return false
	await db
		.prepare(`UPDATE flux_exchange_attempts SET attempts = attempts + 1 WHERE ip = ?1`)
		.bind(ip)
		.run()
	return true
}

/** Clear a throttled (or merely counted) IP after a successful exchange. */
export async function clearExchangeThrottle(db: D1Database, ip: string): Promise<void> {
	await db.prepare(`DELETE FROM flux_exchange_attempts WHERE ip = ?1`).bind(ip).run()
}
