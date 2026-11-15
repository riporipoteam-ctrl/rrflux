"""Upload linux relay binary to the public dataset (works on free HF accounts)."""
import os
from pathlib import Path

from huggingface_hub import HfApi

REPO = "Echoxr/rrflux-game"
api = HfApi(token=os.environ["HF_TOKEN"])
root = Path(__file__).resolve().parents[2] / "huggingface-space" / "fluxrec-relay"
binary = root / "TachyonServer"
if not binary.is_file():
    raise SystemExit(f"Run dotnet publish first; missing {binary}")
api.upload_file(
    path_or_fileobj=str(binary),
    path_in_repo="tachyon-server/TachyonServer-linux-x64",
    repo_id=REPO,
    repo_type="dataset",
    commit_message="Tachyon linux relay binary for VPS/CI",
)
print("uploaded tachyon-server/TachyonServer-linux-x64")