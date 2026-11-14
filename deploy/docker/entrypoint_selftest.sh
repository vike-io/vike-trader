#!/usr/bin/env bash
# entrypoint_selftest.sh — exercise `deploy/docker/entrypoint.sh` on a scratch tree, with no Docker
# daemon and no image.
#
#     ./deploy/docker/entrypoint.sh --selftest        # the normal way in
#     ./deploy/docker/entrypoint_selftest.sh <path>   # what that execs
#
# ## Why this file exists
#
# **Nothing a PR runs can `docker build` this image, let alone run it** — and the reason is the
# runner ACCOUNT, not the box.
#
# ⚠ What stood here: *"**No runner in this repository has a container runtime.** MEASURED on the CI box
# 2026-08-24 … `which docker podman buildah nerdctl` finds none of them and `/var/run/docker.sock`
# does not exist, while the same probe for `bash sha256sum install` answers with all three."* True
# when measured, **false now.** MEASURED 2026-09-23 and re-measured 2026-09-25: `/usr/bin/docker`
# exists on the CI box and on the latency box, both daemons answer, the socket is there `root:docker`, and the CI box's
# image store holds every released image (`vike-tradehub` and `vike-thin`, 0.1.16 through 0.1.34,
# GHCR and Docker Hub names alike). Treat the versions as dated evidence; the durable fact is that a
# runtime is present and the shipped images were built there.
#
# What survives is the part this file actually needs: `the CI user` is in no `docker` group on either
# box, so a job reaches the daemon only through `sudo -n /usr/local/sbin/vike-image-build` — fixed
# verbs, granted on the CI box only, held by `.github/workflows/release-image.yml` and by nothing else.
# No PR job, and no `cargo test` anywhere, can build or run this image.
#
# ⚠ The superseded version was load-bearing in the wrong direction: "no box has a runtime" makes a
# hole in the image build's own smokes look theoretical. It was not — an unsmoked image reached both
# registries as `:latest` (fixed in #2142).
#
# That would leave the entrypoint in the worst category this repo knows: an uncompiled artifact
# nothing checks. But almost everything the entrypoint DOES is pure shell over a directory — the
# content stamp, the verification, the wholesale swap, the refusals — and none of that needs a
# container. So the logic is driven HERE, against the REAL script, through its real `main`; only
# `FROM`/`COPY`/`ENV` remain unexecutable, and `crates/vike-ops/tests/container_deploy/container_image_gate.rs`
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
  # `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs` pins positionally. But a fixture that races turns a
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
# places. `tradehub` is the daemon verb since 2026-11-06 (it was `trade` from 2026-09-09, `tradehub`
# for the hours between the prefix drop and that rename, and `vike-tradehub` before that); `backtesthub` and
# `strategy-builder` joined when the image became one container running three processes; `vike-cli`
# deliberately kept its prefix. If any name drifts from `deploy/docker/entrypoint.sh`, that script
# spawns a path that does not exist — which is the whole point of this fixture, so the two files
# must always be edited together.
#
# ⚠ Every stub writes `pid.<name>` and `args.<name>`, and the refusal cases below assert "…and the
# daemon never started" by the ABSENCE of `pid.tradehub`. Those cases read `daemon.pid` before the
# supervisor landed; the file is per-child now because there are three children and "did the daemon
# start" stopped being a question with one answer.
make_image_bin() {
  local dir="$1" cli_exit="$2"
  rm -rf "$dir"
  mkdir -p "$dir"
  # ⚠ `secrets list` is the one verb the stub ANSWERS rather than only records: the strategy builder's
  # key is a row of the settings database (the owner's ruling of 2026-10-31), and the supervisor asks
  # the product whether the row exists. The real verb prints key NAMES, one per line behind two
  # spaces, and never a value; the stub prints the row's name while the marker file `builder-key-row`
  # exists (`store_holds_builder_key` / `store_lacks_builder_key` below), and nothing otherwise.
  # While `store-fails` exists the read FAILS the way an unreadable store does — exit 1, a reason on
  # stderr, nothing on stdout — the number of times it holds, or every time for `always`.
  cat > "$dir/vike-cli" <<EOF
#!/usr/bin/env bash
echo "stub-vike-cli \$*" >> "$WORK/cli.log"
if [ "\$1 \$2" = "secrets list" ] && [ -f "$WORK/store-fails" ]; then
  left="\$(cat "$WORK/store-fails")"
  if [ "\$left" = always ] || [ "\$left" -gt 0 ]; then
    [ "\$left" = always ] || echo \$((left - 1)) > "$WORK/store-fails"
    echo "error: the settings database could not be opened: database is locked" >&2
    exit 1
  fi
fi
if [ "\$1 \$2" = "secrets list" ] && [ -f "$WORK/builder-key-row" ]; then
  printf '%s\n' '1 secret(s):' '  VIKE_STRATEGY_BUILDER_KEY'
fi
exit $cli_exit
EOF
  chmod 755 "$dir/vike-cli"
  make_daemon_stub "$dir/tradehub" tradehub
  make_daemon_stub "$dir/backtesthub" backtesthub
  make_daemon_stub "$dir/strategy-builder" strategy-builder
}

# A COMPLETE project folder: `settings/`, the one writable directory. `settings/state`'s absence is
# still a Tier-2 refusal; `settings/db/vike.db`'s absence is a WARNING only (0086 — the active
# daemon-profile ROW decides the mount, and a fresh box with none is a legitimate all-paper start,
# same shape as a checkout that has never run `vike-cli secrets init`).
#
# ⚠ **This used to also write `settings/tradehub.toml`, the file `check_project` refused a start
# without.** That Tier-2 arm is GONE with the file itself (0086: `--config` no longer names anything
# this wrapper checks), so a fixture no longer needs one to be "complete" — a leftover
# `settings/tradehub.toml` is harmless clutter now rather than the thing that makes a start possible,
# which is exactly the property [`the_profile_file_no_longer_gates_a_start`] proves.
make_project() {
  local dir="$1"
  rm -rf "$dir"
  mkdir -p "$dir/settings/state"
}

# The (now-ignored) daemon profile path a `--config` argument names — kept only so the cases that
# still pass `--config` for coverage of the RETIRED flag have something to point it at.
profile_in() { printf '%s' "$1/settings/tradehub.toml"; }

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

# The supervisor's poll cadence for every start this file drives (`VIKE_IMAGE_SUPERVISE_NAP`, read
# by the entrypoint's `SUPERVISE_NAP`). Empty means "pass nothing", i.e. the IMAGE'S own default.
#
# ⚠ **Why not the default everywhere: it was 2/3 of this file's wall time.** Every start here ends
# in a stop, and a stop sleeps one whole nap after the children are already gone — one second per
# case at the image's cadence, ~25 cases. MEASURED 2026-10-03 on a the latency box lane: 28.5 s at 1 s,
# 9.6 s at 0.05 s. The cadence decides how SOON the supervisor notices a decision, never which
# decision it takes, so 0.05 s drives every arm of the real loop. The SIGTERM-forwarding case below
# — the one this image exists to get right — still runs at the image's default, so the shipped
# cadence is executed here too and not only pinned as text.
SELFTEST_NAP=0.05

