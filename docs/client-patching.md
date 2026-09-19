# Client patching guide

How we turn the stock Nov 2022 Rec Room build into the RRFlux client.
(Based on the community patching workflow — dnSpy/HxD/metadata editing.)

## Tools

| Tool | Use |
|---|---|
| [dnSpy v6.1.8](https://github.com/dnSpy/dnSpy/releases/tag/v6.1.8) | Inspect managed code **if** a Mono build; ours is IL2CPP, so dnSpy is for reference/older builds |
| [HxD](https://mh-nexus.de/en/downloads.php?product=HxD20) | Hex-edit `GameAssembly.dll` string blobs |
| [Metadata String Editor](https://github.com/JeremieCHN/MetaDataStringEditor/releases) | Edit IL2CPP `global-metadata.dat` strings cleanly (preferred over raw hex) |

> Our build is **IL2CPP**: there are no plain .NET assemblies to edit with
> dnSpy. All endpoint strings live in `GameAssembly.dll` (native) and the
> IL2CPP metadata. The Metadata String Editor is the primary tool.

## Targets (to confirm during inspection)

1. **API base URLs** — `https://*.rec.net`, `https://*.recroom.com`
   (auth, profiles, rooms, store, etc.)
2. **Realtime / Photon config** — Photon endpoint + App ID location
   (may be constructed at runtime — check `globalgamemanagers` too)
3. **CDN / asset URLs** — where the client fetches bundles/assets
4. **EAC bootstrap** — code that loads `EasyAntiCheat_x64.dll` /
   calls `EasyAntiCheat_Setup.exe`; needs neutering
5. **Certificate pinning** — if the client pins certs, endpoint swaps alone
   won't be enough (TLS interception or deeper patches needed)

## Procedure

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
