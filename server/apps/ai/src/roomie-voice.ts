/** Connect the private RipoBot voice endpoint to a caller expecting mono PCM16. */
export async function synthesizeRoomiePcm(
	text: string,
	token: string,
	voiceUrl: string,
	fetcher: typeof fetch = fetch
): Promise<Uint8Array | null> {
	if (!token || !voiceUrl || !text.trim() || text.length > 500) return null
	try {
		const endpoint = new URL(voiceUrl)
		if (endpoint.protocol !== 'https:') return null
		const response = await fetcher(endpoint, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', 'x-roomie-token': token },
			body: JSON.stringify({ text }),
			redirect: 'error',
			signal: AbortSignal.timeout(90_000),
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

/** Fetch the legacy MP3 voice without leaking failures as an empty Hono response. */
export async function synthesizeRoomieMp3(
	text: string,
	fetcher: typeof fetch = fetch
): Promise<Uint8Array | null> {
	const normalized = text.trim()
	if (!normalized) return null
	try {
		const endpoint = new URL('https://translate.google.com/translate_tts')
		endpoint.searchParams.set('ie', 'UTF-8')
		endpoint.searchParams.set('tl', 'en')
		endpoint.searchParams.set('client', 'tw-ob')
		endpoint.searchParams.set('q', normalized.slice(0, 200))
		const response = await fetcher(endpoint, {
			headers: { 'User-Agent': 'Mozilla/5.0' },
			redirect: 'error',
			signal: AbortSignal.timeout(30_000),
		})
		const contentType = response.headers.get('content-type') || ''
		const contentLength = Number(response.headers.get('content-length') || 0)
		const isMp3 = contentType.toLowerCase().split(';', 1)[0].trim() === 'audio/mpeg'
		if (!response.ok || !isMp3 || contentLength > 512_000) {
			return null
		}
		const audio = new Uint8Array(await response.arrayBuffer())
		return audio.length > 0 && audio.length <= 512_000 ? audio : null
	} catch {
		return null
	}
}
