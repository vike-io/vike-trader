# Dockerfile — the ADDITIVE backend image: the `vike-backend` multicall (reached as `trade`) +
# `vike-cli`, reaching the ten wired venues, with the project folder MOUNTED from the host rather
# than baked.
#
#     docker build -f deploy/docker/Dockerfile -t vike-tradehub:<tag> <staged-context>
#
# ⚠ The context is a STAGING DIRECTORY built by `scripts/release_container_image.sh`, never the repo
# root — `target/` on the release runner has been measured at 1.7 TB, and a repo-root context would
# hand every byte of it to the daemon. That script is also what runs `entrypoint.sh --stamp` over
# `tools/`, so the two files this image needs at `/opt/vike/tools` exist before the build starts.
#
# ## What governs this file
#
# `docs/decisions/0026-containerisation-additive-backend-image.md` — accepted and ratified. Its
# boundary is the one thing that cannot move: **containerisation is ADDITIVE or it does not
# happen.** The release tarball plus systemd units stays the reference deployment; this image is a
# SECOND asset built from the SAME bytes, which is what keeps "additive" true by construction rather
# than by promise. `docs/ops/tradehub-container.md` is the operator-facing run recipe and carries
# the systemd-directive mapping.
#
# ## Route (a): COPY the release's binaries, never compile here
#
# 0026 argues this at length and the decisive half is identity. `crates/vike-buildinfo/build.rs`'s
# `emit_rerun_paths` resolves `GIT_SHA` at COMPILE time by reading real git paths, so a binary built
# inside a `docker build` whose context has no `.git` degrades exactly as `crates/vike-buildinfo/
# src/lib.rs`'s `git_state` predicts — `dirty?`, a case its own doc names ("a source tarball, a
# container with no `git`"). A shipped image that cannot say which commit it is would undo the
# incident that crate was written for. So `.github/workflows/release.yml` builds the binaries where
# `.git` exists, with the SBOM embedded and the checksum written, and this file only copies them:
# the same bytes as the tarball, one artifact to trust, one SBOM, one `SHA256SUMS` line.
#
# ## The base carries the RUST TOOLCHAIN, and it is pinned by DIGEST
#
# ⚠ **The base was `ubuntu:24.04` until 2026-09-23 and the change is not a cosmetic bump.** The
# image ships the `strategy-builder` verb, whose whole job is to invoke `cargo` on source an
# authenticated caller sent it — and on a base with no compiler that verb could only ever fail.
# The verdict that licenses it is
# `docs/decisions/0035-the-image-ships-every-feature-and-may-be-the-primary-install.md` — the image
# MAY be the primary install, and a deliberately-partial image is a deliberately-partial product.
# The half worth carrying here is that the toolchain arrives by CHANGING THIS LINE and by nothing
# else, so the "NOTHING IS INSTALLED" claim below survives intact (see the next block).
#
# ⚠ **And for a Windows or macOS user this image is not an additive convenience — it is the ONLY
# way to run the backend at all.** Measured against the v0.1.33 asset list: there is no
# `vike-backend.exe` and no macOS asset of any kind. A defect here is not a second-class defect.
#
# ### The glibc floor — unchanged, and the obvious base is the broken one
#
# The binary this image COPYs is glibc-DYNAMIC on the release runner's host triple, so the base's
# glibc must be at least the builder's. MEASURED on the CI box — the box that job runs on — 2026-08-24
# and again 2026-09-23: `ldd (Ubuntu GLIBC 2.39-0ubuntu8.8) 2.39`.
# ⚠ This read "no `--target` anywhere in `release.yml`" and that is no longer true — that workflow
# carries two `--target x86_64-pc-windows-gnu` cross-builds, for `vike-desktop` and `vike-cli`. The
# ARGUMENT is unharmed and narrows to the one binary that matters here: `vike-backend` is built
# with no `--target`, i.e. for the runner's own triple, which is what makes the base's glibc the
# thing that decides. Spelled as a property of THIS COPY rather than of the whole workflow, so a
# third cross-build cannot falsify it again.
# ⚠ Anything older fails at EXEC rather than at build: an image that builds green and dies the first
# time somebody runs it, which is the failure mode worth naming because CI cannot see it.
#
# MEASURED 2026-09-23, inside each candidate:
#
#   | image                      | glibc | verdict                                  |
#   |----------------------------|-------|------------------------------------------|
#   | the CI box itself (the floor)   | 2.39  | what the binaries are built against      |
#   | `rust:1.97-slim-bookworm`  | 2.36  | ⚠ TOO OLD — builds green, dies at exec   |
#   | `rust:1.97-slim-trixie`    | 2.41  | ✅ (`Debian GNU/Linux 13 (trixie)`)      |
#
# ⚠ **`rust:1.97-slim` is the trap: it LOOKS correct and names no distribution at all.** What it
# resolves to is upstream's choice of default suite, made and changed without reference to this
# repository — so it is not a thing to depend on, whatever it happens to point at on the day you
# look.
#
# ⚠ **And what it points at TODAY is the reason it reads as safe.** MEASURED at the registry
# 2026-09-23: `1.97-slim` and `1.97-slim-trixie` resolve to ONE index digest
# (`sha256:8e8cf8f7…`), while `1.97-slim-bookworm` is a different one (`sha256:2775a09d…`). So the
# short spelling is correct at this instant and carries no statement that it will stay correct,
# which is the whole difference between the two spellings. (This block asserted outright that
# `1.97-slim` "has pointed at bookworm" — that was an inference from how the tag works, stated as
# a measurement, and the measurement above is what is actually in hand.)
#
# The bookworm row in the table is not hypothetical either: a real released `vike-backend` was run
# on that base 2026-09-23 and died with ``/lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.39' not
# found``, while the same binary on the trixie digest answered `--version` normally.
#
# Spell `-trixie` outright and pin the digest;
# `crates/vike-ops/tests/container_image_gate.rs`'s `the_base_image_is_pinned_by_digest` refuses
# anything else.
#
# ### Why a digest and not the tag
#
# A TAG is MUTABLE — it moves on every point release, so two builds of one release commit could sit
# on two different bases. That is the same property `crates/vike-ops/tests/supply_chain_gate.rs`'s
# `every_action_reference_is_sha_pinned` enforces for actions and
# `no_tool_download_resolves_a_mutable_pointer` enforces for downloads; the base image is the same
# class of dependency and gets the same treatment. Re-resolve with:
#   TOKEN=$(curl -s \
#     "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/rust:pull" \
#     | sed -n 's/.*"token":"\([^"]*\)".*/\1/p')
#   curl -sI -H "Authorization: Bearer $TOKEN" \
#     -H "Accept: application/vnd.oci.image.index.v1+json" \
#     https://registry-1.docker.io/v2/library/rust/manifests/1.97-slim-trixie \
#     | grep -i docker-content-digest
# Resolved 2026-09-23. Bump it deliberately, and only alongside a re-measurement of the runner's
# glibc AND of the toolchain agreement below.
#
# ### ⚠ The toolchain VERSION is now part of what this line pins
#
# `vike_strategy_plugin`'s fingerprint check refuses a plugin built by a different toolchain than
# the loader, so the image's rustc and the release's rustc disagreeing is a real failure that lands
# on a user as "every strategy I write is refused". MEASURED inside this digest: `rustc 1.97.1
# (8bab26f4f 2026-07-14)`, `cargo 1.97.1`, which is what `vike-backend` on the CI box reports.
# ⚠ A version STRING comparison is not a proof of that and must not be written as one — the
# decisive check builds a plugin in the image and `dlopen`s it, and it lives in
# `scripts/release_container_image.sh`'s smoke rather than here.
#
# ### ⚠ The base bakes ENV of its own, which no gate in this repository can see
#
# `container_image_gate.rs`'s `no_baked_environment_variable_is_a_removed_setting` reads the `ENV`
# lines in THIS file; the base's are invisible to it without a container runtime, and no runner here
# has one. MEASURED 2026-09-23, the whole set this base contributes: `PATH` (prefixed with
# `/usr/local/cargo/bin`, which is how `cargo` is on the strategy builder's PATH at all),
# `RUSTUP_HOME=/usr/local/rustup`, `CARGO_HOME=/usr/local/cargo` and `RUST_VERSION=1.97.1`. None is
# a `vike_config::REMOVED_ENV` name and none is a vike setting. Re-measure that list when the digest
# moves — `docker image inspect <digest> --format '{{json .Config.Env}}'`.
# ⚠ `CARGO_HOME` is mode 0777 in this base (measured), which is what lets a container started with
# `--user "$(id -u):$(id -g)"` populate the registry cache. That is the base's choice, not ours.
FROM rust:1.97-slim-trixie@sha256:8e8cf8f7fd54a2d23d5a743b3a03f56e26b6c774276c33fa0595111704ebb15c

