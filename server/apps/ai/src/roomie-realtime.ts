/** OpenAI-Realtime-wire-compatible event adapter backed only by RipoBot + Kokoro. */
export type RoomieMessage = { role: 'system' | 'user' | 'assistant'; content: string }
export type RoomieEvent = { type: string; [key: string]: unknown }
export interface RoomieProviders {
	transcribe(pcm: Uint8Array): Promise<string | null>
	reply(messages: RoomieMessage[]): Promise<string | null>
	synthesize(text: string): Promise<Uint8Array | null>
}

export type RoomieRealtimeSession = {
	id: string
	model?: string
	instructions?: string
	voice?: string
}

const MAX_INPUT_BYTES = 1_440_000
const MAX_OUTPUT_BYTES = 1_440_000
const MAX_HISTORY = 12
const AUDIO_CHUNK_BYTES = 24_000
const ROOMIE_PROMPT =
	'You are Roomie, a friendly AI pet companion in Flux Rec. Be cheerful, playful, helpful, and concise. ' +
	'Keep replies suitable for a social game with younger players. Do not ask for passwords, contact details, ' +
	'or other sensitive personal information. Do not claim to see or hear anything unless the player shared it.'

function eventId(): string {
	return `event_${crypto.randomUUID().replaceAll('-', '')}`
}

function b64(bytes: Uint8Array): string {
	let binary = ''
	for (const byte of bytes) binary += String.fromCharCode(byte)
	return btoa(binary)
}

function b64Bytes(encoded: string): Uint8Array {
	const binary = atob(encoded)
	const bytes = new Uint8Array(binary.length)
	for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
	return bytes
}

/** One instance belongs to exactly one authenticated realtime connection. */
export class RoomieRealtime {
	private chunks: Uint8Array[] = []
	private size = 0
	private messages: RoomieMessage[] = []
	private busy = false
	private generation = 0
	private instructions = ROOMIE_PROMPT
	private voice = 'af_heart'
	private readonly sessionId: string
	private readonly model: string

	constructor(
		private readonly providers: RoomieProviders,
		session: RoomieRealtimeSession = { id: crypto.randomUUID() }
	) {
		this.sessionId = session.id
		this.model = session.model || 'ripo-roomie-realtime'
		this.instructions = `${ROOMIE_PROMPT}${session.instructions?.trim() ? `\nPlayer preferences: ${session.instructions.trim().slice(0, 1500)}` : ''}`
		this.voice = session.voice || 'af_heart'
	}

	created(): RoomieEvent {
		return {
			type: 'session.created',
			event_id: eventId(),
			session: this.snapshot(),
		}
	}

	private snapshot(): Record<string, unknown> {
		return {
			id: this.sessionId,
			object: 'realtime.session',
			model: this.model,
			modalities: ['audio', 'text'],
			instructions: this.instructions,
			voice: this.voice,
			input_audio_format: 'pcm16',
			output_audio_format: 'pcm16',
			input_audio_transcription: { model: 'whisper-1' },
			turn_detection: null,
			tools: [],
			tool_choice: 'auto',
		}
	}

	private error(code: string, message = 'Roomie could not process that event.'): RoomieEvent[] {
		return [{ type: 'error', event_id: eventId(), error: { type: 'invalid_request_error', code, message } }]
	}

