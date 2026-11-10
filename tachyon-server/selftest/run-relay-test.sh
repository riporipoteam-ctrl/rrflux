#!/usr/bin/env bash
# Tachyon two-client relay test: client B joins and listens; client A joins
# and sends a DissonanceToServer voice probe; the server must relay it to B
# as a DissonanceToClient named message. Same sandbox UDP shim as
# run-selftest.sh (see the note there).
#
# KNOWN SANDBOX LIMIT (verified 2026-11-08): this test CANNOT pass inside
# the Muse VM. The shim leaves the server's single UDP socket connected to
# the first approved peer, and this netstack kernel-filters inbound
# datagrams from any other peer — a second client's ConnectRequest never
# reaches LiteNetLib (the server log shows only the first client). On a
# normal host (no shim, unconnected socket) the same binaries + script are
# expected to pass as-is; run it there first when a UDP host exists.
set -u
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
DOTNET=/usr/bin/dotnet
PORT="${TACHYON_TEST_PORT:-17778}"
SERVER_LOG="$ROOT/selftest/relay-server.log"
A_LOG="$ROOT/selftest/relay-client-a.log"
B_LOG="$ROOT/selftest/relay-client-b.log"

SERVER_DLL="$ROOT/src/TachyonServer/bin/Release/net8.0/TachyonServer.dll"
CLIENT_DLL="$ROOT/selftest/TachyonTestClient/bin/Release/net8.0/TachyonTestClient.dll"
for f in "$SERVER_DLL" "$CLIENT_DLL"; do
  [ -f "$f" ] || { echo "RELAYTEST: FAIL (missing build: $f — run run-selftest.sh first)"; exit 1; }
done

export TACHYON_UDP_SHIM=1
export LD_PRELOAD="$ROOT/selftest/udp-shim.so"

"$DOTNET" "$SERVER_DLL" --port "$PORT" --key-dir "$ROOT" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
B_PID=""
cleanup() { kill $SERVER_PID $B_PID 2>/dev/null; wait 2>/dev/null; }
trap cleanup EXIT

for _ in $(seq 1 50); do grep -q "LISTENING" "$SERVER_LOG" && break; sleep 0.2; done
grep -q "LISTENING" "$SERVER_LOG" || { echo "RELAYTEST: FAIL (server never listened)"; cat "$SERVER_LOG"; exit 1; }

echo "== client B (listener, account 424243) =="
"$DOTNET" "$CLIENT_DLL" --port "$PORT" --account 424243 --expect-relay --hold-ms 6000 >"$B_LOG" 2>&1 &
B_PID=$!
sleep 2  # B must be approved and on the roster before A sends its probe

echo "== client A (sender, account 424242) =="
"$DOTNET" "$CLIENT_DLL" --port "$PORT" --account 424242 >"$A_LOG" 2>&1
A_RC=$?

wait $B_PID; B_RC=$?
B_PID=""

echo "--- client A ---"; cat "$A_LOG"
echo "--- client B ---"; cat "$B_LOG"
echo "--- server ---"; cat "$SERVER_LOG"

FAIL=0
[ $A_RC -eq 0 ] || { echo "client A: FAIL"; FAIL=1; }
[ $B_RC -eq 0 ] && grep -q "RELAY_RECEIVED=PASS" "$B_LOG" || { echo "client B relay: FAIL"; FAIL=1; }
grep -q "relayed to 1 peer" "$SERVER_LOG" || { echo "server relay line: FAIL"; FAIL=1; }
grep -q "instance 'default': 424242" "$SERVER_LOG" || { echo "two-member roster: NOT SEEN"; FAIL=1; }

[ $FAIL -eq 0 ] && echo "RELAYTEST: PASS (two clients, one instance, voice packet relayed A -> server -> B)" || echo "RELAYTEST: FAIL"
exit $FAIL
