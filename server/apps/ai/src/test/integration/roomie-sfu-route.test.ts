import { adminSecretsStore, env, SELF } from 'cloudflare:test'
import { beforeAll, describe, expect, it } from 'vitest'
import { createRoomieClientSecret } from '../../roomie-session'
import type { Env } from '../../context'

declare module 'cloudflare:test' {
	interface ProvidedEnv extends Env {}
}

beforeAll(async () => {
	await adminSecretsStore(env.JWT_SECRET).create('test-roomie-signing-key')
})

describe('2026 Roomie SDP route', () => {
	it('requires a Roomie session credential', async () => {
		const response = await SELF.fetch('https://example.com/v1/realtime/calls', { method: 'POST', body: 'v=0' })
		expect(response.status).toBe(401)
	})
	it('fails closed when the SFU is not configured', async () => {
		const token = await createRoomieClientSecret('test-roomie-signing-key', 42, 'route-test')
		const response = await SELF.fetch('https://example.com/v1/realtime/calls', {
			method: 'POST', headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/sdp' }, body: 'v=0',
		})
		expect(response.status).toBe(503)
	})
	it('accepts a signed Roomie credential in Unity form-encoded WebRTC requests', async () => {
		const token = await createRoomieClientSecret('test-roomie-signing-key', 42, 'form-route-test')
		const body = new URLSearchParams({ sdp: 'v=0', client_secret: token })
		const response = await SELF.fetch('https://example.com/v1/realtime/calls', {
			method: 'POST', headers: { 'Content-Type': 'application/x-www-form-urlencoded' }, body,
		})
		expect(response.status).toBe(503)
	})
})
