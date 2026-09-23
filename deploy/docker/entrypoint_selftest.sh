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

# A LONG-LIVED daemon stub: it records that it started and with what argv, traps SIGTERM (recording
# that it received one), and then blocks.
#
# ⚠ **`sleep N & wait $!` rather than a bare `sleep`, and that is the whole fixture.** Bash does not
# run a trap while a FOREGROUND command is executing — it runs it after that command returns — so a
# stub written `while :; do sleep 5; done` would take up to five seconds to notice SIGTERM, and a
# stub written `sleep infinity` would never notice it at all. `wait` is a BUILTIN and IS
# interruptible, so this shape reacts immediately. A fixture that reacted late would make the stop
# cases below pass or fail on timing rather than on the entrypoint's behaviour.
#
# With `die_after` it exits with `die_status` instead of blocking — the fixture for "a child died and
# nobody asked", which is the failure this supervisor exists to make loud.
make_daemon_stub() {
  local path="$1" name="$2" die_after="${3:-}" die_status="${4:-0}"
  # ⚠ **THE TRAP IS THE FIRST STATEMENT, AND `pid.<name>` IS WRITTEN AFTER IT.** That ordering is
  # what makes `pid.<name>` a READINESS marker rather than just a record: a SIGTERM arriving before
  # the trap is installed kills the stub with the DEFAULT disposition and records nothing, so a
  # stop case would report a child that "never got SIGTERM" when in fact the fixture was not ready
  # to notice one. MEASURED on the CI box at load 118: `strategy-builder` — the LAST child started, so
  # the one with the smallest window — went missing from the stop case that way. The supervisor is
  # not what raced; a real daemon installs its handler as the first statement of `main`, which
  # `crates/vike-ops/tests/graceful_stop_pin.rs` pins positionally. But a fixture that races turns a
  # load-bearing check into a flaky one, which is worse than not having it.
  cat > "$path" <<EOF
#!/usr/bin/env bash
trap 'printf "%s\n" "$name" >> "$WORK/termed"; exit 143' TERM
printf '%s\n' "$name" >> "$WORK/started"
printf '%s\n' "\$*" > "$WORK/args.$name"
printf '%s\n' "\$\$" > "$WORK/pid.$name"
EOF
  if [ -n "$die_after" ]; then
    cat >> "$path" <<EOF
sleep $die_after
printf '%s\n' "$name" >> "$WORK/exited"
exit $die_status
EOF
  else
    cat >> "$path" <<'EOF'
while :; do
  sleep 5 &
  wait $!
done
EOF
  fi
  chmod 755 "$path"
}

