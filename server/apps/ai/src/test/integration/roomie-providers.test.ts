import { describe, expect, it, vi } from 'vitest'
import { createRoomieProviders } from '../../roomie-providers'
import { RoomieRealtime } from '../../roomie-realtime'

describe('Roomie realtime provider wiring', () => {
	it('passes a PCM turn through the transcription, chat and voice provider contracts', async () => {
		const run = vi.fn(async () => ({ text: 'Hello' }))
		const fetcher = vi.fn(async (url: string) => {
			if (url.startsWith('https://router.huggingface.co/')) {
				return Response.json({ choices: [{ message: { content: 'Hi there' } }] })
			}
			return new Response(new Uint8Array([4, 0]), { headers: {
				'x-audio-format': 'pcm16le', 'x-audio-sample-rate': '24000', 'x-audio-channels': '1',
			} })
		})
		const providers = createRoomieProviders({ ai: { run }, hfToken: 'test-hf', voiceToken: 'test-voice', fetcher: fetcher as unknown as typeof fetch })
		const session = new RoomieRealtime(providers)
		await session.handle({ type: 'input_audio_buffer.append', audio: btoa('\x01\x00') })
		expect(await session.handle({ type: 'input_audio_buffer.commit' })).toEqual([{ type: 'input_audio_buffer.committed' }])
		const [model, input] = run.mock.calls[0] as unknown as [string, { audio: number[] }]
		expect(model).toBe('@cf/openai/whisper')
		expect(String.fromCharCode(...input.audio.slice(0, 4))).toBe('RIFF')
		expect(await session.handle({ type: 'response.create' })).toEqual([
			{ type: 'response.audio.delta', delta: btoa('\x04\x00') },
			{ type: 'response.audio.done' }, { type: 'response.done' },
		])
		expect(fetcher).toHaveBeenCalledTimes(2)
	})
})
