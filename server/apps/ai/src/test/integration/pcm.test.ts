import { describe, expect, it } from 'vitest'
import { pcm16ToWav } from '../../pcm'

describe('PCM16 audio container', () => {
	it('produces a mono 24 kHz WAV with the original samples', () => {
		const pcm = new Uint8Array([0, 0, 255, 127, 0, 128])
		const wav = pcm16ToWav(pcm)
		const view = new DataView(wav.buffer)
		expect(new TextDecoder().decode(wav.subarray(0, 4))).toBe('RIFF')
		expect(view.getUint32(4, true)).toBe(36 + pcm.length)
		expect(view.getUint16(22, true)).toBe(1)
		expect(view.getUint32(24, true)).toBe(24_000)
		expect(view.getUint32(28, true)).toBe(48_000)
		expect(view.getUint16(34, true)).toBe(16)
		expect(view.getUint32(40, true)).toBe(pcm.length)
		expect(wav.subarray(44)).toEqual(pcm)
	})
	it('rejects malformed or unbounded audio', () => {
		expect(() => pcm16ToWav(new Uint8Array())).toThrow()
		expect(() => pcm16ToWav(new Uint8Array(3))).toThrow()
		expect(() => pcm16ToWav(new Uint8Array(4_800_002))).toThrow()
		expect(() => pcm16ToWav(new Uint8Array(2), 0)).toThrow()
	})
})
