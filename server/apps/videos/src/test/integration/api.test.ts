import { createExecutionContext, env, waitOnExecutionContext } from 'cloudflare:test'
import { describe, expect, it } from 'vitest'

import app from '../../videos.app'

import type { Env } from '../../context'

declare module 'cloudflare:test' {
	interface ProvidedEnv extends Env {}
}

const ORIGIN = 'https://example.com'

async function fetchApp(url: string, init?: RequestInit): Promise<Response> {
	const ctx = createExecutionContext()
	const res = await app.fetch(new Request(url, init), env, ctx)
	await waitOnExecutionContext(ctx)
	return res
}

describe('videos worker', () => {
	it('GET / returns the liveness probe', async () => {
		const res = await fetchApp(`${ORIGIN}/`)
		expect(res.status).toBe(200)
		expect(await res.json()).toEqual({ service: 'videos', status: 'ok' })
	})

	it('GET /api/videos lists the built-in catalog', async () => {
		const res = await fetchApp(`${ORIGIN}/api/videos`)
		expect(res.status).toBe(200)
		const body = (await res.json()) as { videos: { id: string; url: string }[] }
		expect(body.videos.length).toBeGreaterThan(0)
		for (const v of body.videos) {
			expect(v.url).toMatch(/^https:\/\//)
		}
	})

	it('GET /api/videos/:id resolves a known id', async () => {
		const res = await fetchApp(`${ORIGIN}/api/videos/big-buck-bunny`)
		expect(res.status).toBe(200)
		const body = (await res.json()) as { id: string; url: string }
		expect(body.id).toBe('big-buck-bunny')
		expect(body.url).toContain('.mp4')
	})

	it('GET /api/videos/:id 404s on an unknown id', async () => {
		const res = await fetchApp(`${ORIGIN}/api/videos/no-such-video`)
		expect(res.status).toBe(404)
	})

	it('GET /api/videos/:id/stream 302s to the MP4', async () => {
		const res = await fetchApp(`${ORIGIN}/api/videos/big-buck-bunny/stream`, {
			redirect: 'manual',
		})
		expect(res.status).toBe(302)
		expect(res.headers.get('location')).toContain('.mp4')
	})

	it('GET /api/videos/lookup extracts a YouTube id from a watch URL', async () => {
		const ctx = createExecutionContext()
		const testEnv = {
			...env,
			VIDEOS_MAP: JSON.stringify({
				dQw4w9WgXcQ: 'https://cdn.example.com/v/never-gonna.mp4',
			}),
		}
		const res = await app.fetch(
			new Request(`${ORIGIN}/api/videos/lookup?url=${encodeURIComponent('https://www.youtube.com/watch?v=dQw4w9WgXcQ')}`),
			testEnv,
			ctx
		)
		await waitOnExecutionContext(ctx)
		expect(res.status).toBe(200)
		const body = (await res.json()) as { video: { id: string; url: string } | null }
		expect(body.video).not.toBeNull()
		expect(body.video!.id).toBe('dQw4w9WgXcQ')
		expect(body.video!.url).toBe('https://cdn.example.com/v/never-gonna.mp4')
	})

	it('VIDEOS_MAP overrides the built-in catalog', async () => {
		const ctx = createExecutionContext()
		const testEnv = {
			...env,
			VIDEOS_MAP: JSON.stringify({
				'big-buck-bunny': 'https://cdn.example.com/v/custom.mp4',
			}),
		}
		const res = await app.fetch(new Request(`${ORIGIN}/api/videos/big-buck-bunny`), testEnv, ctx)
		await waitOnExecutionContext(ctx)
		expect(res.status).toBe(200)
		const body = (await res.json()) as { url: string }
		expect(body.url).toBe('https://cdn.example.com/v/custom.mp4')
	})
})
