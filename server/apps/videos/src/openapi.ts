import { resolver } from 'hono-openapi'
import { z } from 'zod'

/**
 * OpenAPI schemas for the videos worker.
 *
 * IMPORTANT: these are DESCRIPTIVE ONLY. They are passed to `describeRoute` to
 * generate the spec and are never wired into `hono-openapi`'s `validator()`. Same
 * rationale as the other workers: a reverse-engineered protocol, lenient
 * handlers, no runtime validation.
 *
 * Do NOT add `.meta({ id })` to these schemas — with this hono-openapi + zod v4
 * setup a meta'd schema used in a response emits a `$ref` the framework doesn't
 * always hoist into `components.schemas`, leaving a dangling reference. Leaving
 * meta off makes every schema inline, which renders correctly in any tool.
 */

/** Emit a zod schema as an `application/json` response body. */
export function json(schema: z.ZodType, description: string) {
	return { description, content: { 'application/json': { schema: resolver(schema) } } }
}

// ---- Response schemas ------------------------------------------------------

/** `GET /` — the liveness probe body. */
export const ServiceStatus = z.object({
	service: z.literal('videos'),
	status: z.literal('ok'),
})

/** A single resolved video: the id the client asked for plus a direct MP4 url. */
export const VideoEntry = z.object({
	id: z.string().describe('The video id the client requested (board id or YouTube id)'),
	title: z.string().describe('Human-readable title for the board UI'),
	url: z.string().describe('Direct playable MP4 URL for Unity VideoPlayer'),
})

/** `GET /api/videos` — the full configured catalog. */
export const VideoCatalog = z.object({
	videos: z.array(VideoEntry),
})

/** `GET /api/videos/lookup` — echo of the requested url plus the resolved entry. */
export const VideoLookup = z.object({
	requestedUrl: z.string().describe('The url (or id) the client asked to resolve'),
	video: VideoEntry.nullable().describe('The resolved video, or null when unknown'),
})
