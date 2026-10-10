import { Hono } from 'hono'
import { useWorkersLogger } from 'workers-tagged-logger'

import { withNotFound, withOnError } from '@repo/hono-helpers'

import { buildEndpoints } from './endpoints'

import type { Context } from 'hono'
import type { App } from './context'

/**
 * Name-server / service-discovery worker served at the apex domain.
 * Returns the endpoints document the game client fetches to discover every
 * service host. Hosts are derived from the `DOMAIN` var, which is injected at
 * deploy time (see `run-wrangler-deploy`) and defaults in `wrangler.jsonc` for
 * local dev.
 */
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

	// Endpoints document, derived from the deploy-time base domain.
	.get('/', (c) => c.json(buildEndpoints(c.env.DOMAIN, c.env.SUBDOMAINS)))

/**
 * The 2025 client's orientation flow looks rooms up on the ns host itself
 * (`https://ns.rec.net//rooms?name=SocialOrientation…`) rather than on the
 * Rooms service host — the real ns.rec.net answered those. Proxy them to the
 * Rooms service instead of 404ing, so "Go Now!" can resolve the next room.
 * The double slash is the client's own doing; collapse it before proxying.
 */
const proxyToRooms = (c: Context<App>) => {
	const roomsBase = buildEndpoints(c.env.DOMAIN, c.env.SUBDOMAINS)['Rooms']
	const url = new URL(c.req.url)
	const path = url.pathname.replace(/\/{2,}/g, '/')
	return fetch(new Request(`${roomsBase}${path}${url.search}`, c.req.raw))
}

app.all('/rooms', proxyToRooms)
app.all('/rooms/*', proxyToRooms)

// Hono matches `//rooms` literally, so catch the client's double-slash form here.
app.use('*', async (c, next) => {
	if (/^\/{2,}rooms(\/|$)/.test(new URL(c.req.url).pathname)) {
		return proxyToRooms(c)
	}
	await next()
})

export default app
