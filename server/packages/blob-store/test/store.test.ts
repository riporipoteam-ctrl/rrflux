import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'

import { _resetTokenCache } from '../src/auth'
import { docIdFor } from '../src/codec'
import { BlobStoreNotConfiguredError } from '../src/errors'
import { deleteBlobs, getBlob, headBlob, putBlob } from '../src/store'

import type { BlobStoreEnv } from '../src/env'

/** In-memory fake standing in for the token endpoint + Firestore REST. */
interface FakeState {
	docs: Map<string, { fields: Record<string, any>; updateTime: string }>
	tokenCalls: number
	batchWriteCalls: number
	lastAssertion: string | null
	failBatchWrite: boolean
	seenProjects: string[]
}

function newState(): FakeState {
	return {
		docs: new Map(),
		tokenCalls: 0,
		batchWriteCalls: 0,
		lastAssertion: null,
		failBatchWrite: false,
		seenProjects: [],
	}
}

function installFake(state: FakeState): void {
	const fake = async (input: any, init: any = {}): Promise<Response> => {
		const url = new URL(typeof input === 'string' ? input : input.url)
		if (url.hostname === 'oauth2.googleapis.com') {
			state.tokenCalls++
			const params = new URLSearchParams(init.body as string)
			state.lastAssertion = params.get('assertion')
			return Response.json({ access_token: 'fake-access-token', expires_in: 3600 })
		}
		const m = url.pathname.match(
			/^\/v1\/projects\/([^/]+)\/databases\/\(default\)\/documents(?::(batchWrite))?(?:\/(.+))?$/
		)
		if (!m) return new Response('nope', { status: 404 })
		state.seenProjects.push(m[1])
		if (init.method === 'POST' && m[2] === 'batchWrite') {
			state.batchWriteCalls++
			if (state.failBatchWrite) return new Response('boom', { status: 500 })
			const body = JSON.parse(init.body as string)
			const now = new Date().toISOString()
			const writeResults: Array<Record<string, unknown>> = []
			for (const w of body.writes) {
				if (w.update) {
					state.docs.set(w.update.name, { fields: w.update.fields, updateTime: now })
					writeResults.push({ updateTime: now })
				} else if (w.delete) {
					state.docs.delete(w.delete)
					writeResults.push({})
				}
			}
			return Response.json({ writeResults, status: [{}] })
		}
		if ((init.method === undefined || init.method === 'GET') && m[3]) {
			const name = `projects/${m[1]}/databases/(default)/documents/${m[3]}`
			const doc = state.docs.get(name)
			if (!doc) {
				return Response.json(
					{ error: { code: 404, message: 'not found', status: 'NOT_FOUND' } },
					{ status: 404 }
				)
			}
			return Response.json({
				name,
				fields: doc.fields,
				createTime: doc.updateTime,
				updateTime: doc.updateTime,
			})
		}
		return new Response('bad', { status: 400 })
	}
	vi.stubGlobal('fetch', fake)
}

/** A fake (but correctly-shaped) service-account JSON with a real RSA key. */
async function makeFakeSaJson(): Promise<string> {
	const key = (await crypto.subtle.generateKey(
		{
			name: 'RSASSA-PKCS1-v1_5',
			modulusLength: 2048,
			publicExponent: new Uint8Array([1, 0, 1]),
			hash: 'SHA-256',
		},
		true,
		['sign']
	)) as CryptoKeyPair
	const der = new Uint8Array(
		(await crypto.subtle.exportKey('pkcs8', key.privateKey)) as ArrayBuffer
	)
	let binary = ''
	for (const b of der) binary += String.fromCharCode(b)
	const pem =
		`-----BEGIN PRIVATE KEY-----\n${btoa(binary).match(/.{1,64}/g)!.join('\n')}\n-----END PRIVATE KEY-----\n`
	return JSON.stringify({
		type: 'service_account',
		project_id: 'flux-544a6',
		private_key_id: 'test-kid-1',
		private_key: pem,
		client_email: 'test-sa@flux-544a6.iam.gserviceaccount.com',
	})
}

function b64uDecode(s: string): string {
	const padded = s.replace(/-/g, '+').replace(/_/g, '/')
	return atob(padded + '='.repeat((4 - (padded.length % 4)) % 4))
}

function patternBytes(length: number): Uint8Array {
	const out = new Uint8Array(length)
	for (let i = 0; i < length; i++) out[i] = (i * 31 + 7) % 256
	return out
}

const CHUNK_SIZE = 716_800

let state: FakeState
let saJson: string

beforeEach(async () => {
	_resetTokenCache()
	state = newState()
	installFake(state)
	saJson = await makeFakeSaJson()
})

afterEach(() => {
	vi.unstubAllGlobals()
})

const env = (): BlobStoreEnv => ({ FIRESTORE_SA_JSON: saJson })

