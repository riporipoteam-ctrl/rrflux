#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Diagnostics {
    apple_mobile_device_service: bool,
    device_helper_path: Option<String>,
    official_ipa_configured: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Device {
    udid: String,
    transport: String,
}

fn candidate_tool_paths(name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            paths.push(parent.join("tools").join(name));
            paths.push(parent.join("resources").join("tools").join(name));
            paths.push(parent.join(name));
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        paths.extend(std::env::split_paths(&path).map(|dir| dir.join(name)));
    }
    paths
}

fn find_tool(name: &str) -> Option<PathBuf> {
    candidate_tool_paths(name)
        .into_iter()
        .find(|path| path.is_file())
}

fn apple_service_running() -> bool {
    Command::new("sc.exe")
        .args(["query", "Apple Mobile Device Service"])
        .output()
        .map(|output| output.status.success() && String::from_utf8_lossy(&output.stdout).contains("RUNNING"))
        .unwrap_or(false)
}

#[tauri::command]
fn diagnostics() -> Diagnostics {
    Diagnostics {
        apple_mobile_device_service: apple_service_running(),
        device_helper_path: find_tool("ripo-ios-device-helper.exe").map(|p| p.display().to_string()),
        official_ipa_configured: option_env!("RIPO_LAUNCHER_IPA_URL").is_some()
            && option_env!("RIPO_LAUNCHER_IPA_SHA256_URL").is_some(),
    }
}

fn run_device_list(tool: &Path) -> Result<Vec<Device>, String> {
    let output = Command::new(tool)
        .arg("devices")
        .output()
        .map_err(|e| format!("could not run {}: {e}", tool.display()))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("device helper returned invalid data: {e}"))
}

#[tauri::command]
async fn list_devices() -> Result<Vec<Device>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let tool = find_tool("ripo-ios-device-helper.exe")
            .ok_or("The bundled iPhone device helper is missing. Reinstall Ripo Team iPhone Installer.")?;
        let mut devices = BTreeMap::<String, String>::new();
        for device in run_device_list(&tool)? {
            let rank = |transport: &str| if transport == "USB" { 2 } else { 1 };
            match devices.get(&device.udid) {
                Some(current) if rank(current) >= rank(&device.transport) => {}
                _ => { devices.insert(device.udid, device.transport); }
            }
        }
        Ok(devices
            .into_iter()
            .map(|(udid, transport)| Device { udid, transport })
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn choose_ipa() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("iPhone app", &["ipa"])
        .pick_file()
        .map(|path| path.display().to_string())
}

fn validate_ipa(path: &Path) -> Result<(), String> {
    if path.extension().and_then(|value| value.to_str()).map(|value| value.eq_ignore_ascii_case("ipa")) != Some(true) {
        return Err("Select an .ipa file.".to_string());
    }
    let file = File::open(path).map_err(|e| format!("could not open IPA: {e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("invalid IPA archive: {e}"))?;
    let mut has_app = false;
    let mut has_profile = false;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|e| format!("invalid IPA entry: {e}"))?;
        let name = entry.name();
        has_app |= name.starts_with("Payload/") && name.contains(".app/");
        has_profile |= name.starts_with("Payload/") && name.ends_with(".app/embedded.mobileprovision");
    }
    if !has_app {
        return Err("The IPA does not contain a Payload/*.app bundle.".to_string());
    }
    if !has_profile {
        return Err("The IPA has no embedded provisioning profile. Build and sign it for this device first.".to_string());
    }
    Ok(())
}

#[tauri::command]
async fn install_ipa(ipa_path: String, udid: String, wireless: bool) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let ipa = PathBuf::from(ipa_path);
        validate_ipa(&ipa)?;
        if udid.trim().is_empty() {
            return Err("Select an iPhone first.".to_string());
        }
        let tool = find_tool("ripo-ios-device-helper.exe")
            .ok_or("The bundled iPhone device helper is missing. Reinstall Ripo Team iPhone Installer.")?;
        let mut command = Command::new(tool);
        command
            .arg("install")
            .args(["--udid", udid.trim()])
            .args(["--transport", if wireless { "Wi-Fi" } else { "USB" }])
            .arg(&ipa);
        let output = command.output().map_err(|e| format!("installer failed to start: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !output.status.success() {
            return Err(if stderr.is_empty() { stdout } else { stderr });
        }
        Ok(if stdout.is_empty() { "Installation completed.".to_string() } else { stdout })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn download_official_ipa() -> Result<String, String> {
    let ipa_url = option_env!("RIPO_LAUNCHER_IPA_URL")
        .ok_or("This build does not have an official launcher IPA URL configured.")?;
    let checksum_url = option_env!("RIPO_LAUNCHER_IPA_SHA256_URL")
        .ok_or("This build does not have an official launcher checksum URL configured.")?;
    let client = reqwest::Client::new();
    let bytes = client
        .get(ipa_url)
        .send()
        .await
        .map_err(|e| format!("IPA download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("IPA download failed: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("IPA download failed: {e}"))?;
    let expected = client
        .get(checksum_url)
        .send()
        .await
        .map_err(|e| format!("checksum download failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("checksum download failed: {e}"))?
        .text()
        .await
        .map_err(|e| format!("checksum download failed: {e}"))?
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let actual = hex::encode(Sha256::digest(&bytes));
    if expected.len() != 64 || expected != actual {
        return Err("The downloaded IPA failed SHA-256 verification.".to_string());
    }
    let destination = std::env::temp_dir().join("RipoTeamLauncher.ipa");
    std::fs::write(&destination, bytes).map_err(|e| format!("could not save IPA: {e}"))?;
    validate_ipa(&destination)?;
    Ok(destination.display().to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            diagnostics,
            list_devices,
            choose_ipa,
            install_ipa,
            download_official_ipa,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Ripo Team iPhone Installer");
}
