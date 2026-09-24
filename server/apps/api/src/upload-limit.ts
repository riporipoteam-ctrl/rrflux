import { intVar } from '@repo/hono-helpers'

import type { App } from './context'

/**
 * Safe fallback when the deployment does not configure an API upload ceiling.
 *
 * Image blobs are stored in Firestore (see @repo/blob-store), not R2, so every
 * upload's cost is counted in documents, not bytes:
 *
 * - Each blob is split into ≤716800-byte chunks, and each chunk is one document
 *   write (plus one for the metadata doc). A 64 MiB upload is ~93 writes —
 *   nearly 0.5% of the Spark plan's 20,000 writes/day — in a single request,
 *   and ~85 MiB of stored documents (base64 inflates each chunk ~4/3) against a
 *   1 GiB free storage quota. A handful of max-size uploads a day would starve
 *   every other write the backend does.
 * - Firestore also caps a single document at 1 MiB, which is why chunks are
 *   sized the way they are — the cap here is about quota, not document size.
 *
 * 16 MiB keeps the quota math sane (~24 chunk writes per worst-case upload)
 * while leaving generous headroom: client photos are JPEGs, typically well
 * under 5 MiB, and custom avatar item assets are PNGs of a few hundred KiB.
 * The `storage` worker's own cap (room saves, inventions) is separate — see
 * apps/storage/src/storage.app.ts.
 */
export const DEFAULT_MAX_API_UPLOAD_BYTES = 16 * 1024 * 1024

/**
 * Resolve the per-file ceiling shared by API-owned image uploads. A non-positive
 * setting does not disable the protection: public upload routes must always remain
 * bounded, so invalid values fall back to the safe default.
 */
export function maxApiUploadBytes(env: App['Bindings']): number {
	const configured = intVar(env.RECFLARE_MAX_API_UPLOAD_BYTES, DEFAULT_MAX_API_UPLOAD_BYTES)
	return configured > 0 ? configured : DEFAULT_MAX_API_UPLOAD_BYTES
}

/** Whether a parsed multipart file is safe to copy into memory and persist to the blob store. */
export function exceedsApiUploadLimit(file: File, limit: number): boolean {
	return file.size > limit
}
