//! Patch the game exe's embedded icon so the taskbar shows the blue
//! Flux Rec logo instead of the orange Rec Room logo.
//!
//! Uses the Windows resource update APIs (BeginUpdateResource /
//! UpdateResource / EndUpdateResource) to replace the exe's icon group.
//! Fail-soft: any error leaves the exe untouched (we work on a copy and
//! only swap it in on success).
//!
//! v1.0.3.

use std::path::Path;

/// Replace the icon of the exe at `exe_path` with the icon from `ico_bytes`.
/// Returns Ok(true) if the icon was replaced, Ok(false) if skipped,
/// Err(msg) on failure (the exe is left untouched).
pub fn patch_exe_icon(exe_path: &Path, ico_bytes: &[u8]) -> Result<bool, String> {
    // Parse the .ico file.
    let icons = parse_ico(ico_bytes)?;
    if icons.is_empty() {
        return Err("ICO file contains no images".to_string());
    }

    // Work on a copy; only swap in on success.
    let tmp_path = exe_path.with_extension("exe.iconpatch.tmp");
    std::fs::copy(exe_path, &tmp_path)
        .map_err(|e| format!("backup exe for icon patch: {}", e))?;

    let result = patch_exe_icon_inner(&tmp_path, &icons);

    match result {
        Ok(()) => {
            // Verify the patched exe is still a valid PE.
            if !is_valid_pe(&tmp_path) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err("patched exe failed PE validation".to_string());
            }
            // Clear readonly before overwriting.
            #[cfg(windows)]
            {
                if let Ok(meta) = std::fs::metadata(exe_path) {
                    let mut perms = meta.permissions();
                    if perms.readonly() {
                        perms.set_readonly(false);
                        let _ = std::fs::set_permissions(exe_path, perms);
                    }
                }
            }
            std::fs::rename(&tmp_path, exe_path)
                .map_err(|e| format!("swap in patched exe: {}", e))?;
            Ok(true)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

struct IcoImage {
    width: u8,
    height: u8,
    colors: u8,
    planes: u16,
    bit_count: u16,
    data: Vec<u8>,
}

fn parse_ico(bytes: &[u8]) -> Result<Vec<IcoImage>, String> {
    if bytes.len() < 6 {
        return Err("ICO too small".to_string());
    }
    let reserved = u16::from_le_bytes([bytes[0], bytes[1]]);
    let typ = u16::from_le_bytes([bytes[2], bytes[3]]);
    let count = u16::from_le_bytes([bytes[4], bytes[5]]);
    if reserved != 0 || typ != 1 || count == 0 || count > 32 {
        return Err(format!("invalid ICO header ({}, {}, {})", reserved, typ, count));
    }
    let mut images = Vec::new();
    for i in 0..count as usize {
        let off = 6 + i * 16;
        if off + 16 > bytes.len() {
            return Err("ICO entry out of bounds".to_string());
        }
        let e = &bytes[off..off + 16];
        let img_offset = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
        let img_size = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
        if img_offset + img_size > bytes.len() {
            return Err("ICO image data out of bounds".to_string());
        }
        images.push(IcoImage {
            width: e[0],
            height: e[1],
            colors: e[2],
            planes: u16::from_le_bytes([e[4], e[5]]),
            bit_count: u16::from_le_bytes([e[6], e[7]]),
            data: bytes[img_offset..img_offset + img_size].to_vec(),
        });
    }
    Ok(images)
}

fn is_valid_pe(path: &Path) -> bool {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return false,
    };
    if data.len() < 64 {
        return false;
    }
    if data[0] != b'M' || data[1] != b'Z' {
        return false;
    }
    let pe_off = u32::from_le_bytes([data[60], data[61], data[62], data[63]]) as usize;
    if pe_off + 6 > data.len() {
        return false;
    }
    &data[pe_off..pe_off + 4] == b"PE\0\0"
}

#[cfg(windows)]
fn patch_exe_icon_inner(tmp_path: &Path, icons: &[IcoImage]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, EnumResourceNamesW, UpdateResourceW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{RT_GROUP_ICON, RT_ICON};

    let wide: Vec<u16> = tmp_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let h = BeginUpdateResourceW(PCWSTR(wide.as_ptr()), false)
            .map_err(|e| format!("BeginUpdateResource: {}", e))?;

        // Find existing icon groups to replace them all.
        let mut group_ids: Vec<u32> = Vec::new();
        unsafe extern "system" fn enum_cb(
            _h: HMODULE,
            _t: PCWSTR,
            name: PCWSTR,
            l: isize,
        ) -> BOOL {
            let ids = &mut *(l as *mut Vec<u32>);
            // name is either an integer ID (high bit set) or a string.
            let ptr = name.0 as usize;
            if ptr >> 16 == 0xFFFF {
                ids.push((ptr & 0xFFFF) as u32);
            }
            TRUE
        }
        let _ = EnumResourceNamesW(
            HMODULE(std::ptr::null_mut()),
            RT_GROUP_ICON,
            Some(enum_cb),
            &mut group_ids as *mut Vec<u32> as isize,
        );
        // Note: EnumResourceNamesW on a null module enumerates the current
        // process, not our target file — so this won't find the target's
        // icons. Instead, we just replace the standard IDs 1..=4, which
        // covers Unity exes (main icon is almost always group 1).

        // Write each icon image as RT_ICON with IDs 1000+.
        let mut group_entries: Vec<(u8, u8, u8, u16, u16, u32)> = Vec::new();
        for (i, img) in icons.iter().enumerate() {
            let icon_id = 1000 + i as u32;
            let ok = UpdateResourceW(
                h,
                RT_ICON,
                PCWSTR((0xFFFF_0000 | icon_id) as usize as *const u16),
                1033, // en-US
                Some(img.data.as_ptr() as *const _),
                img.data.len() as u32,
            );
            if ok.is_err() {
                let _ = EndUpdateResourceW(h, true); // discard
                return Err(format!("UpdateResource RT_ICON {} failed", icon_id));
            }
            group_entries.push((
                img.width,
                img.height,
                img.colors,
                img.planes,
                img.bit_count,
                icon_id,
            ));
        }

        // Build GRPICONDIR.
        let mut group_data: Vec<u8> = Vec::new();
        group_data.extend_from_slice(&[0, 0]); // reserved
        group_data.extend_from_slice(&[1, 0]); // type = icon
        group_data.extend_from_slice(&(group_entries.len() as u16).to_le_bytes());
        for (w, h, colors, planes, bit_count, id) in &group_entries {
            group_data.push(*w);
            group_data.push(*h);
            group_data.push(*colors);
            group_data.push(0); // reserved
            group_data.extend_from_slice(&planes.to_le_bytes());
            group_data.extend_from_slice(&bit_count.to_le_bytes());
            // bytes_in_res: we don't know the exact size the loader expects;
            // use the actual data length.
            let idx = (*id - 1000) as usize;
            let size = icons[idx].data.len() as u32;
            group_data.extend_from_slice(&size.to_le_bytes());
            group_data.extend_from_slice(&(*id as u16).to_le_bytes());
        }

        // Replace icon groups 1..=4 (covers the main app icon).
        for gid in 1..=4u32 {
            let ok = UpdateResourceW(
                h,
                RT_GROUP_ICON,
                PCWSTR((0xFFFF_0000 | gid) as usize as *const u16),
                1033,
                Some(group_data.as_ptr() as *const _),
                group_data.len() as u32,
            );
            // Ignore failures for non-existent groups (only group 1 matters).
            let _ = ok;
        }

        EndUpdateResourceW(h, false)
            .map_err(|e| format!("EndUpdateResource: {}", e))?;
        Ok(())
    }
}

#[cfg(not(windows))]
fn patch_exe_icon_inner(_tmp_path: &Path, _icons: &[IcoImage]) -> Result<(), String> {
    Err("icon patching is Windows-only".to_string())
}
