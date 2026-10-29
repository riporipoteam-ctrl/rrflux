import { describe, expect, it, vi } from 'vitest'
import { synthesizeRoomiePcm } from '../../roomie-voice'

const headers = { 'x-audio-format': 'pcm16le', 'x-audio-sample-rate': '24000', 'x-audio-channels': '1' }

describe('RipoBot PCM voice provider', () => {
	it('sends a bounded authenticated request and returns verified PCM', async () => {
		const fetcher = vi.fn(async () => new Response(new Uint8Array([1, 0, 2, 0]), { headers }))
		expect(await synthesizeRoomiePcm('Hello', 'secret', fetcher as unknown as typeof fetch)).toEqual(new Uint8Array([1, 0, 2, 0]))
		const [url, options] = fetcher.mock.calls[0] as unknown as [string, RequestInit]
		expect(url).toBe('https://echoxr-ripobot.hf.space/api/roomie/voice')
		expect((options.headers as Record<string, string>)['x-roomie-token']).toBe('secret')
		expect(options.redirect).toBe('error')
	})
	it('rejects wrong audio formats, malformed audio and oversized streams', async () => {
		for (const response of [
			new Response(new Uint8Array([0, 0]), { headers: { ...headers, 'x-audio-sample-rate': '44100' } }),
			new Response(new Uint8Array([0]), { headers }),
			new Response(new Uint8Array(1_440_002), { headers }),
		]) {
			expect(await synthesizeRoomiePcm('Hi', 'secret', (async () => response) as typeof fetch)).toBeNull()
		}
		expect(await synthesizeRoomiePcm('x'.repeat(501), 'secret')).toBeNull()
	})
})
