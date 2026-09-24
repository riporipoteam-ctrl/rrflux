import { Hono } from 'hono'
import { describeRoute, openAPIRouteHandler, validator } from 'hono-openapi'
import { useWorkersLogger } from 'workers-tagged-logger'

import {
	createAccount,
	defaultAccount,
	getAccount,
	getAccountByUsername,
	getAccountsByIds,
	searchAccounts,
	updateAccount,
} from '@repo/domain'
import {
	logger,
	withCleanSpec,
	withDefaultCors,
	withNotFound,
	withOnError,
} from '@repo/hono-helpers'
import { validateAndGetAccountId } from '@repo/jwt'

// The notification-type ids the hub carries (owned by the `notify` worker). Imported as a
// value — the enum has no runtime dependencies.
import { NotificationType } from '../../notify/src/notification-types'
import {
	AccountDto,
	BannerImageRequest,
	BioRequest,
	BioResponse,
	BulkIdsRequest,
	CreateAccountRequest,
	CreateAccountResult,
	DisplayNameRequest,
	EmailRequest,
	EmojiRequest,
	FluxSocialExchangeRequest,
	FluxSocialExchangeResponse,
	FluxSocialLinkCodeResponse,
	FluxSocialMeResponse,
	FluxSocialPrivacySettings,
	FluxSocialStatusResponse,
	form,
	HealthResponse,
	IdentityFlagsRequest,
	json,
	jsonBody,
	ParentalControl,
	PhoneRequest,
	PrivacySettings,
	ProfileImageRequest,
	PronounsRequest,
	SelfAccountDto,
	SuccessResponse,
	UsernameRequest,
	UsernameResult,
	WhitelistedEmojis,
} from './openapi'
import {
	checkExchangeThrottle,
	clearExchangeThrottle,
	getAccountIdBySession,
	getPrivacy,
	hasWebsiteSession,
	issueLinkCode,
	issueWebsiteSession,
	LINK_CODE_TTL_SECONDS,
	redeemLinkCode,
	revokeWebsiteSession,
	setPrivacy,
} from './fluxsocial-db'
import { resolveWhitelistedEmoji, WHITELISTED_EMOJIS } from './whitelisted-emojis'

import type { Context } from 'hono'
import type { DescribeRouteOptions } from 'hono-openapi'
import type { Account } from '@repo/domain'
import type { FluxSocialPrivacy } from './fluxsocial-db'
import type { App } from './context'

/**
 * Account reads/writes are backed by the shared `accounts` table in D1 (schema
 * owned by the `auth` worker). Accounts not in the table fall back to a
 * synthesized default (every column has a fallback anyway). Profile mutations
 * persist to the account row and push an AccountUpdate through the notifications
 * hub (see `pushAccountUpdate`).
 *
 * Auth-gated routes validate the Bearer JWT issued by the `auth` worker.
 */

/**
 * Resolve the account id from a Bearer token, mirroring the repeated
 * auth-header check. Returns `null` when the header is missing,
 * the token is invalid, or the `sub` claim isn't an integer.
 */
async function authedId(c: Context<App>): Promise<number | null> {
	return validateAndGetAccountId(c.req.raw, await c.env.JWT_SECRET.get())
}

/** Results.Unauthorized() equivalent — 401 with empty body. */
function unauthorized(c: Context<App>) {
	return c.body(null, 401)
}

/**
 * Resolve the account id from the Flux Social website's opaque session token
 * (minted by `POST /fluxsocial/exchange`), or `null` when the header is missing
 * or the token is unknown/revoked/expired. Unlike `authedId` this is NOT a JWT —
 * it is resolved by SHA-256 hash against `flux_social_links`.
 */
async function websiteSessionId(c: Context<App>): Promise<number | null> {
	const authHeader = c.req.header('Authorization')
	if (!authHeader || !authHeader.toLowerCase().startsWith('bearer ')) return null
	const token = authHeader.slice('bearer '.length).trim()
	if (!token) return null
	return getAccountIdBySession(c.env.DB, token)
}

/**
 * Resolve the caller for Flux Social routes that accept EITHER credential: the
 * game client's Bearer JWT or the website's opaque session token. The token
 * spaces don't overlap (a JWT vs 64 hex chars), so the order is irrelevant.
 */
async function fluxAccountId(c: Context<App>): Promise<number | null> {
	return (await authedId(c)) ?? (await websiteSessionId(c))
}

/**
 * Parse a JSON object body, or `null` when the body isn't one. The request is
 * cloned for the attempt so a non-JSON body stays readable as a form.
 */
async function requestJson(c: Context<App>): Promise<Record<string, unknown> | null> {
	try {
		const parsed: unknown = await c.req.raw.clone().json()
		if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
			return parsed as Record<string, unknown>
		}
	} catch {
		// Not JSON — the caller falls back to form fields.
	}
	return null
}

/**
 * Read one string field from a JSON body (what website `fetch` sends) or a
 * form-urlencoded/multipart body (what the game client sends).
 */
async function requestField(c: Context<App>, name: string): Promise<string> {
	const fromJson = (await requestJson(c))?.[name]
	if (typeof fromJson === 'string') return fromJson
	return formField(c, name)
}

