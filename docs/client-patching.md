# Client patching guide

How we turn the stock Nov 2022 Rec Room build into the RRFlux client.
Primary method: the reproducible `client/patch.py` script (no manual hex edits).

## Verified findings (2026-09-19, build 9857464)

- **IL2CPP metadata v27** (`global-metadata.dat`, 29.8 MB, magic `0xFAB11BAF`).
  String-literal table at file offset 256 (46,203 entries of
  `{u32 length, i32 dataOffset}`); raw literal bytes at 369,880.
  Patching = overwrite bytes in place (zero-pad) + update the length field.
  Replacement hosts must be **shorter-or-equal** to the originals.
- **Endpoint literals** (indices in the literal table):
  - `#39458` `https://auth.rec.net/Account/LoginWithToken?loginToken={0}&accountId={1}&redirectUrl={2}`
  - `#39462` `https://ns.rec.net`
  - `#39463` `https://rec.net`, `#39464` `https://rec.net/shop?utm_source=pc`,
    `#39465` `https://rec.net/{0}` (web links, optional)
  - `#39455/#39456` `https://api2.amplitude.com/httpapi|/identify` (telemetry)
  - `auth.rec.net` = 12 chars, `ns.rec.net` = 10 chars → replacement hostnames
    must fit those budgets.
- **Photon App IDs**: two GUIDs in `resources.assets` right after the
  `PhotonServerSettings` marker (file offsets 153674568, 153674612):
  `9372aa8d-d3f4-44a0-986d-419e145a2b83`,
  `e93ae440-f238-4b6c-848f-1df89faf14f5`.
  GUIDs are always 36 chars → byte-swap is trivially length-safe.
  Photon Cloud name servers (`ns.exitgamescloud.com`) are Photon's infra and
  stay as-is; our App ID routes the client to our Photon Cloud app.
- **No certificate pinning indicators** found (only stock .NET/Mono TLS API
  names). The client almost certainly uses default cert validation → our
  servers just need valid TLS certs.
- **Anti-cheat**: EAC is neutered by `patch.py` (default on):
  deletes the `EasyAntiCheat/` installer dir and replaces
  `RecRoom_Data/Plugins/x86_64/EasyAntiCheat.dll` with a minimal stub DLL
  (`client/eac_stub.py` builds it in pure Python — no Windows toolchain).
  The stub exports the 11 `Cerberus_*` functions the client P/Invokes as
  no-ops, so `EasyAntiCheat.Runtime.Initialize()` resolves and runs against
  dead stubs instead of the defunct EAC backend. CodeStage ACTk detectors
  are embedded client-side only; left in place for v1 (private server).
- Known residual: 2 copies of the amplitude URLs also live in the
  `fieldAndParameterDefaultValueData` blob (reflection-only default values).
  The runtime code path uses the patched literals; telemetry failing is
  harmless for a private revival.

## Tools (manual alternative)

| Tool | Use |
|---|---|
| [dnSpy v6.1.8](https://github.com/dnSpy/dnSpy/releases/tag/v6.1.8) | Reference only — our build is IL2CPP |
| [HxD](https://mh-nexus.de/en/downloads.php?product=HxD20) | Hex-edit `GameAssembly.dll` string blobs |
| [Metadata String Editor](https://github.com/JeremieCHN/MetaDataStringEditor/releases) | Manual metadata string edits |

> Prefer `client/patch.py` — it parses the v27 literal table, validates
> replacement lengths, swaps Photon GUIDs, refuses double-patching, and
> keeps `.bak` backups. Credentials/App IDs are passed via CLI/env, never
> stored in the repo.

1. Copy the stock build to a working dir (never patch the only copy).
2. `strings`-scan `GameAssembly.dll` for `rec.net` / `recroom.com` /
   `photon` to catalogue endpoints.
3. Open IL2CPP metadata in Metadata String Editor, replace official hosts
   with RRFlux hosts. **Length rule:** replacement strings must fit the
   original allocation — use equal-or-shorter hostnames, or rebuild the
   metadata properly.
4. Hex-verify in HxD that no stale official URLs remain in the hot paths.
5. Handle EAC: prevent the client from requiring EAC init at startup.
6. Smoke-test launch (needs a Windows box — see playtest notes).
7. Diff patched vs. stock; script the transform so it's reproducible
   (`tools/patch.py` — to be written after inspection).

## Reproducibility

Patching must be a **script**, not a manual ritual. Once the endpoint set
is known, `tools/patch.py` will take a stock build + a config file
(endpoints, Photon App ID) and emit a patched build. The launcher then
ships the patched output.
