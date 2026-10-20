#!/usr/bin/env python3
"""Inject the Flux Rec runtime into a user-provided, decrypted iOS IPA.

Run this on macOS with Apple command-line tools, Python 3.11+, and lief 0.17.3.
No game binary or Apple credential is stored in this repository.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    import lief
except ImportError as exc:
    raise SystemExit("Install the pinned dependency: python3 -m pip install lief==0.17.3") from exc


LOAD_PATH = "@executable_path/Frameworks/libfluxrec_mobile.dylib"


def run(*args: str, capture: bool = False, cwd: Path | None = None) -> str:
    result = subprocess.run(args, check=True, text=True, capture_output=capture, cwd=cwd)
    return result.stdout if capture else ""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def find_app(root: Path) -> Path:
    apps = list((root / "Payload").glob("*.app"))
    if len(apps) != 1:
        raise SystemExit("IPA must contain exactly one Payload/*.app bundle")
    return apps[0]


def assert_decrypted(executable: Path) -> None:
    output = run("otool", "-l", str(executable), capture=True)
    lines = [line.strip() for line in output.splitlines()]
    for index, line in enumerate(lines):
        if line == "cryptid 1" or (line.startswith("cryptid ") and not line.endswith(" 0")):
            raise SystemExit(
                "The supplied IPA is FairPlay encrypted. Export a decrypted, legally obtained device build first."
            )


def inject_load_command(executable: Path) -> None:
    parsed = lief.MachO.parse(str(executable))
    if parsed is None:
        raise SystemExit("LIEF could not parse the iOS executable")
    binaries = list(parsed)
    if not binaries:
        raise SystemExit("The iOS executable has no Mach-O slices")
    for binary in binaries:
        libraries = {library.name for library in binary.libraries}
        if LOAD_PATH not in libraries:
            binary.add_library(LOAD_PATH)
    parsed.write(str(executable))


def add_launch_scheme(info: dict) -> None:
    url_types = info.setdefault("CFBundleURLTypes", [])
    for entry in url_types:
        if "fluxrec" in entry.get("CFBundleURLSchemes", []):
            return
    url_types.append({
        "CFBundleURLName": "com.ripoteam.fluxrec.launch",
        "CFBundleURLSchemes": ["fluxrec"],
    })


def extract_entitlements(profile: Path, output: Path) -> None:
    decoded = subprocess.run(
        ["security", "cms", "-D", "-i", str(profile)],
        check=True,
        capture_output=True,
    ).stdout
    profile_plist = plistlib.loads(decoded)
    with output.open("wb") as stream:
        plistlib.dump(profile_plist["Entitlements"], stream)


def resign(app: Path, identity: str, entitlements: Path) -> None:
    signature = app / "_CodeSignature"
    if signature.exists():
        shutil.rmtree(signature)

    nested = []
    for folder_name in ("Frameworks", "PlugIns", "Watch"):
        folder = app / folder_name
        if folder.exists():
            nested.extend(path for path in folder.rglob("*") if path.suffix in {".framework", ".dylib", ".appex", ".app"})
    for path in sorted(set(nested), key=lambda item: len(item.parts), reverse=True):
        run("codesign", "--force", "--sign", identity, "--timestamp=none", str(path))
    run(
        "codesign", "--force", "--sign", identity, "--timestamp=none",
        "--entitlements", str(entitlements), str(app),
    )
    run("codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app))


def main() -> int:
    parser = argparse.ArgumentParser(description="Build a signed Flux Rec iOS IPA")
    parser.add_argument("input_ipa", type=Path)
    parser.add_argument("runtime_dylib", type=Path)
    parser.add_argument("output_ipa", type=Path)
    parser.add_argument("--identity", required=True, help="Apple Development/Distribution codesign identity")
    parser.add_argument("--profile", type=Path, help="Provisioning profile; defaults to the IPA's embedded profile")
    parser.add_argument("--bundle-id", help="Bundle identifier covered by the supplied profile")
    args = parser.parse_args()

    if sys.platform != "darwin":
        raise SystemExit("IPA patching and signing must run on macOS")
    for path in (args.input_ipa, args.runtime_dylib):
        if not path.is_file():
            raise SystemExit(f"Missing input file: {path}")

    args.output_ipa.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="fluxrec-ios-") as temporary:
        root = Path(temporary)
        run("ditto", "-x", "-k", str(args.input_ipa.resolve()), str(root))
        app = find_app(root)
        info_path = app / "Info.plist"
        with info_path.open("rb") as stream:
            info = plistlib.load(stream)
        executable = app / info["CFBundleExecutable"]
        assert_decrypted(executable)

        frameworks = app / "Frameworks"
        frameworks.mkdir(exist_ok=True)
        target_dylib = frameworks / "libfluxrec_mobile.dylib"
        shutil.copy2(args.runtime_dylib, target_dylib)
        inject_load_command(executable)

        info["CFBundleDisplayName"] = "Flux Rec"
        if args.bundle_id:
            info["CFBundleIdentifier"] = args.bundle_id
        add_launch_scheme(info)
        with info_path.open("wb") as stream:
            plistlib.dump(info, stream)

        profile = args.profile or (app / "embedded.mobileprovision")
        if not profile.is_file():
            raise SystemExit("A matching provisioning profile is required")
        if args.profile:
            shutil.copy2(profile, app / "embedded.mobileprovision")
            profile = app / "embedded.mobileprovision"
        entitlements = root / "entitlements.plist"
        extract_entitlements(profile, entitlements)
        resign(app, args.identity, entitlements)

        if args.output_ipa.exists():
            args.output_ipa.unlink()
        run(
            "ditto", "-c", "-k", "--sequesterRsrc", "--keepParent",
            "Payload", str(args.output_ipa.resolve()), cwd=root,
        )

    print(f"Created {args.output_ipa} (sha256 {sha256(args.output_ipa)})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
