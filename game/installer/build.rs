//! Embed the approved single-colour Flux Rec logo in the Windows executable.
//!
//! The icon is generated here rather than copied from an old asset file, preventing the
//! obsolete multicolour logo from returning in the setup executable or desktop shortcut.

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use std::io::Cursor;
use std::path::PathBuf;

fn logo_rgba(size: u32) -> RgbaImage {
    let size = size.max(1);
    let mut image = RgbaImage::new(size, size);
    let s = size as f32;
    let c = s / 2.0;
    let stroke = (s * 0.052).max(1.25);
    let half_stroke = stroke / 2.0;
    let radius = s * 0.075;
    let half_w = s * 0.40;
    let half_h = s * 0.43;
    let y_divider = c - s * 0.16;
    let left = c - s * 0.39;
    let right = c + s * 0.39;
    let post_y = c + s * 0.20;
    let left_post = c - s * 0.17;
    let right_post = c + s * 0.17;

    let line_distance = |x: f32, y: f32, a: (f32, f32), b: (f32, f32)| -> f32 {
        let dx = b.0 - a.0;
        let dy = b.1 - a.1;
        let len2 = dx * dx + dy * dy;
        let t = if len2 <= f32::EPSILON {
            0.0
        } else {
            (((x - a.0) * dx + (y - a.1) * dy) / len2).clamp(0.0, 1.0)
        };
        ((x - (a.0 + t * dx)).powi(2) + (y - (a.1 + t * dy)).powi(2)).sqrt()
    };
    let mut smile = Vec::with_capacity(32);
    let start = (left, c + s * 0.17);
    let end = (right, c + s * 0.17);
    let control = (c, c + s * 0.45);
    let mut previous = start;
    for i in 1..=32 {
        let t = i as f32 / 32.0;
        let u = 1.0 - t;
        let p = (
            u * u * start.0 + 2.0 * u * t * control.0 + t * t * end.0,
            u * u * start.1 + 2.0 * u * t * control.1 + t * t * end.1,
        );
        smile.push((previous, p));
        previous = p;
    }

    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let qx = (px - c).abs() - (half_w - radius);
            let qy = (py - c).abs() - (half_h - radius);
            let outside = qx.max(0.0).hypot(qy.max(0.0));
            let sdf = outside + qx.max(qy).min(0.0) - radius;
            let mut distance = sdf.abs();
            distance = distance.min(line_distance(px, py, (left, y_divider), (right, y_divider)));
            distance = distance.min(line_distance(px, py, (left_post, y_divider), (left_post, post_y)));
            distance = distance.min(line_distance(px, py, (right_post, y_divider), (right_post, post_y)));
            for (a, b) in &smile {
                distance = distance.min(line_distance(px, py, *a, *b));
            }
            let coverage = ((half_stroke + 0.75 - distance) / 1.5).clamp(0.0, 1.0);
            if coverage > 0.0 {
                image.put_pixel(x, y, Rgba([46, 145, 255, (coverage * 255.0).round() as u8]));
            }
        }
    }
    image
}

fn png_bytes(image: RgbaImage) -> Result<Vec<u8>, image::ImageError> {
    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image).write_to(&mut cursor, ImageFormat::Png)?;
    Ok(cursor.into_inner())
}

fn icon_bytes() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let sizes: [u32; 6] = [16, 32, 48, 64, 128, 256];
    let pngs = sizes
        .iter()
        .map(|size| png_bytes(logo_rgba(*size)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut output = Vec::new();
    output.extend_from_slice(&0u16.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&(sizes.len() as u16).to_le_bytes());

    let mut offset = 6 + sizes.len() * 16;
    for (size, png) in sizes.iter().zip(&pngs) {
        output.push(if *size == 256 { 0 } else { *size as u8 });
        output.push(if *size == 256 { 0 } else { *size as u8 });
        output.push(0);
        output.push(0);
        output.extend_from_slice(&1u16.to_le_bytes());
        output.extend_from_slice(&32u16.to_le_bytes());
        output.extend_from_slice(&(png.len() as u32).to_le_bytes());
        output.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for png in pngs {
        output.extend_from_slice(&png);
    }
    Ok(output)
}

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let icon_path = out_dir.join("fluxrec-generated.ico");
    match icon_bytes().and_then(|bytes| std::fs::write(&icon_path, bytes).map_err(Into::into)) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("error: could not generate Flux Rec icon: {e}");
            std::process::exit(1);
        }
    }

    let pkg_version =
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.20".to_string());
    let mut nums = [0u16; 4];
    for (i, part) in pkg_version.split('.').take(4).enumerate() {
        nums[i] = part.parse::<u16>().unwrap_or(0);
    }
    let ver: u64 = ((nums[0] as u64) << 48)
        | ((nums[1] as u64) << 32)
        | ((nums[2] as u64) << 16)
        | (nums[3] as u64);

    let mut res = winres::WindowsResource::new();
    let icon = icon_path.to_string_lossy().into_owned();
    res.set_icon(&icon);
    res.set("FileDescription", "Flux Rec Setup");
    res.set("ProductName", "Flux Rec");
    res.set("CompanyName", "Ripo Team");
    res.set("LegalCopyright", "Ripo Team");
    res.set_version_info(winres::VersionInfo::FILEVERSION, ver);
    res.set_version_info(winres::VersionInfo::PRODUCTVERSION, ver);

    if let Err(e) = res.compile() {
        eprintln!("warning: winres failed ({e}); building without embedded icon");
    }
}
