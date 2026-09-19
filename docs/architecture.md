# Architecture

## Base client

- **Source:** archived Rec Room build, November 4, 2022 06:50:25 UTC
  ("For Local Eyes Only" edition)
- **Build ID:** `9857464` · **Manifest ID:** `8840536006495440831`
- **Type:** IL2CPP (no readable .NET assemblies — strings live in
  `GameAssembly.dll` + IL2CPP metadata)
- **Size:** ~3.49 GB cached
- **Key binaries:** `RecRoom.exe`, `Recroom_Release.exe`, `GameAssembly.dll`,
  `UnityPlayer.dll`, `globalgamemanagers`, level files (`level0`…`level105`)
- **Anti-cheat:** ships `EasyAntiCheat_Setup.exe` + `EasyAntiCheat_x64/x86.dll`.
  EAC's servers are gone with Rec Room; the client must be neutered to not
  require EAC at launch (open question — see `client-patching.md`).

## Components

### 1. Patched client (Windows, player PCs)
The Nov 2022 build with network endpoints redirected from Rec Room's
defunct servers to RRFlux backends:
- API / profile / auth calls → our REST endpoints (fronting Firebase)
- Realtime session traffic → Photon relay (App ID injected at patch time)
- EAC disabled or stubbed

### 2. Firebase (`flux-544a6`) — identity + persistence
- **Authentication:** player accounts (application-approved sign-ups)
- **Firestore:** profiles, saves, inventory, room/session metadata
- **Storage:** player uploads (screenshots, custom content)
- Firebase never carries live gameplay traffic — it's the system of
  record, not the relay.

### 3. Photon — live multiplayer relay
- Player-hosted sessions: the host's PC runs the room, Photon relays
  state between players.
- App ID is baked into the patched client config (kept out of git).

### 4. Launcher / `setup.exe` (NSIS)
- Downloads the (patched) game payload from our distribution point
- Installs to `%LOCALAPPDATA%\RRFlux` (or user-chosen dir)
- Handles updates (manifest diff), login, and launch
- Only distribution channel during the private phase

## Data flow (login → play)

1. Player opens launcher, signs in (Firebase Auth).
2. Launcher checks manifest, downloads/updates game files.
3. Launcher starts patched `RecRoom.exe` with session token.
4. Client calls our API (profile, friends, rooms list) via Firebase-backed endpoints.
5. Player joins/hosts a room → Photon relay connects the session.
6. Saves + profile changes persist to Firestore.

## Open questions

- [ ] Exact endpoint strings in the IL2CPP build (pending client inspection)
- [ ] EAC handling: strip vs. stub
- [ ] Whether the Nov 2022 client hardcodes cert pinning on API calls
- [ ] Photon region + room-capacity tuning for player-hosted sessions