/** Best-effort client IP for the pairing-code exchange throttle. */
function clientIp(c: Context<App>): string {
	return (
		c.req.header('CF-Connecting-IP') ?? c.req.header('x-forwarded-for')?.split(',')[0]?.trim() ?? 'unknown'
	)
}

/** Username changes a fresh account starts with (until one has been consumed). */
const DEFAULT_USERNAME_CHANGES = 3

/**
 * Username-change result envelope: `{ success, error, value }`, always HTTP 200.
 * On success `value` is the updated account; on error `error` carries the message
 * and `value` is an empty string.
 *
 * The envelope-at-200 is the reference's (`RecNet`) convention — a refusal is a
 * successful call that answers "no", and the player-facing sentence rides in `error`.
 * `POST /account/create` does the same. This was briefly a 400 so a caller could branch
 * on the status; it isn't, because that's not what the real service does.
 */
function usernameResult(c: Context<App>, error = '', value: unknown = '') {
	return c.json({ success: error === '', error, value })
}

/** Read a single string field from a form-urlencoded / multipart body. */
async function formField(c: Context<App>, name: string): Promise<string> {
	const body = await c.req.parseBody().catch(() => ({}) as Record<string, unknown>)
	const value = body[name]
	return typeof value === 'string' ? value : ''
}

/**
 * Read a REPEATED string field from a form-urlencoded / multipart body (`id=1&id=2`).
 * `parseBody` keeps only the last value for a duplicated key unless it's told to collect
 * them all, which is the whole point here.
 */
async function formFields(c: Context<App>, name: string): Promise<string[]> {
	const body = await c.req.parseBody({ all: true }).catch(() => ({}) as Record<string, unknown>)
	const value = body[name]
	const values = Array.isArray(value) ? value : [value]
	return values.filter((v): v is string => typeof v === 'string')
}

/**
 * OpenAPI spec for the two `/account/bulk` registrations, which differ only in where the
 * ids ride.
 */
function bulkRouteSpec(source: 'query' | 'body'): DescribeRouteOptions {
	return {
		tags: ['Lookup'],
		summary: 'Look up many accounts by id',
		description: [
			'Accepts repeated `id` values (query params and/or a form-urlencoded body) and',
			'comma-separated lists. Every requested id appears in the response — ids with no',
			'stored row get a synthesized default.',
		].join(' '),
		...(source === 'query'
			? {
					parameters: [
						{
							name: 'id',
							in: 'query',
							required: false,
							description: 'Repeatable; each value may be a comma-separated list of ids',
							schema: { type: 'array', items: { type: 'string' } },
						},
					],
				}
			: { requestBody: form(BulkIdsRequest, 'Repeated `id` fields') }),
		responses: { 200: json(AccountDto.array(), 'One public account per requested id') },
	}
}

/**
 * Look up many accounts at once. Ids come from repeated `id` query params AND, on the
 * POST form, repeated `id` body fields; either may carry a comma-separated list. Deduped
 * (first occurrence wins) so a repeated id can't double a row or waste a bound parameter.
 */
async function bulkAccounts(c: Context<App>) {
	const raw = [...(c.req.queries('id') ?? []), ...(await formFields(c, 'id'))]
	const ids = [
		...new Set(
			raw
				.flatMap((v) => v.split(','))
				.map((v) => Number.parseInt(v.trim(), 10))
				.filter((n) => !Number.isNaN(n))
		),
	]
	// Resolve stored accounts, synthesizing a default for any id not in the DB
	// so every requested id is present in the response.
	const stored = new Map((await getAccountsByIds(c.env.DB, ids)).map((a) => [a.accountId, a]))
	return c.json(ids.map((id) => toAccountDto(stored.get(id) ?? defaultAccount(id))))
}

/**
 * Project a stored account into the public account DTO — the client's camelCase
 * shape, excluding private fields like `email` (surfaced only by /account/me).
 */
function toAccountDto(account: Account) {
	return {
		accountId: account.accountId,
		username: account.username,
		displayName: account.displayName,
		profileImage: account.profileImage,
		// Rows stored before these fields existed have neither key — always emit them as
		// "" rather than letting them go missing.
		bannerImage: account.bannerImage ?? '',
		displayEmoji: account.displayEmoji ?? '',
		isJunior: account.isJunior,
		// Admin flags — the client gates the Settings admin page and dev-only UI on these.
		// Always emit as booleans (never absent/undefined) so the client's parser sees them.
		isDeveloper: account.isDeveloper === true,
		isModerator: account.isModerator === true,
		platforms: account.platforms,
		personalPronouns: account.personalPronouns,
		identityFlags: account.identityFlags,
		createdAt: account.createdAt,
	}
}

/**
 * Project a stored account into the private self DTO (the /account/me shape) —
 * the public DTO plus owner-only fields. `juniorState`/`parentAccountId` are
 * OMITTED when null (emitting `null` makes the client's enum parser throw).
 *
 * An unset `email` is `""`, never null — same as `bio`. Two reasons: the client reads
 * it as a string, and this DTO also rides the `SelfAccountUpdate` hub frame, where the
 * hub DROPS null values from `Msg` — so a null email doesn't arrive as null, it
 * vanishes from the frame entirely.
 */
