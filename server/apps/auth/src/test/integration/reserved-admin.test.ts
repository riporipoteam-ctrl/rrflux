import { adminSecretsStore, env } from 'cloudflare:test'
import { exports } from 'cloudflare:workers'
import { beforeAll, describe, expect, test } from 'vitest'

import '../../auth.app'

import {
	AUDIT_LOG_SCHEMA_DDL,
	claimReservedAdmin,
	createAccount,
	getAccount,
	ROOM_SCHEMA_DDL,
	SCHEMA_DDL,
	updateAccount,
} from '@repo/domain'

import { SCHEMA_DDL as REPORTS_SCHEMA_DDL } from '../../../../api/src/reports-db'

import { PLATFORM_SCHEMA_DDL } from '../../platform-db'
import { REFRESH_SCHEMA_DDL } from '../../refresh-db'

import type { Env } from '../../context'

declare module 'cloudflare:test' {
	interface ProvidedEnv extends Env {}
}

const ORIGIN = 'https://example.com'
const PASSWORD = 'ripo6000-test-password'

beforeAll(async () => {
	// Seed the shared JWT signing key into the local Secrets Store so .get() resolves.
	await adminSecretsStore(env.JWT_SECRET).create('test-signing-key')
	// The accounts table (mirrors the auth migrations).
	for (const stmt of SCHEMA_DDL) await env.DB.prepare(stmt).run()
	// Refresh tokens are issued on every grant.
	for (const stmt of REFRESH_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	// Platform identity links — the ban check reads this table on every grant.
	for (const stmt of PLATFORM_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	// The token grant reads the ban/report table on every grant, and a staff
	// sign-in is written to the audit log — both tables have to exist. The
	// signup also reads the room table (placing the player in Orientation);
	// an empty table is fine, it just means there is no Orientation to join.
	for (const stmt of REPORTS_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	for (const stmt of AUDIT_LOG_SCHEMA_DDL) await env.DB.prepare(stmt).run()
	for (const stmt of ROOM_SCHEMA_DDL) await env.DB.prepare(stmt).run()
})

/** Decode a JWT payload (no verification) for asserting claims. */
function decodePayload(token: string): Record<string, unknown> {
	const part = token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/')
	return JSON.parse(
		new TextDecoder().decode(Uint8Array.from(atob(part), (ch) => ch.charCodeAt(0)))
	) as Record<string, unknown>
}

/**
 * POST a form-urlencoded body to /connect/token, returning status + parsed JSON.
 * No IP header is set, which is how the other auth tests dodge the per-IP
 * signup cap (`countAccountsBySignupIp` answers 0 for an empty IP).
 */
async function postToken(body: string): Promise<{ status: number; json: Record<string, unknown> }> {
	const res = await exports.default.fetch(`${ORIGIN}/connect/token`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
		body,
	})
	return { status: res.status, json: (await res.json()) as Record<string, unknown> }
}

describe('reserved admin (Ripo6000)', () => {
	test('claimReservedAdmin refuses when an admin already exists', async () => {
		// Seed an admin the operator-granted way, under an unrelated name.
		const op = await createAccount(env.DB, { username: 'Operator9' })
		await updateAccount(env.DB, op.accountId, { isModerator: true })
		// A brand-new account under the reserved name gets nothing while any
		// admin exists — first-come means first.
		const fresh = await createAccount(env.DB, { username: 'Fresh99' })
		expect(await claimReservedAdmin(env.DB, fresh.accountId, 'Ripo6000')).toBe(false)
		expect((await getAccount(env.DB, fresh.accountId))?.isDeveloper).not.toBe(true)
		expect((await getAccount(env.DB, fresh.accountId))?.isModerator).not.toBe(true)
		// Revoke again so the DB is pristine for the signup tests below.
		await updateAccount(env.DB, op.accountId, { isModerator: false })
	})

	let ripoId = 0

	test('the first account created as Ripo6000 claims admin at signup', async () => {
		const res = await postToken(`grant_type=create_account&username=Ripo6000&password=${PASSWORD}`)
		expect(res.status).toBe(200)
		const payload = decodePayload(res.json.access_token as string)
		ripoId = Number(payload.sub)
		expect(ripoId).toBeGreaterThan(0)
		// The token's `role` claim carries the admin roles from the first login.
		expect(payload.role).toEqual(
			expect.arrayContaining(['gameClient', 'developer', 'moderator', 'screenshare'])
		)
		// The role lookups agree with the persisted flags.
		const dev = await exports.default.fetch(`${ORIGIN}/role/developer/${ripoId}`)
		expect(await dev.json()).toBe(true)
		const mod = await exports.default.fetch(`${ORIGIN}/role/moderator/${ripoId}`)
		expect(await mod.json()).toBe(true)
	})

	test('a second signup claiming the name is rejected as taken', async () => {
		const res = await postToken('grant_type=create_account&username=RiPo6000&password=otherpw')
		expect(res.status).toBe(400)
		expect(res.json.error_description).toBe('username is already taken')
	})

	test('signing in as Ripo6000 keeps admin (login never mints, never strips)', async () => {
		const res = await postToken(`account_id=${ripoId}&password=${PASSWORD}`)
		expect(res.status).toBe(200)
		const payload = decodePayload(res.json.access_token as string)
		expect(payload.role).toEqual(
			expect.arrayContaining(['gameClient', 'developer', 'moderator', 'screenshare'])
		)
	})

	test('an ordinary username gets no admin at signup', async () => {
		const res = await postToken('grant_type=create_account&username=RegularJoe&password=joepw')
		expect(res.status).toBe(200)
		const payload = decodePayload(res.json.access_token as string)
		expect(payload.role).not.toEqual(expect.arrayContaining(['developer']))
		expect(payload.role).not.toEqual(expect.arrayContaining(['moderator']))
	})
})
