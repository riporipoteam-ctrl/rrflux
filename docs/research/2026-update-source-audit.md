# Flux Rec production update-source audit

Date: 2026-10-23  
Audited production revision: [`41d639f`](https://github.com/riporipoteam-ctrl/rrflux/commit/41d639f53a45ba589b754053670b223f9bbe6490) (`main`, launcher v1.0.49)

This note distinguishes the launcher self-update, the production game install,
and the older manifest-based game updater. They are separate paths.

## Result

The launcher update on GitHub does not update the game build. Production
v1.0.49 still installs the 2025 client from a hard-coded ZIP URL and repairs an
existing 2025 installation in place. That is why updating the Ripo Team
Launcher leaves the in-game version at `20250718.01`.

Hugging Face does contain a 4,480,087,042-byte file named
`client-2026-03/march-2026-client.zip`. It is not represented by the live game
manifest, and there is no per-file 2026 manifest proving that its contents match
the three fingerprints required by `patch-2026`. The existing CI run proves
that the archive extracts and contains the expected top-level filenames, but it
does not prove a successful game boot or fingerprint compatibility. GitHub
Releases contains launcher/setup binaries only and no 2026 game payload.

Therefore the 2026 archive is a candidate source already present in Flux Rec's
Hugging Face storage, but it is not yet a release-grade, verified update source.
It must pass the verification and atomic migration gates below before the live
installer is pointed at it.

## What production currently downloads

### Launcher self-update: GitHub Releases

The installed launcher asks the GitHub Releases API for the newest Flux Rec
release containing `FluxRec-Setup.exe`. Release v1.0.49 contains only
`FluxRec-Setup.exe` and `SHA256SUMS.txt`; it contains no game client archive.

- Source: [`game/installer/src/updater.rs`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/game/installer/src/updater.rs#L25-L27)
- Live release: [ripo-team-launcher-v1.0.49](https://github.com/riporipoteam-ctrl/rrflux/releases/tag/ripo-team-launcher-v1.0.49)

Installing a newer setup runs the setup again, but the game-client source is
still selected by the setup binary described next.

### Production game install: hard-coded 2025 ZIP

At revision `41d639f`, fresh installs use this mirror and MD5:

```text
https://s3.g.megas4.com/2koayuyiwxv4groxzwdbbxg43cwustavrkvfb/recflare/2025/client.zip
MD5 6820e89bff41906ded7f5c066027f1d6
```

- Source: [`game/installer/src/main.rs`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/game/installer/src/main.rs#L46-L50)
- The install flow downloads that ZIP into a staging directory, extracts it,
  applies components, verifies it, and then promotes the staged directory.
  Source: [`build_full_install`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/game/installer/src/main.rs#L1338-L1371)
- An existing non-2026 installation takes the in-place `upgrade_install` path,
  which refreshes managed components and does not replace the client archive.
  Source: [`run_install`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/game/installer/src/main.rs#L1156-L1229)

This is the production path responsible for retaining `20250718.01`.

### Separate manifest updater: Hugging Face manifest points to a 2022 build

The shared bootstrap/downloader defaults to:

```text
https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/manifest.json
```

- Source: [`game/common/src/lib.rs`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/game/common/src/lib.rs#L11-L15)
- Live manifest: [manifest.json](https://huggingface.co/datasets/Echoxr/rrflux-game/blob/main/manifest.json)

Live inspection on 2026-10-23 found:

```text
version: 0.5.0-showdown
files: 3,742
wipe_required: absent (deserializes to false)
all 3,742 URLs: archive.recagain.site
core URL prefix: https://archive.recagain.site/2022-08-26T07-03-14Z/
RecRoom.exe SHA-256: 153860af085119fa5c45d97f109f341fb8dde577dc34207e1bb94c1fcc83d16f
GameAssembly.dll SHA-256: b908276ffe4212267c981db72cfb9bf495c50a4a1b9f5291957971042fbd842d
Recroom_Release.exe SHA-256: 42a71f86609b2ce9e29411ff5a42c4beaefe2ba4bc7a1288c94399cbc5496e6d
```

This manifest is neither the production 2025 ZIP nor the March 2026 client.
Pointing production at it would switch to the Showdown-era 2022 file set.

## March 2026 payload evidence

### Hugging Face

The dataset tree exposes one candidate archive:

```text
path: client-2026-03/march-2026-client.zip
size: 4,480,087,042 bytes
Git LFS object SHA-256: f36e9b527ac1acb67482c0ab42c1c618cf224d9498a9323d4b49a52dfb9c722b
uploaded: 2026-10-03 in commit e65c17dada1e481d580c89cc7a188efdb3f98618
```

- Dataset file: [march-2026-client.zip](https://huggingface.co/datasets/Echoxr/rrflux-game/blob/main/client-2026-03/march-2026-client.zip)
- Dataset commit: [e65c17d](https://huggingface.co/datasets/Echoxr/rrflux-game/commit/e65c17dada1e481d580c89cc7a188efdb3f98618)

The file exists and has a content-addressed LFS object. However, no live JSON
manifest lists its individual files, sizes and SHA-256 values.

A proposed RecAgain fallback inspected during this audit exposed another
identity warning:

```text
/download/manifest/5037299340901764828
  4,480,087,042 bytes
  filename: 2026-03-31T03-05-36Z_473be467.zip

/download/manifest/8867414017562658247
  4,543,084,133 bytes
  filename: 2026-05-12T18-46-14Z_129011d0.zip
```

The first endpoint has the same byte length as the Hugging Face archive, while
the second uses the manifest ID required by `patch-2026` but has different
content length and naming metadata. Neither naming convention proves the
client's internal build identity. This contradiction makes the three module
fingerprints, rather than an archive filename, date, size, or URL identifier,
the mandatory compatibility gate.

### GitHub CI evidence and its limit

The manual workflow downloaded and extracted the Hugging Face archive and found:

```text
RecRoom.exe                 672,800 bytes
Recroom_Release.exe       1,055,776 bytes
Referee.dll              62,253,104 bytes
RefereeClientApp.exe      4,184,624 bytes
RefereeClientInstaller.exe  331,808 bytes
```

`RefereeClientInstaller.exe -install` exited 0, and the launcher process was
still running after 30 seconds. The workflow then reported **no Player.log**.
It did not calculate the patch's required fingerprints, inject `2026patch.dll`,
assert that a game window reached sign-in, or fail when Player.log was absent.
Its green status therefore proves archive availability/extraction, not a working
patched 2026 client.

- Workflow source: [`test-march2026-launch.yml`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/.github/workflows/test-march2026-launch.yml)
- Run: [37195206121](https://github.com/riporipoteam-ctrl/rrflux/actions/runs/37195206121)

### GitHub Releases

The `riporipoteam-ctrl/rrflux` releases inspected on 2026-10-23 contain setup,
mobile launcher/runtime, and checksum assets. None contains a Windows game
client or March 2026 game archive.

### `patch-2026` compatibility gate

`patch-2026` supports only client `20260323.14`, Steam manifest
`8867414017562658247`, and explicitly warns against mixing files from other
builds.

- Primary source: [`patch-2026` README](https://github.com/recflare/patch-2026#installation)
- The pinned source requires these SHA-256 fingerprints:

| File | Required SHA-256 |
| --- | --- |
| `GameAssembly.dll` | `A6D90770CDFDA4043EADB278A16198ED0907C3BA2AC09BC37D06CD4DD22A1D84` |
| `Referee.dll` | `5868C90A7C67757F3B678AA356CBD933D646C9749E0ACAA31B7DCC9EAEE12EAD` |
| `UnityPlayer.dll` | `30F4A99C49566B2C5045469B5A39572DD2298ADC31F7FE2BC39D8BB8FC9D75BB` |

Source: [`BuildGuard` documentation at pinned submodule revision](https://github.com/recflare/patch-2026/blob/6fa58295e7be492be4bf1406c2fbb0fca4370f1b/AGENTS.md#client-identity-module-bases-and-layout)

The current Hugging Face archive has not been connected to those three hashes
by a public manifest or an asserting CI run.

## Required release manifest

After confirming the project is authorized to distribute the candidate client,
publish an immutable, versioned file tree and a manifest dedicated to
`20260323.14`. Do not overwrite the current Showdown manifest until validation
and rollback testing pass.

Minimum manifest shape supported by the existing parser:

```json
{
  "version": "20260323.14",
  "wipe_required": true,
  "files": [
    {
      "path": "RecRoom.exe",
      "size": 672800,
      "sha256": "<sha256>",
      "url": "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/<immutable-revision>/client-20260323.14/RecRoom.exe"
    }
  ]
}
```

Release requirements:

1. Include every file in the 2026 install, with normalized relative paths,
   byte sizes, and SHA-256 hashes. Reject duplicate paths, absolute paths and
   `..` traversal when generating and consuming the manifest.
2. Pin URLs to an immutable Hugging Face commit or release tag. A mutable
   `/resolve/main/` URL cannot guarantee that the manifest and payload remain
   the same release.
3. Assert that the three files above exactly match `patch-2026`'s fingerprints.
   Also require `RecRoom.exe`, `RecRoom_Data`, `Recroom_Release.exe`,
   `RefereeClientApp.exe`, and `RefereeClientInstaller.exe` before promotion.
4. Set `wipe_required: true`. The 2025 and 2026 layouts differ and must never
   be overlaid or partially diffed.
5. Keep patch and launcher-owned files outside the proprietary payload manifest,
   or mark ownership explicitly. The installer should add the five patch files
   after verifying the stock client:
   `2026patch.dll`, `Injector.exe`, `2026patch.ini`, `RecRoomScreen.bat`, and
   `RecRoomVR.bat`.
6. Publish a detached signature or another authenticated manifest digest in the
   launcher release. Per-file hashes protect downloads only if the manifest
   itself is trusted.

## Required atomic 2025 to 2026 switch

Changing only `CLIENT_ZIP_MIRRORS` is insufficient. On v1.0.49 an existing
2025 installation goes through `upgrade_install`, which never downloads the
client ZIP. The migration must be a complete staged build switch:

1. Detect the installed build by layout plus known fingerprints, not by a
   filename alone.
2. Download the immutable 2026 archive or all files in its manifest into a
   sibling staging directory. Verify the archive digest, every file hash, the
   required file set, and the three `patch-2026` fingerprints before touching
   the live install.
3. Apply only 2026-compatible components in staging. Do not write
   `2025Patch.dll`, `2025patch.ini`, the 2025 Steam bypass, or 2025-specific
   branding bundles into the 2026 tree.
4. Generate `2026patch.ini` with the production discovery host and service
   domain. Preserve only the validated public `VoiceKeyXml` value from the old
   config. The existing renderer already models these fields:
   [`render_2026patch_ini`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/game/installer/src/bepinex.rs#L50-L84).
5. Run `Injector.exe --check-install` against the staged client and treat any
   fingerprint mismatch as a hard failure.
6. Atomically rename the live 2025 directory to a rollback directory and the
   verified staging directory to the live path. The existing `Staging::commit`
   already provides this directory-swap primitive:
   [`transaction.rs`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/game/installer/src/transaction.rs#L92-L151).
7. Install or repair the Referee Windows service **after** the directory swap
   from the final live path. The current fresh-build helper invokes the service
   installer while building the staging directory; registering a path under
   `<dir>.staging-<pid>` can become stale after the rename.
8. Launch through the 2026 batch/injector order only after post-commit checks.
   Retain the previous directory until the first patched launch reaches an
   explicit health signal; then remove it. On failure, restore the previous
   directory and its service configuration.
9. Store the installed build ID and the exact manifest digest outside the game
   directory so repair and future updates can identify the deployed release.

The older bootstrap implementation must not be reused unchanged for this switch:
it deletes the live game directory before downloading when `wipe_required` is
true and then updates files in place. That is a clean file set, but it is not an
atomic or rollback-safe migration. Source:
[`game/bootstrap/src/launcher.rs`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/game/bootstrap/src/launcher.rs#L309-L369).

## Configuration and repair changes needed with the manifest

- Make fresh install and existing 2025 migration select the 2026 staged path.
  The `is_2026_install` branch currently only configures a 2026 client that is
  already present; it does not obtain one.
- Split final verification by build. A 2026 verifier must require the five
  patch files and the three fingerprinted stock modules, and must reject stale
  2025 patch files.
- Split Library “Verify files” by build. Its current `smart_repair` directly
  restores embedded `2025Patch.dll` and `2025patch.ini`, even though the lower
  patch abstraction has 2026-aware helpers. Source:
  [`game/installer/src/library.rs`](https://github.com/riporipoteam-ctrl/rrflux/blob/41d639f53a45ba589b754053670b223f9bbe6490/game/installer/src/library.rs#L674-L732).
- Gate Steam replacement, branding bundle mutation, and binary patching by
  exact 2026 hashes. Existing 2025 transformations must not run merely because
  similarly named files exist in the new layout.
- Update the installer version and publish a new `FluxRec-Setup.exe` only after
  the candidate manifest passes an asserting end-to-end test. That test must
  fail on a fingerprint mismatch, missing Player.log/window health signal,
  incorrect displayed version, failed patch injection, or failed backend
  discovery.

## Release decision

The storage location for a possible 2026 client already exists on Hugging Face.
The missing production artifact is a trustworthy per-file 20260323.14 manifest
and a passing, asserting compatibility test. Until those exist, switching the
live installer URL would replace the known 2025 build with an archive whose
patch compatibility and successful boot have not been proved.

## Post-audit fingerprint validation

After this audit identified the mismatch between the two candidate archives,
the required stock modules were read directly from each remote ZIP with HTTP
range requests. The repository utility
`scripts/verify-remote-zip.mjs` parses ZIP64 metadata and hashes selected files
without downloading the complete archive.

Manifest `8867414017562658247` (4,543,084,133 bytes) produced all three hashes
required by the pinned patch source:

```text
GameAssembly.dll  a6d90770cdfda4043eadb278a16198ed0907c3ba2ac09bc37d06cd4dd22a1d84
Referee.dll       5868c90a7c67757f3b678aa356cbd933d646c9749e0acaa31b7dcc9eaee12ead
UnityPlayer.dll   30f4a99c49566b2c5045469b5a39572dd2298adc31f7fe2bc39d8bb8fc9d75bb
```

The Hugging Face `march-2026-client.zip` and byte-identical RecAgain manifest
`5037299340901764828` failed the first fingerprint check: its
`GameAssembly.dll` hash is
`8efde88e39a0e6f2b810d089496e262354d3df8cd0fc2c783d4309b936b55b49`.
It must not be shipped with `patch-2026`.
