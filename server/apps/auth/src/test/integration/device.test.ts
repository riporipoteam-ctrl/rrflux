import { adminSecretsStore, env } from 'cloudflare:test'
import { exports } from 'cloudflare:workers'
import { beforeAll, describe, expect, test } from 'vitest'

import '../../auth.app'

import { AUDIT_LOG_SCHEMA_DDL, hashPassword, SCHEMA_DDL } from '@repo/domain'

import { PLATFORM_SCHEMA_DDL } from '../../platform-db'
import { REFRESH_SCHEMA_DDL } from '../../refresh-db'
import { DEVICE_SCHEMA_DDL } from '../../device-db'

import {
	SCHEMA_DDL as REPORTS_SCHEMA_DDL,
} from '../../../../api/src/reports-db'

import type { Env } from '../../context'

declare module 'cloudflare:test' {
	interface ProvidedEnv extends Env {}
}

const ORIGIN = 'https://example.com'

const LOGIN_PASSWORD = 'device-flow-test-password'

const DEVICE_GRANT = 'urn:ietf:params:oauth:grant-type:device_code'

beforeAll(async () => {
	await adminSecretsStore(env.JWT_SECRET).create('test-signing-key')
	for (const stmt of SCHEMA_DDL) await env.DB.prepare(stmt).run()
	for (const stmt of REFRESH_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	for (const stmt of DEVICE_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	for (const stmt of PLATFORM_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	for (const stmt of AUDIT_LOG_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	for (const stmt of REPORTS_SCHEMA_DDL) await env.DB.prepare(stmt).run()

	const hash = await hashPassword(LOGIN_PASSWORD)
	await env.DB.prepare('INSERT OR IGNORE INTO account (data) VALUES (?1)')
		.bind(JSON.stringify({ accountId: 9001, username: 'DeviceTester', passwordHash: hash }))
		.run()
})

/** Decode a JWT payload (no verification) for asserting claims. */
function decodePayload(token: string): Record<string, unknown> {
	const part = token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/')
	return JSON.parse(
		new TextDecoder().decode(Uint8Array.from(atob(part), (ch) => ch.charCodeAt(0)))
	) as Record<string, unknown>
}

async function postForm(
	path: string,
	body: string
): Promise<{ status: number; text: string; json?: Record<string, unknown> }> {
	const res = await exports.default.fetch(`${ORIGIN}${path}`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
		body,
	})
	const text = await res.text()
	let json: Record<string, unknown> | undefined
	try {
		json = JSON.parse(text) as Record<string, unknown>
	} catch {
		json = undefined
	}
	return { status: res.status, text, json }
}

async function startFlow() {
	const { status, json } = await postForm('/connect/deviceauthorization', 'client_id=ios')
	expect(status).toBe(200)
	expect(json).toBeDefined()
	const j = json as Record<string, unknown>
	expect(typeof j.device_code).toBe('string')
	expect(typeof j.user_code).toBe('string')
	expect((j.user_code as string).length).toBe(8)
	expect(typeof j.verification_uri).toBe('string')
	expect(typeof j.verification_uri_complete).toBe('string')
	expect((j.verification_uri_complete as string)).toContain(encodeURIComponent(j.user_code as string))
	expect(j.expires_in).toBe(600)
	expect(j.interval).toBe(5)
	return j as { device_code: string; user_code: string; verification_uri: string }
}

async function pollToken(deviceCode: string) {
	return postForm(
		'/connect/token',
		`grant_type=${encodeURIComponent(DEVICE_GRANT)}&device_code=${encodeURIComponent(deviceCode)}`
	)
}

async function approve(userCode: string, username: string, password: string, action = 'approve') {
	return postForm(
		'/device/approve',
		`user_code=${encodeURIComponent(userCode)}&username=${encodeURIComponent(username)}&password=${encodeURIComponent(password)}&action=${action}`
	)
}

describe('device authorization flow (iOS login)', () => {
	test(
		'start → poll pending → approve → token issues for the approving account',
		{ timeout: 30000 },
		async () => {
		const flow = await startFlow()

		// Before approval the poll answers authorization_pending…
		const pending = await pollToken(flow.device_code)
		expect(pending.status).toBe(400)
		expect(pending.json?.error).toBe('authorization_pending')

		// …and an unknown code is invalid_grant.
		const bogus = await pollToken('0'.repeat(64))
		expect(bogus.status).toBe(400)
		expect(bogus.json?.error).toBe('invalid_grant')

		// Approve from the "browser" side with the account's password.
		const ok = await approve(flow.user_code, 'DeviceTester', LOGIN_PASSWORD)
		expect(ok.status).toBe(200)
		expect(ok.text).toContain('Login approved')

		// The client honours the advertised poll interval; polling faster earns a
		// slow_down, so wait it out before the final poll.
		await new Promise((resolve) => setTimeout(resolve, 5500))

		// Now the poll succeeds and the access token is for account 9001 on iOS.
		const done = await pollToken(flow.device_code)
		expect(done.status).toBe(200)
		const payload = decodePayload(done.json?.access_token as string)
		expect(payload.sub).toBe('9001')
		expect(payload.platform).toBe(5)
		expect(done.json?.token_type).toBe('Bearer')
		expect(typeof done.json?.refresh_token).toBe('string')

		// Single-use: a second poll finds nothing.
		const replay = await pollToken(flow.device_code)
		expect(replay.status).toBe(400)
		expect(replay.json?.error).toBe('invalid_grant')
		}
	)

	test('denied flow answers access_denied', async () => {
		const flow = await startFlow()
		const denied = await approve(flow.user_code, 'DeviceTester', LOGIN_PASSWORD, 'deny')
		expect(denied.status).toBe(200)
		expect(denied.text).toContain('Login denied')
		const polled = await pollToken(flow.device_code)
		expect(polled.status).toBe(400)
		expect(polled.json?.error).toBe('access_denied')
	})

	test('approval needs the right password', async () => {
		const flow = await startFlow()
		const wrong = await approve(flow.user_code, 'DeviceTester', 'not-the-password')
		expect(wrong.status).toBe(401)
		expect(wrong.text).toContain('Wrong username or password')
		// Still pending afterwards.
		const pending = await pollToken(flow.device_code)
		expect(pending.json?.error).toBe('authorization_pending')
	})

	test('approval page renders for a pending code and rejects a bogus one', async () => {
		const flow = await startFlow()
		const page = await exports.default.fetch(
			`${ORIGIN}/device?code=${encodeURIComponent(flow.user_code)}`
		)
		expect(page.status).toBe(200)
		const html = await page.text()
		expect(html).toContain('Approve device login')
		expect(html).toContain(flow.user_code)

		const bogus = await exports.default.fetch(`${ORIGIN}/device?code=ZZZZZZZZ`)
		expect(bogus.status).toBe(200)
		expect(await bogus.text()).toContain('unknown, expired, or already used')
	})
})
