# Changelog

All notable changes to vike_trader_rust are recorded here.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/); versioning is
[SemVer](https://semver.org/) — pre-1.0, a minor bump may include breaking changes.

Per-release notes are also auto-generated on GitHub (the PRs merged since the previous tag)
by `.github/workflows/release.yml`; this file is the curated human summary. Cut a release with
`just release <X.Y.Z>`.

## [Unreleased]

## [0.1.16] - 2026-09-02

### Added
- **A licence.** The tree carried none at all — `licenseInfo: null`, with signed trading binaries
  already shipping to the end users `docs/decisions/0014` names. It is now **FSL-1.1-ALv2**
  (`LICENSE.md`, `docs/decisions/0034`): source-available today, each version irrevocably becoming
  Apache-2.0 on the second anniversary of its own release, with paid support and consulting
  explicitly permitted — the reason it was chosen over Commons Clause, which bans exactly that.
  Declared as `license-file` in the workspace manifest, never `license`: FSL has no SPDX id, and
  `deny.toml`'s licence gate stays green only through the `private` exemption, which its comment now
  explains instead of claiming the crates are proprietary.
- **`vike` — one executable carrying every headless tool** (`crates/vike`): `vike-cli`,
  `vike-tradehub`, `backtest`, `vike-datahub`, `vike-recorder`, `vike-study`, `tearsheet`,
  dispatched by argv[0] (an installed symlink) or `vike <tool>`. MEASURED on the CI box: the three
  DataFusion-linked binaries were 128 + 106 + 107 MB, each with its own static copy of the same
  closure; `vike --features full` is **128 MB** — the size of the largest tool it replaces. Every
  existing binary is kept as a thin shim, so every `ExecStart=`, `CARGO_BIN_EXE_*` and installed
  path still resolves. One tool per process is ENFORCED (a second dispatch panics), the dispatcher
  itself boots nothing — `multicall_gate` holds both, because `one_owner` structurally cannot see a
  dispatcher-plus-tool double boot. Thirteen settings-registry rows moved `Binary` → `Injected` on
  the way, which is that registry's declared target state, reached by threading parameters rather
  than by exempting ratchets.
- **The container image is the whole product, published where people can get it.**
  `docs/decisions/0035` lifts `0026`'s additive boundary: the image now carries every headless tool
  (as the one `vike` binary plus seven symlinks — 128 MB against ~500), the ForexConnect runtime so
  FXCM trades from a container (the author's distribution grant is recorded in `0035`; the `.so`
  set stays out of the release assets, where its 25 MB would tax every deploy), a portable Temurin
  JRE staged as a tool so Dukascopy's sidecar runs — no `apt-get`, the no-packages gate still holds
  — and it is pushed to **GHCR** (`ghcr.io/vike-io`, the job's own token, one `packages: write`
  scope, no stored secret) and **Docker Hub** (`vikeio/…`, the shopfront, a stored PAT taken
  deliberately). The privileged `vike-image-build` gained a `push` verb whose two destinations are
  CONSTANTS in the script — restoring push via `env_keep` would have handed the caller the
  destination — with the token on stdin and `docker logout` in a trap.
- **`vike-cli secrets template`** — the whole credential grid (every roster venue × SIM/DEMO/LIVE ×
  the credential suffixes, plus attribution keys), with EMPTY values, derived from `VENUES` so a
  new venue appears the day it joins. Prints to **stdout only**: a `--out` flag was deliberately not
  added, because the moment the command owns a path it can truncate a live store. The legacy
  `MAINNET` tier is read by the loader but not emitted — a first store should not be taught a
  spelling being retired. Composition lives in `vike_model::credential_keys::starter_keys`, because
  the registry gate reads a `credential_key` call site as "this crate READS these variables", and a
  command printing names reads none.
- **The the CI box daemon runs what users run**: the shipped `vike-tradehub` asset is now built with
  telegram, polymarket, record-feeds and materialize — decided so a production box can simulate its
  users. `fxcm` stays out of it (a hard `DT_NEEDED` dies at exec where the SDK is absent) and
  remains the separate `vike-tradehub-fxcm` asset; the two release gates that had forbidden ANY
  feature on the deployed pair were narrowed to their actual reason, native linkage.
- **The study runner that can FIT**, and panel provenance reproducing the engine bit-for-bit
  (#1584, prior session's work landing in this span).

### Fixed
- **Three of eight image binaries never built** (#1595): the roster named `vike-run` — a LIBRARY
  whose package builds no bin of its own — and `vike_recorder`, the file name where the manifest
  renames the target. Every text gate was green through it, because all three files AGREED on names
  that do not exist. `every_rostered_binary_is_a_real_cargo_bin_target` now asks `cargo metadata`
  rather than the filesystem.
- **`release-image.yml` could never fire** and the thin image carried seven drivers it cannot load
  (#1582, prior session); the image workflow's download list was also a hand-kept three while the
  image grew to eight — both the list and the expected checksum count now derive from the staging
  script's roster.
- **A venue outage no longer pages as our incident** (#1583, prior session).
- **`key_permissions.rs`'s module doc claimed the withdraw gate was not wired** while
  `vike_mount::make_engine` has called it for every credentialed binance MAINNET mount; corrected,
  with the survey of all fourteen venues recorded where the deferred per-venue table will need it —
  only bybit/okx/deribit can be probed at all, and for polymarket/hyperliquid/aster the capability
  belongs to the KEY ITSELF, where an all-`None` UNKNOWN that never refuses is the worst answer.

### Changed
- `docs/decisions/0026`'s "no registry credential exists on that box" corrected: a `github.token`
  is present in every workflow run, bounded by declared permissions — the distinction that made
  GHCR publishing one scope instead of a stored secret.
- Dependency bumps: the cargo minor-patch and major groups (#1580, #1581).

## [0.1.15] - 2026-08-31

### Added
- **The backend ships as a container image, and so does the client.** Two images, one per half of
  the split-plane architecture: `vike-tradehub` (the daemon) and `vike-thin` (the observe-only GUI).
  Both are built by `.github/workflows/release-image.yml` from the assets the Release already
  published, verified against its own `SHA256SUMS` before anything is copied in — so "built from the
  same bytes the release ships" is checked rather than inferred from sitting in the same directory.
  `docs/decisions/0026` is amended twice for this: the `"only at a real need"` deferral is lifted for
  `vike-tradehub`, and the GUI exclusion is narrowed to what it actually argued — GPU **passthrough**,
  not the application, which this repository's own `png-export` lane has been rendering on Mesa
  lavapipe with no GPU on every PR. The native path is untouched and remains the reference
  deployment; the image is additive or it does not happen.
- **Windows GUI assets — `vike-app.exe` and `vike-app-thin.exe`**, cross-built on the Linux release
  runner. The release published a Linux GUI only, so the largest desktop population had no native
  client and was pushed onto a container costing 1–3 CPU cores where an `.exe` costs about a tenth
  of one. That gap was never a decision; nobody had checked whether the cross-build worked. It does
  — and the binary was RUN on a Windows box, where it enumerated a real GPU through Vulkan,
  connected to a containerised daemon and rendered the full UI. ⚠ `x86_64-pc-windows-gnu`, not
  `-msvc`: the msvc target needs the MSVC linker and Windows SDK, which no Linux runner has. Worth
  knowing, because day-to-day development on a Windows box builds msvc.
- **`vike-app-thin`**, the Linux observe-only GUI asset — 51 MB against the fat build's 152 MB, and
  the input the client image is built from. `0.1.14` anticipated exactly this twin.
- **`vike-tradehub/ibkr`.** `vike-run` had carried the feature since the bridge landed and
  `vike-app` forwarded it, but this crate did not — so the GUI could mount IBKR live while the
  headless daemon fell through `make_engine`'s `("ibkr", _)` to paper **in silence**. The `ibkr` CI
  lane and its trigger set widen with it, so the arm is now compiled by something.
- **`VIKE_FRAME_LOG`** — an opt-in per-second frame-rate log. A measurement hook, not a feature.

### Fixed
- ⚠ **`vike-app` could not start on Linux at all.** `install_fonts` loaded every face from a
  `C:\Windows\Fonts\…` path, so off Windows the named families were never bound and epaint panicked
  on the title bar in frame one. **The release had been publishing a Linux GUI asset that had never
  been run** — `png-export` renders `vike-chart`/`vike-studio` EXAMPLES rather than this binary, and
  `app-check` only compiles it. Found by running it in a container. The regression test is
  platform-blind and mutation-tested both ways: it lays text out rather than checking membership,
  because a family bound to an EMPTY list passes a membership check.
- **An absent observe key signed with `""` and reported it as `bad mac`.** `unwrap_or_default()` on a
  missing key produces a well-formed HMAC the node rejects, so the operator saw "wrong key" when the
  truth was "no key". The daemon already got this right from its side; this is the client's half of
  the same sentence, and it names where the key is actually read from — the credential store, which
  an environment variable alone does not reach.
- **A warning that contradicted the chart in front of the operator.** `ensure_feed` ended "this chart
  will stay empty until one is", which is false in `--observe`: that build links no venue bridge, so
  every chart reaches that arm while the chart fills from `WireSnapshot::bars` over the node
  protocol. Measured while it logged on repeat against five hours of live candles.
- **The release-image workflow used `gh`, which is not installed on the release runner.** It would
  have failed on its first real run. `release.yml` already carried that rule beside its own publish
  step; the new workflow had not inherited it.

### Changed
- **`<project>/data` is now `<project>/market_data`.** It sat beside `<project>/user_data` and the
  pair read as one concept split in half — the question "which data?" had no answer in either name.
  Two doc claims went with it, both false beforehand: `hist` was never "the only thing in it"
  (`ticks` and a checkout's `bench_hist` are siblings), and the folder is wider than "market" — the
  hist store also holds this account's own `equity`, `exec_fill` and `exec_order`.

## [0.1.14] - 2026-08-29

### Added
- **The GUI ships as a release asset.** `vike-app-fat` — the full desktop app (the crate's default
  `fat` features: the local trading core, every venue bridge, the Studio's local store) for Linux
  x86-64 — is now built by `.github/workflows/release.yml` from the tagged commit, with its SBOM
  embedded by `cargo auditable` and its line in that release's `SHA256SUMS`, exactly like the two
  server binaries beside it. Obtaining the GUI used to mean compiling the workspace. The asset name
  carries the configuration on purpose: nothing is published as a bare `vike-app`, so a `thin`
  (`--observe`-only) twin could join later without re-pointing a single instruction that ever quoted
  the fat one. The tag path also grew the gate it was missing — `vike-app` is excluded from the
  derived crate roster a release re-validates, and `ci.yml`'s `app-check` runs on no tag, so that
  crate's unit tests now run on the release path instead of the artifact being published unrun.

## [0.1.13] - 2026-08-25

### Fixed
- **A reconcile pass that changed nothing logged anyway, once per pass.** `reconcile_reports` emitted
  `reconcile pass folded venue=… events=0 alerts=1` on every pass carrying alerts — ~1,440 lines a
  day per venue, and wholly redundant when nothing moved. Measured on the live box an hour after the
  0.1.11 deploy: 11:41:43 and 11:42:43, one minute apart, byte-identical. The line now reuses the
  held-divergence announcer's own "did anything happen this pass" answer rather than growing a
  second rate limiter beside the one 0.1.11 shipped, so the pass line and the backlog summary fire
  together by construction instead of because two cadences happen to agree. A pass that folds events
  still logs every time, and a venue with nothing held stays silent exactly as before — no heartbeat
  was invented where there had never been a line.
- **Deribit's post-reconnect replay stopped forever on the first socket close, in silence.** The A3
  resync transport connected once and nothing re-dialled it, so a venue-side close ended the replay
  of order/trade history across reconnect gaps for the life of the process — with every error
  swallowed, so nothing surfaced. It is the last of three never-heals sockets in that bridge; the
  other two were fixed in 0.1.11. Which cure applied was traced rather than assumed: that transport
  can carry exactly three frames ever — the auth, the order history read and the user-trades read —
  all non-matching-engine, so a re-dial plus a straight retry cannot double anything, and the licence
  is now stated on the type as what it MAY send. The heal deliberately did NOT go into the shared
  history helper, which the fill sentinel also calls on the EXEC ORDER socket — the one that carries
  `private/buy`, whose re-dial policy is decided elsewhere and must not gain a second, unreviewed
  entry point reachable from a timer. An outage is now bracketed by one warning naming the
  consequence and one recovery line, and the latch re-arms, because a latch that fires only once
  makes a broken lane look healthier the longer it stays broken.

## [0.1.12] - 2026-08-25

### Added
- **A venue can mount SEVERAL accounts.** `make_engine_accounts` returns one engine per ACTIVE
  account — its per-account ceiling permits it, its credentials load, the venue's arm can address an
  account at all, and the symbol-collision rule does not refuse it. Selection and the Data Manager's
  Effective column are ONE function, so the screen cannot disagree with the mount. A default-account
  box is identical, measured: one engine per roster venue, `route_key == venue`, empty
  `live_venues`, and a ledger record with no `account` key at all. ⚠ Only binance/bybit/okx/deribit
  consume the account-aware credential loader; a labelled account is REFUSED elsewhere rather than
  half-supported, because arming one would otherwise have built a live client on the first account's
  credentials.

### Fixed
- **The journal's p99.9 tail is gone rather than reduced** — 37 150 → 3 342 ns (11x), interleaved
  medians of 8 reps, bands that do not touch; the journal-less baseline is 3–7 µs, which is where it
  now sits. The warm window was ~20 ms of runway refreshed only AFTER a 36–141 ms blocking `msync`,
  and it was aimed at the watermark — behind the appender. ⚠ Two earlier claims recorded in this
  tree were wrong and are corrected at the code: the tail IS first-touch faults (the null result came
  from a harness appending 85 B records while the gate appends 275 B), and serialization is 61 % of
  the append's median but only 3 % of its p99.9 — the reused-buffer change was measured and NOT
  shipped. Production's 64 MiB configuration was already at 6.4 µs; this removes a large gate number
  and a smaller real one.
- **A HELD reconcile divergence names WHAT diverged**, and its dedup IDENTITY stops riding that text.
  `kind=PositionOnlyExternal detail=PositionOnlyExternal` was what the live box printed — a line
  raised to answer *which* answering *the same*. `Divergence::describe` is a total match, so a new
  variant is a compile error rather than one that inherits an empty description. ⚠ Because
  `HeldId::new` builds an un-keyed alert's identity from its `detail`, a detail carrying `qty=` would
  have made a drifting position a NEW alert every pass — the ~1,560-a-day bug `recon_held_tests.rs`
  exists to prevent, invisible to its own fixture, which holds qty constant. `identity_detail` is the
  churn-free half.

## [0.1.11] - 2026-08-25

### Fixed
- **The IG trade stream sent a made-up Lightstreamer client id, so the fill lane never worked — for
  any account, demo or live.** `LS_cid` is Lightstreamer's client-type LICENSING field; the trade
  lane sent `"vike-trader-rust"` and IG answered `CONERR,71,License not valid for this Client type`
  on every dial since the day it was written. The market-data lane always sent the TLCP-2.1.0
  mandated value from its own `LS_CID`, which is why feeds worked while fills did not — and why the
  split read as an account-permissions problem for weeks. Proven on the wire against the demo
  gateway with one login and two client ids, everything else held constant: the old value returns
  `CONERR,71` and closes; the mandated one returns `CONOK` and streams. The trade lane also carried
  its own copies of `LS_PROTOCOL`, `form` and `urlencode` — that duplication is what let the two
  spellings of one handshake drift — and `lightstreamer.rs`'s `form` doc already CLAIMED the sharing
  that did not exist. All four now come from one place. This settles the create_session handshake,
  not the fill lane end to end.
- **A permanently refused IG stream wrote an error line every second, forever.** Measured on the CI box:
  53,160 lines and 23 MB in one day from one loop, dead flat for 17 hours, 99.5% of the box's entire
  error volume — for a condition whose own message said it was permanent. The dial-failure path had
  the same defect unbounded and was worse (an unreachable endpoint warned ~78,500 times a day); it
  had been argued safe because "each drop costs a live session", which is true of exactly one of the
  six sites that produce it — the other five never establish a session at all. Retry and log policy
  now live in a pure state machine that takes the clock as an argument, so the log RATE is
  unit-tested rather than being a property of a running daemon. Both lanes now cost ~26 lines a day,
  and the driver reports the CONERR code as evidence instead of asserting a cause it cannot
  distinguish.
- **A Deribit socket closing mid-submit reported an order REJECTED that the venue may have
  accepted.** Only a response timeout was treated as ambiguous, so a mid-request close — the common
  first error on a dying socket — took the definite path and synthesized a terminal reject for a
  possibly-live order, stranding a phantom position. Every failure out of the read half is
  post-send by construction. The rule already existed and this transport was the one not following
  it: `vike_bridge_core::transport::read_body_ambiguous` states that a read failure is always
  ambiguous, never `code: 0`. Resolution is re-dial then RE-QUERY, never re-send — a re-dial
  rewrites no order frame. Verified against testnet that the label query returns terminal orders,
  cancelled and filled alike, so resolving on its answer cannot reject a fill that happened.
- **A Deribit reconcile lane died on the first socket close and never recovered.** The recon
  order-WS connected once at mount and nothing re-dialled it, so one venue-side close disabled
  reconciliation for the life of the process — 795 identical failures in a day, one problem that
  could not heal, while the daemon reported healthy. Every other socket in that crate already had a
  reconnect lifecycle; this one was given a dedicated transport and inherited none of it.
- **A held reconcile divergence re-announced itself every interval, and its alert store grew without
  bound.** 0.1.10 made the divergence visible and reasoned that the refresh path staying silent
  would bound it. It did not: an alert carrying no dedup key never takes the refresh path — it
  APPENDS — so every pass was a fresh raise. Measured on the CI box under `quarantine`: 396 warnings in a
  day for one bybit `PositionOnlyExternal`, with a genuinely new divergence indistinguishable among
  them, and ~1,560 store rows a day, each held in memory and projected into every published
  snapshot. A held divergence is a STATE, not an event: it is announced on entering and on clearing,
  with a slow backlog summary, and the store keeps one row per distinct identity. Confirming one now
  resolves what previously needed one confirm per pass.

### Security
- **A Rhai strategy — and a user indicator — could read any file on disk.** (#1520)

### Added
- **The venue sends a funding premium and we were discarding it** — recorded as `kind=perp_metrics`.
  (#1521)
- **A study can be RUN from the Studio GUI**, either tier, with one results surface. (#1523)


## [0.1.10] - 2026-08-24

### Fixed
- **The `ch_http` fake client survives `ETXTBSY`.** The v0.1.10 release re-validation refused to
  publish on `spawn: Text file busy (os error 26)` from a crate the tagged change never touched —
  which is exactly what that gate is for: a tag runs the FULL roster, a merge runs only the affected
  set. The test writes an executable and immediately spawns it; `std::fs::write` closes its handle,
  but a binary's tests run in parallel threads of one process and a `fork` in any other thread hands
  the child an inherited copy of the write descriptor. CI cannot see it — the fast lane runs
  nextest, one process per test — so only the tag's plain `cargo test` pass reaches it. Retried in
  the TEST, matched on the errno alone, and BOTH call sites are wrapped: the `select` twin has the
  identical race and had simply not lost the coin toss yet.
- **A HELD reconcile divergence now reaches the LOG, not only the in-memory ring.** Found on the
  live the CI box box an hour after the 0.1.9 deploy: `reconcile pass folded venue=bybit events=0
  alerts=2` every 60 seconds for hours, zero lines at WARN or above, and no way for anyone reading
  logs to learn WHICH two divergences were being held. `note()` writes a ring reachable only
  through the control channel, and that channel is off on the shipped daemon — so the COUNT reached
  the operator and the CONTENT did not. The raise path now emits one `warn!` carrying the
  divergence kind, venue and detail; the REFRESH path stays silent on purpose, because a divergence
  held for a day re-raises 1440 times and logging each is how an operator learns to filter out the
  whole channel, raise included. `crates/vike-ops/tests/held_divergence_is_visible_gate.rs` anchors
  on the ORDER rather than on presence — a `warn!` anywhere in that 4000-line file would satisfy a
  presence check while the raise path stayed silent.

## [0.1.9] - 2026-08-24

### Added
- **A ForexConnect-linked `vike-tradehub` is obtainable from a release.** A release built on a
  runner with the proprietary ForexConnect SDK staged now attaches an ADDITIONAL
  `vike-tradehub-fxcm` asset — the same tagged commit with `--features fxcm` — covered by the same
  `SHA256SUMS` and installed by nothing. The ordinary `vike-tradehub`/`vike-cli` assets are
  byte-identical: a feature build gains a hard `DT_NEEDED` on `libForexConnect.so` and dies at exec
  where those libraries are absent, so flipping the feature on the name the deploy installs would
  have broken the next deploy of the live daemon. `scripts/release_fxcm_artifact.sh` builds into a
  scratch target directory, reads the produced ELF's dynamic section (a `--features fxcm` build
  without an SDK compiles a STUB and exits 0, so the exit code proves nothing), re-checksums the
  default artifacts before emitting anything, and declines silently where no SDK is staged.
  `docs/ops/fxcm-forexconnect-the CI box.md` Step 5 carries what an operator must then do — the
  libraries are NOT in the release and `just fxcm-package <root>` is a required step, not an
  assumption.

### Fixed
- **The ForexConnect rpath reached `vike-fxcm`'s own test binaries and nothing downstream.** Found
  by building the artifact above, which is the first time anything downstream of that crate had
  ever been linked against the SDK: the binary came out with `NEEDED libForexConnect.so` and no
  rpath tag at all. `crates/bridges/fxcm/build.rs` was emitting all three link args correctly —
  cargo scopes `cargo:rustc-link-arg` to the targets of the package that emitted it, while
  `rustc-link-lib`/`rustc-link-search` propagate, which is why the `-lForexConnect` arrived alone.
  #1470's fix was therefore real and its reach was one package. The release build now sets the same
  args on the final link, pinned equal to `build.rs`'s own emissions by a gate and checked against
  the produced ELF before anything is published.

## [0.1.8] - 2026-08-22

### Added
- **The two runtime tools are OBTAINABLE.** A release now attaches the Dukascopy JForex sidecar jar
  and the pinned LightGBM binary (with the `PROVENANCE` file that `LightGbmCli::new` refuses to run
  without) alongside the two binaries, covered by the same `SHA256SUMS`. `just fetch-tools`
  downloads and verifies them into `<project>/bin/<tool>/`. Before this, `research run cohort`
  hard-failed on a fresh install because nothing shipped LightGBM, and Dukascopy only worked
  because a 43 MB jar was force-added past `.gitignore` into every clone's history.

### Fixed
- **The startup preflight's clock budget was sized against six reads, and the roster outgrew it.**
  The budget was a flat constant, sized against the measurement its own module doc
  records — six real reads, 1781 ms, the CI box 2026-08-09 — and never revisited as venues were added.
  By 2026-08-22 the wired roster's pinned healthy readings summed to 2973 ms, and one unreachable
  venue costs a full 3 s `CLOCK_READ_TIMEOUT`: 5973 ms needed against 5000 available. Observed on
  the dev box, where bybit's endpoint stopped answering and EIGHT venues reported their clock "not
  read" — `aster` among them, which runs against mainnet in practice and whose auth binds the clock
  into the order path. The budget is now DERIVED from how many venues are actually read
  (`clock_budget_for`), clamped between the old value as a floor and a ceiling, so a venue joining
  the roster buys the leg more time instead of squeezing the venues behind it. The sizing
  requirement — absorb one dead venue and still read the rest — is arithmetic over those same
  pinned measurements, so the roster outgrowing the allowance reddens a test instead of silently
  starving the tail. A per-venue deadline was considered and rejected;
  `docs/decisions/0027-clock-budget-derived-from-the-roster.md` records why.

### Changed
- **The JForex jar is no longer committed**, and `.gitignore` has no re-include chain left — the
  repository's one force-added path is gone. `crates/bridges/dukascopy/scripts/provision-jforex.sh`
  (and its `.ps1` sibling) fetch the jar instead of failing with "it is committed — bad checkout?".
  ⚠ **An offline clone therefore no longer gets a jar**; `JFOREX_BRIDGE_JAR` still names one
  outright and `--rebuild` still builds one from source. `README.md` carries both.
- **The jar's drift gate became a REPRODUCIBILITY gate.** With no committed copy there is nothing
  to drift from, but "the published bytes are re-derivable from the tag" is newly load-bearing, so
  `.github/workflows/jforex-bridge.yml` builds the jar twice from a cleaned build directory and
  requires the two byte-identical — the claim `build.gradle.kts` has always made and nothing ever
  checked. `crates/bridges/dukascopy/tests/jdk_pin_gate.rs` gained the release workflow as a fourth
  JDK pin site, plus a test that the release still builds the artifact the gate proves.
- `.github/workflows/deploy.yml` follows the manifest instead of naming assets, so it downloads
  every file `SHA256SUMS` lists rather than only the two it installs — `sha256sum -c` verifies the
  whole manifest, and the same command runs again inside the root-owned deploy helper.

## [0.1.7] - 2026-08-22

### Added
- **A live smoke for the startup preflight's two legs.** It runs the real preflight twice — once
  with an empty credential map (clock reads only, the honest baseline) and once with the real store
  — and fails if any venue lost its clock check *only* when the credential leg ran beside it. The
  two-run control is the point: the budget legitimately starves venues behind a slow clock read, and
  only a baseline separates that from the defect below.
- **`just app-paper`** — runs the GUI with no credentials in scope, so every venue stays paper by
  construction. `vike-app` opens the live gate on the mere presence of credentials and a live mount
  refuses to start without an operator risk budget, so on a populated box the obvious "look at a
  chart" run could not start at all. That refusal is unchanged; this is the other way out, made one
  command. An optional path argument captures a framebuffer PNG.
- **A data-plane-only credential seam in the trading daemon** — a credentialed feed without an armed
  exec, so a venue's market data can be live while its order path is not.
- **One placement rule for third-party artifacts, plus a ratchet that keeps it.**
- **The GUI contact sheet gets a judge**, and the GUI's CI-excluded shell gets a coverage ratchet.

### Fixed
- **The startup preflight's clock budget was being spent by the credential leg.** `run_preflight`
  interleaves the two legs per venue, and a credential probe is a blocking authed read on that
  venue's own 30 s agent — so ONE unreachable venue spent the whole 5 s clock budget and every venue
  behind it reported its clock "not read", blaming "an earlier venue's clock read" and sending an
  operator to inspect rows that were all fast and healthy. Measured: a geo-blocked probe sat ~20 s on
  a TCP connect and cost alpaca, aster and hyperliquid their clock checks while the clock leg itself
  had spent 3.2 s of its 5 s. `aster` mattered most — it runs against mainnet in practice and its
  auth binds the clock into the order path. Each credential probe is now measured and the deadline
  pushed out by its cost, so the budget bounds the clock leg and only the clock leg.
- **OKX options were never in the catalog.** The listing was fetched as a bare `instType=OPTION`,
  which the venue refuses with `400`/`50015`; the module already documented that failure and made
  the loop tolerant so spot/perp/futures survived, but never fixed the call — so `asset_classes()`
  advertised `AssetClass::Option` while the picker received zero options and a warning fired on
  every start. Now fetched once per underlying, paired as `uly` (not `instFamily`, which serves only
  two of the four families). Measured: okx 2004 → 5162 instruments.
- **Alpaca reported a transport failure as `HTTP 401`** — an unreachable token mint read as "bad
  keys", answering a network condition with a credential rotation. `AuthError` now separates a mint
  that answered and refused from one that was never reached.
- **Two warnings that fired forever on a healthy box.** The network row WARNed with a developer's
  TODO on every credential-free start; it is now `NotApplicable` with its reason when no venue would
  mount live, and still WARNs whenever a venue is actually being checked. The Windows Vulkan loader's
  two per-start warnings about absent validation-layer manifests are pinned below WARN on the console
  via a new `NOISY_TARGETS` family in `vike-log`, which obeys the same rules as the credential pins
  (an explicit mention wins; a pin may only ever narrow).
- **The catalog defaults REFUSE** — "cannot enumerate" stopped answering as "empty store".
- **A pinned research price window slid an hour instead of extending.**
- **`App::new` stopped demanding an unconstructible type**, and a stale `Cargo.lock` stops reaching CI.
- **An empty `[Unreleased]` now REFUSES the cut** — v0.1.5 and v0.1.6 both shipped on a note nobody
  read, because an advisory line in a scrolling release log is invisible.

### Changed
- **CI economics**: co-firing small feature lanes share one matrix leg, and the `~/.cargo` GitHub-cache
  round-trip is gone from setup — it restored what was already on disk.
- **47 rotted re-export spellings** retired across nine audited crates.

## [0.1.6] - 2026-08-19

Written after the fact (2026-08-22) from the merge history: this release and 0.1.5 were both cut
while an empty `[Unreleased]` was a legal no-op, so neither wrote a section at the time. It is a
refusal now — `just release` will not cut a version that says nothing about itself.

### Added
- **More venues join the trading daemon's live-wired set** — alpaca, ctrader, oanda, deribit and
  ig. Each mounts a credentialed market-data lane in `vike-tradehub` the way binance and friends
  already did; deribit, oanda and ig also gained their first LIVE market-data adapters (oanda's
  chunked-HTTP pricing stream, ig's Lightstreamer TLCP spoken raw over the shared transport stack —
  and ig serves no depth, which the adapter says outright rather than reporting an empty book).
- **The datahub authenticates.** It served history reads, backtest compute over client-supplied
  Rhai, and — in a backfill-serve build — writes into the store, to anything that could reach the
  socket. It now speaks the tradehub node handshake with scoped reads and writes; keys ABSENT
  serves exactly as before, so an existing single-user install is unchanged. The verdict:
  [`docs/decisions/0025-datahub-remote-posture.md`](docs/decisions/0025-datahub-remote-posture.md).
- **One backend address really is one address.** A daemon can advertise the datahub it fronts
  (a new `datahub_advertise_addr` key in `config.toml`), so a client that configured the tradehub
  reaches the history plane too instead of being configured twice. Unset changes nothing.
- **Settings hot-reload for the classified-safe subset.** The console and file log levels apply on
  a running daemon without a restart; every other key is answered `restart_required`, and
  `policy.toml` is structurally excluded — no future key can make a risk ceiling hot-appliable. The
  periodic summary also stopped over-reporting what is mounted.
- **Runtime mounts survive a restart.** A strategy mounted over the wire is remembered as daemon
  state and comes back after `systemctl restart` or a crash; only an explicit unmount forgets one.
  The journal is deliberately untouched — topology is session state, not order state.
- **A detached Windows daemon can be stopped cleanly**, and CI now compiles the daemon for Windows
  so that path stops resting on one developer's box.
- **Data-Manager coverage over the wire**: the Partial column follows the store in remote mode
  instead of being marked local-only.
- `vike-cli strategy-status` — ask a running node what it is running (human table, or the wire
  payload verbatim under `--json`). Asking used to mean compiling a throwaway probe.
- `just creds-audit` — find credential files by CONTENT across every box, so a stray copy of live
  keys is discoverable rather than remembered.
- Polymarket has a feeds-only build: the market-data plane compiles without the order signer.
- Under `external-quarantine`, a reconcile `MissingFill` that carries no `client_order_id` is HELD
  rather than folded — a foreign order's fill inside the lookback now needs an operator claim,
  while coid-linked fills keep folding.

### Fixed
Places where a capability gap was answering as data — all the same defect:
- A history store that cannot serve depth now REFUSES instead of returning an empty read, so "this
  store has no depth lane" stopped being indistinguishable from "there are no rows".
- Studio: a series scan that FAILED stopped rendering as an empty store, and `depth` being
  unreplayable stopped being a silent omission.
- oanda: the live tier REFUSES instead of vanishing, and a rejected exec stream stops looping in
  silence.
- fxcm: a login that FAILED reddens the smokes instead of being reported as a green test.
- The venue smokes honour `VIKE_SETTINGS_DIR` — they had been skipping in silence wherever the
  credential store was not directly above them.

Also:
- cTrader's OAuth token parser survives the venue sending either field spelling.
- Studio's ChatPane takes its provider keys instead of opening the credential store itself.

## [0.1.5] - 2026-08-19

### Added
- **Aster joins the daemon's live-wired venues** — capability only, and the MAINNET warning travels
  with it: no aster testnet credentials exist, so an armed aster lane reaches the real account.
- **Backends are managed from the GUI** — add, edit and delete backend records in place, with
  key-name hygiene on the credential names each record points at.
- **Settings are writable over the wire.** A `config`/`preferences`/`flags` key can be edited on a
  running daemon behind the control scope and a typed confirmation: the would-be file is validated
  with the same loader that will read it back, edited comment-preservingly (untouched lines stay
  untouched bytes) and landed atomically. `policy.toml` is special-cased — a risk ceiling is not a
  remote edit.
- A core-free direct-bar path: third-mode klines go venue-direct instead of through a trading core
  that has nothing to do with them.

### Fixed
- The Data-Manager grid follows the store — remote inventory arrives over the `HistStore` trait, so
  the grid describes the store you are connected to rather than a local handle.
- The `just cockpit` SOCKS tunnel host is a parameter defaulting to the CI box; it hardwired the parked
  Dublin route, which no longer answers.

### Changed
- No `pub use` shims on a code move: when a symbol changes homes every call site updates, and a
  re-export kept so old spellings compile is a second name that rots. Aster's leftovers are gone.
- [`docs/decisions/0025-datahub-remote-posture.md`](docs/decisions/0025-datahub-remote-posture.md)
  opened (proposed), with the bind guard both routes need.

## [0.1.4] - 2026-08-18

### Added
- Split-plane Phase-1 safety trio: the observe snapshot names its daemon (`WireNodeIdentity` —
  profile name, mounted strategy, paper-vs-live, build identity), a Windows console-ctrl graceful
  stop (Ctrl-C / window-close now run the same bounded teardown as the unix SIGTERM arm), and
  `LiveLock` — one live process per venue account, so two live traders can no longer silently
  rewrite each other's positions through reconcile.
- Runtime backend switching in the GUI: `BackendConn` makes the observe connection switchable (one
  active backend, total state clear on switch), a persisted backend registry owns the records, key
  resolution and the control arming gate, and both status surfaces name the connected daemon —
  LIVE/paper plus its identity.
- Rhai strategies mount LIVE: `[strategy] rhai = "<path>"` in a daemon profile compiles the script
  through the same engine builder the backtest arm uses and mounts it on the same paper/live cores,
  with the script's audit hash logged at mount. The verdict and its fence:
  [`docs/decisions/0024-rhai-strategies-live.md`](docs/decisions/0024-rhai-strategies-live.md).
- Studio as a thin client: Studio operates on the `HistStore` trait (remote-store capable) and its
  default build is DataFusion-free, so it can browse a served store with no local query engine.
- Backfill-on-demand on the datahub wire: `Request::Backfill` has the backend fetch a bounded kline
  range once, write it through to the store, and reply only after the served handle reads it back —
  capability-negotiated, so a client refuses locally against a server that does not advertise it.
- Strategy verbs on the tradehub node wire: the read-only `Request::StrategyStatus` and the
  control-scoped `WireCommand::UpdateParams` — inspect and retune a mounted strategy without
  restarting the daemon.

## [0.1.1] - 2026-08-18

### ⚠ BREAKING — settings unification (2026-08-05 … 2026-08-06)

**If you are upgrading an existing install, read [`docs/ops/upgrading.md`](docs/ops/upgrading.md)
first.** It is the full table: what moved, from where, to where, what today's binary does about the
leftover (silent / warns / refuses / migrates itself), and the exact command to fix each one —
including the `/opt/vike` in-place-upgrade case under an unchanged systemd unit.

The short version: every setting, credential and program-written file now lives in ONE
project-owned directory.

```text
<project>/settings/policy.toml  config.toml  preferences.toml  flags.toml
<project>/settings/secrets.env                 every API key, every venue
<project>/settings/state/…                     program-written files
```

- **Credentials moved** from `<repo>/.env` to `<project>/settings/secrets.env`. Absent credentials
  ARE the live gate, so a leftover `.env` used to mean every venue silently on paper with nothing in
  any log. An absent store with a `.env` beside it now **warns** on every credential read and in both
  `vike-cli secrets` subcommands, naming both paths — a finding, never a refusal, because `.env` is
  also a legitimate systemd `EnvironmentFile`. Nothing reads, prints or moves its contents.
  The interim `~/.vike/secrets.env` and `~/.vike/secrets.enc` slots existed for about a day and are
  gone; they cannot be warned about, because the home-directory resolution that found them was
  deleted with them.
- **Settings TOMLs moved** from `~/.vike/*.toml` to `<project>/settings/*.toml`. Silent — an absent
  file means the code default.
- **`<project>` is resolved at RUNTIME** by walking up for a project marker: a checkout's WORKSPACE
  ROOT (the outermost `Cargo.toml` declaring `[workspace]`), else a deployment's own `settings/`
  directory. `VIKE_SETTINGS_DIR` names it outright and skips the walk — set it on a deployment.
- **Refused at startup, never ignored** (`vike-app`, `vike-cli`, `vike-tradehub`):
  `VIKE_MAX_ORDER_NOTIONAL` and `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` (→ `policy.toml`'s
  `max_notional_per_order`; a ceiling any exported variable can raise is not a ceiling),
  `VIKE_SECRETS_PASSPHRASE` (nothing consumes it), a leftover `<project>/vike.toml`, and the removed
  keys `policy.toml`'s `max_total_exposure` / `[rate] max_utilization` and `preferences.toml`'s
  `rate_utilization`.
- **State files** (`workspace.json`, `alerts.json`, `layouts/`, `studio_workspace.json`, `pace.json`,
  the Telegram ledger, the cTrader token, the REPL history, the rolling trace log) moved under
  `<project>/settings/state/`. Four migrate themselves on first write; the rest are silent and cost
  a layout or a cache, never trading.

### Added
- Security/CI hardening (2026-07-08 audit): `cargo-deny` supply-chain gate (`deny.toml` +
  `deny.yml`), SHA-pinned GitHub Actions with least-privilege tokens + job timeouts, pinned
  toolchain (`rust-toolchain.toml` 1.96.0) + MSRV, workspace `unsafe_code = "forbid"` lint,
  `rustfmt` gate, `gitleaks` pre-commit hook, checksum-verified JForex/Temurin/Gradle
  provisioning, `.gitattributes` EOL policy, a `justfile` task runner, and a weekly
  Windows compile check.
- Dependabot (alerts + cooldowned version updates) and this release pipeline.

<!-- Add the next release's user-facing changes under [Unreleased]. `just release` cuts the
     section under a version heading via scripts/changelog_release.sh — no hand edit needed.

     That script REFUSES to cut an empty [Unreleased], and the refusal lands before the commit,
     the tag and both pushes. It used to print a note and carry on, which is how 0.1.5 and 0.1.6
     shipped with nothing here. A release that genuinely has nothing to say is still possible, but
     it has to be said out loud: ALLOW_EMPTY_CHANGELOG=1. -->
