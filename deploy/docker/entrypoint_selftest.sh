#!/usr/bin/env bash
# entrypoint_selftest.sh — exercise `deploy/docker/entrypoint.sh` on a scratch tree, with no Docker
# daemon and no image.
#
#     ./deploy/docker/entrypoint.sh --selftest        # the normal way in
#     ./deploy/docker/entrypoint_selftest.sh <path>   # what that execs
#
# ## Why this file exists
#
# **No runner in this repository has a container runtime.** MEASURED on the CI box 2026-08-24 — the box
# `.github/workflows/release.yml` runs on — `which docker podman buildah nerdctl` finds none of
# them and `/var/run/docker.sock` does not exist, while the same probe for `bash sha256sum install`
# answers with all three (the control that stops this being a claim about a broken probe). So
# nothing in CI can `docker build` this image, let alone run it.
#
# That would leave the entrypoint in the worst category this repo knows: an uncompiled artifact
# nothing checks. But almost everything the entrypoint DOES is pure shell over a directory — the
# content stamp, the verification, the wholesale swap, the refusals — and none of that needs a
# container. So the logic is driven HERE, against the REAL script, through its real `main`; only
# `FROM`/`COPY`/`ENV` remain unexecutable, and `crates/vike-ops/tests/container_image_gate.rs`
# holds those as text.
#
# ⚠ It drives the shipped script rather than a copy of its logic. `VIKE_IMAGE_STAGE_DIR` and
# `VIKE_IMAGE_BIN_DIR` exist for exactly this and for nothing else — a selftest that reimplemented
# the start sequence would prove its own reimplementation correct and say nothing about the file
# that ships.
set -uo pipefail

ENTRYPOINT="${1:-}"
[ -n "$ENTRYPOINT" ] && [ -f "$ENTRYPOINT" ] || {
  echo "usage: entrypoint_selftest.sh <path-to-entrypoint.sh>" >&2
  exit 2
}
ENTRYPOINT="$(cd "$(dirname "$ENTRYPOINT")" && pwd)/$(basename "$ENTRYPOINT")"

FAILURES=0
CASES=0
SECTION=""

section() {
  SECTION="$1"
  printf '\n== %s\n' "$1"
}
ok() {
  CASES=$((CASES + 1))
  printf '   ok   %s\n' "$1"
}
bad() {
  CASES=$((CASES + 1))
  FAILURES=$((FAILURES + 1))
  printf '   FAIL %s\n      %s\n' "$1" "${2:-}" >&2
}
check() {
  if [ "$2" = "$3" ]; then ok "$1"; else bad "$1" "expected [$3], got [$2]"; fi
}

WORK="$(mktemp -d)" || exit 2
trap 'rm -rf "$WORK"' EXIT

# ── Fixtures ─────────────────────────────────────────────────────────────────────────────────────

# A staged toolset with `n` marker files. `flavour` changes the CONTENT, so two stages with the same
# flavour are the same toolset and two with different flavours are not.
make_stage() {
  local dir="$1" flavour="$2"
  rm -rf "$dir"
  mkdir -p "$dir/toolA" "$dir/toolB"
  printf 'payload-%s\n' "$flavour" > "$dir/toolA/binary"
  printf 'provenance-%s\n' "$flavour" > "$dir/toolA/PROVENANCE"
  printf 'other-%s\n' "$flavour" > "$dir/toolB/thing.jar"
  chmod 755 "$dir/toolA/binary"
  bash "$ENTRYPOINT" --stamp "$dir" > /dev/null 2>&1
}

make_empty_stage() {
  local dir="$1"
  rm -rf "$dir"
  mkdir -p "$dir"
  bash "$ENTRYPOINT" --stamp "$dir" > /dev/null 2>&1
}

# The image's `/opt/vike/bin`: a stub `vike-cli` whose exit code the caller picks, and a stub
# `trade` that records the arguments it got and the PID it runs as.
#
# ⚠ **These two file names ARE the entrypoint's exec targets, and they are the image's LINK names.**
# In a real image both are symlinks to `vike-backend`, staged by
# `scripts/release_container_image.sh`'s `BINARIES`; here they are stubs standing in the same
# places. `trade` is the daemon verb since 2026-09-09 (it was `tradehub` for the hours between the
# prefix drop and the verb rename, and `vike-tradehub` before that);
# `vike-cli` deliberately kept its. If either name drifts from `deploy/docker/entrypoint.sh`, that
# script dies on a path that does not exist — which is the whole point of this fixture, so the two
# files must always be edited together.
make_image_bin() {
  local dir="$1" cli_exit="$2"
  rm -rf "$dir"
  mkdir -p "$dir"
  cat > "$dir/vike-cli" <<EOF
#!/usr/bin/env bash
echo "stub-vike-cli \$*" >> "$WORK/cli.log"
exit $cli_exit
EOF
  cat > "$dir/trade" <<EOF
#!/usr/bin/env bash
echo "\$*" > "$WORK/daemon.args"
echo "\$\$" > "$WORK/daemon.pid"
EOF
  chmod 755 "$dir/vike-cli" "$dir/trade"
}

