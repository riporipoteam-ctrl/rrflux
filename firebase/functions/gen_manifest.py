#!/usr/bin/env python3
"""Generate the launcher manifest (manifest.json) from a patched build tree.

Walks the build directory, SHA-256 hashes every file, and emits:
    {"version": "...", "files": [{"path": ..., "sha256": ..., "url": ...}, ...]}

The launcher fetches this from /v1/manifest (served by the api function from
the meta/manifest Firestore doc) and downloads each file from its `url`.

Usage:
    gen_manifest.py <build_dir> <base_url> <version> [-o manifest.json]

Example:
    gen_manifest.py ./patched-build https://cdn.rrflux.example/game 0.1.0

Upload the resulting JSON to the meta/manifest Firestore document
(e.g. via the console), and host the files under <base_url> preserving
relative paths.
"""

import hashlib
import json
import sys
from pathlib import Path


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> None:
    if len(sys.argv) < 4:
        print(__doc__.strip().splitlines()[-8])
        sys.exit(1)
    build_dir = Path(sys.argv[1])
    base_url = sys.argv[2].rstrip("/")
    version = sys.argv[3]
    out = Path(sys.argv[5] if len(sys.argv) > 5 and sys.argv[4] == "-o" else "manifest.json")

    files = []
    for p in sorted(build_dir.rglob("*")):
        if not p.is_file():
            continue
        rel = p.relative_to(build_dir).as_posix()
        files.append(
            {
                "path": rel,
                "sha256": sha256_of(p),
                "url": f"{base_url}/{rel}",
            }
        )
        print(f"hashed {rel}")

    out.write_text(json.dumps({"version": version, "files": files}, indent=2))
    print(f"\nwrote {out} — {len(files)} files, version {version}")


if __name__ == "__main__":
    main()
