#!/bin/sh
# Hostless/PaaS: app dir is read-only; use /tmp (+ /app/.cache) for all writes.
set -eu
APP_ROOT="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"

BASE="/tmp/fluxrec"
STATE="${TACHYON_KEY_DIR:-$BASE/state}"
PLAYIT_DATA="${PLAYIT_DATA_DIR:-$BASE/playit}"
CERT_DIR="${PLAYIT_CERT_DIR:-$PLAYIT_DATA/certs}"
DOTNET_EXTRACT="${DOTNET_BUNDLE_EXTRACT_BASE_DIR:-$BASE/dotnet-extract}"

export DOTNET_BUNDLE_EXTRACT_BASE_DIR="$DOTNET_EXTRACT"
export TMPDIR=/tmp
export HOME="${HOME:-/root}"
export XDG_CACHE_HOME="${XDG_CACHE_HOME:-/app/.cache}"
export TACHYON_KEY_DIR="$STATE"
export TACHYON_CONSOLE_ONLY="${TACHYON_CONSOLE_ONLY:-1}"

mkdir -p "$STATE" "$PLAYIT_DATA" "$CERT_DIR" "$DOTNET_EXTRACT" "${XDG_CACHE_HOME:-/app/.cache}"

[ -n "${TACHYON_SERVER_KEY_B64:-}" ] || { echo "missing TACHYON_SERVER_KEY_B64" >&2; exit 1; }
[ -n "${PLAYIT_SECRET_KEY:-}" ] || { echo "missing PLAYIT_SECRET_KEY" >&2; exit 1; }

echo "$TACHYON_SERVER_KEY_B64" | base64 -d > "$STATE/server-key.xml"
secret=$(printf '%s' "$PLAYIT_SECRET_KEY" | tr -d '\r\n' ' ')
[ -n "$secret" ] || { echo "PLAYIT_SECRET_KEY is empty" >&2; exit 1; }

export TACHYON_PRESENCE_BASE="${TACHYON_PRESENCE_BASE:-https://match.ripo-ripoteam.workers.dev}"

python3 "$APP_ROOT/health_server.py" &
"$APP_ROOT/TachyonServer" --key-dir "$STATE" &

# playit agent (foreground): official Docker image uses playitd, not `playit run`.
exec /usr/local/bin/playitd --secret="$secret" --platform-docker --cert-dir="$CERT_DIR"