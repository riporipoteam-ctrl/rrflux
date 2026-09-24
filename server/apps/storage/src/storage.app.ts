import { Hono } from 'hono'
import { describeRoute, openAPIRouteHandler } from 'hono-openapi'
import { useWorkersLogger } from 'workers-tagged-logger'

import { BlobStoreNotConfiguredError, putBlob } from '@repo/blob-store'
import {
	intVar,
	withCleanSpec,
	withDefaultCors,
	withNotFound,
	withOnError,
} from '@repo/hono-helpers'
import { validateAndGetAccountId } from '@repo/jwt'

import {
	AUTHED,
	ErrorResponse,
	json,
	text,
	UNAUTHORIZED_RESPONSE,
	UPLOAD_REQUEST_BODY,
	UploadResponse,
} from './openapi'

import type { App } from './context'

/**
 * Storage worker. Handles client file uploads (`POST /upload`) into the shared
 * `CDN_ASSETS` blob store (Firestore), foldered by the posted `FileType` so the
 * `cdn` worker can serve them back.
 */

/**
 * The client's `UploadFileType` enum → the blob-store subfolder uploads of that
 * type are stored under. `Unknown` (0) is intentionally absent: like the referencecdn
 * server's `makeUploadName`, an unrecognized type has no destination and is
 * rejected rather than stored.
 *
 * RoomSave (1) lands under `room/` (not `roomsave/`) so the `cdn` worker's
 * `GET /room/:dataBlob` route serves the blob back — both address the same
 * `CDN_ASSETS` namespace, so the key prefixes must match.
 */
const UPLOAD_SUBFOLDER: Record<number, string> = {
	1: 'room',
	2: 'data',
	3: 'image',
	4: 'video',
	5: 'invention',
	6: 'roommetadata',
}

/** Resolve the storage subfolder for a posted FileType, or `undefined` when unknown. */
function subfolderForFileType(fileType: string): string | undefined {
	return UPLOAD_SUBFOLDER[Number.parseInt(fileType, 10)]
}

/**
 * The file extension an upload of a given type keeps. Invention data blobs are
 * named `<name>.inv` — the client expects the extension on the `BlobName` it later
 * gets back from the api worker, so it has to be part of the stored key too, or the
 * blob wouldn't be there to download. Other types are stored under a bare name.
 */
const UPLOAD_EXTENSION: Record<number, string> = {
	5: '.inv',
}

/**
 * A Worker must not accept an unbounded user-controlled blob into memory and the
 * blob store. Operators can tune this for known room sizes, but an unset or
 * invalid value keeps the 64 MiB default — room saves and invention data can be
 * large, and this is the ceiling the client already works with. Non-positive
 * values are invalid rather than disabling the limit: a public upload endpoint
 * must always have a finite ceiling.
 *
 * NOTE (Firestore quota): blobs are stored chunked (≤716800 bytes per document),
 * so a 64 MiB upload costs ~93 document writes — nearly 0.5% of the Spark plan's
 * 20,000 writes/day — and ~85 MiB of stored documents against the 1 GiB free
 * quota. If uploads start threatening the daily quota, lower this (it is
 * overridable per-deployment via MAX_UPLOAD_BYTES) rather than letting the
 * backend starve. The `api` worker's image cap is deliberately smaller — see
 * apps/api/src/upload-limit.ts.
 */
const DEFAULT_MAX_UPLOAD_BYTES = 64 * 1024 * 1024

function maxUploadBytes(value: unknown): number {
	const configured = intVar(value, DEFAULT_MAX_UPLOAD_BYTES)
	return configured > 0 ? configured : DEFAULT_MAX_UPLOAD_BYTES
}

function extensionForFileType(fileType: string): string {
	return UPLOAD_EXTENSION[Number.parseInt(fileType, 10)] ?? ''
}

