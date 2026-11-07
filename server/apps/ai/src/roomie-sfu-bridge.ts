import { processSfuVoiceTurn, sfuPacketPayload } from './roomie-sfu-audio'
import type { Env } from './context'
import type { RoomieMessage } from './roomie-realtime'
import { createRoomieSfuAdapters, closeRoomieSfuAdapters, requestRoomieVoiceTrack, answerRoomieVoiceTrack } from './roomie-sfu'

/** One instance coordinates the incoming microphone and outgoing voice sockets. */
export class RoomieSfuBridge {
	private input: WebSocket | null = null
	private output: WebSocket | null = null
	private frames: Uint8Array[] = []
	private bytes = 0
	private silenceBytes = 0
	private speaking = false
	private busy = false
	private history: RoomieMessage[] = []
	private sessionId: string | null = null
	private setup: Promise<unknown> | null = null
	constructor(private readonly state: DurableObjectState, private readonly env: Env) {}

	async fetch(request: Request): Promise<Response> {
		const path = new URL(request.url).pathname
		if (request.method === 'POST' && path === '/bind') {
			const body = await request.json() as { sessionId?: string }
			if (!body.sessionId || !/^[A-Za-z0-9_-]{1,128}$/.test(body.sessionId)) return new Response(null, { status: 400 })
			this.sessionId = body.sessionId
			await this.state.storage.put('sfuSessionId', body.sessionId)
			return new Response(null, { status: 204 })
		}
		if (request.method === 'POST' && path === '/ready') {
			const body = await request.json() as { token?: string }
			const sessionId = this.sessionId || await this.state.storage.get<string>('sfuSessionId')
			const { ROOMIE_SFU_APP_ID: appId, ROOMIE_SFU_APP_SECRET: secret } = this.env
			// ROOMIE_SFU_SOCKET_ORIGIN is an optional override. The production worker
			// did not have that binding, which made every /ready handshake return 503
			// after the client had already spawned Roomie. NAME and DOMAIN are injected
			// by run-wrangler-deploy and resolve to the actual public Worker origin.
			const origin = this.env.ROOMIE_SFU_SOCKET_ORIGIN ||
				`https://${this.env.NAME}.${this.env.DOMAIN || 'ripo-ripoteam.workers.dev'}`
			if (!sessionId || !appId || !secret || !origin || !body.token) return new Response(null, { status: 503 })
			if (!this.setup) this.setup = (async () => {
				let lastError: unknown
				for (let attempt = 0; attempt < 10; attempt++) {
					let adapterIds: string[] = []
					try {
						const voice = await createRoomieSfuAdapters(appId, secret, sessionId, origin, body.token!)
						adapterIds = voice.adapterIds
						return await requestRoomieVoiceTrack(appId, secret, sessionId, voice.voiceSessionId, voice.voiceTrackName)
					} catch (error) {
						lastError = error
						if (adapterIds.length) await closeRoomieSfuAdapters(appId, secret, adapterIds).catch(() => {})
						await new Promise(resolve => setTimeout(resolve, 1000))
					}
				}
				throw lastError
			})().catch(error => { this.setup = null; throw error })
			try { return Response.json(await this.setup) } catch { return Response.json({ error: 'SFU adapter setup failed' }, { status: 502 }) }
		}
		if (request.method === 'POST' && path === '/answer') {
			const sessionId = this.sessionId || await this.state.storage.get<string>('sfuSessionId')
			if (!sessionId || !this.env.ROOMIE_SFU_APP_ID || !this.env.ROOMIE_SFU_APP_SECRET) return new Response(null, { status: 503 })
			const body = await request.json() as { sdp?: string }
			if (!body.sdp || body.sdp.length > 100_000 || !body.sdp.startsWith('v=0')) return new Response(null, { status: 400 })
			try { await answerRoomieVoiceTrack(this.env.ROOMIE_SFU_APP_ID, this.env.ROOMIE_SFU_APP_SECRET, sessionId, body.sdp); return new Response(null, { status: 204 }) }
			catch { return new Response(null, { status: 502 }) }
		}
		if (request.headers.get('upgrade')?.toLowerCase() !== 'websocket') return new Response(null, { status: 426 })
		const direction = new URL(request.url).searchParams.get('direction')
		if (direction !== 'input' && direction !== 'output') return new Response(null, { status: 400 })
		const pair = new WebSocketPair()
		const client = pair[0], socket = pair[1]
		socket.accept()
		if (direction === 'input') {
			this.input?.close(1000, 'Replaced')
			this.input = socket
			socket.addEventListener('message', event => { if (event.data instanceof ArrayBuffer) this.receive(new Uint8Array(event.data)) })
			socket.addEventListener('close', () => { if (this.input === socket) this.input = null })
		} else {
			this.output?.close(1000, 'Replaced')
			this.output = socket
			socket.addEventListener('close', () => { if (this.output === socket) this.output = null })
		}
		return new Response(null, { status: 101, webSocket: client })
	}

	private receive(packet: Uint8Array): void {
		if (this.busy || packet.length > 32_768) return
		try {
			const pcm = sfuPacketPayload(packet)
			if (!pcm.length || pcm.length % 8) return
			const view = new DataView(pcm.buffer, pcm.byteOffset, pcm.byteLength)
			let peak = 0
			for (let i = 0; i < pcm.length; i += 2) peak = Math.max(peak, Math.abs(view.getInt16(i, true)))
			if (peak > 600) { this.speaking = true; this.silenceBytes = 0 } else if (this.speaking) this.silenceBytes += pcm.length
			if (!this.speaking) return
			this.frames.push(pcm); this.bytes += pcm.length
			if (this.bytes >= 3_840_000 || this.silenceBytes >= 153_600) void this.finishTurn()
		} catch { /* discard malformed audio */ }
	}

	private async finishTurn(): Promise<void> {
		if (this.busy || !this.bytes) return
		this.busy = true
		const pcm = new Uint8Array(this.bytes)
		let offset = 0
		for (const frame of this.frames) { pcm.set(frame, offset); offset += frame.length }
		this.frames = []; this.bytes = 0; this.silenceBytes = 0; this.speaking = false
		try {
			const token = this.env.ROOMIE_VOICE_TOKEN
			const voiceUrl = this.env.ROOMIE_VOICE_URL
			if (!token || !voiceUrl || !this.output) return
			const result = await processSfuVoiceTurn(pcm, {
				serviceToken: token, voiceUrl,
				chatUrl: this.env.ROOMIE_CHAT_URL || voiceUrl.replace(/\/tts\/?$/, '/chat'),
				sttUrl: this.env.ROOMIE_STT_URL || voiceUrl.replace(/\/tts\/?$/, '/stt'),
			}, this.history)
			if (!result || !this.output) return
			this.history.push({ role: 'user', content: result.transcript }, { role: 'assistant', content: result.reply })
			this.history = this.history.slice(-10)
			for (const packet of result.packets) this.output.send(packet)
		} catch {
			// Provider or socket failures must not terminate the session.
		} finally { this.busy = false }
	}
}