# A COMPLETE project folder: `settings/`, the one writable directory, and the daemon profile the
# image's CMD names. Every one of those three is now a refusal when absent, so a fixture that
# omitted any of them would be testing a refusal rather than the case it was written for.
make_project() {
  local dir="$1"
  rm -rf "$dir"
  mkdir -p "$dir/settings/state"
  printf 'venue = "binance"\nsymbol = "BTCUSDT"\n' > "$dir/settings/tradehub.toml"
}

# The daemon profile inside a project fixture — the path a real start passes to `--config`.
profile_in() { printf '%s' "$1/settings/tradehub.toml"; }

# The image's copy of `settings/tradehub.example.toml`. The selftest uses the REAL repository file
# when it can find it (so `--template` is proved to hand out the shipped bytes) and a stand-in
# otherwise, which keeps this script runnable from a staged context that carries no checkout.
REPO_TEMPLATE="$(cd "$(dirname "$ENTRYPOINT")/../.." 2> /dev/null && pwd)/settings/tradehub.example.toml"
if [ ! -f "$REPO_TEMPLATE" ]; then
  REPO_TEMPLATE="$WORK/fallback-template.toml"
  printf '# stand-in template\nvenue = "binance"\nsymbol = "BTCUSDT"\n' > "$REPO_TEMPLATE"
fi

# Run the entrypoint's real `main` against scratch directories.
run_entrypoint() {
  local project="$1" stage="$2" imagebin="$3"
  shift 3
  rm -f "$WORK/daemon.args" "$WORK/daemon.pid"
  VIKE_SETTINGS_DIR="$project/settings" \
    VIKE_IMAGE_STAGE_DIR="$stage" \
    VIKE_IMAGE_BIN_DIR="$imagebin" \
    VIKE_IMAGE_TEMPLATE_FILE="$REPO_TEMPLATE" \
    bash "$ENTRYPOINT" "$@" > "$WORK/out.log" 2>&1
}

# The ordinary start: this project's own profile as `--config`, which is what every launcher passes.
run_start() {
  local project="$1"
  run_entrypoint "$@" --config "$(profile_in "$project")"
}

files_in() { (cd "$1" 2> /dev/null && find . -type f | LC_ALL=C sort | tr '\n' ' '); }

# =================================================================================================
section "the stamp is a CONTENT digest, not a name"
# =================================================================================================
#
# ⚠ THE property the whole design turns on. `.github/workflows/release.yml` publishes
# `vike-tradehub-fxcm` beside `vike-tradehub` from ONE tag, so two images of one release can carry
# different payloads; a tag-equal stamp would make the second skip its install and run on the
# first's tools. These cases are what make "equal stamps ⇒ identical toolset" true by construction.

make_stage "$WORK/s1" alpha
make_stage "$WORK/s2" alpha
make_stage "$WORK/s3" beta

a="$(head -n1 "$WORK/s1/.version")"
b="$(head -n1 "$WORK/s2/.version")"
c="$(head -n1 "$WORK/s3/.version")"

check "identical content stamps identically" "$a" "$b"
if [ "$a" != "$c" ]; then ok "different content stamps differently"; else
  bad "different content stamps differently" "both were [$a] — the stamp is not content-derived"
fi
case "$a" in
  toolset=sha256:*) ok "the stamp names its algorithm" ;;
  *) bad "the stamp names its algorithm" "got [$a]" ;;
esac

# Creation ORDER must not reach the digest: `find`'s traversal order is not stable across
# filesystems, so a stamp that depended on it would reinstall on every start.
rm -rf "$WORK/s4"
mkdir -p "$WORK/s4/toolB" "$WORK/s4/toolA"
printf 'other-alpha\n' > "$WORK/s4/toolB/thing.jar"
printf 'provenance-alpha\n' > "$WORK/s4/toolA/PROVENANCE"
printf 'payload-alpha\n' > "$WORK/s4/toolA/binary"
chmod 755 "$WORK/s4/toolA/binary"
bash "$ENTRYPOINT" --stamp "$WORK/s4" > /dev/null 2>&1
check "creation order does not reach the digest" "$(head -n1 "$WORK/s4/.version")" "$a"

# The manifest must not describe itself.
if grep -qE '(\.manifest|\.version)$' "$WORK/s1/.manifest"; then
  bad "the manifest excludes its own stamp files" "$(cat "$WORK/s1/.manifest")"
else
  ok "the manifest excludes its own stamp files"
fi

# ⚠ REGRESSION GUARD: `--stamp` must not inherit the START path's `umask 0077`.
#
# That call runs at IMAGE BUILD time, and the image is normally started with
# `--user "$(id -u):$(id -g)"` — the bind mount keeps HOST ownership, so the run-time uid is the
# operator's and is not knowable at build time. An owner-only `.version` therefore makes the
# entrypoint die on "cannot read the image's toolset stamp" for every operator whose uid differs
# from the build's. That bug was real: `umask 0077` sat at file scope in the first cut.
#
# ⚠ It comes WITH ITS OWN MUTATION CHECK, because the guard is worthless unless it can fail — and
# on a Windows working copy it CANNOT: the filesystem does not honour umask, every file reads back
# 644, and the check passes on a deliberately broken script. That was measured, not guessed. So the
# capability is PROBED first and both halves are skipped together when it is absent, rather than a
# vacuous pass being reported as coverage.
umask_is_honoured() {
  (
    umask 0077
    : > "$WORK/.umaskprobe"
  )
  [ "$(stat -c %a "$WORK/.umaskprobe" 2> /dev/null)" = "600" ]
}

