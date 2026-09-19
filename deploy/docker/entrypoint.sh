#!/usr/bin/env bash
# entrypoint.sh — PID 1 for the backend image: prove the mounted project folder can actually start
# this daemon, reconcile `<project>/bin` against the toolset this image carries, run the pre-flight
# the systemd unit runs, then `exec` the daemon so it becomes PID 1 itself.
#
#     entrypoint.sh --config /project/settings/tradehub.toml   # the image's CMD; args go to the daemon
#     entrypoint.sh --template                                 # print the daemon-profile template
#     entrypoint.sh --stamp  <dir>                             # write <dir>/.manifest + <dir>/.version
#     entrypoint.sh --verify <dir>                             # check <dir> against its own .manifest
#     entrypoint.sh --selftest                                 # run the sibling selftest, no daemon
#
# ## ⚠ THE MOUNT SHADOWS THE IMAGE — this file exists for that one sentence
#
# The project folder is a BIND MOUNT (`docker run -v /srv/vike-<unit>:/project`), not a baked layer,
# because settings, credentials, state and data must outlive the image
# (`docs/decisions/0026-containerisation-additive-backend-image.md`). The consequence is easy to
# state and easy to forget: **every path that resolves against `<project>` resolves against the
# HOST's directory, so anything this image baked at `<project>/bin/...` is invisible at runtime.**
#
# That is not a cosmetic loss, because the tool ladders do not FAIL when their project rung is
# empty — they fall through. `crates/bridges/dukascopy/src/config.rs`'s `resolve_dukascopy_tools`
# documents both ladders, and the JVM one ends at rung 3: the bare name `java`, looked up on `PATH`.
# A slim image has no `java` on `PATH`, so the venue degrades to paper reporting a program that
# names nothing — a SILENT failure, which is the class this whole repository is organised against.
#
# Baking into `/opt/vike/tools` and COPYING it out at start is what closes that: the image keeps a
# private copy the mount cannot shadow, and this script makes the mounted directory match it.
#
# ## ⚠ THE DAEMON PROFILE IS A FILE NOTHING CREATES, and this image's CMD names it
#
# The image runs `trade --config /project/settings/tradehub.toml` (the multicall link — the
# `vike-tradehub` CRATE keeps its name, the verb that reaches it dropped the prefix), and
# `crates/vike-tradehub/src/main.rs` EXITS when that file is absent — one line, `bad profile <path>:
# No such file or directory`, after a full start sequence. Nothing in this workspace writes that
# file: it decides venue, symbol and strategy, which is the operator's choice and no image's.
#
# For a checkout the answer has always been `cp settings/tradehub.example.toml settings/tradehub.toml`.
# A container operator has no checkout, so the image carries the SAME template bytes at
# [`TEMPLATE_FILE`] and `--template` prints them:
#
#     docker run --rm vike-tradehub:<version> --template > /srv/vike-<unit>/settings/tradehub.toml
#
# ⚠ `--template` writes NOTHING and prints nothing but the template. It is a `cat` to stdout, so it
# composes with `>` — a verb that logged a banner first would corrupt the file it was asked to
# produce, which is why `check_project`'s refusal names this command rather than doing the write
# itself. This image installs `<project>/bin`, and that is the whole of what it puts in an
# operator's project folder; `<project>/settings` stays theirs.
#
# ## The stamp identifies the TOOLSET, not the release
#
# ⚠ A release tag is NOT a safe stamp, and the counter-example already ships: `.github/workflows/
# release.yml` publishes `vike-tradehub-fxcm` beside `vike-tradehub` from ONE tag — the same commit
# built with `--features fxcm`, a different binary. Two images cut from one release can therefore
# carry different payloads, and a tag-equal stamp would make the second image skip its install and
# run on the first one's tools.
#
# So the stamp is a CONTENT digest: `.manifest` is a `sha256sum`-format listing of the staged tree
# and `.version` is that manifest's own digest. Equal stamps then mean identical toolsets **by
# construction** rather than by promise, and the manifest doubles as the verification input — the
# same "stage, verify, then install, never the other way round" discipline
# `scripts/fetch_release_tools.sh` follows.
#
# ⚠ ONE implementation produces AND consumes it. `--stamp` is called at image-build time by
# `scripts/release_container_image.sh`; `--verify` is called here at start. A second copy of the
# format in the build script is how the two would drift into disagreeing about what a stamp is.
#
# ## The swap is WHOLESALE, and interrupting it leaves the old tree intact
#
# A file-by-file copy leaves a previous version's files lingering beside the new ones, which is how
# a tool directory ends up holding two jars and resolving the wrong one. So the install builds a
# COMPLETE new tree beside the target, writes the stamp into it (the stamp is part of the staged
# tree, so it arrives with the copy), VERIFIES it, and only then renames it into place. Every
# failure path before that rename leaves `<project>/bin` exactly as it was, still carrying its old
# stamp — so the next start retries rather than running on a half-installed tree.
#
# ⚠ The rename is two `mv`s, not one, so `<project>/bin` does not exist for an instant between them.
# That is safe HERE and nowhere else: this runs before `exec`, so no daemon has started and nothing
# is reading the directory. Do not lift this function into a running process.
#
# ⚠ **It refuses to swap over a `bin/` this image did not install** (no `.version`, but files
# present). That directory is the operator's — `scripts/fetch_release_tools.sh` installs into it,
# and a native install puts the daemon's own binaries there — and silently deleting it would make
# this image the first thing in the workspace to destroy an operator's files. `VIKE_CONTAINER_ADOPT_BIN=1`
# is the explicit override for somebody who means it.
#
# ## `exec` is load-bearing, not a style choice
#
# The last line must `exec`, so the daemon REPLACES this shell and becomes PID 1. Without it
# the shell is PID 1, and a shell does not forward SIGTERM to its children: `docker stop` would
# wait out its whole timeout and then SIGKILL the daemon, so `cancel_orders_on_shutdown` never runs
# and the resting book is ABANDONED at the venue. That is precisely the defect
# `crates/vike-ops/tests/graceful_stop_pin.rs` exists to keep fixed, reached through the container
# instead of through the unit file. `crates/vike-ops/tests/container_image_gate.rs` holds the `exec`.
#
# ⚠ The residual, stated rather than discovered: PID 1 ignores signals whose disposition is still
# the default, so SIGTERM does nothing until `vike_ops::stop`'s `install_handlers` runs. That call
# is at the TOP of the daemon's `main` (pinned by `graceful_stop_pin.rs`'s
# `the_tradehub_handler_is_installed_before_anything_can_place_an_order`), so the window is startup
# only — and nothing can be resting at a venue inside it, which is the property that makes it
# acceptable.
#
# ## No init process, and that is measured rather than assumed
#
# A `tini`-style reaper earns its place when PID 1 collects orphaned children. A tree-wide
# `Command::new` sweep finds every program this workspace spawns, and the daemon path spawns NONE:
# `crates/bridges/dukascopy/src/exec.rs` (the JForex sidecar) belongs to a venue this image does not
# carry, `crates/vike-backfill/src/clickhouse_poly/ingest.rs` is
# `clickhouse-client` in a binary this image does not ship (and is the only such site left, since
# the research crate's own ClickHouse sink was deleted with it — and since 2026-09-09 the pmxt
# backfill's own `clickhouse-client` spawn went too, with the NOT-idempotent write path that used
# it), `crates/vike-ml/src/cli.rs` is the
# LightGBM trainer, and `crates/vike-buildinfo/build.rs` runs at BUILD time. The one site linked
# into this binary is `crates/vike-run/src/incident.rs`'s `git_build_info`, which the daemon never
# calls and which its own doc declares best-effort — "on a deployed binary far from the repo it
# simply yields `None`". So there are no children to reap and an init would add a process for
# nothing.
set -uo pipefail

