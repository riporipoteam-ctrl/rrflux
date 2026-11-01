import { describe, expect, it, vi } from 'vitest'
import { createRoomieProviders } from '../../roomie-providers'
import { RoomieRealtime } from '../../roomie-realtime'

describe('Roomie realtime provider wiring', () => {
	it('passes a PCM turn through the transcription, chat and voice provider contracts', async () => {
		const fetcher = vi.fn(async (url: string | URL, _init?: RequestInit) => {
			if (String(url).endsWith('/stt')) return Response.json({ text: 'Hello' })
			if (String(url).endsWith('/chat')) {
				return Response.json({ reply: 'Hi there' })
			}
			return new Response(new Uint8Array([4, 0]), { headers: {
				'x-audio-format': 'pcm16le', 'x-audio-sample-rate': '24000', 'x-audio-channels': '1',
			} })
		})
		const providers = createRoomieProviders({ serviceToken: 'test-roomie', chatUrl: 'https://voice.example/roomie/chat', voiceUrl: 'https://voice.example/roomie/tts', sttUrl: 'https://voice.example/roomie/stt', fetcher: fetcher as unknown as typeof fetch })
		const session = new RoomieRealtime(providers)
		await session.handle({ type: 'input_audio_buffer.append', audio: btoa('\x01\x00') })
		expect((await session.handle({ type: 'input_audio_buffer.commit' })).map(event => event.type)).toContain('input_audio_buffer.committed')
		const sttCall = fetcher.mock.calls.find(([url]) => String(url).endsWith('/stt'))
		expect(sttCall?.[1]?.headers).toMatchObject({ 'Content-Type': 'audio/wav', 'x-roomie-token': 'test-roomie' })
		expect(String.fromCharCode(...Array.from(sttCall?.[1]?.body as Uint8Array).slice(0, 4))).toBe('RIFF')
		const response = await session.handle({ type: 'response.create' })
		expect(response.map(event => event.type)).toContain('response.output_audio.delta')
		expect(response.at(-1)?.type).toBe('response.done')
		expect(fetcher).toHaveBeenCalledTimes(3)
	})
})
