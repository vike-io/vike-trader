# Changelog

All notable changes to vike_trader_rust are recorded here.
Format loosely follows [Keep a Changelog](https://keepachangelog.com/); versioning is
[SemVer](https://semver.org/) — pre-1.0, a minor bump may include breaking changes.

Per-release notes are also auto-generated on GitHub (the PRs merged since the previous tag)
by `.github/workflows/release.yml`; this file is the curated human summary. Cut a release with
`just release <X.Y.Z>`.

## [Unreleased]

## [0.1.20] - 2026-09-08

### Added
- **A gate over the multicall's feature parity, which three files asserted and nothing compared.**
  The release builds the seven tools twice — once per package with an explicit `--features` list,
  once as `vike --features full` — and `release.yml`, `crates/vike/Cargo.toml` and
  `scripts/ci_feature_suite.sh` each said those two spellings "must stay equal" while pointing at
  each other. ⚠ No lane could see a drift: the `multicall` lane compiles `-p vike --features full`,
  so a tool row that fails to COMPILE is caught, but a feature merely DROPPED compiles fine — a
  `vike-recorder` without `polymarket` records nothing for that venue and looks exactly like an
  unconfigured one, while every per-crate lane stays green because each crate is still built with its
  own features elsewhere. `multicall_gate.rs` now resolves `full` transitively and compares it to the
  workflow's per-package list. They agreed on all ten forwards, which is what made this a cheap
  moment to gate rather than a bug report; proven able to FAIL by dropping
  `vike-recorder/polymarket` on a throwaway branch and watching it name that feature.
- **The Windows CLI installs from the release: a `cargo binstall` override pair and a scoop bucket.**
  `release.yml` already cross-built `vike-cli.exe`; nothing installed it. Now
  `[package.metadata.binstall.overrides]` carries `x86_64-pc-windows-gnu` AND
  `x86_64-pc-windows-msvc`: binstall resolves EVERY triple `detect_targets` returns, and on an msvc
  host that list already carries the gnu fallback (`detect_alternative_targets`), so the msvc row is
  not what makes the install possible — it makes the FIRST probe the right one instead of ten
  default-template 404s. The public mirror is now also a scoop bucket:
  `scoop bucket add vike https://github.com/vike-io/vike-trader && scoop install vike-cli`.
  ⚠ The scoop manifest is RENDERED at publish time, never committed: it carries a concrete version,
  url and digest, and the DIGEST alone settles it — it belongs to a binary the release runner has
  not built yet. (This read "this tree has none of the three (every crate is `0.1.0`, the tag is the
  version…)". The version half stopped being true later in the same release — see *The version is
  the tag*, below — while the digest half is permanent.)
  `scripts/scoop/vike-cli.json.tmpl` is the committed
  template; `scripts/render_scoop_manifest.sh` fills it from the tag and the release's own
  `SHA256SUMS`, and `scripts/publish_mirror.sh` writes the result to `bucket/vike-cli.json` in the
  mirror tree BEFORE it pushes — which also moved every `gh`/slug/manifest refusal ahead of the
  source push, so they now fail while nothing is public.
  ⚠ **`scoop update` cannot advance this bucket, and the install docs say so rather than promising
  otherwise.** The mirror is one ORPHAN commit per publish (`git init` + `git push -f`, so no
  earlier tree can hand back a file later withheld) and scoop updates a bucket with a plain
  `git pull -q`, which refuses unrelated histories; a later release is installed by removing the
  bucket and adding it again. `docs/decisions/0037` records the scoop deferral as LIFTED, that
  update gap as a deferral of its own (a bucket repository with real history), and Homebrew as
  still waiting on a macOS runner that does not exist.
- **⚠ BREAKING BY DEFAULT — a live `vike-tradehub` mount now has an automatic stop it was not
  asked for: the CONNECTION-state dead-man.** `policy.toml`'s new `link_deadman_grace_ms` is **ON
  when absent** (`vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS`); `0` is the only off, and the armed
  range is `MIN_LINK_DEADMAN_GRACE_MS`..=`MAX_LINK_DEADMAN_GRACE_MS`, refused at load by name and
  never clamped. When a venue's own bridge reports its market-data link `Disconnected` for longer
  than the grace, **that venue's** resting orders are cancelled (`OrderIntent::MassCancel` scoped to
  it, so a bybit socket death leaves a binance book alone) and — under the SHARED `deadman_action` —
  HALT engages: `Halted` on every engine plus the one cross-process sentinel file, which is
  process-wide because that file is.
  **It is not the silence switch with a new number.** `deadman_timeout_ms` counts INGEST, cannot
  tell a closed market from a dead socket, and stays OPT-IN for that reason; this one reads the
  `FeedStatus` the bridges DISCLOSE, and `Stale` — which is exactly what a weekend or an evening
  close looks like — never counts in either direction. That is what lets it have an armed default.
  ⚠ **It does not cover every venue, and cannot be made to from the file.** Where it arms is a
  per-venue table read from each bridge's emitter (`vike_model::link_deadman_default`): every FX and
  equities venue is OFF because its market has sessions, and most roster venues disclose no
  disconnect at all and are declared residuals. ⚠⚠ **And a second cut:** a venue arms only if this
  daemon subscribes the lane that carries the disconnect. As first shipped that cut was the binding
  one — binance/aster/bybit/okx disclosed a disconnect from their L2 DOM depth feed alone, which
  `vike-tradehub` does not subscribe, so a default (non-`polymarket`) build armed NOTHING and
  constructed no switch. **Closed before release** by the Changed entry below (the CEX tick pumps
  now disclose); the cut itself remains a wiring fact no `policy.toml` line can reach, in either
  direction. The mount logs ONE line per mounted venue at
  startup saying armed-or-why-not, and builds no switch at all when none is armed. `vike-app` and
  the headless IBKR mount stay unwired, as they were for the silence switch.
  `docs/decisions/0038-the-dead-man-observes-the-connection-not-silence.md` is the ruling;
  `docs/ops/kill-switches.md` carries the operator row, the residuals and the two gaps.

