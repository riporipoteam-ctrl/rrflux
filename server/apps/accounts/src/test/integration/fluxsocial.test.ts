import { adminSecretsStore, env } from 'cloudflare:test'
import { exports } from 'cloudflare:workers'
import { beforeAll, describe, expect, test } from 'vitest'

import '../../accounts.app'

import { SCHEMA_DDL } from '@repo/domain'

import { FLUXSOCIAL_SCHEMA_DDL } from '../../fluxsocial-db'

import type { Env } from '../../context'

declare module 'cloudflare:test' {
	interface ProvidedEnv extends Env {}
}

const ORIGIN = 'https://example.com'

// Seed the account schema (for /fluxsocial/me lookups) plus the Flux Social
// tables (migration 0010) into the test D1.
beforeAll(async () => {
	await adminSecretsStore(env.JWT_SECRET).create('test-signing-key')
	for (const stmt of SCHEMA_DDL) await env.DB.prepare(stmt).run()
	for (const stmt of FLUXSOCIAL_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	const insert = env.DB.prepare('INSERT OR IGNORE INTO account (data) VALUES (?1)')
	await env.DB.batch([
		insert.bind(
			JSON.stringify({
				accountId: 42,
				username: 'Player42',
				displayName: 'Player 42',
				profileImage: 'DefaultProfileImage.jpg',
			})
		),
	])
})

// Mint a token the way the `auth` worker does, signing with the shared test key.
const TEST_SECRET = 'test-signing-key'

const bearer = async (sub = '42'): Promise<Record<string, string>> => {
	const b64 = (input: ArrayBuffer | string): string => {
		const bytes = typeof input === 'string' ? new TextEncoder().encode(input) : new Uint8Array(input)
		let binary = ''
		for (const byte of bytes) binary += String.fromCharCode(byte)
		return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
	}
	const now = Math.floor(Date.now() / 1000)
	const signingInput = `${b64(JSON.stringify({ alg: 'HS256', typ: 'JWT' }))}.${b64(
		JSON.stringify({ sub, exp: now + 3600 })
	)}`
	const key = await crypto.subtle.importKey(
		'raw',
		new TextEncoder().encode(TEST_SECRET),
		{ name: 'HMAC', hash: 'SHA-256' },
		false,
		['sign']
	)
	const sig = await crypto.subtle.sign('HMAC', key, new TextEncoder().encode(signingInput))
	return { Authorization: `Bearer ${signingInput}.${b64(sig)}` }
}

const postJson = (body: unknown, headers: Record<string, string> = {}): RequestInit => ({
	method: 'POST',
	headers: { 'Content-Type': 'application/json', ...headers },
	body: JSON.stringify(body),
})

/** Mint a code in-game, then exchange it on the website — the full happy path. */
async function linkAccount(sub = '42'): Promise<{ token: string; accountId: number }> {
	const mintRes = await exports.default.fetch(`${ORIGIN}/account/me/fluxsocial/linkcode`, {
		method: 'POST',
		headers: await bearer(sub),
	})
	expect(mintRes.status).toBe(200)
	const { code } = (await mintRes.json()) as { code: string }
	const exchangeRes = await exports.default.fetch(
		`${ORIGIN}/fluxsocial/exchange`,
		postJson({ code })
	)
	expect(exchangeRes.status).toBe(200)
	const body = (await exchangeRes.json()) as { token: string; accountId: number }
	return { token: body.token, accountId: body.accountId }
}

describe('Flux Social pairing codes', () => {
	test('POST /account/me/fluxsocial/linkcode 401s without a token', async () => {
		const res = await exports.default.fetch(`${ORIGIN}/account/me/fluxsocial/linkcode`, {
			method: 'POST',
		})
		expect(res.status).toBe(401)
	})

	test('mint returns a 6-digit code with a 600s expiry', async () => {
		const res = await exports.default.fetch(`${ORIGIN}/account/me/fluxsocial/linkcode`, {
			method: 'POST',
			headers: await bearer(),
		})
		expect(res.status).toBe(200)
		const body = (await res.json()) as { code: string; expiresIn: number }
		expect(body.code).toMatch(/^\d{6}$/)
		expect(body.expiresIn).toBe(600)
	})

	test('minting again invalidates the previous code', async () => {
		const headers = await bearer()
		const first = (await (
			await exports.default.fetch(`${ORIGIN}/account/me/fluxsocial/linkcode`, {
				method: 'POST',
				headers,
			})
		).json()) as { code: string }
		await exports.default.fetch(`${ORIGIN}/account/me/fluxsocial/linkcode`, {
			method: 'POST',
			headers,
		})
		// The old code is dead — the exchange burns it as invalid.
		const res = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/exchange`,
			postJson({ code: first.code })
		)
		expect(res.status).toBe(400)
	})

	test('exchange rejects a malformed code with 400', async () => {
		const res = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/exchange`,
			postJson({ code: 'not-a-code' })
		)
		expect(res.status).toBe(400)
	})

	test('exchange rejects an unknown code with 400', async () => {
		const res = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/exchange`,
			postJson({ code: '000000' })
		)
		expect(res.status).toBe(400)
	})

	test('exchange rejects an expired code with 410 and burns it', async () => {
		const headers = await bearer()
		const { code } = (await (
			await exports.default.fetch(`${ORIGIN}/account/me/fluxsocial/linkcode`, {
				method: 'POST',
				headers,
			})
		).json()) as { code: string }
		// Age the code past its TTL directly in D1.
		const past = Math.floor(Date.now() / 1000) - 3600
		await env.DB.prepare(`UPDATE flux_link_codes SET expires_at = ?1`).bind(past).run()
		const res = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/exchange`,
			postJson({ code })
		)
		expect(res.status).toBe(410)
		// Burned: a second attempt is 400 (unknown), not 410.
		const again = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/exchange`,
			postJson({ code })
		)
		expect(again.status).toBe(400)
	})

	test('exchange is throttled per IP after 20 bad attempts', async () => {
		const ip = '203.0.113.99'
		for (let i = 0; i < 20; i++) {
			const res = await exports.default.fetch(
				`${ORIGIN}/fluxsocial/exchange`,
				postJson({ code: '000000' }, { 'CF-Connecting-IP': ip })
			)
			expect(res.status).toBe(400)
		}
		const throttled = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/exchange`,
			postJson({ code: '000000' }, { 'CF-Connecting-IP': ip })
		)
		expect(throttled.status).toBe(429)
	})
})

