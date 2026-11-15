# Deploy relay on Fly.io (replace Hostless)

Fly has **real logs** (`fly logs`), no blank UI, and this repo uses a **prebuilt** image so deploy does not compile .NET on the platform.

## One-time

1. [Sign up](https://fly.io/app/sign-up) (card may be required; small relay often stays within free allowance).
2. Install CLI: `winget install fly.io.flyctl` then `fly auth login`
3. Create app (first time only): from repo root:
   ```powershell
   fly apps create fluxrec-relay
   ```
   Or pick another name and set `app = '...'` in `fly.toml`.

## Deploy (every update)

```powershell
powershell -ExecutionPolicy Bypass -File relay-cloud\deploy-fly.ps1
```

That script:

- Publishes `TachyonServer` for `linux-x64`
- Sets secrets from `C:\Games\FluxRec\tachyon-server\` (same as Hostless)
- Runs `fly deploy`

## Secrets (manual)

```powershell
fly secrets set PLAYIT_SECRET_KEY="<from playit-secret.txt>" `
  TACHYON_SERVER_KEY_B64="<base64 of server-key.xml>" `
  TACHYON_PRESENCE_BASE="https://match.ripo-ripoteam.workers.dev"
```

## After green deploy

1. `fly logs` — you should see `[fluxrec] boot` and playit connected.
2. `fly status` — check `https://fluxrec-relay.fly.dev/health` (or your app name).
3. **Stop playit on the gaming PC** (one agent per secret).
4. GitHub variable `RELAY_KEEPALIVE_URL` = `https://<app>.fly.dev/health`
5. Redeploy match worker if the public UDP `host:port` from playit changed.

## Why not Hostless?

Repeated crash-on-deploy (wrong `playitd` binary, read-only FS, weak build logs). Fly + prebuilt Tachyon avoids Hostless’s Docker build OOM and empty log UI.