# ## ⚠ NOTHING IS INSTALLED — no `apt-get`, no `curl`, no package at all
#
# ⚠ **This claim is about `RUN`, not about the image's contents, and the difference is the whole
# reason the toolchain could arrive at all.** A compiler is now in this image; it got there by being
# part of a PINNED BASE somebody else builds, publishes and signs a digest for, not by this file
# fetching anything. `crates/vike-ops/tests/container_image_gate.rs`'s
# `the_image_installs_no_packages` refuses a `RUN` layer outright, and the property it protects —
# every byte here comes from the pinned base or from the release's own checksummed artifacts, so
# this image introduces no fetch, no mirror and no build-time network at all — is unchanged by a
# bigger base. What DID change is the size of the surface that base contributes, which is stated
# honestly rather than waved at: `rust:1.97-slim-trixie` is a Debian trixie userland plus a rustup
# toolchain, against `ubuntu:24.04`'s userland alone.
#
# Two claims used to hold this up. The first was CORRECTED on 2026-09-23 and the second NARROWED.
#
# **1. ⚠ `ca-certificates` IS PRESENT, and this block said it was "deliberately ABSENT".** The
# original argument was right about the daemon and does not cover a compiler. It read: the TLS roots
# are inside the ELF — the root `Cargo.toml` pins `tungstenite` with `rustls-tls-webpki-roots` and
# declares `webpki-roots` outright, and `Cargo.lock` contains `webpki-roots` and ZERO occurrences of
# `rustls-native-certs`, `rustls-platform-verifier` or `openssl` (verified by grep, 2026-08-24 — the
# count was 0). All of that is still true, so `vike-backend` still reads no `/etc/ssl/certs`.
# **But `cargo` does.** It fetches crates over TLS through the SYSTEM trust store, so a strategy
# build against a registry dependency on a root-less base would fail with a certificate error and
# nothing in the vike tree would explain why. MEASURED inside this digest, 2026-09-23: the base
# carries `ca-certificates 20250419` and `/etc/ssl/certs/ca-certificates.crt` (224,449 bytes, 301
# certificates). So the roots arrive the same way the compiler does — as a property of the base —
# and there is still no `RUN apt-get` here, which is the only thing this section ever forbade.
# ⚠ Do NOT "restore" the absence by stripping them. The old sentence described a base that had none
# and a payload that wanted none; this base has them and the payload now does.
#
# **2. A runtime program IS required, on ONE path, and it is the reason for this base.** The
# original sweep said none was reachable, which was true of an image running only `trade`. It now
# also runs `strategy-builder`, and `crates/vike-strategy-builder/src/builder.rs`'s `run` hands
# `render::build_plugin` a `cargo` executable to SPAWN — the process this image exists to make
# possible. The rest of the tree-wide `Command::new` sweep is unchanged and still finds nothing else
# on these paths: the JForex sidecar (`crates/bridges/dukascopy/src/exec.rs`) belongs to a venue
# whose JVM this image does not yet carry, and the LightGBM trainer (`crates/vike-ml/src/cli.rs`)
# lives in a binary this image does not ship.
# ⚠ (a `clickhouse-client` spawn was named here too, and had been attributed to the WRONG file for
# some time — `crates/vike-backfill/src/pmxt/ingest.rs` lost its spawn on 2026-09-09. The real last
# one was `vike-backfill`'s ClickHouse Polymarket ingest, deleted 2026-09-20; there is no
# `clickhouse-client` spawn anywhere in the tree now, so the sweep is shorter, not re-attributed),
# and `crates/vike-buildinfo/build.rs` runs at BUILD time. ⚠ The one site LINKED into this binary is
# `crates/vike-run/src/incident.rs`'s `git_build_info`, and it is not an exception: the daemon never
# calls it, and its own doc declares it best-effort — "on a deployed binary far from the repo it
# simply yields `None`". So `git` is not required either, and an image without it degrades exactly
# as designed.
#
# The consequence worth having survives in the form that was always the point: this image adds NO
# third-party bytes beyond the PINNED BASE and the release's own artifacts. The vike binaries'
# dependency tree is unchanged, so the audit that covers them (`cargo deny --all-features`,
# `deny.toml`) covers exactly as much as it did.
#
# The entrypoint needs only what the base already provides — `bash`, coreutils (`sha256sum`, `cp`,
# `mv`, `sort`, `head`, `dirname`, `seq`, `id`), findutils (`find`, `xargs`) and `flock`. MEASURED
# present in this digest, 2026-09-23, all of them.

