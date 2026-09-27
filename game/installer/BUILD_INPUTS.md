# Flux Rec v0.2.0 — verified build inputs (2026-09-27)

## Pinned client archive
- File: `~/workspace/winetest/client.zip` (also the GitHub release asset)
- MD5: `4c4a94624eba99028bb36445ccb03253` (matches `CLIENT_ZIP_MD5` in installer)
- SHA-256: `4068661971c87eaa905818126db66d33e326d378f88b82b8a46ea8398f4412a9`
- Size: 4,080,598,359 bytes compressed / 7,730,854,183 uncompressed (7,136 files)
- `unzip -t`: no errors

## GameAssembly.dll (extracted, 20230414 build)
- MD5: `746a8d9b49671f1329c8f7627583b02d`
- Size: 156,069,888 bytes
- This is the gate for the embedded interop zip: the interop assemblies were
  generated from a GameAssembly with this exact MD5.

## Interop archive
- File: `game/plugin-src/interop-cache/interop-20230414.zip`
- 15,739,471 bytes compressed / 56,580,608 uncompressed, 283 DLLs
- Embedded in the installer as `assets/fluxloader-interop-20230414.zip`

## Doorstop (winhttp.dll)
- File: `game/installer/assets/fluxloader/winhttp.dll`
- Official Doorstop 4.5.0 x64, byte-identical to `~/workspace/fluxloader/vendor/doorstop/doorstop_win_release_4.5.0.zip` (x64)
- MD5: `e9cefd81b20f8ab4bd859b561e606214`, 26,112 bytes
- Verified 2026-09-27. Resolves the 4.3.0-vs-4.5.0 doc contradiction: we ship official 4.5.0.

## Firebase
- The game client and RecNetPlugin contain ZERO Firebase references
  (verified: 0 matches in interop assemblies, 0 matches in plugin source).
- Firebase's role is backend-side only: the Cloudflare Worker (`ns.ripo-ripoteam.workers.dev`)
  uses Firebase Admin (project flux-544a6) for auth/token verification + Firestore data.
  The ns worker is live (store stubs return 200, hot rooms endpoint returns data).
- Installer responsibility: bake the ns host. No client-side Firebase config exists or is needed.