describe('putBlob / getBlob / headBlob', () => {
	test('round-trips a small blob with its metadata', async () => {
		const data = patternBytes(12_345).buffer as ArrayBuffer
		await putBlob(env(), 'IMAGES', 'sharecamera/2026-09-22/abc.jpg', data, {
			contentType: 'image/jpeg',
		})

		const got = await getBlob(env(), 'IMAGES', 'sharecamera/2026-09-22/abc.jpg')
		expect(got).not.toBeNull()
		expect(new Uint8Array(got!.data)).toEqual(new Uint8Array(data))
		expect(got!.contentType).toBe('image/jpeg')
		expect(got!.size).toBe(12_345)
		expect(new Date(got!.updatedAt).getTime()).not.toBeNaN()

		const head = await headBlob(env(), 'IMAGES', 'sharecamera/2026-09-22/abc.jpg')
		expect(head).toMatchObject({ contentType: 'image/jpeg', size: 12_345 })
	})

	test('splits large blobs into ≤716800-byte chunks in one batchWrite', async () => {
		const data = patternBytes(1_500_000).buffer as ArrayBuffer
		await putBlob(env(), 'CDN_ASSETS', 'room/2026-02-03/big', data)
		// 1 meta + 3 chunks in a single batchWrite call.
		expect(state.batchWriteCalls).toBe(1)

		const docId = docIdFor('CDN_ASSETS', 'room/2026-02-03/big')
		const meta = state.docs.get(
			`projects/flux-544a6/databases/(default)/documents/flux_blobs/${docId}`
		)
		expect(meta!.fields.chunks).toEqual({ integerValue: '3' })
		expect(meta!.fields.size).toEqual({ integerValue: '1500000' })

		for (let i = 0; i < 3; i++) {
			const chunk = state.docs.get(
				`projects/flux-544a6/databases/(default)/documents/flux_blobs/${docId}/c/${i}`
			)
			expect(chunk).toBeDefined()
			const raw = Uint8Array.from(atob(chunk!.fields.d.stringValue), (c) => c.charCodeAt(0))
			expect(raw.length).toBeLessThanOrEqual(CHUNK_SIZE)
		}

		const got = await getBlob(env(), 'CDN_ASSETS', 'room/2026-02-03/big')
		expect(new Uint8Array(got!.data)).toEqual(new Uint8Array(data))
	})

	test('reads byte ranges across chunk boundaries', async () => {
		const data = patternBytes(2 * CHUNK_SIZE + 100)
		await putBlob(env(), 'IMAGES', 'ranged.jpg', data.buffer as ArrayBuffer)

		const offset = CHUNK_SIZE - 50
		const got = await getBlob(env(), 'IMAGES', 'ranged.jpg', {
			range: { offset, length: 200 },
		})
		expect(new Uint8Array(got!.data)).toEqual(data.subarray(offset, offset + 200))
		expect(got!.size).toBe(data.length)
	})

	test('reads a range to the end when length is omitted', async () => {
		const data = patternBytes(1000)
		await putBlob(env(), 'IMAGES', 'tail.jpg', data.buffer as ArrayBuffer)
		const got = await getBlob(env(), 'IMAGES', 'tail.jpg', { range: { offset: 900 } })
		expect(new Uint8Array(got!.data)).toEqual(data.subarray(900))
	})

	test('overwriting with a smaller blob deletes orphaned chunks', async () => {
		const big = patternBytes(CHUNK_SIZE + 100)
		await putBlob(env(), 'IMAGES', 'shrink.jpg', big.buffer as ArrayBuffer)
		const docId = docIdFor('IMAGES', 'shrink.jpg')
		const chunk1 = `projects/flux-544a6/databases/(default)/documents/flux_blobs/${docId}/c/1`
		expect(state.docs.has(chunk1)).toBe(true)

		const small = patternBytes(10)
		await putBlob(env(), 'IMAGES', 'shrink.jpg', small.buffer as ArrayBuffer)
		expect(state.docs.has(chunk1)).toBe(false)

		const got = await getBlob(env(), 'IMAGES', 'shrink.jpg')
		expect(new Uint8Array(got!.data)).toEqual(small)
		expect((await headBlob(env(), 'IMAGES', 'shrink.jpg'))!.size).toBe(10)
	})

	test('missing keys read as null', async () => {
		expect(await getBlob(env(), 'IMAGES', 'nope.jpg')).toBeNull()
		expect(await headBlob(env(), 'IMAGES', 'nope.jpg')).toBeNull()
	})

	test('empty blobs round-trip with no chunk documents', async () => {
		await putBlob(env(), 'IMAGES', 'empty.jpg', new ArrayBuffer(0))
		const head = await headBlob(env(), 'IMAGES', 'empty.jpg')
		expect(head).toMatchObject({ size: 0 })
		const got = await getBlob(env(), 'IMAGES', 'empty.jpg')
		expect(got!.data.byteLength).toBe(0)
		expect(state.batchWriteCalls).toBe(1)
	})

	test('records the sha256 given at put time on the meta doc', async () => {
		const data = patternBytes(100).buffer as ArrayBuffer
		const sha256 = await crypto.subtle.digest('SHA-256', data)
		await putBlob(env(), 'CDN_ASSETS', 'invention/2026-07-12/lamp.inv', data, { sha256 })
		const head = await headBlob(env(), 'CDN_ASSETS', 'invention/2026-07-12/lamp.inv')
		const expected = btoa(String.fromCharCode(...new Uint8Array(sha256)))
		expect(head!.sha256).toBe(expected)
	})

	test('defaults contentType to application/octet-stream', async () => {
		await putBlob(env(), 'CDN_ASSETS', 'data/x', patternBytes(10).buffer as ArrayBuffer)
		expect((await headBlob(env(), 'CDN_ASSETS', 'data/x'))!.contentType).toBe(
			'application/octet-stream'
		)
	})
})