if command -v stat > /dev/null 2>&1 && umask_is_honoured; then
  (
    umask 0022
    bash "$ENTRYPOINT" --stamp "$WORK/s1" > /dev/null 2>&1
  )
  mode="$(stat -c %a "$WORK/s1/.version" 2> /dev/null)"
  case "$mode" in
    *[4567]) ok "--stamp does not inherit the start path's restrictive umask (mode $mode)" ;;
    *) bad "--stamp does not inherit the start path's restrictive umask" \
      "'.version' is mode $mode — unreadable to a container started with --user <host uid>" ;;
  esac

  # The mutation: put `umask 0077` back at file scope and prove the check above turns red.
  sed 's|^readonly VERSION_FILE=.*|&\numask 0077|' "$ENTRYPOINT" > "$WORK/mutant.sh"
  (
    umask 0022
    bash "$WORK/mutant.sh" --stamp "$WORK/s1" > /dev/null 2>&1
  )
  mutant_mode="$(stat -c %a "$WORK/s1/.version" 2> /dev/null)"
  case "$mutant_mode" in
    *[4567]) bad "...and that check can actually FAIL" \
      "a file-scope 'umask 0077' still produced mode $mutant_mode — the guard is vacuous" ;;
    *) ok "...and that check can actually FAIL (mutant produced mode $mutant_mode)" ;;
  esac

  # Re-stamp under the fixture's own umask so later cases compare like with like.
  make_stage "$WORK/s1" alpha
else
  printf '   skip --stamp umask scoping (this filesystem does not honour umask)\n'
fi

# An EMPTY toolset is a legitimate, comparable answer rather than an error.
make_empty_stage "$WORK/s0"
if [ -f "$WORK/s0/.version" ] && [ -f "$WORK/s0/.manifest" ] && [ ! -s "$WORK/s0/.manifest" ]; then
  ok "an empty toolset still stamps"
else
  bad "an empty toolset still stamps" "manifest/version missing or non-empty"
fi

# =================================================================================================
section "--verify catches what a copy that 'succeeded' can still get wrong"
# =================================================================================================

bash "$ENTRYPOINT" --verify "$WORK/s1" > /dev/null 2>&1
check "a good tree verifies" "$?" "0"

cp -a "$WORK/s1" "$WORK/corrupt"
printf 'tampered\n' > "$WORK/corrupt/toolA/binary"
bash "$ENTRYPOINT" --verify "$WORK/corrupt" > /dev/null 2>&1
if [ "$?" -ne 0 ]; then ok "altered content fails verification"; else
  bad "altered content fails verification" "exit 0 on a tampered tree"
fi

cp -a "$WORK/s1" "$WORK/missing"
rm "$WORK/missing/toolB/thing.jar"
bash "$ENTRYPOINT" --verify "$WORK/missing" > /dev/null 2>&1
if [ "$?" -ne 0 ]; then ok "a missing file fails verification"; else
  bad "a missing file fails verification" "exit 0 with a file gone"
fi

# An empty manifest asserts an empty tree — otherwise "nothing was staged" and "the manifest was
# lost" would verify the same way.
cp -a "$WORK/s0" "$WORK/emptyplus"
printf 'stray\n' > "$WORK/emptyplus/stray"
bash "$ENTRYPOINT" --verify "$WORK/emptyplus" > /dev/null 2>&1
if [ "$?" -ne 0 ]; then ok "an empty manifest beside a populated tree fails"; else
  bad "an empty manifest beside a populated tree fails" "exit 0"
fi

bash "$ENTRYPOINT" --verify "$WORK/s0" > /dev/null 2>&1
check "a genuinely empty tree verifies" "$?" "0"

# =================================================================================================
section "the install: fresh, idempotent, and WHOLESALE"
# =================================================================================================

make_project "$WORK/p"
make_image_bin "$WORK/ib" 0

run_start "$WORK/p" "$WORK/s1" "$WORK/ib"
check "a fresh install exits 0" "$?" "0"
if [ -f "$WORK/p/bin/toolA/binary" ] && [ -f "$WORK/p/bin/.version" ]; then
  ok "a fresh install populates <project>/bin"
else
  bad "a fresh install populates <project>/bin" "$(cat "$WORK/out.log")"
fi
check "the installed stamp is the image's" "$(head -n1 "$WORK/p/bin/.version")" "$a"
# ⚠ Conditional on the filesystem, not on the code. An executable that arrives non-executable fails
# at spawn with a message about the PROGRAM rather than about the install, so `cp -a` carrying modes
# is worth asserting — but a Windows working copy cannot represent the bit at all (`chmod 755` on
# the fixture is a no-op there), and an assertion that fails for the filesystem rather than for the
# script would teach a reader to ignore this selftest. So it is skipped where it cannot mean
# anything, and the skip is PRINTED rather than silent.
if [ -x "$WORK/s1/toolA/binary" ]; then
  if [ -x "$WORK/p/bin/toolA/binary" ]; then ok "modes survive the copy"; else
    bad "modes survive the copy" "toolA/binary is not executable — it would fail at spawn"
  fi
