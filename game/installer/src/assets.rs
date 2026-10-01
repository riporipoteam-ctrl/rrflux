//! Branding assets for the Flux Rec installer GUI window.
//!
//! The logo is embedded at compile time as a 32-bit Windows BMP so the GUI
//! needs no runtime file lookup and no image-decoding dependency: hand the
//! bytes straight to `CreateDIBSection`/`StretchDIBits` (or an Image control).

/// The Flux Rec smile logo as a 256x256 32-bit (BGRA, BI_RGB) BMP byte slice.
///
/// Layout: 14-byte BITMAPFILEHEADER + 40-byte BITMAPINFOHEADER, then
/// bottom-up rows of BGRA pixels. The 4th byte of every pixel is a real
/// alpha channel (transparent background outside the smile mark).
pub static LOGO_BMP_BYTES: &[u8] = include_bytes!("../assets/logo.bmp");

/// The Flux Rec smile logo as a multi-size Windows `.ico` (16, 32, 48, 64,
/// 128, and 256 px; 32-bit PNG-compressed entries with alpha preserved),
/// generated from [`LOGO_BMP_BYTES`].
///
/// Written to disk by [`write_icon_ico`] so the shell can point a shortcut
/// at it via `IShellLinkW::SetIconLocation` — a `.ico` *file path* is
/// required there, an embedded byte slice is not enough.
pub static ICON_ICO_BYTES: &[u8] = include_bytes!("../assets/fluxrec.ico");

/// Carousel slide images (800x450 24-bit BMP) for the installer slideshow.
/// Slide 1 is the welcome slide (drawn programmatically with the logo);
/// slides 2-5 are real Rec Room game screenshots with tutorial text.
pub static SLIDE2_BMP_BYTES: &[u8] = include_bytes!("../assets/slide2.bmp");
pub static SLIDE3_BMP_BYTES: &[u8] = include_bytes!("../assets/slide3.bmp");
pub static SLIDE4_BMP_BYTES: &[u8] = include_bytes!("../assets/slide4.bmp");
pub static SLIDE5_BMP_BYTES: &[u8] = include_bytes!("../assets/slide5.bmp");

/// Write [`ICON_ICO_BYTES`] to `dest` (e.g. `<game dir>\fluxrec.ico`).
///
/// Overwrites any existing file. The caller should call this once per
/// install before assigning the path to a shortcut's icon location.
pub fn write_icon_ico(dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(dest, ICON_ICO_BYTES)
}
