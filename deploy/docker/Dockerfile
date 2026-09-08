# Dockerfile — the ADDITIVE backend image: `vike-tradehub` + `vike-cli`, reaching the ten wired
# venues, with the project folder MOUNTED from the host rather than baked.
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
# ## The base is pinned by DIGEST, and the tag alone would not be enough
#
# `ubuntu:24.04` is the matching base because the binaries are glibc-DYNAMIC on the release runner's
# host triple (no `--target` anywhere in `release.yml`), so the base's glibc must be at least the
# builder's. MEASURED on the CI box — the box that job runs on — 2026-08-24: `ldd (Ubuntu GLIBC
# 2.39-0ubuntu8.8) 2.39`. ⚠ Anything older fails at EXEC rather than at build: Debian bookworm's
# 2.36 produces an image that builds green and dies the first time somebody runs it, which is the
# failure mode worth naming because CI cannot see it.
#
# ⚠ The DIGEST is here because `ubuntu:24.04` is a MUTABLE tag — it moves on every point release, so
# two builds of one release commit could sit on two different bases. That is the same property
# `crates/vike-ops/tests/supply_chain_gate.rs`'s `every_action_reference_is_sha_pinned` enforces for
# actions and `no_tool_download_resolves_a_mutable_pointer` enforces for downloads; the base image
# is the same class of dependency and gets the same treatment. Re-resolve with:
#   curl -sI -H "Authorization: Bearer $TOKEN" \
#     -H "Accept: application/vnd.oci.image.index.v1+json" \
#     https://registry-1.docker.io/v2/library/ubuntu/manifests/24.04 | grep -i docker-content-digest
# Resolved 2026-08-24. Bump it deliberately, and only alongside a re-measurement of the runner's
# glibc.
FROM ubuntu:24.04@sha256:33ceb71981b602c1a7443a53469e4dba065f7503eab3078a2d7a57a2ab987517

# ## ⚠ NOTHING IS INSTALLED — no `apt-get`, no `curl`, no package at all
#
# Two claims hold this up, and both were checked rather than assumed.
#
# **1. `ca-certificates` does nothing here, so it is deliberately ABSENT.** The TLS roots are inside
# the ELF: the root `Cargo.toml` pins `tungstenite` with `rustls-tls-webpki-roots` and declares
# `webpki-roots` outright, and `Cargo.lock` contains `webpki-roots` and ZERO occurrences of
# `rustls-native-certs`, `rustls-platform-verifier` or `openssl` (verified by grep, 2026-08-24 — the
# count was 0). Nothing in this binary ever reads `/etc/ssl/certs`, so installing the package would
# add a moving upstream dependency to a trading image in exchange for nothing.
#
# **2. No runtime program is required on this path.** A tree-wide `Command::new` sweep finds every
# program the workspace spawns, and none is reachable from this daemon: the JForex sidecar
# (`crates/bridges/dukascopy/src/exec.rs`) belongs to a venue this image does not carry,
# `clickhouse-client` (`crates/vike-backfill/src/pmxt/ingest.rs` — the only site left in the tree
# since `vike-research`'s own ClickHouse sink was deleted with that crate)
# and the LightGBM trainer (`crates/vike-ml/src/cli.rs`) live in binaries this image does not ship,
# and `crates/vike-buildinfo/build.rs` runs at BUILD time. ⚠ The one site LINKED into this binary is
# `crates/vike-run/src/incident.rs`'s `git_build_info`, and it is not an exception: the daemon never
# calls it, and its own doc declares it best-effort — "on a deployed binary far from the repo it
# simply yields `None`". So `git` is not required either, and an image without it degrades exactly
# as designed.
#
# The consequence worth having: this image adds NO third-party bytes beyond the pinned base and the
# release's own artifacts, so it introduces no new supply-chain surface to the audit
# (`cargo deny --all-features`, `deny.toml`) that already covers the binary.
#
# The entrypoint needs only what the base already provides — `bash`, coreutils (`sha256sum`, `cp`,
# `mv`, `sort`, `head`, `dirname`) and findutils (`find`, `xargs`).

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
# `.github/workflows/release.yml` in agreement, DERIVING the set from the script rather than
# carrying a fourth copy. Add a binary in one place.
#
# `backtest` is the one worth naming: it is why a fresh puller can do anything at all rather than
# stare at a daemon that needs credentials before it does something visible.
COPY vike /opt/vike/bin/vike

