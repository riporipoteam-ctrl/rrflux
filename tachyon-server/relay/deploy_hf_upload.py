import base64
import os
from pathlib import Path

from huggingface_hub import HfApi

repo = "Echoxr/fluxrec-relay"
api = HfApi(token=os.environ["HF_TOKEN"])

try:
    api.repo_info(repo_id=repo, repo_type="space")
    print("Space exists:", repo)
except Exception as e:
    try:
        api.create_repo(
            repo_id=repo,
            repo_type="space",
            space_sdk="docker",
            space_hardware="cpu-basic",
            exist_ok=True,
        )
        print("Created space:", repo)
    except Exception as create_err:
        msg = str(create_err)
        if "402" in msg or "PRO" in msg:
            raise SystemExit(
                "HF Docker/Gradio cpu-basic needs Hugging Face Pro. "
                "Use PC stay-awake relay (deploy_hf_dataset.py) or HF Pro."
            ) from create_err
        raise e

flux_key = Path(r"C:\Games\FluxRec\tachyon-server\server-key.xml")
playit_key = Path(r"C:\Games\FluxRec\tachyon-server\playit-secret.txt")
secrets = {
    "PLAYIT_SECRET_KEY": os.environ.get("PLAYIT_SECRET_KEY", playit_key.read_text().strip()),
    "TACHYON_SERVER_KEY_B64": os.environ.get(
        "TACHYON_SERVER_KEY_B64", base64.b64encode(flux_key.read_bytes()).decode()
    ),
    "TACHYON_PRESENCE_BASE": "https://match.ripo-ripoteam.workers.dev",
}
for key, value in secrets.items():
    api.add_space_secret(repo_id=repo, key=key, value=value)
    print("space secret", key)

root = Path(__file__).resolve().parents[2] / "huggingface-space" / "fluxrec-relay"
for name in ("Dockerfile", "README.md", "entrypoint.sh", "health_server.py", "TachyonServer"):
    path = root / name
    if not path.is_file():
        raise SystemExit(f"missing {path}")
    api.upload_file(
        path_or_fileobj=str(path),
        path_in_repo=name,
        repo_id=repo,
        repo_type="space",
        commit_message="deploy fluxrec relay",
    )
    print("uploaded", name)
print("https://huggingface.co/spaces/Echoxr/fluxrec-relay")