describe('deleteBlobs', () => {
	test('deletes the meta doc and every chunk; missing keys are a no-op', async () => {
		await putBlob(env(), 'IMAGES', 'gone/a.jpg', patternBytes(CHUNK_SIZE + 5).buffer as ArrayBuffer)
		await putBlob(env(), 'IMAGES', 'gone/b.jpg', patternBytes(10).buffer as ArrayBuffer)

		await deleteBlobs(env(), 'IMAGES', 'gone/a.jpg')
		expect(await getBlob(env(), 'IMAGES', 'gone/a.jpg')).toBeNull()
		expect(await getBlob(env(), 'IMAGES', 'gone/b.jpg')).not.toBeNull()

		await deleteBlobs(env(), 'IMAGES', ['gone/b.jpg', 'never-existed.jpg'])
		expect(await getBlob(env(), 'IMAGES', 'gone/b.jpg')).toBeNull()
	})

	test('deleting a missing key issues no writes', async () => {
		const before = state.batchWriteCalls
		await deleteBlobs(env(), 'IMAGES', 'nothing-here.jpg')
		expect(state.batchWriteCalls).toBe(before)
	})
})

describe('configuration', () => {
	test('throws BlobStoreNotConfiguredError without a service-account secret', async () => {
		const empty: BlobStoreEnv = {}
		await expect(putBlob(empty, 'IMAGES', 'k', new ArrayBuffer(1))).rejects.toBeInstanceOf(
			BlobStoreNotConfiguredError
		)
		await expect(getBlob(empty, 'IMAGES', 'k')).rejects.toBeInstanceOf(
			BlobStoreNotConfiguredError
		)
		await expect(headBlob(empty, 'IMAGES', 'k')).rejects.toBeInstanceOf(
			BlobStoreNotConfiguredError
		)
		await expect(deleteBlobs(empty, 'IMAGES', 'k')).rejects.toBeInstanceOf(
			BlobStoreNotConfiguredError
		)
		// And no HTTP was attempted.
		expect(state.tokenCalls).toBe(0)
		expect(state.batchWriteCalls).toBe(0)
	})

	test('uses FIRESTORE_PROJECT_ID when set, defaults to flux-544a6', async () => {
		await putBlob(env(), 'IMAGES', 'a.jpg', patternBytes(4).buffer as ArrayBuffer)
		expect(state.seenProjects).toContain('flux-544a6')

		state.seenProjects.length = 0
		await putBlob({ ...env(), FIRESTORE_PROJECT_ID: 'other-proj' }, 'IMAGES', 'b.jpg', patternBytes(4).buffer as ArrayBuffer)
		expect(state.seenProjects).toContain('other-proj')
	})

	test('a batchWrite HTTP failure surfaces as an error', async () => {
		state.failBatchWrite = true
		await expect(
			putBlob(env(), 'IMAGES', 'k.jpg', patternBytes(4).buffer as ArrayBuffer)
		).rejects.toThrow(/batchWrite failed/)
	})
})

describe('service-account auth', () => {
	test('signs an RS256 JWT and exchanges it for a token', async () => {
		await putBlob(env(), 'IMAGES', 'auth.jpg', patternBytes(4).buffer as ArrayBuffer)
		expect(state.tokenCalls).toBe(1)
		const parts = state.lastAssertion!.split('.')
		expect(parts).toHaveLength(3)

		const header = JSON.parse(b64uDecode(parts[0]))
		expect(header).toMatchObject({ alg: 'RS256', typ: 'JWT', kid: 'test-kid-1' })

		const claims = JSON.parse(b64uDecode(parts[1]))
		expect(claims.iss).toBe('test-sa@flux-544a6.iam.gserviceaccount.com')
		expect(claims.aud).toBe('https://oauth2.googleapis.com/token')
		expect(claims.scope).toBe('https://www.googleapis.com/auth/datastore')
		expect(claims.exp - claims.iat).toBe(3600)
	})

	test('caches the token across operations', async () => {
		await putBlob(env(), 'IMAGES', 'a.jpg', patternBytes(4).buffer as ArrayBuffer)
		await putBlob(env(), 'IMAGES', 'b.jpg', patternBytes(4).buffer as ArrayBuffer)
		await getBlob(env(), 'IMAGES', 'a.jpg')
		expect(state.tokenCalls).toBe(1)
	})

	test('rejects a malformed service-account secret', async () => {
		await expect(
			putBlob({ FIRESTORE_SA_JSON: 'not json' }, 'IMAGES', 'k', new ArrayBuffer(1))
		).rejects.toThrow(/not valid JSON/)
	})
})
