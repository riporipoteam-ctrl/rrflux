// Flux Rec persistent backend (`Flux Rec.exe --backend`, installed as
// `FluxRec-backend.exe`).
//
// This is the same binary as the launcher, but in backend mode it never
// launches the game: it owns the silent Firebase session, the local HTTPS
// translator on 127.0.0.1:443 (+ HTTP on :80), and the /health diagnostics
// endpoint, and it stays alive after the game exits. A scheduled task
// ("FluxRecBackend", logon trigger) starts it; the launcher verifies its
// version over /health and replaces it when outdated.

use crate::translator::{self, BackendState, SharedSession};
use crate::util;
use std::sync::Arc;

pub async fn run() {
    let dir = util::data_dir();
    let _ = std::fs::create_dir_all(&dir);

    // Request log + diagnostics state live here for the backend's lifetime.
    translator::set_log_dir(dir.clone());

    // 1. Silent sign-in (anonymous Firebase account, cached locally).
    let session: SharedSession = Default::default();
    match crate::auth::ensure_session(&dir).await {
        Ok(s) => *session.lock().await = Some(s),
        Err(e) => {
            // Headless: no popup, just log and exit non-zero so the
            // launcher notices the backend is missing and reports why.
            util::crash_log(&format!("backend: sign-in failed: {e}"));
            std::process::exit(1);
        }
    }
    let s2 = session.clone();
    let d2 = dir.clone();
    tokio::spawn(async move { crate::auth::refresh_loop(s2, d2).await; });

    // 2. Serve the translator forever.
    let exe_path = std::env::current_exe().unwrap_or_else(|_| dir.join("FluxRec-backend.exe"));
    let state = Arc::new(BackendState::new(exe_path));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let serve_task = tokio::spawn(translator::serve(session, dir.clone(), state.clone(), tx));
    match rx.await {
        Ok(Ok(())) => {
            util::crash_log(&format!(
                "backend v{} listening: https=127.0.0.1:443 http=127.0.0.1:80 (pid {})",
                state.version,
                std::process::id()
            ));
        }
        Ok(Err(e)) => {
            util::crash_log(&format!("backend: translator failed to start: {e}"));
            std::process::exit(1);
        }
        Err(_) => {
            let panic_info = std::fs::read_to_string(dir.join("panic.log"))
                .ok()
                .and_then(|c| c.lines().last().map(|l| l.to_string()))
                .unwrap_or_else(|| "no details captured".into());
            util::crash_log(&format!("backend: translator died during startup: {panic_info}"));
            std::process::exit(1);
        }
    }

    // 3. Stay alive. serve() only returns if every listener died — then the
    //    backend is useless, so exit and let the launcher restart us.
    match serve_task.await {
        Ok(()) => {
            util::crash_log("backend: translator exited unexpectedly");
            std::process::exit(1);
        }
        Err(e) => {
            util::crash_log(&format!("backend: translator task failed: {e}"));
            std::process::exit(1);
        }
    }
}
