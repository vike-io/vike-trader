#!/usr/bin/env bash
# provision-jforex.sh — supply the Java runtime for the Dukascopy sidecar and smoke-test the jar
# on Linux / macOS (Windows: use scripts/provision-jforex.ps1).
#
# A fresh clone needs TWO things and this script supplies both: a Java 17+ runtime,
# and the sidecar jar itself.
#
#   * the RUNTIME — a portable Temurin 17 JRE (~45 MB) into bin/jre/. No package
#     manager, no root, no env vars; the Rust exec client resolves
#     <project>/bin/jre/*/bin/java at RUNTIME.
#   * the JAR — ⚠ NO LONGER COMMITTED. It is a release asset now, fetched by
#     scripts/fetch_release_tools.sh into bin/jforex/jforex-bridge.jar. It used to
#     be force-added past .gitignore, which made a 43 MB binary part of every
#     clone's history to serve the few checkouts that trade Dukascopy.
#
# ⚠ SO THIS SCRIPT NOW NEEDS NETWORK TO GITHUB, where before it needed only
# Adoptium. An offline clone gets no jar. Two escapes: JFOREX_BRIDGE_JAR names a
# jar OUTRIGHT and beats the project rung entirely (see the exec client's
# resolve_dukascopy_tools), or --rebuild BUILDS one from the committed Java
# sources — which needs a JDK and Dukascopy's maven repo, i.e. strictly more
# network, but no release.
#
# ⚠ THE JRE DOWNLOAD IS NOT HERE EITHER, and for a different reason. The pinned
# Temurin release, its per-image SHA256 table, the verify-before-extract and THE
# SPLIT between the runtime JRE and the build-time JDK all live in
# `deploy/jre/provision-jre.sh`, which this script CALLS. They moved because the
# same pin was needed by a second consumer — the IBKR Client Portal Gateway, whose
# runbook carried an unverified `curl | tar` for the identical Temurin build — and
# because a provisioner under `crates/` cannot run on an INSTALLED project, which
# has no source tree at all. One downloader, one pin, one `<project>/bin/jre`.
#
# So neither artifact this script installs is produced BY it any more: the jar
# comes from a release, the JRE from the shared provisioner. What is left here is
# the Dukascopy-specific part — which jar, which smoke test, and --rebuild.
#
# Idempotent: an existing image or jar is reused.
#
#   ./scripts/provision-jforex.sh            # supply runtime + jar, smoke-test it
#   ./scripts/provision-jforex.sh --rebuild  # build the jar from source instead of
#                                            # fetching it (asks the shared
#                                            # provisioner for a full JDK, into a
#                                            # DIFFERENT directory — see THE SPLIT;
#                                            # needs network to Dukascopy's maven repo)
set -euo pipefail

# Script lives at <workspace>/crates/bridges/dukascopy/scripts/ — 4 up to the root.
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"
BRIDGE="$(cd "$(dirname "$0")/../jforex-bridge" && pwd)" # sibling — rename-proof
PROVISION_JRE="$ROOT/deploy/jre/provision-jre.sh"
JAR="$ROOT/bin/jforex/jforex-bridge.jar"

[ -r "$PROVISION_JRE" ] || {
    echo "cannot read $PROVISION_JRE — it owns the Temurin pin this script used to carry" >&2
    exit 1
}

# --rebuild needs javac (full JDK); the normal path only needs a JRE to run the jar. Each image
# resolves from — and extracts into — its OWN root, per THE SPLIT in the provisioner.
IMAGE=jre
if [ "${1:-}" = "--rebuild" ]; then IMAGE=jdk; fi

JAVA="$(bash "$PROVISION_JRE" --image "$IMAGE")"
[ -n "$JAVA" ] || { echo "provision-jre.sh produced no java path" >&2; exit 1; }

# --- jar: fetched from the release; built from source only on request ---
if [ "${1:-}" = "--rebuild" ]; then
    echo ">> rebuilding sidecar from the committed Java sources (gradlew shadowJar)..."
    JAVA_HOME="$(dirname "$(dirname "$JAVA")")" \
        "$BRIDGE/gradlew" --no-daemon -q -p "$BRIDGE" shadowJar
    mkdir -p "$(dirname "$JAR")"
    cp "$BRIDGE/build/libs/jforex-bridge-all.jar" "$JAR"
    # ⚠ NOT "remember to commit it" any more — the jar is gitignored, and what a release ships is
    # built by .github/workflows/release.yml from these same sources. A local rebuild is for
    # TESTING an edit; the way to publish one is to merge the Java change and cut a release.
    echo ">> rebuilt: $JAR (local only — the shipped jar is built by release.yml from these sources)"
elif [ ! -f "$JAR" ]; then
    # The ordinary fresh-clone path. This used to be a hard error saying the jar was committed and
    # the checkout must be bad; it is a download now.
    echo ">> sidecar jar absent — fetching it from the latest release..."
    bash "$ROOT/scripts/fetch_release_tools.sh" jforex
else
    echo ">> jar already present: $JAR   (use --rebuild to build one from source)"
fi

# --- protocol smoke: with no DUKASCOPY_* env the sidecar must emit one fatal envelope ---
PROBE="$("$JAVA" -jar "$JAR" </dev/null 2>/dev/null | head -1 || true)"
if printf '%s' "$PROBE" | grep -q '"kind":"fatal"'; then
    echo ">> smoke OK — sidecar launches and speaks the protocol"
else
    echo "WARNING: unexpected sidecar probe output: $PROBE" >&2
fi

echo
echo "DONE. Nothing installed; the exec client resolves <project>/bin/jre at runtime."