# ── THE BINARIES ────────────────────────────────────────────────────────────────────────────────
#
# ⚠ `vike-cli` is NOT optional. `deploy/vike-tradehub.service` runs `vike-cli config check` as its
# `ExecStartPre=`, and that unit's own comment block is the argument: without it the daemon STARTS
# ANYWAY on a broken configuration, and every way it can be broken is silent by construction. The
# entrypoint reproduces that call, so an image without this binary could not be the equal of the
# native shape. (It is DataFusion-free and transport-free, so it costs a small binary and no runtime
# dependency.)
#
# ⚠ **THE SET GREW FROM TWO TO THE WHOLE PRODUCT**, per
# `docs/decisions/0035-the-image-ships-every-feature-and-may-be-the-primary-install.md`, which
# lifted 0026's additive boundary after the audience changed: `0014` put END USERS in the audience
# and `0034` licensed the tree so they can have it. For a person with no tarball already working,
# the image is the shortest path from nothing to a running backtest, and a deliberately-partial
# image is a deliberately-partial product.
#
# ⚠ **The roster lives in `scripts/release_container_image.sh`'s `BINARIES`, not here.** That script
# stages the context this file COPYs from, so a name it does not stage fails this build loudly;
# `crates/vike-ops/tests/container_image_gate.rs` holds this file, that script and
# `crates/vike/src/main.rs`'s `TOOLS` table in agreement, DERIVING the set from the script rather
# than carrying a fourth copy. Add a tool in one place.
#
# `backtest` is the one worth naming: it is why a fresh puller can do anything at all rather than
# stare at a daemon that needs credentials before it does something visible.
#
# ⚠ **The file name follows the PACKAGE name silently.** `crates/vike/Cargo.toml` declares no
# `[[bin]]`, so when the multicall package was renamed `vike` -> `vike-backend` the release asset
# renamed with it and no line anywhere said so. This `COPY` names the asset on BOTH sides — the
# staged context file and the installed path — and `.github/workflows/release.yml`'s
# `--bin vike-backend` has to agree with it or `docker build` fails on a missing context file.
COPY vike-backend /opt/vike/bin/vike-backend

