import { describe, expect, it } from 'vitest'
import { createRoomieSocket } from '../../roomie-socket'

describe('isolated Roomie WebSocket', () => {
	it('delivers a PCM voice turn and handles malformed events', async () => {
		const response = createRoomieSocket({
			transcribe: async () => 'hello', reply: async () => 'hi',
			synthesize: async () => new Uint8Array([1, 0]),
		})
		expect(response.status).toBe(101)
		const socket = response.webSocket!
		socket.accept()
		const next = () => new Promise<Record<string, unknown>>(resolve => {
			socket.addEventListener('message', e => resolve(JSON.parse(e.data as string)), { once: true })
		})
		let message = next()
		socket.send('{bad json')
		expect(await message).toMatchObject({ type: 'error', error: { code: 'invalid_event' } })
		message = next()
		socket.send(JSON.stringify({ type: 'input_audio_buffer.append', audio: btoa('\x01\x00') }))
		socket.send(JSON.stringify({ type: 'input_audio_buffer.commit' }))
		expect(await message).toMatchObject({ type: 'input_audio_buffer.committed' })
		const results: Record<string, unknown>[] = []
		const done = new Promise<void>(resolve => socket.addEventListener('message', e => {
			const data = JSON.parse(e.data as string) as Record<string, unknown>
			results.push(data)
			if (data.type === 'response.done') resolve()
		}))
		socket.send(JSON.stringify({ type: 'response.create' }))
		await done
		expect(results).toEqual([
			{ type: 'response.audio.delta', delta: btoa('\x01\x00') },
			{ type: 'response.audio.done' }, { type: 'response.done' },
		])
		socket.close(1000, 'test complete')
	})
})
