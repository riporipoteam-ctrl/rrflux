import { Hono } from 'hono'
import { describeRoute, openAPIRouteHandler } from 'hono-openapi'
import { useWorkersLogger } from 'workers-tagged-logger'

import { withCleanSpec, withNotFound, withOnError } from '@repo/hono-helpers'
import { validateAndGetAccountId } from '@repo/jwt'

import {
	AUTHED,
	boolQuery,
	GameAiAccessDenied,
	GameAiSpendSummaryDenied,
	HealthResponse,
	idParam,
	intQuery,
	json,
	jsonBody,
	MakerAiAccessResponse,
	MakerAiBalances,
	RealtimeSessionCreateBody,
	RealtimeSessionCreated,
	RealtimeSessionUnavailable,
	RoomieAiAccess,
	RoomieUserFacts,
	UNAUTHORIZED_RESPONSE,
} from './openapi'
import { z } from 'zod'
import { replyFromRipoBot } from './ripobot'
import { pcm16ToWav } from './pcm'
import { synthesizeRoomieMp3, synthesizeRoomiePcm } from './roomie-voice'
import { roomieDevSocket } from './roomie-dev'
import { createRoomieProviders, transcribeRoomieAudio } from './roomie-providers'
import { createRoomieSocket } from './roomie-socket'
import { createRoomieClientSecret, verifyRoomieClientSecret } from './roomie-session'

import type { Context } from 'hono'
import type { App } from './context'

/**
 * AI Worker. Serves the access checks and budget reads the client makes before offering
 * its AI features, plus Roomie's authenticated chat, speech and realtime-compatible routes:
 * `/roomieai/chat` (RipoBot's configured Hugging Face model), `/roomieai/speak`
 * (RipoBot Space Kokoro TTS) and `/roomieai/listen` (RipoBot Space Faster-Whisper STT).
 *
 * - Game AI is a SERVER-side feature. This server cannot provide it, so both its reads are
 *   refused and the client hides the feature.
 * - Roomie runs on the CLIENT. Budget reads are granted in full, and its realtime-compatible
 *   adapter keeps provider credentials server-side. Validate the 2026 client transport before
 *   claiming in-game compatibility.
 * - Maker AI meters model usage in dollars. Nothing here bills, so every figure is zero.
 */

/**
 * `int.MaxValue` — the client's energy fields are signed 32-bit ints, so this is the
 * largest budget it can hold. Anything larger (an int64 max, say) overflows on the way in
 * and lands as a negative number, i.e. no energy at all.
 */
const INT32_MAX = 2_147_483_647

/**
 * The reason both Game AI reads refuse with. `AI.RoomDoesNotSupportGameAI` is the id the
 * client renders a message for; the room it names makes no difference, there being no Game
 * AI backend behind any of them.
 */
const GAME_AI_UNSUPPORTED = {
	success: false,
	error_id: 'AI.RoomDoesNotSupportGameAI',
	error: 'This room does not support Rec Room Game AI',
} as const

/**
 * Resolve the account id from a Bearer token (the route is auth-gated).
 * Returns `null` when the header is missing, the token is invalid, or the `sub` claim
 * isn't an integer.
 */
async function authedId(c: Context<App>): Promise<number | null> {
	return validateAndGetAccountId(c.req.raw, await c.env.JWT_SECRET.get())
}

