#!/usr/bin/env python3
"""
RRFlux client patcher v1 — redirects a Rec Room client build to RRFlux servers.

Patches (in place, with .bak backups):
  1. RecRoom_Data/il2cpp_data/Metadata/global-metadata.dat  (metadata v27)
     - Replaces endpoint host literals: auth/api/web/telemetry.
     - Replacement hosts must be SHORTER-or-equal to the originals: literal
       bytes are overwritten in place (zero-padded) and the literal length
       field in the string-literal table is updated. Longer hosts are rejected.
  2. RecRoom_Data/resources.assets
     - Swaps the two Photon App ID GUIDs found after the PhotonServerSettings
       marker. GUIDs are always 36 chars, so the swap is trivially length-safe.
     - Role mapping (verified by binary layout, 2026-09-19): the two GUIDs
       sit 44 bytes apart = 36-char GUID + 4-byte length prefix + one empty
       string between them. That matches PUN 2's PhotonServerSettings field
       order AppIdRealtime, AppIdChat, AppIdVoice with Chat empty (Rec Room
       never used Photon Chat). So GUID #1 = AppIdRealtime, #2 = AppIdVoice.
  3. EasyAntiCheat neutering (default on, --keep-eac to skip)
     - Deletes the EasyAntiCheat/ installer directory (service setup files).
     - Replaces RecRoom_Data/Plugins/x86_64/EasyAntiCheat.dll with a minimal
       stub DLL exporting the 11 Cerberus_* functions as no-ops, so the
       client's EasyAntiCheat.Runtime.Initialize() resolves and runs against
       dead stubs instead of the (defunct) EAC backend.

Usage:
    patch.py --build <path-to-a-COPY-of-the-build> \\
        --auth-host <host> --api-host <host> \\
        [--web-host <host>] [--telemetry-host <host>] \\
        [--photon-guid-1 <guid>] [--photon-guid-2 <guid>]

    patch.py --build <copy> --local      # recommended: local translator mode
        (auth/api/telemetry -> localhost, http; no cloud translator needed)

COPY THE PRISTINE BUILD FIRST. Never patch the original: the script refuses
to run twice on the same tree (it checks for .bak files).

Photon/App ID values are passed on the command line (or env vars) and are
never written anywhere except into the patched binary copy.
"""

import argparse
import os
import re
import shutil
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eac_stub import build_stub_dll

GUID_RE = re.compile(rb"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
                     rb"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")


# ---------------------------------------------------------------- metadata
def parse_metadata(path):
    with open(path, "rb") as f:
        data = bytearray(f.read())
    magic, version = struct.unpack("<II", data[0:8])
    if magic != 0xFAB11BAF:
        raise SystemExit(f"not an IL2CPP metadata file: {path}")
    if version != 27:
        raise SystemExit(f"unsupported metadata version {version} (expected 27)")
    # v27 header: stringLiteralOffset/Size = literal TABLE, then Data = raw bytes
    (table_off, table_size, data_off, data_size) = struct.unpack(
        "<4i", data[8:24])
    n = table_size // 8
    return data, table_off, data_off, n


def iter_literals(data, table_off, data_off, n):
    for i in range(n):
        length, off = struct.unpack("<Ii", data[table_off + i * 8:
                                                table_off + i * 8 + 8])
        yield i, length, off


