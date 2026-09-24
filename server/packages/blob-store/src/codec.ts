/**
 * Deterministic encoding between `(binding, key)` pairs and Firestore document ids,
 * and the chunk-index math that turns byte ranges into the chunk documents to read.
 *
 * A blob lives under one meta document in the `flux_blobs` collection plus one
 * document per chunk in its `c` subcollection. Document ids are base64url (no
 * padding) of `binding + "\n" + key`, so they are deterministic, reversible, and can
 * never contain `/` (which would nest the path).
 */

/** Logical blob namespace. The binding is a namespace only — there is no R2 behind it. */
export type BlobBinding = 'IMAGES' | 'CDN_ASSETS'

/** Raw bytes per chunk document. 716800 * 4/3 ≈ 955 KiB of base64, under the 1 MiB doc limit. */
export const CHUNK_SIZE = 716_800

/** Encode bytes to base64url without padding (URL/path-safe, never contains `/`). */
export function base64UrlEncode(bytes: Uint8Array): string {
	let binary = ''
	for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i])
	return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

/** Decode a base64url (padding optional) string back to bytes. */
export function base64UrlDecode(s: string): Uint8Array {
	const padded = s.replace(/-/g, '+').replace(/_/g, '/')
	const withPad = padded + '='.repeat((4 - (padded.length % 4)) % 4)
	const binary = atob(withPad)
	const out = new Uint8Array(binary.length)
	for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i)
	return out
}

/** Standard base64 encode of bytes (used for chunk payloads, mirroring Firestore). */
export function base64Encode(bytes: Uint8Array): string {
	let binary = ''
	// Chunked so a large chunk can't blow the argument-length limit of apply().
	for (let i = 0; i < bytes.length; i += 0x8000) {
		binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
	}
	return btoa(binary)
}

/** Standard base64 decode to bytes. */
export function base64Decode(s: string): Uint8Array {
	const binary = atob(s)
	const out = new Uint8Array(binary.length)
	for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i)
	return out
}

/** The `flux_blobs` document id for a `(binding, key)` pair. */
export function docIdFor(binding: BlobBinding, key: string): string {
	const raw = `${binding}\n${key}`
	return base64UrlEncode(new TextEncoder().encode(raw))
}

/** Inverse of {@link docIdFor}: the `(binding, key)` a document id was made from. */
export function bindingAndKeyFor(docId: string): { binding: BlobBinding; key: string } {
	const raw = new TextDecoder().decode(base64UrlDecode(docId))
	const nl = raw.indexOf('\n')
	if (nl < 0) throw new Error('Not a blob-store document id')
	const binding = raw.slice(0, nl)
	if (binding !== 'IMAGES' && binding !== 'CDN_ASSETS') {
		throw new Error('Not a blob-store document id')
	}
	return { binding, key: raw.slice(nl + 1) }
}

/** How many chunk documents a blob of `size` bytes is stored as. */
export function chunkCount(size: number): number {
	return size === 0 ? 0 : Math.ceil(size / CHUNK_SIZE)
}

/**
 * Which chunk documents a byte range touches, as `[first, last]` (inclusive chunk
 * indexes) plus the byte offset of the range inside the first chunk.
 *
 * `offset`/`length` follow the resolved-Range semantics the callers use: `length`
 * may extend past the end of the blob and is clamped to it. Returns `null` for an
 * empty range (zero length after clamping), which callers answer without reading.
 */
export function chunksForRange(
	size: number,
	offset: number,
	length: number
): { first: number; last: number; skipInFirst: number; takeTotal: number } | null {
	const start = Math.max(0, offset)
	const end = Math.min(size, offset + length)
	if (end <= start || size === 0) return null
	const first = Math.floor(start / CHUNK_SIZE)
	const last = Math.floor((end - 1) / CHUNK_SIZE)
	return {
		first,
		last,
		skipInFirst: start - first * CHUNK_SIZE,
		takeTotal: end - start,
	}
}

/**
 * Split bytes into chunk payloads of at most {@link CHUNK_SIZE} raw bytes.
 * An empty input yields no chunks (the meta document alone records the empty blob).
 */
export function splitChunks(data: Uint8Array): Uint8Array[] {
	const out: Uint8Array[] = []
	for (let i = 0; i < data.length; i += CHUNK_SIZE) out.push(data.subarray(i, i + CHUNK_SIZE))
	return out
}

/**
 * Reassemble chunk payloads into one buffer, applying the range window
 * {@link chunksForRange} computed: skip `skipInFirst` bytes of the first chunk and
 * take `takeTotal` bytes overall.
 */
export function joinChunks(
	chunks: Uint8Array[],
	skipInFirst: number,
	takeTotal: number
): Uint8Array {
	const out = new Uint8Array(takeTotal)
	let written = 0
	for (let i = 0; i < chunks.length && written < takeTotal; i++) {
		const chunk = i === 0 ? chunks[i].subarray(skipInFirst) : chunks[i]
		const take = Math.min(chunk.length, takeTotal - written)
		out.set(chunk.subarray(0, take), written)
		written += take
	}
	if (written !== takeTotal) throw new Error('Chunk data shorter than the range asked for')
	return out
}