# Whether a start this file drives runs `check_project`'s append-atomicity probe. Off (`VIKE_SKIP_APPEND_PROBE=1`,
# the knob the entrypoint documents) for every case but the one that needs it.
#
# ⚠ **Why: the probe was ~3,100 of this file's ~4,200 process spawns.** It is four subshells taking `flock` 25
# times each (two `flock` execs per append), and ~16 starts below went through it for nothing: a refusal, an
# argument or a child's death has no use for the filesystem's locking. MEASURED 2026-10-18 on prod lanes: 4,200
# spawns and 11-14 s plain became 1,200 and 7.5 s, and 20 copies of this file at once on two cores took a median
# 76 s before and 28 s after (CI's nextest killed it at 240 s after 131-139 s under that kind of load) — a lock
# hand-off between four processes is a scheduler round trip each, which is what a contended box makes slow.
# The probe itself is still driven directly (section "the append-atomicity probe", every verdict), and
# `SELFTEST_PROBE=1` keeps it in `main` for the ONE case that is about `main` having run it: the healthy start that
# must report no phantom death from the probe's stale job-table entries.
SELFTEST_PROBE=0

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
  for n in tradehub backtesthub strategy-builder; do
    p="$(cat "$WORK/pid.$n" 2> /dev/null)" || continue
    [ -n "$p" ] && kill -TERM "$p" 2> /dev/null
  done
  return 0
}

