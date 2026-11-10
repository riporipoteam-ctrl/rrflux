#!/usr/bin/env bash
# Tachyon milestone-1 self-test.
# Builds server + test client, boots the server on a scratch port, runs the
# full client handshake (LiteNetLib through the encrypt layer -> NGO
# ConnectionRequest with a real AuthPayload -> Carrot approval), and verifies
# that BOTH sides independently derive the same session key.
#
# SANDBOX NOTE: this VM's netstack EPERMs sendto() on unconnected UDP
# sockets (loopback included), so stock LiteNetLib cannot emit a datagram
# here unaided. udp-shim.so (source alongside this script) is LD_PRELOADed
# into both test processes to reroute sends via connect()+send(); wire
# bytes are unchanged and the server binary itself is stock. On any normal
# host (or Armin's future UDP host) no shim is needed.
set -u
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
DOTNET=/usr/bin/dotnet
PORT="${TACHYON_TEST_PORT:-17777}"
VERSIONS="${TACHYON_TEST_VERSIONS:-7}"
FRAMING="${TACHYON_TEST_FRAMING:-framed}"
KEY_DIR="$ROOT"
SERVER_LOG="$ROOT/selftest/server-v$VERSIONS-$FRAMING.log"
CLIENT_LOG="$ROOT/selftest/client-v$VERSIONS-$FRAMING.log"
CLIENT_FRAMING_ARG=""
[ "$FRAMING" = "legacy" ] && CLIENT_FRAMING_ARG="--legacy-framing"
echo "== scenario: $VERSIONS MessageVersions entries, $FRAMING framing =="

echo "== building (Release) =="
$DOTNET build -c Release src/TachyonServer/TachyonServer.csproj -v q --nologo 2>&1 | grep -E "error|Build succeeded" || true
$DOTNET build -c Release selftest/TachyonTestClient/TachyonTestClient.csproj -v q --nologo 2>&1 | grep -E "error|Build succeeded" || true

SERVER_DLL="$ROOT/src/TachyonServer/bin/Release/net8.0/TachyonServer.dll"
CLIENT_DLL="$ROOT/selftest/TachyonTestClient/bin/Release/net8.0/TachyonTestClient.dll"
[ -f "$SERVER_DLL" ] || { echo "SELFTEST: FAIL (server build missing)"; exit 1; }
[ -f "$CLIENT_DLL" ] || { echo "SELFTEST: FAIL (client build missing)"; exit 1; }

SHIM_SO="$ROOT/selftest/udp-shim.so"
if [ ! -f "$SHIM_SO" ] || [ "$ROOT/selftest/udp-shim.c" -nt "$SHIM_SO" ]; then
  echo "== building udp sandbox shim =="
  gcc -shared -fPIC -O2 -o "$SHIM_SO" "$ROOT/selftest/udp-shim.c" -ldl || { echo "SELFTEST: FAIL (shim build)"; exit 1; }
fi
export TACHYON_UDP_SHIM=1
export LD_PRELOAD="$SHIM_SO"

echo "== starting server on udp/$PORT (key dir: $KEY_DIR) =="
"$DOTNET" "$SERVER_DLL" --port "$PORT" --key-dir "$KEY_DIR" >"$SERVER_LOG" 2>&1 &
SERVER_PID=$!
trap 'kill $SERVER_PID 2>/dev/null; wait $SERVER_PID 2>/dev/null' EXIT

for _ in $(seq 1 50); do
  grep -q "LISTENING" "$SERVER_LOG" 2>/dev/null && break
  kill -0 $SERVER_PID 2>/dev/null || { echo "SELFTEST: FAIL (server died)"; cat "$SERVER_LOG"; exit 1; }
  sleep 0.2
done
grep -q "LISTENING" "$SERVER_LOG" || { echo "SELFTEST: FAIL (server never listened)"; cat "$SERVER_LOG"; exit 1; }

echo "== running test client =="
"$DOTNET" "$CLIENT_DLL" --port "$PORT" --key-xml "$KEY_DIR/voice-key-public.xml" --versions "$VERSIONS" $CLIENT_FRAMING_ARG >"$CLIENT_LOG" 2>&1
CLIENT_RC=$?
cat "$CLIENT_LOG"

echo "== server log =="
sleep 0.5
cat "$SERVER_LOG"

SERVER_HASH=$(grep -o 'SESSION_KEY_SHA256=[a-f0-9]*' "$SERVER_LOG" | head -1 | cut -d= -f2)
CLIENT_HASH=$(grep -o 'SESSION_KEY_SHA256=[a-f0-9]*' "$CLIENT_LOG" | head -1 | cut -d= -f2)

echo "== verdict =="
FAIL=0
[ $CLIENT_RC -eq 0 ] && grep -q "SELFTEST_RESULT=PASS" "$CLIENT_LOG" || { echo "client handshake: FAIL"; FAIL=1; }
grep -q "APPROVED" "$SERVER_LOG" || { echo "server approval: FAIL (no APPROVED line)"; FAIL=1; }
grep -q "vbSelfTest=PASS" "$SERVER_LOG" || { echo "VB self-test: FAIL"; FAIL=1; }
if [ -n "$SERVER_HASH" ] && [ "$SERVER_HASH" = "$CLIENT_HASH" ]; then
  echo "session keys MATCH: sha256=$SERVER_HASH"
else
  echo "session keys MISMATCH: server=$SERVER_HASH client=$CLIENT_HASH"; FAIL=1
