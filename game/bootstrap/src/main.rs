// Flux Rec — headless game bootstrapper (this IS the "game" as far as the
// player is concerned; there is no launcher window and no login screen).
//
// One binary, two modes:
//   - default: the launcher. Self-updates, refreshes game files, deploys
//     the Steam emulator, makes sure the persistent backend is healthy,
//     then starts RecRoom.exe and waits for it.
//   - `--backend`: the persistent backend. Owns the silent Firebase
//     session and the local HTTPS translator on 127.0.0.1:443 (+ HTTP on
//     :80); serves /health diagnostics; stays alive after the game exits.
//     Started by the "FluxRecBackend" scheduled task (logon trigger).
//   - `--setup-backend`: installer helper. Deploys the backend copy,
//     creates the scheduled task, starts it now, exits with a status code.

#![windows_subsystem = "windows"]

mod auth;
mod backend;
mod diag;
mod launcher;
mod translator;
mod util;

fn main() {
    // Log panics to %LOCALAPPDATA%\FluxRec\panic.log so a crashing
    // background task leaves a trace we can show.
    let pd = util::data_dir();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write as _;
        let _ = std::fs::create_dir_all(&pd);
        let msg = format!("PANIC: {}\n", info);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(pd.join("panic.log"))
            .and_then(|mut f| f.write_all(msg.as_bytes()));
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!("Couldn't start Flux Rec: {e}");
            std::process::exit(1);
        });
    if args.iter().any(|a| a == "--backend") {
        rt.block_on(backend::run());
    } else if args.iter().any(|a| a == "--setup-backend") {
        let code = rt.block_on(launcher::setup_backend());
        std::process::exit(code);
    } else {
        rt.block_on(launcher::run());
    }
}
