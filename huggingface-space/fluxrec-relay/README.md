---
title: Flux Rec Public Relay
emoji: 🎮
colorFrom: indigo
colorTo: blue
sdk: docker
app_port: 7860
pinned: true
license: mit
short_description: 24/7 Tachyon + playit relay for Flux Rec multiplayer
---

# Flux Rec relay (Docker Space)

Runs **Tachyon** (UDP 7777) and the **playit** agent so worldwide players can connect while
the operator PC is off. Match uses the same public `host:port` from
`RECFLARE_TACHYON_HOST_PORT`.

**Secrets** (Space settings — set automatically by GitHub `deploy-hf-relay`):

- `PLAYIT_SECRET_KEY` — playit agent key (64 hex)
- `TACHYON_SERVER_KEY_B64` — base64 of `server-key.xml`
- `TACHYON_PRESENCE_BASE` — e.g. `https://match.ripo-ripoteam.workers.dev`

This Space exposes **HTTP 7860** only for health/keepalive; game UDP goes through playit.

Operator docs: [`tachyon-server/relay/README.md`](../../tachyon-server/relay/README.md)