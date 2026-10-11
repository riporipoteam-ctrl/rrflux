//! Branding assets for the Flux Rec installer GUI window.
//!
//! The approved logo is generated as a single-colour blue vector-like raster at runtime.
//! It deliberately does not depend on a separately generated/multicolour PNG, so the setup,
//! launcher, splash screen and Windows shortcuts all share the same exact blue mark.

use image::{DynamicImage, ImageFormat, Rgba, RgbaImage};
use std::io::{self, Cursor};

/// Generate the approved Flux Rec mark at any requested resolution.
/// Transparent pixels stay transparent; every visible pixel uses the brand blue.
pub fn fluxrec_logo_rgba(width: u32, height: u32) -> RgbaImage {
    let width = width.max(1);
    let height = height.max(1);
    let mut image = RgbaImage::new(width, height);
    let scale = width.min(height) as f32;
    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;
    let stroke = (scale * 0.052).max(1.25);
    let half_stroke = stroke / 2.0;
    let radius = scale * 0.075;
    let half_w = scale * 0.40;
    let half_h = scale * 0.43;

    // Divider and the two inner posts.
    let y_divider = cy - scale * 0.16;
    let left = cx - scale * 0.39;
    let right = cx + scale * 0.39;
    let post_y = cy + scale * 0.20;
    let left_post = cx - scale * 0.17;
    let right_post = cx + scale * 0.17;

    // Curved smile, sampled into short straight segments for smooth antialiasing.
    let smile_start = (left, cy + scale * 0.17);
    let smile_end = (right, cy + scale * 0.17);
    let smile_control = (cx, cy + scale * 0.45);
    let mut smile_segments = Vec::with_capacity(32);
    let mut previous = smile_start;
    for i in 1..=32 {
        let t = i as f32 / 32.0;
        let u = 1.0 - t;
        let point = (
            u * u * smile_start.0 + 2.0 * u * t * smile_control.0 + t * t * smile_end.0,
            u * u * smile_start.1 + 2.0 * u * t * smile_control.1 + t * t * smile_end.1,
        );
        smile_segments.push((previous, point));
        previous = point;
    }

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

    for y in 0..height {
        for x in 0..width {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            // Signed distance to the rounded outer frame.
            let qx = (px - cx).abs() - (half_w - radius);
            let qy = (py - cy).abs() - (half_h - radius);
            let outside = qx.max(0.0).hypot(qy.max(0.0));
            let sdf = outside + qx.max(qy).min(0.0) - radius;
            let mut distance = sdf.abs();

            // Horizontal divider.
            distance = distance.min(line_distance(px, py, (left, y_divider), (right, y_divider)));
            // Two vertical posts.
            distance = distance.min(line_distance(px, py, (left_post, y_divider), (left_post, post_y)));
            distance = distance.min(line_distance(px, py, (right_post, y_divider), (right_post, post_y)));
            // Smile curve.
            for (a, b) in &smile_segments {
                distance = distance.min(line_distance(px, py, *a, *b));
            }

            // Coverage-based antialiasing gives clean blue edges at every DPI.
            let coverage = ((half_stroke + 0.75 - distance) / 1.5).clamp(0.0, 1.0);
            if coverage > 0.0 {
                image.put_pixel(x, y, Rgba([46, 145, 255, (coverage * 255.0).round() as u8]));
            }
        }
    }
    image
}

fn encode_png(image: &RgbaImage) -> io::Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image.clone())
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(io::Error::other)?;
    Ok(cursor.into_inner())
}

/// Build a modern multi-size Windows icon whose entries all use the approved blue mark.
pub fn fluxrec_icon_ico_bytes() -> io::Result<Vec<u8>> {
    let sizes: [u32; 6] = [16, 32, 48, 64, 128, 256];
    let mut pngs = Vec::with_capacity(sizes.len());
    for size in sizes {
        pngs.push(encode_png(&fluxrec_logo_rgba(size, size))?);
    }

    // ICONDIR: reserved, type=icon, count.
    let mut output = Vec::with_capacity(6 + sizes.len() * 16 + pngs.iter().map(Vec::len).sum::<usize>());
    output.extend_from_slice(&0u16.to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&(sizes.len() as u16).to_le_bytes());

    let mut offset = 6 + sizes.len() * 16;
    for (size, png) in sizes.iter().zip(&pngs) {
        output.push(if *size == 256 { 0 } else { *size as u8 });
        output.push(if *size == 256 { 0 } else { *size as u8 });
        output.push(0); // palette
        output.push(0); // reserved
        output.extend_from_slice(&1u16.to_le_bytes()); // planes
        output.extend_from_slice(&32u16.to_le_bytes()); // bit count
        output.extend_from_slice(&(png.len() as u32).to_le_bytes());
        output.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for png in pngs {
        output.extend_from_slice(&png);
    }
    Ok(output)
}

/// Write the approved blue Flux Rec icon to a path used by a Windows shortcut.
/// The file is freshly generated, so an old multicolour icon cannot be copied back in.
pub fn write_icon_ico(dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(dest, fluxrec_icon_ico_bytes()?)
}
