import { pcm16ToWav } from './pcm'
import { synthesizeRoomiePcm } from './roomie-voice'
import type { RoomieProviders } from './roomie-realtime'

/** Construct providers for the isolated bridge; does not create a public session. */
export function createRoomieProviders(options: {
	serviceToken: string
	chatUrl: string
	voiceUrl: string
	sttUrl: string
	fetcher?: typeof fetch
}): RoomieProviders {
	const fetcher = options.fetcher || fetch
	return {
		async transcribe(pcm) {
			try {
				const wav = pcm16ToWav(pcm)
				return await transcribeRoomieAudio(wav, options.serviceToken, options.sttUrl, fetcher)
			} catch { return null }
		},
		async reply(messages) {
			if (!options.serviceToken || messages.length > 13) return null
			try {
				const endpoint = new URL(options.chatUrl)
				if (endpoint.protocol !== 'https:') return null
				const response = await fetcher(endpoint, {
					method: 'POST',
					headers: {
						'Content-Type': 'application/json',
						'x-roomie-token': options.serviceToken,
					},
					body: JSON.stringify({ messages }),
					redirect: 'error',
					signal: AbortSignal.timeout(90_000),
				})
				if (!response.ok) return null
				const data = await response.json() as { reply?: unknown }
				return typeof data.reply === 'string' ? data.reply.trim().slice(0, 500) || null : null
			} catch {
				return null
			}
		},
		synthesize(text) {
			return synthesizeRoomiePcm(text, options.serviceToken, options.voiceUrl, fetcher)
		},
	}
}

/** Send already containerized audio to the private RipoBot STT endpoint. */
export async function transcribeRoomieAudio(
	audio: Uint8Array,
	token: string,
	sttUrl: string,
	fetcher: typeof fetch = fetch
): Promise<string | null> {
	if (!audio.byteLength || audio.byteLength > 1_500_000 || !token || !sttUrl) return null
	try {
		const endpoint = new URL(sttUrl)
		if (endpoint.protocol !== 'https:') return null
		const response = await fetcher(endpoint, {
			method: 'POST',
			headers: { 'Content-Type': 'audio/wav', 'x-roomie-token': token },
			body: audio as BodyInit,
			redirect: 'error',
			signal: AbortSignal.timeout(120_000),
		})
		if (!response.ok) return null
		const data = await response.json() as { text?: unknown }
		return typeof data.text === 'string' ? data.text.trim().slice(0, 2_000) || null : null
	} catch {
		return null
	}
}