else
  printf '   skip modes survive the copy (this filesystem cannot represent the exec bit)\n'
fi

# Second start, same toolset: nothing is reinstalled, and the directory is left alone.
touch -d '2001-01-01' "$WORK/p/bin/toolA/binary" 2> /dev/null
before="$(stat -c %Y "$WORK/p/bin/toolA/binary" 2> /dev/null)"
run_start "$WORK/p" "$WORK/s1" "$WORK/ib"
after="$(stat -c %Y "$WORK/p/bin/toolA/binary" 2> /dev/null)"
check "a matching stamp does not reinstall" "$before" "$after"
if grep -q "already installed" "$WORK/out.log"; then ok "...and says so"; else
  bad "...and says so" "$(cat "$WORK/out.log")"
fi

# ⚠ THE anti-lingering property. Install a DIFFERENT toolset over the first and prove the previous
# version's files are GONE — a file-by-file copy would leave `toolA/binary` from `s1` beside the new
# tree, and a tool directory holding two versions resolves the wrong one.
printf 'left-behind\n' > "$WORK/p/bin/toolA/ORPHAN"
run_start "$WORK/p" "$WORK/s3" "$WORK/ib"
check "a changed toolset reinstalls" "$(head -n1 "$WORK/p/bin/.version")" "$c"
if [ ! -e "$WORK/p/bin/toolA/ORPHAN" ]; then ok "the swap is WHOLESALE — no file lingers"; else
  bad "the swap is WHOLESALE — no file lingers" "ORPHAN survived: $(files_in "$WORK/p/bin")"
fi

# A tree that carries the right stamp but broken content re-installs rather than being trusted.
printf 'rot\n' > "$WORK/p/bin/toolA/binary"
run_start "$WORK/p" "$WORK/s3" "$WORK/ib"
check "a corrupted-but-stamped tree self-heals" "$(cat "$WORK/p/bin/toolA/binary")" "payload-beta"

# =================================================================================================
section "the refusals"
# =================================================================================================

# An unstamped, non-empty bin/ is the OPERATOR's (scripts/fetch_release_tools.sh installs there, and
# a native install puts the daemon's own binaries there). Replacing it wholesale would make this
# image the first thing in the workspace to delete somebody's files by surprise.
make_project "$WORK/p2"
mkdir -p "$WORK/p2/bin/jforex"
printf 'operator-jar\n' > "$WORK/p2/bin/jforex/jforex-bridge.jar"
run_start "$WORK/p2" "$WORK/s1" "$WORK/ib"
if [ "$?" -ne 0 ]; then ok "an unstamped bin/ is REFUSED, not replaced"; else
  bad "an unstamped bin/ is REFUSED, not replaced" "exit 0"
fi
if [ -f "$WORK/p2/bin/jforex/jforex-bridge.jar" ]; then ok "...and the operator's file survives"; else
  bad "...and the operator's file survives" "the refusal still deleted it"
fi
if [ ! -f "$WORK/daemon.pid" ]; then ok "...and the daemon never started"; else
  bad "...and the daemon never started" "the daemon ran past a refusal"
fi

VIKE_CONTAINER_ADOPT_BIN=1 VIKE_SETTINGS_DIR="$WORK/p2/settings" \
  VIKE_IMAGE_STAGE_DIR="$WORK/s1" VIKE_IMAGE_BIN_DIR="$WORK/ib" \
  bash "$ENTRYPOINT" --config "$(profile_in "$WORK/p2")" > "$WORK/out.log" 2>&1
check "the named override adopts it" "$?" "0"

# ⚠ The worst operator mistake: forgetting `-v`. Without the refusal this is a daemon that starts,
# reads no settings and no credentials, keeps every venue on paper, and logs nothing to say why.
rm -rf "$WORK/p3"
mkdir -p "$WORK/p3"
run_entrypoint "$WORK/p3" "$WORK/s1" "$WORK/ib" --config /x
if [ "$?" -ne 0 ]; then ok "a missing settings/ REFUSES to start"; else
  bad "a missing settings/ REFUSES to start" "exit 0 — this is the silent-paper failure"
fi
if grep -q -- "-v" "$WORK/out.log"; then ok "...naming the mount flag"; else
  bad "...naming the mount flag" "$(cat "$WORK/out.log")"
fi
# ⚠ …and it reports THAT and nothing else. An unmounted project has no `settings/state` and no
# daemon profile BECAUSE it has nothing; listing those beside the one problem that matters is how a
# refusal stops being read. (`--config /x` above names a file that does not exist, so this is a real
# opportunity for the profile arm to fire.)
#
# ⚠ Keyed on the tier-2 REPORT rather than on the words: the tier-1 message legitimately mentions
# `settings/state` when it tells you what a project folder must carry, and a grep for that string
# failed here on a correct tree — a false positive of exactly the kind that teaches people to
# delete a check.
if ! grep -q "problem(s)" "$WORK/out.log" && ! grep -q "daemon profile" "$WORK/out.log"; then
  ok "...and does not bury it under the consequences of itself"
else
  bad "...and does not bury it under the consequences of itself" "$(cat "$WORK/out.log")"
fi

VIKE_SETTINGS_DIR="" VIKE_IMAGE_STAGE_DIR="$WORK/s1" VIKE_IMAGE_BIN_DIR="$WORK/ib" \
  bash "$ENTRYPOINT" --config /x > "$WORK/out.log" 2>&1