# The house style for this directory is `set -uo pipefail` WITHOUT `-e` (unanimous across
# `deploy/**/*.sh`), so every step below reports its own failure through `die` rather than trusting
# an exit status to propagate. That is the right shape here anyway: a daemon that signs real orders
# must not start on a half-checked box, and "which step failed" is the first thing an operator asks.

# The image's two private directories. Overridable ONLY so `entrypoint_selftest.sh` can drive this
# script's REAL `main` against a scratch tree — a selftest that re-implemented the start sequence
# would be testing its own copy, which is the failure mode that makes a gate decorative. Nothing in
# the image sets either, and `crates/vike-ops/tests/container_image_gate.rs` pins the defaults so a
# Dockerfile that staged somewhere else could not silently disagree with them.
readonly STAGE_DIR="${VIKE_IMAGE_STAGE_DIR:-/opt/vike/tools}"
readonly BIN_DIR="${VIKE_IMAGE_BIN_DIR:-/opt/vike/bin}"
readonly MANIFEST_FILE=".manifest"
readonly VERSION_FILE=".version"
# The image's copy of `settings/tradehub.example.toml` — ONE set of bytes, staged by
# `scripts/release_container_image.sh` from the repo's own template so a container operator and a
# checkout are handed the same file. Overridable on the same terms as the two above: the selftest,
# and nothing else.
readonly TEMPLATE_FILE="${VIKE_IMAGE_TEMPLATE_FILE:-/opt/vike/templates/tradehub.example.toml}"
# Where the daemon profile lives once copied, named here only so the refusals can say it. The image
# CMD's `--config` is the authority; this is the spelling every shipped launcher uses.
readonly PROFILE_NAME="tradehub.toml"

# The lead-in on every line this script prints. Distinct from the daemon's own output, which is
# JSON — an operator reading `docker logs` must be able to tell the wrapper from the process.
log() { printf 'vike-entrypoint: %s\n' "$*"; }
die() {
  printf 'vike-entrypoint: FATAL: %s\n' "$*" >&2
  exit 1
}

# =================================================================================================
# THE STAMP — one implementation, used by the image build (`--stamp`) and by this start (`--verify`)
# =================================================================================================

