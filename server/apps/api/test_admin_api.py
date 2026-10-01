#!/usr/bin/env python3
"""Test the Flux Rec admin API (/api/admin/v1/*). Reads ADMIN_API_KEY from the
server .env; never prints it. Uses curl (urllib is flaky through the egress
proxy here). Creates a throwaway test account, exercises every endpoint.

Usage: python3 test_admin_api.py
"""
import json
import os
import re
import subprocess
import sys

API = "https://api.ripo-ripoteam.workers.dev"
AUTH = "https://auth.ripo-ripoteam.workers.dev"
MATCH = "https://match.ripo-ripoteam.workers.dev"

passed, failed = [], []


def check(name, cond, detail=""):
    (passed if cond else failed).append(name)
    print(("PASS " if cond else "FAIL ") + name + (f" — {detail}" if detail and not cond else ""))


def load_admin_key():
    with open(os.path.expanduser("~/workspace/goals/private-rec-room-revival-build/fluxrec/server/.env")) as f:
        for line in f:
            m = re.match(r"RECFLARE_ADMIN_API_KEY=(.+)", line.strip())
            if m:
                return m.group(1)
    raise SystemExit("RECFLARE_ADMIN_API_KEY not found in server .env")


ADMIN_KEY = load_admin_key()


def curl(method, url, body=None, headers=None):
    cmd = ["curl", "-s", "-w", "\n%{http_code}", "-X", method, url]
    for k, v in (headers or {}).items():
        cmd += ["-H", f"{k}: {v}"]
    if body is not None:
        cmd += ["-H", "Content-Type: application/json", "-d", json.dumps(body)]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=60).stdout
    payload, _, code = out.rpartition("\n")
    try:
        return int(code.strip()), json.loads(payload or "{}")
    except Exception:
        return -1, {"_raw": payload[:200]}


def admin(method, path, body=None, key=ADMIN_KEY):
    h = {"X-Admin-Key": key} if key else {}
    return curl(method, API + path, body, h)


def form_token(grant_type, username, password):
    s, b = curl("POST", AUTH + "/connect/token", None,
                {"Content-Type": "application/x-www-form-urlencoded"})
    # curl with -d and explicit content type; do the real call:
    cmd = ["curl", "-s", "-X", "POST", AUTH + "/connect/token",
           "-d", f"grant_type={grant_type}&username={username}&password={password}"]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=60).stdout
    return json.loads(out)["access_token"]