# ── ...AND THE TOOL NAMES, AS LINKS ──────────────────────────────────────────────────────────────
#
# MEASURED on the CI box, release profile (under the per-tool binaries' own names, which is what the
# release built then): `vike-tradehub` 128 MB, `backtest` 106 MB, `vike-datahub` 107 MB, while
# `vike-cli` is 9.6 MB and `tearsheet` 1.9 MB. The gap is one static copy of DataFusion/arrow/parquet
# PER BINARY, and this image carried all of them. `vike-backend` links that closure ONCE and answers
# to every tool name through argv[0].
#
# ⚠ **THE LINK NAMES ARE THE MULTICALL'S VERBS, and they lost the `vike-` prefix on 2026-09-09.**
# `crates/vike/src/lib.rs`'s `program_name` routes on the link's BASENAME and `resolve` matches it
# against `crates/vike/src/main.rs`'s `TOOLS` by EXACT string with no aliases — so `trade`,
# `recorder`, `datahub`, `study` and `report` are the names that now dispatch, and the old prefixed spellings
# do not. `vike-cli` is the one row that keeps its prefix.
#
# ⚠ **A WRONG LINK NAME DOES NOT DANGLE, WHICH IS WHY IT IS THE QUIET FAILURE.** A missing link is
# exit 127 at container start, loud. A link whose name no `TOOLS` row answers for resolves fine —
# `ls -l /opt/vike/bin` looks perfect — and falls through argv[0], then through the verb arm, to
# `Route::Usage(2)`: the dispatcher prints its tool list and exits 2. `deploy/docker/entrypoint.sh`
# runs `"$BIN_DIR/vike-cli" config check` and then `exec "$BIN_DIR/trade"`, so both failures land
# on the same two names. `crates/vike-ops/tests/container_image_gate.rs` derives this set from
# `scripts/release_container_image.sh`'s `BINARIES` and checks it against the `TOOLS` table, so the
# list is checked rather than trusted.
#
# ⚠ RELATIVE targets (`vike-backend`, not `/opt/vike/bin/vike-backend`) so the tree stays movable: an
# absolute link breaks the moment anything copies `/opt/vike` elsewhere, which the entrypoint's
# toolset staging already does for `<project>/bin`.
#
# ⚠ **THE LINKS ARE THE IMAGE'S ANSWER, AND A HOST TAKES THE MULTICALL BY A DIFFERENT ROUTE.**
# This block used to say a host deliberately does NOT consolidate at all, and that stopped being
# true on 2026-09-09. What survives unchanged is the argument about LINKS, so read the two apart.
#
# The link trick pays here — an image is COPIED, PULLED and STORED, so 485 MB is bandwidth and
# registry storage on every pull, for every user, forever. On a HOST a link buys nothing and costs a
# failure class: a link whose target is missing is `status=203/EXEC` at deploy time, on a tag,
# automatically, with no reviewer. #1700 shipped host links; #1702 reversed it the same day on that
# measurement, and it was right.
#
# What changed is that the multicall never needed links: it dispatches on a SUBCOMMAND as well as on
# argv[0], so `vike-backend recorder …` runs the recorder out of one executable with nothing to
# dangle. #1716 took the daemon that way, #1717 took `vike-cli`'s engine lookup, and since
# 2026-09-09 the RELEASE attaches `vike-backend` and `vike-cli` and nothing else — so every unit
# under `deploy/` names `bin/vike-backend <tool>` and `deploy/sbin/vike-trader-ci-deploy` installs
# two files.
#
# ⚠ And the size argument #1702 dismissed was measured against the wrong thing. It IS worthless on
# the disk — 7 TB free, so seven real binaries are 0.009% of it — but the same bytes are uploaded to
# the release, downloaded again by EVERY deploy so `sha256sum -c` can verify the manifest, and
# re-published to the public mirror. 629 MB against 148, measured on v0.1.20.
#
# The costs that survive on a host are the ones that were never about links, and they are real:
# `ps`, a core dump and a debugger show `vike-backend` rather than the tool running, and `--version`
# comes from the dispatcher. `crates/vike-ops/tests/multicall_gate.rs` gates the feature parity for
# both now — a tool row that is quieter than the binary of the same name is what either would ship.
#
# ⚠ A tool a systemd unit NAMES is still a RESTART, and consolidating did not change that: the units
# under `deploy/` `ExecStart=` and `ExecStartPre=` real files — `/srv/*/bin/vike-cli` is named on an
# `ExecStartPre=` line by every daemon unit that runs a pre-check — so replacing `vike-backend` or
# `vike-cli` restarts the daemon that signs live orders. That is exactly why the helper gives
# `vike-backend` the pre-flight-and-rollback treatment and gives `vike-cli` none of it: one is what
# boots, the other is what a pre-check runs.
#
# ⚠ **The links are made in the STAGING CONTEXT, not by a `RUN` here**, and that is not a style
# choice: `crates/vike-ops/tests/container_image_gate.rs`'s `the_image_installs_no_packages` refuses
# any `RUN` layer at all — the claim it protects is that this image adds no third-party bytes beyond
# the pinned base and the release's own artifacts, and a `RUN` is where that claim starts eroding.
# `scripts/release_container_image.sh` creates them with `ln -s` while it stages, and `COPY`
# preserves a symlink, so the roster stays in the one place the gate already reads.
COPY links/ /opt/vike/bin/

