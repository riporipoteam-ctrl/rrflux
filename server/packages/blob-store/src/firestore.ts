import { accessToken } from './auth'

import type { BlobStoreEnv } from './env'

/**
 * Minimal Firestore REST client — just the surface the blob store uses:
 * `documents:batchWrite` for writes/deletes and single-document GET for reads.
 *
 * Document layout (collection `flux_blobs`):
 * - meta doc `flux_blobs/{docId}`: `contentType` (string), `size` (integer),
 *   `chunks` (integer), `sha256` (string, optional, base64), `updatedAt`
 *   (timestamp, ISO string)
 * - chunk docs `flux_blobs/{docId}/c/{0,1,…}`: `d` (string, base64 of ≤716800 raw bytes)
 *
 * In tests (`FIRESTORE_TEST_BACKEND` bound) every request is routed through the
 * fake backend instead of `https://firestore.googleapis.com`, and the OAuth2 flow
 * is skipped (see auth.ts).
 */

/** One Firestore field value in REST JSON form. Optional members because a real
 * document only carries one encoding per field, and defensive reads probe several. */
export interface FieldValue {
	stringValue?: string
	integerValue?: string
	timestampValue?: string
	bytesValue?: string
}

export type FirestoreWrite =
	| { update: { name: string; fields: Record<string, FieldValue> } }
	| { delete: string }

export interface FirestoreDocument {
	name: string
	fields: Record<string, FieldValue>
	updateTime: string
}

/** Firestore caps a batchWrite at 500 writes. */
const MAX_BATCH_WRITES = 500

function projectId(env: BlobStoreEnv): string {
	return env.FIRESTORE_PROJECT_ID ?? 'flux-544a6'
}

function basePath(env: BlobStoreEnv): string {
	return `/v1/projects/${projectId(env)}/databases/(default)`
}

/** Full resource name of the meta document for `docId`. */
export function metaName(env: BlobStoreEnv, docId: string): string {
	return `projects/${projectId(env)}/databases/(default)/documents/flux_blobs/${docId}`
}

/** Full resource name of chunk `index` of the blob behind `docId`. */
export function chunkName(env: BlobStoreEnv, docId: string, index: number): string {
	return `${metaName(env, docId)}/c/${index}`
}

async function rest(
	env: BlobStoreEnv,
	path: string,
	init?: RequestInit
): Promise<Response> {
	// Throws BlobStoreNotConfiguredError when there is no SA secret and no test
	// backend — the single choke point for "storage is not configured".
	const token = await accessToken(env)
	const headers = new Headers(init?.headers)
	headers.set('authorization', `Bearer ${token}`)
	if (env.FIRESTORE_TEST_BACKEND) {
		return env.FIRESTORE_TEST_BACKEND.fetch(`https://firestore.test${path}`, {
			...init,
			headers,
		})
	}
	return fetch(`https://firestore.googleapis.com${path}`, { ...init, headers })
}

/** Apply writes (updates and deletes) via `documents:batchWrite`, ≤500 per call. */
export async function batchWrite(env: BlobStoreEnv, writes: FirestoreWrite[]): Promise<void> {
	for (let i = 0; i < writes.length; i += MAX_BATCH_WRITES) {
		const slice = writes.slice(i, i + MAX_BATCH_WRITES)
		const res = await rest(env, `${basePath(env)}/documents:batchWrite`, {
			method: 'POST',
			headers: { 'content-type': 'application/json' },
			body: JSON.stringify({ writes: slice }),
		})
		if (!res.ok) {
			throw new Error(`Firestore batchWrite failed: HTTP ${res.status}`)
		}
		const body = (await res.json()) as {
			writeResults?: Array<{ status?: { code?: number; message?: string } }>
		}
		const failed = (body.writeResults ?? []).find(
			(r) => r.status && typeof r.status.code === 'number' && r.status.code !== 0
		)
		if (failed?.status) {
			throw new Error(
				`Firestore batchWrite write failed: code ${failed.status.code} ${failed.status.message ?? ''}`.trim()
			)
		}
	}
}

/**
 * Read one document by full resource name. `null` when it doesn't exist (HTTP
 * 404); anything else non-OK throws.
 */
export async function readDocument(
	env: BlobStoreEnv,
	name: string
): Promise<FirestoreDocument | null> {
	const res = await rest(env, `/v1/${name}`, { method: 'GET' })
	if (res.status === 404) return null
	if (!res.ok) {
		throw new Error(`Firestore read failed: HTTP ${res.status}`)
	}
	const body = (await res.json()) as {
		name?: string
		fields?: Record<string, FieldValue>
		updateTime?: string
	}
	if (typeof body.name !== 'string') throw new Error('Firestore read returned no document name')
	return {
		name: body.name,
		fields: body.fields ?? {},
		updateTime: typeof body.updateTime === 'string' ? body.updateTime : '',
	}
}
