# RRFlux Firebase backend

> **Superseded (2026-09-19):** the Cloud Functions below need the Blaze plan,
> so the live translator is now the Cloudflare Worker in `../worker/`
> (free tier, no billing). This directory keeps the Firestore rules
> (`firestore.rules`, still deployed and live) and the original function
> sources for reference.

Two Cloud Functions (Node.js 20) + Firestore rules + Hosting rewrites.

| Function | Source | Handles |
|---|---|---|
| `loginWithToken` | `functions/auth` | `GET /Account/LoginWithToken` — the patched game's auth call. Verifies the Firebase ID token (from the launcher), ensures `players/{uid}`, returns a session token. |
| `api` | `functions/api` | Express app: `/api/*` (game client, Rec Room-compatible surface) + `/v1/*` (launcher: manifest, version, profile). Unimplemented game endpoints 404 gracefully. |

## Deploy — option A: Firebase CLI

```bash
cd firebase
npm --prefix functions/auth install
npm --prefix functions/api install
firebase use flux-544a6   # or: firebase use --add
firebase deploy --only functions,firestore:rules
```

Needs the Blaze plan (Cloud Functions requirement).

## Deploy — option B: Cloud console inline editor

1. [Cloud Functions](https://console.cloud.google.com/functions) → Create function → 2nd gen.
2. Name `loginWithToken`, region matching Firestore, HTTPS trigger, paste `functions/auth/index.js` (+ add `firebase-admin`, `firebase-functions` deps), deploy.
3. Same for `api` with `functions/api/index.js` (+ `express`).
4. Firestore → Rules tab → paste `firestore.rules` → Publish.
5. Hosting rewrites (custom short domains) come later with the domain.

## Custom domains (pending)

- API: `rrflux.gg` (7 chars — fits the ≤10 `ns.rec.net` slot)
- Auth: `auth.rrflux.gg` (12 chars — fits the ≤12 `auth.rec.net` slot)
- Add both as Firebase Hosting custom domains, then set the Hosting
  rewrites from `firebase.json`. Requires buying the domain + DNS.

## Notes

- Response shapes for the game client are best-effort until we capture
  real client traffic (Windows + mitmproxy). Shapes are centralized and
  easy to tweak.
- The launcher signs users in with Firebase Auth (email/password) and
  passes the ID token to the game as `loginToken`.
