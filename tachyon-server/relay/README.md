# Flux Rec public relay (24/7 multiplayer)

**GitHub Actions cannot run your game server while your PC is asleep.** Workflows run for
minutes and stop; they are not a free UDP host. Cloudflare Workers are the always-on **match
API** — the **Tachyon + playit** pair must run on a machine that stays on.

## What actually stays up (free, no credit card)

| Option | Cost | PC can sleep? |
|--------|------|----------------|
| Your gaming PC + playit | $0 | No — relay dies with the PC |
| **Hugging Face Space `Echoxr/fluxrec-relay`** | HF **Pro** (~$9/mo) or org | **Yes** (hosted on HF) |
| **Self-hosted GitHub runner + `start-relay.ps1`** | $0 | While **PC is on** — see `install-github-runner.md` |
| Oracle Cloud Always Free VM | $0* (*card often required) | Yes |
| Home Raspberry Pi / old laptop | ~$0 power | Yes, if *that* box stays on |
| GitHub `relay-health` workflow | $0 | Monitors only; does **not** host |

### Fully automated HF path (you already have Echoxr)

1. Repo secrets (CI sets most of this once from your PC):

   ```powershell
   powershell -ExecutionPolicy Bypass -File tachyon-server\relay\publish-github-secrets.ps1
   ```

   Needs `HF_TOKEN` (write) and `PLAYIT_SECRET_KEY` in GitHub already.

2. Push workflows to `main` (or add them in GitHub UI).

3. Run **Actions → Deploy HF relay (24/7)** once.

4. **Stop playit on your gaming PC** so only the Space agent holds the tunnel.

5. `hf-relay-keepalive` pings the Space every 8 minutes so free `cpu-basic` stays awake.

Space URL: https://huggingface.co/spaces/Echoxr/fluxrec-relay

Players worldwide still use **Workers** + **`RECFLARE_TACHYON_HOST_PORT`**; only **one**
machine must keep playit connected to the same tunnel.

## Quick start — free Oracle VM (≈15 minutes one-time)

1. Create an [Oracle Cloud](https://www.oracle.com/cloud/free/) account and an **Ampere A1**
   Ubuntu 22.04 instance (Always Free shape is enough).
2. Open **UDP** in the instance firewall / security list if you later expose a port directly.
   With **playit**, outbound-only is fine — the tunnel is outbound from the VM.
3. SSH into the VM and run:

```bash
curl -fsSL https://raw.githubusercontent.com/riporipoteam-ctrl/rrflux/main/tachyon-server/relay/bootstrap-ubuntu.sh | bash
```

4. Copy secrets from your operator install (never commit these):

```bash
# From your PC (paths may differ)
scp C:/Games/FluxRec/tachyon-server/server-key.xml ubuntu@YOUR_VM:/opt/fluxrec-relay/secrets/
scp C:/Games/FluxRec/tachyon-server/playit-secret.txt ubuntu@YOUR_VM:/opt/fluxrec-relay/secrets/
```

5. On the VM:

```bash
cd /opt/fluxrec-relay
docker compose up -d --build
```

6. **Stop playit on your gaming PC** so only the VM agent uses the account secret
   (two agents fighting = flapping tunnel).

7. Confirm the public address (playit keeps the same `host:port` if the tunnel name is
   unchanged). Update GitHub secret if it changed:

   `Settings → Secrets → RECFLARE_TACHYON_HOST_PORT` → `ip:port`

8. Run **Deploy Workers** (or push match) so Cloudflare picks up the address.

## Repo automation

| Workflow | What it does |
|----------|----------------|
| [**Build Relay (Linux)**](../../.github/workflows/build-relay-linux.yml) | Builds Tachyon for `linux-x64` and packs the relay image context. |
| [**Relay health**](../../.github/workflows/relay-health.yml) | Every 20 minutes: checks playit API (`PLAYIT_SECRET_KEY`) + match `/tachyon/shard`. Fails → GitHub emails you. |
| [**Relay deploy (VPS)**](../../.github/workflows/relay-deploy-vps.yml) | Manual: `docker compose pull/build/up` over SSH if `RELAY_SSH_*` secrets are set. |

Optional secrets for deploy/restart:

- `RELAY_SSH_HOST` — `user@1.2.3.4`
- `RELAY_SSH_KEY` — private key (ed25519)
- `RELAY_SSH_PATH` — default `/opt/fluxrec-relay`

## Windows (while PC is on)

`start-relay.ps1` + Task Scheduler (logon) — see `DEPLOY_CHECKLIST.md`. Use this for dev;
use the Linux VM for tomorrow’s public window.

## `relay-health` is not a host

It is a **smoke alarm**: tunnel inactive → failed workflow → you fix the VM or agent.
For true “I slept, game still works,” run the Docker stack on an **always-on free VM**.