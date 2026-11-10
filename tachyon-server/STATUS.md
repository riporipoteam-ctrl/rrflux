# TachyonServer — status (milestone 1, 2026-11-08)

Self-hosted room/voice session server for the Rec Room 2026 client (Flux Rec),
built from the reference docs in `docs/` (RecFlare rr-voice-server doc set) with
the patch-2026 maintainers' notes as a secondary contract. .NET 8 / C#.

Layout:

- `src/TachyonWire/` — protocol library (byte packing, NGO fork batches,
  message (de)serialisation, encrypt layer, handshake crypto, RSA utilities)
- `src/TachyonServer/` — the server console app
- `selftest/` — throwaway test client + harnesses (not shipped)
- `server-key.xml` — server RSA keypair, **private + public** (file mode 600)
- `voice-key-public.xml` — public half only; this is the `VoiceKeyXml` content
  for the client config

Run it:

```bash
dotnet build -c Release src/TachyonServer/TachyonServer.csproj
dotnet src/TachyonServer/bin/Release/net8.0/TachyonServer.dll [--port 7777] [--key-dir <dir>] [--instance <id>]
# env equivalents: TACHYON_PORT, TACHYON_KEY_DIR, TACHYON_INSTANCE, TACHYON_AUTOSTOP_SECONDS
```

## Implemented and verified by the self-test (`selftest/run-selftest.sh` → PASS)

- **Clean build**: `dotnet build -c Release` — 0 warnings, 0 errors
  (TachyonWire, TachyonServer, TachyonTestClient), verified from a clean tree.
- **Transport**: stock LiteNetLib 1.3.1 (the version whose wire fingerprint —
  ConnectRequest/ConnectAccept sizes, MaxConnectionNumber 4, protocol id 13 —
  matches the docs; verified at runtime: server logs `protocol id: 13`).
  `RecRoomClientEncryptLayer` per `recroom-encrypt-layer-format.md`: trailing
  `0x11` plaintext framing on every datagram (LiteNetLib connect data arrived
  as the documented 2 bytes `01 00`), `0x2A` handling and `ExtraPacketSizeForLayer = 9`
  reservation implemented per spec (see "not established" below for the cipher).
- **NGO fork batch format** per `ngo-fork-batch-and-approval.md`: 24-byte
  header, magic `0x1160`, BatchCount at offset 16, XXHash64 of the payload
  (seed 0) verified on receive and written on send. Bit-packing (3-bit size
  prefix, raw-`u32` escape, ZigZag signed) round-trips between the two
  independent implementations in this repo (server + test client).
- **Tachyon handshake + RSA→AES key exchange** per
  `voice-handshake-key-exchange.md`: server decrypts `CKA`/`CIA` with its own
  RSA key (PKCS#1 v1.5), decrypts `AT` (AES-256-CBC/PKCS7) and reads
  accountId/environment (access token length only — the value is never
  logged), recovers the client RSA public key from `CPK` (CAPI PUBLICKEYBLOB
  parse; CBC chain continuing from AT's last block, CIA-IV fallback per the
  patch-2026 notes), and runs the `VB` self-test against the documented
  constant — **PASS** in the self-test (which also proves our server key is
  the one the client encrypted to, i.e. the VoiceKeyXml swap works).
- **Approval**: NGO `ConnectionApprovedMessage` in the fork's trimmed layout,
  MessageVersions echoed, Payload = `{"DR":"Carrot","KT":…}` with `KT` minted
  under the recovered client key. The client decrypted it and derived the
  session key `CKA XOR SessionKeyB` — **server and client independently
  computed the same session key** (SHA-256 compared by the harness; a fresh
  key every run).
- **Message-version hash resolution**: the approval/named-message indices are
  resolved from the client's own version table by XXHash32 over candidate
  type names, exactly as the docs describe. In the self-test all 7 hashes
  resolved against `Unity.Netcode.`-prefixed names, with ConnectionRequest
  at index 1 as the docs record.
- **Roster / instance model** per `voice-instance-routing.md` +
  `voice-roster-account-id.md`: sessions keyed by peer, identity = account id
  as a decimal string (from `AI`, cross-checked against the decrypted token),
  single default instance, per-session Dissonance player id assigned; roster
  logged on join/leave.
- **Named-message intake**: fork framing (u16 type) parsed; a Dissonance
  packet (type 1, stock magic `0x8BC7` verified) was received and passed
  through the instance relay loop, re-framed as type 2 with each recipient's
  own named-message index.
- **RSA keypair management**: generated on first run (2048-bit), persisted to
  `server-key.xml` (private, mode 600), reloaded on later runs (verified),
  public half re-emitted to `voice-key-public.xml` every start.

Sandbox note: this VM's netstack rejects `sendto()` on unconnected UDP
sockets (EPERM, loopback included), so the self-test LD_PRELOADs a small shim
(`selftest/udp-shim.c`) that reroutes sends via `connect()`+`send()` — wire
bytes unchanged, server binary untouched. On any normal host the shim is
unnecessary. The same filtering means a **second** client cannot be heard by
the shimmed server socket in-sandbox, so the two-client relay harness
(`selftest/run-relay-test.sh`) cannot pass here; it is ready to run as-is on
a real UDP host.

## Stubbed / logged-only

- **Kinds 3/4 (proxied room events) and 5/6 (room data)** named messages:
  received and logged, not routed — room gameplay state is a later milestone.
- **TimeSyncMessage**: not answered yet (the real client expects the built-in
  RPC-style reply on the same index; likely part of milestone (a)).
- **Dissonance RemoveClient broadcast on leave**: not sent yet — without it
  remaining clients keep a stale voice playback object. The packet body must
  be stock Dissonance (base-player RemoveClient), not yet constructed.
- **Auth validation**: per the docs the server never has to validate the
  access token or `AI` to approve; we currently approve any well-formed
  handshake (token/account cross-check is logged). Real validation against
  the Flux Rec backend is a later milestone.
- **Instance routing**: single default instance; the routing inputs
  (`X-RecRoomInstanceId` equivalent for this transport) are not parsed yet.

## Not established in the docs (deliberately NOT invented here)

- **The datagram cipher**: the docs pin the framing and the 32-byte key
  material only; the client's transform is Burst-compiled and the doc set
  says explicitly not to assume AES from the key size (patch-2026 notes
  mention a Rabbit IV in encrypted datagrams). The layer therefore runs the
  proven plaintext path; `SetKey` stores the session key, and the encrypted
  path is fenced off loudly in code rather than guessed. Every datagram in
  the self-test — and in the docs' own captures — is the plaintext form.
- **ConnectionRequest connection-data prefix**: the docs describe the data as
  `[length][JSON]` without pinning the length's encoding, and the captured
  real-client request additionally carries plain fields after the JSON whose
  prosections conflict. The server extracts the JSON by structure (first
  `{` … last `}`) and keeps the raw tail for RE logging; if a future capture
  shows the JSON offset differs, only that extractor changes.
- **Approval message's pre-approval index**: not pinned by the docs; resolved
  at runtime from the client's version hashes (works in the self-test), with
  a logged fallback.
- **Listen port**: the docs pin no Tachyon port. Default here is **7777**,
  configurable via `--port`/`TACHYON_PORT`; whatever is chosen must match the
  `TACHYON_HOST_PORT` value wired into the match worker's
  `VoiceConnectionInfo`.

## Next milestones

- **(a) Real 2026 client enters the dorm**: needs a public-UDP host for this
  server (not Cloudflare Workers, not free HF Spaces — UDP is blocked there),
  `RECFLARE_TACHYON_HOST_PORT="<ip>:<port>"` set for the match worker so
  clients get a `VoiceConnectionInfo` at all, and `voice-key-public.xml`'s
  contents shipped to clients as `VoiceKeyXml` in 2026patch.ini. Then: watch
  the real client's handshake in the server log (its connection-data tail is
  already captured for diffing), answer TimeSync, and iterate on whatever it
  sends next.