if [ "$?" -ne 0 ]; then ok "an unset VIKE_SETTINGS_DIR REFUSES"; else
  bad "an unset VIKE_SETTINGS_DIR REFUSES" "exit 0 — the walk would start at /"
fi

# The `ExecStartPre=` equivalent must be able to STOP the start, or it is decoration.
make_project "$WORK/p4"
make_image_bin "$WORK/ibfail" 1
run_start "$WORK/p4" "$WORK/s1" "$WORK/ibfail"
if [ "$?" -ne 0 ]; then ok "a failed config check REFUSES to start"; else
  bad "a failed config check REFUSES to start" "exit 0"
fi
if [ ! -f "$WORK/daemon.pid" ]; then ok "...and the daemon never started"; else
  bad "...and the daemon never started" "the daemon ran past a failed pre-flight"
fi

# =================================================================================================
section "an EMPTY toolset installs nothing and touches nothing"
# =================================================================================================
#
# ⚠ This is the CURRENT production shape: the ten venues this image reaches spawn no program, and
# the one `<project>/bin` consumer left in the tree (dukascopy's jar+JRE) belongs
# to code the image does not carry. An empty stage must therefore leave an operator's own tools
# alone rather than replace them with nothing.
#
# ⚠ The fixture below still plants `bin/lightgbm`, deliberately. The second consumer — the research
# crate's binary — is gone, but `scripts/fetch_release_tools.sh` still INSTALLS that CLI there, so
# an operator's directory really can hold it and "leaves it untouched" is exactly the property
# under test. The subject is the operator's file, not a live resolver.

make_project "$WORK/p5"
mkdir -p "$WORK/p5/bin/lightgbm"
printf 'operator-tool\n' > "$WORK/p5/bin/lightgbm/lightgbm"
run_start "$WORK/p5" "$WORK/s0" "$WORK/ib"
check "an empty toolset still starts" "$?" "0"
if [ -f "$WORK/p5/bin/lightgbm/lightgbm" ]; then ok "...and leaves <project>/bin untouched"; else
  bad "...and leaves <project>/bin untouched" "an empty image deleted the operator's tools"
fi

# =================================================================================================
section "--template hands out the daemon profile, and nothing else"
# =================================================================================================
#
# ⚠ THE WALL every new user hit. The image's CMD is `--config /project/settings/tradehub.toml`,
# `crates/vike-tradehub/src/main.rs` EXITS when that file is absent, and nothing in the workspace
# creates it — while the only copy-paste sources, the nine per-venue runbooks under `docs/ops/`,
# live in a checkout a container operator does not have. `--template` is how the image hands over
# the repository's own `settings/tradehub.example.toml`.
#
# ⚠ **Stdout must carry the template and NOTHING else**, because the whole point is `--template >
# <project>/settings/tradehub.toml`: one `log` banner and the operator's profile has a line the
# daemon refuses at parse. That is what the byte comparison below is for, and the mutation after it
# is what makes the comparison mean something.

VIKE_IMAGE_TEMPLATE_FILE="$REPO_TEMPLATE" bash "$ENTRYPOINT" --template \
  > "$WORK/template.out" 2> "$WORK/template.err"
check "--template exits 0" "$?" "0"
if cmp -s "$WORK/template.out" "$REPO_TEMPLATE"; then
  ok "--template emits the shipped template BYTE FOR BYTE"
else
  bad "--template emits the shipped template BYTE FOR BYTE" \
    "stdout differs from $REPO_TEMPLATE: $(head -c 300 "$WORK/template.out")"
fi

# The mutation: put a `log` line in front of the `cat` and prove the comparison above turns red.
# Without this, a check that compared two identical empty files would read as coverage.
sed 's|^      cat "$TEMPLATE_FILE".*|      log "about to print the template"\n&|' \
  "$ENTRYPOINT" > "$WORK/tmutant.sh"
VIKE_IMAGE_TEMPLATE_FILE="$REPO_TEMPLATE" bash "$WORK/tmutant.sh" --template \
  > "$WORK/template.mut" 2>&1
if cmp -s "$WORK/template.mut" "$REPO_TEMPLATE"; then
  bad "...and that check can actually FAIL" \
    "a banner printed before the template did not change stdout — the comparison is vacuous"
else
  ok "...and that check can actually FAIL (a banner on stdout is detected)"
fi

# A malformed image must not produce a half-file: stdout stays EMPTY so a redirect leaves nothing
# worth keeping, and the message goes to stderr.
VIKE_IMAGE_TEMPLATE_FILE="$WORK/nope.toml" bash "$ENTRYPOINT" --template \
  > "$WORK/template.out" 2> "$WORK/template.err"
if [ "$?" -ne 0 ] && [ ! -s "$WORK/template.out" ]; then
  ok "a missing template REFUSES and writes nothing to stdout"
else
  bad "a missing template REFUSES and writes nothing to stdout" \
    "exit 0 or stdout non-empty: $(cat "$WORK/template.out")"
fi

VIKE_IMAGE_TEMPLATE_FILE="$REPO_TEMPLATE" bash "$ENTRYPOINT" --template extra \
  > "$WORK/template.out" 2>&1