# ── THE TOOLSET — the image's PRIVATE copy of `<project>/bin` ────────────────────────────────────
#
# ⚠ **THE MOUNT SHADOWS THE IMAGE.** The project folder is a bind mount, so every path that
# resolves against `<project>` resolves against the HOST's directory — and anything baked at
# `<project>/bin/...` is invisible at runtime. Worse, the tool ladders do not FAIL when their
# project rung is empty, they fall through: `crates/bridges/dukascopy/src/config.rs`'s
# `resolve_dukascopy_tools` ends its JVM ladder at the bare name `java` on `PATH`, which a slim
# image does not satisfy — a SILENT degrade to paper. Staging here and copying out at start is what
# closes that; `deploy/docker/entrypoint.sh` carries the mechanism and the stamp's contract.
#
# ## ⚠ THE TOOL TABLE IS EMPTY TODAY, and that is a measured fact about this image's venue set
#
# The ten venues this image reaches — binance, bybit, okx, hyperliquid, aster, deribit, alpaca,
# ctrader, ig, oanda — spawn no external program. And `<project>/bin` has exactly ONE consumer in
# the whole tree, belonging to code this image does not carry:
#
#   * `crates/bridges/dukascopy/src/config.rs`'s `resolve_dukascopy_tools` — the JForex jar and its
#     JRE. Dukascopy is out of scope: it drives execution through a Java sidecar child process.
#
# ⚠ It was TWO until the research crate dissolved: that crate's binary resolved `<project>/bin`'s
# pinned LightGBM CLI, and it was deleted outright. The RELEASE still ships that CLI
# (`.github/workflows/release.yml`, `scripts/fetch_release_tools.sh`) and `crates/vike-ml` still
# knows how to drive one — `crates/vike-ml/src/cli.rs`'s `LightGbmCli` takes the path as a
# PARAMETER — but no shipped binary resolves `<project>/bin/lightgbm` today; only this repo's own
# `#[ignore]`d suites do, against the checkout root. So the count fell by one and the tool is not
# orphaned.
#
# So the staged tree is empty BY POLICY rather than by omission, and the entrypoint's empty-toolset
# arm is the CURRENT production path: it installs nothing and — importantly — touches nothing, so an
# operator's own `<project>/bin` (whatever `scripts/fetch_release_tools.sh` put there) survives.
#
# The mechanism ships anyway, and that is the deliberate call: the moment a tool IS needed, adding
# it is one row in `scripts/release_container_image.sh`'s staging step, and the alternative — bake
# it at `<project>/bin` and discover the shadowing later — is precisely the silent failure above.
# It is not untested scaffolding either: `deploy/docker/entrypoint_selftest.sh` drives the real
# script against a synthetic NON-empty toolset, so the path is proven before it carries anything.
COPY tools/ /opt/vike/tools/

