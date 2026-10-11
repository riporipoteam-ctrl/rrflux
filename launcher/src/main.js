// Flux Rec launcher frontend. Talks to the Rust backend via Tauri IPC.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const DEFAULT_MANIFEST_URL = "https://huggingface.co/datasets/Echoxr/rrflux-game/resolve/main/manifest.json";
const manifestUrl = () => (localStorage.getItem("fluxrec_manifest_url") || "").trim() || DEFAULT_MANIFEST_URL;

const loginCard = document.getElementById("login-card");
const mainCard = document.getElementById("main-card");
const loginBtn = document.getElementById("login-btn");
const signupBtn = document.getElementById("signup-btn");
const loginErr = document.getElementById("login-err");
const emailEl = document.getElementById("email");
const passwordEl = document.getElementById("password");
const whoEl = document.getElementById("who");
const statusEl = document.getElementById("status");
const barEl = document.getElementById("bar");
const pctEl = document.getElementById("pct");
const actionBtn = document.getElementById("action");
const manifestUrlEl = document.getElementById("manifest-url");
const versionEl = document.getElementById("installed-version");
const progressLabelEl = document.getElementById("progress-label");
let progressUnlisten = null;

const fmtMB = (n) => (n / 1048576).toFixed(1) + " MB";
function setStatus(t) { statusEl.textContent = t; }
function setBar(frac) { const n = Math.max(0, Math.min(1, Number(frac) || 0)); barEl.style.width = (n * 100).toFixed(1) + "%"; pctEl.textContent = (n * 100).toFixed(0) + "%"; }
function setVersion(t) { if (versionEl) versionEl.textContent = t; }

manifestUrlEl.value = localStorage.getItem("fluxrec_manifest_url") || "";
manifestUrlEl.addEventListener("change", () => {
  const v = manifestUrlEl.value.trim();
  if (v) localStorage.setItem("fluxrec_manifest_url", v); else localStorage.removeItem("fluxrec_manifest_url");
  setStatus("Manifest setting saved. It will be used on the next check.");
});

async function showPlayState(message = "Ready to play.") {
  setStatus(message);
  setBar(1);
  setVersion(localStorage.getItem("fluxrec_game_version") || "INSTALLED");
  actionBtn.disabled = false;
  actionBtn.textContent = "▶  PLAY FLUX REC";
  actionBtn.onclick = play;
}

async function refresh() {
  actionBtn.disabled = true;
  setStatus("Checking installation…");
  try {
    const installed = await invoke("game_installed");
    const translatorOk = await invoke("translator_status");
    if (!translatorOk) {
      setStatus("The local game bridge is unavailable. Restart Flux Rec and try again.");
      setVersion("BRIDGE OFFLINE");
      actionBtn.disabled = true;
      actionBtn.textContent = "BRIDGE OFFLINE";
      return;
    }
    if (!installed) {
      setStatus("Flux Rec isn't installed on this device yet.");
      setBar(0); setVersion("NOT INSTALLED");
      actionBtn.disabled = false; actionBtn.textContent = "⬇  INSTALL FLUX REC"; actionBtn.onclick = install;
      return;
    }

    // Check the live manifest each time the launcher opens. download_game compares SHA-256
    // per file, so clicking Update only fetches changed or missing files.
    setStatus("Checking for game updates…");
    try {
      const manifestJson = await invoke("fetch_manifest", { manifestUrl: manifestUrl() });
      const manifest = JSON.parse(manifestJson);
      const latest = String(manifest.version || "").trim();
      if (!latest || !Array.isArray(manifest.files)) throw new Error("Invalid manifest");
      const installedVersion = localStorage.getItem("fluxrec_game_version") || "";
      setVersion(installedVersion ? "INSTALLED " + installedVersion : "VERSION CHECK");
      if (!installedVersion || installedVersion !== latest) {
        setStatus(installedVersion ? "A game update is available." : "Verifying installed files against the latest manifest…");
        setBar(0); actionBtn.disabled = false;
        actionBtn.textContent = installedVersion ? "↻  UPDATE GAME" : "VERIFY & UPDATE";
        actionBtn.onclick = install; return;
      }
      await showPlayState("You're up to date · " + latest);
    } catch (_) {
      // A temporary outage shouldn't strand someone whose game is already installed.
      await showPlayState("Update check unavailable · you can still play.");
    }
  } catch (e) {
    setStatus("Could not check installation: " + String(e));
    setVersion("CHECK FAILED");
    actionBtn.disabled = false; actionBtn.textContent = "↻  RETRY CHECK"; actionBtn.onclick = refresh;
  }
}

