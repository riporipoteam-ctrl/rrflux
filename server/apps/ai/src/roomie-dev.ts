import { createRoomieProviders } from './roomie-providers'
import { createRoomieSocket } from './roomie-socket'

export interface RoomieDevConfig {
	ENVIRONMENT: string
	ROOMIE_REALTIME_DEV_ENABLED?: string
	ROOMIE_REALTIME_DEV_KEY?: string
	HF_TOKEN?: string
	ROOMIE_VOICE_TOKEN?: string
	AI?: { run(model: string, input: { audio: number[] }): Promise<{ text?: string }> }
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
	if (!env.AI || !env.HF_TOKEN || !env.ROOMIE_VOICE_TOKEN) {
		return new Response(null, { status: 503 })
	}
	return createRoomieSocket(createRoomieProviders({
		ai: env.AI, hfToken: env.HF_TOKEN, voiceToken: env.ROOMIE_VOICE_TOKEN, fetcher,
	}))
}
