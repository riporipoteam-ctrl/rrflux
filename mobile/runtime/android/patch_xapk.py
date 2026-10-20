#!/usr/bin/env python3
"""Inject the Flux Rec ARM64 hook into a user-supplied split XAPK.

The script never downloads or ships game files. It rewrites the ARM64 split,
then aligns and signs every APK with one key so Android can install the set in a
single PackageInstaller session.
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

try:
    import lief
except ImportError as exc:  # pragma: no cover - environment prerequisite
    raise SystemExit("Install the package dependency first: python -m pip install lief==0.17.3") from exc


ARM_LIB = "lib/arm64-v8a/libmain.so"
HOOK_LIB = "lib/arm64-v8a/libfluxrec_mobile.so"


def run(*args: str) -> None:
    command = list(args)
    if sys.platform == "win32" and Path(command[0]).suffix.lower() in {".bat", ".cmd"}:
        command = ["cmd.exe", "/d", "/s", "/c", *command]
    subprocess.run(command, check=True)


def copy_zip_without_signatures(source: Path, target: Path, replacements: dict[str, Path]) -> None:
    with zipfile.ZipFile(source, "r") as incoming, zipfile.ZipFile(target, "w", allowZip64=True) as outgoing:
        names = set()
        for item in incoming.infolist():
            upper = item.filename.upper()
            if upper.startswith("META-INF/") and upper.endswith((".RSA", ".DSA", ".EC", ".SF", "MANIFEST.MF")):
                continue
            names.add(item.filename)
            if item.filename in replacements:
                replacement = replacements[item.filename]
                info = zipfile.ZipInfo(item.filename, item.date_time)
                info.compress_type = zipfile.ZIP_STORED
                info.external_attr = item.external_attr
                with replacement.open("rb") as src, outgoing.open(info, "w", force_zip64=True) as dst:
                    shutil.copyfileobj(src, dst, 1024 * 1024)
                continue
            with incoming.open(item, "r") as src, outgoing.open(item, "w", force_zip64=True) as dst:
                shutil.copyfileobj(src, dst, 1024 * 1024)

        for name, replacement in replacements.items():
            if name in names:
                continue
            info = zipfile.ZipInfo(name)
            info.compress_type = zipfile.ZIP_STORED
            info.external_attr = 0o755 << 16
            with replacement.open("rb") as src, outgoing.open(info, "w", force_zip64=True) as dst:
                shutil.copyfileobj(src, dst, 1024 * 1024)


def has_entry(apk: Path, entry: str) -> bool:
    with zipfile.ZipFile(apk, "r") as archive:
        return entry in archive.namelist()


def ensure_keystore(keytool: Path, keystore: Path, password: str) -> None:
    if keystore.exists():
        return
    keystore.parent.mkdir(parents=True, exist_ok=True)
    run(
        str(keytool), "-genkeypair", "-keystore", str(keystore), "-alias", "fluxrec-mobile",
        "-keyalg", "RSA", "-keysize", "3072", "-validity", "10000",
        "-storepass", password, "-keypass", password,
        "-dname", "CN=Flux Rec Mobile, O=Ripo Team",
    )


def patch(args: argparse.Namespace) -> None:
    source = args.xapk.resolve()
    hook = args.hook.resolve()
    output = args.output.resolve()
    if not source.is_file() or not hook.is_file():
        raise SystemExit("The XAPK and compiled hook library must both exist.")

    zipalign = args.build_tools / ("zipalign.exe" if sys.platform == "win32" else "zipalign")
    apksigner = args.build_tools / ("apksigner.bat" if sys.platform == "win32" else "apksigner")
    keytool = args.keytool or Path(shutil.which("keytool") or "")
    for tool, label in ((zipalign, "zipalign"), (apksigner, "apksigner"), (keytool, "keytool")):
        if not tool or not tool.is_file():
            raise SystemExit(f"{label} was not found: {tool}")

    ensure_keystore(keytool, args.keystore, args.password)

    with tempfile.TemporaryDirectory(prefix="fluxrec-mobile-") as temporary:
        root = Path(temporary)
        split_dir = root / "splits"
        split_dir.mkdir()
        with zipfile.ZipFile(source, "r") as xapk:
            apk_entries = [item for item in xapk.infolist() if not item.is_dir() and item.filename.lower().endswith(".apk")]
            if not apk_entries:
                raise SystemExit("The selected XAPK contains no APK splits.")
            for item in apk_entries:
                destination = split_dir / Path(item.filename).name
                with xapk.open(item) as src, destination.open("wb") as dst:
                    shutil.copyfileobj(src, dst, 1024 * 1024)

        arm_split = next((apk for apk in split_dir.glob("*.apk") if has_entry(apk, ARM_LIB)), None)
        if arm_split is None:
            raise SystemExit(f"No split contains {ARM_LIB}; an ARM64 IL2CPP Android build is required.")

        native_dir = root / "native"
        native_dir.mkdir()
        original_main = native_dir / "libmain.so"
        patched_main = native_dir / "libmain.patched.so"
        with zipfile.ZipFile(arm_split, "r") as archive, archive.open(ARM_LIB) as src, original_main.open("wb") as dst:
            shutil.copyfileobj(src, dst, 1024 * 1024)

        binary = lief.parse(str(original_main))
        if binary is None:
            raise SystemExit("LIEF could not parse libmain.so.")
        if "libfluxrec_mobile.so" not in list(binary.libraries):
            binary.add_library("libfluxrec_mobile.so")
        binary.write(str(patched_main))

        signed_dir = root / "signed"
        signed_dir.mkdir()
        for apk in sorted(split_dir.glob("*.apk")):
            unsigned = root / f"{apk.stem}.unsigned.apk"
            aligned = root / f"{apk.stem}.aligned.apk"
            signed = signed_dir / apk.name
            replacements = {ARM_LIB: patched_main, HOOK_LIB: hook} if apk == arm_split else {}
            copy_zip_without_signatures(apk, unsigned, replacements)
            run(str(zipalign), "-f", "-p", "4", str(unsigned), str(aligned))
            run(
                str(apksigner), "sign", "--ks", str(args.keystore), "--ks-key-alias", "fluxrec-mobile",
                "--ks-pass", f"pass:{args.password}", "--key-pass", f"pass:{args.password}",
                "--v2-signing-enabled", "true", "--v3-signing-enabled", "true",
                "--out", str(signed), str(aligned),
            )
            run(str(apksigner), "verify", "--verbose", str(signed))

        output.parent.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_STORED, allowZip64=True) as bundle:
            for apk in sorted(signed_dir.glob("*.apk")):
                bundle.write(apk, apk.name)

    hasher = hashlib.sha256()
    with output.open("rb") as packaged:
        while chunk := packaged.read(8 * 1024 * 1024):
            hasher.update(chunk)
    digest = hasher.hexdigest()
    output.with_suffix(output.suffix + ".sha256").write_text(f"{digest}  {output.name}\n", encoding="ascii")
    print(f"Flux Rec Android runtime: {output}")
    print(f"SHA-256: {digest}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--xapk", type=Path, required=True, help="User-supplied 2025 Android XAPK")
    parser.add_argument("--hook", type=Path, required=True, help="Compiled libfluxrec_mobile.so")
    parser.add_argument("--output", type=Path, required=True, help="Output Flux Rec XAPK/ZIP")
    parser.add_argument("--build-tools", type=Path, required=True, help="Android SDK build-tools directory")
    parser.add_argument("--keystore", type=Path, required=True, help="Persistent signing keystore")
    parser.add_argument("--password", default="fluxrec-mobile", help="Keystore password")
    parser.add_argument("--keytool", type=Path, help="Path to keytool; defaults to PATH")
    patch(parser.parse_args())


if __name__ == "__main__":
    main()
