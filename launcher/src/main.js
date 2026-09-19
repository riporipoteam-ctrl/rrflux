// RRFlux launcher frontend. Talks to the Rust backend via Tauri IPC.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const MANIFEST_URL = "https://example.invalid/rrflux/manifest.json"; // TODO: real host

const statusEl = document.getElementById("status");
const barEl = document.getElementById("bar");
const pctEl = document.getElementById("pct");
const actionBtn = document.getElementById("action");

const fmtMB = (n) => (n / 1048576).toFixed(1) + " MB";

function setStatus(t) { statusEl.textContent = t; }
function setBar(frac) {
  barEl.style.width = (frac * 100).toFixed(1) + "%";
  pctEl.textContent = (frac * 100).toFixed(0) + "%";
}

async function refresh() {
  const installed = await invoke("game_installed");
  if (installed) {
    setStatus("Ready to play.");
    setBar(1);
    actionBtn.disabled = false;
    actionBtn.textContent = "▶  PLAY";
    actionBtn.onclick = play;
  } else {
    setStatus("Game not installed.");
    setBar(0);
    actionBtn.disabled = false;
    actionBtn.textContent = "⬇  INSTALL";
    actionBtn.onclick = install;
  }
}

async function install() {
  actionBtn.disabled = true;
  try {
    setStatus("Fetching manifest…");
    const manifestJson = await invoke("fetch_manifest", { manifestUrl: MANIFEST_URL });
    const manifest = JSON.parse(manifestJson);
    setStatus(`Downloading ${manifest.files.length} files…`);
    await listen("progress", (e) => {
      const p = e.payload;
      const frac = p.files_total ? p.files_done / p.files_total : 0;
      setBar(frac);
      setStatus(p.file ? `${p.file} — ${fmtMB(p.downloaded)} / ${fmtMB(p.total)}` : "Verifying…");
    });
    await invoke("download_game", { manifestJson });
    setStatus("Installed. Verifying…");
    await refresh();
  } catch (e) {
    setStatus("Error: " + e);
    actionBtn.disabled = false;
    actionBtn.textContent = "↻  RETRY";
  }
}

async function play() {
  setStatus("Launching…");
  try {
    await invoke("launch_game", { extraArgs: [] });
    setStatus("Game is running. Have fun! 🎮");
  } catch (e) {
    setStatus("Launch failed: " + e);
  }
}

refresh();