# Write `<dir>/.manifest` (sha256sum format, sorted, paths relative to `<dir>`) and `<dir>/.version`
# (`toolset=sha256:<digest of the manifest>`).
#
# ⚠ The listing is SORTED and the paths are RELATIVE, both for the same reason: the digest must
# depend on the tree's CONTENT and on nothing else. An absolute path would bake the build
# directory's name into the stamp, and `find`'s traversal order is not stable across filesystems —
# either would make two identical toolsets stamp differently and reinstall on every start.
#
# ⚠ The two stamp files are EXCLUDED from the listing they describe: a manifest cannot contain its
# own digest, and including `.version` would make the digest depend on itself.
#
# An EMPTY toolset is a legitimate answer, not an error — see `install_tools`. It produces an empty
# `.manifest` and the well-defined digest of an empty file, so "this image stages nothing" is a
# statement the next start can compare against rather than a case it has to guess at.
stamp_tree() {
  local dir="$1"
  [ -d "$dir" ] || die "--stamp: '$dir' is not a directory"

  local manifest="$dir/$MANIFEST_FILE"
  local version="$dir/$VERSION_FILE"
  rm -f "$manifest" "$version" || die "--stamp: cannot clear previous stamp files in '$dir'"

  # ⚠ The checksums are taken from INSIDE `$dir`, so every path in the manifest is RELATIVE. That is
  # not tidiness — it is what makes the manifest describe a TREE rather than a location:
  #
  #   * the digest must depend on content alone, and an absolute path bakes the build directory's
  #     name into it, so two identical toolsets staged at different paths would stamp differently
  #     and reinstall on every start;
  #   * `verify_tree` runs `sha256sum -c` from the tree it is checking, so absolute paths would
  #     send it to verify the ORIGINAL files instead of the copy in front of it — a verification
  #     that passes on a tree whose every file has been replaced.
  #
  # Both were MEASURED rather than reasoned about: `entrypoint_selftest.sh` failed four cases on the
  # first cut of this function, which built the lines with `sed` over absolute output.
  #
  # `-print0`/`sort -z` so a path containing whitespace or a newline cannot split a record, and
  # `LC_ALL=C` pins the order to bytes so the digest does not depend on the build box's locale.
  # `xargs -r` so an empty tree produces an empty manifest instead of checksumming the CWD.
  (
    cd "$dir" &&
      find . -type f ! -name "$MANIFEST_FILE" ! -name "$VERSION_FILE" -print0 |
      LC_ALL=C sort -z |
      xargs -0 -r sha256sum
  ) > "$manifest" || die "--stamp: cannot build '$manifest'"

  local digest
  digest="$(sha256sum "$manifest" | cut -d' ' -f1)" || die "--stamp: cannot digest '$manifest'"
  printf 'toolset=sha256:%s\n' "$digest" > "$version" || die "--stamp: cannot write '$version'"
  log "stamped $(wc -l < "$manifest" | tr -d ' ') file(s) in '$dir' as sha256:${digest}"
}

# The one compared value. Read from the FIRST line only, so provenance comments could be appended
# later without changing what the comparison means.
stamp_of() {
  local dir="$1"
  [ -f "$dir/$VERSION_FILE" ] || return 1
  head -n 1 "$dir/$VERSION_FILE"
}

# Check a tree against its own `.manifest`. This is the "VERIFY rather than assume" half: a copy
# that reported success is not a copy that landed, and a truncated tool is indistinguishable from a
# complete one until something tries to run it.
#
# An empty manifest verifies as OK and asserts the tree carries no payload — `sha256sum -c` on an
# empty file reports "no properly formatted checksum lines" and exits non-zero, which would read as
# corruption when it actually means "nothing was staged".
verify_tree() {
  local dir="$1"
  local manifest="$dir/$MANIFEST_FILE"
  [ -f "$manifest" ] || return 1
  if [ ! -s "$manifest" ]; then
    # No payload declared — prove none arrived either, so an empty manifest beside a populated
    # directory is caught rather than waved through.
    local strays
    strays="$(cd "$dir" && find . -type f ! -name "$MANIFEST_FILE" ! -name "$VERSION_FILE" |
      head -n 1)"
    [ -z "$strays" ]
    return
  fi
  (cd "$dir" && sha256sum -c --quiet "$MANIFEST_FILE" > /dev/null 2>&1)
}

# =================================================================================================
# THE INSTALL
# =================================================================================================

# Does `<project>/bin` hold anything at all (ignoring the two stamp files)?
bin_has_payload() {
  local dir="$1"
  [ -d "$dir" ] || return 1
  local first
  first="$(cd "$dir" && find . -type f ! -name "$MANIFEST_FILE" ! -name "$VERSION_FILE" |
    head -n 1)"
  [ -n "$first" ]
}

