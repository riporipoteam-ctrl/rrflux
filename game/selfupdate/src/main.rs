// fluxrec-selfupdate — swaps in a new bootstrapper after the old one exits.
//
// Windows won't let a running exe replace itself, so the old bootstrapper
// downloads the new one, spawns this helper, and exits immediately. This
// helper waits until the target file is replaceable (the old process has
// exited and released its lock), moves the new exe into place, and
// optionally relaunches it.
//
// Usage: fluxrec-selfupdate --from <new exe> --to <target exe> [--launch]

use std::path::PathBuf;
use std::time::Duration;

fn main() {
    let mut from: Option<PathBuf> = None;
    let mut to: Option<PathBuf> = None;
    let mut launch = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--from" => from = args.next().map(PathBuf::from),
            "--to" => to = args.next().map(PathBuf::from),
            "--launch" => launch = true,
            _ => {}
        }
    }
    let (from, to) = match (from, to) {
        (Some(f), Some(t)) => (f, t),
        _ => {
            eprintln!("usage: fluxrec-selfupdate --from <new> --to <target> [--launch]");
            std::process::exit(2);
        }
    };

    // Wait for the old exe to exit (its file lock is what blocks us).
    // Poll by attempting the replace; ~60s of patience, then give up.
    let mut replaced = false;
    for _ in 0..120 {
        // Remove the target first: rename won't overwrite on Windows.
        let _ = std::fs::remove_file(&to);
        match std::fs::rename(&from, &to) {
            Ok(()) => {
                replaced = true;
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }
    if !replaced {
        eprintln!("self-update failed: couldn't replace {}", to.display());
        std::process::exit(1);
    }

    if launch {
        if let Err(e) = std::process::Command::new(&to).spawn() {
            eprintln!("self-update relaunch failed: {e}");
            std::process::exit(1);
        }
    }
}