# ── ...AND THE TOOL NAMES, AS LINKS ──────────────────────────────────────────────────────────────
#
# MEASURED on the CI box, release profile: `vike-tradehub` 128 MB, `backtest` 106 MB, `vike-datahub`
# 107 MB, while `vike-cli` is 9.6 MB and `tearsheet` 1.9 MB. The gap is one static copy of
# DataFusion/arrow/parquet PER BINARY, and this image carried all of them. `vike` links that closure
# ONCE and answers to every tool name through argv[0].
#
# ⚠ **THE LINKS ARE THE INTERFACE, and a missing one is not cosmetic.** `deploy/docker/entrypoint.sh`
# runs `"$BIN_DIR/vike-cli" config check` and then `exec "$BIN_DIR/vike-tradehub"`; a name absent
# here is exit 127 at container start, not a fallback. `crates/vike-ops/tests/container_image_gate.rs`
# derives this set from `scripts/release_container_image.sh`'s `BINARIES` and fails when the two
# disagree, so the list below is checked rather than trusted.
#
# ⚠ RELATIVE targets (`vike`, not `/opt/vike/bin/vike`) so the tree stays movable: an absolute link
# breaks the moment anything copies `/opt/vike` elsewhere, which the entrypoint's toolset staging
# already does for `<project>/bin`.
#
# ⚠ **THIS IS THE IMAGE ONLY, and a HOST deliberately does NOT do the same.** The link trick pays
# for itself here — an image is COPIED, PULLED and STORED, so 485 MB is bandwidth and registry
# storage on every pull, for every user, forever.
#
# On a host it buys almost nothing and costs a failure class. Measured on the deployment box
# 2026-09-08: its data disk had 7 TB free, so the seven real binaries are 0.009% of it, and
# `.github/workflows/deploy.yml` already downloads every asset in `SHA256SUMS` — so the transfer is
# identical whether the box then keeps one binary or seven. Against that: a link whose target is
# missing is `status=203/EXEC` at deploy time, on a tag, automatically, with no reviewer; `ps`, a
# core dump and a debugger show `vike` rather than the tool actually running; and `--version` comes
# from a dispatcher rather than from the binary the operator named. Real files cannot fail that way
# at all.
#
# So `deploy/sbin/vike-trader-ci-deploy` installs REAL per-tool binaries (`backtest`, `vike-study`,
# `tearsheet` beside the daemon and the CLI). It briefly did the link thing — #1700 — and #1702
# reversed it on that measurement. The multicall stays the image's answer, and
# `crates/vike-ops/tests/multicall_gate.rs` still gates its feature parity, because a link that is
# quieter than the binary of the same name is exactly what the image would ship.
#
# ⚠ What no deployment does yet, links or not: touch a tool a systemd unit NAMES. The units under
# `deploy/` `ExecStart=` and `ExecStartPre=` real files — `/srv/*/bin/vike-cli` alone is named by
# FOUR of them — so replacing `vike-tradehub`, `vike-recorder`, `vike-datahub` or `vike-cli` is a
# RESTART, and the first of those signs live orders. Those need the pre-flight-and-rollback
# treatment the daemon already gets, which is why the helper's list is "nothing runs it as a
# service".
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
      org.opencontainers.image.description="vike backend: the headless trading daemon plus vike-cli. Additive to the tarball+systemd reference deployment (docs/decisions/0026)." \
      org.opencontainers.image.source="https://github.com/vike-io/vike-trader" \
      org.opencontainers.image.documentation="https://vike.io/docs/trader/"

ENTRYPOINT ["/opt/vike/entrypoint.sh"]
CMD ["--config", "/project/settings/tradehub.toml"]