def patch_metadata(path, replacements):
    """replacements: list of (old_substr, new_substr), applied longest-first.

    Runs to a fixpoint (bounded): a replacement may introduce text that a
    later-listed replacement matches (e.g. --local first swaps the host,
    then downgrades the scheme of the resulting 127.0.0.1 URL)."""
    data, table_off, data_off, n = parse_metadata(path)
    reps = sorted(replacements, key=lambda r: -len(r[0]))
    changed = []
    for _pass in range(5):
        pass_changed = False
        for i, length, off in iter_literals(data, table_off, data_off, n):
            if length == 0 or length > 512:
                continue
            raw = bytes(data[data_off + off:data_off + off + length])
            try:
                text = raw.decode("utf-8")
            except UnicodeDecodeError:
                continue
            new_text = text
            for old, new in reps:
                if old in new_text:
                    new_text = new_text.replace(old, new)
            if new_text == text:
                continue
            new_raw = new_text.encode("utf-8")
            if len(new_raw) > length:
                raise SystemExit(
                    f"replacement too long for literal #{i} "
                    f"({len(new_raw)} > {length} bytes): {text!r} -> {new_text!r}\n"
                    f"Pick a shorter hostname.")
            # overwrite bytes, zero-pad the remainder, update length field
            data[data_off + off:data_off + off + length] = b"\x00" * length
            data[data_off + off:data_off + off + len(new_raw)] = new_raw
            struct.pack_into("<I", data, table_off + i * 8, len(new_raw))
            changed.append((i, text, new_text))
            pass_changed = True
        if not pass_changed:
            break
    backup = path + ".bak"
    shutil.copy2(path, backup)
    with open(path, "wb") as f:
        f.write(data)
    return changed, backup


# ------------------------------------------------------------------ assets
def patch_assets(path, guid1, guid2):
    with open(path, "rb") as f:
        data = bytearray(f.read())
    marker = data.find(b"PhotonServerSettings")
    if marker == -1:
        raise SystemExit("PhotonServerSettings marker not found in assets")
    found = [(m.start(), m.group()) for m in GUID_RE.finditer(data)]
    if len(found) < 2:
        raise SystemExit(f"expected 2 Photon GUIDs, found {len(found)}")
    # the two GUIDs right after the marker are the App IDs
    after = [g for off, g in found if off > marker][:2]
    if len(after) < 2:
        raise SystemExit("could not locate the two Photon App ID GUIDs")
    swaps = []
    if guid1:
        swaps.append((after[0], guid1.encode()))
    if guid2:
        swaps.append((after[1], guid2.encode()))
    for old, new in swaps:
        if len(new) != 36 or not GUID_RE.fullmatch(new):
            raise SystemExit(f"not a valid GUID: {new!r}")
        data = data.replace(old, new)
    backup = path + ".bak"
    shutil.copy2(path, backup)
    with open(path, "wb") as f:
        f.write(data)
    return [(o.decode(), n.decode()) for o, n in swaps], backup


# --------------------------------------------------------------------- eac
def neuter_eac(build):
    """Delete EAC installer dir; swap the EAC plugin DLL for a no-op stub."""
    steps = []
    eac_dir = os.path.join(build, "EasyAntiCheat")
    if os.path.isdir(eac_dir):
        shutil.rmtree(eac_dir)
        steps.append(f"removed {eac_dir}/")
    dll = os.path.join(build, "RecRoom_Data", "Plugins", "x86_64",
                       "EasyAntiCheat.dll")
    if os.path.isfile(dll):
        bak = dll + ".bak"
        if not os.path.exists(bak):
            shutil.copy2(dll, bak)
        with open(dll, "wb") as f:
            f.write(build_stub_dll())
        steps.append(f"replaced {dll} with no-op stub (backup: {bak})")
    else:
        steps.append("EAC plugin DLL not found, nothing to stub")
    return steps


