import type { HonoApp } from '@repo/hono-helpers'
import type { SharedHonoEnv, SharedHonoVariables } from '@repo/hono-helpers/src/types'

export type Env = SharedHonoEnv & {
	/**
	 * Shared Secrets Store binding for the HS256 JWT signing key. Resolve the value with
	 * `await env.JWT_SECRET.get()`; every worker binds the same store, so tokens signed by
	 * `auth` verify here.
	 */
	JWT_SECRET: SecretsStoreSecret
	HF_TOKEN?: string
	HF_MODEL?: string
	ROOMIE_VOICE_TOKEN?: string
	ROOMIE_CHAT_URL?: string
	ROOMIE_VOICE_URL?: string
	ROOMIE_STT_URL?: string
}

/** Variables can be extended */
export type Variables = SharedHonoVariables

export interface App extends HonoApp {
	Bindings: Env
	Variables: Variables
}