### Changed
- **`vike-cli` resolves the datahub node keys from the credential store, not only the environment.**
  It read `VIKE_DATAHUB_OBSERVE_KEY`/`VIKE_DATAHUB_CONTROL_KEY` from `std::env::vars()` and nowhere
  else, so a pair sitting in `<project>/settings/secrets.env` — where its own refusal text sends an
  operator — did nothing. ⚠ The same defect had already been found and fixed one key pair over: the
  module doc records that both TRADEHUB node keys "used to read `std::env::var` and nothing else, so
  a correctly-configured box answered 'nothing to do' and exited", and the datahub pair was then
  written in that shape with a comment claiming it worked "exactly as `node_keyring` does". The worst
  part was the refusal naming the wrong remedy. `datahub_keyring` now mirrors `node_keyring` — env
  first, store second, lazily, for the five arms that can dial a datahub — and `credential_store` is
  factored out so the read has ONE spelling. Measured against a keyed datahub: keys in the store
  alone gave "no node keys were supplied"; the same keys resolve now.
  ⚠ This WIDENS which arms open the venue-key file. The rule was "a provenance or backtest command
  has no business opening the file that holds every venue key on the box"; it is now "an arm that can
  dial a node may open the store", and `config`/`secrets`/`init`/`indicators` still never do. The
  narrower version — read the store only for a REMOTE invocation — was refused on measurement:
  `cmd::data`'s address resolves to a default whether or not `--addr` was passed, so "did this go
  remote" is not answerable from argv.
