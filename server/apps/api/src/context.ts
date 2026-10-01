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
	/**
	 * Shared admin key for the operator admin API (`/api/admin/v1/*`, used by the
	 * RipoBot Discord bot). Injected at deploy time via `--var ADMIN_API_KEY` from
	 * `RECFLARE_ADMIN_API_KEY` in the server `.env`. Absent until the operator sets it.
	 */
	ADMIN_API_KEY?: string
	/**
	 * Tokens granted at signup (operator knob, default 500). Read here only so the
	 * admin token-grant applies the same signup grant econ does before crediting.
	 */
	STARTING_TOKENS?: string | number
	/**
	 * Fallback token gift minted by `/api/avatar/v2/gifts/generate` when no
	 * EarnableRewards catalog exists (operator knob, default 50).
	 */
	GIFT_FALLBACK_TOKENS?: string | number
	/**
	 * Operator-configured default gift box minted by
	 * `POST /api/admin/v1/gifts/grant` when no `gift_id` is given. There is no
	 * authentic gift-box catalog, so what "a gift" contains is the operator's
	 * explicit choice — these values are NOT Rec Room data. Defaults grant
	 * nothing; set e.g. `OPERATOR_DEFAULT_GIFT_TOKENS=100` to restore a token gift.
	 */
	OPERATOR_DEFAULT_GIFT_TOKENS?: string | number
	OPERATOR_DEFAULT_GIFT_XP?: string | number
	OPERATOR_DEFAULT_GIFT_RARITY?: string | number
	OPERATOR_DEFAULT_GIFT_MESSAGE?: string
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
	/**
	 * Service bindings for the ns-host proxies (see api.app.ts). Older
	 * `2025patch.ini` files point the ns host at this worker instead of
	 * `fluxrec-auth`, so the same `/api/storefronts/*`, `/rooms/*` and
	 * `/sections/*` paths are served here by calling the owning workers directly.
	 */
	ECON: Fetcher
	ROOMS: Fetcher
	DISCOVERY: Fetcher
}

/** Variables can be extended */
export type Variables = SharedHonoVariables

export interface App extends HonoApp {
	Bindings: Env
	Variables: Variables
}
