import type { BlobStoreEnv } from '@repo/blob-store'
import type { HonoApp } from '@repo/hono-helpers'
import type { SharedHonoEnv, SharedHonoVariables } from '@repo/hono-helpers/src/types'
// Type-only import (erased at build) of the DO class owned by the `notify`
// worker, so the cross-worker RPC stub is fully typed.
import type { NotificationsHub } from '../../notify/src/notifications-hub'

export type Env = SharedHonoEnv &
	BlobStoreEnv & {
	// Shared Secrets Store binding for the HS256 JWT signing key. Resolve the value
	// with `await env.JWT_SECRET.get()`; all workers bind the same store so tokens
	// signed by `auth` verify here.
	JWT_SECRET: SecretsStoreSecret
	/**
	 * Base domain the share-link URL is derived from, e.g. `rec.example.com`.
	 * Injected at deploy time via `--var DOMAIN`; defaults in `wrangler.jsonc`
	 * for local dev and tests.
	 */
	DOMAIN: string
	/** Maximum accepted size of each API-owned image upload, in bytes. */
	RECFLARE_MAX_API_UPLOAD_BYTES?: string
	// Shared rooms database (schema/migrations owned by the `rooms` worker). Used
	// read-only here to resolve room roles for `/api/rooms/v1/verifyRole`.
	DB: D1Database
	// Image blobs (shared with the `img` worker, which serves them back by key).
	// Stored in Firestore via @repo/blob-store (`binding: 'IMAGES'`); the
	// `FIRESTORE_SA_JSON` secret and `FIRESTORE_PROJECT_ID` var come from
	// BlobStoreEnv.
	//
	// Shared CDN blobs (owned by the `cdn` worker, written by `storage`). Read
	// here only to hash an invention's uploaded data blob under `invention/`
	// (`binding: 'CDN_ASSETS'`).
	/**
	 * The per-player settings map the `playersettings` worker owns (`player:<id>` → JSON
	 * `{ key: value }`). Read and written here by
	 * `GET|PUT /api/players/v1/playerPhotoTaggingSetting`: the photo-tagging preference is
	 * one key (`playerPhotoTaggingSetting`) in that shared bag, not a store of its own, so
	 * the write MERGES — see `writePhotoTaggingSetting`.
	 */
	RECFLARE_PLAYER_SETTINGS: KVNamespace
	// SignalR notifications hub (DO owned by the `notify` worker). Bound here to
	// push RelationshipChanged notifications when a player's relationship changes.
	RECFLARE_NOTIFICATIONS_HUB: DurableObjectNamespace<NotificationsHub>
}

/** Variables can be extended */
export type Variables = SharedHonoVariables

export interface App extends HonoApp {
	Bindings: Env
	Variables: Variables
}