# Run a SCRIPT (the real entrypoint, or one of the mutants below) against scratch directories.
run_script() {
  local script="$1" project="$2" stage="$3" imagebin="$4"
  shift 4
  rm -f "$WORK/args.tradehub" "$WORK/pid.tradehub" "$WORK/args.backtesthub" "$WORK/pid.backtesthub" \
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
  local nap_env=()
  [ -n "$SELFTEST_NAP" ] && nap_env=(VIKE_IMAGE_SUPERVISE_NAP="$SELFTEST_NAP")
  local probe_env=()
  [ "$SELFTEST_PROBE" = 1 ] || probe_env=(VIKE_SKIP_APPEND_PROBE=1)
  env "${ENTRY_ENV[@]}" "${nap_env[@]}" "${probe_env[@]}" \
    VIKE_SETTINGS_DIR="$project/settings" \
    VIKE_IMAGE_STAGE_DIR="$stage" \
    VIKE_IMAGE_BIN_DIR="$imagebin" \
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

# ⚠ The two RETIRED variables the strategy builder used to take its key from are unset for the WHOLE
# suite, deliberately and explicitly: nothing reads them any more (the key is a row of the settings
# database), and a case below plants them on purpose to prove it — inheriting a value from whoever
# ran this script would turn those into assertions about that person's shell.
unset VIKE_STRATEGY_BUILDER_KEY VIKE_STRATEGY_BUILDER_KEY_FILE
# The settings database "holds" the builder's key row exactly while this marker exists — the stub
# `vike-cli secrets list` answers from it. Absent for the whole suite until a case files it.
store_holds_builder_key() { : > "$WORK/builder-key-row"; }
store_lacks_builder_key() { rm -f "$WORK/builder-key-row"; }
store_lacks_builder_key
# …and the cadence override, for the same reason: the one case that runs at the image's default
# must not inherit a value from whoever ran this script.
unset VIKE_IMAGE_SUPERVISE_NAP

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
if [ ! -f "$WORK/pid.tradehub" ]; then ok "...and the daemon never started"; else
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
if [ ! -f "$WORK/pid.tradehub" ]; then ok "...and the daemon never started"; else
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
section "the project folder must be able to START the daemon"
# =================================================================================================
#
# ⚠ ONE refusal remains, and a WARNING replaced the second (decision 0086).
#
#   * `settings/state` — `check_mount` NAMED it in its message and never checked it. Without it
#     `crates/vike-boot/src/halt.rs`'s `halt_path_arming_error` finds no parent directory and
#     the daemon logs 'HALT KILL SWITCH IS NOT ARMABLE', reached even on a paper mount: the kill
#     switch is dead until somebody reaches for it. The native reference refuses the same case —
#     systemd will not start a unit whose `ReadWritePaths=` names a missing path (status=226).
#   * the daemon PROFILE, the file the image's own CMD used to name, is GONE as a Tier-2 REFUSAL:
#     0086 retires reading one from any binary, so `check_project` no longer stats a `--config`
#     argument at all — it WARNS (never refuses) when `settings/db/vike.db` itself is absent, since
#     that is a legitimate all-paper first run, the same shape as a checkout that has never run
#     `vike-cli secrets init`.
#   * and the surviving refusal is still preceded by `vike-cli config check` printing
#     '0 error(s), 0 warning(s)', which reads as a clean bill for a box that cannot start. The cure
#     is ORDER: the refusal comes first, so on a broken box the green report is never printed at
#     all. `cli.log` is how that is proved — the stub records every invocation.

make_image_bin "$WORK/ib" 0

# ── the profile file no longer gates a start ──────────────────────────────────────────────────
make_project "$WORK/p8"
rm -f "$WORK/p8/settings/tradehub.toml"
run_start "$WORK/p8" "$WORK/s1" "$WORK/ib"
if [ "$?" -eq 0 ]; then
  ok "the_profile_file_no_longer_gates_a_start: a missing daemon-profile FILE no longer refuses"
else
  bad "the_profile_file_no_longer_gates_a_start" \
    "exit $? — check_project must not stat a --config argument any more (0086): $(cat "$WORK/out.log")"
fi
if [ -f "$WORK/pid.tradehub" ]; then ok "...and the daemon actually started"; else
  bad "...and the daemon actually started" "no pid.tradehub — something else is still refusing"
fi

# The `--config=PATH` spelling is the daemon's other accepted (and equally IGNORED) form, and must
# not trip this wrapper either.
run_entrypoint "$WORK/p8" "$WORK/s1" "$WORK/ib" "--config=$(profile_in "$WORK/p8")"
check "the --config=PATH spelling still starts (its value is ignored, not checked)" "$?" "0"

# …and arguments that name NO profile at all are not this wrapper's business either way: the
# daemon's own parser answers for them.
run_entrypoint "$WORK/p8" "$WORK/s1" "$WORK/ib" --some-other-flag
check "arguments with no --config are left to the daemon" "$?" "0"

# ── settings/db/vike.db absent: WARN, never refuse ────────────────────────────────────────────
make_project "$WORK/p8b"
: > "$WORK/cli.log"
run_start "$WORK/p8b" "$WORK/s1" "$WORK/ib"
check "a missing settings database is not refused by the wrapper (the daemon's own refusal decides)" "$?" "0"
if grep -q "bootstrap-daemon" "$WORK/out.log"; then
  ok "...and the pre-flight names the cure rather than staying silent"
else
  bad "...and the pre-flight names the cure" "$(cat "$WORK/out.log")"
fi
# The STORE step of that cure is `secrets init` — the empty store — and no longer
# `secrets template` redirected into the credential file and then migrated, which filed the blank grid as
# blank credential rows. Both directions, so a revert to the old sequence is a red line here.
if grep -q "secrets init" "$WORK/out.log"; then
  ok "...and its store step is 'secrets init'"
else
  bad "...and its store step is 'secrets init'" "$(cat "$WORK/out.log")"
fi
if grep -q "secrets template" "$WORK/out.log"; then
  bad "...and it no longer redirects the blank template into the credential file" "$(cat "$WORK/out.log")"
else
  ok "...and it no longer redirects the blank template into the credential file"
fi

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

# The mutation: drop the settings/state arm from `check_project` and prove the case above turns
# green, i.e. that it is testing the arm rather than something else that happens to refuse. This
# replaces the old profile-arm mutation, which mutated a check that no longer exists (0086).
sed 's|^  if \[ ! -d "$settings/state" \]; then|  if false; then|' \
  "$ENTRYPOINT" > "$WORK/smutant.sh"
make_project "$WORK/p11"
rmdir "$WORK/p11/settings/state"
run_script "$WORK/smutant.sh" "$WORK/p11" "$WORK/s1" "$WORK/ib" \
  --config "$(profile_in "$WORK/p11")"
if [ "$?" -eq 0 ]; then
  ok "...and removing the settings/state arm makes a missing one start again (the check is load-bearing)"
else
  bad "...and removing the settings/state arm makes a missing one start again" \
    "the mutant still refused, so the case above may be passing for another reason: \
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
SELFTEST_PROBE=1
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
start_rc=$?
SELFTEST_PROBE=0
check "a supervised start stops cleanly on SIGTERM" "$start_rc" "0"

# ⚠ The container's arguments and NOTHING of the image's: the consent to a public node-server bind is
# the operator's (decision 0280, the owner's ruling of 2026-11-12). A baked `--allow-public-bind` let
# one `config.tradehub_addr = 0.0.0.0:7879` row open order sending on every interface of a
# `--network host` box.
check "the trading daemon receives the container's arguments alone, with no --allow-public-bind of the image's" \
  "$(cat "$WORK/args.tradehub" 2> /dev/null)" "--config $p6_profile"

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

# ⚠ `--addr` WITH NO VALUE, exactly as `deploy/vike-backtesthub.service`'s ExecStart= spells it: the
# FLAG says "become a daemon" and the VALUE is the config.backtest_addr row, else 127.0.0.1:7880.
# A supervisor that passed an address here would put it in two places in one container.
check "the backtesthub is started as a DAEMON, not a one-shot" \
  "$(cat "$WORK/args.backtesthub" 2> /dev/null)" "--addr"

# ⚠ The container's CMD belongs to `tradehub` ALONE. A supervisor that fanned `"$@"` out to all three
# would hand `backtesthub` a `--config` it does not take and the container would die on its own
# arguments.
if [ ! -s "$WORK/args.backtesthub" ] || ! grep -q -- "--config" "$WORK/args.backtesthub"; then
  ok "...and the container's --config was NOT fanned out to it"
else
  bad "...and the container's --config was NOT fanned out to it" \
    "backtesthub got [$(cat "$WORK/args.backtesthub")]"
fi

# =================================================================================================
section "the strategy builder is SKIPPED, loudly, when it cannot start"
# =================================================================================================
#
# ⚠ `crates/vike-strategy-builder/src/builder.rs`'s `run` refuses to start without BOTH its key
# (the `VIKE_STRATEGY_BUILDER_KEY` row of the settings database) and its `--workspace-root` source
# tree, which the supervisor names as `<project>/vendor/vike-strategy-source`; no image can supply
# either — a key the image invented would be a credential nobody holds. Starting it anyway would
# make it exit FAILURE, which under this supervisor takes the CONTAINER down: every existing `docker
# run`, none of which filed a key, would stop working. So it is skipped and the skip is announced.
# See `supervise`'s own `builder_blockers` for the argument.

if [ ! -f "$WORK/pid.strategy-builder" ]; then
  ok "an unconfigured strategy builder is not started"
else
  bad "an unconfigured strategy builder is not started" \
    "it ran, and would have exited FAILURE and taken the container with it"
fi
if grep -q "VIKE_STRATEGY_BUILDER_KEY" "$WORK/out.log" &&
  grep -q "secrets set VIKE_STRATEGY_BUILDER_KEY" "$WORK/out.log"; then
  ok "...and the log names the row that would turn it on, and the one command that files it"
else
  bad "...and the log names the row that would turn it on, and the one command that files it" \
    "$(cat "$WORK/out.log")"
fi
if [ -f "$WORK/pid.tradehub" ] && [ -f "$WORK/pid.backtesthub" ]; then
  ok "...and the other two are unaffected"
else
  bad "...and the other two are unaffected" "started: $(cat "$WORK/started" 2> /dev/null)"
fi

# (The supervisor's argv, continued — placed after the checks above, which read the default start's log.)
# The bridge-network operator's opt-in: `docker run … -p 127.0.0.1:7879:7879 <image> --allow-public-bind`.
# The argument after the image name reaches the trading daemon, and only it.
run_entrypoint "$WORK/p6" "$WORK/s1" "$WORK/ib" --allow-public-bind
check "an operator's --allow-public-bind starts" "$?" "0"
check "...and reaches the trading daemon, as the operator passed it" \
  "$(cat "$WORK/args.tradehub" 2> /dev/null)" "--allow-public-bind"
check "...and no other process" "$(cat "$WORK/args.backtesthub" 2> /dev/null)" "--addr"

# ⚠ HALF-configured must skip too, and this is the case the first draft of the rule got wrong. A
# KEY with no source tree is a service that still refuses — so gating on the key alone would let
# an operator who supplied one of the two take the whole container down, which is a NEW regression
# introduced by the very change meant to avoid one.
store_holds_builder_key
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
half_rc=$?
store_lacks_builder_key
check "a HALF-configured builder still starts the container" "$half_rc" "0"
if [ ! -f "$WORK/pid.strategy-builder" ]; then
  ok "...and the builder is still skipped"
else
  bad "...and the builder is still skipped" "it ran with no workspace root and would have refused"
fi
if grep -qF "no strategy source tree at $WORK/p6/vendor/vike-strategy-source" "$WORK/out.log"; then
  ok "...naming the one that is still missing"
else
  bad "...naming the one that is still missing" "$(cat "$WORK/out.log")"
fi

# ...and with BOTH it really is the third process, handed the tree as its --workspace-root ARGUMENT
# (decision 0111). The tree stays for every case below.
mkdir -p "$WORK/p6/vendor/vike-strategy-source"
store_holds_builder_key
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
both_rc=$?
store_lacks_builder_key
check "a fully-configured builder starts the container" "$both_rc" "0"
if [ -f "$WORK/pid.strategy-builder" ]; then
  ok "...and the builder IS the third process"
else
  bad "...and the builder IS the third process" "started: $(cat "$WORK/started" 2> /dev/null)"
fi
check "...given the project's source tree as its --workspace-root" \
  "$(cat "$WORK/args.strategy-builder" 2> /dev/null)" "--workspace-root $WORK/p6/vendor/vike-strategy-source"

# ⚠ THE OWNER'S RULING (2026-10-31, "all in the SQLite, not files"): the key's two RETIRED homes — the
# variable `-e` used to pass, and the variable naming a mounted secret file — are read by NOTHING, and
# neither is a precondition. A `docker run` that still passes them (the settings database holding no
# row) must get the same loud SKIP as one that passes nothing, never a crash and never a start, and the
# log must name the row and the one command that files it without ever printing a planted value.
rm -f "$WORK/pid.strategy-builder"
ENTRY_ENV=(VIKE_STRATEGY_BUILDER_KEY=selftest-key
  VIKE_STRATEGY_BUILDER_KEY_FILE=/run/secrets/selftest-builder-key)
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
retired_rc=$?
ENTRY_ENV=()
check "the retired key variables still start the container" "$retired_rc" "0"
if [ ! -f "$WORK/pid.strategy-builder" ]; then
  ok "...and the builder is skipped: a retired variable is no key"
else
  bad "...and the builder is skipped: a retired variable is no key" \
    "it ran on a variable nothing reads any more, and the service would have refused it"
fi
if grep -q "settings database holds no VIKE_STRATEGY_BUILDER_KEY row" "$WORK/out.log" &&
  grep -q "secrets set VIKE_STRATEGY_BUILDER_KEY" "$WORK/out.log"; then
  ok "...naming the missing row and the command that files it"
else
  bad "...naming the missing row and the command that files it" "$(cat "$WORK/out.log")"
fi
if grep -q "selftest-key\|selftest-builder-key" "$WORK/out.log"; then
  bad "...and never printing a planted value" "a planted value reached the log"
else
  ok "...and never printing a planted value"
fi

# ⚠ A store that cannot be READ is not a store with no row (the CI box 2026-11-11: a `vikectl up` read the
# store while the other daemons were opening `vike.db`, the read failed with stderr dropped, and a
# builder whose key is filed was skipped as "no row"). `builder_blockers` asks again, and a store that
# stays unreadable is skipped under its OWN reason. `store-fails` is the stub's failure counter.
secrets_lists() { grep -c '^stub-vike-cli secrets list' "$WORK/cli.log"; }

# A read that fails ONCE and then answers: the builder starts.
store_holds_builder_key
printf '1\n' > "$WORK/store-fails"
: > "$WORK/cli.log"
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
flaky_rc=$?
rm -f "$WORK/store-fails"
store_lacks_builder_key
check "a store unreadable ONCE still starts the container" "$flaky_rc" "0"
if [ -f "$WORK/pid.strategy-builder" ]; then
  ok "...and the builder starts: the second read found the row"
else
  bad "...and the builder starts: the second read found the row" "$(cat "$WORK/out.log")"
fi
check "...having asked the store twice" "$(secrets_lists)" "2"

# A store that STAYS unreadable: the builder is skipped, naming the read failure, never "no row".
store_holds_builder_key
printf 'always\n' > "$WORK/store-fails"
: > "$WORK/cli.log"
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
dead_rc=$?
rm -f "$WORK/store-fails"
store_lacks_builder_key
check "a store that stays unreadable still starts the container" "$dead_rc" "0"
if [ ! -f "$WORK/pid.strategy-builder" ]; then
  ok "...and the builder is skipped"
else
  bad "...and the builder is skipped" "it started on a key nobody could read"
fi
if grep -q "cannot read the settings database to look for the VIKE_STRATEGY_BUILDER_KEY row" "$WORK/out.log" &&
  grep -q "last exit 1: error: the settings database could not be opened: database is locked" "$WORK/out.log"; then
  ok "...naming the READ failure and stderr's last line"
else
  bad "...naming the READ failure and stderr's last line" "$(cat "$WORK/out.log")"
fi
if grep -q "holds no VIKE_STRATEGY_BUILDER_KEY row" "$WORK/out.log"; then
  bad "...and never calling it a missing row" "$(cat "$WORK/out.log")"
else
  ok "...and never calling it a missing row"
fi
check "...after asking the store BUILDER_KEY_READ_TRIES times" "$(secrets_lists)" \
  "$(sed -n 's/^readonly BUILDER_KEY_READ_TRIES=\([0-9]*\)$/\1/p' "$ENTRYPOINT")"

# A store that ANSWERS with no row: the old reason, word for word, asked once — a clean answer is
# never retried.
: > "$WORK/cli.log"
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
norow_rc=$?
check "a readable store with no row still starts the container" "$norow_rc" "0"
if [ ! -f "$WORK/pid.strategy-builder" ] &&
  grep -q "NOT starting strategy-builder: the settings database holds no VIKE_STRATEGY_BUILDER_KEY row\. " "$WORK/out.log" &&
  ! grep -q "cannot read the settings database" "$WORK/out.log"; then
  ok "...and the builder is skipped under the unchanged 'no row' reason"
else
  bad "...and the builder is skipped under the unchanged 'no row' reason" "$(cat "$WORK/out.log")"
fi
check "...having asked the store once" "$(secrets_lists)" "1"

# =================================================================================================
section "SIGTERM reaches EVERY child — the guarantee \`exec\` used to buy"
# =================================================================================================
#
# ⚠ **THE most dangerous thing this file checks.** `docker stop` sends SIGTERM to PID 1, which is
# now a shell — and a shell does NOT forward it. Without `supervise`'s trap the container hangs
# until `--stop-timeout`, SIGKILL wins, `cancel_orders_on_shutdown` never runs and the resting book
# is ABANDONED at the venue. That is the defect `crates/vike-ops/tests/container_deploy/graceful_stop_pin.rs` exists
# to keep fixed, reached through the container instead of through the unit file.
#
# Every stub records the SIGTERM it received, so this is a positive check on each child rather than
# an inference from the container's exit status.

# ⚠ At the IMAGE'S OWN cadence (`SELFTEST_NAP=""` passes no override): this is the case the
# container exists to get right, so it is the one that runs exactly as a container runs.
store_holds_builder_key
SELFTEST_NAP=""
run_start "$WORK/p6" "$WORK/s1" "$WORK/ib"
stop_rc=$?
SELFTEST_NAP=0.05
store_lacks_builder_key
check "a stopped container exits 0 — a requested stop is not a failure" "$stop_rc" "0"
termed="$(LC_ALL=C sort "$WORK/termed" 2> /dev/null | tr '\n' ' ')"
check "all three children received SIGTERM" "$termed" "backtesthub strategy-builder tradehub "

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
store_holds_builder_key
run_script "$WORK/tmutant2.sh" "$WORK/p6" "$WORK/s1" "$WORK/ib" --config "$p6_profile"
store_lacks_builder_key
mut_termed="$(LC_ALL=C sort "$WORK/termed" 2> /dev/null | tr '\n' ' ')"
if [ "$mut_termed" != "backtesthub strategy-builder tradehub " ]; then
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
# stopped GRACEFULLY on the way out: a crashed `backtesthub` must not cost `tradehub` its cancel sweep.

make_image_bin "$WORK/ibdie" 0
make_daemon_stub "$WORK/ibdie/backtesthub" backtesthub 1 17
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
# `backtesthub`, which every healthy run already contains (`started backtesthub (pid …)`) — so it passed
# against a supervisor that reported `child '?'`, and a REAL container run on the CI box is what caught
# that. An assertion that cannot fail for its stated reason is worse than no assertion, because it
# is counted as coverage.
if grep -q "FATAL: child 'backtesthub'" "$WORK/out.log"; then
  ok "...and the FATAL line NAMES which child died"
else
  bad "...and the FATAL line NAMES which child died" \
    "no \"FATAL: child 'backtesthub'\" in the log — a supervisor that cannot say WHICH process died \
sends an operator to read three daemons' output to find out: $(grep FATAL "$WORK/out.log" || cat "$WORK/out.log")"
fi
if grep -q "^tradehub$" "$WORK/termed" 2> /dev/null; then
  ok "...and the survivors were stopped GRACEFULLY, not abandoned to SIGKILL"
else
  bad "...and the survivors were stopped GRACEFULLY, not abandoned to SIGKILL" \
    "termed: [$(cat "$WORK/termed" 2> /dev/null | tr '\n' ' ')] — tradehub never got SIGTERM, so its \
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

# =================================================================================================
section "the supervisor's cadence is the image's one second unless this file asks"
# =================================================================================================
#
# Every start above but one ran at `SELFTEST_NAP`, so the override is what most of this file
# exercises — which makes its two edges worth pinning by execution: that NOT asking yields the
# image's one-second poll, and that a value `sleep` would mishandle cannot reach PID 1's wait.
# `sleep 0`, or a `sleep` refusing a garbled argument, returns at once, and the supervise loop
# would then spin a core for the container's whole life with nothing in the log to say so.
#
# Lifted out textually, the way the probe section below lifts `append_atomicity_ok` — never by
# sourcing the script, which ends in `main "$@"`.
sed -n '/^readonly SUPERVISE_NAP_ASKED=/,/^fi$/p' "$ENTRYPOINT" > "$WORK/nap.fn"
nap_for() {
  (
    if [ -n "$1" ]; then export VIKE_IMAGE_SUPERVISE_NAP="$1"; else unset VIKE_IMAGE_SUPERVISE_NAP; fi
    . "$WORK/nap.fn"
    printf '%s' "${SUPERVISE_NAP:-<unset>}"
  )
}
check "with no override the supervisor polls once a second, as the image always has" "$(nap_for '')" "1"
check "the override is honoured when it is a positive number" "$(nap_for 0.05)" "0.05"
for bad in 0 0.0 00 -1 abc 1e3 ' ' '1;2'; do
  check "an unusable cadence [$bad] falls back to one second, never to a busy loop" \
    "$(nap_for "$bad")" "1"
done

section "the append-atomicity probe"

# ⚠ WHAT THIS PROBE NOW MEASURES. `crates/vike-change-journal/src/lib.rs`'s `append_record` takes
# an exclusive flock around its open/write/sync, so a plain-`>>` probe would refuse a filesystem the
# daemon handles correctly AND would have stopped testing the real write pattern. The probe is
# therefore the flock-SERIALISED shape, and what it still catches is the one case no code change
# rescues: a filesystem whose locking is a no-op, so serialising does not serialise.
#
# ⚠ WHAT THIS FILE CAN AND CANNOT TEST. Reproducing the LOSS needs a filesystem that loses
# concurrent appends, and the only one to hand is a Docker Desktop host bind mount on Windows/macOS
# — which no CI box has, because no CI box is Windows or macOS. (⚠ This parenthetical read "there is
# no container runtime on any of them, measured", which is false: both self-hosted boxes run Docker
# — see this file's header. The claim that holds is about the HOST OS, and a Linux runner's runtime
# cannot produce the host-passthrough filesystem this needs.) So the loss
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
section "the image supervises no gateway and no data daemon (decisions 0164 D11 and 0026)"
# =================================================================================================
#
# TEXT ONLY, and on purpose. Wave 2 makes `tradehub` a CLIENT of the `gatewayhub` (every demo/live venue
# session) and of the `datahub` (its strategy lane). The container stays the PAPER and BACKTEST image:
# it runs neither daemon, so a demo/live account in its store is NOT RUNNING `GatewayUnavailable`
# after the bounded connect wait, never a paper client in its place (decision 0156). Nothing here can
# run a gateway, so what is pinned is the shipped script's text: the child list is exactly the three
# names the header promises, and no CODE line starts, names or reserves a child for either daemon.
# The first line of defence is `container_image_gate.rs` (it holds the three names against the
# Dockerfile and the release staging); this section is the one that fails in the entrypoint's own file
# when somebody adds the fourth.

# Code lines (comments dropped) that give a child to a gateway or data daemon.
daemon_children() {
  grep -v -E '^[[:space:]]*#' "$1" |
    grep -i -E '^[[:space:]]*(readonly[[:space:]]+CHILD_|start_child[[:space:]]+")[^#]*(gatewayhub|datahub|gateway)' || true
}
child_names() { grep -E '^readonly CHILD_[A-Z]+=' "$1" | sed -E 's/^readonly CHILD_[A-Z]+="?([^"]*)"?$/\1/' | LC_ALL=C sort | tr '\n' ' '; }
# Counted inside `supervise` alone: that is the paper default. The server install's `start_server_daemon`
# (decision 0260) starts its roster through the same `start_child`, by a rule row, and its own section
# below drives it.
child_starts() {
  awk '/^supervise\(\) \{$/ { f = 1; next } f && /^\}$/ { f = 0 } f' "$1" |
    grep -v -E '^[[:space:]]*#' | grep -c -E '^[[:space:]]+start_child "' || true
}

check "the entrypoint names exactly three children, and they are the paper and backtest trio" \
  "$(child_names "$ENTRYPOINT")" "backtesthub strategy-builder tradehub "
check "...and starts exactly three" "$(child_starts "$ENTRYPOINT")" "3"
check "...and no code line gives a child to the gateway or the data daemon" \
  "$(daemon_children "$ENTRYPOINT")" ""
if grep -q 'GatewayUnavailable' "$ENTRYPOINT" && grep -q 'LIVE TRADING IS A SERVER INSTALL' "$ENTRYPOINT"; then
  ok "...and says in the file what a demo or live account does in the image (GatewayUnavailable, a server install)"
else
  bad "...and says in the file what a demo or live account does in the image" \
    "the D11 comment block above the child list is gone: an operator would discover the outcome instead of reading it"
fi

# THE MUTATIONS, one per direction, so the detector above is shown to be able to say no. A fourth
# child named for the gateway, and a start of one, must each be caught in a copy of the real script.
sed 's|^readonly CHILD_BUILDER="strategy-builder"$|&\nreadonly CHILD_GATEWAYHUB="gatewayhub"|' \
  "$ENTRYPOINT" > "$WORK/gwmutant1.sh"
check "the mutation that adds a gateway child NAME is caught by the name list" \
  "$(child_names "$WORK/gwmutant1.sh")" "backtesthub gatewayhub strategy-builder tradehub "
if [ -n "$(daemon_children "$WORK/gwmutant1.sh")" ]; then
  ok "...and by the code-line detector"
else
  bad "...and by the code-line detector" "a readonly CHILD_GATEWAYHUB line went unnoticed"
fi
sed 's|^  start_child "\$CHILD_BACKTESTHUB" --addr$|&\n  start_child "datahub" --record|' \
  "$ENTRYPOINT" > "$WORK/gwmutant2.sh"
check "the mutation that STARTS a data daemon is caught by the start count" \
  "$(child_starts "$WORK/gwmutant2.sh")" "4"
if [ -n "$(daemon_children "$WORK/gwmutant2.sh")" ]; then
  ok "...and by the code-line detector"
else
  bad "...and by the code-line detector" 'a `start_child "datahub"` line went unnoticed'
fi

# =================================================================================================
section "the server install's --server (decision 0260): every daemon, each under its unit's rules"
# =================================================================================================
#
# The six daemons are PLAN-driven stubs: each start takes the next line of `plan.<name>` — `exit N`
# (die with N), `hang` (ignore SIGTERM) or nothing (run until SIGTERM, recording it in srv.termed). The
# logic is driven on a copy of the real script whose rule rows say 1 s and 2 s instead of 5 s and the
# units' stop windows, so a case takes seconds; the SHIPPED numbers are held against the units by
# crates/vike-ops/tests/container_deploy/deploy_layout_gate/server_container.rs, and one case below runs
# the real script to see its 5 s.
make_server_bin() {
  local dir="$1" cli_exit="$2" name
  rm -rf "$dir"
  mkdir -p "$dir"
  cat > "$dir/vike-cli" <<EOF
#!/usr/bin/env bash
echo "stub-vike-cli \$*" >> "$WORK/cli.log"
# The shipped-file seed is the REAL binary's when one is named (section 7), never a reimplementation here.
if [ "\$1" = init ] && [ -n "\${VIKE_SELFTEST_CLI:-}" ]; then exec "\$VIKE_SELFTEST_CLI" "\$@"; fi
if [ "\$1 \$2" = "secrets list" ] && [ -f "$WORK/builder-key-row" ]; then
  printf '%s\n' '1 secret(s):' '  VIKE_STRATEGY_BUILDER_KEY'
fi
if [ "\$1 \$2" = "secrets init" ]; then mkdir -p "\$VIKE_SETTINGS_DIR/db" && : > "\$VIKE_SETTINGS_DIR/db/vike.db"; fi
if [ "\$1 \$2" = "config check" ]; then exit $cli_exit; fi
exit 0
EOF
  # The multicall: the supervisor spawns `vike-backend <verb>` (the host has no per-verb links), and the
  # pager is its `notify` verb.
  cat > "$dir/vike-backend" <<EOF
#!/usr/bin/env bash
name="\$1"
if [ "\$name" = notify ]; then
  shift
  printf '%s\n' "\$*" >> "$WORK/paged"
  exit 0
fi
plan="$WORK/plan.\$name"
step=""
if [ -s "\$plan" ]; then
  step="\$(head -n 1 "\$plan")"
  tail -n +2 "\$plan" > "\$plan.next" && mv -f "\$plan.next" "\$plan"
fi
# Which daemons already listened when this one started. Written whole BEFORE the start is counted: a check that
# waits on \`srv.started\` then reads this file read it empty under load (CI run 42593857914, "telegram started
# once the tradehub listened ... got []").
ls "$WORK"/srv.listening.* 2> /dev/null | sed 's#.*/srv.listening.##' | tr '\n' ' ' > "$WORK/srv.saw.\$name.part"
mv -f "$WORK/srv.saw.\$name.part" "$WORK/srv.saw.\$name"
printf '%s\n' "\$name" >> "$WORK/srv.started"
printf '%s\n' "\$*" >> "$WORK/srv.args"
case "\$step" in
  exit\ *) sleep 0.2; exit "\${step#exit }" ;;
  hang) trap '' TERM; printf '%s\n' "\$name" >> "$WORK/srv.ready"; while :; do sleep 1; done ;;
  # This very process (the pid the supervisor holds) listens on a TCP port after N seconds, then marks it.
  listen\ *) exec python3 "$dir/listen.py" "\${step#listen }" "$WORK/srv.listening.\$name" ;;