# ----------------------------------------------------------------------
def main():
    ap = argparse.ArgumentParser(description="RRFlux client patcher v1")
    ap.add_argument("--build", required=True,
                    help="path to a COPY of the pristine build")
    ap.add_argument("--auth-host",
                    default=os.environ.get("RRFLUX_AUTH_HOST"),
                    help="replaces auth.rec.net (<=12 chars)")
    ap.add_argument("--api-host",
                    default=os.environ.get("RRFLUX_API_HOST"),
                    help="replaces ns.rec.net (<=10 chars)")
    ap.add_argument("--web-host", default=os.environ.get("RRFLUX_WEB_HOST"),
                    help="replaces rec.net in web links (<=7 chars)")
    ap.add_argument("--telemetry-host",
                    default=os.environ.get("RRFLUX_TELEMETRY_HOST"),
                    help="replaces api2.amplitude.com (<=18 chars)")
    ap.add_argument("--photon-guid-1",
                    default=os.environ.get("RRFLUX_PHOTON_GUID_1"),
                    help="replaces AppIdRealtime (1st GUID after the marker)")
    ap.add_argument("--photon-guid-2",
                    default=os.environ.get("RRFLUX_PHOTON_GUID_2"),
                    help="replaces AppIdVoice (2nd GUID after the marker)")
    ap.add_argument("--keep-eac", action="store_true",
                    help="skip EAC neutering (not recommended: EAC backend "
                         "is dead)")
    ap.add_argument("--local", action="store_true",
                    help="point auth/api/telemetry at the launcher's local "
                         "translator (localhost:80, plain http). This is the "
                         "recommended RRFlux mode: no cloud translator, no "
                         "custom domain needed. Overrides --auth-host etc.")
    args = ap.parse_args()

    meta = os.path.join(args.build, "RecRoom_Data", "il2cpp_data",
                        "Metadata", "global-metadata.dat")
    assets = os.path.join(args.build, "RecRoom_Data", "resources.assets")
    for p in (meta, assets):
        if not os.path.isfile(p):
            raise SystemExit(f"missing: {p}")
        if os.path.exists(p + ".bak"):
            raise SystemExit(f"already patched (found {p}.bak); "
                             f"start from a fresh copy")

    reps = []
    if args.local:
        # Local-translator mode: the launcher serves the Rec Room API itself
        # on 127.0.0.1:443 (HTTPS). "localhost" (9 chars) fits every hostname slot.
        # We use "localhost" (not "127.0.0.1") because the original hosts
        # (ns.rec.net, auth.rec.net) were hostnames, not IPs. Using a bare
        # IP can trigger IP-specific URI construction paths (e.g. building
        # "127.0.0.1:port" without a scheme) that throw "Invalid URI scheme"
        # in Mono's new Uri(). "localhost" is syntactically a valid URI
        # scheme-name, resolves to 127.0.0.1, and preserves the original
        # hostname code paths.
        # IMPORTANT (2026-09-19): Do NOT downgrade https:// -> http://.
        # The game's HTTP wrapper validates that API URLs use https://
        # and throws "Invalid URI scheme" for http:// URLs. The local
        # translator must serve HTTPS (with a cert the game trusts).
        for host in ("auth.rec.net", "ns.rec.net", "api2.amplitude.com"):
            reps.append((host, "localhost"))
        # (https://localhost -> http://localhost downgrade REMOVED 2026-09-19)
    else:
        if args.auth_host:
            reps.append(("auth.rec.net", args.auth_host))
        if args.api_host:
            reps.append(("ns.rec.net", args.api_host))
        if args.telemetry_host:
            reps.append(("api2.amplitude.com", args.telemetry_host))
    if args.web_host:
        reps.append(("rec.net", args.web_host))
    if not reps and not (args.photon_guid_1 or args.photon_guid_2) \
            and args.keep_eac:
        raise SystemExit("nothing to patch: pass at least one replacement")

    if not args.keep_eac:
        for step in neuter_eac(args.build):
            print(f"[eac] {step}")

    if reps:
        changed, bak = patch_metadata(meta, reps)
        print(f"[metadata] backup: {bak}")
        print(f"[metadata] patched {len(changed)} literals:")
        for i, old, new in changed:
            print(f"  #{i}: {old!r}\n      -> {new!r}")
    if args.photon_guid_1 or args.photon_guid_2:
        swaps, bak = patch_assets(assets, args.photon_guid_1,
                                  args.photon_guid_2)
        print(f"[assets] backup: {bak}")
        for old, new in swaps:
            print(f"[assets] GUID {old}\n      -> {new}")
    print("done.")


if __name__ == "__main__":
    main()