fi
grep -q "Dissonance packet from account" "$SERVER_LOG" || { echo "named-message relay path: NOT EXERCISED"; FAIL=1; }
# v4 assertions: request ground-truth dump present; framing mode matches the
# scenario; the post-approval probes were captured (TimeSync with hex,
# Dissonance HandshakeRequest consumed server-side); the RemoveClient
# broadcast fired when the test client disconnected.
grep -q "\[reqdump\] .* batchHash=" "$SERVER_LOG" || { echo "request ground-truth dump: MISSING"; FAIL=1; }
if [ "$FRAMING" = "framed" ]; then
  grep -q "framing=size-prefixed (MessageHeader)" "$SERVER_LOG" || { echo "framed-mode handshake: NOT LOGGED"; FAIL=1; }
else
  grep -q "framing=legacy (no size prefix)" "$SERVER_LOG" || { echo "legacy-mode handshake: NOT LOGGED"; FAIL=1; }
fi
grep -q "TimeSyncMessage from client" "$SERVER_LOG" || { echo "TimeSync capture: MISSING"; FAIL=1; }
grep -q "Dissonance HandshakeRequest from account" "$SERVER_LOG" || { echo "HandshakeRequest consumption: MISSING"; FAIL=1; }
# v5 assertions: the NamedMessage full capture fired; the stock handshake
# was answered (response + ClientState burst) and the client verified both.
grep -q "\[ngo\] NamedMessage from client" "$SERVER_LOG" || { echo "named-message full capture: MISSING"; FAIL=1; }
grep -q "sent Dissonance HandshakeResponse" "$SERVER_LOG" || { echo "HandshakeResponse send: MISSING"; FAIL=1; }
grep -q "HANDSHAKE_RESPONSE=PASS" "$CLIENT_LOG" || { echo "client HandshakeResponse check: FAIL"; FAIL=1; }
grep -q "HANDSHAKE_CLIENTSTATE=PASS" "$CLIENT_LOG" || { echo "client ClientState check: FAIL"; FAIL=1; }
# v6 assertions: startup self-identification (build version + exe sha256;
# the build string tracks the current ServerBuildVersion, v7 as of v7);
# stock-u64 dual-parse fallback logged and server stayed alive (handshake
# answer above already proves liveness after the probe).
grep -q "build=v8" "$SERVER_LOG" || { echo "startup build line (build=v8): MISSING"; FAIL=1; }
grep -q "approval body sent to client" "$SERVER_LOG" || { echo "v8 approval body hex log: MISSING"; FAIL=1; }
grep -q "APPROVAL_V8_LAYOUT=PASS" "$CLIENT_LOG" || { echo "client v8 approval layout check: FAIL"; FAIL=1; }
grep -q "exeSha256=" "$SERVER_LOG" || { echo "v6 startup exe hash: MISSING"; FAIL=1; }
grep -q "stock-u64 named header fallback, hash=0x1122334455667788" "$SERVER_LOG" || { echo "stock-u64 fallback: MISSING"; FAIL=1; }
grep -q "STOCK_U64_PROBE_SENT=PASS" "$CLIENT_LOG" || { echo "client stock-u64 probe: FAIL"; FAIL=1; }
grep -q "verbatim broadcast to 0 other" "$SERVER_LOG" || { echo "ClientState verbatim broadcast: MISSING"; FAIL=1; }
grep -q "CLIENTSTATE_PROBE_SENT=PASS" "$CLIENT_LOG" || { echo "client ClientState probe: FAIL"; FAIL=1; }
# v7 assertions: the room lane is RELAYED, not just logged — kind 3 -> 4
# proxied relay line with the probe's event/cache values and 0 recipients
# (single-client sandbox), kind 5 -> 6 room-data relay line, both probes
# sent by the client; and the stock TimeSync stream: server's first-send
# line present, client received a TimeSync that parses as one packed tick.
grep -q "proxied relay (kind 3->4)" "$SERVER_LOG" || { echo "proxied relay (kind 3->4): MISSING"; FAIL=1; }
grep -q "event=42 cacheIndex=777" "$SERVER_LOG" || { echo "proxied relay event/cache values: MISSING"; FAIL=1; }
grep -q "room-data relay (kind 5->6)" "$SERVER_LOG" || { echo "room-data relay (kind 5->6): MISSING"; FAIL=1; }
grep -q "sender 1 prepended -> 0 recipient" "$SERVER_LOG" || { echo "proxied relay recipient count: MISSING"; FAIL=1; }
grep -q "room-data relay (kind 5->6).*-> 0 recipient" "$SERVER_LOG" || { echo "room-data relay recipient count: MISSING"; FAIL=1; }
grep -q "ROOM_PROXIED_PROBE_SENT=PASS" "$CLIENT_LOG" || { echo "client proxied probe: FAIL"; FAIL=1; }
grep -q "ROOM_DATA_PROBE_SENT=PASS" "$CLIENT_LOG" || { echo "client room-data probe: FAIL"; FAIL=1; }
grep -q "TimeSync -> client" "$SERVER_LOG" || { echo "server TimeSync stream first send: MISSING"; FAIL=1; }
grep -q "TIMESYNC_REPLY=PASS" "$CLIENT_LOG" || { echo "client TimeSync reply check: FAIL"; FAIL=1; }
sleep 1
grep -q "broadcast Dissonance RemoveClient" "$SERVER_LOG" || { echo "RemoveClient broadcast on leave: MISSING"; FAIL=1; }

if [ $FAIL -eq 0 ]; then
  echo "SELFTEST: PASS (transport + layer + handshake + approval + session key + named-message intake + v5 named parse/capture + Dissonance handshake answer + v6 self-id/dual-parse/ClientState broadcast code path + v7 proxied/room-data relay + stock TimeSync stream + v8 approval layout (two extra scalars before Payload); $VERSIONS versions, $FRAMING framing)"
else
  echo "SELFTEST: FAIL"
fi
exit $FAIL
