#!/bin/sh
set -eu

log() { printf '%s  entrypoint: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*" >&2; }
die() { log "$*"; exit 1; }

STATE_DIR="${TS_STATE_DIR:-/data/tailscale}"
DATA_DIR="${SYNCPARTY_DATA_DIR:-/data}"
SOCKET="/var/run/tailscale/tailscaled.sock"

if [ "${1:-}" = "healthcheck" ]; then
    address="$(tailscale ip -4 2>/dev/null | head -n 1)" || true
    [ -n "${address}" ] || die "not on the tailnet"

    SYNCPARTY_HEALTH_ADDRESS="${address}" \
    SYNCPARTY_HEALTH_PORT="${SYNCPARTY_PORT:-8999}" \
    /opt/syncplay-venv/bin/python -c '
import os, socket, sys
address = os.environ["SYNCPARTY_HEALTH_ADDRESS"]
port = int(os.environ["SYNCPARTY_HEALTH_PORT"])
try:
    socket.create_connection((address, port), timeout=3).close()
except OSError as error:
    sys.exit(f"nothing listening on {address}:{port} ({error})")
'
    exit 0
fi

mkdir -p "${STATE_DIR}" "${DATA_DIR}"

# stat, not mountpoint: debian slim does not ship the latter.
if [ "$(stat -c %d "${DATA_DIR}" 2>/dev/null || echo 0)" = "$(stat -c %d / 2>/dev/null || echo 1)" ]; then
    log "WARNING: ${DATA_DIR} is not a mounted volume — the server password, salt and this node's Tailscale identity will be lost when the container is recreated"
fi

# The Syncplay server binds to the tailnet address itself, so userspace
# networking is not an option here.
[ -c /dev/net/tun ] || die "no /dev/net/tun. Run with --device=/dev/net/tun --cap-add=NET_ADMIN"

mkdir -p "$(dirname "${SOCKET}")"

log "starting tailscaled"
tailscaled \
    --state="${STATE_DIR}/tailscaled.state" \
    --socket="${SOCKET}" \
    --statedir="${STATE_DIR}" \
    --tun="${TS_TUN:-tailscale0}" \
    --port="${TS_PORT:-41641}" &

# status --json, not plain status: the latter exits non-zero while logged out,
# which is the state every first run is in.
waited=0
until tailscale --socket="${SOCKET}" status --json >/dev/null 2>&1 || [ "${waited}" -ge 30 ]; do
    sleep 1
    waited=$((waited + 1))
done
[ "${waited}" -lt 30 ] || die "tailscaled did not come up within 30s"

# accept-dns=false: tailscaled rewriting resolv.conf breaks the rest of the
# container, and only guests resolve MagicDNS names.
# timeout: without it `up` blocks forever when there is no auth key, long
# after it has printed the sign-in URL.
# A variable, not "$@" — that holds the command to exec below.
up_flags="--hostname=${TS_HOSTNAME:-syncparty}"
up_flags="${up_flags} --accept-dns=false --accept-routes=false"
up_flags="${up_flags} --timeout=${TS_UP_TIMEOUT:-30s}"

if [ -n "${TS_AUTHKEY:-}" ]; then
    log "authenticating with the supplied auth key"
    # shellcheck disable=SC2086
    tailscale --socket="${SOCKET}" up ${up_flags} --authkey="${TS_AUTHKEY}" ${TS_EXTRA_ARGS:-} \
        || log "tailscale up did not complete — check that the auth key is valid and unexpired"
else
    log "no TS_AUTHKEY set — a sign-in URL will be printed below"
    # shellcheck disable=SC2086
    tailscale --socket="${SOCKET}" up ${up_flags} ${TS_EXTRA_ARGS:-} \
        || log "not signed in yet; syncpartyd will keep waiting for it"
fi

address="$(tailscale --socket="${SOCKET}" ip -4 2>/dev/null | head -n 1)" || true
if [ -n "${address}" ]; then
    log "tailnet address: ${address}"
fi

# exec so syncpartyd is PID 1 and gets the SIGTERM from docker stop.
exec "$@"