# ── THE FXCM RUNTIME — libraries the loader finds WITHOUT being told ─────────────────────────────
#
# `/opt/vike` is an install root, so `/opt/vike/bin/vike-tradehub-fxcm` resolves these through the
# ONE relocatable rpath entry `crates/bridges/fxcm/build.rs` bakes — `$ORIGIN/../lib`. That is why
# there is deliberately **no `ENV LD_LIBRARY_PATH` and no `/etc/ld.so.conf.d` entry** here: the
# binary already knows, and `DT_RPATH` outranks `LD_LIBRARY_PATH` anyway, so adding one would be
# inert at best and, if it ever disagreed, misleading.
#
# ⚠ Usually EMPTY, and empty is the normal case. `scripts/release_container_image.sh` fills the
# staged `lib/` only where the proprietary SDK is staged on the build box (`--if-staged` is a no-op
# elsewhere), and it stages `vike-tradehub-fxcm` only alongside it — the two travel together or
# neither does, so this image can never hold the linked daemon without the libraries it dies
# without. An empty directory here means FXCM stays on paper, which is that venue's documented
# behaviour when `vike_fxcm::sdk_linked()` is false, announced out loud at the mount rather than
# silently.
#
# The directory is COPY'd unconditionally because the staging script always creates it: a `COPY` of
# a path that does not exist fails the build, and making the instruction conditional is not
# something a Dockerfile can do.
COPY lib/ /opt/vike/lib/

# ── THE DAEMON-PROFILE TEMPLATE — the file this image's own CMD requires ─────────────────────────
#
# ⚠ `CMD` at the bottom of this file is `--config /project/settings/tradehub.toml`, and
# `crates/vike-tradehub/src/main.rs` EXITS when that file is absent. **Nothing in this workspace
# creates it** — it decides venue, symbol and strategy, which is the operator's choice — so before
# this line the only copy-paste sources were the nine per-venue LIVE runbooks under `docs/ops/`:
# inside a git checkout, i.e. exactly what a container operator does not have. Every new user met
# `bad profile … No such file or directory` on their first run, one screen below a green
# `config check`.
#
# The cure is the fifth member of the `settings/` template family — the repository's own
# `settings/tradehub.example.toml`, staged here by `scripts/release_container_image.sh` — and
# `deploy/docker/entrypoint.sh`'s `--template` verb, which prints it to stdout and nothing else:
#
#     docker run --rm vike-tradehub:<version> --template > /srv/vike-<unit>/settings/tradehub.toml
#
# ONE set of bytes reaches both audiences, so the checkout's `cp settings/tradehub.example.toml
# settings/tradehub.toml` and the command above cannot come to disagree.
#
# ⚠ The image does NOT seed that file into the mount, and the restraint is deliberate: it would be
# the first thing in this workspace to write into `<project>/settings`, the directory holding live
# venue keys, and the tighter run recipe in `docs/ops/tradehub-container.md` mounts that path `:ro`
# anyway. The entrypoint refuses and hands over the command instead.
COPY templates/ /opt/vike/templates/
COPY entrypoint.sh /opt/vike/entrypoint.sh

# ── WHERE THE PROJECT IS ────────────────────────────────────────────────────────────────────────
#
# `VIKE_SETTINGS_DIR` names the settings directory OUTRIGHT and beats the project walk
# (`crates/vike-model/src/state_path.rs`'s `SETTINGS_DIR_ENV`, duplicated in the dependency-free
# `crates/vike-secrets/src/dotenv.rs` and pinned equal by
# `crates/vike-bridge-core/tests/settings_dir_spellings.rs`'s `agree`). 0026 prescribes exactly
# this, and all three shipped systemd units already set it for the same reason: so resolution never
# depends on the working directory.
#
# ⚠ Without it, Docker's default working directory is `/` and the walk finds nothing: no
# `secrets.env`, no `policy.toml`, no state — every venue silently on PAPER, with no error, because
# "no settings" and "settings that say nothing" are indistinguishable downstream. The entrypoint
# refuses to start rather than let that happen.
ENV VIKE_SETTINGS_DIR=/project/settings

