# Flux Rec Admin API — reference for RipoBot

Base URL: `https://api.ripo-ripoteam.workers.dev`

All endpoints live under `/api/admin/v1/*` on the **api** worker. They are gated
by a shared admin key — **not** by the user JWT:

- Header: `X-Admin-Key: <key>`
- Key location: `~/workspace/goals/private-rec-room-revival-build/fluxrec/server/.env`
  → `RECFLARE_ADMIN_API_KEY` (gitignored; the deployed worker has it as the
  `ADMIN_API_KEY` env var)
- Missing/wrong key → `401 {"success":false,"error":"unauthorized"}` (identical
  either way; the key is never logged)

All request bodies are JSON. Usernames are the players' in-game usernames
(case-insensitive lookup). Error shape on 4xx: `{"success":false,"error":"..."}`.

## Endpoints

### Set / remove rank
`POST /api/admin/v1/ranks/set`
```json
{ "username": "Ripo6000", "rank": "community_mod" }
```
`rank` is one of `community_mod` | `developer` | `none`.
Sets the account's moderator/developer flags; `none` clears both.
Takes effect on the player's **next login**.
Response: `{ "success": true, "username", "accountId", "rank", "isModerator", "isDeveloper" }`

### Grant / remove Flux Rec+ membership
`POST /api/admin/v1/membership/set`
```json
{ "username": "Ripo6000", "duration_months": 3 }
```
`duration_months`: positive integer = months of Plus (30 days each);
`0` = never expires; `-1` = **remove** membership.
Takes effect on the player's **next login**.
Response: `{ "success": true, "username", "accountId", "hasPlus", "plusSince", "plusUntil" }`
(`plusUntil` is `null` when removed.)

### Ban a player
`POST /api/admin/v1/bans/create`
```json
{ "username": "SomeTroll", "reason": "Cheating in paintball", "duration_minutes": 1440, "voice_ban": false }
```
`duration_minutes`: `0` = **permanent**; positive = timed ban (auto-lifts).
`reason` (required, ≤ 500 chars) is shown to the player **in-game** on the ban
screen, verbatim. `voice_ban` (optional, default false) additionally mutes the
player's voice chat until the same expiry.
Response: `{ "success": true, "username", "accountId", "reportId", "permanent",
"banExpires", "voiceBanned", "voiceBanUntil" }`
(`banExpires`/`voiceBanUntil` are `null` for permanent.)

### Lift a ban (and any voice ban)
`POST /api/admin/v1/bans/lift`
```json
{ "username": "SomeTroll" }
```
Response: `{ "success": true, "username", "accountId", "lifted": true }`
(`lifted: false` if the player wasn't banned.)

### Grant tokens
`POST /api/admin/v1/tokens/grant`
```json
{ "username": "Ripo6000", "amount": 1000 }
```
or, for everyone:
```json
{ "grant_to": "everyone", "amount": 1000 }
```
`amount`: positive integer ≤ 1,000,000,000. New accounts keep their 500-token
signup grant — the everyone-grant never swallows it.
Response (single): `{ "success": true, "grantedTo": "<username>", "accountId", "amount", "newBalance" }`
Response (everyone): `{ "success": true, "grantedTo": "everyone", "accounts": <n>, "amount" }`

### Online players
`GET /api/admin/v1/players/online`
Response: `{ "success": true, "count": 3, "players": [
  { "accountId": 12, "username": "Ripo6000", "room": "Dorm Room", "roomId": 5 }
] }`
(`room` may be `null` if the player is between rooms.)

## Notes for bot commands
- Rank and membership changes take effect on **next login** — tell the player to
  re-log if they're online.
- Bans apply immediately to matchmaking (banned players can't join rooms except
  their dorm); the ban screen shows the exact `reason` text.
- Voice bans strip the player's voice-server connection info, so they can't
  hear/speak in voice chat until it expires or is lifted.
- The RipoBot commands should be Owner/Co-Owner-gated — this key can ban
  anyone and grant unlimited tokens, so keep it out of the bot's repo.