# Reconcile `<project>/bin` with `/opt/vike/tools`. See the header for the swap's shape and for why
# an unstamped directory is refused rather than replaced.
install_tools() {
  local project="$1"
  local target="$project/bin"

  [ -f "$STAGE_DIR/$VERSION_FILE" ] ||
    die "the image is malformed: '$STAGE_DIR/$VERSION_FILE' is missing. It is written at build \
time by 'entrypoint.sh --stamp' (scripts/release_container_image.sh)."

  # ── The empty toolset: install nothing, and touch nothing ──────────────────────────────────────
  #
  # ⚠ This is the CURRENT production state, not a degenerate case. The ten venues this image reaches
  # (binance/bybit/okx/hyperliquid/aster/deribit/alpaca/ctrader/ig/oanda) spawn no external program,
  # and the only consumer of `<project>/bin` in the tree belongs to code this image does not
  # carry: `crates/bridges/dukascopy/src/config.rs` (the JForex jar and its JRE). It was TWO until
  # the research crate dissolved and took the one binary that resolved `<project>/bin/lightgbm`
  # with it; the release still SHIPS that CLI and no shipped binary looks for it there any more.
  # So the staged tree is empty
  # BY POLICY — see the tool table in `deploy/docker/Dockerfile`.
  #
  # It must therefore leave the directory ALONE rather than replace it with an empty one: an
  # operator whose project folder already carries tools installed by
  # `scripts/fetch_release_tools.sh` would otherwise have them deleted by an image that had nothing
  # to offer in their place.
  if [ ! -s "$STAGE_DIR/$MANIFEST_FILE" ]; then
    log "this image stages no runtime tools; '$target' left untouched"
    return 0
  fi

  local want
  want="$(stamp_of "$STAGE_DIR")" || die "cannot read the image's toolset stamp"

  # ── Already installed? Verify anyway ──────────────────────────────────────────────────────────
  #
  # A matching stamp is a claim about what SHOULD be there; `verify_tree` is what makes it a claim
  # about what IS. A tree corrupted after a previous install (a truncated write, a half-finished
  # host-side copy) carries a valid stamp and broken content, and re-installing is the cheap
  # recovery — so a failed verification falls through to the swap instead of dying.
  local have
  if have="$(stamp_of "$target")" && [ "$have" = "$want" ]; then
    if verify_tree "$target"; then
      log "toolset ${want#toolset=} already installed in '$target'"
      return 0
    fi
    log "WARNING: '$target' carries the right stamp but fails verification — reinstalling"
  elif bin_has_payload "$target" && [ ! -f "$target/$VERSION_FILE" ]; then
    [ "${VIKE_CONTAINER_ADOPT_BIN:-}" = "1" ] ||
      die "'$target' holds files this image did not install (no '$VERSION_FILE'), and installing \
the image's toolset would REPLACE the directory wholesale.
  Nothing in this workspace deletes an operator's files by surprise, so this refuses instead.
  Either move those files aside, or set VIKE_CONTAINER_ADOPT_BIN=1 to let this image take the \
directory over."
    log "WARNING: VIKE_CONTAINER_ADOPT_BIN=1 — replacing an unstamped '$target'"
  fi

  # ── The swap ──────────────────────────────────────────────────────────────────────────────────
  #
  # Both scratch paths are SIBLINGS of the target, inside the mount, so the final move is a
  # same-filesystem `rename(2)` rather than a copy that can be interrupted half-written. (The copy
  # INTO the new tree does cross a filesystem — image layer to bind mount — which is fine: it is a
  # copy, and nothing observes its destination until the rename.)
  local new="$project/.bin.new.$$"
  local old="$project/.bin.old.$$"
  rm -rf "$new" "$old" || die "cannot clear scratch directories beside '$target'"
  mkdir -p "$new" || die "cannot create '$new' — is the project folder mounted read-only?"

  # `cp -a` carries modes across, which matters: an executable that arrives non-executable fails at
  # spawn with a message about the program rather than about the install. The stamp files are part
  # of the staged tree, so they land here too — i.e. the stamp is written INTO the new tree before
  # the rename, never after it.
  cp -a "$STAGE_DIR/." "$new/" || die "cannot copy the staged toolset into '$new'"

  verify_tree "$new" ||
    die "the staged toolset failed verification after copying into '$new' — refusing to install it. \
'$target' is unchanged."

  if [ -e "$target" ]; then
    mv "$target" "$old" || die "cannot move the existing '$target' aside"
  fi
  if ! mv "$new" "$target"; then
    # Put the old tree back before reporting, so a failed install leaves the box where it started
    # rather than with no `bin/` at all.
    [ -e "$old" ] && mv "$old" "$target"
    die "cannot move the new toolset into '$target'"
  fi
  rm -rf "$old"

  # ⚠ Verified AGAIN, at the published path. The check above proved the scratch tree; this proves
  # the thing the daemon will actually read, which is the only claim worth making.
  verify_tree "$target" || die "'$target' fails verification after the install"
  log "installed toolset ${want#toolset=} into '$target'"
}

# =================================================================================================
# START
# =================================================================================================

# `<project>` is DERIVED from `VIKE_SETTINGS_DIR` rather than read from a second variable, because
# "which project am I in" must have ONE answer — the rule `vike_model::state_path`'s
# `project_bin_dir_from` implements on the Rust side, where `bin/` hangs off the OVERRIDE's root
# exactly like this. A second variable here could disagree with the daemon about where its own
# tools are.
resolve_project() {
  local settings="${VIKE_SETTINGS_DIR:-}"
  [ -n "$settings" ] ||
    die "VIKE_SETTINGS_DIR is unset. The image sets it; something has unset it. Without it the \
project walk starts at the working directory and a container's is not a project — every venue \
would silently stay on paper."
  printf '%s\n' "$(dirname "$settings")"
}

# The daemon profile path this start will hand to `--config`, or nothing when the arguments name
# none.
#
# ⚠ It recognises exactly the two spellings `crates/vike-tradehub/src/main.rs`'s own parser accepts
# (`--config PATH` and `--config=PATH`) and answers exactly one question: WHICH FILE am I about to
# require. It deliberately does not validate anything else about the argument list — a second,
# disagreeing copy of the daemon's parser is how a wrapper starts refusing arguments the binary
# would have taken. Arguments that name no `--config` (a `--help`, a hand-driven invocation) simply
# have no profile to check, and the daemon's own parser answers for them.
profile_arg() {
  local arg want=0
  for arg in "$@"; do
    if [ "$want" = 1 ]; then
      printf '%s\n' "$arg"
      return 0
    fi
    case "$arg" in
      --config) want=1 ;;
      --config=*)
        printf '%s\n' "${arg#--config=}"
        return 0
        ;;
    esac
  done
  return 1
}

