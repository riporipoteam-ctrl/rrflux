"""Deploy Flux Rec relay to **Echoxr/ripoteamnewserver** (Gradio Linux Space).

Same model as ripobot-desktop: packages.txt + app.py + background servers — no Dockerfile.
"""
from __future__ import annotations

import base64
import os
import subprocess
import sys
from pathlib import Path

from huggingface_hub import HfApi

REPO = os.environ.get("HF_SPACE", "Echoxr/ripoteamnewserver")
ROOT = Path(__file__).resolve().parents[2] / "huggingface-space" / "ripoteamnewserver"
ALLOW = frozenset(
    {"README.md", "app.py", "requirements.txt"}
)
_owner = REPO.split("/")[0] if "/" in REPO else "Echoxr"
TACHYON_DATASET = os.environ.get("TACHYON_DATASET", f"{_owner}/fluxrec-tachyon-linux")


def publish_linux_tachyon() -> None:
    out = ROOT / "publish"
    out.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [
            "dotnet",
            "publish",
            "-c",
            "Release",
            "-r",
            "linux-x64",
            "--self-contained",
            "true",
            "-p:PublishSingleFile=true",
            "-p:IncludeNativeLibrariesForSelfExtract=true",
            str(
                Path(__file__).resolve().parents[1]
                / "src"
                / "TachyonServer"
                / "TachyonServer.csproj"
            ),
            "-o",
            str(out),
        ],
        check=True,
    )
    dest = ROOT / "TachyonServer"
    dest.write_bytes((out / "TachyonServer").read_bytes())
    if os.name != "nt":
        dest.chmod(dest.stat().st_mode | 0o111)


def ensure_space(api: HfApi) -> None:
    try:
        info = api.repo_info(repo_id=REPO, repo_type="space")
        print("Space exists:", REPO, "sdk:", getattr(info, "card_data", info))
    except Exception:
        try:
            api.create_repo(
                repo_id=REPO,
                repo_type="space",
                space_sdk="gradio",
                space_hardware=os.environ.get("HF_SPACE_HARDWARE", "zero-a10g"),
                private=False,
                exist_ok=True,
            )
            print("Created gradio space:", REPO)
        except Exception as create_err:
            msg = str(create_err)
            if "402" in msg or "PRO" in msg.upper():
                raise SystemExit(
                    "This Gradio Space could not be created (HTTP 402 / Pro). "
                    "If ripoteamnewserver already exists on your account, only upload runs — ignore."
                ) from create_err
            raise


def push_secrets(api: HfApi) -> None:
    flux_key = Path(r"C:\Games\FluxRec\tachyon-server\server-key.xml")
    playit_key = Path(r"C:\Games\FluxRec\tachyon-server\playit-secret.txt")
    secrets = {
        "PLAYIT_SECRET_KEY": os.environ.get(
            "PLAYIT_SECRET_KEY", playit_key.read_text().strip() if playit_key.is_file() else ""
        ),
        "TACHYON_SERVER_KEY_B64": os.environ.get(
            "TACHYON_SERVER_KEY_B64",
            base64.b64encode(flux_key.read_bytes()).decode() if flux_key.is_file() else "",
        ),
        "TACHYON_PRESENCE_BASE": os.environ.get(
            "TACHYON_PRESENCE_BASE", "https://match.ripo-ripoteam.workers.dev"
        ),
        "TACHYON_DATASET": TACHYON_DATASET,
    }
    for key, value in secrets.items():
        if not value:
            raise SystemExit(f"Missing secret {key}")
        api.add_space_secret(repo_id=REPO, key=key, value=value)
        print("space secret", key)


def upload_tachyon_dataset(api: HfApi, tachyon_path: Path) -> None:
    try:
        api.repo_info(repo_id=TACHYON_DATASET, repo_type="dataset")
    except Exception:
        api.create_repo(
            repo_id=TACHYON_DATASET,
            repo_type="dataset",
            private=False,
            exist_ok=True,
        )
        print("Created dataset:", TACHYON_DATASET)
    api.upload_file(
        path_or_fileobj=str(tachyon_path),
        path_in_repo="TachyonServer",
        repo_id=TACHYON_DATASET,
        repo_type="dataset",
        commit_message="Tachyon linux-x64 for cloud relay",
    )
    print("dataset tachyon uploaded")


def upload_files(api: HfApi) -> None:
    for name in sorted(ALLOW):
        path = ROOT / name
        if not path.is_file():
            raise SystemExit(f"missing {path}")
        api.upload_file(
            path_or_fileobj=str(path),
            path_in_repo=name,
            repo_id=REPO,
            repo_type="space",
            commit_message="Ripo Team Server: Tachyon + playit relay",
        )
        print("uploaded", name)


def main() -> None:
    token = os.environ.get("HF_TOKEN")
    if not token:
        try:
            from huggingface_hub import get_token

            token = get_token()
        except Exception:
            token = None
    if not token:
        raise SystemExit("Set HF_TOKEN or run hf auth login")
    tachyon_bin = ROOT / "TachyonServer"
    if "--build" in sys.argv:
        publish_linux_tachyon()
    if not tachyon_bin.is_file() and (ROOT / "publish" / "TachyonServer").is_file():
        tachyon_bin.write_bytes((ROOT / "publish" / "TachyonServer").read_bytes())
    api = HfApi(token=token)
    ensure_space(api)
    push_secrets(api)
    if tachyon_bin.is_file() and os.environ.get("SKIP_DATASET") != "1":
        upload_tachyon_dataset(api, tachyon_bin)
    try:
        api.delete_file("TachyonServer", repo_id=REPO, repo_type="space")
        print("removed TachyonServer from Space repo")
    except Exception:
        pass
    upload_files(api)
    slug = REPO.replace("/", "-").lower()
    print(f"https://huggingface.co/spaces/{REPO}")
    print(f"keepalive: https://{slug}.hf.space/")


if __name__ == "__main__":
    main()