function toSelfAccountDto(account: Account) {
	return {
		...toAccountDto(account),
		email: account.email ?? '',
		// @todo he game client needs this to be set. I forget how birthdays were set, so for now
		// everyone can be old.
		birthday: '1904-01-01T00:00:00.000Z',
		availableUsernameChanges: account.availableUsernameChanges ?? DEFAULT_USERNAME_CHANGES,
	}
}

/** The notifications hub is a single global DO instance (see the `notify` worker). */
const HUB_INSTANCE = 'global'

/**
 * Push the notifications that follow an account mutation, mirroring the reference
 * hub behavior: the owner receives `SelfAccountUpdate` and `AccountUpdate`, and
 * every connected client receives an `AccountUpdate` broadcast. Hub failures are
 * logged and swallowed — the account write has already committed, so a hub
 * hiccup must not fail the request.
 */
async function pushAccountUpdate(c: Context<App>, account: Account): Promise<void> {
	try {
		const hub = c.env.RECFLARE_NOTIFICATIONS_HUB.getByName(HUB_INSTANCE)
		const publicDto = toAccountDto(account)
		await hub.notifyPlayer(
			account.accountId,
			NotificationType.SubscriptionUpdateSelfProfile,
			toSelfAccountDto(account)
		)
		await hub.notifyPlayer(account.accountId, NotificationType.SubscriptionUpdateProfile, publicDto)
		await hub.broadcast(NotificationType.SubscriptionUpdateProfile, publicDto)
	} catch (err) {
		logger.error('failed to push account update notifications', {
			accountId: account.accountId,
			error: err instanceof Error ? err.message : String(err),
		})
	}
}

/** The empty-body 401 every auth-gated route returns; reused across their specs. */
const UNAUTHORIZED_RESPONSE = { description: 'Missing or invalid bearer token (empty body)' }

/** Bearer-JWT security requirement, for the auth-gated routes. */
const AUTHED = [{ bearerAuth: [] }]

/**
 * Bearer-token security requirement for Flux Social website routes. Same
 * `Authorization: Bearer` mechanism as `AUTHED`, but the token is the opaque
 * website session from `POST /fluxsocial/exchange`, not a game JWT — hence the
 * separate scheme in the generated spec.
 */
const WEBSITE_AUTHED = [{ fluxSocialAuth: [] }]

