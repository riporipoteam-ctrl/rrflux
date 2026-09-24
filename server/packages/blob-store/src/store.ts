import { accessToken } from './auth'
import {
	base64Decode,
	base64Encode,
	chunkCount,
	chunksForRange,
	docIdFor,
	joinChunks,
	splitChunks,
} from './codec'
import { batchWrite, chunkName, metaName, readDocument } from './firestore'

import type { BlobBinding } from './codec'
import type { BlobStoreEnv } from './env'
import type { FieldValue } from './firestore'

export type { BlobBinding } from './codec'
export type { BlobStoreEnv } from './env'

/**
 * Firestore-backed blob storage replacing Cloudflare R2 (which needs a paid plan).
 *
 * Layout: collection `flux_blobs`, meta document per blob (`contentType`, `size`,
 * `chunks`, `sha256?`, `updatedAt`) plus one document per ≤716800-byte chunk in its
 * `c` subcollection. Document ids are base64url of `binding + "\n" + key`.
 *
 * `binding` is a logical namespace only (`IMAGES` for api/img uploads,
 * `CDN_ASSETS` for storage/cdn/rooms/invention blobs) — no R2 is involved.
 */

export interface BlobPutOptions {
	/** Stored as the blob's content type; defaults to `application/octet-stream`. */
	contentType?: string
	/**
	 * SHA-256 of `data`, recorded on the meta doc (base64) so readers can answer a
	 * hash from metadata instead of downloading the blob (invention `BlobHash`).
	 */
	sha256?: ArrayBuffer
}

export interface BlobMeta {
	contentType: string
	size: number
	/** ISO timestamp string written at put time. */
	updatedAt: string
	/** base64 SHA-256 recorded at put time, when the writer supplied one. */
	sha256?: string
}

export interface BlobRead {
	data: ArrayBuffer
	contentType: string
	size: number
	updatedAt: string
}

export interface BlobRange {
	offset: number
	length?: number
}

function strField(fields: Record<string, FieldValue>, name: string): string | undefined {
	const v = fields[name]?.stringValue
	return typeof v === 'string' && v !== '' ? v : undefined
}

function intField(fields: Record<string, FieldValue>, name: string): number | undefined {
	const v = fields[name]?.integerValue
	if (typeof v !== 'string') return undefined
	const n = Number.parseInt(v, 10)
	return Number.isSafeInteger(n) ? n : undefined
}

/** Reads a Firestore timestamp field (RFC 3339 string) — accepts the native
 * `timestampValue` encoding and, defensively, a plain `stringValue`. */
function tsField(fields: Record<string, FieldValue>, name: string): string | undefined {
	const v = fields[name]
	const ts = v?.timestampValue
	if (typeof ts === 'string' && ts !== '') return ts
	const s = v?.stringValue
	return typeof s === 'string' && s !== '' ? s : undefined
}

function metaFromFields(fields: Record<string, FieldValue>): BlobMeta | null {
	const size = intField(fields, 'size')
	const updatedAt = tsField(fields, 'updatedAt')
	if (size === undefined || updatedAt === undefined) return null
	const meta: BlobMeta = {
		contentType: strField(fields, 'contentType') ?? 'application/octet-stream',
		size,
		updatedAt,
	}
	const sha256 = strField(fields, 'sha256')
	if (sha256 !== undefined) meta.sha256 = sha256
	return meta
}

/**
 * Deterministic etag for a blob, replacing the R2 `httpEtag` the img/cdn workers
 * used to serve. It is the hex SHA-256 of `"<size>\n<updatedAt>"` — stable for a
 * given stored blob, and a new upload under an existing key (new `updatedAt`)
 * yields a new etag, so `If-None-Match` conditional requests keep working.
 */
export async function blobEtag(meta: Pick<BlobMeta, 'size' | 'updatedAt'>): Promise<string> {
	const digest = await crypto.subtle.digest(
		'SHA-256',
		new TextEncoder().encode(`${meta.size}\n${meta.updatedAt}`)
	)
	return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('')
}

/**
 * Parse an HTTP `Range` header against a blob of `size` bytes.
 *
 * Returns `null` when there is no header or its unit isn't `bytes` (RFC 9110: an
 * unrecognised unit is ignored, answered with a plain 200). Any `bytes=` value —
 * including a malformed, multi-range, or unsatisfiable one — resolves to a concrete
 * `{ offset, length }`, defaulting to the whole object: callers always answer such
 * a request 206 with a `Content-Range`, never a bare 200 carrying bytes the
 * downloader would misplace. (There is no 416 path; R2's InvalidRange mapping is gone.)
 */
