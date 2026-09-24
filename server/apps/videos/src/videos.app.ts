import { Hono } from 'hono'
import { describeRoute, openAPIRouteHandler } from 'hono-openapi'
import { useWorkersLogger } from 'workers-tagged-logger'

import { withCleanSpec, withNotFound, withOnError } from '@repo/hono-helpers'

import { json, ServiceStatus, VideoCatalog, VideoEntry, VideoLookup } from './openapi'

import type { App, Env } from './context'

/**
 * Videos worker — serves direct playable MP4 URLs for the in-game video board.
 *
 * NS discovery advertises this worker to the 2023 client as `Videos`
 * (`https://videos.<domain>`). Unity's VideoPlayer cannot play a
 * `youtube.com/watch` page — it needs a DIRECT media file URL — so this worker
 * maps board video ids (or YouTube ids) to direct MP4 URLs and hands those back.
 *
 * The catalog is the built-in map below plus the `VIDEOS_MAP` var (a JSON object
 * mapping id -> url or id -> { title, url }), which wins over the built-ins. That
 * keeps the real Rec Center board ids configurable without a code change: once
 * traffic capture reveals the board's actual video ids, point each at a hosted
 * MP4 (Ripo Team's own CDN copy, or any direct-MP4 host) via the var.
 *
 * IMPORTANT: this worker does NOT extract YouTube streams. There is no free /
 * supported way to do that inside a Worker, and hot-linking YouTube's
 * `googlevideo.com` URLs breaks (they are signed per-IP and expire). Map each
 * YouTube id to a directly-hosted MP4 instead.
 */

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

interface CatalogEntry {
	title: string
	url: string
}

/**
 * Built-in catalog. These are Google's public sample-bucket MP4s — direct,
 * stable, freely-licensed test videos that Unity VideoPlayer plays without
 * issue. They are placeholders: replace/extend via the VIDEOS_MAP var with the
 * real board content (e.g. Ripo Team's own hosted MP4s).
 */
const BUILTIN_CATALOG: Record<string, CatalogEntry> = {
	// Google GCS sample bucket — direct MP4s, extremely stable.
	'big-buck-bunny': {
		title: 'Big Buck Bunny (sample)',
		url: 'https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/BigBuckBunny.mp4',
	},
	'elephants-dream': {
		title: 'Elephants Dream (sample)',
		url: 'https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/ElephantsDream.mp4',
	},
	'for-bigger-blazes': {
		title: 'For Bigger Blazes (sample)',
		url: 'https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/ForBiggerBlazes.mp4',
	},
	'for-bigger-escapes': {
		title: 'For Bigger Escapes (sample)',
		url: 'https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/ForBiggerEscapes.mp4',
	},
	'for-bigger-fun': {
		title: 'For Bigger Fun (sample)',
		url: 'https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/ForBiggerFun.mp4',
	},
	'for-bigger-joyrides': {
		title: 'For Bigger Joyrides (sample)',
		url: 'https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/ForBiggerJoyrides.mp4',
	},
	'for-bigger-meltdowns': {
		title: 'For Bigger Meltdowns (sample)',
		url: 'https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/ForBiggerMeltdowns.mp4',
	},
	'sintel': {
		title: 'Sintel (sample)',
		url: 'https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/Sintel.mp4',
	},
	'tears-of-steel': {
		title: 'Tears of Steel (sample)',
		url: 'https://commondatastorage.googleapis.com/gtv-videos-bucket/sample/TearsOfSteel.mp4',
	},
}

/** Parse the VIDEOS_MAP var into catalog entries. Malformed input is ignored. */
function parseVideosMapVar(raw: string | undefined): Record<string, CatalogEntry> {
	if (!raw) return {}
	let parsed: unknown
	try {
		parsed = JSON.parse(raw)
	} catch {
		return {}
	}
	if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return {}
	const out: Record<string, CatalogEntry> = {}
	for (const [id, value] of Object.entries(parsed as Record<string, unknown>)) {
		if (typeof value === 'string' && /^https?:\/\//.test(value)) {
			out[id] = { title: id, url: value }
		} else if (
			typeof value === 'object' &&
			value !== null &&
			typeof (value as Record<string, unknown>).url === 'string' &&
			/^https?:\/\//.test((value as Record<string, unknown>).url as string)
		) {
			const v = value as Record<string, unknown>
			out[id] = {
				title: typeof v.title === 'string' ? v.title : id,
				url: v.url as string,
			}
		}
		// Anything else is silently skipped: lenient handler, descriptive spec.
	}
	return out
}

/** Full catalog: built-ins overridden by the VIDEOS_MAP var. */
function getCatalog(env: Env): Record<string, CatalogEntry> {
	return { ...BUILTIN_CATALOG, ...parseVideosMapVar(env.VIDEOS_MAP) }
}