# The image's `/opt/vike/bin`: a stub `vike-cli` whose exit code the caller picks, plus a stub for
# each of the three processes the entrypoint supervises.
#
# ⚠ **These file names ARE the entrypoint's spawn targets, and they are the image's LINK names.**
# In a real image all four are symlinks to `vike-backend`, staged by
# `scripts/release_container_image.sh`'s `BINARIES`; here they are stubs standing in the same
# places. `trade` is the daemon verb since 2026-09-09 (it was `tradehub` for the hours between the
# prefix drop and the verb rename, and `vike-tradehub` before that); `backtest` and
# `strategy-builder` joined when the image became one container running three processes; `vike-cli`
# deliberately kept its prefix. If any name drifts from `deploy/docker/entrypoint.sh`, that script
# spawns a path that does not exist — which is the whole point of this fixture, so the two files
# must always be edited together.
#
# ⚠ Every stub writes `pid.<name>` and `args.<name>`, and the refusal cases below assert "…and the
# daemon never started" by the ABSENCE of `pid.trade`. Those cases read `daemon.pid` before the
# supervisor landed; the file is per-child now because there are three children and "did the daemon
# start" stopped being a question with one answer.
make_image_bin() {
  local dir="$1" cli_exit="$2"
  rm -rf "$dir"
  mkdir -p "$dir"
  cat > "$dir/vike-cli" <<EOF
#!/usr/bin/env bash
echo "stub-vike-cli \$*" >> "$WORK/cli.log"
exit $cli_exit
EOF
  chmod 755 "$dir/vike-cli"
  make_daemon_stub "$dir/trade" trade
  make_daemon_stub "$dir/backtest" backtest
  make_daemon_stub "$dir/strategy-builder" strategy-builder
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

# ⚠ **A SUCCESSFUL START NO LONGER EXITS, and that is the single change that reaches every case
# below.** Until 2026-09-23 this script ended in `exec "$BIN_DIR/trade"`, so a healthy start was a
# process that ran to completion and `run_entrypoint` could simply be a foreground call whose status
# was the verdict. The entrypoint now SUPERVISES three children and stays with them — which is the
# shape under test — so a foreground call would hang the suite forever.
#
# So every invocation runs in the BACKGROUND and is driven to a DECISION: either it dies on its own
# (a refusal, which is still the whole of what most cases here check) or it announces that it is
# supervising, at which point it is stopped the way `docker stop` stops it. The status returned is
# therefore still "what this start was worth" — non-zero for a refusal, and the supervisor's own
# requested-stop status for a start.
#
# ⚠ `env` rather than a `VAR=… bash` prefix, because `env` EXECs: `$!` is then the entrypoint's own
# bash, so `kill -TERM "$ENTRY_PID"` reaches the supervisor rather than a wrapper that would swallow
# it. A `( … ) &` subshell would swallow it for the same reason, and a stop test against a shell
# that never received the signal is the exact vacuous pass this file exists to avoid.
ENTRY_ENV=()
ENTRY_PID=""

# ⚠ **TWO MODES, and picking the wrong one silently invalidates a case.** `AWAIT_STOP=1` (the
# default) drives a start to the point where it is supervising and then STOPS it, which is what
# every ordinary case wants. `AWAIT_STOP=0` waits for the entrypoint to exit ON ITS OWN, and the
# cases that need it are the ones about a CHILD dying: sending SIGTERM the moment the supervisor
# announces itself would make it a requested stop before the child ever died, and the case would
# report a clean exit 0 while proving nothing at all.
AWAIT_STOP=1

# Wait for the backgrounded entrypoint to reach a decision, then stop it if `AWAIT_STOP` says to.
# Returns what it settled on; 99 means it did neither within the budget, which is a FAILED case
# rather than a hung suite — a selftest that hangs is one nobody runs.
await_decision() {
  local waited=0
  while kill -0 "$ENTRY_PID" 2> /dev/null; do
    if [ "$AWAIT_STOP" = 1 ]; then
      grep -q 'supervising' "$WORK/out.log" 2> /dev/null && break
    fi
    waited=$((waited + 1))
    # ⚠ 60s at 100ms, and the generosity is deliberate rather than lazy: this budget exists to stop
    # a HANG becoming a hung suite, not to measure anything. the CI box runs the merge gate for several
    # branches at once and was measured at load 115 while this suite ran, so a budget tuned to an
    # idle box would turn a busy runner into a flaky gate — which is how a real failure stops being
    # believed.
    if [ "$waited" -gt 600 ]; then
      kill -KILL "$ENTRY_PID" 2> /dev/null
      wait "$ENTRY_PID" 2> /dev/null
      return 99
    fi
    sleep 0.1
  done
  if [ "$AWAIT_STOP" = 1 ] && kill -0 "$ENTRY_PID" 2> /dev/null; then
    # ⚠ …but not before every child the supervisor SAYS it started has written its readiness
    # marker. See `make_daemon_stub` for the race this closes and for why closing it in the fixture
    # is right: the marker is written after the stub's `trap`, so counting markers against the
    # supervisor's own "started" lines is exactly "is every child able to notice a SIGTERM yet".
    local started ready spins=0
    started="$(grep -c '^vike-entrypoint: started ' "$WORK/out.log" 2> /dev/null)" || started=0
    while [ "$spins" -lt 200 ]; do
      ready="$(ls "$WORK"/pid.* 2> /dev/null | wc -l)"
      [ "$ready" -ge "$started" ] && break
      spins=$((spins + 1))
      sleep 0.05
    done
    kill -TERM "$ENTRY_PID" 2> /dev/null
  fi
  wait "$ENTRY_PID" 2> /dev/null
  return $?
}

# Stop anything a case deliberately orphaned — reached through the pid each stub recorded, so it is
# exact rather than a `pkill` pattern match (and `pkill` does not reach these on every box this
# selftest runs on).
reap_orphans() {
  local n p
  for n in trade backtest strategy-builder; do
    p="$(cat "$WORK/pid.$n" 2> /dev/null)" || continue
    [ -n "$p" ] && kill -TERM "$p" 2> /dev/null
  done
  return 0
}

# Run a SCRIPT (the real entrypoint, or one of the mutants below) against scratch directories.
run_script() {
  local script="$1" project="$2" stage="$3" imagebin="$4"
  shift 4
  rm -f "$WORK/args.trade" "$WORK/pid.trade" "$WORK/args.backtest" "$WORK/pid.backtest" \
    "$WORK/args.strategy-builder" "$WORK/pid.strategy-builder" \
    "$WORK/started" "$WORK/termed" "$WORK/exited"
  # ⚠ **`out.log` IS TRUNCATED HERE, IN THE PARENT, AND THAT IS A FIX FOR A MEASURED FLAKE.** The
  # `>` redirect below truncates it too — but in the CHILD, after the fork, so for a moment the
  # previous run's output is still in the file. `await_decision` polls that file for the
  # supervisor's `supervising` line, and on a loaded box it can win that race: a REFUSAL case then
  # matched the PREVIOUS case's `supervising`, decided a start had happened, and SIGTERM'd an
  # entrypoint that was still in its pre-flight. The refusal case still "passed" (143 is non-zero)
  # and the next assertion failed against an out.log that had been truncated and never rewritten.
  # Seen once in 1091 tests on the CI box at load ~120; three consecutive local runs could not reproduce
  # it, which is exactly why it is fixed at the mechanism rather than retried.
  # ⚠ It is the SAME defect class the supervisor's own wait loop documents — reading state left by
  # an earlier step as though it described the current one. Worth noticing that a harness can have
  # it too.
  : > "$WORK/out.log"
  env "${ENTRY_ENV[@]}" \
    VIKE_SETTINGS_DIR="$project/settings" \
    VIKE_IMAGE_STAGE_DIR="$stage" \
    VIKE_IMAGE_BIN_DIR="$imagebin" \
    VIKE_IMAGE_TEMPLATE_FILE="$REPO_TEMPLATE" \
    bash "$script" "$@" > "$WORK/out.log" 2>&1 &
  ENTRY_PID=$!
  await_decision
}

# Run the entrypoint's real `main` against scratch directories.
run_entrypoint() {
  run_script "$ENTRYPOINT" "$@"
}

# The ordinary start: this project's own profile as `--config`, which is what every launcher passes.
run_start() {
  local project="$1"
  run_entrypoint "$@" --config "$(profile_in "$project")"
}

# ⚠ The two variables the strategy builder refuses to start without are unset for the WHOLE suite,
# deliberately and explicitly: the cases below assert that it is SKIPPED, and inheriting a value
# from whoever ran this script would turn those into assertions about that person's shell.
unset VIKE_STRATEGY_BUILDER_KEY VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT

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
if [ ! -f "$WORK/pid.trade" ]; then ok "...and the daemon never started"; else
  bad "...and the daemon never started" "the daemon ran past a refusal"
fi

ENTRY_ENV=(VIKE_CONTAINER_ADOPT_BIN=1)
run_start "$WORK/p2" "$WORK/s1" "$WORK/ib"
check "the named override adopts it" "$?" "0"
ENTRY_ENV=()

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
if [ ! -f "$WORK/pid.trade" ]; then ok "...and the daemon never started"; else
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
if [ ! -f "$WORK/pid.trade" ]; then ok "...and the daemon never started"; else
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
run_script "$WORK/pmutant.sh" "$WORK/p11" "$WORK/s1" "$WORK/ib" \
  --config "$(profile_in "$WORK/p11")"
if [ "$?" -eq 0 ]; then
  ok "...and removing the profile arm makes a missing profile start again (the check is load-bearing)"
else
  bad "...and removing the profile arm makes a missing profile start again" \
    "the mutant still refused, so the cases above may be passing for another reason: \
$(cat "$WORK/out.log")"
fi

# =================================================================================================
section "the supervisor starts the right processes, with the right arguments"
# =================================================================================================
#
# ⚠ **THIS SECTION REPLACED "the daemon is exec'd".** That one proved PID IDENTITY — if the daemon
# reported the same pid this script spawned, it had REPLACED the shell — and that was the whole
# guarantee while the image ran one process. Three processes cannot each replace the shell, so the
# guarantee moved into `supervise`'s `trap` and the proof has to move with it: the stop section
# below is what now stands where that check stood, and it is a STRICTLY harder thing to prove,
# which is why it carries a mutation of its own.

make_project "$WORK/p6"
make_image_bin "$WORK/ib" 0
p6_profile="$(profile_in "$WORK/p6")"
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
check "a supervised start stops cleanly on SIGTERM" "$?" "0"

check "the trading daemon receives the container's arguments" \
  "$(cat "$WORK/args.trade" 2> /dev/null)" "--config $p6_profile"

# ⚠ **THE REGRESSION CASE, and it is the one a real container run had to find.** A supervisor that
# INVENTS a death is worse than one that misses a real one: `wait -n` — the obvious way to wait for
# any child — returned immediately against a STALE job-table entry left by `check_project`'s
# append-atomicity probe, so the built image exited 70 seconds after start with both daemons
# healthy inside it. `supervise`'s own wait loop carries the measurement.
# ⚠ On a filesystem with no `flock(1)` this case is WEAKER than it looks, and deliberately kept:
# the probe returns early there without spawning anything, so the stale entry never exists. It is
# the Linux runs — CI's, and the box that ships the image — where it bites.
if ! grep -q "FATAL" "$WORK/out.log"; then
  ok "a healthy supervised run reports NO death (no phantom from a stale job entry)"
else
  bad "a healthy supervised run reports NO death" \
    "$(grep FATAL "$WORK/out.log") — every child was healthy, so this is an INVENTED failure"
fi

# ⚠ `--addr` WITH NO VALUE, exactly as `deploy/vike-backtest.service`'s ExecStart= spells it: the
# FLAG says "become a daemon" and the VALUE resolves through VIKE_BACKTEST_ADDR ->
# config.backtest_addr -> 127.0.0.1:7880. A supervisor that passed an address here would put it in
# two places in one container.
check "the compute daemon is started as a DAEMON, not a one-shot" \
  "$(cat "$WORK/args.backtest" 2> /dev/null)" "--addr"

# ⚠ The container's CMD belongs to `trade` ALONE. A supervisor that fanned `"$@"` out to all three
# would hand `backtest` a `--config` it does not take and the container would die on its own
# arguments.
if [ ! -s "$WORK/args.backtest" ] || ! grep -q -- "--config" "$WORK/args.backtest"; then
  ok "...and the container's --config was NOT fanned out to it"
else
  bad "...and the container's --config was NOT fanned out to it" \
    "backtest got [$(cat "$WORK/args.backtest")]"
fi

# =================================================================================================
section "the strategy builder is SKIPPED, loudly, when it cannot start"
# =================================================================================================
#
# ⚠ `crates/vike-strategy-builder/src/builder.rs`'s `run` refuses to start without BOTH
# `VIKE_STRATEGY_BUILDER_KEY` and `VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT`, and no image can supply
# either — a key the image invented would be a credential nobody holds. Starting it anyway would
# make it exit FAILURE, which under this supervisor takes the CONTAINER down: every existing
# `docker run`, none of which sets those variables, would stop working. So it is skipped and the
# skip is announced. See `supervise`'s own `builder_blockers` for the argument.

if [ ! -f "$WORK/pid.strategy-builder" ]; then
  ok "an unconfigured strategy builder is not started"
else
  bad "an unconfigured strategy builder is not started" \
    "it ran, and would have exited FAILURE and taken the container with it"
fi
if grep -q "VIKE_STRATEGY_BUILDER_KEY" "$WORK/out.log"; then
  ok "...and the log names the variable that would turn it on"
else
  bad "...and the log names the variable that would turn it on" "$(cat "$WORK/out.log")"
fi
if [ -f "$WORK/pid.trade" ] && [ -f "$WORK/pid.backtest" ]; then
  ok "...and the other two are unaffected"
else
  bad "...and the other two are unaffected" "started: $(cat "$WORK/started" 2> /dev/null)"
fi

# ⚠ HALF-configured must skip too, and this is the case the first draft of the rule got wrong. A
# KEY with no WORKSPACE_ROOT is a service that still refuses — so gating on the key alone would let
# an operator who set one of two variables take the whole container down, which is a NEW regression
# introduced by the very change meant to avoid one.
ENTRY_ENV=(VIKE_STRATEGY_BUILDER_KEY=selftest-key)
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
half_rc=$?
ENTRY_ENV=()
check "a HALF-configured builder still starts the container" "$half_rc" "0"
if [ ! -f "$WORK/pid.strategy-builder" ]; then
  ok "...and the builder is still skipped"
else
  bad "...and the builder is still skipped" "it ran with no workspace root and would have refused"
fi
if grep -q "VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT" "$WORK/out.log"; then
  ok "...naming the one that is still missing"
else
  bad "...naming the one that is still missing" "$(cat "$WORK/out.log")"
fi

# ...and with BOTH set it really is the third process.
ENTRY_ENV=(VIKE_STRATEGY_BUILDER_KEY=selftest-key
  VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT="$WORK")
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
both_rc=$?
ENTRY_ENV=()
check "a fully-configured builder starts the container" "$both_rc" "0"
if [ -f "$WORK/pid.strategy-builder" ]; then
  ok "...and the builder IS the third process"
else
  bad "...and the builder IS the third process" "started: $(cat "$WORK/started" 2> /dev/null)"
fi

# =================================================================================================
section "SIGTERM reaches EVERY child — the guarantee `exec` used to buy"
# =================================================================================================
#
# ⚠ **THE most dangerous thing this file checks.** `docker stop` sends SIGTERM to PID 1, which is
# now a shell — and a shell does NOT forward it. Without `supervise`'s trap the container hangs
# until `--stop-timeout`, SIGKILL wins, `cancel_orders_on_shutdown` never runs and the resting book
# is ABANDONED at the venue. That is the defect `crates/vike-ops/tests/graceful_stop_pin.rs` exists
# to keep fixed, reached through the container instead of through the unit file.
#
# Every stub records the SIGTERM it received, so this is a positive check on each child rather than
# an inference from the container's exit status.

ENTRY_ENV=(VIKE_STRATEGY_BUILDER_KEY=selftest-key
  VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT="$WORK")
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
stop_rc=$?
ENTRY_ENV=()
check "a stopped container exits 0 — a requested stop is not a failure" "$stop_rc" "0"
termed="$(LC_ALL=C sort "$WORK/termed" 2> /dev/null | tr '\n' ' ')"
check "all three children received SIGTERM" "$termed" "backtest strategy-builder trade "

# ⚠ THE MUTATION, and without it every case above would pass against a script with no trap at all —
# the children would simply be killed when the suite exits and nothing would notice. Removing the
# `trap` line must make the check above turn RED.
sed 's|^  trap forward_term TERM INT$|  : no trap installed|' "$ENTRYPOINT" > "$WORK/tmutant2.sh"
if grep -q "no trap installed" "$WORK/tmutant2.sh"; then
  ok "the trap mutation applied (the line is still spelled the way this sed matches)"
else
  bad "the trap mutation applied" \
    'the trap line is no longer spelled the way this sed matches — re-anchor this mutation rather \
than dropping it, or the case above proves nothing'
fi
ENTRY_ENV=(VIKE_STRATEGY_BUILDER_KEY=selftest-key
  VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT="$WORK")
run_script "$WORK/tmutant2.sh" "$WORK/p6" "$WORK/s1" "$WORK/ib" --config "$p6_profile"
ENTRY_ENV=()
mut_termed="$(LC_ALL=C sort "$WORK/termed" 2> /dev/null | tr '\n' ' ')"
if [ "$mut_termed" != "backtest strategy-builder trade " ]; then
  ok "...and a trap-less supervisor forwards SIGTERM to NOBODY (mutant termed: [$mut_termed])"
else
  bad "...and a trap-less supervisor forwards SIGTERM to NOBODY" \
    "the mutant still stopped every child, so the check above is passing for another reason"
fi
# Nothing forwarded the signal, so the mutant's children are still running and orphaned — which IS
# the finding. Reap them rather than leaving them to outlive the suite's own scratch directory.
reap_orphans

# =================================================================================================
section "a child that dies takes the container with it"
# =================================================================================================
#
# ⚠ A container sitting apparently-healthy with a DEAD trading daemon inside it is the
# container-shaped twin of "every venue silently on paper" — the failure class this whole repository
# is organised against. So any child exiting must fail the container, and the survivors must be
# stopped GRACEFULLY on the way out: a crashed `backtest` must not cost `trade` its cancel sweep.

make_image_bin "$WORK/ibdie" 0
make_daemon_stub "$WORK/ibdie/backtest" backtest 1 17
# ⚠ `AWAIT_STOP=0`: this case must let the container fail ON ITS OWN. Stopping it the moment it
# announces itself would make the run a REQUESTED stop before the child ever died — exit 0, every
# assertion below satisfied by a code path that never ran.
AWAIT_STOP=0
run_start "$WORK/p6" "$WORK/s1" "$WORK/ibdie"
die_rc=$?
AWAIT_STOP=1
if [ "$die_rc" -ne 0 ]; then
  ok "a dead child makes the container exit NON-ZERO (status $die_rc)"
else
  bad "a dead child makes the container exit NON-ZERO" \
    "exit 0 — the container would sit 'healthy' with a process missing: $(cat "$WORK/out.log")"
fi
check "...carrying the dead child's own status" "$die_rc" "17"
# ⚠ **KEYED ON THE FATAL LINE, and the first draft was not.** It grepped the whole log for
# `backtest`, which every healthy run already contains (`started backtest (pid …)`) — so it passed
# against a supervisor that reported `child '?'`, and a REAL container run on the CI box is what caught
# that. An assertion that cannot fail for its stated reason is worse than no assertion, because it
# is counted as coverage.
if grep -q "FATAL: child 'backtest'" "$WORK/out.log"; then
  ok "...and the FATAL line NAMES which child died"
else
  bad "...and the FATAL line NAMES which child died" \
    "no \"FATAL: child 'backtest'\" in the log — a supervisor that cannot say WHICH process died \
sends an operator to read three daemons' output to find out: $(grep FATAL "$WORK/out.log" || cat "$WORK/out.log")"
fi
if grep -q "^trade$" "$WORK/termed" 2> /dev/null; then
  ok "...and the survivors were stopped GRACEFULLY, not abandoned to SIGKILL"
else
  bad "...and the survivors were stopped GRACEFULLY, not abandoned to SIGKILL" \
    "termed: [$(cat "$WORK/termed" 2> /dev/null | tr '\n' ' ')] — trade never got SIGTERM, so its \
resting-order cancel sweep never ran"
fi

# ⚠ THE MUTATION. Make the supervisor return 0 whatever happened and prove the cases above turn
# red. Without it, a supervisor that always exited 0 — the exact regression worth catching — would
# satisfy a suite that only ever asserted "the start did not hang".
sed 's|^  return "$rc"$|  return 0|' "$ENTRYPOINT" > "$WORK/dmutant.sh"
if grep -q '^  return 0$' "$WORK/dmutant.sh"; then
  ok "the dead-child mutation applied"
else
  bad "the dead-child mutation applied" \
    'the supervisor failing return is no longer spelled the way this sed matches — re-anchor the \
mutation rather than dropping it'
fi
AWAIT_STOP=0
run_script "$WORK/dmutant.sh" "$WORK/p6" "$WORK/s1" "$WORK/ibdie" --config "$p6_profile"
dmut_rc=$?
AWAIT_STOP=1
if [ "$dmut_rc" -eq 0 ]; then
  ok "...and a supervisor that swallows the failure exits 0 (so the check above is load-bearing)"
else
  bad "...and a supervisor that swallows the failure exits 0" \
    "the mutant still failed, so the cases above may be passing for another reason"
fi


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
