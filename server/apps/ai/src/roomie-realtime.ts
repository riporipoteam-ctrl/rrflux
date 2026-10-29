/** Experimental per-connection Roomie event bridge. Not exposed as a route. */
export type RoomieMessage = { role: 'user' | 'assistant'; content: string }
export type RoomieEvent = { type: string; [key: string]: unknown }
export interface RoomieProviders {
	transcribe(pcm: Uint8Array): Promise<string | null>
	reply(messages: RoomieMessage[]): Promise<string | null>
	synthesize(text: string): Promise<Uint8Array | null>
}

const MAX_INPUT_BYTES = 1_440_000
const MAX_HISTORY = 12

/** One instance belongs to exactly one authenticated connection. */
export class RoomieRealtime {
	private chunks: Uint8Array[] = []
	private size = 0
	private messages: RoomieMessage[] = []
	private busy = false
	private generation = 0

	constructor(private readonly providers: RoomieProviders) {}

	private error(code: string): RoomieEvent[] {
		return [{ type: 'error', error: { code } }]
	}

	async handle(event: RoomieEvent): Promise<RoomieEvent[]> {
		switch (event.type) {
			case 'session.update':
				return [{ type: 'session.updated', session: { input_audio_format: 'pcm16', output_audio_format: 'pcm16' } }]
			case 'input_audio_buffer.clear':
				this.chunks = []; this.size = 0
				return [{ type: 'input_audio_buffer.cleared' }]
			case 'input_audio_buffer.append': {
				if (this.busy || typeof event.audio !== 'string' || event.audio.length > 1_920_000 ||
					!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(event.audio)) return this.error('invalid_audio')
				let raw: string
				try { raw = atob(event.audio) } catch { return this.error('invalid_audio') }
				if (!raw.length || raw.length % 2 || raw.length + this.size > MAX_INPUT_BYTES) return this.error('invalid_audio')
				const chunk = new Uint8Array(raw.length)
				for (let i = 0; i < raw.length; i++) chunk[i] = raw.charCodeAt(i)
				this.chunks.push(chunk); this.size += chunk.length
				return []
			}
			case 'input_audio_buffer.commit': {
				if (this.busy || !this.size) return this.error('empty_or_busy')
				this.busy = true
				const audio = new Uint8Array(this.size)
				let offset = 0
				for (const chunk of this.chunks) { audio.set(chunk, offset); offset += chunk.length }
				this.chunks = []; this.size = 0
				try {
					const text = await this.providers.transcribe(audio)
					if (!text?.trim()) return this.error('transcription_failed')
					this.messages.push({ role: 'user', content: text.trim() })
					this.messages = this.messages.slice(-MAX_HISTORY)
					return [{ type: 'input_audio_buffer.committed' }]
				} catch { return this.error('transcription_failed') } finally { this.busy = false }
			}
			case 'response.cancel':
				this.generation++
				return [{ type: 'response.cancelled' }]
			case 'response.create': {
				if (this.busy || !this.messages.length) return this.error('empty_or_busy')
				this.busy = true
				const generation = this.generation
				try {
					const reply = await this.providers.reply([...this.messages])
					if (generation !== this.generation) return []
					if (!reply?.trim()) return this.error('reply_failed')
					const pcm = await this.providers.synthesize(reply)
					if (generation !== this.generation) return []
					if (!pcm?.length || pcm.length % 2 || pcm.length > MAX_INPUT_BYTES) return this.error('voice_failed')
					this.messages.push({ role: 'assistant', content: reply })
					this.messages = this.messages.slice(-MAX_HISTORY)
					let raw = ''
					for (const byte of pcm) raw += String.fromCharCode(byte)
					return [{ type: 'response.audio.delta', delta: btoa(raw) }, { type: 'response.audio.done' }, { type: 'response.done' }]
				} catch { return this.error('response_failed') } finally { this.busy = false }
			}
			default:
				return this.error('unsupported_event')
		}
	}
}
