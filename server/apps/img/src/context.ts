import type { BlobStoreEnv } from '@repo/blob-store'
import type { HonoApp } from '@repo/hono-helpers'
import type { SharedHonoEnv, SharedHonoVariables } from '@repo/hono-helpers/src/types'

export type Env = SharedHonoEnv &
	BlobStoreEnv & {
	/** Shared `recflare` D1 — the `images` metadata table this worker owns. */
	DB: D1Database
	// Image blobs, keyed by filename, served back by this worker. Stored in
	// Firestore via @repo/blob-store (`binding: 'IMAGES'`); the
	// `FIRESTORE_SA_JSON` secret and `FIRESTORE_PROJECT_ID` var come from
	// BlobStoreEnv.
	//
	// Shared CDN blobs. Only the `image/` prefix is read here: images uploaded
	// through the `storage` worker are stored extensionless under
	// `image/<date>/<uuid>` and requested from this worker by the bare name
	// (`binding: 'CDN_ASSETS'`).
	/** Static assets (fallback images) served from `static/`. */
	ASSETS: Fetcher
	/**
	 * RSA-2048 private key (PKCS8 DER, base64) used to sign image responses
	 * requested with `?sig=p1`. Optional — when absent, responses are unsigned.
	 */
	IMG_SIGNING_KEY?: string
	/**
	 * Feature flag for REAL response signing. `?sig=p1` always returns a
	 * `Content-Signature` header, but only when this is true is the value an
	 * actual RSA-SHA1 signature over the body; when false (the default) it is a
	 * cheap placeholder derived from the object key, which keeps the response on
	 * the streaming path. See `stubSignature()` in `img.app.ts`.
	 */
	IMG_SIGNING_ENABLED?: boolean
}

/** Variables can be extended */
export type Variables = SharedHonoVariables

export interface App extends HonoApp {
	Bindings: Env
	Variables: Variables
}
