# RRFlux backend API design

The patched client and launcher talk to **our** API — we define this
surface. Host: `https://api.rrflux.example` (placeholder until infra lands).

## Auth model

- Players sign in with **Firebase Authentication** (email/password during
  the private phase; application-approved accounts).
- Every API call carries the Firebase **ID token**:
  `Authorization: Bearer <id-token>`.
- Server verifies the token with the Firebase Admin SDK, then maps it to
  the `players/{uid}` profile. No custom session cookies, no extra login
  system.

## Endpoints (v1)

### Launcher / updates
| Method | Path | Purpose |
|---|---|---|
| `GET` | `/v1/manifest` | Current build manifest: version, file list with sha256 + sizes + download URLs |
| `GET` | `/v1/manifest/version` | Lightweight version check (launcher polls this) |

### Profiles
| Method | Path | Purpose |
|---|---|---|
| `GET` | `/v1/profile/me` | Own profile |
| `PUT` | `/v1/profile/me` | Update display name etc. |
| `GET` | `/v1/profile/{uid}` | Another player's public profile |

### Saves
| Method | Path | Purpose |
|---|---|---|
| `GET` | `/v1/saves` | List own saves |
| `PUT` | `/v1/saves/{saveId}` | Write a save (small payloads inline; large → Storage URL flow) |
| `DELETE` | `/v1/saves/{saveId}` | Delete a save |

### Rooms (lobby)
| Method | Path | Purpose |
|---|---|---|
| `GET` | `/v1/rooms` | List open rooms (lobby) |
| `POST` | `/v1/rooms` | Host opens a room → returns `photonRoom` id |
| `DELETE` | `/v1/rooms/{roomId}` | Host closes a room (host-only) |

Live gameplay traffic goes over **Photon**, never through this API.

## Implementation

- **Firebase Cloud Functions** (Node.js) in project `flux-544a6`,
  fronted by Firebase Hosting rewrites (`/v1/**` → functions).
- Firestore is the store; Storage holds large saves/uploads.
- Rate-limit room creation per user (anti-spam); validate all input.

## Client mapping (to fill after client inspection)

| Old (stock client) | New (RRFlux) | Status |
|---|---|---|
| `https://*.rec.net/...` (auth) | Firebase Auth directly | ☐ confirm |
| `https://*.rec.net/...` (profile) | `GET /v1/profile/...` | ☐ confirm |
| `https://*.rec.net/...` (rooms) | `GET /v1/rooms` | ☐ confirm |
| Photon cloud endpoint | Our Photon App ID + region | ☐ confirm |

The patch script (`tools/patch.py`) will rewrite the left column to the
right column once the exact strings are catalogued from the build.
