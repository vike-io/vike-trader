#!/usr/bin/env bash
# provision-fcsdk.sh — stage & verify the ForexConnect SDK for the FXCM bridge on Linux x86_64.
#
# Unlike the Dukascopy JForex runtime (a public Temurin download), the ForexConnect SDK is
# PROPRIETARY and cannot be fetched from a public URL — so, exactly like the committed-jar /
# portable-JDK story, it must be provisioned OUT-OF-BAND (copied from the dev box), never via
# `git pull` (vendor/ is gitignored). This script therefore does NOT download; it VERIFIES the
# staged tree, checks its integrity against the committed pin, confirms the runtime floor, and
# prints how to make the loader find the sibling .so's. Idempotent — safe to re-run.
#
#   ./scripts/provision-fcsdk.sh            # verify the staged SDK + print run/build guidance
#
# Stage the SDK first (on the rig), from the dev box that already has it:
#   rsync -a <devbox>:C:/Projects/vike_trader_rust/vendor/fcsdk/linux/  <repo>/vendor/fcsdk/linux/
set -euo pipefail

# Script lives at <workspace>/crates/bridges/fxcm/scripts/ — 4 up to the repo root.
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"
SDK="$ROOT/vendor/fcsdk/linux"
LIB="$SDK/lib"
HERE="$(cd "$(dirname "$0")" && pwd)"
PIN="$(cd "$(dirname "$0")/.." && pwd)/FCSDK.linux.sha256"   # sibling of scripts/ — rename-proof
PACKAGER="$HERE/package-fcsdk-runtime.sh"                    # owns the copy set; --list prints it

case "$(uname -s)" in
    Linux) ;;
    MINGW*|MSYS*|CYGWIN*)
        echo "Windows detected — the Windows build uses vendor/fcsdk directly (no provisioning needed)." >&2
        exit 1 ;;
    Darwin)
        echo "macOS is deferred for FXCM (the .dylib set is staged but not wired). Build on Linux/Windows." >&2
        exit 1 ;;
    *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
    x86_64|amd64) ;;
    *) echo "unsupported arch: $(uname -m) — the staged ForexConnect .so's are x86_64 only." >&2; exit 1 ;;
esac

# --- 1) the SDK tree must be present (copied out-of-band) ---
if [ ! -f "$LIB/libForexConnect.so" ] || [ ! -f "$SDK/include/forexconnect/ForexConnect.h" ]; then
    echo "ERROR: ForexConnect Linux SDK not staged at $SDK" >&2
    echo "  vendor/ is gitignored — copy it from the dev box (it is NOT fetched by git):" >&2
    echo "    rsync -a <devbox>:.../vendor/fcsdk/linux/  $SDK/" >&2
    echo "  expected: $LIB/libForexConnect.so  and  $SDK/include/forexconnect/ForexConnect.h" >&2
    exit 1
fi
echo ">> SDK staged at $SDK"

# --- 2) integrity: verify the .so set against the committed pin ---
if [ -f "$PIN" ]; then
    fail=0
    while read -r want rel; do
        case "$want" in \#*|"") continue ;; esac       # skip comments / blank lines
        f="$SDK/${rel#fcsdk/linux/}"                    # fcsdk/linux/lib/foo.so -> $SDK/lib/foo.so
        if [ ! -f "$f" ]; then echo "   MISSING: $rel" >&2; fail=1; continue; fi
        got="$( (sha256sum "$f" 2>/dev/null || shasum -a 256 "$f") | awk '{print $1}')"
        if [ "$got" != "$want" ]; then echo "   MISMATCH: $rel (pinned $want, got $got)" >&2; fail=1; fi
    done < "$PIN"
    if [ "$fail" -ne 0 ]; then
        echo "ERROR: SDK integrity check failed against $PIN — SDK was swapped/corrupted; investigate." >&2
        exit 1
    fi
    echo ">> integrity OK — all pinned .so's match FCSDK.linux.sha256"
else
    echo "WARNING: no pin file at $PIN — skipping integrity check" >&2
fi

# How many runtime libraries the manifest actually names, DERIVED — never typed. The guidance below
# used to state a hardcoded twelve, having counted the unversioned `libX.so` names only; the manifest
# ALSO pins their SONAME-versioned twins, which is what a DT_NEEDED entry resolves to, so the figure
# was both wrong and the wrong SHAPE of claim. package-fcsdk-runtime.sh owns that set (it is exactly
# what the packaging step copies), so the number comes from asking IT, not from a second parser here.
N_LIB="$(bash "$PACKAGER" --pin "$PIN" --list 2>/dev/null | grep -c . || true)"
[ -n "$N_LIB" ] && [ "$N_LIB" != 0 ] || N_LIB="the pinned"

