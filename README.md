# RRFlux — Private Rec Room Revival

> 🔒 **Private project.** Ripo Team internal only. Not for redistribution.
> Player access is application-based while in development.

RRFlux revives Rec Room from the archived **November 4, 2022** client
("For Local Eyes Only" edition, Build ID `9857464`) as a private,
community-run game.

## How it works

```
┌─────────────────┐      patched endpoints      ┌──────────────────┐
│  Patched client │ ──────────────────────────▶ │  RRFlux backend  │
│  (Nov 2022      │                              │  (our servers)   │
│   IL2CPP build) │ ◀────────────────────────── │                  │
└─────────────────┘      Photon relay            └──────────────────┘
        │                        (live multiplayer)
        │ downloads/updates via
        ▼
┌─────────────────┐
│   setup.exe     │  Custom installer/launcher (NSIS)
│   (launcher)    │  Installs game, handles updates + login
└─────────────────┘

Backend services:
- **Firebase** (`flux-544a6`) — auth, player profiles, saves, rooms metadata
- **Photon** — realtime multiplayer relay (player-hosted sessions)
```

Players install through our `setup.exe`. Game sessions are hosted on
player PCs. Firebase owns identity + persistence; Photon carries live
multiplayer traffic.

## Repo layout

| Path | What |
|---|---|
| `docs/architecture.md` | Full system design |
| `docs/client-patching.md` | How the client gets patched (tools + targets) |
| `firebase/` | Firestore rules + data model |
| `launcher/` | `setup.exe` installer source (NSIS) |
| `tools/` | Patching/utility scripts (coming) |

## Status

🚧 **Milestone 1 — in progress:** base build acquisition + client inspection
+ patching feasibility validation.

## Ground rules

- Never commit secrets (Photon App ID, Firebase private keys, tokens).
  Client-side Firebase web config is public by design; server credentials
  stay out of git.
- The archived client is a reference for patching research. Distribution
  of the client happens only through our own installer to approved players.
