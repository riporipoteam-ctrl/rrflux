# RRFlux edge translator (Cloudflare Worker) — FALLBACK

> **Status: fallback.** The primary design is now the **local translator**
> built into the launcher (`launcher/src-tauri/src/translator.rs`): it serves
> the same Rec Room API from `127.0.0.1:80` on the player's PC, so no cloud
> translator, no extra account, and no custom domain are needed. This Worker
> stays as a backup in case Windows testing shows the 2022 client refuses
> plain-HTTP localhost calls. Nothing here is deployed until someone sets
> `CLOUDFLARE_API_TOKEN`.

The Nov 2022 game client only speaks Rec Room's HTTP API. Firebase speaks
Firebase. Photon does multiplayer but not login. This Worker is the tiny
translator between them — and it runs on Cloudflare's free tier, so the
Firebase project stays on the free (Spark) plan. No Blaze, no billing.

## What it does

- `GET /Account/LoginWithToken?loginToken=…` — verifies the Firebase ID
  token (Google's public certs + Web Crypto, RS256) and returns the player
  session. This is what the patched client calls on launch.
- `/api/versioncheck/*` — tells the client it's up to date.
- `/api/config/*` — permissive remote-config defaults.
- `/api/players/v2/me` — player profile, derived from token claims
  (stateless MVP; Firestore lookup comes after traffic capture).
- `/api/sanitize/*` — text sanitization, pass-through on a private server.
- `/2/httpapi`, `/telemetry` — telemetry sink (the Amplitude host points here).
- `/v1/*` — launcher endpoints (manifest version; full manifest once game
  files are hosted).
- Everything else under `/api/` → graceful JSON 404. The client tolerates
  missing endpoints; we implement them as it proves it needs them.

## Deploy

Push to `main` — the `deploy-worker` GitHub workflow runs `wrangler deploy`.
Needs the `CLOUDFLARE_API_TOKEN` repo secret (free Cloudflare account, no card).

## Later

- Firestore REST integration for profiles/saves (after traffic capture).
- Custom domain (pairs with the short-hostname plan for the client patch).