async function doLogin() {
  loginErr.textContent = ""; loginBtn.disabled = true; loginBtn.textContent = "SIGNING IN…";
  try {
    const s = await invoke("sign_in", { email: emailEl.value.trim(), password: passwordEl.value });
    passwordEl.value = ""; whoEl.textContent = s.username;
    loginCard.classList.add("hidden"); mainCard.classList.remove("hidden");
    await refresh();
  } catch (e) {
    loginErr.textContent = String(e); loginBtn.disabled = false; loginBtn.textContent = "SIGN IN";
  }
}
loginBtn.onclick = doLogin;

async function doSignUp() {
  loginErr.textContent = ""; loginBtn.disabled = true; signupBtn.disabled = true; signupBtn.textContent = "CREATING…";
  try {
    const s = await invoke("sign_up", { email: emailEl.value.trim(), password: passwordEl.value });
    passwordEl.value = ""; whoEl.textContent = s.username;
    loginCard.classList.add("hidden"); mainCard.classList.remove("hidden");
    await refresh();
  } catch (e) {
    loginErr.textContent = String(e);
  } finally {
    loginBtn.disabled = false; loginBtn.textContent = "SIGN IN";
    signupBtn.disabled = false; signupBtn.textContent = "CREATE ACCOUNT";
  }
}
signupBtn.onclick = doSignUp;
passwordEl.addEventListener("keydown", (e) => { if (e.key === "Enter") doLogin(); });
emailEl.addEventListener("keydown", (e) => { if (e.key === "Enter") doLogin(); });

async function install() {
  actionBtn.disabled = true; actionBtn.textContent = "PREPARING UPDATE…"; setBar(0);
  if (progressUnlisten) { try { progressUnlisten(); } catch (_) {} progressUnlisten = null; }
  try {
    setStatus("Fetching the official manifest…");
    const manifestJson = await invoke("fetch_manifest", { manifestUrl: manifestUrl() });
    const manifest = JSON.parse(manifestJson);
    if (!manifest || !Array.isArray(manifest.files) || !manifest.version) throw new Error("The published manifest is missing its version or file list.");
    setVersion("TARGET " + manifest.version); setStatus("Preparing " + manifest.files.length + " files…");
    if (progressLabelEl) progressLabelEl.textContent = "Verifying / downloading files";
    progressUnlisten = await listen("progress", (e) => {
      const p = e.payload || {};
      setBar(p.files_total ? p.files_done / p.files_total : 0);
      setStatus(p.file ? (p.total > 0 ? p.file + " — " + fmtMB(p.downloaded) + " / " + fmtMB(p.total) : "Verifying " + p.file + "…") : "Verifying file checksums…");
    });
    await invoke("download_game", { manifestJson });
    localStorage.setItem("fluxrec_game_version", String(manifest.version));
    setStatus("Game files verified. Finishing update…"); setBar(1); setVersion("INSTALLED " + manifest.version);
    await refresh();
  } catch (e) {
    setStatus("Update failed: " + String(e)); setVersion("RETRY AVAILABLE");
    actionBtn.disabled = false; actionBtn.textContent = "↻  RETRY UPDATE"; actionBtn.onclick = install;
  } finally {
    if (progressUnlisten) { try { progressUnlisten(); } catch (_) {} progressUnlisten = null; }
    if (progressLabelEl) progressLabelEl.textContent = "Game files";
  }
}

async function play() {
  actionBtn.disabled = true; actionBtn.textContent = "LAUNCHING…"; setStatus("Starting Flux Rec…");
  try {
    await invoke("launch_game", { extraArgs: [] });
    setStatus("Game is running. Have fun!"); actionBtn.disabled = false; actionBtn.textContent = "▶  PLAY AGAIN";
  } catch (e) {
    setStatus("Launch failed: " + String(e)); actionBtn.disabled = false; actionBtn.textContent = "↻  RETRY LAUNCH";
  }
}