- **(b) Two clients seeing each other**: run `selftest/run-relay-test.sh` on
  the real host to verify the relay live; implement the Dissonance
  RemoveClient broadcast; then the room-data lane (kinds 3–6) so presence and
  room state — not just voice packets — flow between sessions.
- **(c) Voice relay**: the packet relay itself is in (stock Dissonance
  packets forwarded verbatim inside named messages). Remaining: verify with
  two real clients + real Dissonance traffic (join/leave announcements
  originate from the clients' own Dissonance cores; the server stays a dumb
  relay), then decide whether the encrypted datagram path is ever in play
  (depends on the unpinned cipher above — the session key already exists).

## v2 — instrumentation build (2026-11-08, later same day)

Shipped after the first real-Windows deployment hung the 2026 client at 18%
("voice connecting server failed") with zero diagnostics: the launcher
spawns the server with stdio=null, so the console log vanished. v2 changes
**logging only** — no protocol behaviour changed (the encrypt layer's drop
outcomes are byte-identical; only previously silent drops now say why).

- **Persistent file log**: every `Log()` line is mirrored, flushed per line,
  to **`server.log` in the key-dir** (the same directory as `server-key.xml`
  / `voice-key-public.xml`), append mode, `FileShare.Read` so it can be
  tailed/attached while the server runs. File lines carry a full UTC
  timestamp (`[2026-11-08T15:29:08.842Z]`); console format is unchanged.
  If the file cannot be opened the server runs console-only with a warning.
  On a player's machine the key-dir is the server's working directory, so
  the log lands at `C:\Games\FluxRec\tachyon-server\server.log`.
