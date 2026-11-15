"""Flux Rec cloud relay — Gradio + optional ZeroGPU decorator."""
from __future__ import annotations

import base64
import os
import stat
import subprocess
import time
import urllib.request
from pathlib import Path

import gradio as gr

try:
    import spaces

    _HAS_SPACES = True
except ImportError:
    spaces = None
    _HAS_SPACES = False

ROOT = Path(__file__).resolve().parent
PLAYIT_URL = (
    "https://github.com/playit-cloud/playit-agent/releases/download/v2.0.2/"
    "playit-cli-linux-amd64"
)


def tachyon_url() -> str:
    custom = os.environ.get("TACHYON_DATASET_URL", "").strip()
    if custom:
        return custom
    ds = os.environ.get("TACHYON_DATASET", "riponoob/fluxrec-tachyon-linux")
    return f"https://huggingface.co/datasets/{ds}/resolve/main/TachyonServer"


def _download(url: str, dest: Path) -> None:
    if dest.is_file():
        return
    urllib.request.urlretrieve(url, dest)
    dest.chmod(dest.stat().st_mode | stat.S_IEXEC)


def boot() -> str:
    key_b64 = os.environ.get("TACHYON_SERVER_KEY_B64", "")
    playit_key = os.environ.get("PLAYIT_SECRET_KEY", "").strip()
    if not key_b64 or not playit_key:
        return "missing secrets"

    state = ROOT / "state"
    state.mkdir(exist_ok=True)
    (state / "server-key.xml").write_bytes(base64.b64decode(key_b64))

    env = os.environ.copy()
    env["TACHYON_PRESENCE_BASE"] = os.environ.get(
        "TACHYON_PRESENCE_BASE", "https://match.ripo-ripoteam.workers.dev"
    )
    env["TACHYON_CONSOLE_ONLY"] = "1"
    env["DOTNET_BUNDLE_EXTRACT_BASE_DIR"] = str(state / "dotnet-extract")

    _download(tachyon_url(), ROOT / "TachyonServer")
    _download(PLAYIT_URL, ROOT / "playit")
    subprocess.Popen(
        [str(ROOT / "TachyonServer"), "--key-dir", str(state)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    subprocess.Popen(
        [str(ROOT / "playit"), "--secret", playit_key, "--platform-docker"],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    return "cloud relay started"


def _status_text() -> str:
    return f"{MSG} @ {time.strftime('%H:%M:%S', time.gmtime())}"


if _HAS_SPACES:

    @spaces.GPU(duration=300)
    def status_gpu() -> str:
        return _status_text()

    _poll = status_gpu
else:
    _poll = _status_text

MSG = boot()
time.sleep(1)

with gr.Blocks(title="Flux Rec relay") as demo:
    gr.Markdown("**Flux Rec relay** — keeps this Space awake; Tachyon + playit run in the background.")
    st = gr.Textbox(value=MSG, label="status")
    demo.load(_poll, outputs=st)

demo.launch(server_name="0.0.0.0", server_port=7860)