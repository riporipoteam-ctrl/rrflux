// GUID case-insensitivity end-to-end check for the store-crash fix.
//
// The in-game Store resolves the items on its shelves through
// `POST /api/customAvatarItems/v1/bulk`, which funnels every id through
// `getCustomAvatarItems`. The 2023 client is not consistent about GUID case
// (uppercase from some flows, lowercase from others) while the D1 lookup is
// case-sensitive: an id that misses here renders as "not for sale" and breaks
// checkout — the store-crash / custom-shirt-purchase-error path.
//
// This test seeds a player-made shirt stored under a LOWERCASE guid and asks
// the bulk endpoint for the UPPERCASE spelling, the way the client does. On the
// unfixed code the answer is [] (the bug); on the fixed code the shirt resolves.
import { adminSecretsStore, env } from 'cloudflare:test'
import { exports } from 'cloudflare:workers'
import { beforeAll, expect, test } from 'vitest'

import {
	createCustomAvatarItem,
	SCHEMA_DDL as CUSTOM_AVATAR_ITEM_SCHEMA_DDL,
} from '../../custom-avatar-items-db'

import type { Env } from '../../context'

declare module 'cloudflare:test' {
	interface ProvidedEnv extends Env {}
}

const ORIGIN = 'https://example.com'
// Test-only HMAC key, seeded into the local Secrets Store below — not a secret.
const SIGNING_KEY = 'test-signing-key'
// Stored lowercase, as player-created shirts are stored.
const LOWER_GUID = 'a1b2c3d4-e5f6-47a8-9b0c-d1e2f3a4b5c6'

function b64url(input: ArrayBuffer | string): string {
	const bytes = typeof input === 'string' ? new TextEncoder().encode(input) : new Uint8Array(input)
	let binary = ''
	for (const byte of bytes) binary += String.fromCharCode(byte)
	return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

async function bearer(sub = '42'): Promise<Record<string, string>> {
	const now = Math.floor(Date.now() / 1000)
	const signingInput = `${b64url(JSON.stringify({ alg: 'HS256', typ: 'JWT' }))}.${b64url(
		JSON.stringify({ sub, exp: now + 3600 })
	)}`
	const key = await crypto.subtle.importKey(
		'raw',
		new TextEncoder().encode(SIGNING_KEY),
		{ name: 'HMAC', hash: 'SHA-256' },
		false,
		['sign']
	)
	const sig = await crypto.subtle.sign('HMAC', key, new TextEncoder().encode(signingInput))
	return { Authorization: `Bearer ${signingInput}.${b64url(sig)}` }
}

async function bulk(guid: string): Promise<Array<{ CustomAvatarItemId: string; Name: string }>> {
	const res = await exports.default.fetch(`${ORIGIN}/api/customAvatarItems/v1/bulk`, {
		method: 'POST',
		headers: { 'content-type': 'application/x-www-form-urlencoded', ...(await bearer()) },
		body: new URLSearchParams([['customAvatarItemIds', guid]]),
	})
	expect(res.status).toBe(200)
	return (await res.json()) as Array<{ CustomAvatarItemId: string; Name: string }>
}

beforeAll(async () => {
	// Seed the shared JWT signing key into the local Secrets Store so .get() resolves.
	await adminSecretsStore(env.JWT_SECRET).create('test-signing-key')
	for (const stmt of CUSTOM_AVATAR_ITEM_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	await createCustomAvatarItem(env.DB, {
		customAvatarItemId: LOWER_GUID,
		creatorAccountId: 42,
		name: 'Guid Case Shirt',
		description: 'store crash repro',
		price: 100,
		baseAvatarItemId: 2184,
		baseAvatarItemColor: 'FFFFFF',
		accessibility: 1,
		designFilename: 'design.png',
		thumbnailImageFilename: 'thumb.png',
	})
})

test('bulk resolves a stored-lowercase GUID asked for in UPPERCASE', async () => {
	const items = await bulk(LOWER_GUID.toUpperCase())
	expect(items).toHaveLength(1)
	expect(items[0].CustomAvatarItemId).toBe(LOWER_GUID)
	expect(items[0].Name).toBe('Guid Case Shirt')
})

test('bulk still resolves the exact-case (lowercase) GUID', async () => {
	const items = await bulk(LOWER_GUID)
	expect(items).toHaveLength(1)
	expect(items[0].CustomAvatarItemId).toBe(LOWER_GUID)
})

test('bulk still misses a genuinely unknown GUID', async () => {
	const items = await bulk('00000000-0000-4000-8000-000000000000')
	expect(items).toHaveLength(0)
})
