// Shared building blocks for the Flux Rec bootstrapper, installer
// downloader, and self-updater: manifest handling, parallel downloads
// with resume, sleep prevention, and a small native progress window.

pub mod download;
pub mod manifest;
pub mod progress;
pub mod sleep;
pub mod update;

/// Where the game files manifest lives. Build-time override for tests.
pub const MANIFEST_URL: &str = match option_env!("FLUXREC_MANIFEST_URL") {
    Some(u) => u,
    None => "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/manifest.json",
};