# ⚠ `WORKDIR` IS ALSO WARRANTED, and it is not redundant with the line above — it covers a
# different resolver.
#
# `VIKE_SETTINGS_DIR` is honoured only by the `_from` resolvers. The `_from`-LESS ones are
# `$VIKE_SETTINGS_DIR`-BLIND by construction and answer from the working directory, and the tree
# still has live callers of that shape: `crates/vike-ops/tests/settings_registry.rs`'s
# `CREDENTIAL_STORE_PIN` is the ratcheted roster of library files that reach the credential store
# through `load_workspace_dotenv()` — a walk from the CWD that no variable redirects. One of them is
# on this image's path: `crates/bridges/polymarket/src/egress.rs`'s `dotenv_proxy_vars`, whose
# whole purpose is the second tier that makes a `.env`-written `POLY_PROXY_*` line take effect.
# From `/` that walk finds nothing and the tier is silently dead.
#
# So the two lines are complementary rather than belt-and-braces: the variable answers for the
# daemon's own boot, and this answers for every resolver the variable cannot reach. It also puts
# `crates/vike-run/src/incident.rs`'s best-effort `git` call in the project rather than at `/`.
WORKDIR /project

# ── THE OPERATOR-VISIBLE DEFAULTS ───────────────────────────────────────────────────────────────
#
# ⚠ `VIKE_LOG_FILE_LEVEL=warn`, carried over from the units and NOT a preference. `RUST_LOG`/
# `VIKE_LOG` filter the CONSOLE only; the file layer defaults to `trace` and captures the
# `log`→`tracing` bridge, and an unattended run once wrote 341 GB and nearly filled the disk hosting
# a live trading node. A container makes this worse, not better: the log lands in the MOUNT, i.e. on
# the operator's own disk.
ENV VIKE_LOG_FILE_LEVEL=warn

# The out-of-band kill switch, named explicitly for the same reason the unit names it: the answer
# then does not depend on the working directory, and it needs no rebuild to correct on a box running
# an older binary. It must sit inside the mount — a sentinel on the container's writable layer would
# VANISH on the next `docker run`, which is a safety regression rather than an inconvenience.
#   docker exec <c> touch /project/settings/state/HALT   # stop new orders
#   docker exec <c> rm    /project/settings/state/HALT   # resume
ENV VIKE_HALT_FILE=/project/settings/state/HALT

# ⚠ **THE BIND GUARD IS SATISFIED HONESTLY, AND DELIBERATELY NOT MADE CONTAINER-AWARE.**
#
# A containerised daemon must bind the wildcard inside its namespace, and
# `crates/vike-tradehub/src/server.rs`'s `bind_exposure` classifies `0.0.0.0`/`::` as
# `BindExposure::Public` ON PURPOSE — its doc says they "listen on every interface, which is the
# single most common way this surface gets accidentally exposed". Without the opt-in below,
# `bind_decision` returns `BindDecision::Refuse` and the listener never opens, so `-p 7879:7879`
# yields connection refused: Docker publishes to the container's eth0, never to its loopback.
#
# 0026 rules that the guard must NOT gain a `/.dockerenv` probe, because "an address cannot be its
# own consent, because typing the address is the mistake" — a namespace is not a security boundary
# once a port is published, and the process cannot honestly answer a question decided outside it at
# `docker run -p`. So the image takes the warning DELIBERATELY, and sets the opt-in HERE where
# `docker inspect` shows it — a more auditable place than a shell export on a box.
#
# What an operator sees: ONE line at startup, `BindDecision::ProceedExposed` announcing the address
# that is now reachable. That statement is TRUE — the process really is listening on every interface
# of its namespace — and the real containment is in the run command, which must publish to
# `127.0.0.1:7879` and not `0.0.0.0:7879`. `docs/ops/tradehub-container.md` carries the recipe.
#
# ⚠ Inert unless the server is actually turned on: the control surface is opt-in
# (`VIKE_TRADEHUB_ADDR` + `VIKE_TRADEHUB_CONTROL`), and this flag changes nothing for a loopback
# bind. What makes it ACCEPTABLE rather than merely tolerable is that tradehub's remote surface is
# authenticated — scoped keys with an HMAC nonce challenge
# (`crates/vike-tradehub-client/src/auth.rs`'s `NodeKeys`). That is exactly why 0026 clears
# tradehub and BLOCKS `vike-datahub`, which authenticates nothing.
ENV VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1

# ⚠ Explicit, though it is also the default: the container twin of the unit's `KillSignal=SIGTERM`.
# SIGTERM raises the daemon's stop flag — the same flag the interactive `shutdown` word raises — and
# the ordinary teardown runs: the resting-order cancel sweep, the strategy-state save, the terminal
# journal snapshot, the feed quiesce, the bounded core join. Stating it here means a future base
# image or tooling default cannot quietly change what `docker stop` sends.
#
# ⚠ **IT NO LONGER REACHES THE DAEMON DIRECTLY, AND THAT IS THE ONE THING TO KNOW ABOUT THE 2026-09-23
# SHAPE CHANGE.** This image used to `exec` the daemon, so `docker stop`'s SIGTERM went straight to
# it. It now runs THREE processes, so PID 1 is `entrypoint.sh` and the signal lands on a shell —
# which does NOT forward to its children unless it is written to. `deploy/docker/entrypoint.sh`'s
# `forward_term` is that forwarding, installed as a trap BEFORE the first child starts, and
# `deploy/docker/entrypoint_selftest.sh` proves it by stopping a real process tree. If that trap is
# ever lost, this line still sends SIGTERM and NOTHING receives it: the stop becomes a SIGKILL after
# `--stop-timeout` and the resting book is abandoned at the venue, silently.
STOPSIGNAL SIGTERM

