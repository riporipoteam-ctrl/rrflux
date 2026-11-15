"""Mirror Echoxr/rrflux-game -> riponoob/rrflux-game (one-shot publish bootstrap)."""
from __future__ import annotations

import os
import tempfile
from pathlib import Path

from huggingface_hub import HfApi, snapshot_download

SRC = "Echoxr/rrflux-game"
DST = "riponoob/rrflux-game"


def main() -> None:
    echoxr = os.environ.get("HF_TOKEN_ECHOXR") or os.environ.get("HF_TOKEN")
    rip = os.environ.get("HF_TOKEN_RIPONOOB")
    if not echoxr or not rip:
        raise SystemExit("Set HF_TOKEN_ECHOXR and HF_TOKEN_RIPONOOB")

    with tempfile.TemporaryDirectory(prefix="rrflux-game-mirror-") as tmp:
        root = Path(tmp)
        print(f"Downloading {SRC} …")
        snapshot_download(
            repo_id=SRC,
            repo_type="dataset",
            local_dir=root,
            token=echoxr,
        )
        HfApi(token=rip).create_repo(DST, repo_type="dataset", exist_ok=True)
        print(f"Uploading to {DST} …")
        HfApi(token=rip).upload_folder(
            folder_path=str(root),
            path_in_repo=".",
            repo_id=DST,
            repo_type="dataset",
            commit_message="Mirror from Echoxr/rrflux-game for riponoob publish",
        )
    print("Done:", DST)


if __name__ == "__main__":
    main()