import { Hono } from 'hono'
import { describeRoute } from 'hono-openapi'
import { z } from 'zod'

import {
	createGift,
	getAccountByUsername,
	getAccountsByIds,
	getGift,
	getRoomById,
	updateAccount,
} from '@repo/domain'
import { intVar, logger } from '@repo/hono-helpers'

// Token balances live in econ's balance table (shared `recflare` D1). Imported
// cross-worker the same way moderation.ts imports chat/notify modules.
import {
	creditCurrency,
	CurrencyType,
	DEFAULT_STARTING_TOKENS,
} from '../../../econ/src/balance-db'
import { json, jsonBody } from '../openapi'
import {
	ADMIN_BAN_DETAILS_PREFIX,
	adminBanReason,
	banFromReport,
	createReport,
	getActiveBan,
	getBansInForce,
} from '../reports-db'

import type { Context } from 'hono'
import type { App } from '../context'

/**
 * Operator admin API for the RipoBot Discord bot (Owner / Co-Owner commands).
 *
 * Auth is a shared admin key, NOT a player JWT: the caller sends it as the
 * `X-Admin-Key` header, and it must equal the `ADMIN_API_KEY` env var. The key
 * is never logged and never appears in a response body. Every endpoint answers
 * 401 `{ success: false, error: 'unauthorized' }` when the key is missing,
 * wrong, or not configured — deliberately the same answer in all three cases,
 * so a prober learns nothing.
 *
 * All bodies are JSON. Usernames are matched case-insensitively and must belong
 * to an account that already exists — there is no account creation here.
 */

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

/** Constant-time string compare (avoids leaking the key via timing). */
function timingSafeEqual(a: string, b: string): boolean {
	if (a.length !== b.length) return false
	let diff = 0
	for (let i = 0; i < a.length; i++) {
		diff |= a.charCodeAt(i) ^ b.charCodeAt(i)
	}
	return diff === 0
}

/**
 * Returns null when the request is authorized, otherwise the 401 response to
 * send. Never says WHY it failed.
 */
function requireAdminKey(c: Context<App>): Response | null {
	const configured = c.env.ADMIN_API_KEY
	const presented = c.req.header('X-Admin-Key') ?? ''
	if (!configured || !timingSafeEqual(presented, configured)) {
		return c.json({ success: false, error: 'unauthorized' }, 401)
	}
	return null
}

// ---------------------------------------------------------------------------
// Shared shapes
// ---------------------------------------------------------------------------

const UsernameBody = z.object({
	username: z.string().describe('Player username (case-insensitive); must already have a Flux Rec account'),
})

const AdminError = z.object({
	success: z.literal(false),
	error: z.string(),
})

const RankName = z.enum(['community_mod', 'developer', 'none']).describe(
	"The rank to set: 'community_mod' (moderator tools), 'developer' (dev tools), or 'none' (remove all ranks)"
)

/** Far-future instant used for "never expires" grants. */
const NEVER_EXPIRES_ISO = '9999-12-31T23:59:59.000Z'

/** The system owner account (@FluxRec) — recorded as the issuer of admin bans. */
const OPERATOR_ACCOUNT_ID = 1

/** Upper bound on a single token grant, against fat-finger accidents. */
const MAX_TOKEN_GRANT = 1_000_000

/**
 * The roomInstance a presence row carries is the match worker's full instance
 * object (with `roomId`) or, from other writers, just the numeric id. Either
 * way this resolves the room id, or null when the player is in no room (lobby).
 */