# ⚠ THE most likely operator mistake, and the one with the worst failure mode: forgetting `-v`.
#
# Docker's default working directory is `/` and an unmounted `/project` is an empty directory in the
# image, so the settings walk finds nothing, the credential store is absent, and every venue stays
# on PAPER — with no error, because "no settings" and "settings that say nothing" are
# indistinguishable downstream (`crates/vike-model/src/state_path.rs`'s `project_settings_dir`
# carries that history, and `deploy/vike-tradehub.service`'s pre-flight block is the native cure).
# So this refuses, naming the flag.
#
# ## ⚠ TWO TIERS, and the split is what keeps the message useful
#
# Tier 1 is the MOUNT. If `/project` or `/project/settings` is not there, everything below is noise:
# an unmounted project has no `settings/state` and no profile BECAUSE it has nothing, and listing
# those as separate problems buries the one that matters.
#
# Tier 2 is what must be INSIDE a real project folder, and those are collected and reported
# TOGETHER. That is the shape `vike_config::refuse_credential_file_arming` uses and for its stated
# reason: fixing a box one restart at a time is a worse experience than being handed the list. A
# first run on a fresh folder is missing BOTH of these, and reporting one, being fixed, then
# reporting the other is two round trips through a container start.
#
# ## ⚠ IT REFUSES; IT DOES NOT CREATE
#
# `mkdir -p` was the alternative for `settings/state` and it is wrong three times over. The native
# reference REFUSES the same case — systemd will not start a unit whose `ReadWritePaths=` names a
# missing path (status=226), which `deploy/vike-tradehub.service` documents in its own
# `ReadWritePaths=` block — and `docs/decisions/0026-containerisation-additive-backend-image.md`
# forbids the image being the weaker shape, not the stranger one. It would also be a fix that stops
# working exactly when an operator takes the better advice: the TIGHTER run recipe in
# `docs/ops/tradehub-container.md` mounts the project `:ro` with `settings/state` as its own bind,
# so the `mkdir` would fail there while succeeding on the loose one. And a missing `settings/state`
# under that recipe means the BIND is wrong — creating the directory would paper over a
# misconfigured mount and hand the operator a HALT file inside the container that vanishes on the
# next `docker run`.
#
# Creating a DIRECTORY would not have breached the credential-store rules (those forbid this
# workspace writing a credential FILE, and say nothing about a `mkdir`), so the refusal is not
# obedience to them — but it does keep the stronger property they exist to protect: nothing here
# writes into `<project>/settings` at all, so there is no path by which this image touches the
# directory holding live venue keys.
# ── The append-atomicity probe ──────────────────────────────────────────────────────────────────
#
# ⚠ THIS EXISTS BECAUSE A HOST BIND MOUNT CAN LOSE WRITES, SILENTLY. Measured 2026-08-24 on Docker
# Desktop 29.7.2 / Windows 11 / WSL2: four processes appending 25 lines each into one file opened
# `>>` left **36 of 100** lines, and this repository's own
# `crates/vike-model/tests/change_journal_concurrent.rs` run with TMPDIR on that mount reported
# `got 60 of 200`. Not torn lines: LOST writes. The same binary on the container's own overlayfs
# passed, so it is the mount, not the test. `O_APPEND` atomicity is not preserved across the
# host-passthrough layer (gRPC-FUSE / virtiofs), so unserialised concurrent appends overwrite one
# another. And it fails with NOTHING TO SEE — no error, no short write, just a ledger that is
# quietly incomplete.
#
# ⚠ **THE PROBE TESTS THE FLOCK-SERIALISED PATTERN, BECAUSE THAT IS NOW WHAT THE JOURNAL DOES.**
# `crates/vike-model/src/change_journal.rs`'s `append_record` takes an exclusive advisory lock on
# `<dir>/changes.lock` (`CHANGES_LOCK_FILE`) around its open/write/`sync_data`, precisely because
# `flock` IS honoured across that mount (measured the same day: a second process was refused while
# the first held, and acquired after release) and the identical four-process probe with each append
# serialised behind `flock` left **100 of 100**. A probe that still measured plain `>>` would refuse
# a filesystem this daemon now handles correctly, and would have stopped testing the real write
# pattern — wrong twice over.
#
# What it therefore still catches is the case that is genuinely not survivable: a filesystem where
# `flock` itself is a no-op (or errors), so serialising does not serialise and the loss happens
# anyway. That is the only remaining shape, and there is no code change that rescues it.
#
# WHY IT TESTS THE PROPERTY RATHER THAN SNIFFING THE FILESYSTEM. `stat -f` reports `UNKNOWN` for
# the Docker Desktop mount and a real name elsewhere, so a type check would "work" — and would be a
# proxy. It would miss NFS, miss a future passthrough with a recognised name, and refuse a healthy
# filesystem whose name we failed to list. Serialised concurrent appends are the thing the journal
# actually needs, so that is what is measured.
#
# The probe is the smallest honest version of the real test: N processes each taking the lock, doing
# ONE `>>` append and releasing, M times over — the journal's own per-record shape — then a line
# count plus a count of acquisitions that FAILED. It writes into a temporary directory under the
# path being judged and removes it, so it proves the property of THAT filesystem and leaves nothing
# behind.
append_atomicity_ok() {
  local dir="$1" procs=4 lines=25 want probe rc=0
  want=$((procs * lines))
  # No `flock(1)` = no measurement. 2 is "could not run", never a verdict: refusing a mount because
  # this image lost a coreutil would be the same mistake as trusting a filesystem name.
  command -v flock > /dev/null 2>&1 || return 2
  probe="$dir/.vike-append-probe.$$"
  mkdir -p "$probe" 2> /dev/null || return 2   # 2 = could not run; NOT a verdict
  # The sentinel, opened `>>` rather than `>` for the same reason the Rust guard passes
  # `truncate(false)`: never rewrite a file another process may be holding.
  : >> "$probe/lock" 2> /dev/null || { rm -rf "$probe" 2> /dev/null; return 2; }
  local i
  for i in $(seq 1 "$procs"); do
    (
      # One fd per process — a distinct open file description, which is what `flock` keys on, so
      # these really contend with one another exactly as separate `append_record` calls do.
      exec 9>> "$probe/lock"
      local j
      for j in $(seq 1 "$lines"); do
        # A FAILED acquire is recorded rather than swallowed: "the lock does not work here" is the
        # one diagnosis this probe now exists to make, and a silent fallthrough to a bare append
        # would let it read as a healthy filesystem.
        flock 9 || printf 'x\n' >> "$probe/lockfail"
        printf 'p%s-l%s\n' "$i" "$j" >> "$probe/log"
        flock -u 9
      done
    ) &
  done
  wait
  # ⚠ Both counts are read through a `-f` test rather than `wc -l < file 2>/dev/null`: a FAILED
  # input redirection is reported by the shell BEFORE the `2>` takes effect, so the tidy-looking
  # spelling prints "No such file or directory" every healthy run — and `lockfail` is absent on
  # every healthy run by construction.
  local got=0 fails=0
  [ -f "$probe/log" ] && got=$(wc -l < "$probe/log")
  [ -f "$probe/lockfail" ] && fails=$(wc -l < "$probe/lockfail")
  rm -rf "$probe" 2> /dev/null
  APPEND_PROBE_GOT="$got"
  APPEND_PROBE_WANT="$want"
  APPEND_PROBE_LOCKFAIL="$fails"
  { [ "$got" -eq "$want" ] && [ "$fails" -eq 0 ]; } || rc=1
  return $rc
}

