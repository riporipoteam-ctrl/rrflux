#!/bin/sh
# Hostless/PaaS: filesystem is read-only except /tmp, /root, /app/.cache.
set -eu
APP_ROOT="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
cd "$APP_ROOT"
STATE="${TACHYON_KEY_DIR:-/root/.fluxrec-relay}"
mkdir -p "$STATE"
export TACHYON_KEY_DIR="$STATE"
export HOME="${HOME:-/root}"

[ -n "${TACHYON_SERVER_KEY_B64:-}" ] || { echo "missing TACHYON_SERVER_KEY_B64" >&2; exit 1; }
[ -n "${PLAYIT_SECRET_KEY:-}" ] || { echo "missing PLAYIT_SECRET_KEY" >&2; exit 1; }

echo "$TACHYON_SERVER_KEY_B64" | base64 -d > "$STATE/server-key.xml"
secret=$(printf '%s' "$PLAYIT_SECRET_KEY" | tr -d '\r\n')
mkdir -p /root/.config/playit_gg
printf 'secret_key = "%s"\n' "$secret" > /root/.config/playit_gg/playit.toml

export TACHYON_PRESENCE_BASE="${TACHYON_PRESENCE_BASE:-https://match.ripo-ripoteam.workers.dev}"

python3 "$APP_ROOT/health_server.py" &
./TachyonServer --key-dir "$STATE" &
exec /usr/local/bin/playit run