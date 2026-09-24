import { describe, expect, test } from 'vitest'
import app from '../api.app'

/**
 * The app's global middleware reads `c.env.NAME` (workers-tagged-logger), so a
 * bare `app.request()` without env bindings 500s on EVERY route — a test-harness
 * artifact, not an endpoint bug. Pass a minimal env so this test exercises the
 * actual announcement handler.
 */
const TEST_ENV = { NAME: 'api', ENVIRONMENT: 'vitest' } as never

describe('announcement endpoint smoke test', () => {
	test('GET /api/announcement/v1/get returns 200 with announcements', async () => {
		const res = await app.request('/api/announcement/v1/get', {}, TEST_ENV)
		expect(res.status).toBe(200)
		const data = (await res.json()) as Array<{ ImageName?: string }>
		expect(Array.isArray(data)).toBe(true)
		expect(data.length).toBeGreaterThan(0)
		// Every banner must name a real image — a dangling ImageName makes the
		// client render the DefaultProfileImage.jpg fallback for the banner.
		for (const a of data) {
			expect(typeof a.ImageName).toBe('string')
			expect(a.ImageName!.length).toBeGreaterThan(0)
		}
	})
})
