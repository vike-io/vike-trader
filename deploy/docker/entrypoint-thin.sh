#!/bin/sh
# Bring up a virtual display, export it, and run the observe-only GUI against a remote node.
#
# Four processes, and the order is load-bearing: Xvfb must be answering before the GUI starts or
# wgpu finds no display and exits; x11vnc must attach to a display that exists; websockify serves
# the VNC stream to a browser and needs x11vnc listening first.
set -eu

NODE="${VIKE_TRADEHUB_ADDR:-}"
[ -n "$NODE" ] || {
    echo "vike-thin: set VIKE_TRADEHUB_ADDR to the tradehub node's observe address, e.g." >&2
    echo "vike-thin:   -e VIKE_TRADEHUB_ADDR=host.docker.internal:7879" >&2
    exit 2
}
# The daemon authenticates the connection; with no key the client signs with an empty one and the
# node answers `bad mac` forever. Refusing here names the cause instead of leaving a reconnect loop.
#
# ⚠ TWO sources, and the first draft of this guard accepted only the environment — which is the one
# `vike-app` does NOT read. It resolves the key from the CREDENTIALS MAP, i.e.
# `<project>/settings/secrets.env` under `VIKE_SETTINGS_DIR`; an environment variable reaches it
# only where the process resolves a store containing it. So the store is the primary path and the
# env var is the convenience, and a guard that demanded the env var rejected the correct setup.
_store="${VIKE_SETTINGS_DIR:-}/secrets.env"
if [ -n "${VIKE_TRADEHUB_OBSERVE_KEY:-}" ]; then
    echo "vike-thin: observe key from the environment"
elif [ -n "${VIKE_SETTINGS_DIR:-}" ] && grep -qs '^VIKE_TRADEHUB_OBSERVE_KEY=' "$_store"; then
    echo "vike-thin: observe key from $_store"
else
    echo "vike-thin: no observe key. The node authenticates the OBSERVE scope and will answer" >&2
    echo "vike-thin: 'bad mac' to a client without one. Provide EITHER:" >&2
    echo "vike-thin:   -v <project>:/project -e VIKE_SETTINGS_DIR=/project/settings   (the store" >&2
    echo "vike-thin:      vike-app actually reads — it must hold VIKE_TRADEHUB_OBSERVE_KEY)" >&2
    echo "vike-thin:   -e VIKE_TRADEHUB_OBSERVE_KEY=<key>" >&2
    echo "vike-thin: Either way it must match the daemon's own key byte for byte." >&2
    exit 2
fi

GEOM="${VIKE_THIN_GEOMETRY:-1600x900x24}"
DISP="${DISPLAY:-:99}"

echo "vike-thin: virtual display $DISP at $GEOM (lavapipe software rendering)"
Xvfb "$DISP" -screen 0 "$GEOM" -nolisten tcp &
XVFB_PID=$!

# Wait for the display rather than sleeping a guessed interval: a slow box would otherwise start the
# GUI against a server that is not answering yet, which fails as "no display" and looks like a
# missing package.
#
# ⚠ The probe is the SOCKET, not `xdpyinfo`. The first version of this script used `xdpyinfo` and
# every run died at "Xvfb did not come up" — because that binary lives in `x11-utils`, which this
# image does not install. The readiness check was itself the missing dependency, and it reported the
# absence as a failure of the thing it was checking. A socket test needs nothing but the shell.
SOCK="/tmp/.X11-unix/X${DISP#:}"
i=0
while [ "$i" -lt 100 ]; do
    if [ -S "$SOCK" ] && ! kill -0 "$XVFB_PID" 2>/dev/null; then
        echo "vike-thin: Xvfb exited while starting on $DISP" >&2; exit 1
    fi
    [ -S "$SOCK" ] && break
    i=$((i + 1)); sleep 0.1
done
[ -S "$SOCK" ] || { echo "vike-thin: Xvfb did not create $SOCK within 10s" >&2; exit 1; }

# ─── The VNC export, and the exposure it is ───────────────────────────────────────────────────
#
# ⚠ WHOEVER REACHES 6080 DRIVES THIS GUI. VNC here is not a viewer: it carries mouse and keyboard,
# so a browser tab on that port is a hand on the application — the same application that is
# authenticated to a trading node. That was true from the first version of this file and nothing
# said so; `-nopw` sat in the command line and the docs said `-p 6080:6080`, which Docker binds on
# EVERY interface. Measured 2026-09-03 on a dev box: the GUI was reachable and controllable from
# the whole LAN, by anyone, with no password.
#
# Two changes, and neither refuses to start — a viewer that will not run is worse than one that
# warns, and `docs/decisions/0013-degrade-vs-refuse.md` is the standing call:
#
#   1. `-localhost` binds the RFB port inside the container only, so the sole route in is
#      websockify's, and the only port worth publishing is 6080.
#   2. A password when `VIKE_THIN_VNC_PASSWORD` is set; without one, a WARNING that names the
#      exposure and the two ways to close it. Same shape as the daemon's own non-loopback warning.
readonly VNC_PW_FILE=/tmp/.vike-thin-vncpw
if [ -n "${VIKE_THIN_VNC_PASSWORD:-}" ]; then
    # `-storepasswd` writes the obfuscated file x11vnc's `-rfbauth` reads. The variable never
    # reaches the command line, where `ps` would show it to every process in the container.
    x11vnc -storepasswd "$VIKE_THIN_VNC_PASSWORD" "$VNC_PW_FILE" >/dev/null 2>&1 ||
        { echo "vike-thin: could not store the VNC password" >&2; exit 1; }
    chmod 600 "$VNC_PW_FILE"
    AUTH="-rfbauth $VNC_PW_FILE"
    echo "vike-thin: VNC password set from VIKE_THIN_VNC_PASSWORD"
else
    AUTH="-nopw"
    echo "vike-thin: WARNING — no VNC password (VIKE_THIN_VNC_PASSWORD is unset). Anyone who can" >&2
    echo "vike-thin:   reach port 6080 can SEE AND CONTROL this GUI — VNC carries mouse and" >&2
    echo "vike-thin:   keyboard, and this GUI is authenticated to a trading node. Close it by" >&2
    echo "vike-thin:   publishing to loopback only (-p 127.0.0.1:6080:6080) or by setting" >&2
    echo "vike-thin:   VIKE_THIN_VNC_PASSWORD." >&2
fi

echo "vike-thin: exporting $DISP over VNC :5900 (container-local) and noVNC :6080"
# shellcheck disable=SC2086  # AUTH is a deliberate two-word flag pair
x11vnc -display "$DISP" -forever -shared -localhost -quiet -rfbport 5900 $AUTH &
websockify --web /usr/share/novnc 6080 localhost:5900 >/dev/null 2>&1 &

echo "vike-thin: connecting to node $NODE"
# `exec` so the GUI is this container's main process and takes SIGTERM directly — a wrapper in the
# way would swallow the stop and leave Docker to SIGKILL it.
exec /opt/vike/bin/vike-app --observe "$NODE"