export function parseRangeHeader(
	header: string | null | undefined,
	size: number
): { offset: number; length: number } | null {
	if (!header || !header.startsWith('bytes=')) return null
	const whole = { offset: 0, length: size }
	const spec = header.slice('bytes='.length).trim()
	// Multi-range: serve the whole object as one 206 (same as the old R2 behavior).
	if (spec.includes(',')) return whole
	const dash = spec.indexOf('-')
	if (dash < 0) return whole
	const first = spec.slice(0, dash).trim()
	const last = spec.slice(dash + 1).trim()
	if (first === '') {
		// Suffix range: the last N bytes.
		const n = Number.parseInt(last, 10)
		if (!Number.isSafeInteger(n) || n <= 0) return whole
		const length = Math.min(n, size)
		return { offset: size - length, length }
	}
	const start = Number.parseInt(first, 10)
	if (!Number.isSafeInteger(start) || start < 0 || start >= size) return whole
	if (last === '') return { offset: start, length: size - start }
	const end = Number.parseInt(last, 10)
	if (!Number.isSafeInteger(end) || end < start) return whole
	return { offset: start, length: Math.min(end, size - 1) - start + 1 }
}

/** Store `data` under `key`, replacing any blob already there. */
export async function putBlob(
	env: BlobStoreEnv,
	binding: BlobBinding,
	key: string,
	data: ArrayBuffer,
	opts?: BlobPutOptions
): Promise<void> {
	const docId = docIdFor(binding, key)
	const bytes = new Uint8Array(data)
	const chunks = splitChunks(bytes)
	const updatedAt = new Date().toISOString()
	const fields: Record<string, FieldValue> = {
		contentType: { stringValue: opts?.contentType ?? 'application/octet-stream' },
		size: { integerValue: String(bytes.length) },
		chunks: { integerValue: String(chunks.length) },
		updatedAt: { timestampValue: updatedAt },
	}
	if (opts?.sha256) {
		fields.sha256 = { stringValue: base64Encode(new Uint8Array(opts.sha256)) }
	}
	// Replacing a larger blob would orphan its higher-numbered chunk documents,
	// so read the prior meta and delete the now-obsolete chunks in the same batch.
	const prior = await headBlob(env, binding, key)
	const staleDeletes: Array<{ delete: string }> = []
	if (prior) {
		const priorChunks = chunkCount(prior.size)
		for (let i = chunks.length; i < priorChunks; i++) {
			staleDeletes.push({ delete: chunkName(env, docId, i) })
		}
	}
	await batchWrite(env, [
		{ update: { name: metaName(env, docId), fields } },
		...chunks.map((chunk, i) => ({
			update: {
				name: chunkName(env, docId, i),
				fields: { d: { stringValue: base64Encode(chunk) } },
			},
		})),
		...staleDeletes,
	])
}

/** Read a blob (or a byte range of it). `null` when no blob is stored under `key`. */
export async function getBlob(
	env: BlobStoreEnv,
	binding: BlobBinding,
	key: string,
	opts?: { range?: BlobRange }
): Promise<BlobRead | null> {
	const docId = docIdFor(binding, key)
	const doc = await readDocument(env, metaName(env, docId))
	if (!doc) return null
	const meta = metaFromFields(doc.fields)
	if (!meta) return null

	const range = opts?.range
	const window = range
		? chunksForRange(meta.size, range.offset, range.length ?? meta.size - range.offset)
		: meta.size === 0
			? null
			: { first: 0, last: chunkCount(meta.size) - 1, skipInFirst: 0, takeTotal: meta.size }
	if (!window) {
		return { data: new ArrayBuffer(0), ...meta }
	}

	const names: string[] = []
	for (let i = window.first; i <= window.last; i++) names.push(chunkName(env, docId, i))
	const chunkDocs = await Promise.all(names.map((name) => readDocument(env, name)))
	const payloads: Uint8Array[] = []
	for (const chunkDoc of chunkDocs) {
		const d = chunkDoc?.fields ? strField(chunkDoc.fields, 'd') : undefined
		// Meta exists but a chunk is gone (raced delete / partial write): read as missing.
		if (d === undefined) return null
		payloads.push(base64Decode(d))
	}
	const joined = joinChunks(payloads, window.skipInFirst, window.takeTotal)
	// Copy to a fresh ArrayBuffer so the returned buffer is exactly the bytes.
	const data = joined.slice().buffer as ArrayBuffer
	return { data, ...meta }
}

/** Metadata for the blob under `key`, without reading its bytes. `null` when missing. */
export async function headBlob(
	env: BlobStoreEnv,
	binding: BlobBinding,
	key: string
): Promise<BlobMeta | null> {
	const doc = await readDocument(env, metaName(env, docIdFor(binding, key)))
	if (!doc) return null
	return metaFromFields(doc.fields)
}

/**
 * Delete the blobs under `keys` (one key or many). Missing keys are a no-op, like
 * the R2 deletes these replace.
 */
export async function deleteBlobs(
	env: BlobStoreEnv,
	binding: BlobBinding,
	keys: string | string[]
): Promise<void> {
	// Touch auth once up front so an unconfigured store throws before any reads.
	await accessToken(env)
	const list = Array.isArray(keys) ? keys : [keys]
	const writes: Array<{ delete: string }> = []
	for (const key of list) {
		const docId = docIdFor(binding, key)
		const doc = await readDocument(env, metaName(env, docId))
		if (!doc) continue
		const count = intField(doc.fields, 'chunks') ?? 0
		writes.push({ delete: metaName(env, docId) })
		for (let i = 0; i < count; i++) writes.push({ delete: chunkName(env, docId, i) })
	}
	if (writes.length > 0) await batchWrite(env, writes)
}
