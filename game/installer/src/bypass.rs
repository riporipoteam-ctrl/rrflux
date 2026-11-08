//! Steam bypass orchestration.
//!
//! Primary method: the Goldberg emulator (gbe_fork) — proven on cloud
//! Windows runners to boot the 2025 client with no Steam client installed.
//! Fallback: our own full Steam emulator `steam_api64.dll` (clean-room,
//! built from `game/bootstrap/steam_emu` for the MinGW target — zero VC++
//! runtime dependency), exporting the complete 995-function surface of
//! the stock DLL, `SteamInternal_CreateInterface` included. Proven on a
//! real Windows laptop to boot the 2026 client (2026-11-08).
//!
//! History: the previous fallback was a minimal C stub with ~6 flat
//! exports. It could never boot the 2026 client — BootSequence resolves
//! `SteamInternal_CreateInterface` first and died with
//! EntryPointNotFoundException — yet `verify` below rubber-stamped it as
//! OK, so broken installs never healed. `verify` now rejects any DLL
//! that lacks the boot export, so those installs repair themselves.
//!
//! The bypass is verified — not just installed — and the same verify/repair
//! path runs from the installer AND from the launcher (`--play`), so a
//! broken or reverted bypass is healed automatically instead of ever
//! letting the game hit "Failed to initialize Steam Platform" or crash.

use std::path::Path;

/// Fallback emulator DLL, embedded at compile time. Built from
/// `game/bootstrap/steam_emu` (`cargo build --release --target
/// x86_64-pc-windows-gnu`); links only against kernel32/msvcrt — never
/// the VC++ runtime. Despite the legacy file name this is NOT the old
/// 6-export C stub: it exports the full stock surface (995 functions).
pub static STUB_DLL_BYTES: &[u8] = include_bytes!("../assets/steam_api64_stub.dll");

/// Which bypass is (or should be) in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassMethod {
    /// Goldberg emulator — the proven primary method.
    Goldberg,
    /// Flux Rec Steam emulator — automatic fallback, zero VC++ dependency.
    Stub,
}

/// Pure selection logic: Goldberg whenever it is available; stub otherwise.
/// Unit-tested.
pub fn choose_method(goldberg_available: bool) -> BypassMethod {
    if goldberg_available {
        BypassMethod::Goldberg
    } else {
        BypassMethod::Stub
    }
}

/// Result of inspecting an install dir's bypass state.
#[derive(Debug, PartialEq, Eq)]
pub enum BypassState {
    /// A working bypass is in place.
    Ok(BypassMethod),
    /// Broken or missing — carries the human-readable reason.
    Broken(String),
}

fn plug_dir(dir: &Path) -> std::path::PathBuf {
    // 2026 client uses RecRoom_Data, 2025 uses Recroom_Release_Data.
    // (2023 also used RecRoom_Data, but that's identified by BepInEx.)
    let p2026 = dir.join("RecRoom_Data").join("Plugins").join("x86_64");
    if p2026.join("steam_api64.dll").exists() || dir.join("RecRoom_Data").is_dir() {
        return p2026;
    }
    // v0.3.1: 2025 client layout.
    dir.join("Recroom_Release_Data").join("Plugins").join("x86_64")
}

/// The export the client's BootSequence resolves before anything else
/// (`Steamworks.CSteamAPIContext.Init` P/Invokes it). The name sits in
/// plain ASCII in the PE export directory of every capable DLL — the
/// stock Valve DLL, the Goldberg emulator and our own emulator all carry
/// it; the retired minimal C stub does not.
const BOOT_EXPORT: &[u8] = b"SteamInternal_CreateInterface";

/// True when the DLL bytes contain the boot export's name.
fn dll_has_boot_export(bytes: &[u8]) -> bool {
    bytes.windows(BOOT_EXPORT.len()).any(|w| w == BOOT_EXPORT)
}

