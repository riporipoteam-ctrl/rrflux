# Flux Rec — Windows Defender / SmartScreen mitigation

## What happened (2026-09-24)
On Armin's PC, Windows Security quarantined/removed a game file — almost
certainly `RecNetPlugin.dll` or the Goldberg `steam_api64.dll` (both are
classic heuristic targets: unsigned .NET DLL / emulator DLL). Without the
redirect plugin the client talks to the dead real backend and hangs at
"Connecting to server..." forever. Separately, SmartScreen flags every new
`FluxRec-Setup.exe` as "Unknown publisher" because we ship unsigned.

## What the installer now does automatically (game/installer/src/defender.rs)
1. **Defender exclusion at install time** — the game dir goes into
   `ExclusionPath` (+ `RecRoom.exe` / `FluxRecLauncher.exe` into
   `ExclusionProcess`). Idempotent, verified, silent, fail-soft. Needs
   admin (the installer already elevates); if Tamper Protection is on, the
   call is denied and we log manual steps instead.
2. **Self-heal on every launch** — the launcher verifies the plugin DLL,
   the Steam bypass, and the `ns.rec.net` mapping, and repairs them
   automatically (re-download / re-apply hosts / re-add exclusion).
3. **Unblock-File** on every internet-sourced file we write, so .NET/BepInEx
   never refuse to load a DLL over Mark-of-the-Web.
4. **Conditional hosts writes** — the hosts file is only touched when
   `ns.rec.net` does NOT already resolve to the backend (writing hosts is
   itself AV-suspicious).

## SmartScreen "Unknown publisher"
- Cause: the setup is Authenticode-unsigned (zero-spend rule: no cert).
- Free remediation: submit each release to Microsoft as a software
  developer at https://www.microsoft.com/en-us/wdsi/filesubmission
  ("incorrectly detected as malware"). Per-hash, manual review, takes days.
  There is no allowlist program.
- Reputation accrues per file hash for unsigned binaries: **every new
  version gets flagged again** until it builds install count. Keep the
  publisher metadata (winres version info) consistent across releases.
- What to tell players on the blue screen: click **More info → Run anyway**.

## What we deliberately never do
Disable SmartScreen/Defender programmatically, stop security services, or
touch Tamper Protection. That is malware behavior and gets software flagged
*harder*. If Defender still quarantines a file, the launcher self-heals it,
or add the folder manually: Windows Security > Virus & threat protection >
Manage settings > Exclusions.
