# Firebase data model (`flux-544a6`)

## Collections

### `players/{uid}`
Player profile. Created on first sign-in (application-approved accounts).
| Field | Type | Notes |
|---|---|---|
| `displayName` | string | chosen by player |
| `createdAt` | timestamp | |
| `lastSeen` | timestamp | updated on login |
| `role` | string | `player` / `moderator` / `admin` |

### `players/{uid}/saves/{saveId}`
Per-player game saves. Owner-only read/write.
| Field | Type | Notes |
|---|---|---|
| `data` | map/blob ref | save payload (large saves → Storage, ref here) |
| `updatedAt` | timestamp | |

### `rooms/{roomId}`
Ephemeral session metadata. Created by the host when a room opens,
deleted/expired when the session ends. Photon carries the live traffic;
this is just the lobby listing.
| Field | Type | Notes |
|---|---|---|
| `hostUid` | string | Firebase UID of the host |
| `name` | string | room name |
| `mapId` | string | which level/scene |
| `playerCount` | number | current players |
| `maxPlayers` | number | |
| `photonRoom` | string | Photon room identifier |
| `createdAt` | timestamp | |
| `expiresAt` | timestamp | TTL for stale rooms |

## Auth
Firebase Authentication. Sign-up is application-based during the private
phase (manual approval → account creation). No anonymous auth.

## Notes
- Firestore is the system of record, **not** the realtime layer.
- Secrets (service-account keys, Photon App ID) never live in this repo.
