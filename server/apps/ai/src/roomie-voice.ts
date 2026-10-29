/** Connect the private RipoBot voice endpoint to a caller expecting mono PCM16. */
export async function synthesizeRoomiePcm(
	text: string,
	token: string,
	fetcher: typeof fetch = fetch
): Promise<Uint8Array | null> {
	if (!token || !text.trim() || text.length > 500) return null
	try {
		const response = await fetcher('https://echoxr-ripobot.hf.space/api/roomie/voice', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', 'x-roomie-token': token },
			body: JSON.stringify({ text }),
			redirect: 'error',
			signal: AbortSignal.timeout(30_000),
		})
		if (!response.ok || response.headers.get('x-audio-format') !== 'pcm16le' ||
			response.headers.get('x-audio-sample-rate') !== '24000' ||
			response.headers.get('x-audio-channels') !== '1') return null
		// Bound the stream even when the peer omits Content-Length.
		if (!response.body) return null
		const reader = response.body.getReader()
		const chunks: Uint8Array[] = []
		let size = 0
		while (true) {
			const { done, value } = await reader.read()
			if (done) break
			size += value.byteLength
			if (size > 1_440_000) { await reader.cancel(); return null }
			chunks.push(value)
		}
		if (!size || size % 2) return null
		const audio = new Uint8Array(size)
		let offset = 0
		for (const chunk of chunks) { audio.set(chunk, offset); offset += chunk.byteLength }
		return audio
	} catch { return null }
}
