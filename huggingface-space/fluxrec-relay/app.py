"""Gradio Space — HTTP keepalive on 7860; Tachyon + playit in the background (free cpu-basic)."""
import base64
import os
import shutil
import subprocess
import time
import urllib.request
from pathlib import Path

import gradio as gr

ROOT = Path(__file__).resolve().parent
PLAYIT_URL = (
    "https://github.com/playit-cloud/playit-agent/releases/download/v2.0.0-r1/"
    "playitd-linux-x86_64"
)


def ensure_playit() -> Path:
    dest = ROOT / "playitd"
    if dest.is_file():
        return dest
    urllib.request.urlretrieve(PLAYIT_URL, dest)
    dest.chmod(0o755)
    return dest


def start_relay() -> str:
    key_b64 = os.environ.get("TACHYON_SERVER_KEY_B64", "")
    playit_key = os.environ.get("PLAYIT_SECRET_KEY", "")
    if not key_b64 or not playit_key:
        return "Secrets missing — set PLAYIT_SECRET_KEY and TACHYON_SERVER_KEY_B64 on the Space."
    (ROOT / "server-key.xml").write_bytes(base64.b64decode(key_b64))
    playit_key = playit_key.strip()
    (ROOT / "playit-secret.txt").write_text(playit_key, encoding="utf-8")
    (ROOT / "playit.toml").write_text(f'secret_key = "{playit_key}"\n', encoding="utf-8")
    cfg = Path.home() / ".config" / "playit_gg"
    cfg.mkdir(parents=True, exist_ok=True)
    (cfg / "playit.toml").write_text((ROOT / "playit.toml").read_text(encoding="utf-8"), encoding="utf-8")

    env = os.environ.copy()
    env["TACHYON_PRESENCE_BASE"] = os.environ.get(
        "TACHYON_PRESENCE_BASE", "https://match.ripo-ripoteam.workers.dev"
    )
    tachyon = ROOT / "TachyonServer"
    if not tachyon.is_file():
        return f"Missing {tachyon}"

    subprocess.Popen([str(tachyon), "--log-path", str(ROOT / "server.log")], cwd=ROOT, env=env)
    subprocess.Popen([str(ensure_playit())], cwd=ROOT)
    return "Relay starting (Tachyon + playit)."


_status = start_relay()
time.sleep(2)


def health() -> str:
    return _status + "\n" + time.strftime("%Y-%m-%d %H:%M:%S UTC", time.gmtime())


demo = gr.Interface(fn=health, inputs=None, outputs="text", title="Flux Rec relay")
demo.launch(server_name="0.0.0.0", server_port=7860)