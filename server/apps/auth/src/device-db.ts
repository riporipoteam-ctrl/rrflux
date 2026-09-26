/**
 * OAuth device authorization flow (RFC 8628) storage on the shared `recflare` D1
 * database (owned by the `auth` worker, migration 0010).
 *
 * This is what the iOS client (20230414 build, `connect/deviceauthorization` /
 * `connect/token` with the `urn:ietf:params:oauth:grant-type:device_code` grant)
 * uses for "login with another device": the phone shows a short `user_code`, the
 * player opens `verification_uri` in a browser on a device where they can type
 * their username and password, and the phone polls until the flow is approved.
 *
 * Only a SHA-256 hash of the high-entropy `device_code` is stored — never the
 * raw value, same as refresh tokens. The 8-character `user_code` is stored in
 * plaintext because the approval page looks the flow up by it; it lives for at
 * most 10 minutes.
 */

/** Device codes live this long (s) before the flow expires. */
export const DEVICE_CODE_TTL_SECONDS = 10 * 60

/** Suggested poll interval (s) returned to the client. */
export const DEVICE_POLL_INTERVAL_SECONDS = 5

/** Alphabet without lookalikes (no 0/O, 1/I/L) for the human-typed code. */
const USER_CODE_ALPHABET = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789'

/** Schema DDL (mirror of migrations/0010_device_codes.sql). */
export const DEVICE_SCHEMA_DDL: string[] = [
	`CREATE TABLE IF NOT EXISTS device_codes (
		device_code_hash TEXT PRIMARY KEY,
		user_code TEXT NOT NULL UNIQUE,
		status TEXT NOT NULL DEFAULT 'pending',
		account_id INTEGER,
		created_at INTEGER NOT NULL,
		expires_at INTEGER NOT NULL,
		last_poll_at INTEGER,
		interval_seconds INTEGER NOT NULL DEFAULT 5
	)`,
	`CREATE INDEX IF NOT EXISTS idx_device_codes_user ON device_codes (user_code)`,
	`CREATE INDEX IF NOT EXISTS idx_device_codes_expires ON device_codes (expires_at)`,
]

/** SHA-256 hex of the device code. Codes are high-entropy random, so no salt is needed. */
async function hashDeviceCode(code: string): Promise<string> {
	const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(code))
	return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('')
}

function randomUserCode(): string {
	const bytes = crypto.getRandomValues(new Uint8Array(8))
	return [...bytes].map((b) => USER_CODE_ALPHABET[b % USER_CODE_ALPHABET.length]).join('')
}

export interface IssuedDeviceCode {
	deviceCode: string
	userCode: string
	expiresIn: number
	interval: number
}

/**
 * Ensure the device_codes table exists. Safe to call on every request —
 * uses IF NOT EXISTS. This fixes the 500 on /connect/deviceauthorization
 * when the D1 migration hasn't been applied.
 */
export async function ensureDeviceSchema(db: D1Database): Promise<void> {
	for (const stmt of DEVICE_SCHEMA_DDL) {
		await db.prepare(stmt).run()
	}
}

/**
 * Start a device flow: mint a device/user code pair and persist the pending
 * row. Expired rows are swept opportunistically so the table stays small.
 */