- **The deploy installs the tools a box could not previously run.** `deploy/sbin/vike-trader-ci-deploy`
  installed `vike-tradehub` and `vike-cli` and nothing else, so everything else on the box arrived by
  hand — which is how its `vike-recorder` came to be months old with nothing saying so. It now also
  installs `backtest`, `vike-study` and `tearsheet` as REAL per-tool binaries, selected by the rule
  *no systemd unit names it*, so installing them restarts nothing. `tearsheet` is the one worth
  naming: it reads the live daemon's journal and prints the same metrics a backtest over those fills
  would, on a box that could not previously render its own performance. Each goes down through a temp
  file and `rename(2)` — writing over a RUNNING executable fails with `ETXTBSY`, and a human's
  `backtest` can be mid-deploy. A missing asset is REPORTED per name, never fatal: the helper is
  installed out of band and will meet older releases. `status` now RUNS each tool's `--version`
  rather than listing names, so a tool that cannot execute is visible instead of implied.
  ⚠ Briefly this installed the `vike` multicall plus symlinks instead (631 MB of binaries against
  146 MB) and that was REVERSED on measurement: the deployment disk had 7 TB free, so the saving was
  0.009% of it, the deploy downloads every asset either way, and a dangling link is
  `status=203/EXEC` on a tag with no reviewer. The container image still ships the multicall, where
  the size genuinely matters because every user pulls it.
- **The version is the tag.** Nineteen releases shipped as `0.1.0`: all 64 member manifests spelled
  `version = "0.1.0"`, `[workspace.package]` carried no version, and `just release` cut a tag while
  bumping nothing. ⚠ The costs were user-facing, not cosmetic — `cargo binstall` had to resolve
  `releases/latest/download/` because `{ version }` rendered ONE string for every tag, so a user
  could install the **newest release and no other**, and a re-run reported "up to date" whatever the
  tag (`--force` was the way round it); `scoop list` said `0.1.19` while the binary it installed said
  `0.1.0`. The root `[workspace.package]` now carries the one `version`, every member takes
  `version.workspace = true` — the version is written **once** where it was written 64 times — and
  `just release X` rewrites and commits that line, refreshes `Cargo.lock`, and only then tags `vX`,
  so the tag cannot point at a tree that disagrees with it. `binstall` moved to
  `releases/download/v{ version }/`, which is exactly what `crates/vike-cli/Cargo.toml` had promised
  for "the day the release recipe bumps this manifest's version to the tag". A build BETWEEN releases
  reports the last released version; the identity that moves is the git commit
  `vike_buildinfo::version_line` already prints beside it. Three manifests keep their own versions
  because they are not workspace members (`crates/bridges/ctrader/protogen`, `fuzz`, the vendored
  `ibapi`), and `Cargo.lock` was updated by package NAME because five third-party crates genuinely
  are `0.1.0`.

- **The connection-state dead-man now REACHES the venues a default `vike-tradehub` daemon mounts.**
  It shipped ON by default and armed nothing on a non-`polymarket` build: binance/aster/bybit/okx
  disclosed a dead link from their L2 DOM depth feed alone, and the daemon's CEX arm subscribes the
  kline lane and the quote/trade/book tick pump — not that one. The pumps now disclose their own
  transport state (`crates/bridges/binance/src/family/depth.rs`'s `disclose_link` and its bybit/okx
  twins) straight onto the core tick lane they already write ticks to: one `Disconnected` per
  outage, one `Live` on the FIRST FRAME back rather than at connect, so a venue that accepts a
  socket and drops it in a loop reads as one outage instead of resetting the grace forever.
  Chosen over subscribing the DOM feed because that would open a SECOND socket to the same
  `@depth`-class stream on the same host, maintain a second copy of the same book and add a
  five-minutely weight-50 REST re-seed — for a signal the first socket already has, delivered into
  a sink verb that is a no-op in this daemon. Zero extra bytes on the wire. The per-venue table is
  still the authority (every FX and equities venue stays OFF — a close is not an outage), each CEX
  row now cites both emitters, and a gate panics if the mount ever stops subscribing a disclosing
  lane. `docs/decisions/0038-…` records the closure under its own "what would reopen this" clause;
  the residual is that no production link death has yet been watched tripping it.