if [ "$?" -ne 0 ]; then ok "--template takes no arguments"; else
  bad "--template takes no arguments" "exit 0 with a trailing argument"
fi

# ⚠ THE END-TO-END shape, and the reason the two halves are one section: what `--template` emits is
# a file the daemon ACCEPTS. A template that printed perfectly and did not parse would pass every
# check above.
make_project "$WORK/p7"
rm -f "$WORK/p7/settings/tradehub.toml"
VIKE_IMAGE_TEMPLATE_FILE="$REPO_TEMPLATE" bash "$ENTRYPOINT" --template \
  > "$WORK/p7/settings/tradehub.toml" 2> /dev/null
make_image_bin "$WORK/ib" 0
run_start "$WORK/p7" "$WORK/s1" "$WORK/ib"
check "a project seeded from --template starts" "$?" "0"

# =================================================================================================
section "the project folder must be able to START the daemon"
# =================================================================================================
#
# ⚠ Two refusals that did not exist, and one report that was worse than none.
#
#   * `settings/state` — `check_mount` NAMED it in its message and never checked it. Without it
#     `crates/vike-bridge-core/src/halt.rs`'s `halt_path_arming_error` finds no parent directory and
#     the daemon logs 'HALT KILL SWITCH IS NOT ARMABLE', reached even on a paper mount: the kill
#     switch is dead until somebody reaches for it. The native reference refuses the same case —
#     systemd will not start a unit whose `ReadWritePaths=` names a missing path (status=226).
#   * the daemon PROFILE — the file the image's own CMD names.
#   * and both were preceded by `vike-cli config check` printing '0 error(s), 0 warning(s)', which
#     is TRUE about the four settings files and reads as a clean bill for a box that cannot start.
#     The cure is ORDER: these refusals come first, so on a broken box the green report is never
#     printed at all. `cli.log` is how that is proved — the stub records every invocation.

make_image_bin "$WORK/ib" 0

# ── the profile ────────────────────────────────────────────────────────────────────────────────
make_project "$WORK/p8"
rm -f "$WORK/p8/settings/tradehub.toml"
: > "$WORK/cli.log"
run_start "$WORK/p8" "$WORK/s1" "$WORK/ib"
if [ "$?" -ne 0 ]; then ok "a missing daemon profile REFUSES to start"; else
  bad "a missing daemon profile REFUSES to start" "exit 0 — the daemon would exit on 'bad profile'"
fi
if grep -q -- "--template" "$WORK/out.log"; then ok "...naming the command that produces one"; else
  bad "...naming the command that produces one" "$(cat "$WORK/out.log")"
fi
if [ ! -f "$WORK/daemon.pid" ]; then ok "...and the daemon never started"; else
  bad "...and the daemon never started" "the daemon ran past a refusal"
fi
# ⚠ THE GREEN-REPORT PROPERTY, stated as a machine check rather than as a claim about output order.
if [ ! -s "$WORK/cli.log" ]; then
  ok "...and 'vike-cli config check' never ran, so no clean bill precedes the failure"
else
  bad "...and 'vike-cli config check' never ran, so no clean bill precedes the failure" \
    "the pre-flight reported on a box that cannot start: $(cat "$WORK/cli.log")"
fi

# The `--config=PATH` spelling is the daemon's other accepted form and must be seen too.
run_entrypoint "$WORK/p8" "$WORK/s1" "$WORK/ib" "--config=$(profile_in "$WORK/p8")"
if [ "$?" -ne 0 ]; then ok "the --config=PATH spelling is recognised"; else
  bad "the --config=PATH spelling is recognised" "exit 0 — the inline form slipped past the check"
fi

# …and arguments that name NO profile are not this wrapper's business: the daemon's own parser
# answers for them. A second, disagreeing copy of that parser is how a wrapper starts refusing
# arguments the binary would have taken.
run_entrypoint "$WORK/p8" "$WORK/s1" "$WORK/ib" --some-other-flag
check "arguments with no --config are left to the daemon" "$?" "0"

# ── settings/state ─────────────────────────────────────────────────────────────────────────────
make_project "$WORK/p9"
rmdir "$WORK/p9/settings/state"
: > "$WORK/cli.log"
run_start "$WORK/p9" "$WORK/s1" "$WORK/ib"
if [ "$?" -ne 0 ]; then ok "a missing settings/state REFUSES to start"; else
  bad "a missing settings/state REFUSES to start" "exit 0 — the HALT switch would be unarmable"
fi
if grep -q "HALT" "$WORK/out.log"; then ok "...naming what is lost"; else
  bad "...naming what is lost" "$(cat "$WORK/out.log")"
fi
if [ ! -d "$WORK/p9/settings/state" ]; then ok "...and it REFUSES rather than creating it"; else
  bad "...and it REFUSES rather than creating it" \
    "the entrypoint wrote into <project>/settings — see check_project's own argument"
fi
if [ ! -s "$WORK/cli.log" ]; then ok "...and no clean bill precedes this failure either"; else
  bad "...and no clean bill precedes this failure either" "$(cat "$WORK/cli.log")"
fi