/** Pull a YouTube video id out of a watch / youtu.be / embed / shorts URL or a bare id. */
function extractYouTubeId(input: string): string | null {
	const trimmed = input.trim()
	// Bare 11-char id.
	if (/^[A-Za-z0-9_-]{11}$/.test(trimmed)) return trimmed
	const patterns = [
		/[?&]v=([A-Za-z0-9_-]{11})/,
		/youtu\.be\/([A-Za-z0-9_-]{11})/,
		/\/embed\/([A-Za-z0-9_-]{11})/,
		/\/shorts\/([A-Za-z0-9_-]{11})/,
		/\/live\/([A-Za-z0-9_-]{11})/,
	]
	for (const re of patterns) {
		const m = trimmed.match(re)
		if (m) return m[1]
	}
	return null
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

const app = new Hono<App>()
	.use(
		'*',
		// middleware
		(c, next) =>
			useWorkersLogger(c.env.NAME, {
				environment: c.env.ENVIRONMENT,
				release: c.env.SENTRY_RELEASE,
			})(c, next)
	)

	.onError(withOnError())
	.notFound(withNotFound())

	// Liveness probe.
	.get(
		'/',
		describeRoute({
			tags: ['Videos'],
			summary: 'Service status',
			description: 'Liveness probe for the videos service.',
			responses: { 200: json(ServiceStatus, 'The service is up.') },
		}),
		async (c) => {
			return c.json({ service: 'videos', status: 'ok' })
		}
	)

	// Full catalog — what the board can offer.
	.get(
		'/api/videos',
		describeRoute({
			tags: ['Videos'],
			summary: 'List videos',
			description:
				'The configured video catalog: each entry carries a direct playable MP4 URL ' +
				'suitable for Unity VideoPlayer.',
			responses: { 200: json(VideoCatalog, 'The video catalog.') },
		}),
		async (c) => {
			const catalog = getCatalog(c.env)
			return c.json({
				videos: Object.entries(catalog).map(([id, entry]) => ({ id, ...entry })),
			})
		}
	)

	// Accept a pasted YouTube URL (or bare id), extract the id, resolve it.
	//
	// Registered BEFORE `/api/videos/:id`: the `:id` param route would otherwise
	// swallow the literal `lookup` path segment.
	.get(
		'/api/videos/lookup',
		describeRoute({
			tags: ['Videos'],
			summary: 'Lookup by URL',
			description:
				'Extract a YouTube video id from a watch / youtu.be / embed / shorts ' +
				'URL (or accept a bare id) and resolve it against the catalog.',
			responses: { 200: json(VideoLookup, 'The lookup result.') },
		}),
		async (c) => {
			const requestedUrl = c.req.query('url') ?? ''
			const id = extractYouTubeId(requestedUrl)
			const entry = id ? getCatalog(c.env)[id] : undefined
			return c.json({
				requestedUrl,
				video: entry ? { id: id as string, ...entry } : null,
			})
		}
	)

	// Resolve one id to its direct MP4 url.
	.get(
		'/api/videos/:id',
		describeRoute({
			tags: ['Videos'],
			summary: 'Resolve video',
			description:
				'Resolve a board video id (or YouTube id) to a direct playable MP4 URL. ' +
				'404 when the id is not in the catalog.',
			responses: {
				200: json(VideoEntry, 'The resolved video.'),
				404: json(VideoEntry, 'Unknown video id.'),
			},
		}),
		async (c) => {
			const id = c.req.param('id')
			const entry = getCatalog(c.env)[id]
			if (!entry) {
				return c.json({ id, title: '', url: '' }, 404)
			}
			return c.json({ id, ...entry })
		}
	)

	// Stable worker URL that 302s to the MP4 — hand THIS to VideoPlayer when the
	// board wants an indirection-stable url instead of the raw CDN url.
	.get(
		'/api/videos/:id/stream',
		describeRoute({
			tags: ['Videos'],
			summary: 'Stream redirect',
			description:
				'302 redirect to the direct MP4 for the given video id. Useful when the ' +
				'board wants a stable first-party URL to hand to the video player.',
			responses: {
				302: { description: 'Redirect to the direct MP4 URL.' },
				404: json(VideoEntry, 'Unknown video id.'),
			},
		}),
		async (c) => {
			const id = c.req.param('id')
			const entry = getCatalog(c.env)[id]
			if (!entry) {
				return c.json({ id, title: '', url: '' }, 404)
			}
			// Long-lived client-side cache: the mapping only changes on redeploy /
			// var change, and the target MP4 itself is immutable.
			return c.redirect(entry.url, 302)
		}
	)

// The generated spec. Documentation only — no request is validated against it (see
// openapi.ts). `hide: true` keeps this route out of its own output.
//
// Registered as a separate statement (not chained): `openAPIRouteHandler` needs
// `app` itself, and referencing `app` inside its own initializer chain is a
// use-before-declaration error. The handler walks `app.routes` at request time,
// so the spec still covers every route above.
app.get(
	'/openapi.json',
	describeRoute({ hide: true }),
	withCleanSpec(
		openAPIRouteHandler(app, {
			documentation: {
				info: {
					title: 'fluxrec videos',
					version: '0.1.0',
					description:
						'Direct playable MP4 URLs for the in-game video board. ' +
						'Unity VideoPlayer needs a direct media file URL, not a youtube.com/watch page.',
				},
			},
		})
	)
)

export default app
