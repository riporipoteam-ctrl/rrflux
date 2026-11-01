import { describe, expect, it, vi } from 'vitest'
import { synthesizeRoomieMp3, synthesizeRoomiePcm } from '../../roomie-voice'

const headers = { 'x-audio-format': 'pcm16le', 'x-audio-sample-rate': '24000', 'x-audio-channels': '1' }

describe('RipoBot PCM voice provider', () => {
	it('sends a bounded authenticated request and returns verified PCM', async () => {
		const fetcher = vi.fn(async () => new Response(new Uint8Array([1, 0, 2, 0]), { headers }))
		expect(await synthesizeRoomiePcm('Hello', 'secret', 'https://voice.example/api/roomie', fetcher as unknown as typeof fetch)).toEqual(new Uint8Array([1, 0, 2, 0]))
		const [url, options] = fetcher.mock.calls[0] as unknown as [string, RequestInit]
		expect(String(url)).toBe('https://voice.example/api/roomie')
		expect((options.headers as Record<string, string>)['x-roomie-token']).toBe('secret')
		expect(options.redirect).toBe('error')
	})
	it('rejects wrong audio formats, malformed audio and oversized streams', async () => {
		for (const response of [
			new Response(new Uint8Array([0, 0]), { headers: { ...headers, 'x-audio-sample-rate': '44100' } }),
			new Response(new Uint8Array([0]), { headers }),
			new Response(new Uint8Array(1_440_002), { headers }),
		]) {
			expect(await synthesizeRoomiePcm('Hi', 'secret', 'https://voice.example/api/roomie', (async () => response) as typeof fetch)).toBeNull()
		}
		expect(await synthesizeRoomiePcm('x'.repeat(501), 'secret', 'https://voice.example/api/roomie')).toBeNull()
		expect(await synthesizeRoomiePcm('Hi', 'secret', 'http://voice.example/api/roomie')).toBeNull()
	})
})

describe('Roomie legacy MP3 voice provider', () => {
	it('returns bounded audio and safely encodes the spoken text', async () => {
		const fetcher = vi.fn(async () => new Response(new Uint8Array([1, 2, 3]), {
			headers: { 'content-type': 'audio/mpeg' },
		}))
		expect(await synthesizeRoomieMp3('Hello Roomie', fetcher as unknown as typeof fetch)).toEqual(new Uint8Array([1, 2, 3]))
		const [url, options] = fetcher.mock.calls[0] as unknown as [URL, RequestInit]
		expect(url.hostname).toBe('translate.google.com')
		expect(url.searchParams.get('q')).toBe('Hello Roomie')
		expect(options.redirect).toBe('error')
	})

	it('returns null for provider errors, non-audio, empty, or oversized responses', async () => {
		for (const response of [
			new Response('unavailable', { status: 503, headers: { 'content-type': 'text/plain' } }),
			new Response('bad', { headers: { 'content-type': 'text/plain' } }),
			new Response(new Uint8Array(), { headers: { 'content-type': 'audio/mpeg' } }),
			new Response(new Uint8Array(512_001), { headers: { 'content-type': 'audio/mpeg' } }),
		]) {
			expect(await synthesizeRoomieMp3('Hello', (async () => response) as typeof fetch)).toBeNull()
		}
		expect(await synthesizeRoomieMp3('  ')).toBeNull()
	})
})
