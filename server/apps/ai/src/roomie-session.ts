const encoder = new TextEncoder()
const SESSION_TTL_SECONDS = 15 * 60

type RoomieClaims = {
	v: 1
	aud: 'roomie-realtime'
	sub: number
	sid: string
	exp: number
	model: string
	instructions: string
	voice: string
}

function base64Url(bytes: Uint8Array): string {
	let binary = ''
	for (const byte of bytes) binary += String.fromCharCode(byte)
	return btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '')
}

function decodeBase64Url(text: string): Uint8Array {
	const base64 = text.replaceAll('-', '+').replaceAll('_', '/') + '='.repeat((4 - text.length % 4) % 4)
	const binary = atob(base64)
	const output = new Uint8Array(binary.length)
	for (let i = 0; i < binary.length; i++) output[i] = binary.charCodeAt(i)
	return output
}

async function hmacKey(secret: string): Promise<CryptoKey> {
	return crypto.subtle.importKey('raw', encoder.encode(secret), { name: 'HMAC', hash: 'SHA-256' }, false, ['sign', 'verify'])
}

/** Issue a short-lived client credential. It is only valid on this Worker’s realtime route. */
export async function createRoomieClientSecret(
	jwtSecret: string,
	accountId: number,
	sessionId: string,
	options: { model?: string; instructions?: string; voice?: string } = {},
	nowSeconds = Math.floor(Date.now() / 1000)
): Promise<string> {
	const claims: RoomieClaims = {
		v: 1,
		aud: 'roomie-realtime',
		sub: accountId,
		sid: sessionId,
		exp: nowSeconds + SESSION_TTL_SECONDS,
		model: options.model?.slice(0, 120) || 'ripo-roomie-realtime',
		instructions: options.instructions?.slice(0, 1500) || '',
		voice: options.voice?.slice(0, 40) || 'alloy',
	}
	const payload = base64Url(encoder.encode(JSON.stringify(claims)))
	const signature = await crypto.subtle.sign('HMAC', await hmacKey(jwtSecret), encoder.encode(payload))
	return `ek_roomie.${payload}.${base64Url(new Uint8Array(signature))}`
}

/** Verify the one-session credential without consulting storage or exposing the player JWT. */
export async function verifyRoomieClientSecret(
	jwtSecret: string,
	clientSecret: string,
	nowSeconds = Math.floor(Date.now() / 1000)
): Promise<RoomieClaims | null> {
	try {
		const [prefix, payload, signature, extra] = clientSecret.split('.')
		if (prefix !== 'ek_roomie' || !payload || !signature || extra !== undefined) return null
		const valid = await crypto.subtle.verify('HMAC', await hmacKey(jwtSecret), decodeBase64Url(signature), encoder.encode(payload))
		if (!valid) return null
		const value = JSON.parse(new TextDecoder().decode(decodeBase64Url(payload))) as Partial<RoomieClaims>
		if (value.v !== 1 || value.aud !== 'roomie-realtime' || !Number.isSafeInteger(value.sub) ||
			!Number.isSafeInteger(value.exp) || typeof value.sid !== 'string' || !value.sid || value.sid.length > 80 ||
			typeof value.model !== 'string' || typeof value.instructions !== 'string' || typeof value.voice !== 'string' ||
			(value.exp as number) <= nowSeconds || (value.exp as number) > nowSeconds + SESSION_TTL_SECONDS + 5) return null
		return value as RoomieClaims
	} catch {
		return null
	}
}
