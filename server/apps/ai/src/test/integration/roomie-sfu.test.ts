import { afterEach, describe, expect, it, vi } from 'vitest'
import { createRoomieSfuAdapters, createRoomieSfuSession, requestRoomieVoiceTrack } from '../../roomie-sfu'
import { monoToSfuAudio, sfuAudioPackets, sfuAudioToMono, sfuPacketPayload } from '../../roomie-sfu-audio'

afterEach(() => vi.unstubAllGlobals())

describe('Roomie SFU audio and adapters', () => {
	it('requests a bidirectional audio transceiver for the client offer', async () => {
		const fetcher = vi.fn(async (url: string | URL, _init?: RequestInit) => String(url).endsWith('/sessions/new')
			? Response.json({ sessionId: 'session' })
			: Response.json({ tracks: [{ trackName: 'roomie-player-mic' }], sessionDescription: { type: 'answer', sdp: 'v=0' } }))
		vi.stubGlobal('fetch', fetcher)
		const offer = 'v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=sendrecv\r\n'
		expect(await createRoomieSfuSession('a'.repeat(32), 'secret', offer)).toEqual({ sessionId: 'session', answer: 'v=0' })
		const request = JSON.parse(String(fetcher.mock.calls[1][1]?.body)) as { tracks: Array<{ bidirectionalMediaStream?: boolean }> }
		expect(request.tracks[0].bidirectionalMediaStream).toBe(true)
	})
	it('converts audio and preserves protobuf packet payloads', () => {
		const stereo = new Uint8Array([1, 0, 1, 0, 3, 0, 3, 0])
		expect([...sfuAudioToMono(stereo)]).toEqual([2, 0])
		const output = monoToSfuAudio(new Uint8Array([2, 0]))
		expect([...sfuPacketPayload(sfuAudioPackets(output)[0])]).toEqual([...output])
		expect(() => sfuPacketPayload(new Uint8Array([42, 5, 1]))).toThrow()
	})

	it('matches adapter results by track and requests a voice offer', async () => {
		const fetcher = vi.fn(async (url: string | URL) => String(url).includes('/adapters/')
			? Response.json({ tracks: [
				{ trackName: 'roomie-generated-voice', adapterId: 'output', sessionId: 'voice' },
				{ trackName: 'roomie-player-mic', adapterId: 'input' },
			] })
			: Response.json({ tracks: [{}], requiresImmediateRenegotiation: false }))
		vi.stubGlobal('fetch', fetcher)
		const adapters = await createRoomieSfuAdapters('a'.repeat(32), 'secret', 'session', 'https://voice.example', 'token')
		expect(adapters.adapterIds).toEqual(['input', 'output'])
		expect(await requestRoomieVoiceTrack('a'.repeat(32), 'secret', 'session', adapters.voiceSessionId, adapters.voiceTrackName)).toEqual({ attached: true })
	})

	it('closes the successful adapter when a batch partially fails', async () => {
		const fetcher = vi.fn(async (url: string | URL) => String(url).endsWith('/close')
			? Response.json({ tracks: [{ adapterId: 'input' }] })
			: Response.json({ tracks: [{ adapterId: 'input', trackName: 'roomie-player-mic' }, { errorCode: 'websocket_handshake_failed' }] }))
		vi.stubGlobal('fetch', fetcher)
		await expect(createRoomieSfuAdapters('a'.repeat(32), 'secret', 'session', 'https://voice.example', 'token')).rejects.toThrow()
		expect(fetcher).toHaveBeenCalledTimes(2)
	})
})