# ── BOTH missing: ONE refusal carrying the whole list ───────────────────────────────────────────
#
# ⚠ This is a fresh project folder, i.e. the ordinary first run. Reporting one problem, being fixed,
# then reporting the other is two round trips through a container start — the shape
# `vike_config::refuse_credential_file_arming` avoids for the same reason.
make_project "$WORK/p10"
rmdir "$WORK/p10/settings/state"
rm -f "$WORK/p10/settings/tradehub.toml"
run_start "$WORK/p10" "$WORK/s1" "$WORK/ib"
if [ "$?" -ne 0 ]; then ok "a fresh project folder REFUSES"; else
  bad "a fresh project folder REFUSES" "exit 0"
fi
if grep -q "2 problem" "$WORK/out.log"; then ok "...counting BOTH problems"; else
  bad "...counting BOTH problems" "$(cat "$WORK/out.log")"
fi
if grep -q "settings/state" "$WORK/out.log" && grep -q "daemon profile" "$WORK/out.log"; then
  ok "...and naming each of them in ONE refusal"
else
  bad "...and naming each of them in ONE refusal" "$(cat "$WORK/out.log")"
fi

# The mutation: drop the profile arm from `check_project` and prove the cases above turn green,
# i.e. that they are testing the arm rather than something else that happens to refuse.
sed 's|^  if \[ -n "$profile" \] && \[ ! -f "$profile" \]; then|  if false; then|' \
  "$ENTRYPOINT" > "$WORK/pmutant.sh"
make_project "$WORK/p11"
rm -f "$WORK/p11/settings/tradehub.toml"
rm -f "$WORK/daemon.pid"
VIKE_SETTINGS_DIR="$WORK/p11/settings" VIKE_IMAGE_STAGE_DIR="$WORK/s1" \
  VIKE_IMAGE_BIN_DIR="$WORK/ib" VIKE_IMAGE_TEMPLATE_FILE="$REPO_TEMPLATE" \
  bash "$WORK/pmutant.sh" --config "$(profile_in "$WORK/p11")" > "$WORK/out.log" 2>&1
if [ "$?" -eq 0 ]; then
  ok "...and removing the profile arm makes a missing profile start again (the check is load-bearing)"
else
  bad "...and removing the profile arm makes a missing profile start again" \
    "the mutant still refused, so the cases above may be passing for another reason: \
$(cat "$WORK/out.log")"
fi

# =================================================================================================
section "the daemon is exec'd, and gets its arguments"
# =================================================================================================
#
# ⚠ `exec` is what makes `docker stop` reach the daemon's SIGTERM handler. Without it the shell is
# PID 1, a shell does not forward signals, and the stop is a SIGKILL after the timeout — the
# resting-order cancel sweep never runs and the book is abandoned at the venue. Proved by PID
# identity: if the daemon reports the same PID this script spawned, it REPLACED the shell.

make_project "$WORK/p6"
make_image_bin "$WORK/ib" 0
rm -f "$WORK/daemon.pid" "$WORK/daemon.args"
p6_profile="$(profile_in "$WORK/p6")"
VIKE_SETTINGS_DIR="$WORK/p6/settings" VIKE_IMAGE_STAGE_DIR="$WORK/s1" \
  VIKE_IMAGE_BIN_DIR="$WORK/ib" bash "$ENTRYPOINT" --config "$p6_profile" \
  > "$WORK/out.log" 2>&1 &
spawned=$!
wait $spawned
check "the daemon receives its arguments" \
  "$(cat "$WORK/daemon.args" 2> /dev/null)" "--config $p6_profile"
check "the daemon REPLACED the shell (exec)" "$(cat "$WORK/daemon.pid" 2> /dev/null)" "$spawned"


section "the append-atomicity probe"

# ⚠ WHAT THIS PROBE NOW MEASURES. `crates/vike-model/src/change_journal.rs`'s `append_record` takes
# an exclusive flock around its open/write/sync, so a plain-`>>` probe would refuse a filesystem the
# daemon handles correctly AND would have stopped testing the real write pattern. The probe is
# therefore the flock-SERIALISED shape, and what it still catches is the one case no code change
# rescues: a filesystem whose locking is a no-op, so serialising does not serialise.
#
# ⚠ WHAT THIS FILE CAN AND CANNOT TEST. Reproducing the LOSS needs a filesystem that loses
# concurrent appends, and the only one to hand is a Docker Desktop host bind mount on Windows/macOS
# — which no CI box has (there is no container runtime on any of them, measured). So the loss
# measurements are BY HAND on real Docker Desktop 29.7.2 and are recorded in the commit and in
# docs/ops/tradehub-container.md: plain `>>` lost 64 of 100 lines there while the flock-serialised
# form kept 100 of 100, and the container's own overlayfs kept 100 of 100 either way. What IS
# testable here is everything else — the no-false-refusal case (the one that would bite every Linux
# operator if it broke), the unrunnable-probe verdict, and the broken-lock refusal, which is reached
# by shadowing `flock` with a failing shim rather than by needing an exotic filesystem.

# ⚠ NEVER `. "$ENTRYPOINT"` here. That file ends in `main "$@"`, and `main` reaches `die`, which
# calls `exit` — in a SOURCED script that exits THIS shell, so a `|| true` guard never runs. Doing
# it printed the section header and killed the suite with no further output. Lift the function out
# textually instead, which is also what an operator debugging it would do.
sed -n '/^append_atomicity_ok()/,/^}/p' "$ENTRYPOINT" > "$WORK/probe.fn"
. "$WORK/probe.fn"

