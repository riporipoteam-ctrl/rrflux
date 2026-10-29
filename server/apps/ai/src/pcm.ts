/** Wrap mono 24 kHz signed 16-bit little-endian PCM in a WAV container. */
export function pcm16ToWav(pcm: Uint8Array, sampleRate = 24_000): Uint8Array {
	if (pcm.byteLength === 0 || pcm.byteLength % 2 !== 0 || pcm.byteLength > 4_800_000) {
		throw new RangeError('Invalid PCM16 audio length')
	}
	if (!Number.isInteger(sampleRate) || sampleRate < 8_000 || sampleRate > 48_000) {
		throw new RangeError('Invalid sample rate')
	}
	const result = new Uint8Array(44 + pcm.byteLength)
	const view = new DataView(result.buffer)
	const tag = (offset: number, text: string) => {
		for (let i = 0; i < text.length; i++) result[offset + i] = text.charCodeAt(i)
	}
	tag(0, 'RIFF')
	view.setUint32(4, 36 + pcm.byteLength, true)
	tag(8, 'WAVE')
	tag(12, 'fmt ')
	view.setUint32(16, 16, true)
	view.setUint16(20, 1, true)
	view.setUint16(22, 1, true)
	view.setUint32(24, sampleRate, true)
	view.setUint32(28, sampleRate * 2, true)
	view.setUint16(32, 2, true)
	view.setUint16(34, 16, true)
	tag(36, 'data')
	view.setUint32(40, pcm.byteLength, true)
	result.set(pcm, 44)
	return result
}