/** Results.Unauthorized() equivalent — 401 with empty body. */
function unauthorized(c: Context<App>) {
	return c.body(null, 401)
}

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

	.onError(withOnError())
	.notFound(withNotFound())

	// Root health check.
	.get(
		'/',
		describeRoute({
			tags: ['Service'],
			summary: 'Health check',
			description: 'Liveness probe for the ai worker. No auth.',
			responses: { 200: json(HealthResponse, 'Service is up') },
		}),
		(c) => c.json({ service: 'ai', status: 'ok' })
	)

	// Whether the caller may use Game AI in a room. Always refused: no Game AI backend
	// exists here, so the honest answer for every room is that it doesn't support it.
	.get(
		'/gameai/user/access',
		describeRoute({
			tags: ['Game AI', '2025'],
			summary: 'May the caller use Game AI here?',
			description: [
				'Asked before the client offers any Game AI feature in a room. This server hosts no',
				'Game AI, so it always refuses — with a 200 carrying `success: false`, NOT an HTTP',
				'error: the client branches on the body, and an error status would read as a failed',
				'request rather than the “not available here” state this is. `AI.RoomDoesNotSupportGameAI`',
				'is the reason the client renders.',
				'',
				'`roomId` is accepted and ignored — the answer is the same for every room, and the',
				'refusal is per-room by nature, so the client asks again for the next one. The token',
				'is still validated first, as the reference does.',
			].join(' '),
			security: AUTHED,
			parameters: [intQuery('roomId', 'The room the client is asking about. Ignored.')],
			responses: {
				200: json(GameAiAccessDenied, 'Always a refusal'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			return c.json(GAME_AI_UNSUPPORTED)
		}
	)

	// What a room has spent on Game AI. Refused for the same reason as the access check
	// above, but note the body differs: this one carries an explicit `value: null`.
	.get(
		'/gameai/room/:roomId{[0-9]+}/spendsummary',
		describeRoute({
			tags: ['Game AI', '2025'],
			summary: 'A room’s Game AI spend summary',
			description: [
				'What a room has spent of its Game AI budget. Refused with the same 200-plus-',
				'`success: false` body as the access check, since a room that cannot use Game AI has',
				'no spend to summarise.',
				'',
				'The body is NOT identical to the access check’s: it carries `value: null` where that',
				'one omits the key entirely. The access check answers a yes/no and has nothing to',
				'carry; this endpoint’s payload slot exists and is empty. Reproduced as the reference',
				'server sends it — don’t unify the two.',
			].join(' '),
			security: AUTHED,
			parameters: [idParam('roomId', 'The room being asked about. Ignored.')],
			responses: {
				200: json(GameAiSpendSummaryDenied, 'Always a refusal'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			return c.json({ ...GAME_AI_UNSUPPORTED, value: null })
		}
	)

	// Roomie AI's energy budget. Granted, unlike Game AI above: Roomie runs on the client
	// and only asks this service how much energy it has, so the honest answer for a server
	// that meters nothing is "as much as you can count".
	.get(
		'/roomieai/user/access',
		describeRoute({
			tags: ['Roomie AI', '2026'],
			summary: 'The caller’s Roomie AI energy budget',
			description: [
				'What Roomie may spend: an energy ceiling, what is left of it, and when it next',
				'refills. Nothing here meters energy, so the budget is `int.MaxValue` and never',
				'depletes — which is why `NextSubscriptionEnergyRechargeAt` is null, there being no',
				'spend to recharge from.',
				'',
				'The envelope is `{ success, error_id, error, value }`, NOT the flat body the Game AI',
				'check answers with. The two are different shapes on purpose — don’t unify them.',
			].join(' '),
			security: AUTHED,
			responses: {
				200: json(RoomieAiAccess, 'The energy budget — always granted, always full'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			return c.json({
				success: true,
				error_id: null,
				error: null,
				value: {
					MaxEnergyFromSubscriptions: INT32_MAX,
					EnergyLeft: INT32_MAX,
					NextSubscriptionEnergyRechargeAt: null,
					OutputAudioEnabled: true,
				},
			})
		}
	)

	// What Roomie has been told about the caller. Nothing observes players here, so it has
	// been told nothing.
	.get(
		'/roomieai/user/facts',
		describeRoute({
			tags: ['Roomie AI', '2026'],
			summary: 'What Roomie knows about the caller',
			description: [
				'The memory Roomie is primed with: `UserContext`, a prose profile written from past',
				'conversations, and `UserFacts`, the discrete `(Predicate, Object)` claims behind it —',
				'live, these are things the player told Roomie about themselves.',
				'',
				'Both are empty here. Nothing on this server observes a conversation, so there is',
				'nothing to remember, and Roomie starts every session knowing nothing about who it is',
				'talking to. A flat body, like the Maker AI balances and unlike the access check',
				'above.',
			].join(' '),
			security: AUTHED,
			responses: {
				200: json(RoomieUserFacts, 'An empty profile — always'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			return c.json({ UserContext: '', UserFacts: [] })
		}
	)

	// Roomie chat. Uses the same Hugging Face model configured for RipoBot. It has no
	// anonymous third-party or OpenAI fallback. The native realtime client does not call
	// this custom HTTP endpoint without a separate compatible integration.
	.post(
		'/roomieai/chat',
		describeRoute({
			tags: ['Roomie AI', '2026'],
			summary: 'Chat with Roomie',
			description:
				'Sends the player message to RipoBot’s configured Hugging Face model with a Roomie system ' +
				'prompt. Returns service unavailable when Hugging Face is not configured.',
			security: AUTHED,
			responses: {
				200: json(z.object({}).passthrough(), 'Roomie\'s reply'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			let body: { message?: string; history?: unknown[] }
			try {
				body = await c.req.json()
			} catch {
				return c.json({ success: false, error: 'invalid_json' }, 400)
			}
			const message = (body.message || '').trim()
			if (!message) {
				return c.json({ success: false, error: 'empty_message' }, 400)
			}
			if (message.length > 2000) {
				return c.json({ success: false, error: 'message_too_long' }, 413)
			}

			const systemPrompt =
				'You are Roomie, a friendly AI pet companion in the game Flux Rec (a Rec Room-style ' +
				'social VR game). You are cheerful, playful, and helpful. Keep replies short and fun ' +
				'(1-3 sentences) since players read them in-game. You love the game world, making ' +
				'friends, and going on adventures. Never break character. Never mention you are an AI ' +
				'language model — you are Roomie, the player\'s loyal companion.'
			const history = Array.isArray(body.history)
				? body.history.slice(-10).flatMap((entry) => {
						if (!entry || typeof entry !== 'object') return []
						const item = entry as { role?: unknown; content?: unknown }
						if ((item.role !== 'user' && item.role !== 'assistant') || typeof item.content !== 'string') {
							return []
						}
						const content = item.content.trim().slice(0, 1000)
						return content ? [{ role: item.role, content }] : []
					})
				: []
			const messages = [
				{ role: 'system', content: systemPrompt },
				...history,
				{ role: 'user', content: message },
			]
			const token = c.env.HF_TOKEN
			if (!token) return c.json({ success: false, error: 'roomie_model_not_configured' }, 503)
			const reply = await replyFromRipoBot(token, messages, fetch, c.env.HF_MODEL)
			if (!reply) return c.json({ success: false, error: 'roomie_model_unavailable' }, 503)
			return c.json({ success: true, error: null, value: { reply } })
		}
	)

	// Roomie voice (TTS). Google Translate supplies the legacy MP3 path. PCM output is
	// sent only to a separately configured RipoBot voice endpoint; no OpenAI fallback.
	.post(
		'/roomieai/speak',
		describeRoute({
			tags: ['Roomie AI', '2026'],
			summary: 'Roomie text-to-speech',
			description:
				'Converts text to MP3 with Google TTS for the legacy path or PCM16 with a configured ' +
				'RipoBot voice service.',
			security: AUTHED,
			responses: {
				200: json(z.object({}).passthrough(), 'Audio data'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			let body: { text?: string; voice?: string; format?: string }
			try {
				body = await c.req.json()
			} catch {
				return c.json({ success: false, error: 'invalid_json' }, 400)
			}
			const text = (body.text || '').trim()
			if (!text) {
				return c.json({ success: false, error: 'empty_text' }, 400)
			}
			if (body.format === 'pcm16') {
				const token = c.env.ROOMIE_VOICE_TOKEN
				const voiceUrl = c.env.ROOMIE_VOICE_URL
				if (!token || !voiceUrl) return c.json({ success: false, error: 'pcm_voice_not_configured' }, 503)
				const audio = await synthesizeRoomiePcm(text, token, voiceUrl)
				if (!audio) return c.json({ success: false, error: 'pcm_voice_unavailable' }, 503)
				return new Response(audio as BodyInit, { headers: {
					'Content-Type': 'application/octet-stream', 'Cache-Control': 'no-store',
					'X-Audio-Format': 'pcm16le', 'X-Audio-Sample-Rate': '24000', 'X-Audio-Channels': '1',
				} })
			}

			const audio = await synthesizeRoomieMp3(text)
			if (!audio) return c.json({ success: false, error: 'tts_unavailable' }, 502)
			return new Response(audio, {
				headers: { 'Content-Type': 'audio/mpeg', 'Cache-Control': 'no-store' },
			})
		}
	)

	// Roomie hearing (STT). Audio goes to the private RipoBot Space's local Faster-Whisper
	// endpoint; no Cloudflare AI or OpenAI speech endpoint is used.
	.post(
		'/roomieai/listen',
		describeRoute({
			tags: ['Roomie AI', '2026'],
			summary: 'Roomie speech-to-text',
			description:
				'Transcribes player mic audio to text using Whisper, so players can talk ' +
				'to Roomie with their voice.',
			security: AUTHED,
			responses: {
				200: json(z.object({}).passthrough(), 'Transcribed text'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			const contentType = c.req.header('content-type') || ''
			let audio: ArrayBuffer
			if (contentType.includes('application/json')) {
				// Base64-encoded audio in JSON: { audio: "base64...", format: "mp3" }
				let body: { audio?: string; format?: string }
				try {
					body = await c.req.json()
				} catch {
					return c.json({ success: false, error: 'invalid_json' }, 400)
				}
				if (!body.audio) {
					return c.json({ success: false, error: 'empty_audio' }, 400)
				}
				if (body.audio.length > 6_400_000) return c.json({ success: false, error: 'audio_too_large' }, 413)
				try {
					const binary = atob(body.audio)
					const bytes = new Uint8Array(binary.length)
					for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
					const converted = body.format === 'pcm16' ? pcm16ToWav(bytes) : bytes
					audio = converted.buffer as ArrayBuffer
				} catch {
					return c.json({ success: false, error: 'invalid_audio' }, 400)
				}
			} else {
				// Raw audio bytes in the body
				audio = await c.req.arrayBuffer()
				if (!audio.byteLength) {
					return c.json({ success: false, error: 'empty_audio' }, 400)
				}
			}

			const token = c.env.ROOMIE_VOICE_TOKEN
			const voiceUrl = c.env.ROOMIE_VOICE_URL || 'https://echoxr-ripobot.hf.space/roomie/tts'
			const sttUrl = c.env.ROOMIE_STT_URL || voiceUrl.replace(/\/tts\/?$/, '/stt')
			if (!token || !sttUrl) return c.json({ success: false, error: 'stt_not_configured' }, 503)
			if (audio.byteLength > 1_500_000) return c.json({ success: false, error: 'audio_too_large' }, 413)
			const text = await transcribeRoomieAudio(new Uint8Array(audio), token, sttUrl)
			if (text === null) return c.json({ success: false, error: 'stt_unavailable' }, 502)
			return c.json({ success: true, error: null, value: { text } })
		}
	)

	// Whether the caller may use Maker AI at all. Granted, like the Roomie budget reads and
	// unlike the Game AI checks: this is a gate, not a model call, and refusing it hides the
	// feature outright. (The balances below are still zeroed: the client reads its usage
	// meter separately, and a server that bills nothing has spent nothing.)
	//
	// The envelope is its own shape again — PascalCase `Success`/`Error` beside a snake_case
	// `error_id`, which is neither the Game AI refusal's all-lowercase body nor the Roomie
	// check's `{ success, error_id, error, value }`. Reproduced as the reference sends it;
	// the mixed casing is not a typo to tidy up.
	.get(
		'/makerai/user/access',
		describeRoute({
			tags: ['Maker AI', '2025'],
			summary: 'May the caller use Maker AI?',
			description: [
				'Asked before the client offers Maker AI. Always granted — the gate is about',
				'entitlement, not capacity, and nothing here meters what Maker AI would cost.',
				'',
				'The envelope carries PascalCase `Success`/`Error` next to a snake_case `error_id`,',
				'which matches neither neighbour on this worker. That mix is what the reference sends;',
				'it is not an inconsistency to clean up.',
				'',
				'`roomInstanceSpecificCheck` (the client sends .NET’s `True`/`False`) is accepted and',
				'ignored: it asks whether the check is about the instance the player is standing in',
				'rather than the account, and the answer is the same either way. The token is still',
				'validated first.',
			].join(' '),
			security: AUTHED,
			parameters: [
				boolQuery(
					'roomInstanceSpecificCheck',
					'Whether to check the current room instance rather than the account. Ignored.'
				),
			],
			responses: {
				200: json(MakerAiAccessResponse, 'Always granted'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			return c.json({ Success: true, Error: null, error_id: null })
		}
	)

	// Maker AI's dollar balances. Zeroed rather than refused: the client reads these to
	// render a usage meter, and a server that bills nothing has spent nothing.
	.get(
		'/makerai/user/balances',
		describeRoute({
			tags: ['Maker AI', '2025'],
			summary: 'The caller’s Maker AI usage balances',
			description: [
				'What Maker AI has cost the caller. Live, these meter model usage in DOLLARS against',
				'a per-user ceiling and a separate RR+ allowance, and the client renders them as a',
				'usage bar with a status word.',
				'',
				'Nothing here bills for model usage, so every figure is zero and both usage buckets',
				'report `Good` — an untouched allowance, not an exhausted one. The time bucket is',
				'`Empty` with `TimeExpiresAt` at `DateTime.MinValue`, this server selling no timed',
				'access for it to hold.',
				'',
				'A flat body — no `{ success, error, value }` envelope, unlike the Roomie access check.',
			].join(' '),
			security: AUTHED,
			responses: {
				200: json(MakerAiBalances, 'All zero — nothing is metered here'),
				401: UNAUTHORIZED_RESPONSE,
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)

			return c.json({
				UsageDollars: 0,
				UsersMaxUsageDollars: 0,
				RRPlusUsageDollars: 0,
				UsersMaxRRPlusUsageDollars: 0,
				TimeBalanceStatus: 'Empty',
				TimeExpiresAt: '0001-01-01T00:00:00',
				UsageBalanceStatus: 'Good',
				UsagePercent: 0,
				RRPlusUsageBalanceStatus: 'Good',
				RRPlusUsagePercent: 0,
			})
		}
	)

	// Open a client session. The returned key is app-local and can only authenticate to the
	// Realtime-compatible socket below; no provider credential reaches the game client.
	.post(
		'/realtime-session/create',
		describeRoute({
			tags: ['Roomie AI', '2026'],
			summary: 'Open a realtime AI session',
			description:
				'Creates a 15-minute Roomie credential. The game connects to this service’s realtime ' +
				'socket; RipoBot Space Faster-Whisper handles transcription, its configured Hugging Face ' +
				'model handles replies, and the Space Kokoro endpoint produces PCM voice.',
			security: AUTHED,
			requestBody: jsonBody(RealtimeSessionCreateBody, 'Roomie realtime session options.'),
			responses: {
				200: json(RealtimeSessionCreated, 'Short-lived Roomie session credentials'),
				401: UNAUTHORIZED_RESPONSE,
				503: json(RealtimeSessionUnavailable, 'Roomie model or voice service is not configured'),
			},
		}),
		async (c) => {
			const id = await authedId(c)
			if (id === null) return unauthorized(c)
			let body: unknown
			try { body = await c.req.json() } catch {
				return c.json({ Success: false, Error: 'Invalid realtime session request', error_id: 'AI.InvalidRequest', Value: null }, 400)
			}
			const parsed = RealtimeSessionCreateBody.safeParse(body)
			if (!parsed.success || !['RoomieAI', 'Roomie'].includes(parsed.data.AIType || 'RoomieAI')) {
				return c.json({ Success: false, Error: 'Unsupported realtime assistant', error_id: 'AI.UnsupportedAssistant', Value: null }, 400)
			}
			if (!c.env.ROOMIE_VOICE_TOKEN || !c.env.ROOMIE_VOICE_URL) {
				return c.json({ Success: false, Error: 'Roomie service is not configured', error_id: 'AI.RoomieUnavailable', Value: null }, 503)
			}
			const voiceUrl = c.env.ROOMIE_VOICE_URL
			const chatUrl = c.env.ROOMIE_CHAT_URL || voiceUrl.replace(/\/tts\/?$/, '/chat')
			const sttUrl = c.env.ROOMIE_STT_URL || voiceUrl.replace(/\/tts\/?$/, '/stt')
			if (!chatUrl || !sttUrl) {
				return c.json({ Success: false, Error: 'Roomie service endpoints are invalid', error_id: 'AI.RoomieUnavailable', Value: null }, 503)
			}
			const sessionId = crypto.randomUUID()
			const clientSecret = await createRoomieClientSecret(await c.env.JWT_SECRET.get(), id, sessionId, {
				model: parsed.data.Model,
				instructions: parsed.data.Instructions,
				voice: 'af_heart',
			})
			return c.json({ Success: true, Error: null, error_id: null, Value: { SessionId: sessionId, ClientSecret: clientSecret } })
		}
	)

// Realtime-compatible bridge. The exact 2026 client transport must be verified to route
// WebSocket traffic here; the client secret and service credentials stay server-side.
app.get('/v1/realtime', async (c) => {
	if (c.req.header('upgrade')?.toLowerCase() !== 'websocket') return c.body(null, 426)
	const match = /^Bearer\s+(\S+)$/i.exec(c.req.header('authorization') || '')
	if (!match) return c.body(null, 401)
	const claims = await verifyRoomieClientSecret(await c.env.JWT_SECRET.get(), match[1])
	if (!claims) return c.body(null, 401)
	const serviceToken = c.env.ROOMIE_VOICE_TOKEN
	const voiceUrl = c.env.ROOMIE_VOICE_URL || 'https://echoxr-ripobot.hf.space/roomie/tts'
	const chatUrl = c.env.ROOMIE_CHAT_URL || voiceUrl.replace(/\/tts\/?$/, '/chat')
	const sttUrl = c.env.ROOMIE_STT_URL || voiceUrl.replace(/\/tts\/?$/, '/stt')
	if (!serviceToken) return c.body(null, 503)
	return createRoomieSocket(createRoomieProviders({
		serviceToken,
		chatUrl,
		voiceUrl,
		sttUrl,
	}), { id: claims.sid, model: claims.model, instructions: claims.instructions, voice: claims.voice })
})

// This experimental socket is disabled by default, is impossible to enable in
// production, and requires both a player JWT and a separate development key.
app.get('/roomieai/realtime-dev', describeRoute({ hide: true }), async (c) => {
	if (c.env.ENVIRONMENT !== 'development' && c.env.ENVIRONMENT !== 'VITEST') return c.body(null, 404)
	if (await authedId(c) === null) return unauthorized(c)
	return roomieDevSocket(c.req.raw, c.env)
})

// The generated spec. Documentation only — no request is validated against it (see
// openapi.ts). `hide: true` keeps this route out of its own output.
app.get(
	'/openapi.json',
	describeRoute({ hide: true }),
	withCleanSpec(
		openAPIRouteHandler(app, {
			documentation: {
				info: {
					title: 'recflare ai',
					version: '1.0.0',
					description: [
						'The AI service for recflare, a private-server reimplementation of the Rec Room',
						'backend. The client checks here before offering any of its AI features: Game AI in a',
						'room, the Roomie assistant, and Maker AI’s usage meter.',
						'',
						'Roomie custom chat uses the configured RipoBot Hugging Face model; the legacy MP3',
						'speech path uses Google TTS, while PCM output requires a configured RipoBot voice URL.',
						'',
						'Game AI is a server-side feature this server cannot provide, so both its reads',
						'refuse. Roomie and Maker AI only ask what the caller may SPEND, which nothing here',
						'meters, so those reads are granted in full; session creation returns a short-lived',
						'worker credential for the realtime-compatible Roomie adapter. Validate the game',
						'transport against the 2026 client before claiming in-game compatibility.',
						'',
						'The refusals are 200s carrying `success: false`, which is the shape the client',
						'branches on — the worker exists so the client gets a definite answer on the host its',
						'endpoints document names, instead of a failed request.',
					].join('\n'),
				},
				servers: [{ url: 'https://ai.recflare.net', description: 'Production' }],
				components: {
					securitySchemes: {
						bearerAuth: {
							type: 'http',
							scheme: 'bearer',
							bearerFormat: 'JWT',
							description: 'An `access_token` from the auth worker’s `POST /connect/token`.',
						},
					},
				},
			},
		})
	)
)

export default app
