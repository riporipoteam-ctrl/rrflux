#!/bin/sh
# Hostless/PaaS: app dir is read-only; use /tmp (+ /app/.cache) for all writes.
set -eu
APP_ROOT="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"

log() {
  printf '[fluxrec] %s\n' "$*"
}

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

log "boot (PORT=${PORT:-?})"
mkdir -p "$STATE" "$PLAYIT_DATA" "$CERT_DIR" "$DOTNET_EXTRACT" "${XDG_CACHE_HOME:-/app/.cache}"

[ -n "${TACHYON_SERVER_KEY_B64:-}" ] || { log "FATAL: missing TACHYON_SERVER_KEY_B64"; exit 1; }
[ -n "${PLAYIT_SECRET_KEY:-}" ] || { log "FATAL: missing PLAYIT_SECRET_KEY"; exit 1; }

echo "$TACHYON_SERVER_KEY_B64" | base64 -d > "$STATE/server-key.xml"
secret=$(printf '%s' "$PLAYIT_SECRET_KEY" | tr -d '\r\n' ' ')
[ -n "$secret" ] || { log "FATAL: PLAYIT_SECRET_KEY is empty"; exit 1; }

export TACHYON_PRESENCE_BASE="${TACHYON_PRESENCE_BASE:-https://match.ripo-ripoteam.workers.dev}"

PLAYITD=$(command -v playitd 2>/dev/null || true)
[ -n "$PLAYITD" ] || { log "FATAL: playitd not in PATH"; exit 1; }
log "playitd=$PLAYITD"

log "starting HTTP health on 0.0.0.0:${PORT:-8000}"
python3 "$APP_ROOT/health_server.py" &

log "starting Tachyon (key-dir=$STATE)"
"$APP_ROOT/TachyonServer" --key-dir "$STATE" 2>&1 &

log "starting playit agent"
exec "$PLAYITD" --secret="$secret" --platform-docker --cert-dir="$CERT_DIR" 2>&1