/** Read a text form field by any of its accepted names, matched case-insensitively. */
function textField(body: Record<string, unknown>, ...names: string[]): string | undefined {
	for (const [key, value] of Object.entries(body)) {
		if (typeof value === 'string' && names.includes(key.toLowerCase())) return value
	}
	return undefined
}

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
	// The game posts here with no Origin at all, but the website does too — it uploads a
	// subroom's scene blob straight from the browser, the same way it calls `rooms` and
	// `accounts` directly. An `Authorization` header makes that a preflighted request, so
	// without this the OPTIONS gets a 404 and the upload never leaves the page.
	.use('*', withDefaultCors())

	.onError(withOnError())
	.notFound(withNotFound())

	.get(
		'/',
		describeRoute({
			tags: ['Meta'],
			summary: 'Health check',
			description: 'Plain-text liveness probe. No auth.',
			responses: { 200: text('Service is up (`hello, world!`)') },
		}),
		async (c) => {
			return c.text('hello, world!')
		}
	)

	// File upload. Auth-gated — any valid account token is allowed (no role check).
	// Multipart form with `FileType` (the client's UploadFileType enum) and a binary
	// part. Stores the file in the shared `CDN_ASSETS` blob store under
	// `<type-subfolder>/<upload-date>/<random-name>` and returns the generated name
	// (the `<upload-date>/<random-name>` part the `cdn` worker serves back) the client
	// references it by. Also accepts a name-only post (no binary) that just echoes
	// back an explicit `name`/`filename`/`imagename`. Mirrors the reference `Upload`.
	.post(
		'/upload',
		describeRoute({
			tags: ['Upload'],
			summary: 'Upload a file',
			description: [
				'Stores the posted file in the shared `CDN_ASSETS` blob store under',
				'`<type-subfolder>/<upload-date>/<random-name>` and returns the',
				'`<upload-date>/<random-name>` part the client references it by (the same name the',
				'`cdn` worker serves back). The subfolder comes from `FileType`; RoomSave (1) lands',
				'under `room/` so the cdn worker’s `GET /room/:dataBlob` finds it, and an Invention',
				'(5) keeps a `.inv` extension on both the key and the returned name. Auth-gated —',
				'any valid account token is allowed, no role check. A post with no binary part but',
				'an explicit `imageName` / `filename` / `name` just echoes that name back. Mirrors',
				'the reference server’s `Upload`.',
			].join(' '),
			security: AUTHED,
			requestBody: UPLOAD_REQUEST_BODY,
			responses: {
				200: json(UploadResponse, 'The stored (or echoed) file name'),
				400: json(ErrorResponse, 'Unknown/missing FileType, or neither a file nor a name'),
				401: UNAUTHORIZED_RESPONSE,
				413: json(ErrorResponse, 'The binary file exceeds the configured upload limit'),
				503: json(ErrorResponse, 'Blob storage is not configured on this deployment'),
			},
		}),
		async (c) => {
			const id = await validateAndGetAccountId(c.req.raw, await c.env.JWT_SECRET.get())
			if (id === null) return c.body(null, 401)

			const body = await c.req.parseBody().catch(() => ({}) as Record<string, unknown>)

			// The binary part is identified by being a file (filename/content-type),
			// not by its field name — matching the reference's part detection.
			const file = Object.values(body).find((v): v is File => v instanceof File)

			if (file) {
				const limit = maxUploadBytes(c.env.MAX_UPLOAD_BYTES)
				if (file.size > limit) {
					return c.json({ error: `file exceeds the ${limit}-byte upload limit` }, 413)
				}

				const fileType = textField(body, 'filetype') ?? '0'
				const subfolder = subfolderForFileType(fileType)
				if (subfolder === undefined) {
					// makeUploadName == "" → no destination for an unknown/missing type.
					return c.json({ error: 'missing or unknown FileType' }, 400)
				}
				// Folder each upload under its date (e.g. `room/2026-02-03/<uuid>`) so the
				// bucket stays browsable. The date is part of the returned name, so the key
				// the `cdn` worker reads back (`<subfolder>/<name>`) still round-trips — as
				// does the extension, which is why it goes on the key, not just the name.
				const datePrefix = new Date().toISOString().slice(0, 10)
				const filename = `${datePrefix}/${crypto.randomUUID()}${extensionForFileType(fileType)}`
				const bytes = await file.arrayBuffer()
				try {
					await putBlob(c.env, 'CDN_ASSETS', `${subfolder}/${filename}`, bytes, {
						contentType: file.type || 'application/octet-stream',
						// Record the SHA-256 on the blob's metadata doc. The hashes the client
						// is served (an invention's `BlobHash`) are SHA-256, and only a hash
						// given at put time is readable later — this lets the `api` worker
						// answer one from a metadata read instead of downloading the blob to
						// digest it.
						sha256: await crypto.subtle.digest('SHA-256', bytes),
					})
				} catch (e) {
					// The blob store has no credentials configured — a deployment problem,
					// not a client one.
					if (e instanceof BlobStoreNotConfiguredError) {
						return c.json({ error: 'blob storage is not configured' }, 503)
					}
					throw e
				}
				return c.json({ filename })
			}

			// No binary — accept an explicit name and echo it straight back.
			const explicitName = textField(body, 'imagename', 'filename', 'name')
			if (explicitName) return c.json({ filename: explicitName })

			return c.json({ error: 'missing filename or valid upload data' }, 400)
		}
	)

// The generated spec. Documentation only — no request is validated against it (see
// openapi.ts). `hide: true` keeps this route out of its own output.
app.get(
	'/openapi.json',
	describeRoute({ hide: true }),
	withCleanSpec(
		openAPIRouteHandler(app, {
			documentation: {
				info: {
					title: 'recflare storage',
					version: '1.0.0',
					description: [
						'File uploads for recflare, a private-server reimplementation of the Rec Room',
						'backend. The client posts room saves, holotars, images, videos, inventions and',
						'room metadata here; each lands in the shared `CDN_ASSETS` blob store under a folder chosen',
						'by its `FileType`, and the `cdn` worker serves them back from the same bucket.',
					].join('\n'),
				},
				servers: [{ url: 'https://storage.recflare.net', description: 'Production' }],
				components: {
					securitySchemes: {
						bearerAuth: {
							type: 'http',
							scheme: 'bearer',
							bearerFormat: 'JWT',
							description: 'An `access_token` from the auth worker’s `POST /connect/token`.',
						},
					},
				},
			},
		})
	)
)

export default app