/// Inspect the install dir and report the bypass state. Pure filesystem
/// checks — safe to run on every `--play`.
pub fn verify(dir: &Path) -> BypassState {
    let plug = plug_dir(dir);
    let dll = plug.join("steam_api64.dll");
    let settings = plug.join("steam_settings");
    let appid = settings.join("steam_appid.txt");
    let ifaces = settings.join("steam_interfaces.txt");

    if !dll.exists() {
        return BypassState::Broken("steam_api64.dll is missing".to_string());
    }
    let cur = match std::fs::read(&dll) {
        Ok(b) => b,
        Err(_) => return BypassState::Broken("steam_api64.dll is unreadable".to_string()),
    };
    // Detect a reverted bypass: if the stock DLL backup we made exists and
    // the live DLL is byte-identical to stock, something (re-extract,
    // "verify files", AV quarantine+restore) undid the swap.
    let backup = plug.join("steam_api64.dll.fluxrec-stock");
    if backup.exists() {
        if let Ok(stock) = std::fs::read(&backup) {
            if cur == stock {
                return BypassState::Broken(
                    "the stock steam_api64.dll is back — the bypass was reverted".to_string(),
                );
            }
        }
    }
    // A DLL without the boot export can never start the client: the
    // BootSequence P/Invokes SteamInternal_CreateInterface before
    // anything else and dies with EntryPointNotFoundException. The
    // retired minimal C stub (~6 exports, the fallback up to v1.0.76) is
    // exactly such a DLL — report it broken so the repair path replaces
    // it instead of rubber-stamping it as OK.
    if !dll_has_boot_export(&cur) {
        return BypassState::Broken(
            "steam_api64.dll lacks SteamInternal_CreateInterface — an outdated stub is installed"
                .to_string(),
        );
    }
    match std::fs::read_to_string(&appid) {
        Ok(s) if s.trim() == crate::STEAM_APP_ID => {}
        _ => {
            return BypassState::Broken(
                "steam_settings/steam_appid.txt is missing or wrong".to_string(),
            )
        }
    }
    if !ifaces.exists() {
        return BypassState::Broken(
            "steam_settings/steam_interfaces.txt is missing".to_string(),
        );
    }
    let method = if cur == STUB_DLL_BYTES {
        BypassMethod::Stub
    } else {
        BypassMethod::Goldberg
    };
    BypassState::Ok(method)
}

