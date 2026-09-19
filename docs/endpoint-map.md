# Endpoint map — Nov 4 2022 client (Build 9857464)

Extracted via `strings` from `GameAssembly.dll` (137 MB, PE32+ x86-64)
and `RecRoom_Data/il2cpp_data/Metadata/global-metadata.dat` (29 MB).
Endpoint strings live in the **metadata**, not the native DLL.

## Auth
| Endpoint | Purpose |
|---|---|
| `https://auth.rec.net/Account/LoginWithToken?loginToken=` | Login with token |

## API base
`https://ns.rec.net` — main API host. Paths (all under `/api/`):

| Path | Feature |
|---|---|
| `/api/versioncheck/v4` | Client version check |
| `/api/config/v1/...` | Remote config (feature flags, gift button, …) |
| `/api/players/v2/...` | Player profiles, progression |
| `/api/relationships/...` | Friends (send/accept) |
| `/api/roomkeys/v1/...` | Room keys (create/mine) |
| `/api/messages/v2/...` | Messages, friend online status |
| `/api/images/v5/...` | Images (bulk) |
| `/api/inventions/v1/...` | Inventions, dorm skins |
| `/api/storefronts/v1/...` | Store purchases (room keys, currency) |
| `/api/avatar/v1/...` | Avatar items |
| `/api/userreporting/...` | Reports |
| `/api/playerevents/...`, `/api/playerReputation/...` | Events, reputation |
| `/api/sanitize/...` | Text sanitization |
| `/api/ugcPurchasables` | UGC purchases |

Plus `https://rec.net/` and `https://rec.net/shop?utm_source=pc` (web/shop).

## Multiplayer (Photon)
- Full Photon stack embedded: **PUN**, **Realtime**, **Chat**, **Voice**
  (`ExitGames.Client.Photon`, `Photon.Pun`, `Photon.Realtime`, …)
- App IDs are **settable fields**: `AppIdRealtime`, `AppIdVoice`,
  `AppIdChat` (with `SetAppID`/`get_AppID` accessors)
- Default name servers: `ns.exitgames.com`, `ns.exitgamescloud.com`
  → with our App ID the client uses **Photon Cloud**, no custom server needed

## Telemetry (neuter/redirect)
- `https://api2.amplitude.com/httpapi`, `/identify` — Amplitude analytics

## Anti-cheat surface
- **EasyAntiCheat** ships as files (`EasyAntiCheat_Setup.exe`, x64/x86 DLLs)
- **CodeStage Anti-Cheat Toolkit** embedded in the client
  (InjectionDetector, SpeedHackDetector, WallHackDetector, …)
- Both must be neutered/stubbed — EAC's servers are gone anyway.

## Patching verdict: ✅ feasible
1. All server endpoints are **plain strings in the IL2CPP metadata** —
   exactly what the Metadata String Editor workflow targets.
2. Photon App IDs are settable fields — inject ours at patch time.
3. Constraints: replacement hostnames must fit the original string
   allocations (equal-or-shorter, or rebuild metadata); then hex-verify.
4. The REST API surface is large but we only need to implement what the
   client actually calls at login/lobby — the rest can 404 gracefully
   while we build it out.

## Still to catalogue
- Exact JSON shapes of key endpoints (needs traffic capture against a
  live client — Windows box with mitmproxy)
- Which `/api/config/v1` flags the client requires at startup
- EAC/ACTk init call sites (for stubbing)
