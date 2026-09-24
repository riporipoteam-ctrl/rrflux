/**
 * In-memory fake of the Firestore REST surface `@repo/blob-store` uses.
 *
 * Loaded by integration-test miniflare configs (via `scriptPath`) and bound to the
 * app under test as the `FIRESTORE_TEST_BACKEND` service binding. The blob store
 * routes every Firestore REST call through it and skips the OAuth2 flow, so tests
 * exercise the real REST layer (batchWrite / document GET / deletes, chunk
 * layout, base64 payloads) with no network access.
 *
 * Supported:
 * - `POST /v1/projects/{p}/databases/(default)/documents:batchWrite`
 *   `{ writes: [{ update: { name, fields } } | { delete: name }] }`
 * - `GET  /v1/projects/{p}/databases/(default)/documents/{docPath…}`
 * - `POST /__reset` — clear all stored documents (test isolation; call it through
 *   the same binding: `env.FIRESTORE_TEST_BACKEND.fetch('https://firestore.test/__reset',
 *   { method: 'POST' })`).
 *
 * This file is plain JavaScript on purpose: miniflare loads it directly.
 */

const docs = new Map()

function notFound() {
	return Response.json(
		{ error: { code: 404, message: 'Not found', status: 'NOT_FOUND' } },
		{ status: 404 }
	)
}

export default {
	async fetch(request) {
		const url = new URL(request.url)

		if (url.pathname === '/__reset' && request.method === 'POST') {
			docs.clear()
			return Response.json({ ok: true })
		}

		const m = url.pathname.match(
			/^\/v1\/projects\/([^/]+)\/databases\/\(default\)\/documents(?::(batchWrite))?(?:\/(.+))?$/
		)
		if (!m) return notFound()
		const projectId = m[1]
		const action = m[2]
		const docPath = m[3]

		if (request.method === 'POST' && action === 'batchWrite') {
			let body
			try {
				body = await request.json()
			} catch {
				return Response.json({ error: { code: 400, message: 'Bad JSON' } }, { status: 400 })
			}
			const now = new Date().toISOString()
			const writeResults = []
			for (const w of body.writes ?? []) {
				if (w.update && typeof w.update.name === 'string') {
					docs.set(w.update.name, { fields: w.update.fields ?? {}, updateTime: now })
					writeResults.push({ updateTime: now })
				} else if (w.delete && typeof w.delete === 'string') {
					docs.delete(w.delete)
					writeResults.push({})
				} else {
					writeResults.push({ status: { code: 3, message: 'Unsupported write' } })
				}
			}
			return Response.json({ writeResults, status: [{}] })
		}

		if (request.method === 'GET' && docPath) {
			const name = `projects/${projectId}/databases/(default)/documents/${docPath}`
			const doc = docs.get(name)
			if (!doc) return notFound()
			return Response.json({
				name,
				fields: doc.fields,
				createTime: doc.updateTime,
				updateTime: doc.updateTime,
			})
		}

		return Response.json(
			{ error: { code: 400, message: 'Unsupported operation', status: 'INVALID_ARGUMENT' } },
			{ status: 400 }
		)
	},
}
