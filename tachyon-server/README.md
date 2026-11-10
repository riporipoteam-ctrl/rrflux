# TachyonServer — Flux Rec's Rec Room 2026 room/voice server

Custom .NET 8 server implementing the Rec Room 2026 client's **Tachyon**
transport (forked Unity Netcode over LiteNetLib): connection approval with the
RSA→AES "Carrot" handshake, Dissonance voice handshake/relay, room-event relay,
and the stock TimeSync stream. In the 2026 client this connection is the **room
transport**, not just voice — no working Tachyon server, no multiplayer.

- **Current build: v8** (in `src/`), exe SHA-256
  `68e2523a6fd3cdb1dd64f013d196730e3e320ebbdf2402f0bac7ec7374a0a54d`
- The exe is distributed from the HF dataset `Echoxr/rrflux-game` at
  `tachyon-server/TachyonServer.exe` (+ `.sha256` sidecar). The Ripo Team
  Launcher (v1.0.83, `game/installer` in this repo) hash-syncs it on every
  `--play`, kills any stale running copy (spawned-hash staleness check), and
  starts it hidden on `127.0.0.1:7777`. Matchmaking (match worker,
  `RECFLARE_TACHYON_HOST_PORT`) points clients at the host's playit.gg UDP
  tunnel (`147.185.221.214:17346` → host `127.0.0.1:7777`).
- Full build-by-build history, protocol notes and test evidence: **STATUS.md**.
- Wire-format references (from the RecFlare 2025 fork decompilation): **docs/**.

## THE OPEN BUG (read this first)

The game still hangs at 18% loading into the dorm. Hard evidence, from the
**2026 client's own log** (attempt 2026-11-08 21:43 UTC, against server v7):

> Newtonsoft JSON parse exception inside
> `TachyonClient.NetworkManager_OnClientConnectedCallback`:
> **"unexpected character D at position 0"** — then, after waiting, room load
> reports **FailedToConnectToVoice (8719)**.

What this proves: the LiteNetLib connection, the NGO batch layer and the
approval dispatch all work — `ConnectionApprovedMessage.Handle` runs and sets
`NetworkManager.RecRoom_ConnectionInfo` from the message `Payload` string, and
the client's callback then `JsonConvert.DeserializeObject`s that string as
`{"DR":"Carrot","KT":"…"}`. The string the client *reconstructs* is corrupt:
our JSON's character at index 2 is `D`, matching the exception exactly.

Bob's v8 hypothesis (UNPROVEN — v8 also failed on the real client on
2026-11-10; no newer client exception text captured yet): the **2026**
`ConnectionApprovedMessage.Deserialize` consumes **two extra bit-packed scalar
values between `NetworkTick` and the `Payload` string** compared to the 2025
fork layout documented in `docs/ngo-fork-batch-and-approval.md`. Under that
story the client reads our payload length as scalar #1, our `{` as scalar #2,
our `"` (34) as *its* string length, then 34 chars starting at `D`. v8
therefore writes `[bitpacked ownerClientId][bitpacked 0]` in that gap
(`NgoMessages.BuildApprovalBody`). If the layout is still wrong, the decisive
evidence is the pair: (a) the client's new exception text (which character /
position), and (b) the approval-body hex the server now logs on every approval
(`[ngo] approval body sent to client N: … first 128B hex: …`). Decompiling the
2026 client's `ConnectionApprovedMessage.Deserialize` (2025's was at
`0x972F7D0`; type names are unobfuscated `Unity.Netcode.*`) settles it outright.

Everything else in the stack is implemented and self-tested: LiteNetLib
protocol 13 + the fork encrypt layer (plaintext marker `0x11`; encryption
arming is a known separate gap — see STATUS.md), the full AuthPayload
decryption chain (VB self-test passes against the real client, token account
matches), Dissonance handshake answer + ClientState broadcast, proxied
room-event relay (kind 3→4 with the measured header: int32 sender, byte event,
int32 cache index, unchanged payload), room-data relay (5→6), and the 1/sec
server→client TimeSync stream (stock NGO sends it; there is no client
TimeSync request).

## Layout

- `src/TachyonWire/` — wire library: `BytePacking.cs` (fork bit-packing:
  `encoded = (value << 3) | numBytes`, ZigZag for signed), `NgoBatch.cs`
  (24-byte batch header, BatchCount @16, XXHash64), `NgoMessages.cs` (message
  build/parse incl. `BuildApprovalBody`), `TachyonHandshake.cs` (AuthPayload
  decrypt + Carrot approval minting), `RecRoomClientEncryptLayer.cs`,
  `RsaUtil.cs`.
- `src/TachyonServer/Program.cs` — the server: LiteNetLib host on UDP 7777,
  approval, roster/instances, named-message routing, Dissonance, TimeSync
  stream, persistent `server.log` next to the key file.
- `selftest/` — synthetic client + `run-selftest.sh`. **Caveat:** the test
  client shares the server's format assumptions; its PASSes prove internal
  consistency only. Only the real client is ground truth.
- `docs/` — RecFlare protocol references (2025 fork; the 2026 client may
  differ — see the bug above).

## Build / publish

```
dotnet build -c Release src/TachyonServer/TachyonServer.csproj
dotnet publish -c Release -r win-x64 --self-contained true \
  -p:PublishSingleFile=true src/TachyonServer/TachyonServer.csproj
```

Then upload the exe to the HF dataset path above and replace the `.sha256`
sidecar **last** (the launcher acts on the sidecar; its content must be the raw
ASCII hex hash + newline, 65 bytes — a base64-encoded sidecar silently breaks
syncing).

## Keys — IMPORTANT

`server-key.xml` (the RSA **private** key) is deliberately **NOT in this
repo**. The server loads it from its working directory at startup; a copy
lives on Armin's PC in `C:\Games\FluxRec\tachyon-server\` and with Bob.
`voice-key-public.xml` (public half) is included here and is baked into the
launcher as `DEFAULT_VOICE_KEY_XML` (and into `2026patch.ini`). If you
regenerate the keypair you must ship the new public key in the launcher/INI
too, or every client's handshake fails the VB self-test.