def main():
    try:
        access = form_token("create_account", "admintest99", "test123456")
        print("test account created: admintest99")
    except Exception:
        access = form_token("password", "admintest99", "test123456")
        print("test account exists, logged in: admintest99")

    # 1. Auth
    s, b = admin("GET", "/api/admin/v1/players/online", key=None)
    check("no key -> 401", s == 401 and b.get("success") is False, f"{s} {b}")
    s, b = admin("GET", "/api/admin/v1/players/online", key="wrong-key")
    check("wrong key -> 401", s == 401 and b.get("success") is False, f"{s} {b}")

    # 2. ranks/set
    s, b = admin("POST", "/api/admin/v1/ranks/set",
                {"username": "admintest99", "rank": "community_mod"})
    check("ranks/set community_mod", s == 200 and b.get("isModerator") is True, f"{s} {b}")
    acct_id = b.get("accountId")
    s2, b2 = curl("GET", f"{AUTH}/role/moderator/{acct_id}")
    check("native /role/moderator/:id true", s2 == 200 and (b2 is True or b2.get("value") is True),
          f"{s2} {b2}")
    s, b = admin("POST", "/api/admin/v1/ranks/set",
                {"username": "admintest99", "rank": "developer"})
    check("ranks/set developer (replaces mod)",
          s == 200 and b.get("isDeveloper") is True and b.get("isModerator") is False, f"{s} {b}")
    s, b = admin("POST", "/api/admin/v1/ranks/set",
                {"username": "admintest99", "rank": "none"})
    check("ranks/set none (removes)",
          s == 200 and b.get("isDeveloper") is False and b.get("isModerator") is False, f"{s} {b}")
    s, b = admin("POST", "/api/admin/v1/ranks/set",
                {"username": "nosuchplayer_xyz", "rank": "developer"})
    check("ranks/set unknown user -> 404", s == 404, f"{s} {b}")
    s, b = admin("POST", "/api/admin/v1/ranks/set",
                {"username": "admintest99", "rank": "bogus"})
    check("ranks/set bad rank -> 400", s == 400, f"{s} {b}")

    # 3. membership/set
    s, b = admin("POST", "/api/admin/v1/membership/set",
                {"username": "admintest99", "duration_months": 2})
    check("membership/set 2 months",
          s == 200 and b.get("hasPlus") is True and b.get("plusUntil"), f"{s} {b}")
    s, b = admin("POST", "/api/admin/v1/membership/set",
                {"username": "admintest99", "duration_months": 0})
    check("membership/set never expires",
          s == 200 and str(b.get("plusUntil", "")).startswith("9999"), f"{s} {b}")
    s, b = admin("POST", "/api/admin/v1/membership/set",
                {"username": "admintest99", "duration_months": -1})
    check("membership/set remove",
          s == 200 and b.get("hasPlus") is False and b.get("plusUntil") is None, f"{s} {b}")

    # 4. tokens/grant
    s, b = admin("POST", "/api/admin/v1/tokens/grant",
                {"username": "admintest99", "amount": 1234})
    check("tokens/grant single",
          s == 200 and b.get("grantedTo") == "admintest99" and (b.get("newBalance") or 0) >= 1234,
          f"{s} {b}")
    before = b["newBalance"]
    s, b = admin("POST", "/api/admin/v1/tokens/grant",
                {"username": "admintest99", "amount": 100})
    check("tokens/grant accumulates",
          s == 200 and b.get("newBalance") == before + 100, f"{s} {b}")
    s, b = admin("POST", "/api/admin/v1/tokens/grant",
                {"grant_to": "everyone", "amount": 7})
    check("tokens/grant everyone",
          s == 200 and b.get("grantedTo") == "everyone" and (b.get("accounts") or 0) > 0, f"{s} {b}")

    # 5. bans
    s, b = admin("POST", "/api/admin/v1/bans/create",
                {"username": "admintest99", "reason": "test ban from admin API",
                 "duration_minutes": 30, "voice_ban": True})
    check("bans/create timed+voice",
          s == 200 and b.get("permanent") is False and b.get("banExpires")
          and b.get("voiceBanned") is True and b.get("voiceBanUntil"), f"{s} {b}")
    s2, b2 = curl("GET", API + "/api/PlayerReporting/v1/moderationBlockDetails",
                  headers={"Authorization": f"Bearer {access}"})
    check("ban reason shown in-game (TopMessageOverride)",
          s2 == 200 and b2.get("IsBan") is True
          and (b2.get("TopMessageOverride") or "") == "test ban from admin API",
          f"{s2} {str(b2)[:200]}")
    s2, b2 = curl("POST", MATCH + "/matchmake/room/2", {},
                  {"Authorization": f"Bearer {access}"})
    refused = s2 == 200 and (b2.get("errorCode") not in (None, 0) or b2.get("result") != 0)
    check("matchmake refused while banned", refused, f"{s2} {str(b2)[:200]}")
    s, b = admin("POST", "/api/admin/v1/bans/lift", {"username": "admintest99"})
    check("bans/lift", s == 200 and b.get("lifted") is True, f"{s} {b}")
    s2, b2 = curl("GET", API + "/api/PlayerReporting/v1/moderationBlockDetails",
                  headers={"Authorization": f"Bearer {access}"})
    check("no longer blocked after lift",
          s2 == 200 and b2.get("IsBan") is not True, f"{s2} {str(b2)[:200]}")
    s, b = admin("POST", "/api/admin/v1/bans/create",
                {"username": "admintest99", "reason": "permanent test", "duration_minutes": 0})
    check("bans/create permanent",
          s == 200 and b.get("permanent") is True and b.get("banExpires") is None, f"{s} {b}")
    s, b = admin("POST", "/api/admin/v1/bans/lift", {"username": "admintest99"})
    check("bans/lift permanent", s == 200 and b.get("lifted") is True, f"{s} {b}")
    s, b = admin("POST", "/api/admin/v1/bans/create",
                {"username": "nosuchplayer_xyz", "reason": "x", "duration_minutes": 5})
    check("bans/create unknown user -> 404", s == 404, f"{s} {b}")

    # 6. players/online
    s, b = admin("GET", "/api/admin/v1/players/online")
    check("players/online",
          s == 200 and b.get("success") is True and isinstance(b.get("count"), int)
          and isinstance(b.get("players"), list), f"{s} {str(b)[:200]}")

    print(f"\n{len(passed)} passed, {len(failed)} failed")
    if failed:
        print("FAILED:", failed)
        sys.exit(1)


if __name__ == "__main__":
    main()