esac
trap 'printf "%s\n" "\$name" >> "$WORK/srv.termed"; exit 143' TERM
printf '%s\n' "\$name" >> "$WORK/srv.ready"
while :; do
  sleep 5 &
  wait \$!
done
EOF
  cat > "$dir/listen.py" << 'EOF'
import socket, sys, time
time.sleep(float(sys.argv[1]))
s = socket.socket()
s.bind(("127.0.0.1", 0))
# The marker BEFORE listen(): the entrypoint starts the next daemon once it sees LISTEN, and that stub reads the markers (CI run 43552280727).
open(sys.argv[2], "w").close()
s.listen(1)
while True:
    time.sleep(1)
EOF
  chmod 755 "$dir"/*
}
FAST_RULES='strategy-builder 1 2 - - no no - -\ngatewayhub 1 2 - 78 no yes 10G -\ndatahub 1 2 - 78 no yes 8G gatewayhub\nbacktesthub 1 2 - - no yes 8G -\ntradehub 1 2 75 78 yes yes 2G gatewayhub,datahub\ntelegram 1 2 - 78 no yes 256M tradehub'
# `$2` is the copy's SERVER_AFTER_WAIT: 0 keeps every case below in the roster's own start order (a held start
# times out at once), the start-order case holds for real.
fast_copy() {
  awk -v rules="$FAST_RULES" -v wait="$2" '
    /^readonly SERVER_RULES=/ { print; print rules; skip = 1; next }
    skip && /^'\''$/ { skip = 0; print; next }
    skip { next }
    /^readonly SERVER_AFTER_WAIT=/ { print "readonly SERVER_AFTER_WAIT=" wait; next }
    { print }
  ' "$ENTRYPOINT" > "$1"
}
fast_copy "$WORK/fast_entrypoint.sh" 0
srv_run() {
  rm -f "$WORK"/srv.* "$WORK/paged" "$WORK/cli.log"
  : > "$WORK/out.log"
  env VIKE_IMAGE_SUPERVISE_NAP=0.05 VIKE_SETTINGS_DIR="$2/settings" VIKE_IMAGE_STAGE_DIR="$WORK/srv-stage" \
    VIKE_IMAGE_BIN_DIR="$3" bash "$1" --server > "$WORK/out.log" 2>&1 &
  ENTRY_PID=$!
}
srv_until() {
  local n=0
  until eval "$1"; do
    n=$((n + 1))
    [ "$n" -gt "${2:-300}" ] && return 1
    sleep 0.1
  done
  return 0
}
srv_stop() {
  local n=0
  kill -TERM "$ENTRY_PID" 2> /dev/null
  while kill -0 "$ENTRY_PID" 2> /dev/null; do
    n=$((n + 1))
    if [ "$n" -gt 600 ]; then
      kill -KILL "$ENTRY_PID" 2> /dev/null
      break
    fi
    sleep 0.1
  done
  wait "$ENTRY_PID" 2> /dev/null
}
started_count() {
  [ -f "$WORK/srv.started" ] || { echo 0; return; }
  grep -cx "$1" "$WORK/srv.started" || true
}
in_order() { tr '\n' ' ' < "$1" 2> /dev/null; }
srv_state() { grep -E "^$1 " "$WORK/srv-proj/settings/state/supervisor.state" 2> /dev/null | cut -d' ' -f2; }

if grep -q '^tradehub 1 2 75 78 yes yes 2G gatewayhub,datahub$' "$WORK/fast_entrypoint.sh" &&
  grep -q '^readonly SERVER_AFTER_WAIT=0$' "$WORK/fast_entrypoint.sh"; then
  ok "the fast copy carries the test rule rows"
else
  bad "the fast copy carries the test rule rows" "the awk splice of SERVER_RULES found nothing to replace"
fi

# ── 1. a crash restarts that daemon alone; 75 restarts; 78 parks and pages; 0 is not restarted ──
make_project "$WORK/srv-proj"
mkdir -p "$WORK/srv-proj/vendor/vike-strategy-source"
store_holds_builder_key
make_server_bin "$WORK/srv-bin" 0
printf 'exit 1\n' > "$WORK/plan.gatewayhub"
printf 'exit 78\n' > "$WORK/plan.datahub"
printf 'exit 75\nexit 78\n' > "$WORK/plan.tradehub"
printf 'exit 0\n' > "$WORK/plan.telegram"
srv_run "$WORK/fast_entrypoint.sh" "$WORK/srv-proj" "$WORK/srv-bin"
if srv_until '[ "$(started_count gatewayhub)" -ge 2 ] && grep -q "vike-tradehub.service" "$WORK/paged" 2> /dev/null' 200; then
  ok "the run reached a restarted gateway and a parked tradehub"
else
  bad "the run reached a restarted gateway and a parked tradehub" "$(tail -20 "$WORK/out.log")"
fi
sleep 2.5
check "the first run created the default settings database (secrets init)" \
  "$([ -f "$WORK/srv-proj/settings/db/vike.db" ] && grep -c 'stub-vike-cli secrets init' "$WORK/cli.log")" "1"
# Decision 0280: every start mints the hub keys the project lacks, after the store exists and before
# any daemon's pre-flight (`config check`, the first thing a daemon's start runs).
check "...and minted the missing hub keys once" \
  "$(grep -c 'stub-vike-cli backend mint-missing' "$WORK/cli.log")" "1"
check "...after the store was created and before the first daemon's pre-flight" \
  "$(awk '/stub-vike-cli secrets init/ && !s {s=NR} /stub-vike-cli backend mint-missing/ && !m {m=NR}
          /stub-vike-cli config check/ && !c {c=NR}
          END {print (s && m > s && c > m) ? "ordered" : "init=" s " mint=" m " check=" c}' "$WORK/cli.log")" \
  "ordered"
# The shipped strategy files are seeded at every start, after the keys and before any daemon (section 7 runs
# the real seed against absent, present and edited files).
check "...and seeded the shipped strategy files once, after the keys and before the first pre-flight" \
  "$(awk '/stub-vike-cli backend mint-missing/ && !m {m=NR} /stub-vike-cli init --strategies-only/ && !i {i=NR}
          /stub-vike-cli config check/ && !c {c=NR}
          END {print (m && i > m && c > i) ? "ordered" : "mint=" m " seed=" i " check=" c}' "$WORK/cli.log")" \
  "ordered"
# IBKR's cash-settled futures roots are seeded at every start too (create-new: the verb leaves an existing table
# alone), after the keys and before any daemon.
check "...and seeded IBKR's cash-settled roots once, after the keys and before the first pre-flight" \
  "$(awk '/stub-vike-cli backend mint-missing/ && !m {m=NR} /stub-vike-cli config ibkr-cash-settled-roots seed/ && !r {r=NR}
          /stub-vike-cli config check/ && !c {c=NR}
          END {print (m && r > m && c > r) ? "ordered" : "mint=" m " roots=" r " check=" c}' "$WORK/cli.log")" \
  "ordered"
# The supervisor's own `started` lines, not the stubs' records: six processes write those concurrently.
check "the roster starts in DEPLOY_UNITS order" \
  "$(grep -o '^vike-entrypoint: started [a-z-]*' "$WORK/out.log" | head -n 6 | cut -d' ' -f3 | tr '\n' ' ')" \
  "strategy-builder gatewayhub datahub backtesthub tradehub telegram "
check "a gateway that exits 1 is restarted, once, after its RestartSec" "$(started_count gatewayhub)" "2"
check "...and alone: the datahub (parked on its 78, a setup refusal) started once" "$(started_count datahub)" "1"
check "...the datahub's state says parked" "$(srv_state datahub)" "parked"
check "...the backtesthub once" "$(started_count backtesthub)" "1"
check "...the strategy builder once" "$(started_count strategy-builder)" "1"
check "the tradehub's 75 restarts it and its 78 parks it: two starts, no third" "$(started_count tradehub)" "2"
check "...the park paged once, for its unit (the datahub's row pages nobody)" "$(cat "$WORK/paged" 2> /dev/null)" "vike-tradehub.service"
check "...and the state says parked" "$(srv_state tradehub)" "parked"
check "a telegram channel that exits 0 is not restarted" "$(started_count telegram)" "1"
check "...and the state says done" "$(srv_state telegram)" "done"
check "the rest keep running" "$(srv_state gatewayhub) $(srv_state backtesthub)" "running running"
if kill -0 "$ENTRY_PID" 2> /dev/null; then
  ok "...and the container did not exit over any of it"
else
  bad "...and the container did not exit over any of it" "$(tail -20 "$WORK/out.log")"
fi
check "the builder runs its unit's arguments, on the release tree" \
  "$(grep -c -x "strategy-builder --port 7881 --out-dir $WORK/srv-proj/user_data/strategy-builds --retain 5 --workspace-root $WORK/srv-proj/vendor/vike-strategy-source" "$WORK/srv.args")" "1"
check "the datahub runs its unit's arguments, the row deciding its store" \
  "$(grep -c -x 'datahub --recorder-profile default' "$WORK/srv.args")" "1"
check "the backtesthub runs --addr, the row deciding the address" "$(grep -c -x 'backtesthub --addr' "$WORK/srv.args")" "1"
# Decision 0280: the server install binds the loopback door `backend mint-missing` writes, which needs no
# consent, so the tradehub gets no `--allow-public-bind` (a public row is refused at its start).
check "the tradehub runs with no arguments, so no public-bind consent, at both its starts" \
  "$(grep -c -x 'tradehub' "$WORK/srv.args")" "2"
srv_stop
check "a docker stop is a clean exit" "$?" "0"
check "...stopping the running daemons in reverse start order" "$(in_order "$WORK/srv.termed")" \
  "backtesthub gatewayhub strategy-builder "

# ── 2. the stop order with every daemon up, and SIGKILL after a daemon's stop window ──
make_server_bin "$WORK/srv-bin" 0
printf 'hang\n' > "$WORK/plan.gatewayhub"
srv_run "$WORK/fast_entrypoint.sh" "$WORK/srv-proj" "$WORK/srv-bin"
if srv_until '[ "$(wc -l < "$WORK/srv.ready" 2> /dev/null || echo 0)" -ge 6 ]' 200; then
  ok "all six daemons up"
else
  bad "all six daemons up" "$(tail -20 "$WORK/out.log")"
fi
srv_stop
check "...stopped one at a time, tradehub before datahub and gateway" "$(in_order "$WORK/srv.termed")" \
  "telegram tradehub backtesthub datahub strategy-builder "
if grep -q 'gatewayhub outlived its 2 s: SIGKILL' "$WORK/out.log"; then
  ok "...and a gateway that ignored SIGTERM was SIGKILLed after its stop window"
else
  bad "...and a gateway that ignored SIGTERM was SIGKILLed after its stop window" "$(tail -20 "$WORK/out.log")"
fi
th="$(grep -n 'stopping tradehub' "$WORK/out.log" | head -n 1 | cut -d: -f1)"
gw="$(grep -n 'stopping gatewayhub' "$WORK/out.log" | head -n 1 | cut -d: -f1)"
if [ -n "$th" ] && [ -n "$gw" ] && [ "$th" -lt "$gw" ]; then
  ok "...the tradehub's stop began before the gateway's"
else
  bad "...the tradehub's stop began before the gateway's" "tradehub line $th, gateway line $gw"
fi

# ── 3. a failed pre-flight holds the daemons that carry one, and is retried ──
make_server_bin "$WORK/srv-bin" 1
srv_run "$WORK/fast_entrypoint.sh" "$WORK/srv-proj" "$WORK/srv-bin"
if srv_until '[ "$(grep -c "pre-flight FAILED before gatewayhub" "$WORK/out.log")" -ge 2 ]' 200; then
  ok "a failed pre-flight is retried after RestartSec"
else
  bad "a failed pre-flight is retried after RestartSec" "$(tail -20 "$WORK/out.log")"
fi
check "...the daemons that carry one never started" \
  "$(started_count gatewayhub)$(started_count datahub)$(started_count tradehub)" "000"
check "...and the builder, whose unit carries none, did" "$(started_count strategy-builder)" "1"
srv_stop

# ── 4. an operator's restart of a parked daemon ──
make_server_bin "$WORK/srv-bin" 0
printf 'exit 78\n' > "$WORK/plan.tradehub"
srv_run "$WORK/fast_entrypoint.sh" "$WORK/srv-proj" "$WORK/srv-bin"
srv_until '[ "$(srv_state tradehub)" = parked ]' 200
printf 'tradehub\n' > "$WORK/srv-proj/settings/state/supervisor.restart"
if srv_until '[ "$(started_count tradehub)" -ge 2 ]' 100; then
  ok "vikectl restart starts a parked tradehub again"
else
  bad "vikectl restart starts a parked tradehub again" "$(tail -20 "$WORK/out.log")"
fi
check "...and consumes the request" "$([ -e "$WORK/srv-proj/settings/state/supervisor.restart" ] && echo left)" ""
srv_stop

# ── 4b. host mode: the programs beside the script, no image directory, the same rules ──
make_server_bin "$WORK/host-stubs" 0
mkdir -p "$WORK/hostbin"
cp "$WORK/fast_entrypoint.sh" "$WORK/hostbin/vike-entrypoint.sh"
cp "$WORK/host-stubs/vike-backend" "$WORK/host-stubs/vike-cli" "$WORK/hostbin/"
rm -f "$WORK"/srv.* "$WORK/paged" "$WORK/cli.log"
: > "$WORK/out.log"
env VIKE_IMAGE_SUPERVISE_NAP=0.05 VIKE_SETTINGS_DIR="$WORK/srv-proj/settings" \
  bash "$WORK/hostbin/vike-entrypoint.sh" --server --host > "$WORK/out.log" 2>&1 &
ENTRY_PID=$!
if srv_until '[ "$(wc -l < "$WORK/srv.ready" 2> /dev/null || echo 0)" -ge 6 ]' 200; then
  ok "--server --host runs the roster from the script's own directory"
else
  bad "--server --host runs the roster from the script's own directory" "$(tail -20 "$WORK/out.log")"
fi
if grep -q 'per-daemon memory caps: off' "$WORK/out.log"; then
  ok "...and says so when its cgroup is not delegated (no per-daemon caps here)"
else
  bad "...and says so when its cgroup is not delegated" "$(tail -20 "$WORK/out.log")"
fi
srv_stop
check "...and stops on SIGTERM like the container" "$?" "0"
mkdir -p "$WORK/hostbin-empty"
cp "$WORK/fast_entrypoint.sh" "$WORK/hostbin-empty/vike-entrypoint.sh"
AWAIT_STOP=0
env VIKE_SETTINGS_DIR="$WORK/srv-proj/settings" bash "$WORK/hostbin-empty/vike-entrypoint.sh" --server --host > "$WORK/out.log" 2>&1
check "--host with no vike-backend beside it refuses" "$?" "1"
AWAIT_STOP=1

# ── 4c. the start order is the units' After= chain, by readiness: a daemon starts once those it is After= listen ──
# (the CI box's first host-mode start, 2026-11-08: the tradehub started before the datahub listened, refused on "price
# feeds unavailable" and came back 5 s later.) Each stub that listens is that very process, as the daemon is.
if command -v python3 > /dev/null 2>&1; then
  fast_copy "$WORK/order_entrypoint.sh" 3
  make_server_bin "$WORK/srv-bin" 0
  printf 'listen 0\n' > "$WORK/plan.gatewayhub"
  printf 'listen 1\n' > "$WORK/plan.datahub"
  printf 'listen 0\n' > "$WORK/plan.tradehub"
  srv_run "$WORK/order_entrypoint.sh" "$WORK/srv-proj" "$WORK/srv-bin"
  if srv_until '[ "$(started_count telegram)" -ge 1 ]' 100; then
    ok "the chain came up"
  else
    bad "the chain came up" "$(tail -20 "$WORK/out.log")"
  fi
  check "the datahub started once the gateway listened" "$(cat "$WORK/srv.saw.datahub" 2> /dev/null)" "gatewayhub "
  check "the tradehub started once the gateway AND the datahub listened (a second after the gateway)" \
    "$(cat "$WORK/srv.saw.tradehub" 2> /dev/null)" "datahub gatewayhub "
  check "telegram started once the tradehub listened" "$(cat "$WORK/srv.saw.telegram" 2> /dev/null)" \
    "datahub gatewayhub tradehub "
  check "...the hold was said, and the tradehub started once" \
    "$(grep -c '^vike-entrypoint: holding tradehub until' "$WORK/out.log") $(started_count tradehub)" "1 1"
  srv_stop
  # The bound: a datahub that never listens holds the tradehub its 3 s, then the tradehub starts anyway.
  make_server_bin "$WORK/srv-bin" 0
  printf 'listen 0\n' > "$WORK/plan.gatewayhub"
  srv_run "$WORK/order_entrypoint.sh" "$WORK/srv-proj" "$WORK/srv-bin"
  if srv_until '[ "$(started_count tradehub)" -ge 1 ]' 100; then
    ok "a dependency that never listens holds the start only for the bound"
  else
    bad "a dependency that never listens holds the start only for the bound" "$(tail -20 "$WORK/out.log")"
  fi
  check "...and says so" \
    "$(grep -c '^vike-entrypoint: tradehub: datahub did not listen within 3 s; starting it anyway' "$WORK/out.log")" "1"
  check "...the tradehub started without the datahub listening" "$(cat "$WORK/srv.saw.tradehub" 2> /dev/null)" "gatewayhub "
  srv_stop
else
  bad "the start-order cases run" "they need python3 to stand in a listener, and this box has none"
fi

# ── 5. the SHIPPED rules: a failed daemon waits its unit's 5 s ──
make_server_bin "$WORK/srv-bin" 0
# The gateway listens at once, so the shipped 60 s hold does not delay the datahub's start here.
printf 'listen 0\n' > "$WORK/plan.gatewayhub"
printf 'exit 1\n' > "$WORK/plan.datahub"
srv_run "$ENTRYPOINT" "$WORK/srv-proj" "$WORK/srv-bin"
srv_until '[ "$(started_count datahub)" -ge 1 ]' 100
sleep 3
check "the shipped script does not restart a failed daemon within 3 s" "$(started_count datahub)" "1"
if srv_until '[ "$(started_count datahub)" -ge 2 ]' 120; then
  ok "...and restarts it once its 5 s are up"
else
  bad "...and restarts it once its 5 s are up" "$(tail -20 "$WORK/out.log")"
fi
srv_stop
store_lacks_builder_key

# ── 7. the shipped strategy files: absent is copied, present and edited are left alone ──
# The REAL seed (`vike-cli init --strategies-only`), which the stub hands over to when VIKE_SELFTEST_CLI names a
# built vike-cli; with none, these cases are skipped and say so (the seed's own tests are vike-strategy's and
# vike-cli's). A start whose seed FAILS goes on: the stub exits 0, and the real one's failure is only a warning.
if [ -z "${VIKE_SELFTEST_CLI:-}" ] || [ ! -x "${VIKE_SELFTEST_CLI:-}" ]; then
  printf '   SKIP the shipped-file cases: set VIKE_SELFTEST_CLI to a built vike-cli to run them\n'
else
  make_project "$WORK/seed-proj"
  shipped="$WORK/seed-proj/vendor/vike-strategies"
  mine="$WORK/seed-proj/strategies"
  mkdir -p "$shipped/trend" "$shipped/other" "$mine/trend" "$mine/other"
  printf '// absent\n' > "$shipped/trend/absent.rs"
  printf '// present\n' > "$shipped/other/present.rs"
  printf '// shipped\n' > "$shipped/trend/edited.rs"
  printf '// present\n' > "$mine/other/present.rs"
  printf '// my edit\n' > "$mine/trend/edited.rs"
  VIKE_SETTINGS_DIR="$WORK/seed-proj/settings" "$VIKE_SELFTEST_CLI" secrets init > /dev/null 2>&1
  make_server_bin "$WORK/srv-bin" 0
  srv_run "$WORK/fast_entrypoint.sh" "$WORK/seed-proj" "$WORK/srv-bin"
  srv_until 'grep -q "serving the roster" "$WORK/out.log"' 200 || bad "the seeded start reached its roster" "$(tail -20 "$WORK/out.log")"
  srv_stop
  check "absent: a shipped file missing from strategies/ is copied" "$(cat "$mine/trend/absent.rs" 2> /dev/null)" "// absent"
  check "present: a file as shipped is left as it is" "$(cat "$mine/other/present.rs")" "// present"
  check "edited: an edited file is never touched" "$(cat "$mine/trend/edited.rs")" "// my edit"
  check "...and the start said what it copied, and what it kept" \
    "$(grep -c -e 'copied trend/absent.rs' -e '1 copied, 2 already there' "$WORK/out.log")" "2"
fi

# ── 6. the refusals ──
AWAIT_STOP=0
run_entrypoint "$WORK/srv-proj" "$WORK/srv-stage" "$WORK/srv-bin" --server extra
check "--server refuses arguments" "$?" "1"
rm -rf "$WORK/srv-proj/settings/state"
run_entrypoint "$WORK/srv-proj" "$WORK/srv-stage" "$WORK/srv-bin" --server
check "--server refuses a folder with no settings/state" "$?" "1"
AWAIT_STOP=1

# =================================================================================================
printf '\n'
if [ "$FAILURES" -eq 0 ]; then
  printf 'entrypoint selftest: PASS (%d cases)\n' "$CASES"
  exit 0
fi

printf 'entrypoint selftest: FAIL (%d of %d cases)\n' "$FAILURES" "$CASES" >&2
exit 1