/// Write the `steam_settings` sidecar files both bypass methods need.
fn write_steam_settings(plug: &Path) -> Result<(), String> {
    let settings_dir = plug.join("steam_settings");
    std::fs::create_dir_all(&settings_dir).map_err(|e| e.to_string())?;
    std::fs::write(settings_dir.join("steam_appid.txt"), crate::STEAM_APP_ID)
        .map_err(|e| e.to_string())?;
    std::fs::write(
        settings_dir.join("steam_interfaces.txt"),
        crate::STEAM_INTERFACES,
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Install the fallback emulator bypass: back up stock once, swap in the
/// emulator, write settings, remove the legacy root `steam_appid.txt`.
pub fn install_stub(dir: &Path) -> Result<(), String> {
    let plug = plug_dir(dir);
    let dll = plug.join("steam_api64.dll");
    if !dll.exists() {
        return Err(
            "steam_api64.dll not found under the game dir — client extract broken?"
                .to_string(),
        );
    }
    let backup = plug.join("steam_api64.dll.fluxrec-stock");
    if !backup.exists() {
        std::fs::copy(&dll, &backup).map_err(|e| format!("steam dll backup failed: {e}"))?;
    }
    std::fs::write(&dll, STUB_DLL_BYTES).map_err(|e| format!("stub dll install failed: {e}"))?;
    write_steam_settings(&plug)?;
    let _ = std::fs::remove_file(dir.join("steam_appid.txt"));
    println!("[steam] Flux Rec steam emulator installed (fallback, no VC++ runtime needed).");
    Ok(())
}

/// Full bypass pipeline: VC++ runtime first (the vs22-built emulator DLL
/// cannot load without it — this was the v0.1.7 crash), then Goldberg;
/// on ANY Goldberg failure, fall back to the stub automatically.
/// Returns the method that ended up in place. Verifies — never assumes.
pub async fn apply_steam_bypass(
    client: &reqwest::Client,
    dir: &Path,
    progress: &crate::progress::Progress,
) -> Result<BypassMethod, String> {
    #[cfg(windows)]
    crate::vcredist::ensure_installed(client, progress).await?;

    let goldberg_ok = crate::apply_goldberg_steam_fix(client, dir, progress)
        .await
        .map_err(|e| {
            eprintln!("[steam] WARNING: Goldberg emulator failed ({e}); trying stub fallback.");
            e
        })
        .is_ok();

    match choose_method(goldberg_ok) {
        BypassMethod::Goldberg => match verify(dir) {
            BypassState::Ok(BypassMethod::Goldberg) => {
                println!("[steam] Goldberg emulator installed and verified.");
                Ok(BypassMethod::Goldberg)
            }
            other => {
                eprintln!(
                    "[steam] WARNING: Goldberg install did not verify ({other:?}); \
                     trying stub fallback."
                );
                install_stub(dir).map(|()| BypassMethod::Stub)
            }
        },
        // Goldberg unavailable: straight to the stub.
        BypassMethod::Stub => install_stub(dir).map(|()| BypassMethod::Stub),
    }
}

/// Re-run the bypass pipeline to heal a broken install. Idempotent: safe to
/// call on every `--play`.
pub async fn repair_bypass(
    client: &reqwest::Client,
    dir: &Path,
    progress: &crate::progress::Progress,
) -> Result<BypassMethod, String> {
    progress.set_stage("Repairing Steam bypass…");
    apply_steam_bypass(client, dir, progress).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_plug(dir: &Path) -> std::path::PathBuf {
        let p = plug_dir(dir);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fluxrec-bypass-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn choose_method_prefers_goldberg() {
        assert_eq!(choose_method(true), BypassMethod::Goldberg);
        assert_eq!(choose_method(false), BypassMethod::Stub);
    }

    #[test]
    fn verify_detects_missing_dll() {
        let d = tmp("missing-dll");
        let p = fake_plug(&d);
        std::fs::create_dir_all(p.join("steam_settings")).unwrap();
        std::fs::write(p.join("steam_settings").join("steam_appid.txt"), "471710").unwrap();
        std::fs::write(p.join("steam_settings").join("steam_interfaces.txt"), "x").unwrap();
        assert!(matches!(verify(&d), BypassState::Broken(_)));
    }

    #[test]
    fn verify_detects_missing_settings() {
        let d = tmp("missing-settings");
        let p = fake_plug(&d);
        std::fs::write(p.join("steam_api64.dll"), b"emulator-bytes").unwrap();
        assert!(matches!(verify(&d), BypassState::Broken(_)));
    }

    #[test]
    fn verify_detects_wrong_appid() {
        let d = tmp("wrong-appid");
        let p = fake_plug(&d);
        std::fs::write(p.join("steam_api64.dll"), b"emulator-bytes").unwrap();
        let s = p.join("steam_settings");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("steam_appid.txt"), "480").unwrap(); // legacy trick
        std::fs::write(s.join("steam_interfaces.txt"), "x").unwrap();
        assert!(matches!(verify(&d), BypassState::Broken(_)));
    }

    #[test]
    fn verify_detects_reverted_stock_dll() {
        let d = tmp("reverted");
        let p = fake_plug(&d);
        std::fs::write(p.join("steam_api64.dll"), b"stock-bytes").unwrap();
        std::fs::write(p.join("steam_api64.dll.fluxrec-stock"), b"stock-bytes").unwrap();
        let s = p.join("steam_settings");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("steam_appid.txt"), "471710").unwrap();
        std::fs::write(s.join("steam_interfaces.txt"), "x").unwrap();
        match verify(&d) {
            BypassState::Broken(reason) => assert!(reason.contains("reverted")),
            other => panic!("expected Broken(reverted), got {other:?}"),
        }
    }

    #[test]
    fn verify_ok_goldberg_and_stub() {
        let d = tmp("ok");
        let p = fake_plug(&d);
        let s = p.join("steam_settings");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("steam_appid.txt"), "471710").unwrap();
        std::fs::write(s.join("steam_interfaces.txt"), "x").unwrap();

        std::fs::write(
            p.join("steam_api64.dll"),
            b"emulator-bytes\x00SteamInternal_CreateInterface\x00",
        )
        .unwrap();
        assert_eq!(verify(&d), BypassState::Ok(BypassMethod::Goldberg));

        std::fs::write(p.join("steam_api64.dll"), STUB_DLL_BYTES).unwrap();
        assert_eq!(verify(&d), BypassState::Ok(BypassMethod::Stub));
    }

    #[test]
    fn verify_detects_outdated_stub_without_boot_export() {
        // Regression (v1.0.76): the minimal C stub satisfied every old
        // check — file present, settings correct — yet can never boot
        // the client. It must read as Broken so --play repairs it.
        let d = tmp("outdated-stub");
        let p = fake_plug(&d);
        std::fs::write(p.join("steam_api64.dll"), b"minimal-stub-bytes").unwrap();
        let s = p.join("steam_settings");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("steam_appid.txt"), "471710").unwrap();
        std::fs::write(s.join("steam_interfaces.txt"), "x").unwrap();
        match verify(&d) {
            BypassState::Broken(reason) => {
                assert!(reason.contains("SteamInternal_CreateInterface"))
            }
            other => panic!("expected Broken(boot export), got {other:?}"),
        }
    }

    #[test]
    fn install_stub_swaps_and_writes_settings() {
        let d = tmp("install-stub");
        let p = fake_plug(&d);
        std::fs::write(p.join("steam_api64.dll"), b"stock-bytes").unwrap();
        std::fs::write(d.join("steam_appid.txt"), "480").unwrap(); // legacy file

        install_stub(&d).unwrap();

        assert_eq!(std::fs::read(p.join("steam_api64.dll")).unwrap(), STUB_DLL_BYTES);
        assert_eq!(
            std::fs::read(p.join("steam_api64.dll.fluxrec-stock")).unwrap(),
            b"stock-bytes"
        );
        assert_eq!(
            std::fs::read_to_string(p.join("steam_settings").join("steam_appid.txt")).unwrap(),
            "471710"
        );
        assert!(p.join("steam_settings").join("steam_interfaces.txt").exists());
        assert!(!d.join("steam_appid.txt").exists()); // legacy removed
        assert_eq!(verify(&d), BypassState::Ok(BypassMethod::Stub));
    }

    #[test]
    fn install_stub_fails_without_dll() {
        let d = tmp("no-dll");
        fake_plug(&d);
        assert!(install_stub(&d).is_err());
    }
}