check_project() {
  local project="$1" settings="$2" profile="$3"

  # ── Tier 1: the mount ─────────────────────────────────────────────────────────────────────────
  [ -d "$project" ] ||
    die "the project folder '$project' is not a directory. Mount it: \
docker run -v /srv/vike-<unit>:$project ..."
  [ -d "$settings" ] ||
    die "'$settings' is not a directory, so this container has NO settings and NO credentials — \
every venue would stay on paper with nothing in the log to say why.
  Mount a real project folder: docker run -v /srv/vike-<unit>:$project ...
  and make sure it carries settings/ and settings/state/ (docs/ops/tradehub-container.md)."

  # ── Tier 2: what a real project folder must carry ─────────────────────────────────────────────
  local problems=0 report=""

  if [ ! -d "$settings/state" ]; then
    problems=$((problems + 1))
    report="$report
  [$problems] '$settings/state' is missing — the daemon's ONE writable directory.
      The HALT kill switch lives there (VIKE_HALT_FILE) and NOTHING creates it lazily:
      crates/vike-bridge-core/src/halt.rs's halt_path_arming_error probes the parent, finds no
      directory, and the daemon logs 'HALT KILL SWITCH IS NOT ARMABLE' — reached even on a paper
      mount. The switch is then dead until somebody reaches for it mid-incident and 'touch' fails
      with ENOENT. The rolling log file lands in the same directory.
      Fix, on the HOST, inside the folder you pass to -v:
          mkdir -p <your-project>/settings/state"
  fi

  # ⚠ The second arm re-tests `-f` rather than leaning on the first arm having excluded it, even
  # though `elif` already guarantees that today. `[ ! -r ]` is TRUE for a file that does not exist,
  # so an arm written as the bare negation reports "cannot be READ, this is a --user mismatch" for
  # an ABSENT profile the moment anything reorders these branches — sending an operator to fix
  # ownership on a file they have not created yet. Measured: the selftest's own mutation of the
  # first arm produced exactly that message.
  if [ -n "$profile" ] && [ ! -f "$profile" ]; then
    problems=$((problems + 1))
    report="$report
  [$problems] the daemon profile '$profile' is missing — this image's CMD names it, and
      the daemon EXITS without it ('bad profile <path>: No such file or directory'). It decides
      venue, symbol and strategy, so nothing in this workspace writes it for you.
      Fix: take the shipped template, which is a PAPER mount as it stands, and edit it:
          docker run --rm <this-image> --template > <your-project>/settings/$PROFILE_NAME
      For a LIVE mount start from that venue's own runbook in the repository's docs/ops/ instead —
      it carries the venue's measured tick size and the full arming checklist."
  elif [ -n "$profile" ] && [ -f "$profile" ] && [ ! -r "$profile" ]; then
    problems=$((problems + 1))
    report="$report
  [$problems] the daemon profile '$profile' exists but cannot be READ by uid $(id -u 2> /dev/null || echo '?').
      A bind mount keeps HOST ownership, so this is almost always a --user mismatch.
      Fix: docker run --user \"\$(id -u):\$(id -g)\" ...  (or chown the file to that uid)"
  fi


  # ── Tier 3: can this filesystem actually HOLD the change journal? ─────────────────────────────
  # Only worth asking once the state directory exists, so it runs after the Tier-2 arm above.
  if [ -d "$settings/state" ] && [ "${VIKE_SKIP_APPEND_PROBE:-}" != "1" ]; then
    append_atomicity_ok "$settings/state"
    case $? in
      1)
        problems=$((problems + 1))
        report="$report
  [$problems] '$settings/state' LOSES APPENDS EVEN UNDER A LOCK — $APPEND_PROBE_GOT of
      $APPEND_PROBE_WANT lines survived a $((APPEND_PROBE_WANT / 25))-process probe in which every
      append was serialised behind flock ($APPEND_PROBE_LOCKFAIL acquisitions failed outright).
      That means this filesystem's LOCKING IS A NO-OP: serialising does not serialise. This is not
      survivable and no setting fixes it.
      The change journal is appended by more than one process (the daemon on a control-socket
      settings write, the GUI on a credential rotation) and every append takes an exclusive lock on
      <state>/changes/changes.lock precisely so that a filesystem which does not preserve O_APPEND
      atomicity — a Docker Desktop host bind mount is one — stays correct anyway. A filesystem where
      the lock itself does nothing has no such fallback.
      ⚠ It fails with NOTHING TO SEE: no error is logged, no write reports short. The ledger just
      stops being complete.
      Fix: put the WRITABLE paths somewhere with working locks. Docker volumes live on ext4 inside
      the VM and never cross a passthrough layer:
          docker volume create vike-state
          docker volume create vike-data
          docker run -v /srv/vike-<unit>:$project:ro \
                     -v vike-state:$settings/state \
                     -v vike-data:$project/data ...
      Drop --user with a named volume: it is owned by whoever first writes it.
      Full recipe and the measurements behind this: docs/ops/tradehub-container.md.
      To proceed anyway (you accept an incomplete ledger): VIKE_SKIP_APPEND_PROBE=1"
        ;;
      2)
        # Could not run the probe at all — report it, refuse nothing. A probe that cannot execute
        # is not evidence, and turning "I could not measure" into "your mount is broken" would be
        # the same mistake as trusting a filesystem name.
        log "WARNING: could not run the append probe in '$settings/state' — proceeding unmeasured"
        ;;
    esac
  fi
  [ "$problems" -eq 0 ] ||
    die "this project folder cannot start the daemon — $problems problem(s):