export async function issueDeviceCode(db: D1Database): Promise<IssuedDeviceCode> {
	// Ensure table exists (fixes 500 when migration not applied)
	await ensureDeviceSchema(db)
	const now = Math.floor(Date.now() / 1000)
	// 32 random bytes as hex — high entropy, never stored raw.
	const bytes = crypto.getRandomValues(new Uint8Array(32))
	const deviceCode = [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('')
	// A user_code collision is a 1-in-2^40 event; retry the insert a few times
	// instead of failing the login when the UNIQUE constraint fires.
	for (let attempt = 1; ; attempt++) {
		const userCode = randomUserCode()
		try {
			await db
				.prepare(
					`INSERT INTO device_codes
					 (device_code_hash, user_code, status, created_at, expires_at, interval_seconds)
					 VALUES (?1, ?2, 'pending', ?3, ?4, ?5)`
				)
				.bind(
					await hashDeviceCode(deviceCode),
					userCode,
					now,
					now + DEVICE_CODE_TTL_SECONDS,
					DEVICE_POLL_INTERVAL_SECONDS
				)
				.run()
			// Expired rows are swept opportunistically so the table stays small.
			await db
				.prepare(`DELETE FROM device_codes WHERE expires_at < ?1`)
				.bind(now)
				.run()
				.catch(() => {})
			return { deviceCode, userCode, expiresIn: DEVICE_CODE_TTL_SECONDS, interval: DEVICE_POLL_INTERVAL_SECONDS }
		} catch (err) {
			if (attempt >= 3) throw err
		}
	}
}

export type DevicePollStatus = 'pending' | 'approved' | 'denied'

export interface DevicePollResult {
	status: DevicePollStatus
	/** Throttled: the client polled faster than `interval_seconds`. */
	slowDown: boolean
}

/**
 * Look up a flow by its device code. Unknown or expired codes return null;
 * expired rows are deleted on sight. Records the poll time so clients polling
 * faster than the advertised interval get `slow_down`.
 */
export async function pollDeviceCode(db: D1Database, deviceCode: string): Promise<DevicePollResult | null> {
	await ensureDeviceSchema(db)
	const now = Math.floor(Date.now() / 1000)
	const row = await db
		.prepare(
			`SELECT status, expires_at AS expiresAt, last_poll_at AS lastPollAt, interval_seconds AS intervalSeconds
			 FROM device_codes WHERE device_code_hash = ?1`
		)
		.bind(await hashDeviceCode(deviceCode))
		.first<{ status: string; expiresAt: number; lastPollAt: number | null; intervalSeconds: number }>()
	if (!row) return null
	if (row.expiresAt < now) {
		await db
			.prepare(`DELETE FROM device_codes WHERE device_code_hash = ?1`)
			.bind(await hashDeviceCode(deviceCode))
			.run()
			.catch(() => {})
		return null
	}
	if (row.status !== 'pending' && row.status !== 'approved' && row.status !== 'denied') return null
	const slowDown = row.lastPollAt != null && now - row.lastPollAt < row.intervalSeconds
	await db
		.prepare(`UPDATE device_codes SET last_poll_at = ?1 WHERE device_code_hash = ?2`)
		.bind(now, await hashDeviceCode(deviceCode))
		.run()
		.catch(() => {})
	return { status: row.status as DevicePollStatus, slowDown }
}

/**
 * Atomically consume an APPROVED device code: single-use, so a concurrent
 * second token request finds no row. Returns the approving account id, or null
 * if the code is unknown, expired, or not approved.
 */
export async function consumeDeviceCode(db: D1Database, deviceCode: string): Promise<number | null> {
	await ensureDeviceSchema(db)
	const now = Math.floor(Date.now() / 1000)
	const row = await db
		.prepare(
			`DELETE FROM device_codes
			 WHERE device_code_hash = ?1 AND status = 'approved' AND expires_at >= ?2
			 RETURNING account_id AS accountId`
		)
		.bind(await hashDeviceCode(deviceCode), now)
		.first<{ accountId: number | null }>()
		.catch(() => null)
	if (!row || row.accountId == null) return null
	return row.accountId
}

/**
 * Approve a pending flow by its human-typed user code, binding the approving
 * account. Returns false when the code is unknown, expired, or not pending.
 */
export async function approveDeviceCode(db: D1Database, userCode: string, accountId: number): Promise<boolean> {
	await ensureDeviceSchema(db)
	const now = Math.floor(Date.now() / 1000)
	const res = await db
		.prepare(
			`UPDATE device_codes SET status = 'approved', account_id = ?1
			 WHERE upper(user_code) = upper(?2) AND status = 'pending' AND expires_at >= ?3`
		)
		.bind(accountId, userCode.trim(), now)
		.run()
		.catch(() => null)
	return res != null && res.meta.changes > 0
}

/**
 * Deny a pending flow by its user code. Returns false when the code is
 * unknown, expired, or not pending.
 */
export async function denyDeviceCode(db: D1Database, userCode: string): Promise<boolean> {
	const now = Math.floor(Date.now() / 1000)
	const res = await db
		.prepare(
			`UPDATE device_codes SET status = 'denied'
			 WHERE upper(user_code) = upper(?1) AND status = 'pending' AND expires_at >= ?2`
		)
		.bind(userCode.trim(), now)
		.run()
		.catch(() => null)
	return res != null && res.meta.changes > 0
}

/**
 * Read the pending flow behind a user code, for rendering the approval page.
 * Returns null when the code is unknown, expired, or already decided.
 */
export async function getPendingDeviceFlow(
	db: D1Database,
	userCode: string
): Promise<{ userCode: string; expiresIn: number } | null> {
	const now = Math.floor(Date.now() / 1000)
	const row = await db
		.prepare(
			`SELECT user_code AS userCode, expires_at AS expiresAt FROM device_codes
			 WHERE upper(user_code) = upper(?1) AND status = 'pending' AND expires_at >= ?2`
		)
		.bind(userCode.trim(), now)
		.first<{ userCode: string; expiresAt: number }>()
		.catch(() => null)
	if (!row) return null
	return { userCode: row.userCode, expiresIn: Math.max(0, row.expiresAt - now) }
}
