import { pcm16ToWav } from './pcm'
import { replyFromRipoBot } from './ripobot'
import { synthesizeRoomiePcm } from './roomie-voice'
import type { RoomieProviders } from './roomie-realtime'

/** Construct providers for the isolated bridge; does not create a public session. */
export function createRoomieProviders(options: {
	ai: { run(model: string, input: { audio: number[] }): Promise<{ text?: string }> }
	hfToken: string
	voiceToken: string
	fetcher?: typeof fetch
}): RoomieProviders {
	return {
		async transcribe(pcm) {
			try {
				const wav = pcm16ToWav(pcm)
				const result = await options.ai.run('@cf/openai/whisper', { audio: Array.from(wav) })
				return result.text?.trim() || null
			} catch { return null }
		},
		reply(messages) {
			return replyFromRipoBot(options.hfToken, messages, options.fetcher)
		},
		synthesize(text) {
			return synthesizeRoomiePcm(text, options.voiceToken, options.fetcher)
		},
	}
}
