#!/bin/sh
# Bring up a virtual display, export it, and run the observe-only GUI against a remote node.
#
# Four processes, and the order is load-bearing: Xvfb must be answering before the GUI starts or
# wgpu finds no display and exits; x11vnc must attach to a display that exists; websockify serves
# the VNC stream to a browser and needs x11vnc listening first.
set -eu

# The node is the container's ARGUMENT (what follows the image name in `docker run`): which node one
# viewer watches is how that process is started, and no setting is read from the environment
# (decision 0111 retired the variable this used to be).
NODE="${1:-}"
[ -n "$NODE" ] || {
    echo "vike-thin: pass the tradehub's observe address as the container's argument, e.g." >&2
    echo "vike-thin:   docker run ... vike-thin:<version> host.docker.internal:7879" >&2
    exit 2
}
# The daemon authenticates the connection; with no key the client signs with an empty one and the
# node answers `bad mac` forever. Refusing here names the cause instead of leaving a reconnect loop.
#
# ⚠ ONE HOME: the settings database is the only place a hub key is read from — the owner's ruling
# of 2026-10-31 (every key lives only as a row of `<project>/settings/db/vike.db`; no environment
# variable, no key file, no value on argv). `vike-desktop` resolves the observe key from the
# `hub_key` table under `VIKE_SETTINGS_DIR` and from nowhere else, so the container is handed a
# MOUNTED project folder holding that row. The retired `-e VIKE_TRADEHUB_OBSERVE_KEY` is refused
# by the GUI itself (`vike_config::REMOVED_ENV`), by name.
#
# This shell cannot read a row, and it must not pretend to: with a database present the GUI
# performs its own resolution seconds later, so the honest act is to start
# (`docs/decisions/0013-degrade-vs-refuse.md`).
_settings="${VIKE_SETTINGS_DIR:-}"
_db="$_settings/db/vike.db"
if [ -n "$_settings" ] && [ -f "$_db" ]; then
    echo "vike-thin: settings database $_db answers for this project, so the hub keys are ROWS" >&2
    echo "vike-thin:   this shell cannot read. vike-desktop resolves them itself; starting. If the" >&2
    echo "vike-thin:   pair is not in there, the node will answer 'bad mac' — check with:" >&2
    echo "vike-thin:   vike-cli backend status" >&2
else
    echo "vike-thin: no settings database, so no observe key. The node authenticates the OBSERVE" >&2
    echo "vike-thin: scope and will answer 'bad mac' to a client without one. The key is a ROW" >&2
    echo "vike-thin: (VIKE_TRADEHUB_OBSERVE_KEY in the hub_key table) of a project folder, mounted:" >&2
    echo "vike-thin:   -v <project>:/project -e VIKE_SETTINGS_DIR=/project/settings" >&2
    echo "vike-thin:      (the settings database vike-desktop reads the hub key from, db/vike.db)" >&2
    echo "vike-thin: It must match the daemon's own key byte for byte." >&2
    exit 2
fi

# A password variable that looked set would leave the GUI OPEN while its operator believes it
# locked, so it is refused, not ignored (the VNC block below says why the password route is gone).
if [ -n "${VIKE_THIN_VNC_PASSWORD:-}" ]; then
    echo "vike-thin: VIKE_THIN_VNC_PASSWORD is NO LONGER READ — there is no VNC password any more," >&2
    echo "vike-thin:   so the GUI would start OPEN while you believe it locked. Remove the variable" >&2
    echo "vike-thin:   and publish to loopback only: -p 127.0.0.1:6080:6080, then reach port 6080" >&2
    echo "vike-thin:   through an ssh tunnel to the Docker host." >&2
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
# What closes it, and neither refuses to start by itself — a viewer that will not run is worse than
# one that warns, and `docs/decisions/0013-degrade-vs-refuse.md` is the standing call:
#
#   1. `-localhost` binds the RFB port inside the container only, so the sole route in is
#      websockify's, and the only port worth publishing is 6080.
#   2. PUBLISH 6080 TO LOOPBACK ONLY (`-p 127.0.0.1:6080:6080`) and reach it through an ssh tunnel,
#      so only a person who can already log in to the Docker host can drive the GUI. The warning
#      below says so on every start. Same shape as the daemon's own non-loopback warning.
#
# ⚠ THERE IS NO VNC PASSWORD, ON PURPOSE. The password route this file used to carry ran
# `x11vnc -storepasswd "$PASSWORD" <file>`: `-storepasswd` takes the password as an ARGUMENT, so it
# sat on argv where `ps` shows it to every process in the container, and it landed in a FILE — the
# comment on that code claimed the variable "never reaches the command line", which was false. No
# password, key or credential may travel on argv, in a file or in the environment (the owner's
# ruling of 2026-10-31; `docs/decisions/0036-credentials-are-read-only-from-the-cli-and-the-mcp-surface.md`),
# so the route is gone rather than patched. A stale `-e VIKE_THIN_VNC_PASSWORD` is refused at the
# top of this file, before anything starts.
echo "vike-thin: WARNING — this VNC has no password. Anyone who can reach port 6080 can SEE AND" >&2
echo "vike-thin:   CONTROL this GUI — VNC carries mouse and keyboard, and this GUI is" >&2
echo "vike-thin:   authenticated to a trading node. Publish to loopback only" >&2
echo "vike-thin:   (-p 127.0.0.1:6080:6080) and reach it through an ssh tunnel." >&2

echo "vike-thin: exporting $DISP over VNC :5900 (container-local) and noVNC :6080"
x11vnc -display "$DISP" -forever -shared -localhost -quiet -rfbport 5900 -nopw &
websockify --web /usr/share/novnc 6080 localhost:5900 >/dev/null 2>&1 &

echo "vike-thin: connecting to node $NODE"
# `exec` so the GUI is this container's main process and takes SIGTERM directly — a wrapper in the
# way would swallow the stop and leave Docker to SIGKILL it.
exec /opt/vike/bin/vike-desktop --observe "$NODE"