$report

  All of it lives in the directory you mounted; see docs/ops/tradehub-container.md's first run."
}

main() {
  case "${1:-}" in
    --stamp)
      [ $# -eq 2 ] || die "usage: entrypoint.sh --stamp <dir>"
      stamp_tree "$2"
      return $?
      ;;
    --verify)
      [ $# -eq 2 ] || die "usage: entrypoint.sh --verify <dir>"
      verify_tree "$2" || die "'$2' does not match its own $MANIFEST_FILE"
      log "'$2' matches its $MANIFEST_FILE"
      return 0
      ;;
    --template)
      # ⚠ STDOUT CARRIES THE TEMPLATE AND NOTHING ELSE. This verb exists to be redirected
      # (`--template > <project>/settings/tradehub.toml`), so a `log` line here would land INSIDE
      # the operator's profile and the daemon would refuse it at the first line. Every message
      # below is a `die`, i.e. stderr, and produces no file worth keeping.
      [ $# -eq 1 ] || die "usage: entrypoint.sh --template   (it takes no arguments)"
      [ -f "$TEMPLATE_FILE" ] ||
        die "the image is malformed: '$TEMPLATE_FILE' is missing. It is the repository's \
'settings/tradehub.example.toml', staged into the image by scripts/release_container_image.sh."
      cat "$TEMPLATE_FILE" || die "cannot read '$TEMPLATE_FILE'"
      return 0
      ;;
    --selftest)
      exec bash "$(dirname "${BASH_SOURCE[0]}")/entrypoint_selftest.sh" "${BASH_SOURCE[0]}"
      ;;
  esac

  # ⚠ The container equivalent of the unit's `UMask=` directive, and one of only two hardening
  # directives with an equivalent in shell rather than in a `docker run` flag (no such flag exists,
  # so there is nowhere else to put it). New files become owner-only, which is the intended trade
  # for a tree holding venue keys: a future reader under a DIFFERENT account breaks on it loudly,
  # with a permission error rather than with silence.
  #
  # ⚠ It is set HERE — on the start path — and deliberately NOT at file scope, where it would also
  # apply to `--stamp`. That call runs at IMAGE BUILD time, and a `.version`/`.manifest` written
  # owner-only would be unreadable to a container started with `--user <host uid>`, which is the
  # NORMAL way to run this image: the entrypoint would die on "cannot read the image's toolset
  # stamp" for every operator whose uid differs from the build's. `scripts/release_container_image.sh`
  # normalises the staged tree's modes as the second half of that guard.
  umask 0077

  local project settings profile
  settings="${VIKE_SETTINGS_DIR:-}"
  project="$(resolve_project)" || exit 1
  # ⚠ Resolved BEFORE the checks and never re-derived: the file this start requires must be the
  # file this start passes to `--config`, and a second scan of `"$@"` is how those two come to
  # disagree. `|| true` because "no `--config` in these arguments" is a legitimate answer, not a
  # failure — see `profile_arg`.
  profile="$(profile_arg "$@" || true)"
  check_project "$project" "$settings" "$profile"

  # ⚠ WARN, never refuse. The image cannot choose a uid: the project folder is a bind mount that
  # keeps HOST ownership, so the correct one is the host's and only the operator knows it — which is
  # also why `deploy/docker/Dockerfile` bakes no `USER` line, exactly like
  # `deploy/vike-tradehub.service`, which carries no `User=` either. Running as root writes
  # root-owned files into somebody's project folder and quietly drops the container's half of the
  # unit's `NoNewPrivileges=`/`User=` posture. Refusing would strand a legitimate root operator, so
  # this says it once and continues.
  if [ "$(id -u 2> /dev/null || echo 0)" = "0" ]; then
    log "WARNING: running as uid 0. New files in '$project' will be root-owned, and this drops the \
container's equivalent of the unit's User=/NoNewPrivileges= posture. Prefer: \
docker run --user \"\$(id -u):\$(id -g)\" ..."
  fi

  install_tools "$project"

  # ── The `ExecStartPre=` equivalent ────────────────────────────────────────────────────────────
  #
  # `deploy/vike-tradehub.service` runs `vike-cli config check` before `ExecStart`, and the unit's
  # own comment block is the argument for it: without the line the daemon STARTS ANYWAY on a broken
  # configuration, and every way it can be broken is silent by construction. A container has no
  # `ExecStartPre=`, so the equivalent is this call — and dropping it would make the image the
  # weaker of the two deployment shapes, which
  # `docs/decisions/0026-containerisation-additive-backend-image.md` forbids.
  #
  # ⚠ Deliberately NOT `--strict`, matching the unit for the reason the unit gives: `--strict`
  # promotes every warning to a failure, including a 0644 `secrets.env`, and
  # `crates/vike-secrets/src/store.rs`'s `PermissionWarning` argues that one must never be a
  # refusal. Run `docker run ... vike-cli config check --strict` by hand when auditing a box.
  #
  # ⚠ **`config check` DOES NOT JUDGE THE DAEMON PROFILE, and it must not** — which is why
  # `check_project` above already refused a missing one, several steps before this line runs.
  #
  # `crates/vike-cli/src/cmd/config_check.rs`'s `inspect` judges the four settings files and the
  # credential store. It cannot judge this profile for three independent reasons, each fatal on its
  # own: the path is a DAEMON ARGUMENT that only the caller knows (this entrypoint's `--config`,
  # the unit's `ExecStart=`, the PowerShell launcher's `-ProfilePath`), and that file's own doc
  # already refuses to re-derive an answer nothing else reads; the parser is
  # `crates/vike-tradehub/src/config.rs`'s `DaemonProfile`, and `vike-cli` and `vike-tradehub`
  # declare the SAME `layer` in their manifests, so `crates/vike-ops/tests/layer_gate.rs` refuses
  # the dependency that would be needed to call it; and the data daemon unit runs this
  # same verb as their own `ExecStartPre=` while having no daemon profile at all, so a profile row
  # in a general CLI verb would fail two correct installs.
  #
  # What was WRONG was not that `config check` stayed silent — it was that its "0 error(s), 0
  # warning(s)" stood one screen above `bad profile … No such file or directory` and read as a
  # clean bill for a box that could not start. Two changes fix that here rather than there: the
  # refusal now happens FIRST, so on the failing box the green report is never printed at all; and
  # on a healthy box the line below states the profile this start is about to use, so the pre-flight
  # names both things it checked instead of one.
  if [ -n "$profile" ]; then
    log "pre-flight: daemon profile '$profile' — present"
  else
    log "pre-flight: no --config in these arguments; the daemon's own parser answers for them"
  fi
  log "pre-flight: vike-cli config check (the four settings files + the credential store)"
  "$BIN_DIR/vike-cli" config check ||
    die "configuration pre-flight FAILED — refusing to start the daemon. \
Fix what it named above, or run 'vike-cli config check --json' for the machine-readable report."

  # ⚠ `exec` — see the header. The daemon must BE PID 1, or `docker stop` cannot reach its handler
  # and the resting book is abandoned at the venue.
  #
  # ⚠ **`tradehub`, not `vike-tradehub`** — this is a LINK in `/opt/vike/bin`, staged by
  # `scripts/release_container_image.sh`'s `BINARIES`, and the multicall routes on the link's
  # basename against `crates/vike/src/main.rs`'s `TOOLS`. That table dropped the redundant `vike-`
  # prefix when the dispatcher became `vike-backend`. Spell this and that roster the same or the
  # container exits 127 (link absent) or exits 2 printing a tool list (link present, no matching
  # row) — and the second one leaves `ls -l /opt/vike/bin` looking perfectly healthy.
  log "starting trade (the vike-tradehub daemon)"
  exec "$BIN_DIR/trade" "$@"
}

main "$@"
