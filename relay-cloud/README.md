# Flux Rec relay (cloud image)

Always-on **Tachyon + playit** for worldwide multiplayer. Match + auth stay on Cloudflare
Workers 24/7; this container is only the UDP relay.

## One-click hosts (no credit card on many tiers)

| Host | RAM (free) | Always-on free? |
|------|------------|-------------------|
| [Hostless](https://hostless.cloud) | 1 GB | Yes |
| [Deplexo](https://deplexo.com) | 128 MB (tight) | Yes |
| [Omeganodes](https://omeganodes.com) | 256 MB | Best-effort |
| [Botkeep](https://botkeep.cloud) | 512 MB | Yes |

GitHub Actions publishes `ghcr.io/riporipoteam-ctrl/fluxrec-relay:latest` (see workflow).

### Env vars on the host

- `PLAYIT_SECRET_KEY` — from `playit-secret.txt` (same tunnel as today)
- `TACHYON_SERVER_KEY_B64` — already in GitHub `TACHYON_SERVER_KEY_B64`
- `TACHYON_PRESENCE_BASE` — `https://match.ripo-ripoteam.workers.dev`

After the cloud agent is up, **stop playit on your gaming PC** (one agent per secret).

`RECFLARE_TACHYON_HOST_PORT` stays `147.185.221.214:17346` if the tunnel name is unchanged.