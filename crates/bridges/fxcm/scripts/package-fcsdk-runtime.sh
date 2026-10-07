#!/usr/bin/env bash
# package-fcsdk-runtime.sh — copy the PINNED ForexConnect runtime libraries into a deployed tree,
# so the relocatable rpath the bridge is built with actually resolves against something.
#
#   ./scripts/package-fcsdk-runtime.sh /var/lib/vike   # writes <root>/lib/
#   ./scripts/package-fcsdk-runtime.sh --list                       # print the copy set, copy nothing
#   ./scripts/package-fcsdk-runtime.sh --sdk DIR --pin FILE DEST    # override either input
#   ./scripts/package-fcsdk-runtime.sh --if-staged DEST             # ...and no-op where no SDK is staged
#
# ## ⚠ WHAT `<root>/lib` IS FOR CHANGED ON 2026-09-09 — read this before the history below
#
# It used to hold the SDK's libraries so that an EXECUTABLE one directory up, at `<root>/bin/<exe>`,
# could reach them through its baked `$ORIGIN/../lib`. There is no such executable any more: no Rust
# binary links ForexConnect at all. The C++ boundary is `libfcshim.so`, built by
# `crates/bridges/fxcm/build.rs`, carrying its own `$ORIGIN` rpath, and opened at runtime by
# `crates/bridges/fxcm/src/loader.rs` — whose first search rung is `<root>/lib/libfcshim.so`.
#
# So this directory is now **the shim plus everything the shim needs**, and the shim is the ELEVENTH
# file in it rather than a binary somewhere else. `--shim <path>` installs it (see the block at the
# bottom); without one the script says so loudly, because "packaged the libraries" on a box that
# still cannot trade FXCM is a report that reads as success.
#
# Everything below about DERIVING the copy set, the SONAME twins, `--if-staged` and the pin is
# unchanged and still true.
#
# ## Why this exists
#
# `crates/bridges/fxcm/build.rs` bakes ONE rpath entry on Linux. ⚠ It used to bake a second,
# ABSOLUTE one into the vendor tree, so a dev/CI run resolved straight out of
# `vendor/fcsdk/linux/lib`; that entry came FIRST in search order, shipped in the release artifact,
# and made a published binary load from the release runner's own tree rather than from its install
# root. It is gone from every build in this tree, which makes THIS SCRIPT the only thing that makes
# an installed binary loadable (a checkout run uses `LD_LIBRARY_PATH`; a container image supplies the
# path itself). It also emits `FCSDK_LIB` / `FCSDK_BIN` as `cargo:rustc-env` values, described
# in its own comments as hooks for "a packaging step". Nothing in this tree read either variable and
# nothing ever performed the copy — so the relocatable half of the rpath pointed at a directory
# no step populated, and an fxcm-enabled binary could only ever run from the box it was linked on
# (or under a hand-set `LD_LIBRARY_PATH`). This script IS that missing step.
#
# ⚠ It used to say here that "the resolution mechanism is untouched: this only fills in the
# directory the loader was already told to search." That was FALSE, and it stayed unfalsified for as
# long as nothing tested the loader instead of the paths. The loader had NOT been told to search it:
# `build.rs` emitted a bare `-Wl,-rpath`, which a modern `ld` records as `DT_RUNPATH` — a tag
# `ld.so(8)` applies only to an object's own direct `DT_NEEDED` entries, never to THEIR dependencies.
# The binary's one direct NEEDED on the SDK is `libForexConnect.so`; the ~20 siblings this script
# copies are its dependencies, so they were searched for as if no rpath existed. Measured on the CI box
# with this script's own output correctly in place: 5 libraries "not found", exit 127 at run. The
# missing half was one link arg (`-Wl,--disable-new-dtags`, now in `build.rs` with the measurement
# beside it), and this script's copy was right all along — it was being read by nobody.
#
# The deployed shape it targets is the one `deploy/sbin/vike-trader-ci-deploy` installs into — a
# single self-contained project root holding `bin/`, `settings/`, `state/` — so `<install-root>` is
# that root, the binary lands in `<install-root>/bin/`, and `$ORIGIN/../lib` names `<install-root>/lib`.
#
# ## ⚠ THE COPY SET IS DERIVED FROM THE PIN, AND IT IS BIGGER THAN THE LIBRARY NAMES
#
# `crates/bridges/fxcm/FCSDK.linux.sha256` pins the unversioned `libX.so` names AND their
# SONAME-versioned twins (`libForexConnect.so.1.6.5`, `liblog4cplus.so.4`, `libquotesmgr2.so.2.8`,
# and the rest). Those are not decoration: the dynamic loader resolves a `DT_NEEDED` entry by
# SONAME, so a `lib/` directory holding only the unversioned names fails AT LOAD, after a build and
# a deploy have both gone green. Every count of this set that was ever written down by hand was
# wrong, so no count is written down here either — the set is READ from the manifest, and
# `crates/bridges/fxcm/tests/fcsdk_packaging.rs` proves the copy equals exactly the manifest's
# `lib/` rows (both directions, over a synthetic manifest that a hand-written list cannot satisfy).
#
# The SDK itself is proprietary and out-of-band (`crates/bridges/fxcm/scripts/provision-fcsdk.sh`
# stages and verifies it); `vendor/` is gitignored, so there is nothing to download here either.
#
# ## `--if-staged`: the flag that lets an INSTALL call this unconditionally
#
# For its first release this script was reachable only from `provision-fcsdk.sh`'s closing guidance —
# i.e. from the script you run BECAUSE you have the SDK. An install step cannot be spelled that way:
# it runs on every box, and almost no box has ForexConnect. `--if-staged` is that spelling. It makes
# exactly ONE case soft — a `--sdk` tree where NONE of the pinned libraries is present — reporting it
# on stdout and exiting 0 without creating so much as the `lib/` directory. Every other refusal is
# untouched, and that is the whole design:
#
#   * an SDK with SOME pinned rows staged and some missing is still a hard failure naming the file.
#     "Absent" and "half there" must never look alike — the short-set tree is precisely what this
#     script exists to keep away from a binary, and a guard that swallowed it would hand that tree
#     back through the door the guard opened. (Same distinction
#     `vike_bridge_core::credentials::try_load_workspace_secrets_at` draws between a credential store
#     that is absent and one that is present-and-unreadable.)
#   * a pin that names no runtime libraries is still a hard failure, guard or no guard. That is a
#     defect in THIS repository rather than a fact about the box, so no flag may soften it — which
#     is why the manifest is parsed BEFORE the guard is consulted, a few lines below.
#   * a staged library whose bytes do not match the pin is still refused.
#
# The probe is DERIVED from the manifest (how many pinned rows are staged), not from a hardcoded
# `libForexConnect.so`: the same reason the copy set is derived. `--sdk` therefore aims the guard as
# well as the copy, which is what lets the packaging be driven end to end over a synthetic SDK in
# `crates/bridges/fxcm/tests/fcsdk_packaging.rs`.
#
# ## Who calls it, and who deliberately does not
#
# `just fxcm-package <project-root>` is the reachable spelling, and `docs/ops/tradehub-the CI box.md`'s
# project-folder layout step names it — the same shape `just lightgbm-build` and
# `crates/bridges/dukascopy/scripts/provision-jforex.sh` already have for the other two third-party
# runtime artifacts this workspace installs under a project root.
#
# ⚠ Neither `.github/workflows/release.yml` nor `deploy/sbin/vike-trader-ci-deploy` calls it, and
# that stays true for a REASON THAT CHANGED on 2026-09-09. It used to be that nothing installed
# FXCM at all — the released binaries declared no `vike-fxcm` dependency, so none could carry a
# `DT_NEEDED` on ForexConnect. The shipped `vike` now DOES carry the fxcm arm (`crates/vike/
# Cargo.toml`'s `full` forwards `vike-tradehub/fxcm`), and it is safe precisely because the feature
# adds no linkage: the binary starts everywhere, and FXCM arms only where this script has run.
#
# So the deploy still does not call it, and now that is a POSTURE rather than a consequence: FXCM is
# opt-in per box, because the ~25 MB library set is proprietary, staged out-of-band, and useless on
# the boxes that will never trade FX. `crates/bridges/fxcm/CLAUDE.md` carries the rest, and
# `docs/ops/fxcm-forexconnect.md` is the operator page for the boxes that do.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$(cd "$HERE/.." && pwd)"          # crates/bridges/fxcm — sibling of scripts/, rename-proof
ROOT="$(cd "$CRATE/../../.." && pwd)"    # crates/bridges/fxcm -> repo root

