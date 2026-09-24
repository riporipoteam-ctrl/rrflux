# videos worker

Serves direct playable MP4 URLs for the in-game video board.

NS discovery advertises this worker to the 2023 client as `Videos`
(`https://videos.<domain>`). Unity's VideoPlayer cannot play a
`youtube.com/watch` page — it needs a DIRECT media file URL — so this worker
maps board video ids (or YouTube ids) to direct MP4 URLs.

## Routes

- `GET /` — liveness probe (`{ service: 'videos', status: 'ok' }`)
- `GET /openapi.json` — OpenAPI spec
- `GET /api/videos` — full catalog: `[{ id, title, url }]`
- `GET /api/videos/:id` — resolve one id → `{ id, title, url }` (404 when unknown)
- `GET /api/videos/:id/stream` — 302 redirect to the direct MP4 (hand this stable
  first-party URL to the video player instead of the raw CDN URL)
- `GET /api/videos/lookup?url=<youtube-url-or-id>` — extract a YouTube id from a
  watch / youtu.be / embed / shorts URL (or bare id) and resolve it

## Configuring the catalog

The catalog is the built-in map in `src/videos.app.ts` (Google's public sample
MP4s — placeholders) overridden by the `VIDEOS_MAP` var, a JSON object mapping
id → url or id → `{ title, url }`:

```json
{
	"dQw4w9WgXcQ": "https://cdn.example.com/videos/never-gonna.mp4",
	"board-intro": { "title": "Board intro", "url": "https://cdn.example.com/videos/intro.mp4" }
}
```

Set it in `wrangler.jsonc` vars (or via the deploy env). Once traffic capture
reveals the Rec Center board's real video ids, point each at a hosted MP4.

## What this worker does NOT do

YouTube stream extraction. There is no free/supported way to extract YouTube
streams inside a Worker, and `googlevideo.com` URLs are signed per-IP and
expire. Map each YouTube id to a directly-hosted MP4 instead.
