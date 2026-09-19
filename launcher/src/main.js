// Flux Rec launcher frontend. Talks to the Rust backend via Tauri IPC.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// Where the launcher downloads the game from. Set it once in Settings —
// saved locally, so changing hosts never needs a launcher rebuild.
const DEFAULT_MANIFEST_URL = "https://example.invalid/fluxrec/manifest.json";
const manifestUrl = () =>
  (localStorage.getItem("fluxrec_manifest_url") || "").trim() || DEFAULT_MANIFEST_URL;

const loginCard = document.getElementById("login-card");
const mainCard = document.getElementById("main-card");
const loginBtn = document.getElementById("login-btn");
const loginErr = document.getElementById("login-err");
const emailEl = document.getElementById("email");
const passwordEl = document.getElementById("password");
const whoEl = document.getElementById("who");
const statusEl = document.getElementById("status");
const barEl = document.getElementById("bar");
const pctEl = document.getElementById("pct");
const actionBtn = document.getElementById("action");
const manifestUrlEl = document.getElementById("manifest-url");

// Persist the manifest URL setting.
manifestUrlEl.value = localStorage.getItem("fluxrec_manifest_url") || "";
manifestUrlEl.addEventListener("change", () => {
  const v = manifestUrlEl.value.trim();
  if (v) localStorage.setItem("fluxrec_manifest_url", v);
  else localStorage.removeItem("fluxrec_manifest_url");
});

const fmtMB = (n) => (n / 1048576).toFixed(1) + " MB";

function setStatus(t) { statusEl.textContent = t; }
function setBar(frac) {
  barEl.style.width = (frac * 100).toFixed(1) + "%";
  pctEl.textContent = (frac * 100).toFixed(0) + "%";
}

async function doLogin() {
  loginErr.textContent = "";
  loginBtn.disabled = true;
  loginBtn.textContent = "SIGNING IN…";
  try {
    const s = await invoke("sign_in", {
      email: emailEl.value.trim(),
      password: passwordEl.value,
    });
    passwordEl.value = "";
    whoEl.textContent = s.username;
    loginCard.classList.add("hidden");
    mainCard.classList.remove("hidden");
    await refresh();
  } catch (e) {
    loginErr.textContent = String(e);
    loginBtn.disabled = false;
    loginBtn.textContent = "SIGN IN";
  }
}

loginBtn.onclick = doLogin;
passwordEl.addEventListener("keydown", (e) => { if (e.key === "Enter") doLogin(); });
emailEl.addEventListener("keydown", (e) => { if (e.key === "Enter") doLogin(); });

async function refresh() {
  const installed = await invoke("game_installed");
  const translatorOk = await invoke("translator_status");
  if (!translatorOk) {
    setStatus("Local translator failed to start (port 80 busy?). Restart the launcher.");
    actionBtn.disabled = true;
    actionBtn.textContent = "⚠ TRANSLATOR DOWN";
    return;
  }
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
    const manifestJson = await invoke("fetch_manifest", { manifestUrl: manifestUrl() });
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
    // The local translator (127.0.0.1:80) answers the game's API calls
    // from the live Firebase session — no extra args needed.
    await invoke("launch_game", { extraArgs: [] });
    setStatus("Game is running. Have fun! 🎮");
  } catch (e) {
    setStatus("Launch failed: " + e);
  }
}
