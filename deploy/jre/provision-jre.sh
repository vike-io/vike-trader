#!/usr/bin/env bash
# provision-jre.sh — THE portable-Java provisioner. Pinned Temurin release, per-(os/arch/image)
# SHA256 verified BEFORE extraction, idempotent, no package manager and no root.
#
#     ./provision-jre.sh                     # the RUNTIME JRE -> <project>/bin/jre
#     ./provision-jre.sh --image jdk         # the BUILD-TIME JDK -> <workspace>/vendor/tools
#     ./provision-jre.sh --dest DIR          # ...or somewhere named outright
#
# Progress goes to STDERR and the resolved `.../bin/java` path to STDOUT, so a caller can write
# `JAVA="$(provision-jre.sh --image jdk)"` and an operator still sees the download.
#
# # Why this is one script and not two
#
# It was two. `crates/bridges/dukascopy/scripts/provision-jforex.sh` owned this table for the
# sidecar's JVM, and `docs/ops/ibkr-cpapi-gateway.md` carried a hand `curl | tar` for the Client
# Portal Gateway's — SAME vendor, SAME release (17.0.19+10), SAME image, SAME Adoptium endpoint, and
# the second one verified NOTHING. Two provisioning paths for one artifact is the duplication the
# artifact-placement program removes, and when one of them is unverified the choice makes itself.
#
# ⚠ It lives under `deploy/` rather than beside its first caller because of WHERE it has to run.
# the CI box's project is `/var/lib/vike` and holds exactly `bin/ data/ settings/
# user_data/` — no `crates/`, no `Cargo.toml`, no `.git` (measured 2026-08-22). A provisioner under
# `crates/` is unreachable there, which is the same reason `vendor/` cannot hold a runtime artifact.
# This file is COPIED to the box as part of the tool directory it fills: `<project>/bin/jre/`.
#
# # THE SPLIT survives the move, and it is still structural
#
#   JRE — merely RUNS a jar / the CP gateway   -> RUNTIME    -> <project>/bin/jre/
#   JDK — javac, and only under a rebuild      -> BUILD-TIME -> <workspace>/vendor/tools/
#
# Discovery searches ONE root, the one belonging to the image being asked for, so a JRE from an
# earlier plain run is not on a JDK request's search path AT ALL. It was not always: both images
# shared `vendor/tools`, Temurin unpacks the JRE as `jdk-<v>-jre` and the JDK as `jdk-<v>`, so the
# JRE sorted LAST and `sort | tail -1` picked it — a `--rebuild` then handed gradle a javac-less
# runtime and the failure surfaced three steps later, at compile, naming neither.
# The javac check below stays as a SECOND line of defence: the roots are a few lines of shell apart
# and a future edit could point them at one directory again.
# `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` holds BOTH, plus the pin equality with CI and
# with the Windows `.ps1`.
set -uo pipefail

_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ -r "$_HERE/../vike-tool-root.sh" ] || {
  echo "no $_HERE/../vike-tool-root.sh — install it first: install -m755 deploy/vike-tool-root.sh <project>/bin/" >&2
  exit 2
}
# shellcheck source=../vike-tool-root.sh
. "$_HERE/../vike-tool-root.sh"
PROJECT="$(vike_project_dir)" || exit 2

IMAGE=jre
DEST=""
while [ $# -gt 0 ]; do
    case "$1" in
        --image) IMAGE="${2:-}"; shift 2 ;;
        --dest)  DEST="${2:-}";  shift 2 ;;
        -h|--help) sed -n '2,10p' "${BASH_SOURCE[0]}" >&2; exit 0 ;;
        *) echo "provision-jre.sh: unknown argument '$1'" >&2; exit 2 ;;
    esac
done
case "$IMAGE" in
    jre|jdk) ;;
    *) echo "provision-jre.sh: --image must be jre or jdk, not '$IMAGE'" >&2; exit 2 ;;
esac

# THE SPLIT, as two DISJOINT roots. Neither is a fallback for the other.
JRE_DIR="$PROJECT/bin/jre"        # runtime: the JVM that runs the sidecar jar and the CP gateway
TOOLS="$PROJECT/vendor/tools"     # build-time: the JDK whose javac compiles the sidecar
IMAGE_ROOT="$JRE_DIR"
[ "$IMAGE" = jdk ] && IMAGE_ROOT="$TOOLS"
[ -n "$DEST" ] && IMAGE_ROOT="$DEST"