function presenceRoomId(roomInstance: unknown): number | null {
	if (typeof roomInstance === 'number') return roomInstance > 0 ? roomInstance : null
	if (roomInstance !== null && typeof roomInstance === 'object') {
		const id = (roomInstance as { roomId?: unknown }).roomId
		return typeof id === 'number' && id > 0 ? id : null
	}
	return null
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

export const adminRoutes = new Hono<App>({ strict: false })
	// ---- Ranks -----------------------------------------------------------
	// community_mod -> isModerator (backs GET /role/moderator/:id and the token's
	// `moderator` role claim); developer -> isDeveloper (backs /role/developer/:id
	// and the `developer` role claim). The auth worker stamps both claims at
	// sign-in, so the tools unlock on the player's NEXT login, not instantly.
	.post(
		'/api/admin/v1/ranks/set',
		describeRoute({
			tags: ['Admin'],
			summary: 'Set a player’s staff rank',
			description: [
				'Grant or remove a Flux Rec staff rank. `community_mod` sets the moderator flag,',
				'`developer` sets the developer flag, `none` clears both. The rank is stored on the',
				'account — the same flags the native `/role/*` lookups and the token `role` claim',
				'read — so the in-game moderator/developer tools unlock for real, not as a label.',
				'Ranks take effect on the player’s next login (roles are stamped into the token at',
				'sign-in). Admin-key only.',
			].join(' '),
			requestBody: jsonBody(
				UsernameBody.extend({ rank: RankName }),
				'Who gets the rank and which rank'
			),
			responses: {
				200: json(
					z.object({
						success: z.literal(true),
						username: z.string(),
						accountId: z.number(),
						isModerator: z.boolean(),
						isDeveloper: z.boolean(),
						note: z.string(),
					}),
					'The rank that is now in force'
				),
				400: json(AdminError, 'Bad username or rank'),
				401: json(AdminError, 'Missing or wrong admin key'),
				404: json(AdminError, 'No account with that username'),
			},
		}),
		async (c) => {
			const denied = requireAdminKey(c)
			if (denied) return denied
			const body = (await c.req.json().catch(() => ({}))) as Record<string, unknown>
			const username = typeof body.username === 'string' ? body.username.trim() : ''
			const rank = body.rank
			if (username === '') {
				return c.json({ success: false as const, error: 'username is required' }, 400)
			}
			if (rank !== 'community_mod' && rank !== 'developer' && rank !== 'none') {
				return c.json(
					{
						success: false as const,
						error: "rank must be 'community_mod', 'developer', or 'none'",
					},
					400
				)
			}
			const account = await getAccountByUsername(c.env.DB, username)
			if (!account) {
				return c.json({ success: false as const, error: 'no such player' }, 404)
			}
			const updated = await updateAccount(c.env.DB, account.accountId, {
				isModerator: rank === 'community_mod',
				isDeveloper: rank === 'developer',
			})
			logger.info('admin rank set', {
				targetAccountId: account.accountId,
				rank,
			})
			return c.json({
				success: true as const,
				username: updated.username,
				accountId: updated.accountId,
				isModerator: updated.isModerator === true,
				isDeveloper: updated.isDeveloper === true,
				note: 'Takes effect on the player’s next login (roles are stamped into the token at sign-in).',
			})
		}
	)

	// ---- Flux Rec+ membership ---------------------------------------------
	// hasPlus is what auth stamps into the token's `rn.plus` claim, and econ's
	// UpdateAndGetSubscription reads that claim plus plusUntil for expiry — so the
	// full native membership unlocks (CampusCard, subscriber discount, Plus
	// storefronts). Takes effect on the player's NEXT login, like every claim.
	.post(
		'/api/admin/v1/membership/set',
		describeRoute({
			tags: ['Admin'],
			summary: 'Grant or remove Flux Rec+ membership',
			description: [
				'Give a player Flux Rec+ for `duration_months` months (30 days each), `0` for a',
				'never-expiring membership, or `-1` to remove it. Sets the account’s `hasPlus` flag',
				'and the `plusSince`/`plusUntil` subscription timestamps — the same fields the native',
				'token purchase writes — so the complete native membership unlocks in-game. Takes',
				'effect on the player’s next login. Admin-key only.',
			].join(' '),
			requestBody: jsonBody(
				UsernameBody.extend({
					duration_months: z
						.number()
						.int()
						.describe('Months of membership (1, 2, 12, …), 0 = never expires, -1 = remove'),
				}),
				'Who gets Flux Rec+ and for how long'
			),
			responses: {
				200: json(
					z.object({
						success: z.literal(true),
						username: z.string(),
						accountId: z.number(),
						hasPlus: z.boolean(),
						plusUntil: z.string().nullable(),
						note: z.string(),
					}),
					'The membership that is now in force'
				),
				400: json(AdminError, 'Bad username or duration'),
				401: json(AdminError, 'Missing or wrong admin key'),
				404: json(AdminError, 'No account with that username'),
			},
		}),
		async (c) => {
			const denied = requireAdminKey(c)
			if (denied) return denied
			const body = (await c.req.json().catch(() => ({}))) as Record<string, unknown>
			const username = typeof body.username === 'string' ? body.username.trim() : ''
			const durationMonths = body.duration_months
			if (username === '') {
				return c.json({ success: false as const, error: 'username is required' }, 400)
			}
			if (
				typeof durationMonths !== 'number' ||
				!Number.isInteger(durationMonths) ||
				(durationMonths < 0 && durationMonths !== -1)
			) {
				return c.json(
					{
						success: false as const,
						error: 'duration_months must be a non-negative integer, or -1 to remove',
					},
					400
				)
			}
			const account = await getAccountByUsername(c.env.DB, username)
			if (!account) {
				return c.json({ success: false as const, error: 'no such player' }, 404)
			}
			let plusUntil: string | null = null
			if (durationMonths === -1) {
				await c.env.DB.prepare(
					`UPDATE account SET data = json_set(json_remove(data, '$.plusSince', '$.plusUntil'), '$.hasPlus', json('false')) WHERE account_id = ?1`
				)
					.bind(account.accountId)
					.run()
			} else {
				const now = new Date()
				const until =
					durationMonths === 0
						? new Date(NEVER_EXPIRES_ISO)
						: new Date(now.getTime() + durationMonths * 30 * 24 * 60 * 60 * 1000)
				plusUntil = until.toISOString()
				await c.env.DB.prepare(
					`UPDATE account SET data = json_set(data, '$.hasPlus', json('true'), '$.plusSince', ?2, '$.plusUntil', ?3) WHERE account_id = ?1`
				)
					.bind(account.accountId, now.toISOString(), plusUntil)
					.run()
			}
			const fresh = await getAccountByUsername(c.env.DB, username)
			logger.info('admin membership set', {
				targetAccountId: account.accountId,
				durationMonths,
			})
			return c.json({
				success: true as const,
				username: fresh?.username ?? account.username,
				accountId: account.accountId,
				hasPlus: fresh?.hasPlus === true,
				plusUntil: fresh?.plusUntil ?? null,
				note: 'Takes effect on the player’s next login (Plus rides the token’s rn.plus claim).',
			})
		}
	)

	// ---- Bans ---------------------------------------------------------------
	// An account-wide ban, enforced by `match` (every matchmake is refused except the
	// dorm) and described to the player by `moderationBlockDetails`. The row is a
	// `report` with `banned` set — the ban carries its evidence — filed by the system
	// owner account with the operator's reason, which the block screen shows via
	// TopMessageOverride (see reports-db). Timed bans lift themselves when
	// `ban_expires` passes; permanent bans carry no expiry.
	.post(
		'/api/admin/v1/bans/create',
		describeRoute({
			tags: ['Admin'],
			summary: 'Ban a player (optionally also voice-ban)',
			description: [
				'Hand down an account-wide ban with a reason the player sees in-game on the ban',
				'screen. `duration_minutes` 0 is permanent; a positive number is a timed ban /',
				'timeout that lifts itself on expiry. `voice_ban: true` additionally mutes the',
				'player’s voice for the same span (permanent when the ban is) — a voice-banned',
				'player gets no voice server from connection-info, so they can play but not speak.',
				'Enforced immediately by matchmaking. Admin-key only.',
			].join(' '),
			requestBody: jsonBody(
				UsernameBody.extend({
					reason: z.string().describe('Why they are banned — shown to the player in-game'),
					duration_minutes: z
						.number()
						.int()
						.min(0)
						.describe('0 = permanent, otherwise the ban lifts after this many minutes'),
					voice_ban: z
						.boolean()
						.optional()
						.describe('Also voice-ban for the same duration (default false)'),
				}),
				'Who is banned, why, and for how long'
			),
			responses: {
				200: json(
					z.object({
						success: z.literal(true),
						username: z.string(),
						accountId: z.number(),
						permanent: z.boolean(),
						banExpires: z.string().nullable(),
						voiceBanned: z.boolean(),
						voiceBanUntil: z.string().nullable(),
					}),
					'The ban that is now in force'
				),
				400: json(AdminError, 'Bad username, reason, or duration'),
				401: json(AdminError, 'Missing or wrong admin key'),
				404: json(AdminError, 'No account with that username'),
			},
		}),
		async (c) => {
			const denied = requireAdminKey(c)
			if (denied) return denied
			const body = (await c.req.json().catch(() => ({}))) as Record<string, unknown>
			const username = typeof body.username === 'string' ? body.username.trim() : ''
			const reason = typeof body.reason === 'string' ? body.reason.trim() : ''
			const durationMinutes = body.duration_minutes
			const voiceBan = body.voice_ban === true
			if (username === '') {
				return c.json({ success: false as const, error: 'username is required' }, 400)
			}
			if (reason === '') {
				return c.json({ success: false as const, error: 'reason is required' }, 400)
			}
			if (reason.length > 500) {
				return c.json(
					{ success: false as const, error: 'reason must be 500 characters or fewer' },
					400
				)
			}
			if (
				typeof durationMinutes !== 'number' ||
				!Number.isInteger(durationMinutes) ||
				durationMinutes < 0
			) {
				return c.json(
					{ success: false as const, error: 'duration_minutes must be 0 or a positive integer' },
					400
				)
			}
			const account = await getAccountByUsername(c.env.DB, username)
			if (!account) {
				return c.json({ success: false as const, error: 'no such player' }, 404)
			}
			if (account.accountId === OPERATOR_ACCOUNT_ID) {
				return c.json(
					{ success: false as const, error: 'the owner account cannot be banned' },
					400
				)
			}
			// The ban lives on a report row filed by the system owner: the reporter IS the
			// operator here, so the reason is safe to show the player (see banBlockDetails).
			const report = await createReport(c.env.DB, {
				reporterPlayerId: OPERATOR_ACCOUNT_ID,
				reportedPlayerId: account.accountId,
				reportCategory: 6,
				details: `${ADMIN_BAN_DETAILS_PREFIX}${reason}`,
			})
			const banExpires =
				durationMinutes === 0
					? null
					: new Date(Date.now() + durationMinutes * 60_000).toISOString()
			await banFromReport(c.env.DB, report.id, {
				banned: true,
				banExpires,
				bannedBy: OPERATOR_ACCOUNT_ID,
			})
			let voiceBanUntil: string | null = null
			if (voiceBan) {
				voiceBanUntil =
					durationMinutes === 0 ? NEVER_EXPIRES_ISO : (banExpires as string)
				await updateAccount(c.env.DB, account.accountId, { voiceBanUntil })
			}
			logger.info('admin ban created', {
				targetAccountId: account.accountId,
				reportId: report.id,
				permanent: durationMinutes === 0,
				voiceBan,
			})
			return c.json({
				success: true as const,
				username: account.username,
				accountId: account.accountId,
				permanent: durationMinutes === 0,
				banExpires,
				voiceBanned: voiceBan,
				voiceBanUntil,
			})
		}
	)

	// ---- Ban lift -------------------------------------------------------------
	.post(
		'/api/admin/v1/bans/lift',
		describeRoute({
			tags: ['Admin'],
			summary: 'Lift a player’s active ban (and voice ban)',
			description: [
				'Lifts the account-wide ban currently in force for the player, and clears any',
				'voice ban with it. Answers success with lifted:false when there is no active ban.',
				'Admin-key only.',
			].join(' '),
			requestBody: jsonBody(UsernameBody, 'Whose ban to lift'),
			responses: {
				200: json(
					z.object({
						success: z.literal(true),
						username: z.string(),
						accountId: z.number(),
						lifted: z.boolean(),
					}),
					'Whether a ban was lifted'
				),
				400: json(AdminError, 'Bad username'),
				401: json(AdminError, 'Missing or wrong admin key'),
				404: json(AdminError, 'No account with that username'),
			},
		}),
		async (c) => {
			const denied = requireAdminKey(c)
			if (denied) return denied
			const body = (await c.req.json().catch(() => ({}))) as Record<string, unknown>
			const username = typeof body.username === 'string' ? body.username.trim() : ''
			if (username === '') {
				return c.json({ success: false as const, error: 'username is required' }, 400)
			}
			const account = await getAccountByUsername(c.env.DB, username)
			if (!account) {
				return c.json({ success: false as const, error: 'no such player' }, 404)
			}
			const ban = await getActiveBan(c.env.DB, account.accountId)
			let lifted = false
			if (ban) {
				await banFromReport(c.env.DB, ban.id, { banned: false })
				lifted = true
			}
			await c.env.DB.prepare(
				`UPDATE account SET data = json_remove(data, '$.voiceBanUntil') WHERE account_id = ?1`
			)
				.bind(account.accountId)
				.run()
			logger.info('admin ban lifted', { targetAccountId: account.accountId, lifted })
			return c.json({
				success: true as const,
				username: account.username,
				accountId: account.accountId,
				lifted,
			})
		}
	)

	// ---- Tokens ---------------------------------------------------------------
	// Credits RecCenterTokens (currency 2 — the general-purpose token balance the
	// client shows) via econ's balance table. Single-player grants go through
	// creditCurrency (which also applies the signup grant first); the everyone-grant
	// does the same two steps as one batched statement pair so nobody loses their
	// signup tokens to the upsert.
	.post(
		'/api/admin/v1/tokens/grant',
		describeRoute({
			tags: ['Admin'],
			summary: 'Grant tokens to a player, or to everyone',
			description: [
				'Credit Flux Rec tokens (RecCenterTokens) to one player (`username`) or to every',
				'account (`grant_to: "everyone"`). The grant lands in the same balance the storefront',
				'and the client balance read use, so it reflects in-game immediately. Admin-key only.',
			].join(' '),
			requestBody: jsonBody(
				z
					.object({
						username: z.string().optional().describe('Player username (case-insensitive)'),
						grant_to: z
							.enum(['everyone'])
							.optional()
							.describe('Set to "everyone" to grant to all accounts'),
						amount: z
							.number()
							.int()
							.positive()
							.max(MAX_TOKEN_GRANT)
							.describe('Tokens to grant (positive integer)'),
					})
					.describe('Either username or grant_to:"everyone", plus the amount'),
				'Who gets tokens and how many'
			),
			responses: {
				200: json(
					z.object({
						success: z.literal(true),
						grantedTo: z.string(),
						accounts: z.number(),
						amount: z.number(),
						newBalance: z.number().nullable(),
					}),
					'The grant that was applied'
				),
				400: json(AdminError, 'Bad target or amount'),
				401: json(AdminError, 'Missing or wrong admin key'),
				404: json(AdminError, 'No account with that username'),
			},
		}),
		async (c) => {
			const denied = requireAdminKey(c)
			if (denied) return denied
			const body = (await c.req.json().catch(() => ({}))) as Record<string, unknown>
			const amount = body.amount
			const grantTo = body.grant_to
			const username = typeof body.username === 'string' ? body.username.trim() : ''
			if (typeof amount !== 'number' || !Number.isInteger(amount) || amount <= 0) {
				return c.json(
					{ success: false as const, error: 'amount must be a positive integer' },
					400
				)
			}
			if (amount > MAX_TOKEN_GRANT) {
				return c.json(
					{
						success: false as const,
						error: `amount must not exceed ${MAX_TOKEN_GRANT}`,
					},
					400
				)
			}
			const startingTokens = intVar(c.env.STARTING_TOKENS, DEFAULT_STARTING_TOKENS)
			if (grantTo === 'everyone') {
				// Two steps: first the signup grant for accounts that never had a balance
				// row (INSERT OR IGNORE), then a plain UPDATE crediting every row. The order
				// matters — without the first step, a fresh account's row would be created
				// holding only the grant, swallowing its signup tokens. Plain statements
				// rather than an upsert, so the logic stays obvious.
				await c.env.DB.prepare(
					'INSERT OR IGNORE INTO balance (account_id, currency_type, amount) SELECT account_id, ?1, ?2 FROM account'
				)
					.bind(CurrencyType.RecCenterTokens, startingTokens)
					.run()
				await c.env.DB.prepare(
					'UPDATE balance SET amount = amount + ?1 WHERE currency_type = ?2'
				)
					.bind(amount, CurrencyType.RecCenterTokens)
					.run()
				const count = await c.env.DB.prepare('SELECT COUNT(*) AS n FROM account').first<{
					n: number
				}>()
				logger.info('admin tokens granted to everyone', {
					amount,
					accounts: count?.n ?? 0,
				})
				return c.json({
					success: true as const,
					grantedTo: 'everyone',
					accounts: count?.n ?? 0,
					amount,
					newBalance: null,
				})
			}
			if (username === '') {
				return c.json(
					{ success: false as const, error: 'username is required (or grant_to: "everyone")' },
					400
				)
			}
			const account = await getAccountByUsername(c.env.DB, username)
			if (!account) {
				return c.json({ success: false as const, error: 'no such player' }, 404)
			}
			const newBalance = await creditCurrency(
				c.env.DB,
				account.accountId,
				CurrencyType.RecCenterTokens,
				amount,
				startingTokens
			)
			logger.info('admin tokens granted', {
				targetAccountId: account.accountId,
				amount,
				newBalance,
			})
			return c.json({
				success: true as const,
				grantedTo: account.username,
				accounts: 1,
				amount,
				newBalance,
			})
		}
	)

	// ---- Online players ---------------------------------------------------------
	// Live presence: one row per account, expiring on a TTL. Lobby (null-instance)
	// presence counts — those players are signed in and playing, just not in a room.
	.get(
		'/api/admin/v1/players/online',
		describeRoute({
			tags: ['Admin'],
			summary: 'Who is online right now',
			description: [
				'Live player count plus the online players and the room each is in (null when in',
				'no room). Feeds the Discord “Flux Rec Status” channels. Admin-key only.',
			].join(' '),
			responses: {
				200: json(
					z.object({
						success: z.literal(true),
						count: z.number(),
						players: z.array(
							z.object({
								username: z.string(),
								accountId: z.number(),
								room: z.string().nullable(),
								roomId: z.number().nullable(),
							})
						),
					}),
					'The online player count and list'
				),
				401: json(AdminError, 'Missing or wrong admin key'),
			},
		}),
		async (c) => {
			const denied = requireAdminKey(c)
			if (denied) return denied
			const now = Math.floor(Date.now() / 1000)
			const { results } = await c.env.DB.prepare(
				'SELECT data FROM presence WHERE expires_at > ?1'
			)
				.bind(now)
				.all<{ data: string }>()
			const presences = results.map((r) => {
				try {
					return JSON.parse(r.data) as { accountId: number; roomInstance: unknown }
				} catch {
					return null
				}
			})
			const live = presences.filter(
				(p): p is { accountId: number; roomInstance: unknown } =>
					p !== null && typeof p.accountId === 'number'
			)
			const accountIds = [...new Set(live.map((p) => p.accountId))]
			const accounts = await getAccountsByIds(c.env.DB, accountIds)
			const byId = new Map(accounts.map((a) => [a.accountId, a]))
			const roomIds = [
				...new Set(live.map((p) => presenceRoomId(p.roomInstance)).filter((id): id is number => id !== null)),
			]
			const rooms = await Promise.all(roomIds.map((id) => getRoomById(c.env.DB, id)))
			const roomNames = new Map(
				rooms.filter((r) => r !== null).map((r) => [r!.RoomId, r!.Name])
			)
			const players = live.map((p) => {
				const account = byId.get(p.accountId)
				const roomId = presenceRoomId(p.roomInstance)
				return {
					username: account?.username ?? `player:${p.accountId}`,
					accountId: p.accountId,
					room: roomId !== null ? (roomNames.get(roomId) ?? `room:${roomId}`) : null,
					roomId,
				}
			})
			return c.json({ success: true as const, count: players.length, players })
		}
	)

	// ---- Ban list ---------------------------------------------------------------
	// Every account-wide ban currently in force, newest first. Feeds the Discord
	// `/fluxbans` command and the NL "list bans". Timed bans that already expired
	// are excluded (getBansInForce filters them); lifted bans have `banned` cleared.
	.get(
		'/api/admin/v1/bans/list',
		describeRoute({
			tags: ['Admin'],
			summary: 'List active bans',
			description:
				'Every account-wide ban currently in force, newest first, with the reason, ' +
				'expiry, and voice-ban state. Admin-key only.',
			responses: {
				200: json(
					z.object({
						success: z.literal(true),
						bans: z.array(
							z.object({
								username: z.string(),
								accountId: z.number(),
								reason: z.string().nullable(),
								permanent: z.boolean(),
								banExpires: z.string().nullable(),
								voiceBanned: z.boolean(),
								voiceBanUntil: z.string().nullable(),
								bannedAt: z.string().nullable(),
							})
						),
					}),
					'The bans currently in force'
				),
				401: json(AdminError, 'Missing or wrong admin key'),
			},
		}),
		async (c) => {
			const denied = requireAdminKey(c)
			if (denied) return denied
			const bans = await getBansInForce(c.env.DB)
			const accountIds = [...new Set(bans.map((b) => b.reported_player_id))]
			const accounts = await getAccountsByIds(c.env.DB, accountIds)
			const byId = new Map(accounts.map((a) => [a.accountId, a]))
			const now = new Date().toISOString()
			const list = bans.map((ban) => {
				const account = byId.get(ban.reported_player_id)
				const voiceBanUntil = account?.voiceBanUntil ?? null
				return {
					username: account?.username ?? `player:${ban.reported_player_id}`,
					accountId: ban.reported_player_id,
					reason: adminBanReason(ban),
					permanent: ban.ban_expires === null,
					banExpires: ban.ban_expires,
					voiceBanned: voiceBanUntil !== null && voiceBanUntil > now,
					voiceBanUntil,
					bannedAt: ban.banned_at ?? ban.created_at ?? null,
				}
			})
			return c.json({ success: true as const, bans: list })
		}
	)

	// ---- Gift grant ---------------------------------------------------------------
	// Mint a gift box directly into a player's inbox. With no `gift_id` the box is
	// the operator-configured default gift (see OPERATOR_DEFAULT_GIFT_* vars —
	// there is no authentic gift-box catalog, so its contents are the operator's
	// explicit choice, not Rec Room data); with `gift_id` the content of that
	// existing box is copied to the player. The box appears in
	// `/api/avatar/v2/gifts` and opens through the normal consume flow. Admin-key only.
	.post(
		'/api/admin/v1/gifts/grant',
		describeRoute({
			tags: ['Admin'],
			summary: 'Grant a gift box to a player',
			description:
				'Mint a gift box into the player’s inbox. Omit `gift_id` for the ' +
				'operator-configured default gift (OPERATOR_DEFAULT_GIFT_* vars; no ' +
				'authentic gift-box catalog exists, so its contents are the operator’s ' +
				'choice, not Rec Room data), or pass an existing box id to copy its ' +
				'contents. Admin-key only.',
			requestBody: jsonBody(
				UsernameBody.extend({
					gift_id: z
						.number()
						.int()
						.positive()
						.optional()
						.describe('Copy the contents of this existing gift box; omit for the operator-configured default gift'),
				}),
				'Who gets the gift and optionally which box to copy'
			),
			responses: {
				200: json(
					z.object({
						success: z.literal(true),
						username: z.string(),
						accountId: z.number(),
						giftId: z.number(),
					}),
					'The gift box that was granted'
				),
				400: json(AdminError, 'Bad username'),
				401: json(AdminError, 'Missing or wrong admin key'),
				404: json(AdminError, 'No account with that username, or no such gift box'),
			},
		}),
		async (c) => {
			const denied = requireAdminKey(c)
			if (denied) return denied
			const body = (await c.req.json().catch(() => ({}))) as Record<string, unknown>
			const username = typeof body.username === 'string' ? body.username.trim() : ''
			const giftId = body.gift_id
			if (username === '') {
				return c.json({ success: false as const, error: 'username is required' }, 400)
			}
			if (giftId !== undefined && (typeof giftId !== 'number' || !Number.isInteger(giftId) || giftId <= 0)) {
				return c.json({ success: false as const, error: 'gift_id must be a positive integer' }, 400)
			}
			const account = await getAccountByUsername(c.env.DB, username)
			if (!account) {
				return c.json({ success: false as const, error: 'no such player' }, 404)
			}
			let content
			if (typeof giftId === 'number') {
				const source = await getGift(c.env.DB, giftId)
				if (!source) {
					return c.json({ success: false as const, error: 'no such gift box' }, 404)
				}
				// Copy the box's content; identity fields are regenerated by createGift.
				const { Id: _id, CreatedAt: _created, ...box } = source.gift
				content = box
			} else {
				// Operator-configured default gift: no authentic gift-box catalog
				// exists, so the contents are the operator's explicit choice via
				// OPERATOR_DEFAULT_GIFT_* vars — never presented as Rec Room data.
				// Defaults grant nothing.
				content = {
					FromPlayerId: OPERATOR_ACCOUNT_ID,
					ConsumableItemDesc: '',
					ConsumableCount: 0,
					AvatarItemDesc: '',
					AvatarItemType: 0,
					EquipmentPrefabName: '',
					EquipmentModificationGuid: '',
					CurrencyType: 2,
					Currency: intVar(c.env.OPERATOR_DEFAULT_GIFT_TOKENS, 0),
					Xp: intVar(c.env.OPERATOR_DEFAULT_GIFT_XP, 0),
					PackageType: 0,
					Message: typeof c.env.OPERATOR_DEFAULT_GIFT_MESSAGE === 'string'
						? c.env.OPERATOR_DEFAULT_GIFT_MESSAGE
						: '',
					GiftRarity: intVar(c.env.OPERATOR_DEFAULT_GIFT_RARITY, 0),
					Platform: -1,
					PlatformsToSpawnOn: -1,
					BalanceType: 0,
					GiftContext: 0,
				}
			}
			const { id } = await createGift(c.env.DB, account.accountId, content)
			logger.info('admin gift granted', {
				targetAccountId: account.accountId,
				giftId: id,
				copiedFrom: typeof giftId === 'number' ? giftId : null,
			})
			return c.json({
				success: true as const,
				username: account.username,
				accountId: account.accountId,
				giftId: id,
			})
		}
	)
