import { createRoomieProviders } from './roomie-providers'
import { createRoomieSocket } from './roomie-socket'

export interface RoomieDevConfig {
	ENVIRONMENT: string
	ROOMIE_REALTIME_DEV_ENABLED?: string
	ROOMIE_REALTIME_DEV_KEY?: string
	ROOMIE_VOICE_TOKEN?: string
	ROOMIE_CHAT_URL?: string
	ROOMIE_VOICE_URL?: string
	ROOMIE_STT_URL?: string
}

/** Restricted test-only handshake. A caller must verify the player's JWT first. */
export function roomieDevSocket(request: Request, env: RoomieDevConfig, fetcher: typeof fetch = fetch): Response {
	if (!['development', 'VITEST'].includes(env.ENVIRONMENT) || env.ROOMIE_REALTIME_DEV_ENABLED !== 'true') {
		return new Response(null, { status: 404 })
	}
	const provided = request.headers.get('x-roomie-dev-key') || ''
	const expected = env.ROOMIE_REALTIME_DEV_KEY || ''
	const encode = new TextEncoder()
	const a = encode.encode(provided), b = encode.encode(expected)
	if (!expected || a.length !== b.length || !crypto.subtle.timingSafeEqual(a, b)) {
		return new Response(null, { status: 401 })
	}
	if (request.headers.get('upgrade')?.toLowerCase() !== 'websocket') {
		return new Response(null, { status: 426 })
	}
	if (!env.ROOMIE_VOICE_TOKEN || !env.ROOMIE_VOICE_URL) {
		return new Response(null, { status: 503 })
	}
	const chatUrl = env.ROOMIE_CHAT_URL || env.ROOMIE_VOICE_URL.replace(/\/tts\/?$/, '/chat')
	const sttUrl = env.ROOMIE_STT_URL || env.ROOMIE_VOICE_URL.replace(/\/tts\/?$/, '/stt')
	return createRoomieSocket(createRoomieProviders({
		serviceToken: env.ROOMIE_VOICE_TOKEN,
		chatUrl,
		voiceUrl: env.ROOMIE_VOICE_URL,
		sttUrl,
		fetcher,
	}), { id: crypto.randomUUID() })
}