PIN="$CRATE/FCSDK.linux.sha256"
# `FCSDK_DIR` honored verbatim, the SAME rule crates/bridges/fxcm/build.rs applies, so a box that
# builds the bridge from a relocated SDK packages from that same SDK with no second setting.
SDK="${FCSDK_DIR:-$ROOT/vendor/fcsdk/linux}"
LIST_ONLY=0
IF_STAGED=0
DEST=""
# The ForexConnect SHIM, ours rather than the vendor's — a release asset or a local build. Empty is
# not an error: this script packages the LIBRARIES, and it reports the gap rather than filling it.
SHIM=""

usage() {
    echo "usage: $(basename "$0") [--sdk DIR] [--pin FILE] [--if-staged] { --list | <install-root> }" >&2
    echo "  <install-root>  the deployed project root; libraries land in <install-root>/lib" >&2
    echo "  --if-staged     a box with NO pinned library staged is a no-op, exit 0 (install path)" >&2
    echo "  --shim FILE     also install the ForexConnect shim (libfcshim.so) into <install-root>/lib" >&2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --list) LIST_ONLY=1; shift ;;
        --if-staged) IF_STAGED=1; shift ;;
        --pin)  PIN="${2:?--pin needs a path}"; shift 2 ;;
        --sdk)  SDK="${2:?--sdk needs a path}"; shift 2 ;;
        --shim) SHIM="${2:?--shim needs a path}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        -*) echo "unknown option: $1" >&2; usage; exit 2 ;;
        *)
            [ -z "$DEST" ] || { echo "only one <install-root> may be given (got '$DEST' and '$1')" >&2; exit 2; }
            DEST="$1"; shift ;;
    esac