const app = new Hono<App>()
	.use(
		'*',
		// middleware
		(c, next) =>
			useWorkersLogger(c.env.NAME, {
				environment: c.env.ENVIRONMENT,
				release: c.env.SENTRY_RELEASE,
			})(c, next)
	)

	// The website (`www`) is a browser origin calling these endpoints directly, the way
	// rec.net's own site called the game's API — so the responses need CORS headers or
	// the browser discards them. `origin: '*'` is deliberate and safe HERE because these
	// endpoints authenticate with a bearer token in the `Authorization` header, never a
	// cookie: a hostile page can't read another origin's stored token, so there is no
	// ambient credential for `*` to expose. Do not add cookie auth without narrowing it.
	.use('*', withDefaultCors())

	.onError(withOnError())
	.notFound(withNotFound())

	// Root health check.
	.get(
		'/',
		describeRoute({
			tags: ['Meta'],
			summary: 'Health check',
			responses: { 200: json(HealthResponse, 'Service is up') },
		}),
		(c) => c.json({ service: 'accounts', status: 'ok' })
	)

	// ---- Emoji config --------------------------------------------------------
	// The picker the client fills its displayEmoji grid from. A BARE array — no
	// `{ success, error, value }` envelope and no wrapper object; the client parses the
	// response body itself as the list.
	.get(
		'/emojiConfig/whitelistedEmojis',
		describeRoute({
			tags: ['Config'],
			summary: 'Emoji a player may use as their displayEmoji',
			description: [
				'A bare JSON array of emoji, in the order the client draws them. Static — not',
				'auth-gated, and identical for every player.',
			].join(' '),
			responses: { 200: json(WhitelistedEmojis, 'The whitelisted emoji, in picker order') },
		}),
		(c) => c.json(WHITELISTED_EMOJIS)
	)

	// ---- Self account --------------------------------------------------------
	.get(
		'/account/me',
		describeRoute({
			tags: ['Self'],
			summary: 'The caller’s own account',
			description: [
				'The private self DTO, including owner-only fields (email, remaining username',
				'changes). An account with no stored row falls back to a synthesized default.',
			].join(' '),
			security: AUTHED,
			responses: {
				200: json(SelfAccountDto, 'The caller’s account'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			// Load the stored account, falling back to a synthesized default.
			const account = (await getAccount(c.env.DB, id)) ?? defaultAccount(id)
			return c.json(toSelfAccountDto(account))
		}
	)

	// ---- Search --------------------------------------------------------------
	// Prefix-search accounts by username (`?name=`). Returns a bare array of public
	// account DTOs, ordered alphabetically. Registered before `/account/:id` so the
	// static `search` path wins over the param route.
	.get(
		'/account/search',
		describeRoute({
			tags: ['Lookup'],
			summary: 'Prefix-search accounts by username',
			description: 'Case-insensitive prefix match on username, ordered alphabetically.',
			parameters: [
				{
					name: 'name',
					in: 'query',
					required: false,
					description: 'Username prefix; empty matches nothing meaningful',
					schema: { type: 'string' },
				},
			],
			responses: { 200: json(AccountDto.array(), 'Matching public accounts') },
		}),
		async (c) => {
			const name = c.req.query('name') ?? ''
			const accounts = await searchAccounts(c.env.DB, name)
			return c.json(accounts.map(toAccountDto))
		}
	)

	// ---- Bulk / single lookup ------------------------------------------------
	// Register the static `bulk` path before the `/account/:id` param route.
	.get('/account/bulk', describeRoute(bulkRouteSpec('query')), bulkAccounts)
	// The 2023 client asks for its friends list as a POST with the ids in a
	// form-urlencoded body — the same `id=1&id=2&…` it would put in a query string,
	// moved off the URL because a few hundred friends overflow it. GET is the same
	// lookup and stays for callers (and docs) that prefer it.
	.post('/account/bulk', describeRoute(bulkRouteSpec('body')), bulkAccounts)

	.get(
		'/account/:id/bio',
		describeRoute({
			tags: ['Lookup'],
			summary: 'A player’s bio',
			parameters: [
				{
					name: 'id',
					in: 'path',
					required: true,
					description: 'Account id; non-numeric is 400',
					schema: { type: 'string' },
				},
			],
			responses: {
				200: json(BioResponse, 'The bio (empty string when unset)'),
				400: { description: 'Non-numeric id (empty body)' },
			},
		}),
		async (c) => {
			const accountId = Number.parseInt(c.req.param('id'), 10)
			if (Number.isNaN(accountId)) return c.body(null, 400)
			// Bio is stored on the account JSON (set via PUT /account/me/bio).
			const account = await getAccount(c.env.DB, accountId)
			return c.json({ accountId, bio: account?.bio ?? '' })
		}
	)

	.get(
		'/account/:id',
		describeRoute({
			tags: ['Lookup'],
			summary: 'A single public account',
			description: 'An id with no stored row falls back to a synthesized default account.',
			parameters: [
				{
					name: 'id',
					in: 'path',
					required: true,
					description: 'Account id; non-numeric is 400',
					schema: { type: 'string' },
				},
			],
			responses: {
				200: json(AccountDto, 'The public account'),
				400: { description: 'Non-numeric id (empty body)' },
			},
		}),
		async (c) => {
			const accountId = Number.parseInt(c.req.param('id'), 10)
			if (Number.isNaN(accountId)) return c.body(null, 400)
			// Load the stored account, falling back to a synthesized default.
			return c.json(
				toAccountDto((await getAccount(c.env.DB, accountId)) ?? defaultAccount(accountId))
			)
		}
	)

	// ---- Create --------------------------------------------------------------
	.post(
		'/account/create',
		describeRoute({
			tags: ['Self'],
			summary: 'Create an account',
			description: [
				'Mints a new account with an auto-assigned random username (players don’t choose',
				'one initially). Not auth-gated. `platformId` is parsed but not yet persisted.',
			].join(' '),
			requestBody: form(CreateAccountRequest, 'Platform fields'),
			responses: { 200: json(CreateAccountResult, 'The created account, in a result envelope') },
		}),
		async (c) => {
			// Parsed for fidelity; unused until there's a DB to persist CachedLogins.
			const platform = await formField(c, 'platform')
			await formField(c, 'platformId')

			// Persist a new account with an auto-assigned random username (players
			// don't choose one initially).
			const platforms = Number.parseInt(platform, 10)
			const account = await createAccount(c.env.DB, {
				platforms: Number.isNaN(platforms) ? 0 : platforms,
			})
			// TODO: also create a dorm Room/SubRoom for the new account.
			return c.json({ success: true, value: toAccountDto(account) })
		}
	)

	// ---- Parental control ----------------------------------------------------
	.get(
		'/parentalcontrol/me',
		describeRoute({
			tags: ['Self'],
			summary: 'The caller’s parental-control flags',
			description: 'Nothing stores parental controls yet; purchases are always allowed.',
			security: AUTHED,
			responses: {
				200: json(ParentalControl, 'Parental-control flags'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			return c.json({ accountId: id, disallowInAppPurchases: true })
		}
	)

	// Privacy settings for an account. A bare `{}` fails the client's deserializer
	// ("Deserialization returned null") — it needs the fields, so echo the id back and
	// report recent history as visible. Nothing stores per-player privacy yet.
	.get(
		'/accountprivacysettings/:id{[0-9]+}',
		describeRoute({
			tags: ['Lookup'],
			summary: 'An account’s privacy settings',
			description: [
				'Nothing stores per-player privacy yet; the id is echoed and recent history is',
				'reported visible (a bare `{}` fails the client’s deserializer).',
			].join(' '),
			parameters: [
				{
					name: 'id',
					in: 'path',
					required: true,
					description: 'Account id (digits only)',
					schema: { type: 'string', pattern: '^[0-9]+$' },
				},
			],
			responses: { 200: json(PrivacySettings, 'Privacy settings') },
		}),
		(c) =>
			c.json({
				accountId: Number.parseInt(c.req.param('id'), 10),
				isRecentHistoryVisible: true,
			})
	)

	// ---- Profile mutations ---------------------------------------------------
	// Set the player's display name (persisted on the account row).
	.put(
		'/account/me/displayname',
		describeRoute({
			tags: ['Profile'],
			summary: 'Set display name',
			description: 'Persisted and broadcast via an AccountUpdate notification.',
			security: AUTHED,
			responses: {
				200: json(SuccessResponse, 'Updated'),
				400: {
					description: 'Empty, over 15 characters, non-alphanumeric, or profane (empty body)',
				},
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		// An EMPTY 400, which is what this route already answered for an empty name: it
		// acks with a bare SuccessResponse and has never sent the client a body on
		// failure, so enforcing the schema doesn't change what a refusal looks like.
		validator('form', DisplayNameRequest, (r, c) => (r.success ? undefined : c.body(null, 400))),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const { displayName } = c.req.valid('form')
			const account = await updateAccount(c.env.DB, id, { displayName })
			await pushAccountUpdate(c, account)
			return c.json({ success: true })
		}
	)

	// Change the caller's username. Rejects a name already taken by another account,
	// and requires the account to have username changes remaining. On success the
	// new name is persisted and the remaining-changes counter is decremented.
	.put(
		'/account/me/username',
		describeRoute({
			tags: ['Profile'],
			summary: 'Change username',
			description: [
				'Letters and digits only, at most 50 characters, and free of profanity (the same',
				'word list as `api`’s `POST /api/sanitize/v1/isPure`). Rejects a name taken by another',
				'account and requires a remaining change; on success the name is persisted and',
				'the counter decremented. Always HTTP 200 — failures carry a message in `error`',
				'(see the UsernameResult envelope).',
			].join(' '),
			security: AUTHED,
			responses: {
				200: json(UsernameResult, 'Result envelope (success or a validation error)'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		// Shape is checked before the handler runs, so a rejected name costs no D1 read and
		// — the part that matters — can never spend one of the account's rationed changes.
		// The message is relayed rather than zod's issue array: `nameRejection` writes the
		// sentence the player reads, and nothing can render an array of issues.
		// `c` is annotated so the hook's context matches this app's bindings, and `error` is
		// Standard Schema's flat issue list rather than a zod error object.
		validator('form', UsernameRequest, (r, c: Context<App>) =>
			r.success
				? undefined
				: usernameResult(c, r.error[0]?.message ?? 'That username cannot be used.')
		),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			const { username } = c.req.valid('form')

			// Duplicate check first (case-insensitive); keeping your own name is allowed.
			const existing = await getAccountByUsername(c.env.DB, username)
			if (existing && existing.accountId !== id) {
				return usernameResult(c, 'That username is already taken.')
			}

			// Then require a remaining change.
			const account = (await getAccount(c.env.DB, id)) ?? defaultAccount(id)
			const remaining = account.availableUsernameChanges ?? DEFAULT_USERNAME_CHANGES
			if (remaining <= 0) {
				return usernameResult(c, 'You have no username changes remaining.')
			}

			const updated = await updateAccount(c.env.DB, id, {
				username,
				displayName: username,
				availableUsernameChanges: remaining - 1,
			})
			await pushAccountUpdate(c, updated)
			return usernameResult(c, '', toAccountDto(updated))
		}
	)

	// Set the player's email (persisted on the account row; surfaced by /account/me).
	.post(
		'/account/me/email',
		describeRoute({
			tags: ['Profile'],
			summary: 'Set email',
			description: 'Persisted; surfaced only by `/account/me`. Not broadcast.',
			security: AUTHED,
			responses: {
				200: json(SuccessResponse, 'Updated'),
				400: { description: 'Not a syntactically valid address (empty body)' },
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		validator('form', EmailRequest, (r, c) => (r.success ? undefined : c.body(null, 400))),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const { email } = c.req.valid('form')
			await updateAccount(c.env.DB, id, { email })
			return c.json({ success: true })
		}
	)

	// Set the player's phone (persisted on the account row).
	.post(
		'/account/me/phone',
		describeRoute({
			tags: ['Profile'],
			summary: 'Set phone number',
			description: 'Persisted on the account row. Not broadcast.',
			security: AUTHED,
			responses: {
				200: json(SuccessResponse, 'Updated'),
				400: { description: 'Empty phone (empty body)' },
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		validator('form', PhoneRequest, (r, c) => (r.success ? undefined : c.body(null, 400))),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const { phone } = c.req.valid('form')
			await updateAccount(c.env.DB, id, { phone })
			return c.json({ success: true })
		}
	)

	// Set the player's identityFlags bitmask (persisted; surfaced by /account/me).
	// `identityFlags` is part of the public account DTO, so the update has to be pushed
	// — see the note on personalpronouns below.
	.put(
		'/account/me/identityflags',
		describeRoute({
			tags: ['Profile'],
			summary: 'Set identity flags',
			description: [
				'`identityFlags` bitmask. In the public DTO, so the update is broadcast via',
				'AccountUpdate.',
			].join(' '),
			security: AUTHED,
			requestBody: form(IdentityFlagsRequest, 'The identityFlags bitmask'),
			responses: {
				200: json(SuccessResponse, 'Updated'),
				400: { description: 'Non-numeric identityFlags (empty body)' },
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const identityFlags = Number.parseInt((await formField(c, 'identityFlags')).trim(), 10)
			if (Number.isNaN(identityFlags)) return c.body(null, 400)
			const account = await updateAccount(c.env.DB, id, { identityFlags })
			await pushAccountUpdate(c, account)
			return c.json({ success: true })
		}
	)

	// Set the player's personalPronouns (posted as `pronounFlags`; persisted).
	// The response body carries no account, so the client only learns the new value from
	// the `SelfAccountUpdate`/`AccountUpdate` the hub pushes — without it the player's own
	// UI (and every other client, since personalPronouns is in the public DTO) keeps
	// showing the old pronouns until something else refetches the account.
	.put(
		'/account/me/personalpronouns',
		describeRoute({
			tags: ['Profile'],
			summary: 'Set personal pronouns',
			description: [
				'Posted as `pronounFlags`. The response carries no account, so the client learns',
				'the new value only from the broadcast AccountUpdate.',
			].join(' '),
			security: AUTHED,
			requestBody: form(PronounsRequest, 'The pronounFlags bitmask'),
			responses: {
				200: json(SuccessResponse, 'Updated'),
				400: { description: 'Non-numeric pronounFlags (empty body)' },
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const personalPronouns = Number.parseInt((await formField(c, 'pronounFlags')).trim(), 10)
			if (Number.isNaN(personalPronouns)) return c.body(null, 400)
			const account = await updateAccount(c.env.DB, id, { personalPronouns })
			await pushAccountUpdate(c, account)
			return c.json({ success: true })
		}
	)

	.put(
		'/account/me/bio',
		describeRoute({
			tags: ['Profile'],
			summary: 'Set bio',
			description: 'Free text up to 255 characters; empty is allowed. Persisted and broadcast.',
			security: AUTHED,
			responses: {
				200: json(SuccessResponse, 'Updated'),
				400: { description: 'Bio over 255 characters (empty body)' },
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		// Refused rather than truncated: silently storing half a sentence reads as data loss.
		validator('form', BioRequest, (r, c) => (r.success ? undefined : c.body(null, 400))),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const { bio } = c.req.valid('form')
			const account = await updateAccount(c.env.DB, id, { bio })
			await pushAccountUpdate(c, account)
			return c.json({ success: true })
		}
	)

	// The emoji shown beside the player's display name. The body is a single field —
	// `displayEmoji=%F0%9F%A4%AA` — and the value is checked against the same list
	// `GET /emojiConfig/whitelistedEmojis` serves, then stored in that list's CANONICAL
	// form: `displayEmoji` is compared as a plain string, and the client highlights the
	// current pick by matching it against the picker list it fetched, so a stored value
	// that differs only by a variation selector highlights nothing.
	//
	// Broadcast like every other public-DTO mutation here — the emoji rides along in the
	// AccountUpdate payload, so it redraws beside the name without a refetch.
	.put(
		'/account/me/emoji',
		describeRoute({
			tags: ['Profile'],
			summary: 'Set display emoji',
			description: [
				'Persists the emoji shown beside the display name and broadcasts it in the',
				'AccountUpdate payload. The value must be one the whitelist serves; an empty value',
				'clears the pick.',
			].join(' '),
			security: AUTHED,
			requestBody: form(EmojiRequest, 'A whitelisted emoji, or "" to clear'),
			responses: {
				200: json(SuccessResponse, 'Updated'),
				400: { description: 'Not a whitelisted emoji (empty body)' },
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const submitted = (await formField(c, 'displayEmoji')).trim()
			// An empty value clears the pick; anything else has to be on the list.
			const displayEmoji = submitted === '' ? '' : resolveWhitelistedEmoji(submitted)
			if (displayEmoji === null) return c.body(null, 400)
			const account = await updateAccount(c.env.DB, id, { displayEmoji })
			await pushAccountUpdate(c, account)
			return c.json({ success: true })
		}
	)

	// The profile banner — the wide image behind the header on a player's profile. Same
	// shape as the avatar below: the body names an image the player has already uploaded
	// (the client posts one of their own photos, `sharecamera/<date>/<uuid>.jpg`), so this
	// stores a key and never bytes.
	//
	// Broadcasts the AccountUpdate like every other profile mutation here — the banner rides
	// along in the DTO payload, so anyone looking at the profile redraws it without a refetch.
	.put(
		'/account/me/bannerimage',
		describeRoute({
			tags: ['Profile'],
			summary: 'Set profile banner image',
			description:
				'Persists the banner object key and broadcasts it in the AccountUpdate payload. The ' +
				'key names an image the player already uploaded — typically one of their own photos ' +
				'(`sharecamera/…`) — so nothing is uploaded here.',
			security: AUTHED,
			requestBody: form(BannerImageRequest, 'The banner object key'),
			responses: {
				200: json(SuccessResponse, 'Updated'),
				400: { description: 'Empty imageName (empty body)' },
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const imageName = await formField(c, 'imageName')
			if (!imageName) return c.body(null, 400)
			const account = await updateAccount(c.env.DB, id, { bannerImage: imageName })
			await pushAccountUpdate(c, account)
			return c.json({ success: true })
		}
	)

	.put(
		'/account/me/profileimage',
		describeRoute({
			tags: ['Profile'],
			summary: 'Set profile image',
			description: 'Persists the avatar object key and broadcasts it in the AccountUpdate payload.',
			security: AUTHED,
			requestBody: form(ProfileImageRequest, 'The avatar object key'),
			responses: {
				200: json(SuccessResponse, 'Updated'),
				400: { description: 'Empty imageName (empty body)' },
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const imageName = await formField(c, 'imageName')
			if (!imageName) return c.body(null, 400)
			// Persist the new avatar key on the account row and fire the AccountUpdate
			// websocket (the new profileImage rides along in the DTO payload).
			const account = await updateAccount(c.env.DB, id, { profileImage: imageName })
			await pushAccountUpdate(c, account)
			return c.json({ success: true })
		}
	)

	// ---- Flux Social account linking -----------------------------------------
	// Real server-backed pairing between a Flux Rec game account and the Flux
	// Social website ("flux perring"). The in-game client mints a 6-digit
	// one-time code (`POST /account/me/fluxsocial/linkcode`); the player types it
	// into the website, which exchanges it for an opaque session token
	// (`POST /fluxsocial/exchange`). The token rides `Authorization: Bearer` on
	// later website calls. Unlinking deletes the session row — instant
	// revocation. Relinking (a fresh exchange) replaces it.
	//
	// Codes and tokens are stored as SHA-256 hashes only, mirroring the
	// refresh_tokens pattern. See apps/accounts/src/fluxsocial-db.ts and
	// apps/auth/migrations/0010_fluxsocial.sql.
	.post(
		'/account/me/fluxsocial/linkcode',
		describeRoute({
			tags: ['FluxSocial'],
			summary: 'Mint a Flux Social pairing code',
			description: [
				'Generates a 6-digit one-time code the player types into the Flux Social',
				'website (Settings → Connect Flux account). The raw code is returned ONCE —',
				'only its hash is stored. Single-use, expires after 600 seconds, and minting',
				'a new code invalidates any previous unconsumed one.',
			].join(' '),
			security: AUTHED,
			responses: {
				200: json(FluxSocialLinkCodeResponse, 'The raw pairing code, shown once'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			const code = await issueLinkCode(c.env.DB, id)
			return c.json({ code, expiresIn: LINK_CODE_TTL_SECONDS })
		}
	)

	.post(
		'/fluxsocial/exchange',
		describeRoute({
			tags: ['FluxSocial'],
			summary: 'Exchange a pairing code for a website session',
			description: [
				'The Flux Social website calls this with the 6-digit code the player generated',
				'in-game. The code is the credential — no auth header needed. Single-use: a',
				'code burns whether it succeeds or was expired. Success mints an opaque',
				'website session token (returned ONCE) bound to the account that generated the',
				'code. Throttled per IP against brute force.',
			].join(' '),
			requestBody: jsonBody(FluxSocialExchangeRequest, 'The 6-digit pairing code shown in-game'),
			responses: {
				200: json(FluxSocialExchangeResponse, 'Website session token, returned once'),
				400: { description: 'Unknown or malformed code (empty body)' },
				410: { description: 'Code expired (empty body)' },
				429: { description: 'Too many attempts from this IP (empty body)' },
			},
		}),
		async (c) => {
			const ip = clientIp(c)
			if (!(await checkExchangeThrottle(c.env.DB, ip))) return c.body(null, 429)
			const code = (await requestField(c, 'code')).trim()
			if (!/^\d{6}$/.test(code)) return c.body(null, 400)
			const result = await redeemLinkCode(c.env.DB, code)
			if (!result.ok) return c.body(null, result.reason === 'expired' ? 410 : 400)
			await clearExchangeThrottle(c.env.DB, ip)
			const token = await issueWebsiteSession(c.env.DB, result.accountId)
			const account = (await getAccount(c.env.DB, result.accountId)) ?? defaultAccount(result.accountId)
			return c.json({
				token,
				accountId: account.accountId,
				username: account.username,
				displayName: account.displayName,
				linkedAt: new Date().toISOString(),
			})
		}
	)

	.get(
		'/fluxsocial/me',
		describeRoute({
			tags: ['FluxSocial'],
			summary: 'Who the website session belongs to',
			description: [
				'Validates the website session token the site keeps in storage (call on page',
				'load). A 401 means the session was revoked or expired — the site should show',
				'the "link again" state.',
			].join(' '),
			security: WEBSITE_AUTHED,
			responses: {
				200: json(FluxSocialMeResponse, 'The linked account and its privacy settings'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await websiteSessionId(c)
			if (id === null) return unauthorized(c)
			const account = (await getAccount(c.env.DB, id)) ?? defaultAccount(id)
			const privacy = await getPrivacy(c.env.DB, id)
			return c.json({
				accountId: account.accountId,
				username: account.username,
				displayName: account.displayName,
				profileImage: account.profileImage,
				privacy,
			})
		}
	)

	.get(
		'/fluxsocial/status',
		describeRoute({
			tags: ['FluxSocial'],
			summary: 'Flux Social link status',
			description:
				'Whether the account has a live website session, plus its privacy settings. ' +
				'Accepts either the game JWT or the website session token.',
			security: WEBSITE_AUTHED,
			responses: {
				200: json(FluxSocialStatusResponse, 'Link status and privacy settings'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await fluxAccountId(c)
			if (id === null) return unauthorized(c)
			const linked = await hasWebsiteSession(c.env.DB, id)
			const privacy = await getPrivacy(c.env.DB, id)
			return c.json({ linked, privacy })
		}
	)

	.post(
		'/fluxsocial/unlink',
		describeRoute({
			tags: ['FluxSocial'],
			summary: 'Unlink the Flux Social website session',
			description: [
				'Revokes the website session instantly by deleting it — the stored token',
				'stops working on the next call. Idempotent. Accepts either the game JWT',
				'(unlink from in-game) or the website session token itself (unlink from the',
				'site). Relinking is just a fresh pairing-code exchange.',
			].join(' '),
			security: WEBSITE_AUTHED,
			responses: {
				200: json(SuccessResponse, 'Unlinked'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await fluxAccountId(c)
			if (id === null) return unauthorized(c)
			await revokeWebsiteSession(c.env.DB, id)
			return c.json({ success: true })
		}
	)

	.get(
		'/fluxsocial/privacy',
		describeRoute({
			tags: ['FluxSocial'],
			summary: 'Read Flux Social visibility settings',
			description:
				'The visibility toggles the Flux Social website honors. Accepts either the ' +
				'game JWT or the website session token. All default to visible.',
			security: WEBSITE_AUTHED,
			responses: {
				200: json(FluxSocialPrivacySettings, 'The visibility toggles'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await fluxAccountId(c)
			if (id === null) return unauthorized(c)
			return c.json(await getPrivacy(c.env.DB, id))
		}
	)

	.post(
		'/fluxsocial/privacy',
		describeRoute({
			tags: ['FluxSocial'],
			summary: 'Update Flux Social visibility settings',
			description: [
				'Sets the visibility toggles the Flux Social website honors (profile, rooms,',
				'photos, inventions). Accepts a partial object — omitted toggles keep their',
				'current value. Accepts either the game JWT or the website session token.',
			].join(' '),
			security: WEBSITE_AUTHED,
			requestBody: jsonBody(
				FluxSocialPrivacySettings,
				'The toggles to set; omitted ones are unchanged'
			),
			responses: {
				200: json(FluxSocialPrivacySettings, 'The updated toggles'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await fluxAccountId(c)
			if (id === null) return unauthorized(c)
			const body = await requestJson(c)
			const current = await getPrivacy(c.env.DB, id)
			const next: FluxSocialPrivacy = {
				showProfile: typeof body?.showProfile === 'boolean' ? body.showProfile : current.showProfile,
				showRooms: typeof body?.showRooms === 'boolean' ? body.showRooms : current.showRooms,
				showPhotos: typeof body?.showPhotos === 'boolean' ? body.showPhotos : current.showPhotos,
				showInventions:
					typeof body?.showInventions === 'boolean' ? body.showInventions : current.showInventions,
			}
			await setPrivacy(c.env.DB, id, next)
			return c.json(next)
		}
	)

// The generated spec. Documentation only — no request is validated against it (see
// openapi.ts). `hide: true` keeps this route out of its own output.
app.get(
	'/openapi.json',
	describeRoute({ hide: true }),
	withCleanSpec(
		openAPIRouteHandler(app, {
			documentation: {
				info: {
					title: 'recflare accounts',
					version: '1.0.0',
					description: [
						'Account reads, profile mutations and lookups for recflare, a private-server',
						'reimplementation of the Rec Room backend. Accounts live in the shared `recflare`',
						'D1 database, whose `account` schema is owned by the `auth` worker.',
					].join('\n'),
				},
				servers: [{ url: 'https://accounts.recflare.net', description: 'Production' }],
				components: {
					securitySchemes: {
						bearerAuth: {
							type: 'http',
							scheme: 'bearer',
							bearerFormat: 'JWT',
							description: 'An `access_token` from the auth worker’s `POST /connect/token`.',
						},
						fluxSocialAuth: {
							type: 'http',
							scheme: 'bearer',
							description:
								'A Flux Social website session token from `POST /fluxsocial/exchange` (opaque, not a JWT).',
						},
					},
				},
			},
		})
	)
)

export default app
