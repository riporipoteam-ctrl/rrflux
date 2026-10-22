const { invoke } = window.__TAURI__.core;

const appleDot = document.querySelector("#apple-dot");
const toolsDot = document.querySelector("#tools-dot");
const appleState = document.querySelector("#apple-state");
const toolsState = document.querySelector("#tools-state");
const sideloadlyDot = document.querySelector("#sideloadly-dot");
const sideloadlyState = document.querySelector("#sideloadly-state");
const devices = document.querySelector("#devices");
const wireless = document.querySelector("#wireless");
const ipaPath = document.querySelector("#ipa-path");
const installButton = document.querySelector("#install");
const officialButton = document.querySelector("#official");
const sideloadlyButton = document.querySelector("#sideloadly");
const log = document.querySelector("#log");
let selectedIpa = "";
let sideloadlyReady = false;

function updateInstallState() {
  installButton.disabled = !selectedIpa || !devices.value;
  sideloadlyButton.disabled = !selectedIpa || !sideloadlyReady;
}

async function check() {
  const data = await invoke("diagnostics");
  appleDot.classList.toggle("ok", data.appleMobileDeviceService);
  appleState.textContent = data.appleMobileDeviceService ? "Apple Mobile Device Service is running" : "Install desktop iTunes and start Apple Mobile Device Service";
  const toolsReady = Boolean(data.deviceHelperPath);
  toolsDot.classList.toggle("ok", toolsReady);
  toolsState.textContent = toolsReady ? "Bundled iPhone install engine is ready" : "The bundled iPhone install engine is missing";
  sideloadlyReady = Boolean(data.sideloadlyPath);
  sideloadlyDot.classList.toggle("ok", sideloadlyReady);
  sideloadlyState.textContent = sideloadlyReady ? "Sideloadly signing and auto-refresh are available" : "Sideloadly is optional and was not detected";
  officialButton.disabled = !data.officialIpaConfigured;
}

async function findDevices() {
  devices.innerHTML = '<option value="">Searching…</option>';
  log.textContent = "Searching USB and paired Wi-Fi devices…";
  try {
    const found = await invoke("list_devices");
    devices.innerHTML = '<option value="">Select an iPhone</option>';
    for (const device of found) {
      const option = document.createElement("option");
      option.value = device.udid;
      option.dataset.transport = device.transport;
      option.textContent = `${device.udid} · ${device.transport}`;
      devices.appendChild(option);
    }
    log.textContent = found.length ? `Found ${found.length} device${found.length === 1 ? "" : "s"}.` : "No devices found. Connect by USB, unlock the phone, press Trust, and enable Wi-Fi sync.";
  } catch (error) {
    devices.innerHTML = '<option value="">No device selected</option>';
    log.textContent = String(error);
  }
  updateInstallState();
}

document.querySelector("#recheck").onclick = check;
document.querySelector("#refresh").onclick = findDevices;
devices.onchange = () => {
  wireless.checked = devices.selectedOptions[0]?.dataset.transport === "Wi-Fi";
  updateInstallState();
};

document.querySelector("#browse").onclick = async () => {
  const chosen = await invoke("choose_ipa");
  if (!chosen) return;
  selectedIpa = chosen;
  ipaPath.textContent = chosen;
  log.textContent = "IPA selected. Choose a paired iPhone and install.";
  updateInstallState();
};

officialButton.onclick = async () => {
  officialButton.disabled = true;
  log.textContent = "Downloading and verifying the official launcher IPA…";
  try {
    selectedIpa = await invoke("download_official_ipa");
    ipaPath.textContent = selectedIpa;
    log.textContent = "Official launcher downloaded and verified.";
  } catch (error) {
    log.textContent = String(error);
  }
  officialButton.disabled = false;
  updateInstallState();
};

installButton.onclick = async () => {
  installButton.disabled = true;
  log.textContent = "Installing… Keep the iPhone unlocked and on the same network.";
  try {
    log.textContent = await invoke("install_ipa", { ipaPath: selectedIpa, udid: devices.value, wireless: wireless.checked });
  } catch (error) {
    log.textContent = `Install failed: ${error}`;
  }
  updateInstallState();
};

sideloadlyButton.onclick = async () => {
  sideloadlyButton.disabled = true;
  log.textContent = "Opening the selected IPA in Sideloadly…";
  try {
    log.textContent = await invoke("open_in_sideloadly", { ipaPath: selectedIpa });
  } catch (error) {
    log.textContent = `Sideloadly handoff failed: ${error}`;
  }
  updateInstallState();
};

check().then(findDevices).catch(error => { log.textContent = String(error); });
