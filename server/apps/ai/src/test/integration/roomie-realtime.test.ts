import { describe, expect, it, vi } from 'vitest'
import { RoomieRealtime } from '../../roomie-realtime'

describe('experimental Roomie realtime bridge', () => {
	it('passes committed PCM through transcription, chat and speech in order', async () => {
		const transcribe = vi.fn(async () => 'hello')
		const reply = vi.fn(async () => 'hi')
		const synthesize = vi.fn(async () => new Uint8Array([7, 0]))
		const session = new RoomieRealtime({ transcribe, reply, synthesize })
		expect(await session.handle({ type: 'input_audio_buffer.append', audio: btoa('\x01\x00') })).toEqual([])
		expect(await session.handle({ type: 'input_audio_buffer.commit' })).toEqual([{ type: 'input_audio_buffer.committed' }])
		expect(transcribe).toHaveBeenCalledWith(new Uint8Array([1, 0]))
		const events = await session.handle({ type: 'response.create' })
		expect(reply).toHaveBeenCalledWith([{ role: 'user', content: 'hello' }])
		expect(synthesize).toHaveBeenCalledWith('hi')
		expect(events).toEqual([{ type: 'response.audio.delta', delta: btoa('\x07\x00') }, { type: 'response.audio.done' }, { type: 'response.done' }])
	})
	it('rejects malformed data and does not send fabricated audio when a provider fails', async () => {
		const session = new RoomieRealtime({ transcribe: async () => 'hi', reply: async () => 'hi', synthesize: async () => null })
		expect(await session.handle({ type: 'input_audio_buffer.append', audio: '%%%' })).toMatchObject([{ type: 'error' }])
		expect(await session.handle({ type: 'input_audio_buffer.commit' })).toMatchObject([{ type: 'error' }])
		await session.handle({ type: 'input_audio_buffer.append', audio: btoa('\x01\x00') })
		await session.handle({ type: 'input_audio_buffer.commit' })
		expect(await session.handle({ type: 'response.create' })).toEqual([{ type: 'error', error: { code: 'voice_failed' } }])
	})
	it('does not emit a reply after cancellation during inference', async () => {
		let finish!: (value: string) => void
		const pending = new Promise<string>(resolve => { finish = resolve })
		const synthesize = vi.fn(async () => new Uint8Array([1, 0]))
		const session = new RoomieRealtime({ transcribe: async () => 'hi', reply: async () => pending, synthesize })
		await session.handle({ type: 'input_audio_buffer.append', audio: btoa('\x01\x00') })
		await session.handle({ type: 'input_audio_buffer.commit' })
		const result = session.handle({ type: 'response.create' })
		await session.handle({ type: 'response.cancel' })
		finish('late reply')
		expect(await result).toEqual([])
		expect(synthesize).not.toHaveBeenCalled()
	})
})
