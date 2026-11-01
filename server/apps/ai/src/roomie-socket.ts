import { RoomieRealtime } from './roomie-realtime'
import type { RoomieEvent, RoomieProviders, RoomieRealtimeSession } from './roomie-realtime'

/** A connection-scoped realtime event bridge; authenticate before calling it. */
export function createRoomieSocket(providers: RoomieProviders, session: RoomieRealtimeSession): Response {
	const pair = new WebSocketPair()
	const client = pair[0]
	const server = pair[1]
	const realtime = new RoomieRealtime(providers, session)
	let closed = false
	let queue = Promise.resolve()
	server.accept()
	server.send(JSON.stringify(realtime.created()))
	server.addEventListener('message', (message) => {
		queue = queue.then(async () => {
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
				server.send(JSON.stringify({ type: 'error', error: { type: 'invalid_request_error', code: 'invalid_event' } }))
				return
			}
			const events = await realtime.handle(event)
			if (!closed) for (const output of events) server.send(JSON.stringify(output))
		}).catch(() => {
			if (!closed) {
				closed = true
				try { server.close(1011, 'Session error') } catch { /* socket already closed */ }
			}
		})
	})
	server.addEventListener('close', () => {
		closed = true
		void realtime.handle({ type: 'response.cancel' })
	})
	server.addEventListener('error', () => { closed = true })
	return new Response(null, { status: 101, webSocket: client })
}
