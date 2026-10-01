# Flux Rec server — deploy checklist (RecFlare fork, workers.dev mode)

Target: 28 Cloudflare Workers at `<name>.<account>.workers.dev` — **no custom
domain, no Cloudflare zone purchase**. Fork changes vs upstream are in
`packages/tools/bin/run-wrangler-deploy` (workers.dev flag + git-less repo-root
lookup).

## 0. Prerequisites
- Cloudflare account (free tier works to start; the client is chatty — free
  100k req/day may run out, $5/mo Workers plan = 10M req/month).
- Node 24, pnpm, bun, `just`, `jq`; `wrangler` (ships via the repo's dev deps).
- This repo checked out; `just install` (or `pnpm install`) run once.

## 1. Authenticate wrangler (no browser login needed)
```sh
export CLOUDFLARE_API_TOKEN="<token with Workers/D1/KV/Secrets Store edit perms>"
export CLOUDFLARE_ACCOUNT_ID="<account id>"
```
(Alternative: `wrangler login`, but the token pair is what CI/headless uses.)

## 2. Create the shared resources (one-time)
```sh
wrangler d1 create recflare
wrangler kv namespace create RECFLARE_MATCH_PRESENCE
wrangler kv namespace create RECFLARE_PLAYER_SETTINGS
wrangler secrets-store store create recflare --scopes workers
# Blob store (Firestore): no buckets to create. Create a GCP service account
# with the Cloud Datastore User role on the Firestore project, then set its
# key JSON on every blob worker (api, img, cdn, rooms, storage, mono):
for w in api img cdn rooms storage mono; do
  (cd apps/$w && wrangler secret put FIRESTORE_SA_JSON)  # paste the SA JSON
done
```
Put the returned ids into the root `.env` (gitignored — never commit):
```sh
RECFLARE_DOMAIN=<account>.workers.dev     # your account's workers.dev subdomain
RECFLARE_D1=<d1 id>
RECFLARE_KV='{"RECFLARE_MATCH_PRESENCE":"<id>","RECFLARE_PLAYER_SETTINGS":"<id>"}'
RECFLARE_SECRETS_STORE=<store id>
RECFLARE_WORKERS_DEV=1                     # <-- the fork flag: skip --domain
```
Generate the JWT signing key and store it in the Secrets Store (required by every
worker for auth tokens):
```sh
openssl rand -hex 32
wrangler secrets-store secret create recflare JWT_SECRET --value "<hex>"
```

## 3. Photon (separate from Cloudflare)
Create **three** Photon Cloud apps (free tier ≈ 20 CCU each): Realtime, Voice,
Chat — or one app reused where the dashboard allows. Record the three App IDs;
they go into the shipped plugin config (step 6), NOT into the server `.env`.
The server hands them out via `GET /player/connection-info`
(`RECFLARE_PHOTON_REALTIME_APP_ID` / `_VOICE_` / `_CHAT_` knobs in `.env`).
Voice (Tachyon): no server implementation exists upstream — leave
`RECFLARE_TACHYON_HOST_PORT` unset; voice stays silent, the client tolerates it.

## 4. Migrate the database
```sh
just migrate        # applies D1 migrations (scope: just migrate -F rooms)
```

## 5. Deploy everything
```sh
RECFLARE_WORKERS_DEV=1 just deploy        # or export it once in .env
# single worker: just deploy -F ns
```
With `RECFLARE_WORKERS_DEV=1`, `run-wrangler-deploy` skips `--domain`, so each
worker lands at `<name>.<account>.workers.dev`. Sanity check afterwards:
`curl https://ns.<account>.workers.dev/` must return the discovery JSON with
every label → `https://<sub>.<account>.workers.dev`. The math holds because
**every worker's `name` in `apps/*/wrangler.jsonc` equals its directory name**
(verified for all 28 apps) and `apps/ns/src/endpoints.ts` advertises
`https://<subdomain>.<domain>` with `DOMAIN=<account>.workers.dev`.

## 6. Rules for workers.dev mode
- **Do NOT set `RECFLARE_SUBDOMAINS` overrides for services that have workers.**
  The override changes the deploy lookup AND the advertised host, but wrangler
  still deploys to `<name>.workers.dev` — advertised and deployed hosts would
  drift apart. Only pure client-side redirects are safe, e.g.
  `RECFLARE_SUBDOMAINS='{"moderation":"api"}'` (moderation has no worker; the
  `api` worker is at `api.<account>.workers.dev` either way).
- After changing `RECFLARE_SUBDOMAINS`, redeploy `ns` (`just deploy -F ns`).
- Never set vars in the Cloudflare dashboard — each deploy replaces a worker's
  vars wholesale. `.env` is the durable place.

## 7. Wire the client to it
Ship `BepInEx/config/net.rec.plugin.cfg` with:
```ini
[Server]
RecNet NameServer Host = https://ns.<account>.workers.dev
[Photon]
App Id Realtime = <your realtime app id>
App Id Voice = <your voice app id>
App Id Chat = <your chat app id>
```
The game fetches `https://ns.<account>.workers.dev/` first and learns every
other service host from there — one address repoints the whole stack.