# --- 3) runtime floor: loader resolves every NEEDED lib, and libstdc++ is new enough ---
# The SDK's own DT_RPATH is unusable ("." + dead Jenkins paths); resolve via LD_LIBRARY_PATH here.
if command -v ldd >/dev/null 2>&1; then
    if LD_LIBRARY_PATH="$LIB" ldd "$LIB/libForexConnect.so" | grep -q "not found"; then
        echo "WARNING: some libraries are 'not found' below — the runtime floor is not met:" >&2
        LD_LIBRARY_PATH="$LIB" ldd "$LIB/libForexConnect.so" | grep "not found" >&2 || true
    else
        echo ">> ldd: all NEEDED libraries resolve (with LD_LIBRARY_PATH=$LIB)"
    fi
fi
# GLIBCXX_3.4.19 (GCC >= 4.8.1) is the measured floor; every modern rig clears it by a wide margin.
STDCPP="$( (gcc -print-file-name=libstdc++.so.6 2>/dev/null) || echo /usr/lib/x86_64-linux-gnu/libstdc++.so.6 )"
# ⚠ COUNT, never `grep -q`. This script runs under `set -o pipefail`, and `grep -q` exits the
# instant it matches — `strings` then dies of SIGPIPE and pipefail reports the whole pipeline as
# 141, so the `if` read FALSE *because the string was found*. Every Linux box therefore got the
# toolchain warning below, on a box that met the floor; measured on the CI box (GCC 13.3, whose
# libstdc++ exports up to GLIBCXX_3.4.33) and written up in
# `docs/ops/fxcm-forexconnect.md`. `grep -c` reads to EOF, so nothing is left writing into a
# closed pipe and the exit status describes the SEARCH rather than a signal; `|| true` then keeps a
# genuine zero-match (grep's exit 1) from tripping `set -e` inside the substitution, so the only
# thing deciding this branch is the number.
GLIBCXX_HITS="$( (strings "$STDCPP" 2>/dev/null || true) | grep -c 'GLIBCXX_3\.4\.19' || true)"
if [ -f "$STDCPP" ] && [ "${GLIBCXX_HITS:-0}" -gt 0 ]; then
    echo ">> libstdc++ exports GLIBCXX_3.4.19 ($STDCPP) — toolchain floor met"
else
    echo "WARNING: could not confirm GLIBCXX_3.4.19 in $STDCPP — verify libstdc++ >= GCC 4.8.1" >&2
fi

# --- 4) how to run: make the loader find the sibling .so's ---
cat <<EOF

DONE. SDK verified. To BUILD the bridge (needs g++ with the old-string ABI, handled by build.rs):
    cargo build -p vike-fxcm --features fxcm      # FCSDK_DIR defaults to vendor/fcsdk/linux

⚠ To RUN anything built FROM THIS CHECKOUT — the test binaries, the live smokes — you MUST export
this. It is not belt-and-braces any more: the build bakes exactly ONE rpath entry, \$ORIGIN/../lib,
which from target/<profile>/deps points at a directory nothing fills. Nothing baked names this tree.
    export LD_LIBRARY_PATH="$LIB\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"

(An absolute rpath into this directory USED to be baked, so this was optional. It was removed because
a downstream consumer inherited it and shipped it: the published vike-tradehub-fxcm resolved through
the release runner's own SDK tree instead of its install root, and would have stopped working the
first time that tree was cleaned. One directory that nothing guarantees is not a convenience worth a
production outage — docs/ops/fxcm-forexconnect.md records the measurement.)

To DEPLOY it elsewhere, populate the \$ORIGIN/../lib half of that rpath — the packaging step copies
the pinned set (versioned twins included) next to the installed binary, re-verifying each hash:
    ./scripts/package-fcsdk-runtime.sh /var/lib/vike   # binary in bin/, libs in lib/

In a CONTAINER with the SDK baked into the image, do neither: put the libraries wherever the
Dockerfile likes and tell the loader once, in the image, e.g.
    ENV LD_LIBRARY_PATH=/opt/fcsdk/lib
    # …or, equivalently and without an env var:
    RUN echo /opt/fcsdk/lib > /etc/ld.so.conf.d/fcsdk.conf && ldconfig
Both work because the binary's only baked entry is \$ORIGIN/../lib, and a non-existent rpath
directory is skipped rather than fatal. This is also why the entry must never be absolute: DT_RPATH
outranks LD_LIBRARY_PATH, so a baked path that happened to exist would beat the image's own setting.

That spelling is the RIGHT one here: you just proved the SDK is staged, so a missing library must be
a hard failure. The INSTALL-path spelling is the guarded one — \`just fxcm-package <project-root>\`,
which adds --if-staged so the same step can sit in a layout runbook every box follows and no-op on
the ones with no SDK. docs/ops/tradehub-the CI box.md's project-folder layout step calls it.

Credentials are unchanged — FXCM_DEMO_USER/... in the workspace .env (provisioning is SDK-only).
EOF