done

# THE COPY SET. One row per pinned file DIRECTLY under `fcsdk/linux/lib/`, emitted as
# `<sha256>  <pin-path>`. `index(path, pfx) == 1` is a LITERAL prefix match (awk's `index`, not a
# regex), which is what selects the runtime libraries and EXCLUDES everything else a manifest may
# carry — the Windows companion `crates/bridges/fxcm/FCSDK.SHA256SUMS` pins `bin/`, `lib/` and
# header rows in one file, and an include/ row must never be copied into a loader search path.
#
# ⚠ The remainder must also carry no `/`. `lib/` is a FLAT loader search path, so a deeper row is
# not a member of it, and admitting one would flatten `lib/a/x.so` and `lib/b/x.so` onto the same
# destination name — a silent overwrite of one pinned library by another.
pinned_lib_rows() {
    [ -f "$PIN" ] || { echo "ERROR: no checksum manifest at $PIN" >&2; return 1; }
    awk -v pfx="fcsdk/linux/lib/" '
        /^[[:space:]]*#/    { next }
        NF < 2              { next }
        index($2, pfx) != 1 { next }
        { rest = substr($2, length(pfx) + 1) }
        rest != "" && index(rest, "/") == 0 { print $1 "  " $2 }
    ' "$PIN"
}

rows="$(pinned_lib_rows)"
# An EMPTY set must fail loudly rather than silently package nothing: a deploy that copies zero
# libraries produces exactly the tree this script exists to prevent, and does it with exit 0.
[ -n "$rows" ] || {
    echo "ERROR: $PIN names no fcsdk/linux/lib/ rows — refusing to package an empty runtime set." >&2
    exit 1
}

if [ "$LIST_ONLY" = 1 ]; then
    printf '%s\n' "$rows" | awk '{ n = split($2, seg, "/"); print seg[n] }'
    exit 0
fi

[ -n "$DEST" ] || { usage; exit 2; }

# THE GUARD. Reached only under `--if-staged`, and deliberately AFTER the manifest has been parsed
# and its empty-set refusal has fired: a broken pin is this repository's defect and no box may be
# allowed to excuse it. What the guard measures is how many pinned rows are actually staged, so the
# three populations stay distinct — NONE staged is the ordinary state of every box in the world and
# is silent; SOME staged falls through to the copy loop, which names the first missing file and
# fails; ALL staged packages exactly as it would without the flag.
#
# Evaluated BEFORE the `mkdir` below, so a no-op leaves no empty `lib/` behind. An empty directory on
# the loader's search path is not inert prose — it is the tree this script exists to prevent, and
# creating one would be this step reporting success at having built it.
if [ "$IF_STAGED" = 1 ]; then
    staged=0
    while read -r _ rel; do
        [ -n "$rel" ] || continue
        if [ -f "$SDK/${rel#fcsdk/linux/}" ]; then staged=$((staged + 1)); fi
    done <<EOF
