#!/bin/sh
set -eu
cd /relay

if [ -z "${TACHYON_SERVER_KEY_B64:-}" ]; then
  echo "TACHYON_SERVER_KEY_B64 secret missing" >&2
  exit 1
fi
echo "$TACHYON_SERVER_KEY_B64" | base64 -d > server-key.xml

if [ -z "${PLAYIT_SECRET_KEY:-}" ]; then
  echo "PLAYIT_SECRET_KEY secret missing" >&2
  exit 1
fi
printf '%s' "$PLAYIT_SECRET_KEY" | tr -d '\r\n' > playit-secret.txt
secret=$(cat playit-secret.txt)
printf 'secret_key = "%s"\n' "$secret" > playit.toml
mkdir -p /root/.config/playit_gg
cp playit.toml /root/.config/playit_gg/playit.toml

export TACHYON_PRESENCE_BASE="${TACHYON_PRESENCE_BASE:-https://match.ripo-ripoteam.workers.dev}"

python3 /relay/health_server.py &
health_pid=$!

./TachyonServer --log-path /relay/server.log &
tachyon_pid=$!

/usr/local/bin/playitd &
playit_pid=$!

shutdown() {
  kill "$health_pid" "$tachyon_pid" "$playit_pid" 2>/dev/null || true
}
trap shutdown INT TERM

wait "$playit_pid"