case "$(uname -s)" in
    Linux)  OS=linux ;;
    Darwin) OS=mac ;;
    MINGW*|MSYS*|CYGWIN*)
        echo "Windows detected — use: powershell -ExecutionPolicy Bypass -File crates\\bridges\\dukascopy\\scripts\\provision-jforex.ps1" >&2
        exit 1 ;;
    *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
    x86_64|amd64)  ARCH=x64 ;;
    aarch64|arm64) ARCH=aarch64 ;;
    *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;;
esac

# Discovery searches ONE root — see THE SPLIT above. The javac filter is the second line of defence.
find_java() {
    find "$IMAGE_ROOT" -maxdepth 3 -path '*/bin/java' -type f 2>/dev/null | sort | while read -r j; do
        if [ "$IMAGE" = jdk ] && [ ! -x "${j%java}javac" ]; then
            continue
        fi
        printf '%s\n' "$j"
    done | tail -1
}

# PINNED Temurin release (2026-07 hardening: no more floating /latest/ piped straight into tar).
# Must match CI's setup-java pin (jforex-bridge.yml, 17.0.19) — javac output feeds the
# drift-gated jar. Checksums from https://api.adoptium.net/v3/assets/version/17.0.19%2B10.
JDK_VERSION="17.0.19+10"

JAVA="$(find_java || true)"
if [ -z "$JAVA" ]; then
    case "${OS}/${ARCH}/${IMAGE}" in
        linux/x64/jre)     SHA256=adb5a2364baa51de1ef91bb9911f5a61d24b045fe1d6647cb8050272a3a8ee75 ;;
        linux/x64/jdk)     SHA256=d8afc263758141a66e0e3aafc321e783f7016696f4eaea067d340a269037d331 ;;
        linux/aarch64/jre) SHA256=aae834297a87736869745be7c1fca3207ea9167c5824f41c88b0ebb2e3ccb9b1 ;;
        linux/aarch64/jdk) SHA256=83a52172678ec8975164648654869cb2e71d7c748b47aca94b29bbfa10c18e81 ;;
        mac/x64/jre)       SHA256=91bbd07b9c65d9ecbe1fa0081b3c1ad549ed34ed21085a72fdb76598a740b54c ;;
        mac/x64/jdk)       SHA256=03632d1fbf139ab3719a9f4b47dc206251449b87557143c822336dbf8c06560f ;;
        mac/aarch64/jre)   SHA256=cef790b404cf168fd1a8a7abc5054fbb442c7d4bfe390cceccfe3f64b9b776a9 ;;
        mac/aarch64/jdk)   SHA256=8fa1eff40bb637a33613b2ccb8b12c70dc3661cc22cf8e784943715769a05336 ;;
        *) echo "no pinned Temurin checksum for ${OS}/${ARCH}/${IMAGE} — add one from api.adoptium.net/v3/assets/version/${JDK_VERSION}" >&2; exit 1 ;;
    esac
    mkdir -p "$IMAGE_ROOT" || exit 1
    URL="https://api.adoptium.net/v3/binary/version/jdk-${JDK_VERSION/+/%2B}/${OS}/${ARCH}/${IMAGE}/hotspot/normal/eclipse?project=jdk"
    echo ">> downloading portable Temurin ${JDK_VERSION} ${IMAGE} (${OS}/${ARCH}, unzip-only, no install) -> ${IMAGE_ROOT}..." >&2
    TMP="$IMAGE_ROOT/temurin17.tar.gz"
    curl -sSfL "$URL" -o "$TMP" || exit 1
    # verify BEFORE extraction (sha256sum on Linux, shasum on macOS)
    ACTUAL="$( (sha256sum "$TMP" 2>/dev/null || shasum -a 256 "$TMP") | awk '{print $1}' )"
    if [ "$ACTUAL" != "$SHA256" ]; then
        rm -f "$TMP"
        echo "Temurin download failed SHA256 verification (expected $SHA256, got $ACTUAL) — refusing to extract" >&2
        exit 1
    fi
    tar -xzf "$TMP" -C "$IMAGE_ROOT" || exit 1
    rm -f "$TMP"
    JAVA="$(find_java)"
    [ -n "$JAVA" ] || { echo "extraction produced no usable ${IMAGE} under ${IMAGE_ROOT} (a jdk must have bin/javac beside bin/java)" >&2; exit 1; }
fi
echo ">> java: $JAVA" >&2
"$JAVA" -version 2>&1 | head -1 >&2
printf '%s\n' "$JAVA"