$rows
EOF
    if [ "$staged" = 0 ]; then
        # Says only what was MEASURED — no pinned row is staged here. Whether anything on this box
        # links ForexConnect is a fact about binaries this script never looks at, so it does not
        # claim it: a reassuring line that is not checked is how a wrong claim survives.
        echo ">> no ForexConnect SDK staged at $SDK — nothing to package."
        echo "   (--if-staged; without the flag a missing pinned library is a hard failure instead.)"
        exit 0
    fi
fi

LIBDIR="$DEST/lib"
mkdir -p "$LIBDIR"

n=0
while read -r want rel; do
    [ -n "$rel" ] || continue
    src="$SDK/${rel#fcsdk/linux/}"        # fcsdk/linux/lib/foo.so -> $SDK/lib/foo.so
    base="${rel##*/}"
    [ -f "$src" ] || { echo "ERROR: $rel is not staged at $src" >&2; exit 1; }
    # Verified HERE and not merely at provision time: the pin is the only thing standing between a
    # deploy and a swapped .so, and provisioning may have happened weeks and several rsyncs ago.
    got="$( (sha256sum "$src" 2>/dev/null || shasum -a 256 "$src") | awk '{print $1}')"
    [ "$got" = "$want" ] || {
        echo "ERROR: $rel does not match the pin (pinned $want, got $got) — refusing to package a swapped library." >&2
        exit 1
    }
    cp -f "$src" "$LIBDIR/$base"
    n=$((n + 1))
done <<EOF
$rows
EOF

# Self-check: a partial copy is the failure mode that survives to load time, so prove the whole set
# landed rather than trusting the loop's own exit status.
while read -r _ rel; do
    [ -n "$rel" ] || continue
    [ -f "$LIBDIR/${rel##*/}" ] || { echo "ERROR: ${rel##*/} did not land in $LIBDIR" >&2; exit 1; }
done <<EOF
$rows
EOF

echo ">> packaged $n pinned ForexConnect libraries into $LIBDIR"

# ── THE SHIM — the eleventh file in this directory, and the one that is OURS ─────────────────────
#
# ⚠ **The libraries alone are no longer enough, and were not the whole story from 2026-09-09.**
# Until then an fxcm binary LINKED `libForexConnect.so` directly, so filling `<root>/lib` was the
# entire operator step: the executable sat at `<root>/bin/<exe>` and its baked `$ORIGIN/../lib`
# reached this directory. That binary is gone. `crates/bridges/fxcm/build.rs` now compiles the C++
# boundary into `libfcshim.so`, which links the SDK the ordinary way and carries its own `$ORIGIN`
# rpath, and `crates/bridges/fxcm/src/loader.rs` opens THAT at runtime.
#
# So the shim belongs IN this directory, beside the libraries it needs — `$ORIGIN` is the directory
# of the object itself — and `<root>/lib/libfcshim.so` is the first rung of the loader's ladder.
#
# ⚠ It is NOT copied out of the SDK, because it is not the vendor's: it is a release ASSET
# (`libfcshim.so`, attached by `scripts/release_fxcm_artifact.sh`) or a local build
# (`target/release/libfcshim.so`, which build.rs stages there). This script does not download and
# does not build, so it takes the file it is given and otherwise SAYS what is missing — a silent
# "libraries packaged" on a box that still cannot trade FXCM is exactly the shape of report this
# tree spends its gates refusing.
if [ -n "$SHIM" ]; then
    [ -f "$SHIM" ] || { echo "ERROR: --shim $SHIM does not exist" >&2; exit 1; }
    cp -f "$SHIM" "$LIBDIR/libfcshim.so"
    chmod 755 "$LIBDIR/libfcshim.so"
    echo ">> installed the ForexConnect shim: $LIBDIR/libfcshim.so"
elif [ -f "$LIBDIR/libfcshim.so" ]; then
    echo ">> the ForexConnect shim is already in place: $LIBDIR/libfcshim.so"
else
    echo ">> ⚠ NO SHIM: $LIBDIR/libfcshim.so is absent, so FXCM stays PAPER on this box."
    echo "   The libraries above are necessary and not sufficient — the process opens libfcshim.so,"
    echo "   not libForexConnect.so. Take it from the release (asset 'libfcshim.so') or from a local"
    echo "   'cargo build --release -p vike-fxcm --features fxcm', then re-run this with"
    echo "   --shim <path>. docs/ops/fxcm-forexconnect.md is the operator page."
fi