describe('Flux Social website sessions', () => {
	test('full link flow: mint → exchange → me', async () => {
		const { token, accountId } = await linkAccount()
		expect(accountId).toBe(42)
		expect(typeof token).toBe('string')
		expect(token.length).toBeGreaterThan(32)
		// The raw code and raw token are never stored — only hashes.
		const codeRows = await env.DB.prepare(`SELECT COUNT(*) AS n FROM flux_link_codes`)
			.first<{ n: number }>()
		expect(codeRows?.n).toBe(0)
		const meRes = await exports.default.fetch(`${ORIGIN}/fluxsocial/me`, {
			headers: { Authorization: `Bearer ${token}` },
		})
		expect(meRes.status).toBe(200)
		const me = (await meRes.json()) as Record<string, unknown>
		expect(me).toMatchObject({
			accountId: 42,
			username: 'Player42',
			displayName: 'Player 42',
			privacy: { showProfile: true, showRooms: true, showPhotos: true, showInventions: true },
		})
	})

	test('a code is single-use', async () => {
		const headers = await bearer()
		const { code } = (await (
			await exports.default.fetch(`${ORIGIN}/account/me/fluxsocial/linkcode`, {
				method: 'POST',
				headers,
			})
		).json()) as { code: string }
		const first = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/exchange`,
			postJson({ code })
		)
		expect(first.status).toBe(200)
		const second = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/exchange`,
			postJson({ code })
		)
		expect(second.status).toBe(400)
	})

	test('GET /fluxsocial/me 401s with a garbage token', async () => {
		const res = await exports.default.fetch(`${ORIGIN}/fluxsocial/me`, {
			headers: { Authorization: 'Bearer <redacted>' },
		})
		expect(res.status).toBe(401)
	})

	test('unlink revokes the session instantly (website token)', async () => {
		const { token } = await linkAccount()
		const unlinkRes = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/unlink`,
			postJson({}, { Authorization: `Bearer ${token}` })
		)
		expect(unlinkRes.status).toBe(200)
		const meRes = await exports.default.fetch(`${ORIGIN}/fluxsocial/me`, {
			headers: { Authorization: `Bearer ${token}` },
		})
		expect(meRes.status).toBe(401)
	})

	test('unlink works with the game JWT too', async () => {
		const { token } = await linkAccount()
		const unlinkRes = await exports.default.fetch(`${ORIGIN}/fluxsocial/unlink`, {
			method: 'POST',
			headers: await bearer(),
		})
		expect(unlinkRes.status).toBe(200)
		const meRes = await exports.default.fetch(`${ORIGIN}/fluxsocial/me`, {
			headers: { Authorization: `Bearer ${token}` },
		})
		expect(meRes.status).toBe(401)
	})

	test('relink replaces the session — the old token dies', async () => {
		const first = await linkAccount()
		const second = await linkAccount()
		expect(second.token).not.toBe(first.token)
		const oldMe = await exports.default.fetch(`${ORIGIN}/fluxsocial/me`, {
			headers: { Authorization: `Bearer ${first.token}` },
		})
		expect(oldMe.status).toBe(401)
		const newMe = await exports.default.fetch(`${ORIGIN}/fluxsocial/me`, {
			headers: { Authorization: `Bearer ${second.token}` },
		})
		expect(newMe.status).toBe(200)
	})

	test('unlink is idempotent', async () => {
		const headers = await bearer()
		for (let i = 0; i < 2; i++) {
			const res = await exports.default.fetch(`${ORIGIN}/fluxsocial/unlink`, {
				method: 'POST',
				headers,
			})
			expect(res.status).toBe(200)
		}
	})
})

describe('Flux Social privacy', () => {
	test('defaults are all visible', async () => {
		const { token } = await linkAccount()
		const res = await exports.default.fetch(`${ORIGIN}/fluxsocial/privacy`, {
			headers: { Authorization: `Bearer ${token}` },
		})
		expect(res.status).toBe(200)
		expect(await res.json()).toEqual({
			showProfile: true,
			showRooms: true,
			showPhotos: true,
			showInventions: true,
		})
	})

	test('partial update merges over current values', async () => {
		const { token } = await linkAccount()
		const headers = { Authorization: `Bearer ${token}` }
		const updateRes = await exports.default.fetch(
			`${ORIGIN}/fluxsocial/privacy`,
			postJson({ showPhotos: false, showRooms: false }, headers)
		)
		expect(updateRes.status).toBe(200)
		expect(await updateRes.json()).toEqual({
			showProfile: true,
			showRooms: false,
			showPhotos: false,
			showInventions: true,
		})
		const readRes = await exports.default.fetch(`${ORIGIN}/fluxsocial/privacy`, { headers })
		expect(await readRes.json()).toEqual({
			showProfile: true,
			showRooms: false,
			showPhotos: false,
			showInventions: true,
		})
	})

	test('privacy is reachable with the game JWT as well', async () => {
		const headers = await bearer()
		const res = await exports.default.fetch(`${ORIGIN}/fluxsocial/privacy`, { headers })
		expect(res.status).toBe(200)
	})

	test('status reflects link state', async () => {
		const headers = await bearer()
		// Fresh account 43 has no session yet.
		const before = await exports.default.fetch(`${ORIGIN}/fluxsocial/status`, {
			headers: await bearer('43'),
		})
		expect(before.status).toBe(200)
		expect(((await before.json()) as { linked: boolean }).linked).toBe(false)
		await linkAccount()
		const after = await exports.default.fetch(`${ORIGIN}/fluxsocial/status`, { headers })
		expect((((await after.json()) as { linked: boolean }).linked)).toBe(true)
	})

	test('privacy endpoints 401 without any credential', async () => {
		const res = await exports.default.fetch(`${ORIGIN}/fluxsocial/privacy`)
		expect(res.status).toBe(401)
	})
})
