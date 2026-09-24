//! Windows resource embedding for FluxRec-Setup.exe.
//!
//! On Windows CI this embeds `fluxrec.ico` as the executable's icon plus
//! version metadata (product name, file description, version), so the setup
//! file shows the blue Flux Rec icon in Explorer and its Properties dialog
//! instead of the default Rust placeholder.
//!
//! On any non-Windows host/target this is a silent no-op, so `cargo check`
//! / `cargo test` keep passing on Linux. A resource-compiler failure never
//! fails the build: the installer must compile even if the icon cannot be
//! embedded.

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let pkg_version =
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.10".to_string());
    let mut nums = [0u16; 4];
    for (i, part) in pkg_version.split('.').take(4).enumerate() {
        nums[i] = part.parse::<u16>().unwrap_or(0);
    }
    let ver: u64 = ((nums[0] as u64) << 48)
        | ((nums[1] as u64) << 32)
        | ((nums[2] as u64) << 16)
        | (nums[3] as u64);

    let mut res = winres::WindowsResource::new();
    res.set_icon("fluxrec.ico");
    res.set("FileDescription", "Flux Rec Setup");
    res.set("ProductName", "Flux Rec");
    res.set("CompanyName", "Ripo Team");
    res.set("LegalCopyright", "Ripo Team");
    res.set_version_info(winres::VersionInfo::FILEVERSION, ver);
    res.set_version_info(winres::VersionInfo::PRODUCTVERSION, ver);

    if let Err(e) = res.compile() {
        // Icon embedding is cosmetic: never fail the build over it.
        eprintln!("warning: winres failed ({e}); building without embedded icon");
    }
}
