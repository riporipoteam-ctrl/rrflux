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
	it('runs an authenticated development audio turn through the provider interfaces', async () => {
		const fetcher = (async (url: string) => {
			if (url.includes('router.huggingface.co')) return Response.json({ choices: [{ message: { content: 'Hi' } }] })
			return new Response(new Uint8Array([2, 0]), { headers: {
				'x-audio-format': 'pcm16le', 'x-audio-sample-rate': '24000', 'x-audio-channels': '1',
			} })
		}) as typeof fetch
		const response = roomieDevSocket(request(), {
			...config, HF_TOKEN: 'test', ROOMIE_VOICE_TOKEN: 'test', AI: { run: async () => ({ text: 'hello' }) },
		}, fetcher)
		const socket = response.webSocket!
		socket.accept()
		const next = () => new Promise<Record<string, unknown>>(resolve => socket.addEventListener(
			'message', e => resolve(JSON.parse(e.data as string)), { once: true },
		))
		let received = next()
		socket.send(JSON.stringify({ type: 'input_audio_buffer.append', audio: btoa('\x01\x00') }))
		socket.send(JSON.stringify({ type: 'input_audio_buffer.commit' }))
		expect(await received).toEqual({ type: 'input_audio_buffer.committed' })
		received = next()
		socket.send(JSON.stringify({ type: 'response.create' }))
		expect(await received).toEqual({ type: 'response.audio.delta', delta: btoa('\x02\x00') })
		socket.close(1000, 'test complete')
	})
})
