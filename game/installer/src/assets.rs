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