	async handle(event: RoomieEvent): Promise<RoomieEvent[]> {
		switch (event.type) {
			case 'session.update': {
				const requested = event.session
				if (requested && typeof requested === 'object') {
					const config = requested as Record<string, unknown>
					if (typeof config.instructions === 'string' && config.instructions.trim()) {
						this.instructions = `${ROOMIE_PROMPT}\nPlayer preferences: ${config.instructions.trim().slice(0, 1500)}`
					}
				}
				return [{ type: 'session.updated', event_id: eventId(), session: this.snapshot() }]
			}
			case 'input_audio_buffer.clear':
				this.chunks = []
				this.size = 0
				return [{ type: 'input_audio_buffer.cleared', event_id: eventId() }]
			case 'input_audio_buffer.append': {
				if (this.busy || typeof event.audio !== 'string' || event.audio.length > 1_920_000 ||
					!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(event.audio)) {
					return this.error('invalid_audio', 'Audio chunk is malformed or the session is busy.')
				}
				let raw: Uint8Array
				try { raw = b64Bytes(event.audio) } catch { return this.error('invalid_audio') }
				if (!raw.length || raw.length % 2 || raw.length + this.size > MAX_INPUT_BYTES) {
					return this.error('invalid_audio_length', 'Audio must be mono PCM16 and no longer than 30 seconds.')
				}
				this.chunks.push(raw)
				this.size += raw.length
				return []
			}
			case 'input_audio_buffer.commit': {
				if (this.busy || !this.size) return this.error('empty_or_busy', 'There is no committed microphone audio to transcribe.')
				this.busy = true
				const audio = new Uint8Array(this.size)
				let offset = 0
				for (const chunk of this.chunks) { audio.set(chunk, offset); offset += chunk.length }
				this.chunks = []
				this.size = 0
				try {
					const text = await this.providers.transcribe(audio)
					if (!text?.trim()) return this.error('transcription_failed', 'Roomie could not understand that audio. Please try again.')
					const transcript = text.trim().slice(0, 2000)
					this.messages.push({ role: 'user', content: transcript })
					this.messages = this.messages.slice(-MAX_HISTORY)
					const itemId = `item_${crypto.randomUUID().replaceAll('-', '')}`
					return [
						{ type: 'input_audio_buffer.committed', event_id: eventId(), item_id: itemId },
						{ type: 'conversation.item.created', event_id: eventId(), previous_item_id: null,
							item: { id: itemId, object: 'realtime.item', type: 'message', role: 'user', status: 'completed',
								content: [{ type: 'input_audio', transcript }] } },
						{ type: 'conversation.item.input_audio_transcription.completed', event_id: eventId(), item_id: itemId,
							content_index: 0, transcript },
					]
				} catch {
					return this.error('transcription_failed', 'Roomie speech recognition is unavailable right now.')
				} finally { this.busy = false }
			}
			case 'conversation.item.create': {
				if (this.busy || !event.item || typeof event.item !== 'object') return this.error('invalid_item')
				const item = event.item as { role?: unknown; content?: unknown }
				if (item.role !== 'user' || !Array.isArray(item.content)) return this.error('invalid_item')
				const text = item.content.flatMap((part) => {
					if (!part || typeof part !== 'object') return []
					const value = part as { type?: unknown; text?: unknown }
					return value.type === 'input_text' && typeof value.text === 'string' ? [value.text] : []
				}).join(' ').trim().slice(0, 2000)
				if (!text) return this.error('empty_message')
				this.messages.push({ role: 'user', content: text })
				this.messages = this.messages.slice(-MAX_HISTORY)
				return [{ type: 'conversation.item.created', event_id: eventId(), previous_item_id: null,
					item: { id: `item_${crypto.randomUUID().replaceAll('-', '')}`, object: 'realtime.item', type: 'message', role: 'user', status: 'completed', content: [{ type: 'input_text', text }] } }]
			}
			case 'response.cancel':
				this.generation++
				return [{ type: 'response.cancelled', event_id: eventId() }]
			case 'response.create': {
				if (this.busy || !this.messages.length) return this.error('empty_or_busy', 'Speak or send a message before asking Roomie to respond.')
				this.busy = true
				const generation = this.generation
				const responseId = `resp_${crypto.randomUUID().replaceAll('-', '')}`
				const itemId = `item_${crypto.randomUUID().replaceAll('-', '')}`
				try {
					const reply = await this.providers.reply([
						{ role: 'system', content: this.instructions },
						...this.messages,
					])
					if (generation !== this.generation) return []
					if (!reply?.trim()) return this.error('reply_failed', 'Roomie could not generate a reply right now.')
					const text = reply.trim().slice(0, 500)
					const pcm = await this.providers.synthesize(text)
					if (generation !== this.generation) return []
					if (!pcm?.length || pcm.length % 2 || pcm.length > MAX_OUTPUT_BYTES) {
						return this.error('voice_failed', 'Roomie voice is unavailable right now.')
					}
					this.messages.push({ role: 'assistant', content: text })
					this.messages = this.messages.slice(-MAX_HISTORY)
					const outputItem = { id: itemId, object: 'realtime.item', type: 'message', role: 'assistant', status: 'completed',
						content: [{ type: 'audio', transcript: text }] }
					const output: RoomieEvent[] = [
						{ type: 'response.created', event_id: eventId(), response: { id: responseId, object: 'realtime.response', status: 'in_progress', status_details: null, output: [] } },
						{ type: 'response.output_item.added', event_id: eventId(), response_id: responseId, output_index: 0, item: { ...outputItem, status: 'in_progress', content: [] } },
						{ type: 'response.content_part.added', event_id: eventId(), response_id: responseId, item_id: itemId, output_index: 0, content_index: 0, part: { type: 'audio', transcript: '' } },
						{ type: 'response.output_audio_transcript.delta', event_id: eventId(), response_id: responseId, item_id: itemId, output_index: 0, content_index: 0, delta: text },
					]
					for (let offset = 0; offset < pcm.length; offset += AUDIO_CHUNK_BYTES) {
						if (generation !== this.generation) return []
						output.push({ type: 'response.output_audio.delta', event_id: eventId(), response_id: responseId,
							item_id: itemId, output_index: 0, content_index: 0, delta: b64(pcm.subarray(offset, Math.min(offset + AUDIO_CHUNK_BYTES, pcm.length))) })
					}
					output.push(
						{ type: 'response.output_audio_transcript.done', event_id: eventId(), response_id: responseId, item_id: itemId, output_index: 0, content_index: 0, transcript: text },
						{ type: 'response.output_audio.done', event_id: eventId(), response_id: responseId, item_id: itemId, output_index: 0, content_index: 0 },
						{ type: 'response.content_part.done', event_id: eventId(), response_id: responseId, item_id: itemId, output_index: 0, content_index: 0, part: { type: 'audio', transcript: text } },
						{ type: 'response.output_item.done', event_id: eventId(), response_id: responseId, output_index: 0, item: outputItem },
						{ type: 'response.done', event_id: eventId(), response: { id: responseId, object: 'realtime.response', status: 'completed', status_details: null, output: [outputItem] } },
					)
					return output
				} catch {
					return this.error('response_failed', 'Roomie is temporarily unavailable. Please try again.')
				} finally { this.busy = false }
			}
			default:
				return this.error('unsupported_event', `Unsupported realtime event: ${event.type}`)
		}
	}
}
