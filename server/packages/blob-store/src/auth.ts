import { BlobStoreNotConfiguredError } from './errors'

import type { BlobStoreEnv } from './env'

/**
 * Service-account OAuth2 for Firestore, done in-worker with WebCrypto.
 *
 * `env.FIRESTORE_SA_JSON` holds the whole service-account JSON as a Worker secret
 * (never logged, never written to a file, never committed). The signed JWT asks for
 * the `datastore` scope and is exchanged at the Google OAuth2 token endpoint; the
 * access token is cached module-level (one isolate = one cache entry) until ~60s
 * before it expires.
 *
 * When `env.FIRESTORE_TEST_BACKEND` is bound (integration tests), auth is skipped
 * and a dummy token is used — the fake backend doesn't check it. The real flow is
 * covered by unit tests with a mocked `fetch`.
 */

const TOKEN_URL = 'https://oauth2.googleapis.com/token'
const SCOPE = 'https://www.googleapis.com/auth/datastore'

interface CachedToken {
	token: string
	expiresAt: number
	clientEmail: string
}

let cached: CachedToken | null = null

function pemToDer(pem: string): Uint8Array {
	const body = pem
		.replace('-----BEGIN PRIVATE KEY-----', '')
		.replace('-----END PRIVATE KEY-----', '')
		.replace(/\s+/g, '')
	if (body === '') throw new Error('Service-account JSON has an empty private_key')
	const binary = atob(body)
	const der = new Uint8Array(binary.length)
	for (let i = 0; i < binary.length; i++) der[i] = binary.charCodeAt(i)
	return der
}

function base64UrlEncodeJson(value: unknown): string {
	const json = JSON.stringify(value)
	const bytes = new TextEncoder().encode(json)
	let binary = ''
	for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i])
	return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

interface ServiceAccountJson {
	client_email: string
	private_key: string
	private_key_id?: string
}

/** Parse the secret; throws {@link BlobStoreNotConfiguredError} when it is absent. */
function readServiceAccount(env: BlobStoreEnv): ServiceAccountJson {
	const raw = env.FIRESTORE_SA_JSON
	if (!raw) throw new BlobStoreNotConfiguredError()
	let parsed: unknown
	try {
		parsed = JSON.parse(raw)
	} catch {
		throw new Error('FIRESTORE_SA_JSON is not valid JSON')
	}
	const sa = parsed as ServiceAccountJson
	if (typeof sa.client_email !== 'string' || sa.client_email === '') {
		throw new Error('FIRESTORE_SA_JSON is missing client_email')
	}
	if (typeof sa.private_key !== 'string' || sa.private_key === '') {
		throw new Error('FIRESTORE_SA_JSON is missing private_key')
	}
	return sa
}

/**
 * An OAuth2 access token for the Firestore `datastore` scope, cached until ~60s
 * before expiry. Test-backend mode returns a dummy token without touching the
 * network.
 */
export async function accessToken(env: BlobStoreEnv): Promise<string> {
	if (env.FIRESTORE_TEST_BACKEND) return 'blob-store-test-token'

	const sa = readServiceAccount(env)
	const now = Math.floor(Date.now() / 1000)
	if (cached && cached.clientEmail === sa.client_email && cached.expiresAt - 60 > now) {
		return cached.token
	}

	const key = await crypto.subtle.importKey(
		'pkcs8',
		pemToDer(sa.private_key).buffer as ArrayBuffer,
		{ name: 'RSASSA-PKCS1-v1_5', hash: 'SHA-256' },
		false,
		['sign']
	)
	const header: Record<string, string> = { alg: 'RS256', typ: 'JWT' }
	if (typeof sa.private_key_id === 'string') header.kid = sa.private_key_id
	const claims = {
		iss: sa.client_email,
		scope: SCOPE,
		aud: TOKEN_URL,
		iat: now,
		exp: now + 3600,
	}
	const signingInput = `${base64UrlEncodeJson(header)}.${base64UrlEncodeJson(claims)}`
	const signature = await crypto.subtle.sign(
		'RSASSA-PKCS1-v1_5',
		key,
		new TextEncoder().encode(signingInput)
	)
	const sigBytes = new Uint8Array(signature)
	let sigBinary = ''
	for (let i = 0; i < sigBytes.length; i++) sigBinary += String.fromCharCode(sigBytes[i])
	const assertion = `${signingInput}.${btoa(sigBinary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')}`

	const res = await fetch(TOKEN_URL, {
		method: 'POST',
		headers: { 'content-type': 'application/x-www-form-urlencoded' },
		body: `grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion=${encodeURIComponent(assertion)}`,
	})
	if (!res.ok) {
		throw new Error(`OAuth2 token exchange failed: HTTP ${res.status}`)
	}
	const body = (await res.json()) as { access_token?: string; expires_in?: number }
	if (typeof body.access_token !== 'string') {
		throw new Error('OAuth2 token exchange returned no access_token')
	}
	const entry = {
		token: body.access_token,
		expiresAt: now + (typeof body.expires_in === 'number' ? body.expires_in : 3600),
		clientEmail: sa.client_email,
	}
	cached = entry
	return entry.token
}

/** Test seam: drop the cached token (unit tests). */
export function _resetTokenCache(): void {
	cached = null
}
