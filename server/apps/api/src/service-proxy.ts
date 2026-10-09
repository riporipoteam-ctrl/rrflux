import type { Context } from 'hono'

import type { App, Env } from './context'

/** Forward a request to the worker that owns its service path. */
export function proxyTo(getService: (env: Env) => Fetcher) {
	return async (c: Context<App>) => {
		const url = new URL(c.req.url)
		const target = `${url.pathname}${url.search}`
		const headers = new Headers()
		for (const name of ['authorization', 'content-type', 'accept', 'accept-language']) {
			const value = c.req.header(name)
			if (value) headers.set(name, value)
		}
		const init: RequestInit = { method: c.req.method, headers }
		if (c.req.method !== 'GET' && c.req.method !== 'HEAD') {
			init.body = c.req.raw.body
			;(init as Record<string, unknown>).duplex = 'half'
		}
		// Service bindings route by worker name; the URL host is intentionally internal.
		const response = await getService(c.env).fetch(`https://proxy.internal${target}`, init)
		return new Response(response.body, { status: response.status, headers: response.headers })
	}
}
