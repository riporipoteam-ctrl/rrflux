import type { BlobStoreEnv } from '@repo/blob-store'
import type { HonoApp } from '@repo/hono-helpers'
import type { SharedHonoEnv, SharedHonoVariables } from '@repo/hono-helpers/src/types'

export type Env = SharedHonoEnv &
	BlobStoreEnv & {
	/** Maximum accepted binary upload size in bytes. */
	MAX_UPLOAD_BYTES?: string | number
	// Shared Secrets Store binding for the HS256 JWT signing key. Resolve the value
	// with `await env.JWT_SECRET.get()`; all workers bind the same store so tokens
	// signed by `auth` verify here.
	JWT_SECRET: SecretsStoreSecret
	// Shared CDN blobs (owned by the `cdn` worker), stored in Firestore via
	// @repo/blob-store (`binding: 'CDN_ASSETS'`). Client uploads are written here
	// under a per-FileType subfolder; the `cdn` worker serves them back. The
	// `FIRESTORE_SA_JSON` secret and `FIRESTORE_PROJECT_ID` var come from
	// BlobStoreEnv.
}

/** Variables can be extended */
export type Variables = SharedHonoVariables

export interface App extends HonoApp {
	Bindings: Env
	Variables: Variables
}
