/**
 * Flux Social website sessions on the shared `recflare` D1 database.
 *
 * The `accounts` worker owns the pairing endpoints and the `flux_social_links`
 * / `flux_social_privacy` tables (auth migration 0010), but other workers need
 * to resolve a website session token too: the `api` worker's Flux Social photo
 * routes accept the website session as auth (alongside the game JWT) so the
 * site can read and manage a player's photos. These helpers live here so every
 * worker validates sessions identically — never duplicate them per worker.
 *
 * Sessions follow the refresh_tokens pattern: only the SHA-256 hash of a token
 * is ever stored, never the raw value.
 */

/** Website sessions live this long (s); unlinking revokes them immediately. */
export const WEBSITE_SESSION_TTL_SECONDS = 365 * 24 * 60 * 60 // 1 year

/** SHA-256 hex of a token. Tokens are high-entropy random, so no salt. */
export async function sha256hex(value: string): Promise<string> {
	const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(value))
	return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('')
}

/**
 * Resolve a Flux Social website session token to its game account id, or
 * `null` when the token is unknown, revoked, or expired. Touches
 * `last_used_at` on success. A game JWT never matches a session hash, so
 * passing one through here is a safe no-op (returns null).
 */
export async function getAccountIdBySession(
	db: D1Database,
	token: string
): Promise<number | null> {
	const now = Math.floor(Date.now() / 1000)
	const row = await db
		.prepare(`SELECT account_id, created_at FROM flux_social_links WHERE session_hash = ?1`)
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
