#!/bin/sh
set -eu
cd /relay
[ -n "${TACHYON_SERVER_KEY_B64:-}" ] || { echo "missing TACHYON_SERVER_KEY_B64"; exit 1; }
[ -n "${PLAYIT_SECRET_KEY:-}" ] || { echo "missing PLAYIT_SECRET_KEY"; exit 1; }
echo "$TACHYON_SERVER_KEY_B64" | base64 -d > server-key.xml
printf '%s' "$PLAYIT_SECRET_KEY" | tr -d '\r\n' > playit-secret.txt
secret=$(cat playit-secret.txt)
printf 'secret_key = "%s"\n' "$secret" > playit.toml
mkdir -p /root/.config/playit_gg && cp playit.toml /root/.config/playit_gg/playit.toml
export TACHYON_PRESENCE_BASE="${TACHYON_PRESENCE_BASE:-https://match.ripo-ripoteam.workers.dev}"
python3 health_server.py &
./TachyonServer --log-path /relay/server.log &
exec /usr/local/bin/playit run