- **What is logged now** (beyond the milestone-1 lines): startup with the
  RSA modulus fingerprint (SHA-256 of the public modulus, `fp8` = first 8
  hex chars — never private material); the file-log path itself; every
  LiteNetLib connect/disconnect with peer id + endpoint; **the first
  datagram's framing marker per endpoint** — `first inbound datagram from
  <ep>: framing marker 0x11 (plaintext)` vs `0x2A (ENCRYPTED)` — the key
  diagnostic for whether the real client speaks the encrypted path from
  packet one; encrypt-layer drops **with reasons** (too short for the nonce
  trailer / no key set / cipher not established / unknown marker); every
  approval request with account id and its outcome (`APPROVED …` /
  `FAILED … account=…`); session-key establishment as SHA-256 hash only;
  a `HEARTBEAT peers=N approved=M` line every 60 s (verified live at
  exactly +60 s).
- **Crash hardening**: exceptions in the packet path are caught and logged
  with full stack instead of killing the server — the inbound encrypt
  layer, the ConnectionRequest/PeerConnected/PeerDisconnected handlers,
  the NetworkReceive handler, and `PollEvents` itself.
- **Verified**: `dotnet build -c Release` clean (0/0);
  `selftest/run-selftest.sh` PASS, with `server.log` at the key-dir
  containing the connect / first-marker / APPROVED / SESSION_KEY lines;
  win-x64 self-contained single-file publish (67,726,355 B,
  sha256 `7ec97e584d5b089f3bc6d9b063b9473c95200c946a8d62be98d9d4e14f853858`)
  uploaded to the HF mirror as `tachyon-server/TachyonServer.exe`
  (commit `5be11b32aa32`, live bytes hash-verified), sidecar
  `tachyon-server/TachyonServer.exe.sha256` updated **after** the exe
  (final commit `c173fc93cd3b`).
- **Sidecar gotcha found + fixed**: the sidecar must contain the raw ASCII
  hex hash (65 B with newline) — the launcher's `parse_sidecar_hash`
  rejects anything else and silently keeps the old exe. An earlier upload
  had stored the base64 *text* of the hash (88 B) because the NDJSON
  commit op omitted `"encoding": "base64"`; the file op needs that field
  or HF stores the base64 string literally. The mirror sidecar is now the
  raw hex form, verified byte-for-byte from the resolve URL.

**How to read the log after a real-client run**: find `LISTENING`, then the
first `first inbound datagram` line — `0x11` means the client connected on
the plaintext path and any failure is downstream (handshake/approval: look
for `APPROVED`/`FAILED` + `SESSION_KEY_SHA256`); `0x2A` means the client
demands the encrypted path whose cipher is still unpinned (see
"not established" above), and the accompanying drop-reason lines will say
exactly where its packets died. `HEARTBEAT` lines prove the process stayed
alive for the whole session.

## v3 — ConnectionRequestMessage rejection fix (2026-11-08, later still)

**The bug (the proven 18% blocker).** On the first full-path live test
(client → playit tunnel → this server on Armin's PC), the client stalled at
18% loading into the dorm room. The v2 server.log showed why: the client's
ConnectionRequestMessage was rejected and the peer dropped.
`NgoMessages.ParseConnectionRequest` read the MessageVersions count and
threw on anything outside 0..64 — but the real 2026 client registers
**1298** message types. The milestone-1 self-test client sent 7 versions,
so no local test ever tripped the cap. (ChatGPT, working the same incident
from the client side, independently read the same count — 1298 — off the
rejection.)

**The fix** (`src/TachyonWire/NgoMessages.cs`):
- Cap raised 64 → **100_000** in `ParseConnectionRequest`, with an
  explicit fit check on top: `count > cursor.Remaining / 5` throws (each
  entry needs a raw u32 hash + ≥1 packed-version byte). The cap is now
  only a sanity bound; the real protection was already there —
  `ByteCursor` throws `FormatException` on every overrun read (verified in
  `BytePacking.cs`: ReadByte/ReadU16LE/ReadU32LE/ReadBytes all bounds-check).
- Same guard added to `ParseApproval` (previously uncapped; cursor-bounded
  but now symmetric).

**Scale audit** — everything else a 1298-entry list / ~9.5 KB request /
~8.5 KB approval echo could trip, checked by grep + read-through of
TachyonWire and TachyonServer:
- `NgoBatch`: BatchSize is u32, BatchCount u16 (we always send 1) — fine.
- `BytePacker.WritePackedUnsigned`: rejects values > u32 — every value the
  approval writes (count 1298, hashes, tick, payload chars) is far below.
- `TachyonHandshake`: fixed-size crypto fields only (RSA/AES blobs) — the
  versions list never touches it.
- `RecRoomClientEncryptLayer`: per-datagram marker strip only, no buffers.
- Send path (`Program.SendBatch`): single batch via LiteNetLib
  ReliableOrdered — fragmentation/reassembly at 1298 and 5000 entries is
  exercised by the regression below and works through the encrypt layer.
- One real hazard found + fixed in `TachyonServer/Program.cs`: the
  handshake handler itemised **every** version entry into server.log
  (flushed per line) — 1298 lines per connect attempt, and the real client
  retries. It now itemises only entries resolving to known internal types
  (the indices the server acts on) and summarises the rest.

**Regression evidence** (`selftest/run-selftest.sh`, test client gained a
`--versions N` flag that pads the 7 internal types with plausible
synthetic XXHash32-named entries, indices 1/2/3 unchanged — the real
client's list shape; logs in `selftest/{server,client}-v<N>.log`):
- **7 versions** (original test): PASS — approval DR="Carrot", session
  keys match (sha256 a93bee1c…), Dissonance probe received.
- **1298 versions** (the real client's count): PASS — request parsed
  (1298 versions, 1436 B auth tail), approval echoes all 1298 versions,
  DR="Carrot" + KT, session keys match (sha256 72e013bf…), Dissonance
  probe received + relay path run.
- **5000 versions** (headroom): PASS — session keys match
  (sha256 925e1a91…), same full flow.
(The two-client relay test remains un-runnable inside this VM for the
documented netstack reason — see run-relay-test.sh; the single-client
runs above exercise Dissonance intake/relay dispatch as before.)

**Shipped**: win-x64 self-contained single-file publish, 67,726,867 B,
sha256 `c87d44ada0afcb6f596d2df2907b796ce8149f79e884f4754aacaef135397509`,
uploaded to the HF mirror as `tachyon-server/TachyonServer.exe`
(commit `d18a1816e889`, live bytes hash-verified), sidecar
`tachyon-server/TachyonServer.exe.sha256` committed **after** the exe
(commit `fe32bf3d0f125`) with `"encoding": "base64"` on the file op —
live sidecar verified via the resolve URL to be the raw ASCII hex +
newline (65 B) of the new hash. Launchers hash-sync on next --play, so
Armin picks v3 up automatically; his already-running v2 process keeps
serving until he relaunches.

## v4 — framing correction + post-approval audit (2026-11-08, evening)

**The v3 diagnosis was wrong — correction.** With v3 live on the real
machine the client STILL stalled at 18%, and the new v3 error was the tell:
`MessageVersions count 1298 cannot fit in the 2596 bytes received`. A real
1298-entry versions array needs ≥ 6,490 bytes — it cannot exist inside a
2,596-byte remainder. The number 1298 is a **misparse**: at that position
the wire carries a packed-**unsigned** value **2596**, and zigzag-decoding
2596 yields exactly 1298. 2596 also equals the bytes remaining after the
field — i.e. the field is a **per-message size prefix** (stock NGO's
MessageHeader pattern: packed type index, packed size, then the body),
with the single request message filling its batch. Everything else lines
up: `voice-stack-package-versions.md` pins the fork's NGO to **7 internal
message types**; `ngo-fork-batch-and-approval.md` records of the captured
request that "the 8 hashes are in the client's local order and
ConnectionRequest sits at index 1" — the real versions section is ~8
entries, not 1298. The 24-byte batch header was never in doubt:
`NgoBatch.TryParseHeader` enforces magic + BatchSize + the XXHash64 the
client itself computes over the payload (doc: the client verifies
`XXHash64(buf+24, size-24, seed 0)`), and the request reached the body
parser — so the body offset 24 is proven by the client's own hash, and the
misparse was inside the message body. The approval layout the v1–v3 parser
mirrored is documented only for `ConnectionApprovedMessage`; the request's
own per-message framing was assumed, wrongly. (The self-test passed at
"1298" in v3 because our test client mirrored the server's assumed layout
— both sides agreed with each other, not with the real client.)

**What v4 ships:**

- **MessageHeader framing both directions**
  (`NgoMessages.FrameMessage` / `TrySegmentFramed`). Receive: a batch is
  first segmented as repeated `[packed index][packed size][body]` records
  that must consume the payload EXACTLY; on success every message is
  routed by bounded slice and unknown messages are skipped without
  desynchronising the batch. If segmentation fails, the exact v1–v3 legacy
  parse runs (and a framed-slice request that fails to parse is retried
  through the legacy whole-batch parse before being rejected). Send: the
  approval and named messages are framed the same way the peer framed its
  request (per-session `Framed` flag) — replies mirror the request's
  framing, so the legacy path remains fully usable. `ParseConnectionRequest`
  now also falls back to scanning the **whole message slice** for the
  AuthPayload JSON if the post-versions tail scan finds none.
- **Request ground-truth dump** (`[reqdump]` lines, before any parsing):
  header fields (magic, BatchSize, BatchHash, BatchCount), the computed
  body offset, candidate decodings of the first body fields (with an
  explicit "packedUnsigned EQUALS the remaining bytes ⇒ size prefix"
  verdict), hex of the first 128 datagram bytes and the first 96 body
  bytes. Full dump on a peer's first attempt and whenever the length
  changes. **If the framing verdict above is somehow still wrong, one live
  run's `[reqdump]` hex is sufficient to decode the real layout offline.**
- **Encrypt-layer accounting** (`RecRoomClientEncryptLayer`): per-endpoint
  datagram/drop counters; the plaintext→encrypted **marker flip**
  (0x11 → 0x2A) is logged with the first encrypted datagram's head hex and
  its 8-byte nonce trailer; per-drop log lines are rate-limited (first 3
  per endpoint) with running totals appended to the 60 s HEARTBEAT.
- **All internal indices resolved at approval** (TimeSync, Unnamed,
  ServerLog, DisconnectReason — logged in one `[ngo] client N message
  indices:` line). Post-approval messages that aren't NamedMessage are now
  logged with the resolved type name (index → version-table hash →
  `DescribeHash`), body length, and first-bytes hex; framed batches
  continue past them.
- **TimeSync capture**: a TimeSyncMessage arrival is logged with its full
  body hex (it is small) — **no reply is sent** (see unpinned list).
- **Dissonance control plane consumed, not relayed**: inside
  DissonanceToServer named messages, the stock packet type byte is now
  inspected. HandshakeRequest (4), ErrorWrongSession (6) and
  DeltaChannelState (9) are server-directed per the type table in
  `dissonance-named-message-framing.md`; they are consumed and logged with
  full packet hex instead of being relayed to other peers (v1–v3 relayed
  them — wrong, and it hid the voice-session setup). Server→client types
  (0/1/5/7/8) arriving from a client are likewise consumed + logged. The
  data plane (VoiceData 2, TextData 3) keeps the verbatim relay.
- **RemoveClient broadcast on leave** (implemented; prescribed by
  `voice-stale-playback-on-leave.md` — clients cannot clean up playback
  objects themselves): on disconnect, remaining approved sessions in the
  instance receive a stock Dissonance RemoveClient packet (magic 0x8BC7 +
  type 0 + departed ushort player id, big-endian id per Dissonance's
  primitive order — the id **width** is pinned for the 2026 build by the
  framing doc; the endianness is inferred from the magic's wire order)
  inside a type-2 named frame. The broadcast is logged with its recipient
  count (0 included, so single-client runs still prove the code path).
- **Kinds 3–6 named messages** now log first-bytes hex (were length-only).

**Self-test** (`run-selftest.sh`; test client gained `--legacy-framing`
and two new post-approval probes — a TimeSyncMessage at the TimeSync index
and a Dissonance HandshakeRequest named message; the voice probe is now a
proper VoiceData packet, type 2): **framed × 7 / 1298 / 5000 versions —
PASS; legacy × 7 — PASS** (session keys match on both sides in every run;
harness additionally asserts the `[reqdump]` dump, the framing-mode line,
the TimeSync capture, the HandshakeRequest consumption, and the
RemoveClient broadcast). The two-client relay remains un-runnable in this
VM (documented netstack limit) — unproven here, as before.

**Shipped**: win-x64 self-contained single-file publish, 67,739,155 B,
sha256 `5e13264517e67c0e8920a91c52850855bd54de79c06ea7f6b79911d2e1fce54c`,
uploaded to the HF mirror as `tachyon-server/TachyonServer.exe`
(commit `0ebba78bde992`, live bytes hash-verified), sidecar
`tachyon-server/TachyonServer.exe.sha256` committed **after** the exe
(commit `32ab4b2a91a3`) with `"encoding": "base64"` on the file op — live
sidecar verified via the resolve URL as raw ASCII hex + newline (65 B).

### Ranked post-approval gaps (what can still stall the client at 18%)

1. **Post-approval encryption deafness — the client goes encrypted and
   the server cannot hear it.** The approval callback passes
   `SessionKeyB` to `RSA.Decrypt` with no null check (KT is Required),
   then calls `RecRoom_SetEncryptionInfo(clientId, key)`: the client arms
   its encrypt layer with the 32-byte session key and encrypts **every**
   outbound datagram from then on (`ngo-fork-batch-and-approval.md` — "the
   client goes encrypted and locks you out"). Outbound branches only on
   key presence; inbound 0x11 is strip-only with no key check, so our
   plaintext sends remain readable — the deafness is one-directional:
   client→server dies at the layer. `dissonance-voice-connection.md`
   independently describes the voice transport as "ENCRYPTED". **The
   cipher transform is NOT pinned by the docs** (Burst-compiled;
   `recroom-encrypt-layer-format.md` warns against assuming AES; patch-2026
   notes mention a Rabbit IV) — v4 deliberately does not guess it. What
   v4 adds is proof: the marker-flip log line with the first ciphertext's
   head + nonce trailer, and drop totals in every HEARTBEAT.
2. **Batch-body framing misparse — FIXED in v4** (see the correction
   above; citations: `voice-stack-package-versions.md` 7 types,
   `ngo-fork-batch-and-approval.md` "the 8 hashes"). If a live run still
   shows `framing=legacy` for the real client, or the framed parse falls
   back, the `[reqdump]` hex settles the layout offline.
3. **Dissonance voice-session handshake never completed.** Room load has
   a dedicated voice stage whose failure is its own result
   (`FailedToConnectToVoice = 5` in `dissonance-voice-connection.md` —
   reaching it means region + room already succeeded). Stock Dissonance
   expects the server to answer HandshakeRequest (4) with
   HandshakeResponse (5); the client's own log string ("Received handshake
   response from server, joined session '{0}'") shows it waits for one.
   v1–v3 relayed the request to nobody-useful; v4 consumes + captures it.
   **The HandshakeResponse body (session-id and name encodings,
   ClientState layout) is not byte-pinned by the docs**, so no response is
   fabricated — the captured HandshakeRequest hex (it carries the player
   name string) is the material that pins the string encoding next round.
4. **TimeSync unanswered — body format unpinned.** TimeSyncMessage is one
   of the 7 internal types (`voice-stack-package-versions.md`), and this
   file's milestone-1 note records the expectation of a built-in RPC-style
   reply on the same index — but no doc pins the body's fields, so v4
   captures the request body hex and sends nothing rather than fabricate
   a reply. If the client blocks on it, its captured hex + timing in the
   log will show exactly that.
5. **Room data (named kinds 3–6) logged-only.** The framing doc names the
   kinds (RRClientToServerProxied 3, RRServerToClientProxied 4,
   RRClientToServer 5, RRServerToClient 6) but pins no payload semantics;
   whether the client blocks on any server-originated room data for a
   mostly-static room (^RecCenter.Home / dorm) is unknown. v4 logs their
   first bytes so a live run shows whether the client even sends them
   before the stall.
6. **RemoveClient missing → stale playback — FIXED in v4** (implemented
   above; a voice-roster leak, not a load blocker, but it was the one
   remaining doc-prescribed server→client Dissonance message).

### Still doc-unpinned (deliberately not guessed)

- The 0x2A datagram **cipher transform** + key schedule (gap #1).
- The **TimeSyncMessage body** layout (gap #4).
- The Dissonance **HandshakeResponse / ClientState bodies** (gap #3).
- **Kinds 3–6 payload semantics** (gap #5).

### What the next live server.log will reveal

- `[reqdump]` lines: the real request's header fields + first-field
  verdict — `packedUnsigned EQUALS the remaining bytes` confirms the size
  prefix on the REAL client, not just our test client; the hex allows a
  full offline decode either way.
- `[handshake] … framing=size-prefixed (MessageHeader)` + a small (~8)
  versions count + `APPROVED` — the request finally parses as documented.
  A `framing=legacy` line or a "retrying legacy whole-batch parse" line
  instead falsifies the framing fix with the hex already in the log.
- The **marker-flip line** ("endpoint … switched to ENCRYPTED framing")
  timestamps the exact moment the client arms encryption after approval;
  HEARTBEAT `encryptedDropped=` totals then quantify the deafness of
  gap #1 — if the client stalls at 18% while only encrypted datagrams
  arrive, gap #1 is the blocker and the captured first ciphertext is the
  starting material for pinning the cipher.
- `[ngo] TimeSyncMessage from client N (… body …B: <hex>)` — presence +
  repetition interval shows whether the client is blocked retrying it.
- `[voice] Dissonance HandshakeRequest from account …: <hex>` — the
  voice stage was reached and is waiting on a response we don't yet send;
  the hex pins the name encoding for building one.
- `[room] proxied/room message (type 3|5) … first-bytes=…` — whether room
  data flows before the stall at all.
- If NONE of those post-approval lines appear but the flip + drop counters
  do, the stall is entirely inside the encrypted channel (gap #1) and no
  plaintext-side fix can move it.

## v5 — named-message parse fix + Dissonance handshake answer (2026-11-08, evening)

**Live verdict that drove v5** (Armin's PC, launcher v1.0.82, server v4):
the real 2026 client connected, its ConnectionRequest parsed with
size-prefixed framing (8 versions), it was APPROVED (account 121, VB
self-test PASS, session key derived) and it STAYED connected
(HEARTBEAT peers=1 approved=1). No encryption marker flip, no 0x2A
drops — the client stayed plaintext, clearing ranked gap #1 from the v4
list for this client. ~160 ms after approval the client sent ONE
NamedMessage (index 3) and v4 logged `[ngo] unparseable NamedMessage
body from client 1`, then nothing — the client waited forever at 18%.

**Root flaw (v5 fix #1 — named parse per doc).**
`TryParseNamedMessageBody` demanded every named body be
`[u16 type][i32 length][packet]`. But
`docs/dissonance-named-message-framing.md` pins the fork's
NamedMessage.Deserialize as reading ONLY the u16 Hash — the rest is
receive data. The i32 length prefix exists ONLY inside Dissonance
SendData (NfgoCommsNetwork packs the Dissonance byte[] with
WriteValueSafe). Any other named type (3 RRClientToServerProxied,
5 RRClientToServer, 7/8 AI) failed the i32 check — and the failure path
logged NO hex, so the message that hung the client was invisible.
v5: parse = `[u16 type][whole rest = payload]`; for Dissonance types
the i32 prefix is unwrapped ONLY when it fits EXACTLY
(len == remaining-4) and the stock magic 0x8BC7 follows
(`UnwrapDissonancePayload`); non-Dissonance payloads are the whole rest.
Named messages never hard-fail on length grounds again; the only guard
left is body < 2 bytes (no room for the type) → logged + dropped.
The SEND path is unchanged (BuildNamedMessageBody still wraps with the
i32 length, as NfgoCommsNetwork does).

**Fix #2 — full capture.** RouteMessage now logs, BEFORE any handling,
for every NamedMessage: client id, account, named type (number + name),
body length, and the FULL body hex (≤512B). Any still-failing body is
in the log with its bytes.

**Fix #3 — the Dissonance handshake is now ANSWERED, pinned from the
actual Dissonance source.** Placeholder-Software/Dissonance on GitHub
hosts docs only (its Readme: "This repository does *not* contain the
source code for Dissonance"). The reference copies are the asset
source / decompilations vendored in public game projects; TWO
independent decompilations of the current generation —
`nomodev-xd/SCPSL-Decompilation` (Assets/Plugins/Assembly-CSharp-firstpass/
Dissonance/Networking/{PacketWriter,PacketReader,BaseServer,MessageTypes}.cs,
…/Server/BroadcastingClientCollection.cs, …/BaseClientCollection.cs,
…/CodecSettings.cs) and `xenoveni/LethalMic` (decompiled/Dissonance.Networking/
{PacketWriter,MessageTypes}.cs) — agree byte-for-byte on every method
used; the string encoding was additionally cross-checked against the
older asset source in `oshaikh13/Boxy` (PacketWriter.Write(string)).
Pinned layouts:

- **Primitives** (PacketWriter.Write / PacketReader.Read*): all
  multi-byte values BIG-ENDIAN ("network order": u16 = MSB,LSB; u32 =
  network order). Magic 35783 = 0x8BC7 is written as a u16, landing as
  bytes 8B C7 — cross-checks against the observed wire bytes.
  **Strings: u16 BE length = UTF8 byte count + 1, then raw UTF8 bytes,
  no terminator; length 0 = null** (the reader does `num--`; the +1 is
  in both decompiled writers AND the old asset source). Byte segments:
  u16 BE count + bytes.
- **CodecSettings** (9B): codec u8, FrameSize u32 BE, SampleRate u32 BE
  (CodecSettings.cs: `uint _frameSize`, `int _sampleRate`;
  PacketReader.ReadCodecSettings reads byte/u32/u32).
- **HandshakeRequest** (type 4; PacketWriter.WriteHandshakeRequest /
  PacketReader.ReadHandshakeRequest): magic + type, **NO session id**
  (the one packet without one), then CodecSettings, then player name.
- **HandshakeResponse** (type 5; WriteHandshakeResponse /
  ReadHandshakeResponseHeader+Body): magic + type + session u32 BE +
  assigned clientId u16 BE + clients u16 count + per-client ClientInfo
  (name string, playerId u16 BE, CodecSettings) + room-names u16 count
  + name strings + room-lists u16 count AGAIN + per room (roomId u16 BE
  = ToRoomId(name), member count u8, member playerIds u16 BE).
- **ClientState** (type 1; WriteClientState / ReadClientStateHeader +
  ReadClientStateRooms): magic + type + session u32 BE + ClientInfo +
  u16 room count + room name strings.
- **Stock server behaviour** (BaseServer.cs;
  Server/BroadcastingClientCollection.ProcessHandshakeRequest):
  session id = random nonzero u32 chosen once per server process.
  On HandshakeRequest: read (codec, name); null name → ignore. The
  response is written with the collection's CLEARED client buffer —
  i.e. **0 clients, 0 rooms** — then, AFTER the response, one
  ClientState per client in the collection is sent to the JOINER ONLY
  ("fake" states). GetOrCreateClientInfo registers the joiner before
  GetClients runs, so the burst INCLUDES the joiner's own state.
  Network-Protocol.md blesses exactly this: "It is valid for the server
  to send some/none of the client data in the initial HandshakeResponse
  packet and instead to send it in individual ClientState messages
  immediately after the HandshakeResponse."
- **v5 implements exactly that**: on a parseable, non-null-name
  HandshakeRequest the server records name+codec on the session,
  replies HandshakeResponse (own session id, session.DissonancePlayerId,
  empty lists) on named type 2, then sends one ClientState per
  handshaken approved member of the instance (joiner included, ordered
  by Dissonance id) to the joiner. An unparseable / null-name request is
  logged with full hex and NOT answered — nothing fabricated.
  Incoming ClientState (type 1) is now stock-parsed (session checked +
  logged, name/codec/rooms recorded on the session) and still consumed;
  the stock server's VERBATIM broadcast of a client's ClientState to all
  clients is NOT implemented yet (see remaining gaps).
- **Version caveat (documented, not acted on)**: the MessageTypes
  numbering for RemoveClient differs across Dissonance generations —
  10 in both decompilations above, 0 in the 2026 build per the fork
  doc's writer immediates. HandshakeRequest 4, HandshakeResponse 5,
  ClientState 1, VoiceData 2, TextData 3, ErrorWrongSession 6,
  DeltaChannelState 9 agree in every source. v5 does not touch v4's
  RemoveClient (type 0, no session field — the 2026-doc form).

**Everything else is byte-identical to v4** (framing, approval,
encrypt-layer accounting, reqdump, RemoveClient, TimeSync capture,
kinds 3–6 first-bytes logging — now backed by the full-body capture).

**Self-test** (test client now sends a REAL stock HandshakeRequest —
codec 1 / frameSize 960 / sampleRate 48000, name = account — and asserts
the HandshakeResponse carries session ≠ 0, assigned id 1, empty lists,
and that the follow-up ClientState echoes its own name/codec/session):
**framed × 7 / 1298 / 5000 — PASS; legacy × 7 — PASS** (session keys
match in every run; harness additionally asserts the named full-capture
line and both handshake PASS markers). `dotnet build -c Release` clean
(0 warnings, 0 errors). The two-client relay remains un-runnable in
this VM (documented netstack limit) — unproven here, as before.

**Shipped**: win-x64 self-contained single-file publish, 67,746,323 B,
sha256 `2ba4452223a69eb3dc21906ad590f208f2305efea02afbf8601224c9fbe4fd34`,
uploaded to the HF mirror as `tachyon-server/TachyonServer.exe`
(commit `33a0e6979302`, live bytes hash-verified), sidecar
`tachyon-server/TachyonServer.exe.sha256` committed **after** the exe
(commit `a47107dacb88`) with `"encoding": "base64"` on the file op —
live sidecar independently re-verified via the resolve URL as raw
ASCII hex + newline (65 B) of the new hash. Launchers (v1.0.82
takeover) pick v5 up automatically on next --play.

### What remains unproven / open after v5

- **Whether the dorm now loads.** v5 removes the two proven blockers in
  sequence (unparseable named message; unanswered voice handshake) but
  only a real-client run on Armin's PC is evidence. NOT claimed here.
- **What the client's first post-approval NamedMessage actually is.**
  If it is a DissonanceToServer HandshakeRequest, v5 answers it. If it
  is a room type (3/5), v5 now logs its full hex — the next live
  server.log settles which, byte for byte.
- **ClientState verbatim broadcast** (stock ProcessClientState relays
  the raw packet to all clients): not implemented; multi-client voice
  state sync depends on it eventually. Solo join does not.
- **TimeSync** still unanswered (body format still unpinned — gap
  unchanged from v4); **kinds 3–6** still logged-only; the **0x2A
  cipher** still unpinned but now evidently unused by this client
  (it stayed plaintext through approval + named traffic).

## v6 — startup self-identification + dual named parse + ClientState broadcast (2026-11-08, evening)

**Why v6 exists.** On Armin's PC the FILE on disk was v5 (matching the
mirror) but the still-RUNNING process was v4 — its parser dropped the
client's first post-approval NamedMessage and the client hung at 18%.
Nothing in any log said WHICH binary was running, so a stale process
masqueraded as the new build. Separately, ChatGPT (PC-side, without the
fork docs) claimed Unity NGO named messages start with a 64-bit hash.
`docs/dissonance-named-message-framing.md` PROVES the fork's
NamedMessage.Hash is a ushort NamedMessageType (Deserialize reads
exactly 2 bytes; stock NGO's ulong is explicitly called out as a trap)
— so the u16 parse stays PRIMARY, and v6 adds a u64 fallback so the
argument is settled in code. Everything else is byte-identical to v5.

**Changes (server only):**

1. **Startup self-identification** (`TachyonServer/Program.cs`): the
   startup block now prints the build version AND the sha256 of the
   server's OWN running executable file:
   `=== TachyonServer (Flux Rec) starting === build=v6 version=v6 exeSha256=<full hash> exePath=<path>`.
   The path is resolved via `Environment.ProcessPath`, the file is
   hashed in-process, and the full lowercase hex hash is logged.
   Fail-soft: any failure logs `unknown` for the affected field and the
   server still starts. Version constant:
   `NgoMessages.ServerBuildVersion = "v6"`, surfaced as
   `Program.BuildVersion` and used in the startup line. (When run via
   `dotnet TachyonServer.dll` in the self-test, ProcessPath is the
   dotnet host, so the logged hash is the host's — in the shipped
   single-file exe ProcessPath IS the server exe, so the line identifies
   the exact binary that is running. That is the stale-process proof.)
2. **Dual named-header parse**
   (`NgoMessages.TryParseNamedMessageBodyDual`; the original
   `TryParseNamedMessageBody` now delegates to it):
   - Primary (unchanged): fork u16 type; known types 0..8 behave
     exactly as v5.
   - Fallback: if the u16 value is NOT a known NamedMessageType (0..8)
     AND the body is >= 8 bytes, the first 8 bytes are interpreted as a
     stock-NGO u64 name hash (little-endian); named type becomes the
     sentinel `NamedStockU64Unknown (0xFFFF)`, payload = bytes after
     the 8-byte header, `isStockU64Fallback=true`, `stockHash` set.
   - Caller (`Program.RouteMessage`): on fallback it logs distinctly —
     `[ngo] stock-u64 named header fallback, hash=0x…` with the full
     payload hex — and treats the message as CAPTURE ONLY: no crash,
     no blind relay, no HandleNamedMessage dispatch. Known-u16 path
     behaviour is unchanged.
3. **ClientState verbatim broadcast** (`Program.HandleNamedMessage`,
   Dissonance type 1): after a client's ClientState packet parses and
   is recorded (name/codec/rooms, as v5), the packet is now forwarded
   VERBATIM to every OTHER approved session in the same instance,
   wrapped as a `NamedDissonanceToClient` named message via the same
   `SendMessage` path the voice relay uses (stock
   `ProcessClientState` behaviour v5 left out). The log line now ends
   `— state recorded, verbatim broadcast to N other client(s)`.

**Self-test** (test client extended; no assertions weakened — two new
required probes were ADDED):
- (a) stock-u64 probe: a NamedMessage whose body is an 8-byte LE u64
  header `0x1122334455667788` (low u16 0x7788 is not a known fork type)
  + `stock-u64-probe` payload, sent BEFORE the handshake probe so the
  subsequent HandshakeResponse/ClientState answer proves the server
  stayed alive and approved after the fallback path.
- (b) the existing stock HandshakeRequest probe is kept, and a new
  ClientState probe (client sends its own stock ClientState after the
  handshake) exercises the v6 broadcast code path.
- Harness (`run-selftest.sh`) additionally asserts: `build=v6` and
  `exeSha256=` in the server log, the
  `stock-u64 named header fallback, hash=0x1122334455667788` line,
  `STOCK_U64_PROBE_SENT=PASS`, the
  `verbatim broadcast to 0 other` line, and
  `CLIENTSTATE_PROBE_SENT=PASS`.
- **Results — all PASS, session keys match on both sides in every run:**
  - framed × 7: PASS — session key sha256
    `a1217f9d35733303fb60ea3e5d8d47b11ece9dc75bbcf080816cc56f6680a875`
  - framed × 1298: PASS — session key sha256
    `18e89c4dafb3d8f7fc856e22770205c1157b23461d6dc19aa749fee7f5f6cbca`
  - framed × 5000: PASS — session key sha256
    `7371ef689330096bef119c46b6bc9d5368cff934563c688805dc83b3966b0f1a`
  - legacy × 7: PASS — session key sha256
    `2afb05e5411cbe6395b42a9acc3ce4598e743e9e76a4498330ae4384088b1d71`
  Each server log shows the fallback line with payload hex
  `73746F636B2D7536342D70726F6265` ("stock-u64-probe") and the
  ClientState line ending `verbatim broadcast to 0 other client(s)`
  (0 because the sandbox runs a single client — see below).
- `dotnet build -c Release`: **0 warnings, 0 errors** (TachyonWire,
  TachyonServer, TachyonTestClient).

**Published (NOT uploaded — Bob ships after review)**: win-x64
self-contained single-file publish, same command/layout as v1–v5
(`dotnet publish -c Release -r win-x64 --self-contained true
-p:PublishSingleFile=true -o dist/win-x64`):
`dist/win-x64/TachyonServer.exe`, **67,750,931 B**, sha256
`5c2146934643ef31a4b9d723d873af05fb440136e762e843aed8fc1837ad4025`.
Nothing was uploaded to the HF mirror by this build; the launcher,
rrflux, and matchmaking were not touched.

### What remains unproven / open after v6

- **Whether the dorm now loads.** v6's fixes only take effect once the
  v6 process is actually the one RUNNING on Armin's PC (the stale-v4
  takeover problem is the launcher's side — v1.0.83). Only a real
  launch with v6 running is evidence; the new startup line
  (`build=v6 … exeSha256=5c214693…` for the shipped exe) is how that
  will be proven from the log. Dorm load is NOT claimed here.
- **ClientState broadcast with a real recipient.** The verbatim
  broadcast loop is exercised in the self-test with 0 other sessions;
  the two-client relay remains un-runnable in this VM (documented
  netstack EPERM limit), so delivery to an actual second client is
  unproven here, as before.
- **Whether the real client's first post-approval NamedMessage is a
  fork-u16 or stock-u64 header.** v6 handles and distinctly logs both;
  the next live v6 server.log settles which the 2026 build sends.
- TimeSync still unanswered, kinds 3–6 still logged-only, 0x2A cipher
  still unpinned (unchanged from v5).

## v7 — room-lane relay (kinds 3→4, 5→6) + stock TimeSync stream (2026-11-08, evening)

**Why v7 exists.** Through v6 the server only LOGGED the room-lane named
messages (kinds 3/5) and never answered TimeSync. New evidence says that
lane is load-bearing: on the real 2026 client Tachyon is the ROOM
transport, not voice-only, and the patch-2026 maintainers' own AGENTS.md
(riporipoteam-ctrl/patch-2026, "Transport fields and approval
observations") records of their working development server: "Named
message kinds 3/4 carried proxied room events, 5/6 room data. The
measured server-to-client proxied header was int32 sender, byte event,
int32 cache index (reader 0x3D33C10), then unchanged tagged payload.
Isolating rejected messages within Netcode batches let later requests
proceed and the development client enter the dorm." Their dev run also
observed account approval and Dissonance handshake/state packets (which
we already handle since v5/v6). Everything else is byte-identical to v6.

**Changes (server only):**

1. **Proxied relay, kind 3 → 4** (`Program.HandleNamedMessage`, new
   `NgoMessages.BuildRawNamedMessageBody`): an approved client's
   `RRClientToServerProxied` payload already begins with
   `[byte event][int32 cache index][tagged payload]` — exactly why the
   measured server→client reader (0x3D33C10) sees sender, then event,
   then cache index, then the unchanged payload. The server therefore
   forwards, to every OTHER approved session in the same instance, a
   kind-4 body whose content is `[int32 LE sender NGO clientId]`
   prepended to the original payload bytes, unchanged. Logging: sender,
   event byte, cache index, payload length, recipient count.
2. **Room-data relay, kind 5 → 6**: `RRClientToServer` payloads are
   forwarded VERBATIM (no header is measured for 5/6, so none is added)
   to every other approved session in the instance as kind 6. Logging:
   length, first bytes, recipient count. Server→client kinds (4/6)
   arriving FROM a client are now logged distinctly and consumed, not
   relayed.
3. **Raw named framing for room kinds** (`BuildRawNamedMessageBody`):
   `[u16 type][payload]` with NO i32 length prefix, matching how v5+
   parses non-Dissonance bodies (the fork's NamedMessage.Deserialize
   reads only the u16 type; the rest is receive data). The i32
   WriteValueSafe wrapper in `BuildNamedMessageBody` is now used ONLY
   for Dissonance type 2, as before — the room kinds never touch it.
4. **TimeSync — researched from the actual Unity Netcode source, and
   the answer is a stream, not a reply.** Sources (Unity-Technologies/
   com.unity.netcode.gameobjects, develop branch, fetched 2026-11-08):
   - `Runtime/Messaging/Messages/TimeSyncMessage.cs`: the message is a
     single field `public int Tick;` — `Serialize` is
     `BytePacker.WriteValueBitPacked(writer, Tick)`, `Deserialize`
     returns FALSE unless the owner `IsClient`, and `Handle` syncs the
     client's NetworkTimeSystem to `new NetworkTime(TickRate, Tick)`.
     So the body is exactly one bit-packed (ZigZag) int, and the
     direction is SERVER→CLIENT ONLY.
   - `Runtime/Timing/NetworkTimeSystem.cs`: only the server sends it —
     `Initialize()` hooks `OnTickSyncTime` solely when
     `LocalClient.IsServer`; it sends `new TimeSyncMessage { Tick =
     ServerTime.Tick }` to all ConnectedClientIds whenever
     `ServerTime.Tick % m_TimeSyncFrequencyTicks == 0`, with
     `k_TimeSyncFrequency = 1.0` ("1 synchronization message per
     second"), delivery Unreliable.
   Implemented exactly per that pin:
   - `NgoMessages.BuildTimeSyncBody/TryParseTimeSyncBody`: one
     bit-packed signed tick via this fork's BytePacker (which mirrors
     stock bit-packing per ngo-fork-batch-and-approval.md).
   - The server now SENDS TimeSync proactively: the main loop pumps
     `SendTimeSyncToApproved()` every 1 s — one message per approved
     session on ITS TimeSync index, in the peer's framing mode (framed
     vs legacy, the same rule as approvals), body = the server's
     current tick. First send per session is logged; the cumulative
     count rides the 60 s HEARTBEAT as `timeSyncSent=`. (Stock delivery
     is Unreliable; this server sends all batches ReliableOrdered —
     approval, named — and keeps that here.)
   - One shared tick clock: `ServerTick()` = elapsed seconds since
     server start × 30 (stock NetworkTickSystem starts at tick 0;
     stock TickRate default is 30). It feeds BOTH the TimeSync stream
     AND the approval's NetworkTick (previously `Environment.TickCount`
     — a value-source change only, wire format identical). Sharing the
     clock is required for coherence: stock feeds both from the same
     NetworkTickSystem, and a TimeSync tick on a different base than
     the approval tick would slam the client's time system backwards.
   - An INBOUND message at the TimeSync index is non-stock (stock
     clients never send one): it is captured with hex + a parsed-tick
     readout and NOT replied to — no fabricated request/response pair.
5. **Version bump**: `NgoMessages.ServerBuildVersion = "v7"`; the
   startup line prints `build=v7 … exeSha256=<hash of the shipped exe>`
   (for this publish: see below).

**Self-test** (test client extended; no assertions weakened — new
required probes were ADDED, and the harness's build-string assertion
moved v6→v7 with the version bump):
- Kind-3 proxied probe: raw kind-3 body, payload `[event=42][int32 LE
  cacheIndex=777]["room-proxied-probe"]` → server logs
  `proxied relay (kind 3->4) … event=42 cacheIndex=777 payload 23B,
  sender 1 prepended -> 0 recipient(s)` (probe body hex confirms the
  raw framing: `0300 2A 09030000 …` — type, event, cache, payload, no
  i32 wrapper).
- Kind-5 room-data probe: payload `"room-data-probe"` → server logs
  `room-data relay (kind 5->6) … 15B … -> 0 recipient(s)`.
- TimeSync: the client asserts a server TimeSync arrives and parses as
  exactly one bit-packed tick (`TIMESYNC_REPLY=PASS`, first observed
  tick=35 in the framed ×7 run); the server's first-send line and the
  non-stock inbound capture (probe tick 123456 read back correctly) are
  both in the log. The v6-era inbound TimeSync probe is kept — it now
  exercises the non-stock capture path.
- Harness additions: greps for both relay lines + their event/cache and
  recipient values, both client probe PASS lines, `TimeSync -> client`
  in the server log, `TIMESYNC_REPLY=PASS` in the client log.
- **Results — all PASS, session keys match on both sides in every run:**
  - framed × 7: PASS — session key sha256
    `92815be42a492605fc34a13d197faa006dbc2b7e76bd147b1133066f7bfbc77b`
  - framed × 1298: PASS — session key sha256
    `8f49151db5de33fa3affc7436da2f2779c3847eeb571e0c8e35496035e7420c5`
  - framed × 5000: PASS — session key sha256
    `c508d5053941bc4cd96c630f252ed2bb880e587ebb7a3da0570da6c1f1f1f4d4`
  - legacy × 7: PASS — session key sha256
    `9c945136c8e07a6db521e5bb5624e9c12667ba2279a6f74a76eb0911549652e8`
- `dotnet build -c Release`: **0 warnings, 0 errors** (TachyonWire,
  TachyonServer, TachyonTestClient).

**Published (NOT uploaded — Bob ships after review)**: win-x64
self-contained single-file publish, same command/layout as v1–v6:
`dist/win-x64/TachyonServer.exe`, **67,750,931 B**, sha256
`807e45c0267b55069c97dd37a53855bd48dd441d42c293b1cbbb642e4775286c`.
(The identical byte size to v6 is a single-file bundle padding
coincidence — the hash differs, the bundled TachyonServer.dll carries
the v7 relay/TimeSync strings, and the self-test server log proves the
running build prints `build=v7`.) Nothing was uploaded to the HF
mirror by this build; the launcher, rrflux, and matchmaking were not
touched.

### What remains unproven / open after v7

- **Whether the dorm now loads.** v7 relays the room lane and streams
  TimeSync per the measured/stock contracts, but only a real-client run
  with the v7 process actually running (startup line
  `build=v7 … exeSha256=807e45c0…`) is evidence. NOT claimed here.
- **Relay delivery to a real second client.** Both relay loops are
  exercised with 0 other sessions in this VM (documented netstack
  EPERM limit — the two-client harness cannot run here), exactly as
  with the voice relay and ClientState broadcast before. The forwarded
  byte construction is pinned by the measured reader layout, but its
  acceptance by a real receiving client is unproven.
- **Whether the fork's TimeSync deviates from stock.** The fork is a
  trimmed NGO (7 message types); its TimeSyncMessage type name hashes
  into the client's version list and the index resolves, but the fork's
  Serialize body was not independently decompiled — v7 implements the
  stock layout from the actual Unity source. If the 2026 build's
  client rejects it, its batches are self-delimiting (framed) and the
  message is one small batch per second, so a rejection cannot wedge
  the session; the live server.log + client behaviour will show it.
- **Tick rate of the real client.** ServerTick assumes the stock
  default TickRate 30. If the fork negotiates a different rate, the
  client's time system still syncs (it consumes absolute ticks), just
  with a proportionally scaled sense of elapsed time.
- 0x2A cipher still unpinned (unchanged); the client has stayed on the
  plaintext path in every live log so far.

## v8 — approval layout: two extra scalars before the Payload (2026-11-10)

**Why v8 exists — hard evidence from the REAL client's log, not a
synthetic client.** Armin's diagnostic (compiled 2026-11-10 from his PC)
includes the 2026 client log of the 2026-11-08 **21:43 UTC** attempt —
that attempt ran against **v7** (local TachyonServer.exe SHA-256 matched
the then-live HF sidecar, `807e45c0…`, so the v7 process was genuinely
the one serving). The client log reports a **Newtonsoft JSON parse
exception inside `TachyonClient.NetworkManager_OnClientConnectedCallback`:
"unexpected character D at position 0"**. After waiting, room load
reported **FailedToConnectToVoice (8719)**.

Read carefully, that one exception line pins a lot:

- The approval batch WAS dispatched and `ConnectionApproved.Handle` DID
  run — the exception is thrown in the callback that consumes the
  message, not in transport. So `ConnectionApprovedMessage.Deserialize`
  COMPLETED; it did not throw, did not run off the end, did not reject.
- Handle's job is parsing the Payload string as JSON
  (`{"DR":…,"KT":…}`). Newtonsoft says the string it got STARTS with
  'D' — a character that appears in our payload only at index 2
  (`{` `"` `D` `R` …).

**Byte-consumption reconstruction.** Through v7, `BuildApprovalBody`
wrote, after NetworkTick: `[packed-unsigned PayloadLength][packed-unsigned
per char]`, i.e. the values ~365, 123 ('{'), 34 ('"'), 68 ('D'), 82
('R'), … Suppose the 2026 build's Deserialize consumes **TWO extra
bit-packed scalar values between NetworkTick and the Payload string**
that the 2025 fork layout (docs/ngo-fork-batch-and-approval.md) does not
have. Then the client reads:

1. scalar #1 = our payload length (~365) — plausible small value, no error;
2. scalar #2 = our first char value, '{' = 123 — same;
3. ITS string length = our second char value, '"' = **34**;
4. ITS string = the next 34 packed values = our chars from index 2:
   `DR":"Carrot","KT":"…` — begins with **'D'**, and is SHORT (34 chars
   of a ~365-char payload), so Deserialize completes comfortably inside
   the body, Handle fires, Newtonsoft throws at position 0. ✔ Every
   observation fits: dispatch, completed Deserialize, Handle firing,
   'D' at position 0, the 8719 that follows when the callback dies.

Alternative formats were each simulated against the same evidence and
fail: stock raw-u32 length + raw UTF-16LE chars, bit-packed length +
raw (unpacked) chars, and a byte[] payload variant — none of them
produces a completed Deserialize whose Payload starts at our char
index 2 with 'D'. The two-extra-scalars story is the only one that does.

**What v8 changes (server only; everything else byte-identical to v7):**

1. **`NgoMessages.BuildApprovalBody`**: after NetworkTick, write TWO
   additional bit-packed unsigned values BEFORE the Payload length+chars:
   first = `ownerClientId` (same value as the OwnerClientId field),
   second = `0`. Rationale: mirrors the stock-NGO-2.x session-owner pair
   order, and also parses as a (count=1, elem=0) list under a list
   interpretation. If the real fields are something else, two small
   scalars are the safest filler — the callback only consumes DR/KT
   afterward; what matters is that the client's string read lands on our
   real length prefix.
2. **Approval ground-truth log** (`Program.cs`): the body is now built
   ONCE and then framed/wrapped (v7's legacy path rebuilt it with a
   second `ServerTick()` call — same format, potentially a tick apart;
   unifying removes that wart), and right after the APPROVED line the
   server logs `[ngo] approval body sent to client N: <len>B, payload
   <chars> chars, first 128B hex: <hex>` — so any future server.log
   shows exactly what went on the wire.
3. **Version bump**: `NgoMessages.ServerBuildVersion = "v8"`; the
   startup line prints `build=v8 … exeSha256=<hash of the shipped exe>`
   (for this publish: see below).

**Self-test** (test client's `ParseApproval` — the shared TachyonWire
inverse of the builder — extended to the v8 layout; assertions ADDED,
none weakened; the harness build-string grep moved v7→v8 with the bump,
as with every prior version, and two greps were added: the server's
approval-body hex line and the client's `APPROVAL_V8_LAYOUT=PASS`):

- After NetworkTick the client parser reads the two scalars and asserts
  **#1 == owner client id, #2 == 0** (`approval v8 scalars: #1=1 #2=0
  (ownerClientId=1)`), then reads the string and asserts it round-trips
  as the full approval JSON: `JsonDocument.Parse` succeeds,
  **DR == "Carrot"**, KT base64-decodes and RSA-decrypts, and the
  derived session key matches the server's — as before.
- Server log evidence (framed ×7 run): `[ngo] approval body sent to
  client 1: 433B, payload 195 chars, first 128B hex: 71427F88C611…`
  (195 chars is the self-test's synthetic KT; the real payload is
  ~365 chars).
- **Results — all PASS, session keys match on both sides in every run:**
  - framed × 7: PASS — session key sha256
    `f8f7b0500f180c0ac29bc6cbcd55ece224f1ac7a33ba8d53e41c87aac7c96aa1`
  - framed × 1298: PASS — session key sha256
    `133df7f8ca9a643eb65f26154abd299b34a3477c822c92685e54b7aa380d1b6f`
  - framed × 5000: PASS — session key sha256
    `9adb12854548ebdee4307e1d265f15de34764d05bf7c0f29931ee382461bbbc4`
  - legacy × 7: PASS — session key sha256
    `39dd7408da29b53e3fbb192aa1176c99a00da08fcaba37a35940f982585e2706`
- `dotnet build -c Release`: **0 warnings, 0 errors** (TachyonWire,
  TachyonServer, TachyonTestClient).

**HYPOTHESIS — stated plainly.** The v8 approval layout is a hypothesis
fitted to the real client's exception text by byte-consumption
reconstruction. It is NOT decompiled fact: the 2026
ConnectionApprovedMessage.Deserialize was never read directly, and the
self-test client SHARES the assumption (its parser is the builder's
inverse), so a self-test PASS proves only internal consistency — the
exact failure mode that made earlier synthetic-client results
worthless as protocol evidence (see v3/v4). **Only a real-client run
against the v8 process proves or kills this layout.** If a real run
still throws in that callback, the approval-body hex line now in
server.log plus the new exception text will pin the true consumption
counts directly, with no guessing left.

**Published (NOT uploaded — Bob ships after review)**: win-x64
self-contained single-file publish, same command/layout as v1–v7
(`dotnet publish -c Release -r win-x64 --self-contained true
-p:PublishSingleFile=true -o dist/win-x64`):
`dist/win-x64/TachyonServer.exe`, **67,751,443 B**, sha256
`68e2523a6fd3cdb1dd64f013d196730e3e320ebbdf2402f0bac7ec7374a0a54d` —
this is the hash the v8 startup line will print when this exe is the
running process. Nothing was uploaded to the HF mirror by this build;
the launcher, rrflux, and matchmaking were not touched.

### What remains unproven / open after v8

- **The dorm load itself.** Even with a correctly parsed approval, the
  post-approval path (room init, relays, TimeSync acceptance) is only
  proven against synthetic clients. NOT claimed here.
- **The two scalars' true identity.** They may be a session-owner pair,
  a list header, flags, or something else entirely; v8's values
  (ownerClientId, 0) are chosen to be safe under the likeliest readings.
- Everything listed under v7's unproven section (second-client relay
  delivery, fork TimeSync deviation, tick rate, 0x2A cipher) is unchanged.