# The probe is only a measurement where flock(1) exists. Asserted as its own case so that a missing
# tool reads as a missing tool rather than as a mysterious refusal three cases below.
if command -v flock > /dev/null 2>&1; then
  ok "flock(1) is available to the probe"
else
  bad "flock(1) is available to the probe" \
      "no flock on PATH — the probe can only return 2 (unmeasured) from here on"
fi

mkdir -p "$WORK/goodfs"
if append_atomicity_ok "$WORK/goodfs"; then
  ok "an ordinary filesystem is NOT refused"
else
  bad "an ordinary filesystem is NOT refused" \
      "got ${APPEND_PROBE_GOT:-?} of ${APPEND_PROBE_WANT:-?}, ${APPEND_PROBE_LOCKFAIL:-?} failed acquires (an unset triple means the probe returned 2, unmeasured) — a false refusal would strand every Linux operator"
fi

# …and it really did go through the lock, rather than passing because the serialisation was skipped.
check "the healthy probe took the lock every time" "${APPEND_PROBE_LOCKFAIL:-unset}" "0"

# It must leave nothing behind: the probe judges a directory the daemon is about to use.
leftovers="$(find "$WORK/goodfs" -name '.vike-append-probe*' 2> /dev/null | wc -l)"
check "the probe removes its own scratch" "$leftovers" "0"

# An unrunnable probe is NOT a verdict. 2 means "could not measure"; turning that into a refusal
# would be the same mistake as trusting a filesystem name.
# ⚠ The path must be one `mkdir -p` genuinely CANNOT create. A merely-absent path is not it — the
# first fixture here was `$WORK/definitely/not/a/directory`, and `mkdir -p` cheerfully built the
# whole chain, so the probe ran and returned 0. A regular FILE with a child path under it fails
# with ENOTDIR and cannot be talked out of it.
printf 'not a directory\n' > "$WORK/a-file"
append_atomicity_ok "$WORK/a-file/state"
check "an unrunnable probe returns 2, not a refusal" "$?" "2"

# ⚠ MUTATION GUARD. A probe that always passes would satisfy every case above, so this asserts the
# arithmetic can actually fail: with the expected count deliberately raised, the same healthy
# directory must be reported as short. Without this, deleting the comparison leaves the file green.
probe_with_want() {
  local dir="$1" want="$2" got
  mkdir -p "$dir/.m" && printf 'x\n' > "$dir/.m/log"
  got=$(wc -l < "$dir/.m/log"); rm -rf "$dir/.m"
  [ "$got" -eq "$want" ]
}
if probe_with_want "$WORK/goodfs" 999; then
  bad "the count comparison can fail" "a 1-line file compared equal to 999 — the check is inert"
else
  ok "the count comparison can fail"
fi

# ⚠ THE CASE THE PROBE NOW EXISTS FOR: a filesystem whose LOCKING does not work. It cannot be
# reached with a filesystem on any box here, so it is reached through the tool: a shell function
# named `flock` shadows the binary for the probe's subshells (and satisfies `command -v`, so the
# unmeasurable arm is not what fires), and every acquire fails. The directory underneath is the same
# healthy one that passed two cases above, which is what makes this about the LOCK and nothing else.
flock() { return 1; }
mkdir -p "$WORK/nolock"
append_atomicity_ok "$WORK/nolock"
broken_rc=$?
unset -f flock
check "a filesystem whose flock always fails is REFUSED" "$broken_rc" "1"
if [ "${APPEND_PROBE_LOCKFAIL:-0}" -gt 0 ]; then
  ok "…and the refusal counts the failed acquires"
else
  bad "…and the refusal counts the failed acquires" \
      "APPEND_PROBE_LOCKFAIL=${APPEND_PROBE_LOCKFAIL:-unset} — the probe is not noticing a dead lock"
fi

# The anti-vacuity twin of the two cases above: the shim is really gone, so a later reader cannot
# mistake a permanently-shadowed `flock` for a passing suite.
mkdir -p "$WORK/afterlock"
if append_atomicity_ok "$WORK/afterlock"; then
  ok "the healthy verdict returns once the shim is removed"
else
  bad "the healthy verdict returns once the shim is removed" \
      "got ${APPEND_PROBE_GOT:-?} of ${APPEND_PROBE_WANT:-?}, ${APPEND_PROBE_LOCKFAIL:-?} failed acquires"
fi

# A probe that cannot find flock(1) is UNMEASURED (2), never a refusal — the same rule as a probe
# that cannot create its scratch directory. Reached by emptying PATH, which is restored immediately.
probe_path="$PATH"
PATH=""
append_atomicity_ok "$WORK/goodfs"
noflock_rc=$?
PATH="$probe_path"
check "a missing flock(1) returns 2, not a refusal" "$noflock_rc" "2"


# =================================================================================================
printf '\n'
if [ "$FAILURES" -eq 0 ]; then
  printf 'entrypoint selftest: PASS (%d cases)\n' "$CASES"
  exit 0
fi

printf 'entrypoint selftest: FAIL (%d of %d cases)\n' "$FAILURES" "$CASES" >&2
exit 1
