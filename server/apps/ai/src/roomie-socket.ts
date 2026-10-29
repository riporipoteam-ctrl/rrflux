import { RoomieRealtime } from './roomie-realtime'
import type { RoomieEvent, RoomieProviders } from './roomie-realtime'

/** Offline/development helper; callers must authenticate before creating a socket. */
export function createRoomieSocket(providers: RoomieProviders): Response {
	const pair = new WebSocketPair()
	const client = pair[0]
	const server = pair[1]
	const session = new RoomieRealtime(providers)
	let closed = false
	server.accept()
	server.addEventListener('message', (message) => {
		void (async () => {
			if (closed) return
			if (typeof message.data !== 'string' || message.data.length > 2_000_000) {
				server.close(1009, 'Invalid message')
				return
			}
			let event: RoomieEvent
			try {
				const parsed: unknown = JSON.parse(message.data)
				if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed) ||
					!('type' in parsed) || typeof parsed.type !== 'string') throw new Error('invalid event')
				event = parsed as RoomieEvent
			} catch {
				server.send(JSON.stringify({ type: 'error', error: { code: 'invalid_event' } }))
				return
			}
			const events = await session.handle(event)
			if (!closed) for (const output of events) server.send(JSON.stringify(output))
		})().catch(() => {
			if (!closed) {
				closed = true
				try { server.close(1011, 'Session error') } catch { /* socket already closed */ }
			}
		})
	})
	server.addEventListener('close', () => {
		closed = true
		void session.handle({ type: 'response.cancel' })
	})
	server.addEventListener('error', () => { closed = true })
	return new Response(null, { status: 101, webSocket: client })
}
