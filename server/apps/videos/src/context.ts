import type { HonoApp } from '@repo/hono-helpers'
import type { SharedHonoEnv, SharedHonoVariables } from '@repo/hono-helpers/src/types'

export type Env = SharedHonoEnv & {
	/**
	 * Optional JSON object mapping video ids (or YouTube video ids) to direct
	 * playable MP4 URLs, merged over the built-in catalog. Values are either a
	 * URL string or `{ title, url }`. Example:
	 * `{"dQw4w9WgXcQ": "https://cdn.example.com/v/never-gonna.mp4"}`.
	 * The URLs are public, so this is a plain var, not a secret.
	 */
	VIDEOS_MAP?: string
}

/** Variables can be extended */
export type Variables = SharedHonoVariables

export interface App extends HonoApp {
	Bindings: Env
	Variables: Variables
}
