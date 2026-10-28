import { describe, expect, it } from 'vitest'
import { roomieDevSocket } from '../../roomie-dev'

describe('restricted Roomie development handshake', () => {
	const request = (key = 'key', upgrade = 'websocket') => new Request('https://example.test/', {
		headers: { 'x-roomie-dev-key': key, upgrade },
	})
	const config = { ENVIRONMENT: 'development', ROOMIE_REALTIME_DEV_ENABLED: 'true', ROOMIE_REALTIME_DEV_KEY: 'key' }
	it('cannot be enabled in production or staging', () => {
		for (const ENVIRONMENT of ['production', 'staging']) {
			expect(roomieDevSocket(request(), { ...config, ENVIRONMENT }).status).toBe(404)
		}
		expect(roomieDevSocket(request(), { ENVIRONMENT: 'development' }).status).toBe(404)
	})
	it('requires the separate dev key, websocket upgrade and configured providers', () => {
		expect(roomieDevSocket(request('wrong'), config).status).toBe(401)
		expect(roomieDevSocket(request('key', 'http'), config).status).toBe(426)
		expect(roomieDevSocket(request(), config).status).toBe(503)
	})
	it('opens an isolated socket only with all required development configuration', () => {
		const response = roomieDevSocket(request(), {
			...config, HF_TOKEN: 'test', ROOMIE_VOICE_TOKEN: 'test', AI: { run: async () => ({ text: 'test' }) },
		})
		expect(response.status).toBe(101)
		response.webSocket?.accept()
		response.webSocket?.close(1000, 'test complete')
	})
})
