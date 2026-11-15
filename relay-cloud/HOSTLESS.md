# Deploy Flux Rec relay on Hostless (24/7, no gaming PC)

## Dashboard (one-time)

1. [Hostless dashboard](https://hostless.cloud/) → **Apps** → **Create app**
2. Connect **GitHub** → install app on **`riporipoteam-ctrl/rrflux`**
3. **Repository:** `riporipoteam-ctrl/rrflux`
4. **Branch:** `main` (or `relay-cloud-deploy` until merged)
5. **Working directory:** `/` (repo root — `hostless.yaml` uses `relay-cloud/Dockerfile`)
6. **Build system:** Docker (reads `hostless.yaml`)
7. **Resources:** ≥ **256 MB** RAM (512 MB safer for .NET + playit)
8. **Environment variables** (app settings → secrets):

| Name | Source |
|------|--------|
| `PLAYIT_SECRET_KEY` | GitHub secret / `C:\Games\FluxRec\tachyon-server\playit-secret.txt` |
| `TACHYON_SERVER_KEY_B64` | GitHub `TACHYON_SERVER_KEY_B64` |
| `TACHYON_PRESENCE_BASE` | `https://match.ripo-ripoteam.workers.dev` |

Do **not** set `PORT` — Hostless sets it.

9. **Health check path:** `/health` (already in `hostless.yaml`)

Runtime writes only under **`/tmp/fluxrec`** and **`/app/.cache`** (app dir is read-only). playit runs as **`playitd --platform-docker`** (not `playit run`).

## After first green deploy

1. Open app logs — confirm playit connected.
2. **Stop playit + Tachyon on the gaming PC** (one agent per secret).
3. Confirm public tunnel still `147.185.221.214:17346` (or update `RECFLARE_TACHYON_HOST_PORT` + Deploy Workers).
4. GitHub repo variable `RELAY_KEEPALIVE_URL` = your `https://<app>.hostless.app/health`

## Local helper (prints names, does not commit secrets)

```powershell
powershell -ExecutionPolicy Bypass -File tachyon-server\relay\print-hostless-env.ps1
```