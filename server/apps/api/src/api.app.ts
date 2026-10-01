import { Hono } from 'hono'
import { describeRoute, openAPIRouteHandler } from 'hono-openapi'
import { useWorkersLogger } from 'workers-tagged-logger'

import { withCleanSpec, withDefaultCors, withNotFound, withOnError } from '@repo/hono-helpers'

import { accountRoutes } from './routes/account'
import { adminRoutes } from './routes/admin'
import { avatarRoutes } from './routes/avatar'
import { configRoutes } from './routes/config'
import { eventRoutes } from './routes/events'
import { gameplayRoutes } from './routes/gameplay'
import { imageRoutes } from './routes/images'
import { inventoryRoutes } from './routes/inventory'
import { moderationRoutes } from './routes/moderation'
import { progressionRoutes } from './routes/progression'
import { roomRoutes } from './routes/rooms'
import { socialRoutes } from './routes/social'

import { buildEndpoints } from '../../ns/src/endpoints'

import type { Context } from 'hono'
import type { App, Env } from './context'

/**
 * The Game API surface. Endpoints that would be backed by a database or on-disk
 * JSON files are stubbed here — no bindings yet.
 * Auth-gated routes still validate the Bearer JWT issued by the `auth` worker.
 *
 * Placeholder responses for file-backed endpoints are marked `TODO: hydrate`.
 *
 * Routes are grouped into per-domain controllers under `./routes` and mounted
 * at `/` below. Shared request helpers live in `./http`.
 */

// strict: false so trailing-slash routes (e.g. `/gifts/consume/`) match either form.
const app = new Hono<App>({ strict: false })
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

	// ---- Controllers ----------------------------------------------------------
	.route('/', configRoutes)
	.route('/', socialRoutes)
	.route('/', progressionRoutes)
	.route('/', avatarRoutes)
	.route('/', gameplayRoutes)
	.route('/', eventRoutes)
	.route('/', moderationRoutes)
	.route('/', inventoryRoutes)
	.route('/', roomRoutes)
	.route('/', imageRoutes)
	.route('/', accountRoutes)
	.route('/', adminRoutes)

// ---- ns-host service proxies ----------------------------------------------
// Older 2025patch.ini files point the game's ns host at THIS worker instead of
// the auth worker. The 2025 client talks to exactly one backend host
// (`ns.rec.net`, rewritten by 2025Patch) and its binary carries no other host
// literals, so service paths implemented by other workers MUST be reachable
// here too — otherwise the client gets 404s and renders empty screens (empty
// Rec Center storefront, empty "Choose Base Room" picker). These proxies
// These proxies call the owning workers directly through service bindings
// (see `services` in wrangler.jsonc): no HTTP edge routing is involved, so
// there is no Host-header or routing mismatch to go wrong.
function proxyTo(getService: (env: Env) => Fetcher) {
	return async (c: Context) => {
		const url = new URL(c.req.url)
		const target = `${url.pathname}${url.search}`
		// Forward only the headers the upstream needs.
		const headers = new Headers()
		for (const name of ['authorization', 'content-type', 'accept', 'accept-language']) {
			const value = c.req.header(name)
			if (value) headers.set(name, value)
		}
		const init: RequestInit = { method: c.req.method, headers }
		if (c.req.method !== 'GET' && c.req.method !== 'HEAD') {
			init.body = c.req.raw.body
			// Required by the Fetch spec when the body is a stream.
			;(init as Record<string, unknown>).duplex = 'half'
		}
		// The host is ignored by service bindings; only path+query route.
		const res = await getService(c.env).fetch(`https://proxy.internal${target}`, init)
		return new Response(res.body, { status: res.status, headers: res.headers })
	}
}

app.all('/api/storefronts/*', proxyTo((env) => env.ECON))
app.all('/rooms/*', proxyTo((env) => env.ROOMS))
app.all('/sections/*', proxyTo((env) => env.DISCOVERY))

// Service-discovery document with the real `fluxrec-*` hosts (see the auth
// worker for the full rationale).
app.get('/', (c) =>
	c.json(
		buildEndpoints(
			'ripo-ripoteam.workers.dev',
			JSON.stringify({ api: 'fluxrec-api', auth: 'fluxrec-auth', econ: 'fluxrec-econ' })
		)
	)
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
					title: 'Flux Rec api',
					version: '1.0.0',
					description: [
						'The catch-all Game API for Flux Rec, a private-server reimplementation of the Rec',
						'Room backend: everything the client calls that has not been split out into its own',
						'worker yet. Today that is config, the friend graph, inventions, saved photos,',
						'player events, reputation and the assorted sinks the client hits while loading.',
						'Relationships, inventions, images and player events are D1-backed; several',
						'endpoints are still stubs, noted per route.',
						'',
						'Expect this surface to shrink. Paths that also exist on a dedicated worker (avatar,',
						'equipment, consumables and objectives on `econ`) are already served there — the',
						'client calls that host and the copy here is a stub, which each route says.',
					].join('\n'),
				},
				servers: [{ url: 'https://api.ripo-ripoteam.workers.dev', description: 'Production' }],
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