# ⚠ NO `USER` line, and that MATCHES the native reference rather than departing from it:
# `deploy/vike-tradehub.service` carries no `User=` either (only the `-the CI box` variant does). The
# image genuinely cannot choose one — the project folder is a bind mount that keeps HOST ownership,
# and the credential store's permissions are reported rather than enforced, so the right uid is the
# host's and only the operator knows it. Pass `--user "$(id -u):$(id -g)"`; the entrypoint warns
# when it finds itself running as uid 0.

# ⚠ BOTH URLs MUST BE PUBLIC, and neither may name the development repository. This image ships to
# Docker Hub and GHCR, so `source` and `documentation` are followed by people who have no account
# here — a private-repo URL 404s for every one of them, silently, and only on their side. Until
# 2026-09-04 both pointed at the private repo, for the whole life of the image. So `source` is the
# MIRROR, and `documentation` is the WEBSITE rather than a repo path: `docs/` is excluded from the
# mirror by publish_mirror.sh's DENY, so a mirror blob URL would 404 exactly as reliably.
LABEL org.opencontainers.image.title="vike-tradehub" \
      org.opencontainers.image.description="vike backend: the trading daemon, the backtest/compute server and the strategy builder in one container, with a live Rust toolchain. Additive to the tarball+systemd reference deployment (docs/decisions/0026, 0035)." \
      org.opencontainers.image.source="https://github.com/vike-io/vike-trader" \
      org.opencontainers.image.documentation="https://vike.io/docs/trader/"

# ── ONE CONTAINER, THREE PROCESSES ──────────────────────────────────────────────────────────────
#
# ⚠ **`CMD` IS `trade`'s ARGUMENTS, NOT THE CONTAINER'S ONE COMMAND**, and that distinction is new.
# The entrypoint used to `exec` the daemon, so `CMD` was straightforwardly its argv. It now
# SUPERVISES three processes and passes `"$@"` to the trading daemon alone; the other two take no
# argv the operator chooses (`backtest --addr`'s address and every `strategy-builder` knob resolve
# from settings/environment). `deploy/docker/entrypoint.sh`'s `supervise` is the one place that
# split is written down.
#
# Why one container rather than two, and why this does not weaken anything: on a HOST the builder is
# a separate unit for a SYSTEMD reason — `deploy/vike-backtest.service` carries `ProtectHome=yes`,
# which puts the toolchain outside that daemon's mount namespace, and
# `deploy/vike-strategy-builder.service`'s `ProtectHome=read-only` is called out there as the one
# deliberate departure. Inside a container there is no `ProtectHome`: the container boundary IS the
# sandbox, so the constraint that forced the split does not exist and one `docker run` is the whole
# product.
#
# ⚠ The cost, stated plainly rather than waved at: user-supplied Rust executes in the same container
# as the process holding live venue keys. That is not a NEW exposure — on the CI box today
# `vike-backtest` and `vike-tradehub` run as one account, and a `dlopen`ed plugin already reads the
# credential store — so one container changes nothing about it. It is worth knowing; it is not an
# argument against this shape. What WOULD be a new exposure is publishing the builder's or the
# compute daemon's port outward, which the paragraph below refuses.
#
# ⚠ The two extra daemons bind LOOPBACK inside this namespace and deliberately stay there:
# `backtest` at `127.0.0.1:7880` and `strategy-builder` at `127.0.0.1:7881`, which is not even a
# knob — `crates/vike-strategy-builder/src/builder.rs`'s `bind_addr` fixes the IP half in code. So
# `-p 7880:7880` reaches NOTHING, by design: the pair exists to serve `trade` and each other over
# this container's own loopback, and the one surface published outward stays tradehub's
# authenticated control port. There is deliberately no `VIKE_DATAHUB_ALLOW_PUBLIC_BIND` here to
# match the tradehub line above — that opt-in would expose a compute daemon whose job is running
# caller-supplied code, and 0026 already blocks the unauthenticated data plane for less.
ENTRYPOINT ["/opt/vike/entrypoint.sh"]
CMD ["--config", "/project/settings/tradehub.toml"]
