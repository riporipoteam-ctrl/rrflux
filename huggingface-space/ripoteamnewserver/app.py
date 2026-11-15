"""
Ripo Team Server on Hugging Face — Linux Gradio Space (like ripobot-desktop).

Gradio stays on 7860; Tachyon + playit run as background processes in the same container.
"""
from __future__ import annotations

import base64
import json
import os
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


def linux_tools_note() -> str:
    lines: list[str] = []
    for cmd in (("firefox", "--version"), ("curl", "--version")):
        try:
            out = subprocess.run(cmd, capture_output=True, text=True, timeout=15, check=False)
            if out.returncode == 0:
                lines.append(out.stdout.splitlines()[0] if out.stdout else " ".join(cmd))
        except Exception:
            pass
    return "\n".join(lines) if lines else "(system packages still installing or unavailable)"


def start_relay() -> str:
    key_b64 = os.environ.get("TACHYON_SERVER_KEY_B64", "")
    playit_key = os.environ.get("PLAYIT_SECRET_KEY", "")
    if not key_b64 or not playit_key:
        return (
            "Secrets missing — in Space Settings add PLAYIT_SECRET_KEY and "
            "TACHYON_SERVER_KEY_B64 (deploy script can push them)."
        )
    (ROOT / "server-key.xml").write_bytes(base64.b64decode(key_b64))
    playit_key = playit_key.strip()
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
        return f"Missing {tachyon} — run deploy with --build"

    subprocess.Popen(
        [str(tachyon), "--log-path", str(ROOT / "server.log")],
        cwd=ROOT,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    subprocess.Popen([str(ensure_playit())], cwd=ROOT)
    return "Relay starting (Tachyon + playit) on this Linux Space."


_status = start_relay()
time.sleep(2)


def poll_playit() -> str:
    key = os.environ.get("PLAYIT_SECRET_KEY", "").strip()
    if not key:
        return "no playit key"
    try:
        req = urllib.request.Request(
            "https://api.playit.gg/tunnels/list",
            data=b"{}",
            method="POST",
            headers={
                "Authorization": f"agent-key {key}",
                "Content-Type": "application/json",
            },
        )
        with urllib.request.urlopen(req, timeout=20) as resp:
            data = json.loads(resp.read().decode())
        active = [t for t in data.get("data", {}).get("tunnels", []) if t.get("active")]
        return json.dumps(active, indent=2) if active else "no active tunnel yet"
    except Exception as err:
        return f"playit API: {err}"


def panel() -> str:
    return (
        f"{_status}\n\n"
        f"Linux tools:\n{linux_tools_note()}\n\n"
        f"UTC: {time.strftime('%Y-%m-%d %H:%M:%S', time.gmtime())}\n\n"
        f"Tunnels:\n{poll_playit()}"
    )


demo = gr.Interface(fn=panel, inputs=None, outputs="text", title="Ripo Team Server — Flux Rec relay")
demo.launch(server_name="0.0.0.0", server_port=7860)