### Removed
- **The `poly-l2-monitor` files left this repository for the one they describe.** Five files — a
  health check, its unit, its timer and two harnesses — watched a SEPARATE project's ClickHouse tape
  (`poly-l2-recorder`) while living here. ⚠ The tree had already NOTICED: two gates carried declared
  exemption rows saying so in as many words ("a `Type=oneshot` health check for the SEPARATE
  closed-repo poly-l2-recorder"; "it belongs to that repo's deployment, not this one's"). That is the
  shape worth remembering — an exemption whose REASON is "this belongs to another project" is a
  request to MOVE the file, not a reason to exempt it, and writing the exemption froze a correct
  diagnosis into infrastructure. They are in that repository now; both tables lost their rows and
  gained a note saying why. `vike-notify` deliberately stayed — it is shared alerting infrastructure
  three other units hang off. The three prose citations of the moved files are KEPT, as
  `DEAD_PATH_EXCEPTIONS` rather than deletions: they are the evidence for an incident write-up, and
  the citation gate cannot follow a path across a repository boundary.

### Fixed
- **The bybit and okx market-data pumps dialled with NO connect bound**, so a black-holed route
  pinned their feed thread for the OS's own SYN ladder (~127 s on Linux defaults) with the stop
  flag already raised behind it. The binance/aster twin of the same pump had this fixed on
  2026-08-08 and these two were missed; `crates/vike-ops/tests/feed_stop_windows_gate.rs` found
  them the moment those files named a feed driver. Both now dial through
  `vike_bridge_core::ws_proxy::connect_ws` under `pump_spec::CONNECT_10S`, the same bounded arm
  every other feed takes. It matters more than it did: this is now the lane the connection-state
  dead-man hears a bybit/okx link die on, and 127 s of unstoppable dial is longer than that
  switch's own default grace.
- **The absent-`deadman_timeout_ms` warning no longer says "this live mount has NO automatic stop".**
  That sentence became false the moment the switch above shipped armed by default, and an operator
  acting on it would either disable a real protection or write a key that halts an FX mount every
  Friday. The message now names WHICH switch is off, says the other is on, and points at the
  per-venue lines beside it; a test asserts the retired sentence cannot come back.

## [0.1.19] - 2026-09-05

### Added
- **`vike-cli secrets set KEY` — the one credential writer the CLI is allowed to have.** The value
  comes from **stdin** or from a **named environment variable** (`--from-env NAME`) and NEVER from
  the command line: `secrets set KEY VALUE` is a usage error, because argv lands in shell history
  and in `ps` output for every user on the box, and the refusal deliberately echoes nothing the
  operator typed. It is a SECOND CALL SITE of `vike_secrets::save_credentials`, the workspace's one
  byte-preserving atomic upsert — never a second writer — so every other line, comment, blank and
  their order survive verbatim. The key is validated against `vike_model::credential_keys` and an
  unknown name is refused BY NAME with the nearest real ones; an ABSENT store is refused too,
  naming `vike-cli secrets template`, because creating the file stays the operator's decision. Each
  write appends one `change_journal` `credential_write` record (key NAMES only, never a value).
  ⚠ The MCP surface stays credential-free and none of this is reachable from it.
  `docs/decisions/0036` fixed this exact shape BEFORE the writer existed and now records that it
  exists; `crates/vike-ops/tests/credential_writer_gate.rs` pins the set of files in the whole tree
  that may call a credential writer, with the reason each may.
  Four narrowings landed in review, each now covered by a test: **no unrecognised token on `set` is
  quoted back at all** (a value starting with `-`, which base64url allows, used to be printed
  verbatim to stderr by the generic unknown-option arm); **`--file` is REFUSED on `set`** and stays
  an inspection flag on `list`/`path`/`template`, because it aimed a write at any path the operator
  named — `$VIKE_SETTINGS_DIR` is how a scripted run targets another project; **a multi-line value
  is refused** rather than written (it truncated the credential at the newline and turned the
  remainder into a second `KEY=VALUE` line for a key nobody named); and **a `KEY__LABEL` account
  name is refused without suggesting the unlabelled key**, which is a different account's live
  credential.
- **`vike-cli.exe` is now a Windows release asset.** The Windows job cross-built the GUI and nothing
  else, so a Windows user got `vike-app.exe` and no terminal surface at all — no `config check`, no
  `secrets path`, no `backtest`, and no way to run the agent-facing `mcp` server. ⚠ The standalone
  `backtest` engine is still a **Linux/container asset only**: `backtest --local`, `sweep --local`
  and the whole `data` verb therefore need an engine a Windows user supplies themselves, and the
  missing-engine message says so and names the command that builds one.
- **`vike-cli data fetch` and `vike-cli data seed-demo`** — market data into the hist store without
  knowing a second binary's name. Both drive the standalone `backtest` engine as a child process;
  the CLI itself stays DataFusion-free.
- **`vike-cli backtest --local` and `vike-cli sweep --local`** — the same run on this machine, with
  no `vike-datahub` to talk to, by driving that same engine. `--preset`/`--script` still apply
  client-side in both modes, so a local run is a rehearsal for a remote one.
- **`--json` on `secrets list` and `init --dry-run`**, the two machine-readable surfaces that were
  missing. `secrets list --json` prints key NAMES and their venue, never a value.
- **`cargo binstall --git https://github.com/vike-io/vike-trader vike-cli` installs the released
  CLI without a toolchain.** `crates/vike-cli/Cargo.toml` gained a `repository` field naming the
  PUBLIC mirror and a `[package.metadata.binstall]` section spelled against what `release.yml`
  ships — a bare Linux binary, no archive, no platform suffix — and read from cargo-binstall's own
  documentation rather than from memory. Linux only: there is no `vike-cli.exe` and no macOS
  build. It points at the mirror's LATEST release, because a release bumps no crate version and a
  `v{ version }` URL would never resolve. `crates/vike-ops/tests/packaging_gate.rs` renders the
  template itself and refuses one naming an asset no `SHA256SUMS.part-*` line carries.
- **The public mirror's release now carries the binaries.** `scripts/publish_mirror.sh` copied
  only the docs-data JSON from the private release, so a public `cargo binstall` would have
  404'd on its first run. It now downloads `SHA256SUMS`, then every asset it names, verifies each
  against it, and uploads them all plus the manifest — derived from the manifest, never a second
  list, the rule `release.yml`'s attach loop already follows. The docs-data handling is unchanged.
  Nothing becomes public until the owner runs the publish for a tag.

### Changed
- **⚠ `vike-cli`'s exit code is now a ladder rather than a boolean, which is a behaviour change for
  anything wrapping it.** `0` (success) and `1` (ran and failed) keep their exact meanings, so a
  script written against the old two-value behaviour still reads correctly — but failures that used
  to exit `1` now exit `2` when the command line was wrong (an unknown verb or flag, a missing
  required flag, no subcommand at all) and `3` when a service could not be reached (no datahub, no
  node, a refused or timed-out connection). The point is the distinction a wrapper needs: `2` is
  FIXED, `3` is WAITED on, `1` is the ordinary failure. `crates/vike-cli/src/exit.rs` is the
  authority; `4` and `5` appear in that enum and are RESERVED — nothing produces them yet, and a
  script may not branch on them.
- **The pinned Rust toolchain moved 1.96.0 → 1.97.1.** `rust-toolchain.toml` is the only thing
  making the four build environments agree — the Windows dev box, CI (the CI box), the latency runner
  (the latency box) and the CI box's test clone do not share a default toolchain — so the pin is the whole
  mechanism, and no box was touched individually.
- **The workspace moved to Rust edition 2024.** 64 manifests; the vendored `ibapi` copy and the
  workspace-excluded `ctrader/protogen` recipe deliberately stay on 2021 — the first is
  byte-compared by its drift gate. The migration was cheap for a reason worth knowing: the
  workspace's `unsafe_code = "forbid"` had already eliminated both of the edition's headline
  breakages (zero `unsafe fn`, zero `static mut`). What it DID force is the useful part: every
  `std::env::set_var` left the tests — 39 sites across 7 files — because the edition makes it an
  `unsafe fn` and the forbid cannot be lifted. Each test now drives a pure core over injected
  values; no settings-registry row moved, because every wrapper keeps its explicit
  `env::var(CONST)` read. `gen` became a reserved word (the two generated-code modules are
  `codegen` now), let-chains are stable (clippy's machine fix collapsed every collapsible `if` nest in the tree, 157 files), and the style-edition reformat
  is the bulk of the diff — with ONE of its hunks reverted after review: a `return` in `vike-app` that clippy, running under the `thin` feature, judged needless and that the default `fat` build depends on. `resolver` stays `"2"` on purpose — dependency resolution is a separate
  decision from the language edition.

### Fixed
- **The credential store's MODE, its SYMLINK and its DUPLICATE keys all survive a write now**
  — `vike_secrets::save_credentials`, so the GUI's Connections editor and cTrader's token rotation
  get these as well as the new CLI verb. The atomic temp+rename replaces the destination inode,
  which silently discarded two properties: a `chmod 600` store came back at the umask default
  (0644 — world-readable live signing keys, at the moment of a rotation that printed success), and
  a store kept as a SYMLINK into an encrypted or root-owned volume was replaced by a regular file
  full of plaintext inside the project directory, while the real file kept its pre-rotation
  contents for anything reading it by its own path. The mode is carried across now (a store this
  code CREATES starts at 0600 rather than inheriting the umask), and a symlinked store is resolved
  and written THROUGH. Separately, the upsert replaced only the FIRST line matching a key while
  `parse_dotenv` is LAST-wins — so rotating a key a store happened to hold twice (an ordinary
  hand-edit, or `secrets template >>`, the append typo of the documented `>`) reported "replaced",
  journalled `Applied`, and left every loader in the workspace reading the OLD credential; every
  matching line is replaced now. A failed write no longer leaves `.secrets.env.tmp-<pid>` beside
  the store holding the whole thing in plaintext. ⚠ The mode and symlink halves are Unix-shaped and
  are therefore covered by `#[cfg(unix)]` tests that the Windows dev box cannot run.
- **A shipped binary no longer carries the build box's paths, and a release refuses an asset that
  names the box.** rustc embeds every registry crate's source path — on the runners, a path under
  the runner account's home — in panic-location strings, and nothing remapped it; the same tokens
  `publish_mirror.sh` refuses in SOURCE went out in every binary. Each asset-building job of
  `release.yml` now takes `rust-ci-setup`'s `remap-paths`, which appends `--remap-path-prefix` for
  the checkout and `CARGO_HOME` to `RUSTFLAGS` (and `-ffile-prefix-map` to `CFLAGS` for the C in
  the tree), then runs `scripts/refuse_box_paths.sh` over every asset its manifest part names. The
  token set is ONE file, `scripts/forbidden_tokens.ere`, read by that guard and by
  `publish_mirror.sh`'s FORBID scan — which now also runs over the staged release assets before
  they reach the mirror. ⚠ The remap alone would have left the next tag RED on every asset lane,
  because the remap rewrites only what the compiler emits: measured on the real v0.1.18 assets,
  `vike-app-fat`/`-thin`/`.exe`, `backtest`, `vike-datahub` and the `vike` multicall each carried
  the runner's checkout path through `env!("CARGO_MANIFEST_DIR")` — the dev-checkout rung of the
  hist-store ladder, a string literal no flag touches — and everything linking `vike-ml` carried
  a box name in two error strings. Both producers are fixed: the three shipped ladder sites
  (`vike-app`'s `studio_store_root`, `vike-backtest`'s `binutil::repo_default`, `vike-datahub`'s
  `run`) pass that rung under `cfg(debug_assertions)` only, so a `--release` build resolves from
  the project walk down and carries no checkout path at all (`vike_model::store_path`'s
  `resolve_store_root` takes `Option<&Path>` for it, and its module doc argues the trade), and
  `vike-ml`'s error text names "the training box" rather than the box. `build_lightgbm.sh` also
  stops recording `hostname` in `lightgbm.PROVENANCE`; the copy cached on the release runner still
  carries one, but that hostname is not in the token set, so nothing refuses it and no rebuild is
  forced — this entry used to say the guards would, which was wrong.
- **The public mirror's release withholds what embeds somebody else's proprietary code, and the
  binstall URL cannot be stolen by the dataset release.** Two findings against the mirror step
  above, both from reading the REAL v0.1.18 manifest: the derived set would have re-published
  `vike-tradehub-fxcm` (the release runner DOES stage the ForexConnect SDK — the "no CI runner has
  it" assumption was stale) and the JForex shadow jar, which bundles Dukascopy's client libraries.
  `scripts/nonfree_release_assets` is the one spelling of what stays private; the asset step subtracts
  its rows from the manifest before it downloads, verifies or uploads anything, and the mirror's
  `SHA256SUMS` is the private one with those rows removed. And `releases/latest/` — the URL the
  template renders — is a property any release on the mirror could take: `publish_starter_data.sh`
  recreates the `starter-data` release at HEAD on every refresh, so a refresh after a snapshot made
  THAT the latest release and 404'd the install line. It now passes `--latest=false`, the version
  release passes `--latest`, and `packaging_gate.rs` holds both scripts and the withheld set.
  `docs/decisions/0037-…` records the withholding verdict and what would lift it.
- **The toolchain gate was blind to the copy every CI job actually uses.**
  `the_toolchain_version_has_exactly_one_value` scanned `.github/workflows/` and nothing else,
  while every Rust job installs its compiler through `./.github/actions/rust-ci-setup`, whose
  `toolchain:` line is a version literal like any other. `mutants.yml` carried a comment asserting
  that input was "counted once" by the gate; it was not. An unscanned copy contributes no entry,
  so a stale value there left the one-value assertion green while the shared setup step installed
  one compiler and `rust-toolchain.toml` overrode it to another. The scan now walks
  `.github/actions/**/action.yml`, and `the_shared_setup_action_is_scanned` fails if it ever stops
  reaching it — the one-value pin cannot catch that class on its own.

## [0.1.18] - 2026-09-04

### Fixed
- **`docker pull vikeio/vike-tradehub:latest` works.** It never had: `release_container_image.sh`
  pushed `:$version` and nothing else, so the floating tag was never created on either registry —
  while the README's FIRST command is that pull. The failure was invisible from the inside, because
  `pull vikeio/vike-tradehub:0.1.17` succeeds and the registry looks healthy to whoever published
  it; only a stranger following the README hits the 404. `push_latest` now moves the tag after each
  push, and REFUSES to move it for a pre-release — `release.yml` fires on `tags: ['v*']`, which
  matches `v0.2.0-rc1` as readily as `v0.2.0`, and `latest` means "the newest thing a stranger
  should run".
- **The published image's OCI labels pointed at a repository nobody can open.**
  `org.opencontainers.image.source` and `.documentation` both named the PRIVATE development
  repository, so both 404'd for everyone who pulled the public image. `source` is now the public
  mirror; `documentation` is the website rather than a repo path, because `docs/` is excluded from
  the mirror and a mirror blob URL would 404 just as reliably.
- **The desktop app showed an internal repository name to its own users.** The window title and the
  About menu read `vike_trader_rust`; they now read **Vike Trader**, the product name.

### Changed
- **The public mirror is `github.com/vike-io/vike-trader`** (was `.../vike`), and this repository is
  `vike-io/vike-trader-private` (was `vike_trader_rust`). Bare "vike" search results belong to
  vike.dev, the Vite meta-framework, so the brand is "Vike Trader", two words. `STARTER_BASE` —
  compiled into every shipped binary for the starter-dataset download — was REWRITTEN rather than
  left riding GitHub's rename redirect: a redirect is a trap rather than a safety net, working right
  up until somebody claims the freed name, and "vike" is a common word. `starter_dataset_gate` now
  detects both private-repo spellings for the same reason.

## [0.1.17] - 2026-09-03

### Added
- **The source is public** — `github.com/vike-io/vike-trader`, published by `scripts/publish_mirror.sh` and
  by nothing else. This repository stays private, and not as a preference: every workflow here runs
  on self-hosted runners and six fire on `pull_request`, for which GitHub runs the workflow file
  FROM THE FORK — so a public repository would mean anyone can execute code on the box that signs
  orders. The script selects by ALLOWLIST (a directory added next month does not ship until someone
  adds it), redacts the box names, paths, usernames and private IPs that ~300 files mention in
  passing, then scans the RESULT and **refuses to publish** if a forbidden token survived. It is a
  SNAPSHOT, one commit per release, because a mirror with history hands back every withheld file
  from an earlier commit. `docs/`, every `CLAUDE.md`, `.github/`, `justfile`, `scripts/` and the
  live `settings/*.toml` are never published, and `publish_mirror_gate.rs` holds that by RUNNING
  the script and inspecting the tree it produced.
- **A fresh install has data.** Three routes, because the reason a new box is empty differs: a
  deterministic **synthetic demo tape** (`vike-data`'s `demo` module, written under venue `demo` so
  it can never be mistaken for a real row) via `backtest --seed-demo`; a **keyless venue fetch**
  (`--fetch`, public klines, no credentials); and a **published starter dataset** (`--fetch-starter`,
  SHA256-verified) for a box that cannot reach a venue at all. Plus `--export` to Parquet.
- **The venue capability tables can now leave the repo.** `vike-ops`'s `docs_data` bin renders
  `venues.json` and `stats.json` as release assets, derived from the CI-gated tables themselves
  (`VENUES` through `caps_for`, `venue_margin_support`, `fee_schedule_for`, `amend_semantics`,
  attribution and `venue_tif`) — so a docs page or a venue matrix reads what the gates enforce
  instead of hand-copying it. Every hand copy of a roster in this tree has rotted.

### Fixed
- **The Connections picker never saved which backend it picked, and a bare launch signed with the
  wrong key.** Two defects on one promise — *configure a node once, launch bare afterwards* —
  found by installing the released Windows thin client and driving it against three live
  `vike-tradehub` daemons. Nothing ever SET `backends.json`'s `active` pointer, so a node added and
  connected in the GUI stayed `"active": null` and the next launch silently observed a DIFFERENT
  daemon with no error; and the startup connect discarded the record and kept only its address,
  signing with the fixed `VIKE_TRADEHUB_OBSERVE_KEY` instead of the record's own key name —
  `tradehub observe auth denied: bad mac`, on a retry loop, forever. `backend_conn::active_after`
  is now the one writer of the pointer, and `startup_backend` resolves a whole RECORD;
  `--observe ADDR` is unchanged.
- **The viewer no longer refuses to start.** A thin build used to `exit(2)` without
  `--observe <host>:<port>`, so a mistyped flag closed the window instead of opening one. The
  address now falls back to the registry's active backend and then to a local default, and the
  status bar says whether it connected — which is the thing a person can act on.
- **The containerised thin client showed a blank page and exposed its VNC port.** Whoever reached
  `6080` drove the same GUI, `x11vnc` listened on all interfaces without a password, and the noVNC
  page needed a manual click before it rendered anything. The port is now loopback-bound with an
  opt-in password, and the page auto-connects.
- **Linux GUI windows drew tofu boxes.** Every font path in `install_fonts` named a Windows
  location, so the window-control glyphs (`✕ ─ □`) resolved to nothing off Windows. Font
  candidates moved to `vike-app-core` with a test that every named family is BOUND on the platform
  the test runs on — the gate whose absence shipped this.

### Removed
- Six inert planning documents, and `docs/superpowers/`'s index is now gated as 1:1 with the tree
  rather than claiming to be.

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
