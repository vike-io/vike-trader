//! `build_strategy_file`: render the cdylib TEMPLATE (`vike-rust-strategy`'s `template/*.in`) around a
//! user strategy's source, invoke `cargo`, and return the compiled artifact's path —
//! content-addressed so an unchanged file is never rebuilt, and one shared build directory per build
//! folder so a new file rebuilds only its own crate.
//!
//! ⚠ **This file is `render.rs`, not `build.rs` — a rename, not a style choice.** It used to be
//! `src/build.rs`, and `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s scanner classifies a read's
//! `Layer` partly by PATH SUFFIX: `rel.ends_with("/build.rs")` scores a file `Layer::BuildScript`
//! unconditionally, before it ever asks whether the file is actually a build script. This file
//! never was one — it is an ordinary library module — so a `std::env::var("CARGO")` call living
//! here was misclassified as a compile-time-only read and slipped straight past
//! `LIBRARY_PIN`'s "may only shrink, never grow" ratchet, which exists to catch exactly a library
//! reading process environment its caller cannot see or override. Renaming the file was the clean
//! fix on its own terms: a genuinely-named module cannot collide with the scanner's build-script
//! heuristic, and the `CARGO` read that had been hiding behind the collision is now threaded out
//! as an ordinary parameter (see [`build_strategy_file`]'s own doc) — the third one out of this file, the
//! same shape already applied to `workspace_root`.
//!
//! ⚠ **This crate is the workspace's FIRST `cdylib` PRODUCER — what that means in practice.** A
//! scoped search of every `crates/*/Cargo.toml` finds no existing `crate-type = ["cdylib"]`
//! anywhere in this tree, so there is no in-tree precedent for "render one crate as a shared
//! object from inside another crate's service code" to copy. Three things that fell out of
//! working that out, written down because the next person touching this file will hit them too:
//!
//! 1. **The rendered crate is NOT a workspace member, and it never becomes one.** It is written
//!    to a work directory OUTSIDE the source tree (`work_directory`), so `cargo build
//!    --manifest-path <scratch>/Cargo.toml` treats it as its own single-crate implicit
//!    workspace. A `path = "…"` dependency reaching back INTO this checkout (`vike-model`,
//!    `vike-rust-strategy`) does not drag the scratch crate into the REAL workspace —
//!    Cargo resolves each path dependency's `{ workspace = true }` manifest fields against ITS
//!    OWN enclosing workspace (found by walking up from ITS manifest), not against the crate that
//!    is building it. This is what makes "a sibling crate's `Cargo.toml.in`, rendered outside the
//!    workspace, importing straight from the real crates" work at all without a second
//!    `[workspace]` declaration or an `exclude` entry anywhere.
//! 2. **The template is `.in`, never `.rs`, for two independent reasons, and it lives in
//!    `vike-rust-strategy`'s tree rather than this crate's own — READ at build time from the SDK
//!    the build compiles against (`crates/vike-rust-strategy/template/` under it, [`SourceTree`];
//!    decision 0296), never this binary's own copy and never a bare `include_str!` file path
//!    crossing the crate boundary** (a first cut of this split did reach them that way, with
//!    `include_str!("../../vike-rust-strategy/template/…")` — a coupling invisible to `cargo
//!    tree`/`layer_gate.rs` and only caught by a rename). The
//!    obvious reason the files stay `.in`: an `{{ENTRY}}`/`{{NAME}}` placeholder is not valid
//!    Rust and a workspace member holding one would fail every lane that touches that crate. The
//!    one that is easy to miss: even a *syntactically valid* `template/lib.rs` sitting under
//!    `vike-rust-strategy`'s root would still not be a workspace member (`template/` is not
//!    `src/`), so the `.in` extension is about keeping
//!    `crates/vike-ops/tests/hygiene/unsafe_and_toolchain_gate.rs`'s file walk from ever visiting it too —
//!    that gate's FULL-TREE scan of `vike-rust-strategy` (an `UNSAFE_EXEMPT` crate) covers its
//!    `src/`, `build.rs`, `tests/`/`benches`/`examples`, but `template/` is none of those, and
//!    `.in` is not `.rs` either, so this file is invisible to it on BOTH counts.
//! 3. **The artifact this crate hands back always ends in `.so`, regardless of the host
//!    platform's own dynamic-library convention.** Locating what `cargo` just built uses
//!    [`std::env::consts::DLL_PREFIX`]/[`std::env::consts::DLL_SUFFIX`] (so this still finds the file on a
//!    non-Linux dev box), but the design's own naming (`<folder>/<sha256>.so`) is what every OTHER
//!    piece of this design — this service, the loader, the retention pruner — matches
//!    against, and this workspace's `dlopen` side only ever runs on the Linux prod boxes. Picking
//!    a platform-native suffix here would make the artifact name a THIRD thing (after the folder
//!    and the sha) that varies per box, for a design that assumes one filesystem shared by this
//!    service and a server that both run on the same Linux host.
//!
//! ⚠ **[`build_strategy_file`]'s `workspace_root` parameter is a fix, not a style choice — read this
//! before changing it back.** An earlier version of [`render_cargo_toml`] computed the sibling
//! `vike-model`/`vike-rust-strategy` paths from `env!("CARGO_MANIFEST_DIR")`, exactly the defect
//! `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` exists to catch: that macro expands, AT
//! COMPILE TIME, to the tree this crate was BUILT in — never the tree a RUNNING binary is later
//! deployed into. On a dev box or a lane, the two coincide, which is precisely why the defect is
//! invisible everywhere except in production — the exact shape `crates/bridges/dukascopy/src/
//! exec.rs`'s retired `bridge_jar`/`java_program` functions already convicted themselves of
//! ("finds neither jar nor JVM and degrades to paper, with nothing in the log but 'bridge jar not
//! found'"). Unlike a missing data file degrading a venue to paper, a builder pointed at the wrong
//! `vike-model` source cannot degrade gracefully at all — cargo either finds real crates to build
//! a strategy file against or the compile fails — so there is no ladder to fall through to the way
//! `crates/vike-paths/src/store_path.rs`'s `resolve_store_root_from` gives a data path a project
//! and per-user rung. The fix is therefore the plain one this gate's own doc asks for first: the
//! caller (`builder::run`, over the command line its composition root hands it) takes
//! `--workspace-root` and REFUSES to start without it — the same "refuse rather
//! than silently degrade" shape already used there for the missing-key case — and hands the real
//! checkout root down as an ordinary parameter. `render_cargo_toml` now resolves no global state of
//! its own; the ONLY place `env!("CARGO_MANIFEST_DIR")` still appears in this file is inside the
//! inline `mod tests` at the end of it (cfg-gated on test builds — deliberately not spelled as the
//! literal attribute here: `crates/vike-ops/tests/hygiene/temp_path_gate.rs`'s scope test is a plain
//! substring search for that exact token marking where a file's TEST code begins, so writing it out
//! in a doc comment ABOVE the real one would have pulled its boundary up to this paragraph and put
//! [`build_strategy_file`] — genuine production code — under that gate's stricter TEST
//! rule, the same species of trap as the `build.rs`/`render.rs` rename above), which
//! `compile_time_path_gate.rs`'s own doc names as one of ~50 always-correct sites (a test resolving
//! a fixture from its own manifest directory, never walked by that scan at all).
//!
//! **The SDK a build compiles against** (decision 0296). `workspace_root` is ONE source tree: an
//! installed, frozen SDK snapshot (`<project>/vendor/vike-strategy-source/sdk-N/`, its marker
//! `vike_rust_strategy::sdk::SdkMarker` naming N and its per-kind source hashes) or a git checkout ("SDK
//! dev", `vike_rust_strategy::sdk::is_dev_checkout`, its hashes computed at each build and a file's pin
//! ignored with one stderr line). Anything else is refused ([`source_tree_refusal`]; it replaced the
//! `SOURCE_GIT_SHA` stamp and its two checks). The templates are READ from that tree
//! (`crates/vike-rust-strategy/template/` under it), never this binary's own copy. The daemon picks the
//! snapshot per file among the SDKs under its `--sdk-root` ([`InstalledSdks::resolve`]: an explicit
//! override, then the file's `[strategy] sdk = N`, then the newest SDK the usability rule
//! `vike_rust_strategy::sdk::usable` accepts).
//!
//! **An older SDK cut with another boundary builds through its ADAPTER** (decision 0296's step 6): its own
//! crates, this host's boundary in place of its `vike-rust-strategy` (the glue, from the installed SDK that
//! holds this boundary), the adapter's face standing for the glue's own, and this host's templates
//! ([`SourceTree`]'s `adapted`, `lay_out_glue`). Inside the window and with an adapter, or refused in the
//! rule's words; the build folder adds the glue and the adapter to its key.
//!
//! **Where a build works.** A numbered SDK's work directories are persistent, beside its build folders:
//! `<builds>/sdk-N/work-<folder>{,-onnx,-study}` (`vike_rust_strategy::artifact::work_directory`), off the
//! container's tmpfs, so a deploy that cut no SDK leaves every compiled closure warm. Each target only
//! ever sees ONE immutable tree, which is what makes cargo's mtime check sound there. SDK dev's stay
//! under `std::env::temp_dir()` ([`scratch_root`]`/sdk-dev/…`): a checkout's tests build into a fresh
//! builds directory each, and a warm closure shared across them is what keeps them minutes, not hours
//! (`crates/vike-ops/tests/hygiene/system_temp_gate/pin.rs`'s `SYSTEM_TEMP_PIN` keeps this file's row for
//! it). Both are bounded per SDK by age and by size ([`prune_stale_scratch`], [`SCRATCH_MAX_AGE`],
//! [`SCRATCH_MAX_BYTES`]).

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use vike_hub_protocol::hash::sha256_hex;
use vike_rust_strategy::host::UNWIRED_HOOKS;
use vike_rust_strategy::sdk::{ONNX_DEPENDENCY_LINE, Sdk, SdkMarker, TemplateKind};
use vike_strategy::file::{StrategyFileHeader, StrategyFileHeaderRun};

/// Which cargo profile [`build_strategy_file`] compiles the rendered crate under.
///
/// ⚠ **This is not a tuning knob — it is a CORRECTNESS parameter, and it used to be hardcoded to
/// `Release`.** `vike_rust_strategy::fingerprint::FINGERPRINT` folds Cargo's own
/// `PROFILE`/`OPT_LEVEL` in, and the loader refuses a strategy file whose fingerprint differs from the
/// host's. A release-only builder therefore produced artifacts that a DEBUG host could never
/// load, which is the fingerprint guard working exactly as designed and, as a side effect, put
/// the design's central mechanism-equivalence test out of reach of an ordinary debug test run.
/// The service passes [`Profile::Release`] (it serves a release server); a test passes whichever
/// profile its own binary was built under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Debug,
    Release,
}

impl Profile {
    /// The profile THIS binary was compiled under, read off the fingerprint it carries rather
    /// than guessed from `cfg!(debug_assertions)`.
    ///
    /// For a caller that will LOAD what it builds — every test in this workspace that does, and
    /// any future in-process builder — this is the only correct answer, because the loader
    /// compares exactly this field. Derived from the fingerprint string itself so the two can
    /// never disagree: a profile renamed in `vike-rust-strategy`'s `build.rs` changes both
    /// sides at once.
    ///
    /// ⚠ **The builder SERVICE must not use this.** It builds for a DIFFERENT process (the
    /// backtest server), whose profile it cannot observe; `builder.rs` names
    /// [`Profile::Release`] outright and says why.
    pub fn of_this_binary() -> Self {
        if vike_rust_strategy::fingerprint::FINGERPRINT.contains("profile=release") {
            Profile::Release
        } else {
            Profile::Debug
        }
    }

    /// The flag to add to `cargo build`, or `None` for cargo's default (dev) profile.
    fn cargo_flag(self) -> Option<&'static str> {
        match self {
            Profile::Debug => None,
            Profile::Release => Some("--release"),
        }
    }

    /// The subdirectory of `CARGO_TARGET_DIR` the artifact lands in.
    fn target_subdir(self) -> &'static str {
        match self {
            Profile::Debug => "debug",
            Profile::Release => "release",
        }
    }
}

vike_utility::refusal_enum! {
    /// Why [`build_strategy_file`] failed to produce an artifact: the words a user reads when a Build is
    /// refused (Studio, the CLI, a mount's NOT RUNNING reason), each with its id (`vike_utility::refusal_enum!`,
    /// decision 0301). The builder answers a refused Build with its id and its bare words, the refusal frame
    /// (`crate::builder`'s `Response::BuildErr`); the surface that shows them renders the id, once. Decide on the
    /// variant, never on its words.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BuildError with ids BuildErrorId {
        /// `cargo` ran and rustc reported a failure. Carries rustc's OWN diagnostic text VERBATIM
        /// (`cargo build`'s captured stderr), never a summary — a generic "build failed" makes the
        /// user guess at their own syntax error. This is the ONE variant a user's OWN mistake can
        /// produce; the other two are host/environment problems. When the source still reaches a
        /// market-data name through `vike_model`, one `hint:` line FOLLOWS rustc's text, which is
        /// otherwise untouched (`compile_diagnostics`).
        Compile { diagnostics: String } => "strategy.build.compile_failed": "compile failed:\n{diagnostics}",
        /// A filesystem operation failed — writing the rendered source, creating the scratch crate
        /// directory, or moving the finished artifact into its build folder.
        Io { why: String } => "strategy.build.io_failed": "i/o error: {why}",
        /// `cargo` itself could not be found or could not be started — a toolchain problem rather
        /// than a problem with the user's source (contrast [`BuildError::Compile`], where `cargo` DID
        /// run and rustc reported a failure).
        Toolchain { why: String } => "strategy.build.toolchain_failed": "toolchain error: {why}",
        /// The SDK this build would compile against cannot be used (decision 0296): the file pins one this
        /// box does not hold, or one the usability rule refuses (retired, or cut with another boundary,
        /// `vike_rust_strategy::sdk::usable`), no SDK on the box is usable, or `workspace_root` is neither
        /// an installed SDK nor a git checkout ([`source_tree_refusal`]). ONE id for the family: `why` is the
        /// whole sentence, the fix included (`vike-cli strategy migrate <FILE> --to N`, or the install).
        ///
        /// ⚠ Never softened into a build against another SDK: a file built against an SDK it did not ask
        /// for compiles clean, loads clean and produces DIFFERENT NUMBERS with nothing loud anywhere. The
        /// service says every refused `Build` on stderr as well (`crates/vike-strategy-builder/src/builder.rs`'s
        /// `build_refusal_line`), because a refusal only the caller sees reads like an idle daemon.
        Sdk { why: String } => "strategy.build.sdk_unusable": "{why}",
        /// A strategy file path that is not a plain relative `.rs` path under `strategies/`
        /// (`check_strategy_path`): absolute, climbing out with `..`, or carrying a `\`, a `:` or a
        /// control character. The path is a wire peer's string, so it is refused, never rewritten. (The
        /// artifact's own name is the text's sha, so no peer string reaches a path any more; the old
        /// `<name>-<sha>.so` shape is where a planted symlink once overwrote the cargo home's config.)
        InvalidPath { path: String } => "strategy.build.invalid_path":
            "invalid strategy file path {path:?}: a strategy file is named by its path under \
             the project's `strategies/` folder, relative and ending in `.rs` \
             (`trend/momentum.rs`), with no `..`, no `\\` and no `:`",
        /// A research study the builder refuses before building (`check_study`): its folder name, a file
        /// name that is not a plain `<stem>.rs`, a file named twice, or no `<name>.rs` entry. Carries the
        /// reason, which names only the study's and its files' names.
        InvalidStudy { why: String } => "strategy.build.invalid_study":
            "{why}: a study is its folder under `user_data/research/studies/rust/`, holding \
             `<name>.rs` and any sibling `<module>.rs` files it declares",
        /// The file exports both entries, `build` and `rule` (`Entry`); a strategy file exports one.
        TwoEntries => "strategy.build.two_entries":
            "this file exports both `pub fn build` and `pub fn rule`: a strategy file exports \
             one entry — `build` for a strategy, `rule` for the order language",
        /// A `rule` file whose header does not hold (`vike_strategy::file::StrategyFileHeader::parse`'s
        /// one-line reason): its `[run]` options are rendered into its entry, so it is not built without
        /// them.
        Header { why: String } => "strategy.build.rule_header":
            "this `rule` file's header does not hold: {why}. The header is the first ```toml \
             block in the file's leading comment lines, and its `[run]` table says how the rule \
             trades",
        /// [`CargoHomePin::check`] found `$CARGO_HOME/config.toml` no longer matches what this daemon
        /// captured at startup. Carries only PRESENT/ABSENT for each side, never a hash or the file's
        /// content — a hash is still enough entropy about attacker-controlled bytes to be worth
        /// withholding, and "present vs. absent" is all an operator needs to act on this.
        CargoHomeTampered { was_present: Presence, now_present: Presence }
            => "strategy.build.cargo_home_tampered":
            "$CARGO_HOME/config.toml changed since this daemon started (was {was_present}, is now \
             {now_present}) — refusing to build against a cargo configuration nobody here approved. \
             Every caller shares this file; a Build's own build.rs can write it with no traversal \
             needed. Restart the daemon to re-pin the current file, after confirming its \
             content is the operator's own.",
        /// This box cannot jail a build (`vike_sandbox::probe`, `crate::jail::grants`), so nothing is
        /// built — never an unjailed build instead (decision 0273, no override). Carries the reason.
        Unsandboxed { why: String } => "strategy.build.unsandboxed":
            "this box cannot build a Rust strategy safely, so it builds none: {why}. A build \
             here runs only inside a jail that keeps it from the settings database, credential \
             files and the network, and that needs Linux 6.7 or later. Rhai strategies and the \
             built-in ones still run.",
        /// The source's `Strategy` impl overrides one or more seam methods the strategy file vtable does not
        /// carry (`vike_rust_strategy::host::UNWIRED_HOOKS`). Carries every offending hook name.
        ///
        /// ⚠ **The only reason this is an ERROR rather than a note.** Such a source compiles, links,
        /// loads and handshakes perfectly, and then never receives the hook — so the SAME file
        /// produces different, entirely plausible numbers depending on which mechanism the host used
        /// to reach it, with no signal at any layer. That is the failure the design's whole "one
        /// folder, the host decides the mechanism" verdict rests on not happening. Refusing here puts
        /// the signal in front of the author while they are standing at the editor, which is the only
        /// place it can be acted on cheaply. A user reads this when a Build is refused (Studio shows it):
        /// plain words, no internal name of the mechanism. How a hook gets wired is `UNWIRED_HOOKS`' doc.
        UnwiredHook { hooks: Hooks } => "strategy.build.unwired_hook":
            "this strategy overrides {hooks} — a `Strategy` hook a Rust strategy built in Studio \
             is never sent. It would build and run cleanly and simply never receive that \
             hook, so the same file would trade differently there than compiled into the \
             program, \
             with no error and plausible numbers. Two ways forward: drop the override, or \
             put the file in the build-time tier (`user_data/strategies/rust/<name>/`, \
             compiled into the program), which honours every hook.",
        /// The builder built an artifact whose path names no SDK and build folder (`crate::builder`'s
        /// `sdk_and_folder`): the builder's own fault, never the file's, so nothing is answered as built.
        ArtifactUnnamed { artifact: String } => "strategy.build.artifact_unnamed":
            "the builder wrote {artifact}, which names no SDK and build folder",
    }
}

impl std::error::Error for BuildError {}

/// Whether `$CARGO_HOME/config.toml` was there ([`BuildError::CargoHomeTampered`]'s two sides): `present` or
/// `absent` in the refusal's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presence(pub bool);

impl fmt::Display for Presence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0 { "present" } else { "absent" })
    }
}

/// The hooks a source overrides that the vtable does not carry ([`BuildError::UnwiredHook`]), each named
/// in backticks, comma-separated, in the refusal's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hooks(pub Vec<String>);

impl fmt::Display for Hooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let named: Vec<String> = self.0.iter().map(|hook| format!("`{hook}`")).collect();
        f.write_str(&named.join(", "))
    }
}

/// The ONLY variables [`build_strategy_file`]'s `cargo` child is handed, each with the reason it is there.
/// Everything else the calling process holds is dropped: the spawn starts from `env_clear()`.
///
/// ⚠ **Why an allowlist and not "inherit, minus the key".** `cargo` runs a caller's `build.rs` and
/// proc-macros — arbitrary code by design (`builder.rs`'s Decision 2) — and every one of them
/// inherits cargo's environment. MEASURED on the production unit on 2026-09-26: the spawn cleared
/// nothing, so `VIKE_STRATEGY_BUILDER_KEY` (the service's own credential), `CREDENTIALS_DIRECTORY`
/// and whatever an operator put in the unit's `.env` all reached the build as ordinary environment.
/// A denylist would be one variable short the first time somebody adds a second secret to that
/// file; an allowlist is short only of what a build NEEDS, which fails loudly as a compile error
/// rather than silently as a leak.
///
/// Values come from the caller's map ([`child_env`]), never from `std::env` here — this is a library
/// module, and a process-environment read in it would be exactly the `Layer::Library` read
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` ratchets down.
///
/// Deliberately NOT forwarded, each a decision rather than an oversight: `RUSTFLAGS`,
/// `CARGO_ENCODED_RUSTFLAGS`, `CARGO_BUILD_*`, `CARGO_PROFILE_*` (each changes WHAT is built, and
/// the loader's fingerprint check refuses an artifact built under a different profile anyway);
/// `RUSTC_WRAPPER` and `SCCACHE_*` (a wrapper runs as the compiler, and its configuration carries
/// a cache CREDENTIAL on the boxes that use one); `HOME` (cargo and rustup fall back to the
/// password database without it, and it costs a registry row in `vike_config::settings::SETTINGS`);
/// proxies and CA paths (an operator sets those in `$CARGO_HOME/config.toml`, which cargo reads
/// without being told); `LANG`/`LC_*`; and every `VIKE_*` and credential variable, which is the
/// point. `TMPDIR` is not forwarded either: the spawn SETS it to the build's own scratch
/// (`<scratch>/tmp`, on the disk the caller's temporary directory names), the one place the build
/// jail lets rustc and the linker write their temporaries.
pub const CHILD_ENV_ALLOWLIST: &[(&str, &str)] = &[
    (
        "PATH",
        "cargo finds the LINKER (`cc`) and `ar` through it, and `rustc` too when `RUSTC` is unset — \
         dropping it fails every build at the link step with somebody else's error message",
    ),
    (
        "CARGO_HOME",
        "the unit's ONE writable cargo cache (`bin/rust-toolchain/cargo-home`); without it cargo \
         falls back to `~/.cargo`, which the unit hides, and takes its package-cache lock there",
    ),
    (
        "RUSTC",
        "the pinned compiler the unit names outright — a service manager's PATH carries no \
         `rustc`, and the loader refuses a strategy file built by a different one",
    ),
    (
        "RUSTUP_HOME",
        "set only where `rustc` is a rustup proxy (a verification lane, a dev box): the proxy \
         needs it to find the toolchain, and it names a directory, not a credential",
    ),
    (
        "RUSTUP_TOOLCHAIN",
        "set by rustup's own proxy for the process it launched; forwarding it keeps the nested \
         `rustc` the one the caller runs on, so a strategy file a TEST builds carries the test's own \
         toolchain fingerprint",
    ),
];

/// The environment [`build_strategy_file`] hands its `cargo` child, built from a caller-supplied map:
/// every [`CHILD_ENV_ALLOWLIST`] name the map carries, in that table's order, and nothing else. An
/// absent name is simply not forwarded — cargo's own default applies, exactly as if the variable
/// had never been set.
///
/// Pure, so both composition roots call it once on the sweep they already own, and a test calls it
/// on `std::env::vars()` to build what the service would.
pub fn child_env(vars: &HashMap<String, String>) -> Vec<(String, String)> {
    CHILD_ENV_ALLOWLIST
        .iter()
        .filter_map(|(name, _)| vars.get(*name).map(|v| ((*name).to_string(), v.clone())))
        .collect()
}

/// `$CARGO_HOME/config.toml`'s content, hashed — or `None` if the file does not exist. `None` is
/// its own distinct, meaningful state (see [`CargoHomePin`]'s doc), never folded into "no drift".
fn hash_cargo_home_config(cargo_home: &Path) -> Option<String> {
    fs::read(cargo_home.join("config.toml")).ok().map(|bytes| sha256_hex(&bytes))
}

/// **Found by an adversarial review of the deployed jail, independent of [`BuildError::InvalidPath`]
/// above.** `CHILD_ENV_ALLOWLIST` forwards `CARGO_HOME` verbatim to every spawned `cargo` (and
/// therefore to every caller's own `build.rs`) because cargo needs it to take its package-cache
/// lock — but that directory is the SAME one for every caller of this daemon, never partitioned
/// per request. A caller's `build.rs` is arbitrary code by this whole design's own premise, and
/// nothing stopped it writing `$CARGO_HOME/config.toml` directly (no traversal, no symlink trick —
/// its own environment already names the path): a `[source.crates-io] replace-with` line silently
/// redirects every LATER caller's crate downloads through an attacker's registry, or a
/// `[registry] credential-provider` line gives cargo an arbitrary executable to run on the next
/// registry access — persistent code execution across every future Build the daemon accepts, not
/// merely inside the caller's own sandbox. That crosses the multi-tenant boundary the unit's own
/// jail exists to hold, and it was not on the unit's residuals list.
///
/// The cure is a PIN, taken once at [`crate::builder::run`]'s startup (before the first Build is
/// ever served, so only the operator's own pre-configured file — or its deliberate absence — can
/// be captured) and checked again before every later Build, refusing the ONE build a drift is
/// first observed on rather than silently building against a config the operator never approved.
/// An absent file is a valid, common pin: the header comment on [`CHILD_ENV_ALLOWLIST`]'s
/// `CARGO_HOME` row already treats an operator-authored `config.toml` (a proxy, a CA path) as
/// legitimate and expected, so the pin must capture "there was none" as faithfully as it captures
/// a real file's content — never treat absent-then-present as anything but drift.
#[derive(Clone)]
pub struct CargoHomePin {
    cargo_home: PathBuf,
    at_startup: Option<String>,
}

impl CargoHomePin {
    /// Capture the pin. Call exactly once, at startup, before `serve` accepts a single connection.
    pub fn capture(cargo_home: &Path) -> Self {
        Self {
            cargo_home: cargo_home.to_path_buf(),
            at_startup: hash_cargo_home_config(cargo_home),
        }
    }

    /// A pin that never refuses — for the many `build_strategy_file` callers (this crate's and its
    /// siblings' tests, and `vike-studio-core`'s in-process equivalence harness) that build
    /// against a scratch `CARGO_HOME` they fully control and are not testing THIS guard. The one
    /// production caller is [`crate::builder::run`], through [`Self::capture`]; nothing here
    /// reaches this constructor.
    pub fn disabled() -> Self {
        Self { cargo_home: PathBuf::new(), at_startup: None }
    }

    /// `Ok(())` if nothing has changed since [`Self::capture`]; the current and pinned hashes
    /// (each `None` meaning "absent") otherwise, for a refusal message that names what drifted
    /// without ever printing the file's contents. [`Self::disabled`]'s empty path always reads
    /// `Ok(())` here without ever touching the filesystem — an explicit short-circuit, not an
    /// accident of an empty path happening to resolve to no file.
    fn check(&self) -> Result<(), (Option<String>, Option<String>)> {
        if self.cargo_home.as_os_str().is_empty() {
            return Ok(());
        }
        let now = hash_cargo_home_config(&self.cargo_home);
        if now == self.at_startup { Ok(()) } else { Err((self.at_startup.clone(), now)) }
    }
}

/// The directory under `std::env::temp_dir()` that holds SDK dev's work directories (`sdk-dev/…`) and
/// [`prefetch`]'s. One name, so the builder and [`prune_stale_scratch`] cannot disagree.
const SCRATCH_ROOT_DIR: &str = "vike-rust-strategy-build";

/// How long a scratch crate may go UNTOUCHED before [`prune_stale_scratch`] removes it: two days,
/// the retention the the CI box and the latency box CI scratch is operated at. Each is ONE build folder's shared
/// cargo `target/` (a per-source one measured 2.9–4.8 GB on the latency box), so every file built inside the
/// window reuses the compiled dependency closure — the reason the directory outlives its build.
pub const SCRATCH_MAX_AGE: Duration = Duration::from_secs(2 * 24 * 60 * 60);

/// How many bytes one SDK's work directories may hold after [`prune_stale_scratch`]'s size pass: 20
/// GiB. Age alone does not bound it where many folders and checkouts build: measured on the latency box on
/// 2026-11-19, 144 entries, 104 GB, all inside [`SCRATCH_MAX_AGE`] (lanes and tests mint ~70 a day,
/// ~0.7 GB each); production the CI box held 1.3 GB.
pub const SCRATCH_MAX_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// Remove every directory under `root` that `swept` names and nothing has built into for longer than
/// `max_age`, except `keep`; then, given `max_bytes`, the least recently used of them until they hold
/// no more than that; returns how many it removed. `root` is one SDK's builds
/// (`<builds>/sdk-N/`, its `work-…` directories, [`is_work_directory`]) or, once, the pre-SDK scratch
/// root's leftovers ([`is_pre_sdk_scratch`]); an entry `swept` does not name is never touched or sized.
///
/// **The size pass** sums every swept entry's files (`keep`'s too) and removes the entries with the
/// oldest stamp first, never `keep` or one whose `build.lock` a build holds right now (tried, never
/// waited on; held while it is removed). It walks every file, so the caller passes `max_bytes` only
/// when its build starts a NEW directory, the step that grows the root, and a warm build walks nothing.
///
/// ⚠ **This is the retention the header of this module said did not exist, and
/// it is the reason `deploy/vike-ci-tmp.conf` can exempt this directory from its per-file sweep.**
/// Measured on the latency box on 2026-10-12: 166 entries, 104 GB, the oldest from 09-22, against the "~45 GB"
/// that file expected — the exemption had been made on a retention claim nothing implemented.
///
/// **An entry's age is its `Cargo.toml`'s modification time, not a walk of its files.**
/// `build_strategy_file` re-writes that file on EVERY call (the rendered manifest embeds the workspace
/// root, so it is rewritten even for byte-identical source), which makes it the entry's
/// last-USED stamp under `noatime` — where a target dir read every day would otherwise look as old
/// as its creation. A per-FILE age would strip artifacts out of an in-use target dir, which is the
/// hazard the sweep file names. An entry with no `Cargo.toml` (a half-created one) falls back to
/// its directory's own mtime.
///
/// `now` is a parameter so the test fixes the clock instead of racing it. Best-effort per entry:
/// one that cannot be read or removed is skipped, never fatal — a sweep must not fail a build.
fn prune_stale_scratch(
    root: &Path,
    keep: &Path,
    swept: fn(&str) -> bool,
    max_age: Duration,
    max_bytes: Option<u64>,
    now: SystemTime,
) -> usize {
    let Ok(entries) = fs::read_dir(root) else { return 0 };
    let mut removed = 0;
    let mut left = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !entry.file_name().to_str().is_some_and(swept) {
            continue;
        }
        let stamp = fs::metadata(path.join("Cargo.toml")).or_else(|_| fs::metadata(&path));
        let Ok(modified) = stamp.and_then(|m| m.modified()) else { continue };
        // A stamp in the future (clock step) is "fresh", not an error.
        let stale = now.duration_since(modified).is_ok_and(|age| age > max_age);
        if path != keep && stale && fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        } else {
            left.push((modified, path));
        }
    }
    let Some(max_bytes) = max_bytes else { return removed };
    let mut sized: Vec<_> =
        left.into_iter().map(|(t, path)| (t, tree_bytes(&path), path)).collect();
    let mut total: u64 = sized.iter().map(|(_, bytes, _)| bytes).sum();
    sized.sort();
    for (_, bytes, path) in sized {
        if total <= max_bytes {
            break;
        }
        let lock = fs::File::open(path.join("build.lock"));
        if path == keep || lock.as_ref().is_ok_and(|f| f.try_lock().is_err()) {
            continue;
        }
        if fs::remove_dir_all(&path).is_ok() {
            total = total.saturating_sub(bytes);
            removed += 1;
        }
    }
    removed
}

/// The bytes of every regular file under `dir`, recursively; an entry that cannot be read counts 0.
fn tree_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => tree_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

/// Whether a directory under one SDK's builds is a work directory ([`prune_stale_scratch`]'s entries
/// there): `work-<folder>…`, never a build folder of artifacts beside it.
fn is_work_directory(name: &str) -> bool {
    name.starts_with(vike_rust_strategy::artifact::WORK_DIRECTORY_PREFIX)
}

/// Whether a work directory's name is a `kind` one: `work-` and an eight-digit folder, then exactly the
/// kind's suffix (`TemplateKind::work_suffix`), so a strategy one is never taken for a study one.
fn is_work_of(name: &str, kind: TemplateKind) -> bool {
    let suffix = kind.work_suffix();
    name.strip_prefix(vike_rust_strategy::artifact::WORK_DIRECTORY_PREFIX)
        .is_some_and(|rest| rest.len() == 8 + suffix.len() && rest.ends_with(suffix))
}

/// Whether an entry of [`scratch_root`] is a scratch crate of the layout before SDKs
/// (`<folder>-<root tag>…`): swept by age once a dev build starts a new work directory, so a lane's
/// leftovers go. Never [`prefetch`]'s, nor `sdk-dev/`, the dev work directories' parent.
fn is_pre_sdk_scratch(name: &str) -> bool {
    !name.starts_with(PREFETCH_PREFIX) && Sdk::of_directory_name(name).is_none()
}

/// The prefix of [`prefetch`]'s directories under [`scratch_root`].
const PREFETCH_PREFIX: &str = "prefetch-";

/// The `crates/vike-rust-strategy/template/` files of a source tree, as [`SourceTree`] reads them. ⚠ The
/// same four names `crates/vike-rust-strategy/src/lib.rs`'s `TEMPLATE_*` constants `include_str!`: they
/// are this binary's own copy, which the builder no longer reads (a build compiles the SDK's template).
const TEMPLATE_FILES: [&str; 4] =
    ["Cargo.toml.in", "lib.rs.in", "study_Cargo.toml.in", "study_lib.rs.in"];

/// The template files of one source tree, read from it ([`TEMPLATE_FILES`]).
struct Templates {
    strategy_cargo_toml: String,
    strategy_lib_rs: String,
    study_cargo_toml: String,
    study_lib_rs: String,
}

/// **The source tree one build compiles against** (`workspace_root`): an installed SDK snapshot, named
/// by its marker, or a git checkout, SDK dev (this module's doc). Opened per build: a marker and four
/// template files, nothing close to a cargo run.
struct SourceTree<'a> {
    root: &'a Path,
    sdk: Sdk,
    /// A snapshot's marker (its per-kind source hashes); `None` for SDK dev, whose hashes are computed.
    marker: Option<SdkMarker>,
    /// The templates the build renders: the tree's own, or for an adapted SDK this host's boundary's.
    templates: Templates,
    /// An SDK cut with another boundary than this host's, built against this host's through its adapter
    /// (decision 0296's step 6); `None` for an SDK of this boundary and for SDK dev.
    adapted: Option<Adapted>,
}

/// **An older SDK built against THIS host's boundary through its adapter** (decision 0296, verdict 7 and
/// step 6). The build compiles the SDK's own crates, and in place of its `vike-rust-strategy` the GLUE: a
/// copy of this host's boundary crate (from the installed SDK that holds this boundary,
/// `vike_rust_strategy::sdk::host_tree`) with this vike's adapter's face (compiled into it,
/// `vike_rust_strategy::sdk::built_in_adapters`) standing for its own (`lay_out_glue`),
/// built in its guest-only `adapted` mode against the SDK's crates ([`glue_manifest`]), around this host's
/// templates. So the artifact speaks this host's boundary (its identity record says so, the copy's own
/// `build.rs` computing the same boundary hash) and its strategy sees its own SDK's API. A renamed method
/// costs nothing (decision 0296's adapter probe), and nothing sits on the core hop (a `.rs` strategy runs in
/// the sandboxed child); an SDK whose lent rows moved is never adapted (verdict 7).
struct Adapted {
    /// The tree that holds this host's boundary: its glue and its templates are this build's.
    host: PathBuf,
    /// The adapter's face, this vike's own.
    face: String,
    /// The glue's manifest ([`glue_manifest`]), against this SDK's crates.
    manifest: String,
    /// What the build folder adds for it: the host tree's strategy source hash (the glue's sources among
    /// it), the glue's manifest and the adapter's marker and face, each read as the hashes read a source
    /// (a comment moves no folder), so another glue or another adapter is another folder, and an adapter's
    /// fix rebuilds only the files pinned to its SDK.
    recipe: String,
}

/// Where the glue is laid out in a build's work directory (and a prefetch's): `boundary/crates/…`, so its
/// `build.rs` reads its boundary at the paths it reads them in a source tree, and its manifest's place is
/// relative to the rendered crate's (`{{VIKE_RUST_STRATEGY_PATH}}`).
const GLUE_DIR: &str = "boundary/crates/vike-rust-strategy";

/// What of this host's boundary crate the glue copies: its build script and the walk it runs, its sources
/// and the boundary's templates and goldens (the walk's input), so the copy's boundary hash is this host's;
/// and its `adapters` folder, which its build script watches unconditionally (a watched path that is missing
/// would re-run the script, and rebuild the glue, on every build).
const GLUE_PARTS: [&str; 6] =
    ["build.rs", "build", "src", "template", "tests/boundary", "adapters"];

/// The glue's package, whatever its dependencies: this host's `vike-rust-strategy` built as a library alone,
/// its build script included (it bakes the fingerprint and the boundary hash the identity record states).
const GLUE_PACKAGE: &str = r#"# @generated by `crates/vike-strategy-builder/src/render.rs`'s `glue_manifest`: this host's strategy
# boundary compiled against an older SDK's crates through that SDK's adapter (decision 0296's step 6).
[package]
name = "vike-rust-strategy"
version = "0.0.0"
edition = "2024"
publish = false
build = "build.rs"
autobins = false
autoexamples = false
autotests = false
autobenches = false

[lib]
path = "src/lib.rs"
"#;

/// The non-comment lines of `section` (`[dependencies]`) in the manifest `text`, up to the next table.
fn manifest_section<'t>(text: &'t str, section: &str) -> Vec<&'t str> {
    text.lines()
        .map(str::trim)
        .skip_while(|line| *line != section)
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// **The glue's manifest**, read off the host tree's OWN `vike-rust-strategy` manifest (the glue's sources and
/// its dependencies come from one commit): [`GLUE_PACKAGE`], that manifest's `[features]` with `adapted` the
/// default, and its `[dependencies]`, a `path = "../X"` one pointed at the SDK at `sdk_tree`'s `crates/X` and a
/// `workspace = true` one at the spec the host tree's root manifest pins (the SDK's `Cargo.lock` decides
/// the exact version). A dependency line of another shape is refused in words, never guessed.
fn glue_manifest(host: &Path, sdk_tree: &Path) -> Result<String, String> {
    let manifest = host.join(vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR).join("Cargo.toml");
    let read = |path: &Path| {
        fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
    };
    let (own, root) = (read(&manifest)?, read(&host.join("Cargo.toml"))?);
    let pinned = manifest_section(&root, "[workspace.dependencies]");
    let mut dependencies = Vec::new();
    for line in manifest_section(&own, "[dependencies]") {
        let refused =
            || format!("the glue cannot render {}'s dependency `{line}`", manifest.display());
        let (name, value) = line.split_once(" = ").ok_or_else(refused)?;
        if value == "{ workspace = true }" {
            let spec =
                pinned.iter().find(|pin| pin.split_once(" = ").is_some_and(|(n, _)| n == name));
            dependencies.push(spec.ok_or_else(refused)?.to_string());
        } else if let Some(dir) =
            value.strip_prefix("{ path = \"../").and_then(|v| v.strip_suffix("\" }"))
        {
            let path = sdk_tree.join("crates").join(dir);
            dependencies.push(format!("{name} = {{ path = \"{}\" }}", path.display()));
        } else {
            return Err(refused());
        }
    }
    let features = manifest_section(&own, "[features]").join("\n");
    Ok(format!(
        "{GLUE_PACKAGE}\n[features]\ndefault = [\"adapted\"]\n{features}\n\n[dependencies]\n{}\n",
        dependencies.join("\n")
    ))
}

impl<'a> SourceTree<'a> {
    fn open(root: &'a Path) -> Result<Self, BuildError> {
        let (sdk, marker) = sdk_and_marker(root)?;
        let adapted = match &marker {
            Some(marker) if marker.boundary_hash != vike_rust_strategy::sdk::BOUNDARY_HASH => {
                Some(adapted_of(root, marker)?)
            }
            _ => None,
        };
        let templates_root = adapted.as_ref().map_or(root, |adapted| adapted.host.as_path());
        let dir =
            templates_root.join(vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR).join("template");
        let [strategy_cargo_toml, strategy_lib_rs, study_cargo_toml, study_lib_rs] = TEMPLATE_FILES
            .map(|file| {
                fs::read_to_string(dir.join(file)).map_err(|e| BuildError::Io {
                    why: format!("reading the template {}: {e}", dir.join(file).display()),
                })
            });
        let templates = Templates {
            strategy_cargo_toml: strategy_cargo_toml?,
            strategy_lib_rs: strategy_lib_rs?,
            study_cargo_toml: study_cargo_toml?,
            study_lib_rs: study_lib_rs?,
        };
        Ok(SourceTree { root, sdk, marker, templates, adapted })
    }

    /// The tree's source hash for `kind`: its marker's, or computed now (SDK dev).
    fn source_hash(&self, kind: TemplateKind) -> String {
        match &self.marker {
            Some(marker) => marker.source_hash.of(kind).to_string(),
            None => vike_rust_strategy::sdk::source_hash(self.root, kind),
        }
    }

    /// The `Cargo.toml` a `kind` build of this tree renders. An adapted SDK's names the glue laid out
    /// beside it ([`GLUE_DIR`]) for its `vike-rust-strategy`.
    fn cargo_toml(&self, kind: TemplateKind) -> String {
        let glue = self.adapted.is_some().then_some(GLUE_DIR);
        match kind {
            TemplateKind::Strategy => {
                render_cargo_toml(&self.templates, CRATE_NAME, self.root, false, glue)
            }
            TemplateKind::StrategyOnnx => {
                render_cargo_toml(&self.templates, CRATE_NAME, self.root, true, glue)
            }
            TemplateKind::Study => {
                render_study_cargo_toml(&self.templates, STUDY_CRATE_NAME, self.root)
            }
        }
    }

    /// Lay out what the rendered crate at `dir` compiles beside its own sources: for an adapted SDK the
    /// glue ([`lay_out_glue`]), for every other tree nothing.
    fn lay_out(&self, dir: &Path) -> Result<(), String> {
        match &self.adapted {
            Some(adapted) => lay_out_glue(adapted, dir),
            None => Ok(()),
        }
    }

    /// The build folder a `kind` artifact of this tree lands in (`vike_rust_strategy::artifact::build_folder`):
    /// the tree's source hash for the kind and the RECIPE, `vike_rust_strategy::artifact::recipe_hash` of what
    /// this builder writes around the file (the rendered `Cargo.toml`, and the `lib.rs` every entry shape
    /// renders to) and how it runs cargo ([`CARGO_BUILD_ARGS`]).
    fn folder(&self, kind: TemplateKind, profile: Profile) -> String {
        let cargo_toml = self.cargo_toml(kind);
        let lib_rs = match kind {
            TemplateKind::Strategy | TemplateKind::StrategyOnnx => {
                let lib = &self.templates.strategy_lib_rs;
                let every = "pub fn refuse(\npub fn echo(\n";
                render_lib_rs(lib, "", Entry::Build)
                    + &render_lib_rs(lib, every, Entry::Rule(StrategyFileHeaderRun::default()))
            }
            TemplateKind::Study => self.templates.study_lib_rs.clone(),
        };
        let args = CARGO_BUILD_ARGS.join(" ");
        // Read as the hashes read a source: a comment in a template moves no folder.
        let (cargo_toml, lib_rs) = (
            vike_rust_strategy::sdk::toml_tokens(&cargo_toml),
            vike_rust_strategy::sdk::rust_tokens(&lib_rs),
        );
        let mut pieces = vec![cargo_toml.as_str(), lib_rs.as_str(), args.as_str()];
        pieces.extend(self.adapted.as_ref().map(|adapted| adapted.recipe.as_str()));
        let recipe = vike_rust_strategy::artifact::recipe_hash(&pieces);
        vike_rust_strategy::artifact::build_folder(
            profile.target_subdir(),
            &self.source_hash(kind),
            &recipe,
        )
    }
}

/// Why `root` is no source tree: neither an installed SDK nor a git checkout.
fn not_a_source_tree(root: &Path) -> String {
    format!(
        "{} is neither an installed SDK (it holds no `{}` marker) nor a git checkout, so nobody can say \
         which sources it holds. Name the installed SDKs with `--sdk-root <project>/{}` (the deploy installs \
         each as `sdk-N/`; `scripts/fetch_release_tools.sh strategy-source --sdk N` fetches one), or point \
         `--workspace-root` at a git checkout.",
        root.display(),
        vike_rust_strategy::sdk::SDK_MARKER_FILE,
        vike_rust_strategy::sdk::SDK_SOURCE_DIR,
    )
}

/// **How the installed SDK at `root`, cut with another boundary than this host's (`marker`), builds here**:
/// through this host's adapter from it ([`Adapted`]), the host's boundary and adapters read from the SDKs
/// installed beside it (`root`'s parent, an `--sdk-root`: `vike_rust_strategy::sdk::host_tree`). Refused in
/// the usability rule's words (`vike_rust_strategy::sdk::usable`) when it is outside the window or this
/// host holds no adapter from its boundary, never built against its own boundary, which this host does not
/// load.
fn adapted_of(root: &Path, marker: &SdkMarker) -> Result<Adapted, BuildError> {
    use vike_rust_strategy::sdk::{self, SdkUse};
    let refused = |why: String| BuildError::Sdk {
        why: format!("{} cannot be built here: {why}", Sdk::Number(marker.number)),
    };
    let sdk_root = root.parent().unwrap_or(root);
    let newest =
        sdk::installed(sdk_root).map_err(refused)?.last().copied().unwrap_or(marker.number);
    let host = sdk::host_tree(sdk_root);
    let set = host.as_ref().map(|_| sdk::built_in_adapters()).unwrap_or_default();
    let adapter = match sdk::usable(marker, root, newest, &set.adapters) {
        Ok(SdkUse::Adapted(adapter)) => adapter,
        Ok(SdkUse::SameBoundary) => unreachable!("only an SDK of another boundary is adapted"),
        // An adapter that does not read is said beside the refusal: it may be this SDK's.
        Err(why) if !set.broken.is_empty() => {
            return Err(refused(format!(
                "{why}; this vike's adapters that do not read: {}",
                set.broken.join("; ")
            )));
        }
        Err(why) => return Err(refused(why.to_string())),
    };
    let host = host.expect("an adapter is served only where an installed SDK holds this boundary");
    let host_hash = SdkMarker::read(&host).map_err(refused)?.source_hash.strategy;
    let manifest = glue_manifest(&host, root).map_err(refused)?;
    let marker = format!("{} {}", adapter.number, adapter.boundary_hash);
    let recipe = vike_rust_strategy::artifact::recipe_hash(&[
        &host_hash,
        &sdk::toml_tokens(&manifest),
        &marker,
        &sdk::rust_tokens(&adapter.face),
    ]);
    Ok(Adapted { host, face: adapter.face, manifest, recipe })
}

/// **Lay out the glue** of `adapted` under `dir` ([`GLUE_DIR`]), for the crate rendered there: this host's boundary crate ([`GLUE_PARTS`], from the tree holding it), the adapter's face
/// as its `adapter/face.rs` (what `vike_rust_strategy::guest`'s `face` reads in the `adapted` mode) and its
/// manifest ([`glue_manifest`]). Each file is written only when its bytes differ, so the target's mtimes
/// stay those of the one immutable tree it compiles (this module's doc): a work directory is named by the
/// glue's recipe, so it only ever holds one glue.
fn lay_out_glue(adapted: &Adapted, dir: &Path) -> Result<(), String> {
    let from = adapted.host.join(vike_rust_strategy::sdk::RUST_STRATEGY_CRATE_DIR);
    let to = dir.join(GLUE_DIR);
    for part in GLUE_PARTS {
        mirror(&from.join(part), &to.join(part))?;
    }
    write_if_changed(&to.join("adapter").join("face.rs"), adapted.face.as_bytes())?;
    write_if_changed(&to.join("Cargo.toml"), adapted.manifest.as_bytes())
}

/// Copy the file or the folder `from` to `to`, writing each file only when its bytes differ ([`lay_out_glue`]).
fn mirror(from: &Path, to: &Path) -> Result<(), String> {
    if from.is_dir() {
        let entries = fs::read_dir(from).map_err(|e| format!("listing {}: {e}", from.display()))?;
        for entry in entries.flatten() {
            mirror(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    let bytes = fs::read(from).map_err(|e| format!("reading {}: {e}", from.display()))?;
    write_if_changed(to, &bytes)
}

/// Write `bytes` to `path` (its folders made), unless it already holds exactly them.
fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if fs::read(path).is_ok_and(|held| held == bytes) {
        return Ok(());
    }
    path.parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(path, bytes))
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

/// **The daemon's check of a `--workspace-root` at its start** (it replaced the `SOURCE_GIT_SHA` stamp's two
/// checks): `None` for a git checkout (SDK dev) and for an installed SDK whose bytes are the ones its marker
/// describes (`vike_rust_strategy::sdk::SdkMarker::verify`); the refusal's words otherwise.
pub fn source_tree_refusal(root: &Path) -> Option<String> {
    if vike_rust_strategy::sdk::is_dev_checkout(root) {
        return None;
    }
    if !root.join(vike_rust_strategy::sdk::SDK_MARKER_FILE).is_file() {
        return Some(not_a_source_tree(root));
    }
    SdkMarker::read(root).and_then(|marker| marker.verify(root)).err()
}

/// The SDK the source tree at `root` is: its marker's number, or `Sdk::Dev` for a git checkout.
///
/// # Errors
/// [`BuildError::Sdk`]: a tree that is neither an installed SDK nor a checkout, or a marker that does
/// not read.
pub fn sdk_of(root: &Path) -> Result<Sdk, BuildError> {
    sdk_and_marker(root).map(|(sdk, _)| sdk)
}

/// [`sdk_of`], and a snapshot's marker beside it (`None` for SDK dev).
fn sdk_and_marker(root: &Path) -> Result<(Sdk, Option<SdkMarker>), BuildError> {
    if vike_rust_strategy::sdk::is_dev_checkout(root) {
        return Ok((Sdk::Dev, None));
    }
    if !root.join(vike_rust_strategy::sdk::SDK_MARKER_FILE).is_file() {
        return Err(BuildError::Sdk { why: not_a_source_tree(root) });
    }
    let marker = SdkMarker::read(root).map_err(|why| BuildError::Sdk { why })?;
    Ok((Sdk::Number(marker.number), Some(marker)))
}

/// The SDK a file's header pins (`[strategy] sdk = N`, `vike_strategy::file::StrategyFileHeaderStrategy`'s
/// `sdk`); `None` for no pin, and for a text whose header does not parse (a `rule` file is refused by
/// that header's words; the hub's read refused every other).
pub fn pin_of(text: &str) -> Option<u32> {
    StrategyFileHeader::parse(text).ok().and_then(|header| header.strategy.sdk)
}

/// **The SDKs installed under a builder's `--sdk-root`** (`<project>/vendor/vike-strategy-source`), each
/// checked against its marker ONCE, at the builder's start ([`InstalledSdks::open`]): a snapshot whose bytes
/// moved since its marker was written is never built against. What a build resolves its SDK among
/// ([`InstalledSdks::resolve`]).
#[derive(Debug, Clone)]
pub struct InstalledSdks {
    root: PathBuf,
    /// Every installed SDK, ascending: its marker, or why it was refused at the start.
    sdks: Vec<(u32, Result<SdkMarker, String>)>,
    /// This host's adapters (decision 0296's step 6): its own, compiled in, where an SDK that verified holds
    /// this host's boundary (the glue's source), and why each other one is none (said in the report).
    adapters: vike_rust_strategy::sdk::AdapterSet,
}

impl InstalledSdks {
    /// The SDKs under `root`, each verified (`vike_rust_strategy::sdk::SdkMarker::verify`; a few
    /// milliseconds each). A snapshot whose marker names another number is refused too.
    ///
    /// # Errors
    /// `root` exists and cannot be listed.
    pub fn open(root: &Path) -> Result<Self, String> {
        let sdks = vike_rust_strategy::sdk::installed(root)?
            .into_iter()
            .map(|number| {
                let dir = vike_rust_strategy::sdk::snapshot_dir(root, number);
                let checked = SdkMarker::read(&dir).and_then(|marker| {
                    if marker.number != number {
                        return Err(format!(
                            "{} holds the marker of SDK {}, so it is no SDK {number}",
                            dir.display(),
                            marker.number
                        ));
                    }
                    marker.verify(&dir).map(|()| marker)
                });
                (number, checked)
            })
            .collect::<Vec<_>>();
        let host = sdks.iter().rev().find_map(|(number, checked)| {
            let marker = checked.as_ref().ok()?;
            (marker.boundary_hash == vike_rust_strategy::sdk::BOUNDARY_HASH)
                .then(|| vike_rust_strategy::sdk::snapshot_dir(root, *number))
        });
        let adapters =
            host.map(|_| vike_rust_strategy::sdk::built_in_adapters()).unwrap_or_default();
        Ok(InstalledSdks { root: root.to_path_buf(), sdks, adapters })
    }

    /// The `--sdk-root` these SDKs are under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The newest SDK installed, usable or not: the window's top (`vike_rust_strategy::sdk::usable`).
    fn newest(&self) -> Option<u32> {
        self.sdks.last().map(|(number, _)| *number)
    }

    /// The snapshot of SDK `number`, when it is installed, verified and usable on this host; why not
    /// otherwise, the fix included.
    fn usable(&self, number: u32, newest: u32) -> Result<PathBuf, String> {
        let Some((_, checked)) = self.sdks.iter().find(|(n, _)| *n == number) else {
            let held: Vec<String> = self.sdks.iter().map(|(n, _)| n.to_string()).collect();
            return Err(format!(
                "SDK {number} is not installed on this box (it holds SDK {}): install it \
                 (`scripts/fetch_release_tools.sh strategy-source --sdk {number}`), or move the file to \
                 SDK {newest} (`vike-cli strategy migrate <FILE> --to {newest}`)",
                held.join(", ")
            ));
        };
        let marker = checked.as_ref().map_err(Clone::clone)?;
        let adapters = &self.adapters.adapters;
        let snapshot = vike_rust_strategy::sdk::snapshot_dir(&self.root, number);
        vike_rust_strategy::sdk::usable(marker, &snapshot, newest, adapters)
            .map_err(|e| e.to_string())?;
        Ok(snapshot)
    }

    /// Every SDK the usability rule accepts, newest first, with its snapshot.
    pub fn usable_snapshots(&self) -> Vec<(u32, PathBuf)> {
        let Some(newest) = self.newest() else { return Vec::new() };
        self.sdks
            .iter()
            .rev()
            .filter_map(|(number, _)| Some((*number, self.usable(*number, newest).ok()?)))
            .collect()
    }

    /// One line per installed SDK, for the builder's start: usable, or why not.
    pub fn report(&self) -> Vec<String> {
        let Some(newest) = self.newest() else {
            return vec![format!(
                "no SDK is installed under {}: every Rust build is refused until the deploy installs one",
                self.root.display()
            )];
        };
        let adapted =
            |number: u32| self.adapters.adapters.iter().any(|adapter| adapter.number == number);
        let mut lines: Vec<String> = self
            .sdks
            .iter()
            .map(|(number, checked)| match self.usable(*number, newest) {
                Ok(dir) => {
                    let same = checked.as_ref().is_ok_and(|marker| {
                        marker.boundary_hash == vike_rust_strategy::sdk::BOUNDARY_HASH
                    });
                    let how = if same || !adapted(*number) {
                        "usable"
                    } else {
                        "usable through its adapter to this vike's boundary"
                    };
                    format!("SDK {number} at {}: {how}", dir.display())
                }
                Err(why) => format!("SDK {number}: NOT usable: {why}"),
            })
            .collect();
        lines.extend(
            self.adapters
                .broken
                .iter()
                .map(|why| format!("an SDK adapter that does not read: {why}")),
        );
        lines
    }

    /// **The snapshot a build compiles against** (decision 0296), in this order: `chosen`, an explicit
    /// override (a backtest profile's key, stage 3); else `pin`, the file's `[strategy] sdk = N`
    /// ([`pin_of`]); else the newest SDK the usability rule accepts. A named SDK that is not installed or
    /// not usable is refused, never swapped for another.
    ///
    /// # Errors
    /// [`BuildError::Sdk`], the reason and the fix in words.
    pub fn resolve(&self, chosen: Option<u32>, pin: Option<u32>) -> Result<PathBuf, BuildError> {
        let Some(newest) = self.newest() else {
            return Err(BuildError::Sdk {
                why: format!(
                    "no SDK is installed under {}, so a Rust strategy file builds against nothing: the deploy \
                 installs one (`scripts/fetch_release_tools.sh strategy-source --sdk N`)",
                    self.root.display()
                ),
            });
        };
        let named = match (chosen, pin) {
            (Some(number), _) => Some((number, "this run asks for")),
            (None, Some(number)) => Some((number, "this file pins (`[strategy] sdk`)")),
            (None, None) => None,
        };
        if let Some((number, who)) = named {
            return self.usable(number, newest).map_err(|why| BuildError::Sdk {
                why: format!("the SDK {who} cannot be built: {why}"),
            });
        }
        self.usable_snapshots().into_iter().next().map(|(_, dir)| dir).ok_or_else(|| {
            BuildError::Sdk {
                why: format!(
                    "no SDK installed under {} is usable on this vike: {}",
                    self.root.display(),
                    self.report().join("; ")
                ),
            }
        })
    }
}

/// The package name every rendered strategy file builds under. ONE package in ONE build directory per
/// build folder ([`build_strategy_file`]), so a new file rebuilds only its own crate and its link:
/// the dependency closure stays compiled in the shared target. Two artifacts built under this one
/// name never meet: a host opens each `dlopen`-local, and the sandbox opens one.
const CRATE_NAME: &str = "strategy_file";

/// Build (or reuse the build of) the strategy FILE whose text is `text` and whose path under the
/// project's `strategies/` folder is `path` (`trend/momentum.rs`), and return its artifact:
/// `<builds_dir>/sdk-N/<folder>/<sha256 of text>.so` (`vike_rust_strategy::artifact::artifact_path`), `sdk-N`
/// the SDK `workspace_root` is ([`SourceTree`], `sdk-dev` for a checkout) and the folder named by that
/// tree's source hash for the file's template kind, this builder's recipe, the boundary, the toolchain
/// and `profile` ([`build_folder`]). The caller reads the file and hands its text over; this function
/// reads no `strategies/` folder (plan 2b, section 3). `path` names the file in refusals only, and is
/// refused when it is not a plain relative `.rs` path ([`BuildError::InvalidPath`]).
///
/// `workspace_root` is the source tree the rendered crate's `path` dependencies resolve against: one
/// installed SDK snapshot, or a git checkout (this module's doc; the daemon resolves the snapshot,
/// [`InstalledSdks::resolve`]). On a checkout a file's `[strategy] sdk` pin is ignored, said on stderr.
/// `cargo_bin` is the `cargo` to run — caller-supplied for the reasons this module's doc gives.
/// `child_env` is the WHOLE environment `cargo` (and every build script it runs) receives; build it
/// with [`child_env`].
///
/// **The cache.** A text hashing to an artifact already in its folder is served without `cargo`,
/// after the artifact's compatibility record is read and checked against this host, running none of
/// its code (a mismatch, or a file with no readable record, is REBUILT rather than served). A text
/// whose last build failed IN ITS OWN CRATE is answered from `<sha256>.err` beside it, rustc's words
/// and the hints as they were: that failure is the text's, and the same text under the same folder
/// fails the same way. A failure
/// of anything else (a crate missing from the cargo home, the toolchain, the jail) is never recorded,
/// so it is retried. Every hit stamps its use (`stamp_use`), which retention evicts by.
///
/// **The shared work directory and the queue.** Every build of one folder uses ONE work directory
/// ([`work_directory`]: `…/sdk-N/work-<folder>`, `…-onnx` for a file that names the ONNX runtime's crate,
/// which only it compiles: `TemplateKind::StrategyOnnx`), whose `target/` keeps the dependency closure
/// compiled between files, and the per-build jail's write grant is that directory alone (so a build
/// never writes the artifacts beside it). Builds into it run one at a time, across threads and
/// processes: each holds an exclusive lock on its `build.lock` from writing `lib.rs` until its artifact
/// is published, and looks at the cache again once it holds it. The sweep ([`prune_stale_scratch`])
/// retires, per SDK, the directory of a folder no build used for [`SCRATCH_MAX_AGE`]; a new directory
/// starts from the newest one's third-party units ([`seed_target`]).
pub fn build_strategy_file(
    text: &str,
    path: &str,
    builds_dir: &Path,
    workspace_root: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
    profile: Profile,
    cargo_home_pin: &CargoHomePin,
) -> Result<PathBuf, BuildError> {
    check_strategy_path(path)?;

    // Before the cache and before cargo: a source overriding a hook the vtable does not carry is
    // refused outright. Ahead of the cache deliberately — an artifact built before this rule
    // existed must not buy a source past it, and a refusal that depends on whether a file happens
    // to be on disk is not a rule.
    let unwired = unwired_hook_overrides(text);
    if !unwired.is_empty() {
        return Err(BuildError::UnwiredHook { hooks: Hooks(unwired) });
    }
    let entry = Entry::of(text)?;
    let tree = SourceTree::open(workspace_root)?;
    if let (Sdk::Dev, Some(pin)) = (tree.sdk, pin_of(text)) {
        eprintln!(
            "vike-strategy-builder: strategy file {path:?} pins SDK {pin}, and {} is a development \
             checkout (SDK dev): the pin is ignored there",
            workspace_root.display()
        );
    }
    let sources = [
        ("lib.rs".to_string(), render_lib_rs(&tree.templates.strategy_lib_rs, text, entry)),
        ("strategy.rs".to_string(), text.to_string()),
    ];
    let kind = template_kind_of(text);
    let build = Build { builds_dir, cargo_bin, child_env, profile, cargo_home_pin };
    build_artifact(&tree, kind, &sha256_hex(text.as_bytes()), &sources, text, &build)
}

/// Build (or reuse the build of) the research STUDY file named `name` (owner's rule, 2026-11-15: a
/// study is a FILE, built like a strategy file): its folder's `.rs` files as `(file name, text)`,
/// `<name>.rs` the entry and the rest its sibling modules, each a plain `<stem>.rs` name (no `/`).
/// The caller reads the folder (`<project>/user_data/research/studies/rust/<name>/`) and hands the
/// texts over; this function reads no study folder. The artifact is
/// `<builds_dir>/sdk-N/<folder>/<sha256>.so` beside the strategy files', the sha that of
/// `vike_rust_strategy::artifact::study_identity` over the files sorted by name; returned with it. A
/// study has no header, so it builds against whatever SDK `workspace_root` is.
///
/// Everything else is [`build_strategy_file`]'s: the cache and the recorded failure, the source-tree
/// and cargo-home guards, the jailed `cargo build`, one shared work directory (a study one beside the
/// strategy one, its own template's closure) and its queue. The folder is written VERBATIM to
/// `src/study/` of the tree's study template (`template/study_lib.rs.in`), whose module `study` is the
/// entry file.
pub fn build_study_file(
    name: &str,
    files: &[(String, String)],
    builds_dir: &Path,
    workspace_root: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
    profile: Profile,
    cargo_home_pin: &CargoHomePin,
) -> Result<(PathBuf, String), BuildError> {
    let files = check_study(name, files)?;
    let sha = study_sha(name, &files);
    let tree = SourceTree::open(workspace_root)?;
    if tree.adapted.is_some() {
        // A study names no SDK: it builds against the SDK of this host's boundary, never through an adapter.
        return Err(BuildError::Sdk {
            why: format!(
                "a study builds against the SDK of this vike's own boundary, and {} was cut with another",
                tree.sdk
            ),
        });
    }
    let entry_file = format!("{name}.rs");
    let lib_rs = tree.templates.study_lib_rs.replace("{{ENTRY_FILE}}", &entry_file);
    let mut sources = vec![("lib.rs".to_string(), lib_rs)];
    sources.extend(files.iter().map(|(file, text)| (format!("study/{file}"), text.clone())));
    let all_text: String =
        files.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n");
    let build = Build { builds_dir, cargo_bin, child_env, profile, cargo_home_pin };
    let artifact = build_artifact(&tree, TemplateKind::Study, &sha, &sources, &all_text, &build)?;
    Ok((artifact, sha))
}

/// The sha256 a study's artifact is named by: of `vike_rust_strategy::artifact::study_identity`
/// over its files sorted by name, so the order a caller lists them in names no other artifact.
pub fn study_sha(name: &str, files: &[(String, String)]) -> String {
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    sha256_hex(&vike_rust_strategy::artifact::study_identity(name, &sorted))
}

/// A study as the builder accepts it, its files sorted by name: `name` a folder name of
/// `[a-z][a-z0-9_]*` (`vike_paths::state_path::is_rust_study_name`: it is the entry module's file stem,
/// the host's rule too), every file a plain `<stem>.rs` with no
/// `/`, `\`, `:` or control character (they are written under the scratch crate's `src/study/`), no
/// file named twice, and `<name>.rs` among them. Peer strings, so refused, never rewritten.
fn check_study(
    name: &str,
    files: &[(String, String)],
) -> Result<Vec<(String, String)>, BuildError> {
    let file_ok = |file: &str| {
        file.strip_suffix(".rs").is_some_and(|stem| {
            !stem.is_empty()
                && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
    };
    let mut sorted = files.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let refused = |why: String| Err(BuildError::InvalidStudy { why });
    if !vike_paths::state_path::is_rust_study_name(name) {
        return refused(format!(
            "study {name:?}: a study folder's name must match [a-z][a-z0-9_]*"
        ));
    }
    if let Some((file, _)) = sorted.iter().find(|(file, _)| !file_ok(file)) {
        return refused(format!("study {name}: {file:?} is not a plain `<name>.rs` file name"));
    }
    if sorted.windows(2).any(|w| w[0].0 == w[1].0) {
        return refused(format!("study {name}: a file is named twice"));
    }
    if !sorted.iter().any(|(file, _)| *file == format!("{name}.rs")) {
        return refused(format!(
            "study {name}: its folder holds no `{name}.rs`, the entry file whose stem is the \
             folder's name"
        ));
    }
    Ok(sorted)
}

/// The crate a strategy file names to run a network, as its source spells it.
const ONNX_CRATE_IDENT: &str = "vike_ml_onnx_prediction";

/// The template a strategy FILE builds from: `TemplateKind::StrategyOnnx` for a text that names
/// [`ONNX_CRATE_IDENT`] anywhere (the strategy template WITH its `vike-ml-onnx-prediction` line, in a
/// work directory of its own, so the runtime's first build, minutes, is paid only by a user whose file
/// runs a network: W6), else `TemplateKind::Strategy`. Complete for a file that USES the crate (any path
/// to it spells the name); a mention in a comment only costs that file the larger build.
fn template_kind_of(text: &str) -> TemplateKind {
    if text.contains(ONNX_CRATE_IDENT) {
        TemplateKind::StrategyOnnx
    } else {
        TemplateKind::Strategy
    }
}

/// The package name a `kind` build compiles: [`CRATE_NAME`], or [`STUDY_CRATE_NAME`].
fn crate_name(kind: TemplateKind) -> &'static str {
    match kind {
        TemplateKind::Strategy | TemplateKind::StrategyOnnx => CRATE_NAME,
        TemplateKind::Study => STUDY_CRATE_NAME,
    }
}

/// The package name every rendered STUDY file builds under, [`CRATE_NAME`]'s twin.
const STUDY_CRATE_NAME: &str = "study_file";

/// What a build runs `cargo` with (the profile's flag aside, which the build folder names apart): one
/// part of the recipe a folder is named by ([`SourceTree::folder`]).
const CARGO_BUILD_ARGS: [&str; 2] = ["build", "--offline"];

/// Where and how one build runs: the caller's parameters, the same for every kind.
struct Build<'a> {
    builds_dir: &'a Path,
    cargo_bin: &'a str,
    child_env: &'a [(String, String)],
    profile: Profile,
    cargo_home_pin: &'a CargoHomePin,
}

/// The one build both kinds take ([`build_strategy_file`]'s doc), against `tree`: `sources` are written
/// under the work directory's `src/` (after it is emptied, so no file of an earlier build is compiled),
/// `hints_src` is the text the moved-name hints are read from.
fn build_artifact(
    tree: &SourceTree<'_>,
    kind: TemplateKind,
    sha: &str,
    sources: &[(String, String)],
    hints_src: &str,
    build: &Build<'_>,
) -> Result<PathBuf, BuildError> {
    let Build { builds_dir, cargo_bin, child_env, profile, cargo_home_pin } = *build;
    let workspace_root = tree.root;
    let folder = tree.folder(kind, profile);
    let artifact = vike_rust_strategy::artifact::artifact_path(builds_dir, tree.sdk, &folder, sha);
    let failure = artifact.with_extension("err");
    if let Some(hit) = cached(&artifact, &failure, workspace_root, cargo_bin, child_env)? {
        return Ok(hit);
    }

    // The cargo-home pin (`CargoHomePin`'s doc) runs HERE — after the cache, not before it: a hit
    // serves what was already built. Still ahead of every write and of `cargo`. The folder is honest
    // by its name: it is named by the source hash the tree's own marker states (verified at the
    // builder's start) or the checkout's hash computed now, so an artifact in it was built from the
    // sources its name says.
    if let Err((was, now)) = cargo_home_pin.check() {
        return Err(BuildError::CargoHomeTampered {
            was_present: Presence(was.is_some()),
            now_present: Presence(now.is_some()),
        });
    }

    // The artifact's own folder, `<builds>/sdk-N/<folder>/` (`artifact_path`'s shape).
    let folder_dir = artifact.parent().unwrap_or(builds_dir);
    fs::create_dir_all(folder_dir)
        .map_err(|e| BuildError::Io { why: format!("creating {}: {e}", folder_dir.display()) })?;

    let root_tag = &root_tag_of(workspace_root);
    let build_dir = work_directory(builds_dir, tree.sdk, &folder, kind);
    let source_dir = build_dir.join("src");
    fs::create_dir_all(&source_dir)
        .map_err(|e| BuildError::Io { why: format!("creating {}: {e}", source_dir.display()) })?;
    let lock_path = build_dir.join("build.lock");
    let lock = fs::File::create(&lock_path)
        .and_then(|f| f.lock().map(|()| f))
        .map_err(|e| BuildError::Io { why: format!("locking {}: {e}", lock_path.display()) })?;
    // Another holder of the lock may have built this very text while this one waited.
    if let Some(hit) = cached(&artifact, &failure, workspace_root, cargo_bin, child_env)? {
        return Ok(hit);
    }
    // SAID, one line per build that runs cargo, so the journal tells a build from a cache hit (the
    // release's toolchain smoke counts it: `scripts/release_container_image.sh`'s `builds`).
    eprintln!(
        "vike-strategy-builder: building {sha} with cargo, against {} ({folder})",
        tree.sdk.directory_name()
    );

    // Re-written on every build: the stamp `prune_stale_scratch` ages a work directory by.
    fs::write(build_dir.join("Cargo.toml"), tree.cargo_toml(kind))
        .map_err(|e| BuildError::Io { why: format!("writing Cargo.toml: {e}") })?;
    seed_lock(workspace_root, &build_dir).map_err(|why| BuildError::Io { why })?;
    // An adapted SDK compiles this host's boundary as its glue, laid out beside the crate (`lay_out_glue`).
    tree.lay_out(&build_dir).map_err(|why| BuildError::Io { why })?;
    // The size pass walks every file, so only a build that starts a new directory pays it; the sweep
    // is per SDK (`build_dir`'s parent holds this SDK's work directories and nothing of another's).
    let new_dir = !build_dir.join("target").exists();
    let (max_bytes, now) = (new_dir.then_some(SCRATCH_MAX_BYTES), SystemTime::now());
    if let Some(sdk_dir) = build_dir.parent() {
        prune_stale_scratch(
            sdk_dir,
            &build_dir,
            is_work_directory,
            SCRATCH_MAX_AGE,
            max_bytes,
            now,
        );
    }
    if new_dir && tree.sdk == Sdk::Dev {
        prune_stale_scratch(
            &scratch_root(),
            &build_dir,
            is_pre_sdk_scratch,
            SCRATCH_MAX_AGE,
            None,
            now,
        );
    }
    seed_target(&build_dir, kind, profile);
    // Emptied first: a study's sibling a previous study had and this one lacks must not compile.
    let _ = fs::remove_dir_all(&source_dir);
    sources
        .iter()
        .try_for_each(|(file, text)| {
            let path = source_dir.join(file);
            path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(path, text))
        })
        .map_err(|e| BuildError::Io { why: format!("writing the scratch crate's source: {e}") })?;

    let crate_name = crate_name(kind);
    let output = cargo_build_jailed(&build_dir, workspace_root, cargo_bin, child_env, profile)?;
    if !output.status.success() {
        // rustc's own words, verbatim — never re-summarized; the hints, when the source draws one,
        // come AFTER them (`compile_diagnostics`).
        let diagnostics = compile_diagnostics(&output.stderr, hints_src);
        let own_crate = format!("could not compile `{crate_name}`");
        if String::from_utf8_lossy(&output.stderr).contains(&own_crate) {
            publish(&failure, root_tag, |dst| dst.write_all(diagnostics.as_bytes()))?;
        }
        return Err(BuildError::Compile { diagnostics });
    }

    let built_dir = build_dir.join("target").join(profile.target_subdir());
    let built = find_cdylib(&built_dir, crate_name).ok_or_else(|| BuildError::Io {
        why: format!(
            "cargo reported success but no cdylib for `{crate_name}` was found under {}",
            built_dir.display()
        ),
    })?;
    let mut from = fs::File::open(&built)
        .map_err(|e| BuildError::Io { why: format!("reading {}: {e}", built.display()) })?;
    publish(&artifact, root_tag, |dst| io::copy(&mut from, dst).map(drop))?;
    drop(lock);
    Ok(artifact)
}

/// Names a temporary file [`publish`] writes, so two checkouts publishing one text never share it.
/// Hashed because a path is not a file-name-safe string.
fn root_tag_of(workspace_root: &Path) -> String {
    sha256_hex(workspace_root.display().to_string().as_bytes())[..12].to_string()
}

/// The work directory a `kind` build of `folder` against `sdk` uses
/// (`vike_rust_strategy::artifact::work_directory`): a numbered SDK's under `builds_dir`, persistent;
/// SDK dev's under [`scratch_root`], shared by a checkout's builds whatever their builds directory (this
/// module's doc).
fn work_directory(builds_dir: &Path, sdk: Sdk, folder: &str, kind: TemplateKind) -> PathBuf {
    let base = if sdk == Sdk::Dev { scratch_root() } else { builds_dir.to_path_buf() };
    vike_rust_strategy::artifact::work_directory(&base, sdk, folder, kind)
}

/// The work directory a `kind` build from `workspace_root` into `builds_dir` under `profile` uses
/// ([`build_strategy_file`]'s shared one). `pub` so a test looks inside exactly it.
///
/// # Errors
/// [`SourceTree`]'s: a tree that is neither an installed SDK nor a checkout, or holds no template.
pub fn work_directory_of(
    builds_dir: &Path,
    workspace_root: &Path,
    kind: TemplateKind,
    profile: Profile,
) -> Result<PathBuf, BuildError> {
    let tree = SourceTree::open(workspace_root)?;
    Ok(work_directory(builds_dir, tree.sdk, &tree.folder(kind, profile), kind))
}

/// The SDK `workspace_root` is and the build folder a `kind` artifact built from it under `profile`
/// lands in (`vike_rust_strategy::artifact::build_folder` over the tree's source hash for the kind and
/// this builder's recipe): with the text's sha, where [`build_strategy_file`] puts it.
///
/// # Errors
/// [`SourceTree`]'s: a tree that is neither an installed SDK nor a checkout, or holds no template.
pub fn build_folder(
    workspace_root: &Path,
    kind: TemplateKind,
    profile: Profile,
) -> Result<(Sdk, String), BuildError> {
    let tree = SourceTree::open(workspace_root)?;
    Ok((tree.sdk, tree.folder(kind, profile)))
}

/// What a build of this text already answered in its folder: `Ok(Some(artifact))` for a current
/// artifact, `Err(Compile)` for a recorded failure of its own crate, `Ok(None)` for neither.
///
/// ⚠ **A hit is VERIFIED, not just found.** The artifact's compatibility record is read from the FILE
/// and checked against this host's `ABI_VERSION`, toolchain fingerprint, layout and strategy boundary
/// (`vike_rust_strategy::loader::check_identity`, one bounded read, nothing close to a `cargo` run); a
/// mismatch, or a file with no readable record, falls through to a REBUILD, said on stderr, never an
/// error: the cache existing must never make a request fail that a cold one would have satisfied.
///
/// ⚠ **It runs NONE of the artifact's code** (2026-11-23; it was a `dlopen`, `verify_handshake`). A
/// `dlopen` here ran the artifact's initializers (`.init_array`) inside this process, in the serving
/// thread whose jail WRITES the builds directory, so one strategy could replace another's published
/// artifact with a link to the settings database before a portfolio named both. The record carries the
/// three values the exported handshake answered (and the boundary), so reading it proves what that
/// `dlopen` proved for a file this builder published; the loader still asks both in the jailed child.
/// On a box that CANNOT jail, a hit is refused the way a fresh build is: every Build is refused there.
///
/// A hit (an artifact or a recorded failure) is a USE: [`stamp_use`] records it, so retention keeps
/// what is used (`crate::builder::prune`).
fn cached(
    artifact: &Path,
    failure: &Path,
    workspace_root: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
) -> Result<Option<PathBuf>, BuildError> {
    if artifact.exists() {
        vike_sandbox::probe()
            .and_then(|()| crate::jail::grants(workspace_root, &[], cargo_bin, child_env).map(drop))
            .map_err(|why| BuildError::Unsandboxed { why })?;
        match vike_rust_strategy::loader::check_identity(artifact) {
            Ok(()) => {
                stamp_use(artifact);
                return Ok(Some(artifact.to_path_buf()));
            }
            Err(mismatch) => eprintln!(
                "vike-strategy-builder: cached artifact at {} no longer matches this host \
                 ({mismatch}) — rebuilding rather than serving it",
                artifact.display()
            ),
        }
    }
    match fs::read_to_string(failure) {
        Ok(diagnostics) => {
            stamp_use(failure);
            Err(BuildError::Compile { diagnostics })
        }
        Err(_) => Ok(None),
    }
}

/// Record that `path` was just served: its ACCESS time is set to now, its bytes and its modification
/// time (when it was built) stay as they were, so a hit rewrites nothing. Retention evicts by this
/// stamp (`crate::builder::prune`). Set explicitly because a `noatime` or `relatime` mount does not
/// keep it for a read. Best effort: a stamp that cannot be written costs an earlier eviction, never
/// the request.
fn stamp_use(path: &Path) {
    let _ = fs::File::open(path)
        .and_then(|file| file.set_times(fs::FileTimes::new().set_accessed(SystemTime::now())));
}

/// Write `dst` whole or not at all: into a temporary file beside it (named for this build
/// directory's `root_tag`, so two checkouts publishing one text never share it), then a `rename`
/// over whatever entry `dst` holds. A reader therefore never sees half an artifact, and a `rename`
/// replaces a planted symlink rather than writing through it.
fn publish(
    dst: &Path,
    root_tag: &str,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> Result<(), BuildError> {
    let mut name = dst.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    name.push(format!(".{root_tag}.tmp"));
    let tmp = dst.with_file_name(name);
    let _ = fs::remove_file(&tmp);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| write(&mut f))
        .and_then(|()| fs::rename(&tmp, dst));
    written.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        BuildError::Io { why: format!("writing {}: {e}", dst.display()) }
    })
}

/// A strategy file's path under `strategies/` as the builder accepts it: relative, `/`-separated,
/// ending in `.rs`, with no empty, `.` or `..` component and no `\`, `:` or control character — the
/// project's rule for a strategy's identity (an absolute path or one climbing out with `..` is
/// refused), checked here because the path is a wire peer's string.
fn check_strategy_path(path: &str) -> Result<(), BuildError> {
    let plain = path.ends_with(".rs")
        && !path.starts_with('/')
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != "..");
    if plain { Ok(()) } else { Err(BuildError::InvalidPath { path: path.to_string() }) }
}

/// Which entry a strategy file exports (plan 2b, section 2): `build`, the strategy contract, or
/// `rule`, the order language, with the `[run]` options of the file's header
/// (`vike_strategy::file::StrategyFileHeader`; absent: every switch off). Read off the TEXT: an item
/// is exported when a line STARTS with `pub fn <name>(` or `pub fn <name><` at column 0 — a
/// top-level item as rustfmt writes it; an indented method of the same name is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Build,
    Rule(StrategyFileHeaderRun),
}

impl Entry {
    /// The file's entry; a file exporting both is refused rather than one picked, and a `rule`
    /// file whose header does not hold is refused with the header's reason (its `[run]` options
    /// decide how it trades). A file exporting neither is a `build` file, whose missing `build`
    /// rustc names.
    fn of(text: &str) -> Result<Self, BuildError> {
        match (exports(text, "build"), exports(text, "rule")) {
            (true, true) => Err(BuildError::TwoEntries),
            (false, true) => StrategyFileHeader::parse(text)
                .map(|header| Entry::Rule(header.run.unwrap_or_default()))
                .map_err(|why| BuildError::Header { why }),
            _ => Ok(Entry::Build),
        }
    }
}

/// Whether `text` exports the top-level function `name` ([`Entry`]'s column-0 rule).
fn exports(text: &str, name: &str) -> bool {
    text.lines().any(|line| {
        line.strip_prefix("pub fn ")
            .and_then(|rest| rest.strip_prefix(name))
            .is_some_and(|rest| rest.starts_with('(') || rest.starts_with('<'))
    })
}

/// The template's `lib.rs` (`template`, the tree's `template/lib.rs.in`) for the file `text`: the entry
/// it exports and its optional `refuse` and `echo`, each called in the module `strategy`, which is the
/// file itself (`src/strategy.rs`, written beside it by [`build_strategy_file`], VERBATIM).
fn render_lib_rs(template: &str, text: &str, entry: Entry) -> String {
    let entry_expression = match entry {
        Entry::Build => "::vike_rust_strategy::guest::build_entry(\
                         strategy::build::<::vike_rust_strategy::guest::HostBroker>, &params, &facts)"
            .to_string(),
        Entry::Rule(run) => {
            let StrategyFileHeaderRun { bound_exits, stacked_closes, fills_a_day, touch_at_market } =
                run;
            format!(
                "{{ if facts.symbol.is_empty() {{ return None; }} \
                 Box::new(::vike_tactics::RuleRun::new(strategy::rule(&params), &facts.symbol)\
                 .with(::vike_tactics::RuleRunOptions {{ bound_exits: {bound_exits}, \
                 stacked_closes: {stacked_closes}, fills_a_day: {fills_a_day:?}, \
                 touch_at_market: {touch_at_market} }})) }}"
            )
        }
    };
    let none = "{ let _ = params; None }";
    let refuse = if exports(text, "refuse") { "strategy::refuse(params)" } else { none };
    let echo = if exports(text, "echo") {
        "Some(::vike_rust_strategy::guest::echo_entry(strategy::echo, params, facts))"
    } else {
        "{ let _ = (params, facts); None }"
    };
    template
        .replace("{{ENTRY}}", &entry_expression)
        .replace("{{REFUSE}}", refuse)
        .replace("{{ECHO}}", echo)
}

/// [`build_strategy_file`]'s cargo step: `cargo build --offline` of the crate at `scratch_dir`, inside the
/// build jail (`crate::jail`) — so cargo, rustc, the linker and every build script read only the
/// toolchain, `workspace_root`, the cargo home and system files, write only `scratch_dir` and the
/// cargo home, and open no socket. `pub` so the hostile-build test drives exactly this step with a
/// crate of its own (`crates/vike-strategy-builder/tests/build_jail.rs`).
///
/// The child starts from `env_clear()` plus `child_env`, with `CARGO_TARGET_DIR` and `TMPDIR`
/// inside `scratch_dir` and `scratch_dir` as its working directory (so no `.cargo/config.toml` of
/// the caller's working tree is read). `--offline`: the jail has no network, so crates come from
/// the cargo home (`prefetch` fills it once, at the builder's start).
///
/// # Errors
/// [`BuildError::Unsandboxed`] when the jail cannot be applied; [`BuildError::Toolchain`] when
/// cargo cannot be started; [`BuildError::Io`] when the temporary directory cannot be made.
pub fn cargo_build_jailed(
    scratch_dir: &Path,
    workspace_root: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
    profile: Profile,
) -> Result<std::process::Output, BuildError> {
    vike_sandbox::probe().map_err(|why| BuildError::Unsandboxed { why })?;
    let grants = crate::jail::grants(workspace_root, &[scratch_dir], cargo_bin, child_env)
        .map_err(|why| BuildError::Unsandboxed { why })?;
    let tmp = scratch_dir.join("tmp");
    fs::create_dir_all(&tmp)
        .map_err(|e| BuildError::Io { why: format!("creating {}: {e}", tmp.display()) })?;
    let wrapper_off = cargo_config_may_exist(scratch_dir, child_env);
    let run = || {
        let mut cmd = Command::new(cargo_bin);
        // ⚠ `env_clear()` FIRST, then only the allowlist. This process's environment carries this
        // service's own key (or the path to it), `CREDENTIALS_DIRECTORY`, and anything an operator
        // put in the unit's `.env`; `cargo` would hand all of it to a caller's `build.rs`.
        // `crates/vike-strategy-builder/tests/child_env.rs` proves it against the real daemon — a
        // planted variable and the key both stay out of the child — and deleting this call is the
        // mutation it goes red on.
        cmd.env_clear().envs(child_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.args(CARGO_BUILD_ARGS);
        if let Some(flag) = profile.cargo_flag() {
            cmd.arg(flag);
        }
        // ⚠ FORCE OFF any `rustc-wrapper` a `config.toml` sets (`sccache` on a dev or CI box): a
        // wrapper runs AS the compiler and sccache talks to its server over a SOCKET, which the jail
        // refuses — so a wrapper here breaks the build outright, not merely "no cache". An empty
        // `RUSTC_WRAPPER` disables it and OVERRIDES the config file, which `env_clear` alone cannot
        // (the wrapper is a config setting, not an inherited variable). ONLY where such a file may
        // exist ([`cargo_config_may_exist`]): cargo stats the empty wrapper's path for its `rustc`
        // probe cache (`target/.rustc_info.json`), fails, and drops the cache, so every build ran
        // `rustc -vV` four times and the target probe once more (measured on the CI box, 2026-11-19). The
        // deployed cargo home has no config, so there the cache holds; cargo keys it by the
        // compiler's path and mtime, so a new toolchain is probed afresh.
        if wrapper_off {
            cmd.env("RUSTC_WRAPPER", "").env("RUSTC_WORKSPACE_WRAPPER", "");
        }
        cmd.arg("--manifest-path")
            .arg(scratch_dir.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", scratch_dir.join("target"))
            .env("TMPDIR", &tmp)
            // ⚠ When `workspace_root` is a git checkout, cargo lists a path dependency's files
            // through git, which reads the GLOBAL and SYSTEM git excludes (`~/.config/git/ignore`,
            // `/etc/gitconfig`). Those are OUTSIDE the jail, and Landlock returns `EACCES` for a
            // denied path where the unjailed world returns `ENOENT` — which git treats as fatal
            // ("Could not read repository exclude") instead of "no global excludes". Point git at
            // nothing for both, and HOME at the build's own scratch, so only the repo's OWN
            // `.git/info/exclude` (granted, under `workspace_root`) is read. A deployed vendored
            // tree is not a git repo, so none of this runs there.
            .env("HOME", scratch_dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .current_dir(scratch_dir)
            .output()
    };
    vike_sandbox::run_jailed(&grants, run).map_err(|why| BuildError::Unsandboxed { why })?.map_err(
        |e| BuildError::Toolchain { why: format!("could not run `{cargo_bin} build`: {e}") },
    )
}

/// Whether a cargo configuration file a build in `scratch_dir` reads may exist: `.cargo/config` or
/// `.cargo/config.toml` in `scratch_dir` or any directory above it (cargo's own walk from its working
/// directory; `HOME` is `scratch_dir`, so `~/.cargo` is among them), or `config` / `config.toml` in
/// `child_env`'s `CARGO_HOME`. Only such a file can name a `rustc-wrapper`, since the spawn forwards
/// no `RUSTC_WRAPPER`; one that cannot even be looked at counts as present.
fn cargo_config_may_exist(scratch_dir: &Path, child_env: &[(String, String)]) -> bool {
    let cargo_home = child_env.iter().find(|(k, _)| k == "CARGO_HOME").map(|(_, v)| Path::new(v));
    let mut files: Vec<PathBuf> = scratch_dir
        .ancestors()
        .flat_map(|dir| [dir.join(".cargo/config"), dir.join(".cargo/config.toml")])
        .collect();
    files.extend(cargo_home.into_iter().flat_map(|h| [h.join("config"), h.join("config.toml")]));
    files
        .iter()
        .any(|f| !matches!(fs::symlink_metadata(f), Err(e) if e.kind() == io::ErrorKind::NotFound))
}

/// Fill the cargo home with every crate the cdylib templates can reach — `cargo fetch` over each
/// template (the strategy file's, without and with its ONNX line, and the research study file's)
/// rendered around an empty source — so
/// a jailed build, which has no network, finds them (`cargo_build_jailed`'s `--offline`). The builder
/// runs it ONCE, at start, before it jails itself (`crate::builder::run`): cargo's own code over the
/// FIXED template manifests, so no source a caller sent is read and no build script runs. It still
/// runs jailed, with the network allowed: it reads and writes only what a build does.
///
/// `workspace_root` is one source tree (an installed SDK or a checkout, [`SourceTree`]): the builder
/// prefetches each SDK it serves, so the cargo home holds every kept SDK's locked crates.
///
/// # Errors
/// cargo's own words when a fetch fails, or why the tree is none; the builder says them and goes on
/// (a warm cargo home still builds).
pub fn prefetch(
    workspace_root: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
) -> Result<(), String> {
    let tree = SourceTree::open(workspace_root).map_err(|e| e.to_string())?;
    TemplateKind::ALL
        .into_iter()
        .filter(|kind| tree.adapted.is_none() || *kind != TemplateKind::Study)
        .try_for_each(|kind| prefetch_kind(&tree, kind, cargo_bin, child_env))
}

/// [`prefetch`] over one template.
fn prefetch_kind(
    tree: &SourceTree<'_>,
    kind: TemplateKind,
    cargo_bin: &str,
    child_env: &[(String, String)],
) -> Result<(), String> {
    let workspace_root = tree.root;
    let tag = root_tag_of(workspace_root);
    let dir = scratch_root().join(format!("{PREFETCH_PREFIX}{tag}{}", kind.work_suffix()));
    let src = dir.join("src");
    fs::create_dir_all(&src).map_err(|e| format!("creating {}: {e}", src.display()))?;
    fs::write(dir.join("Cargo.toml"), tree.cargo_toml(kind))
        .and_then(|()| fs::write(src.join("lib.rs"), ""))
        .map_err(|e| format!("writing the template under {}: {e}", dir.display()))?;
    // The fetch takes the versions the builds will resolve to (`seed_lock`), never the newest ones.
    seed_lock(workspace_root, &dir)?;
    tree.lay_out(&dir)?;
    let grants = crate::jail::network_grants(workspace_root, &[&dir], cargo_bin, child_env)?;
    let run = || {
        Command::new(cargo_bin)
            .env_clear()
            .envs(child_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .args(["fetch", "--manifest-path"])
            .arg(dir.join("Cargo.toml"))
            .env("TMPDIR", &dir)
            .current_dir(&dir)
            .output()
    };
    let out = vike_sandbox::run_jailed(&grants, run)?
        .map_err(|e| format!("could not run `{cargo_bin} fetch`: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// **Seed the scratch crate at `dir` with the tree's `Cargo.lock`** (review L1's second half), where
/// `workspace_root` carries one: written before every build and every prefetch, so the scratch crate
/// resolves to the versions the tree locked (the entries its source hash, `vike_rust_strategy::sdk::source_hash`,
/// names its build folder by) instead of the newest ones the cargo home happens to hold, and the prefetch
/// fetches those same versions. Not `--locked`: the workspace lock has no entry for the scratch crate itself,
/// and cargo adding it (and dropping the packages it does not link) is exactly the edit `--locked` refuses. A
/// tree with no lock leaves the directory as it was.
fn seed_lock(workspace_root: &Path, dir: &Path) -> Result<(), String> {
    match fs::read(workspace_root.join("Cargo.lock")) {
        Ok(lock) => fs::write(dir.join("Cargo.lock"), lock)
            .map_err(|e| format!("writing {}: {e}", dir.join("Cargo.lock").display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("reading {}: {e}", workspace_root.join("Cargo.lock").display())),
    }
}

/// **Seed a new work directory's `target/` from the newest other one's THIRD-PARTY units**, so the
/// first build of a new folder (most often a NEW SDK's first, from the previous SDK's) compiles the
/// tree's own crates and the strategy, not the registry crates again. `build_dir` is the new work
/// directory (`<base>/sdk-N/work-<folder><kind suffix>`), its lock held and its `Cargo.lock` written;
/// the source is the newest, by its `Cargo.toml` stamp (the age sweep's), of the `kind` work
/// directories of every SDK under the same base (`<base>/sdk-*/work-…`) whose `target/<profile>/build/`
/// exists. `pub` so the seeding test drives exactly this step (`crates/vike-strategy-builder/tests/build_jail.rs`).
///
/// ⚠ **Never a `vike-*` unit, and never a target carried whole between SDKs**: a target only ever sees
/// one immutable tree (this module's doc), which is what makes cargo's mtime check sound there.
///
/// ⚠ **Only packages `Cargo.lock` names with a `source` are copied, never a `path` one, and that is
/// the correctness argument, not an optimisation.** Cargo judges a path crate fresh by its files'
/// MTIMES against its last build, and a deployed tree carries `git archive`'s mtimes: every file the
/// COMMIT time (`scripts/package_strategy_source.sh`), which can be EARLIER than a build in the
/// previous folder. Measured on the CI box (2026-11-19): a whole copied `target/`, then an `sma` changed in
/// `vike-indicators` with such an mtime, compiled only the strategy and linked the OLD `sma`. A
/// registry unit is judged by its version, checksum, features, profile and compiler, never an mtime,
/// so the copy is reused exactly where cargo itself says so. A package's unit directory is
/// `target/<profile>/build/<package>/` (cargo's build directory layout of 1.100); under another layout
/// nothing matches and the build is cold, as before.
///
/// Best effort, never an error: no source, a source a build holds (its `build.lock`, tried, never
/// waited on), or a copy that fails leaves `target/` absent, a cold build.
///
/// shortcut: a kept package's units are copied whole, a dead one (an older compiler's, an old feature
/// set's) too, and ride every later seed until a folder goes unused for `SCRATCH_MAX_AGE`; copy only
/// the units the source's last build used if a scratch directory grows.
pub fn seed_target(build_dir: &Path, kind: TemplateKind, profile: Profile) {
    let target = build_dir.join("target");
    let build_subdir = |dir: &Path| dir.join("target").join(profile.target_subdir()).join("build");
    if target.exists() {
        return;
    }
    let Ok(lock) = fs::read_to_string(build_dir.join("Cargo.lock")) else { return };
    let Some(Ok(sdks)) = build_dir.parent().and_then(Path::parent).map(fs::read_dir) else {
        return;
    };
    let newest = sdks
        .flatten()
        .filter(|sdk| sdk.file_name().to_str().and_then(Sdk::of_directory_name).is_some())
        .filter_map(|sdk| fs::read_dir(sdk.path()).ok())
        .flat_map(|works| works.flatten().map(|entry| entry.path()))
        .filter(|dir| dir != build_dir && build_subdir(dir).is_dir())
        .filter(|dir| dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| is_work_of(n, kind)))
        .filter_map(|dir| Some((fs::metadata(dir.join("Cargo.toml")).ok()?.modified().ok()?, dir)))
        .max();
    let Some((_, source)) = newest else { return };
    let Ok(source_lock) = fs::File::open(source.join("build.lock")) else { return };
    if source_lock.try_lock_shared().is_err() {
        return;
    }
    let (from, to) = (build_subdir(&source), build_subdir(build_dir));
    let copied = third_party_packages(&lock)
        .into_iter()
        .filter(|name| from.join(name).is_dir())
        .try_for_each(|name| copy_tree(&from.join(name), &to.join(name)));
    if copied.is_err() {
        let _ = fs::remove_dir_all(&target);
    }
}

/// The packages a `Cargo.lock` names with a `source` (registry or git), less any name a `path`
/// package (no `source`) also carries: those are [`seed_target`]'s to copy.
fn third_party_packages(lock: &str) -> BTreeSet<&str> {
    let (mut sourced, mut path) = (BTreeSet::new(), BTreeSet::new());
    for block in lock.split("[[package]]").skip(1) {
        let name = block.lines().find_map(|l| l.strip_prefix("name = \"")?.strip_suffix('"'));
        let Some(name) = name else { continue };
        if block.lines().any(|l| l.starts_with("source = ")) {
            sourced.insert(name);
        } else {
            path.insert(name);
        }
    }
    sourced.difference(&path).copied().collect()
}

/// Copy the directory `from` to `to`, every regular file with its modification time (cargo orders a
/// unit's outputs against its dependencies' by them). Anything else is skipped: a file cargo then
/// misses is a unit it rebuilds.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else if kind.is_file() {
            fs::copy(&src, &dst)?;
            let modified = entry.metadata()?.modified()?;
            fs::File::options().write(true).open(&dst)?.set_modified(modified)?;
        }
    }
    Ok(())
}

/// The directory SDK dev's work directories (`sdk-dev/work-…`) and the prefetch's live under: one per
/// box, in the temporary directory. A numbered SDK's work directories are not here: they are
/// persistent, under the builds directory (this module's doc). The builder creates it before it jails
/// itself, so its jail can name it.
pub fn scratch_root() -> PathBuf {
    std::env::temp_dir().join(SCRATCH_ROOT_DIR)
}

/// A failed compile's [`BuildError::Compile`] text: `cargo build`'s stderr VERBATIM, then — only
/// when `src` still reaches a market-data name, or a renamed seam name (`HftBroker`), through
/// `vike_model` — one `hint:` line per kind saying what to write instead
/// (`vike_model::host_build::moved_name::source_hints`, the one home of that knowledge, shared with
/// the compiled user-code hosts' build scripts). rustc's bare "unresolved import" says what failed;
/// the hint says the one-line fix. A source in the new spelling gets rustc's text and nothing else,
/// byte for byte.
fn compile_diagnostics(stderr: &[u8], src: &str) -> String {
    let mut msg = String::from_utf8_lossy(stderr).into_owned();
    for hint in vike_model::host_build::moved_name::source_hints(src) {
        if !msg.is_empty() && !msg.ends_with('\n') {
            msg.push('\n');
        }
        msg.push_str("hint: ");
        msg.push_str(&hint);
        msg.push('\n');
    }
    msg
}

/// The `vike_rust_strategy::host::UNWIRED_HOOKS` a source's own `Strategy` impl overrides,
/// sorted and deduplicated — empty for a source the strategy file mechanism can carry faithfully.
///
/// **Scoped to the `Strategy` impl body, not to the file**, and that scoping is the whole
/// difference between a usable rule and an unusable one: a strategy is perfectly entitled to an
/// inherent helper called `params` or `load_state`, and refusing those would be a false refusal
/// over a private name. Only an override inside an `impl … Strategy<…> for …` block is a hook
/// the host would have called.
///
/// ⚠ **Declared limits, because a text scan has them and a silent one is worse than a stated
/// one.** A `Strategy` impl produced by a macro, or written in a module this scan's brace
/// balance mis-tracks (an unbalanced brace inside a string literal in the impl body), is not
/// seen — the hook is then unwired and unrefused, which is the behaviour that existed before
/// this function and not a new failure. The bias is deliberate: an under-report costs what the
/// tree already paid, an over-report would refuse somebody's working strategy.
///
/// Since `ABI_VERSION` 8 every hook crosses but `failures`, the host's own count of the hooks that
/// failed, which a file may not answer; [`hook_overrides`] is the scan itself, over any list.
pub fn unwired_hook_overrides(src: &str) -> Vec<String> {
    hook_overrides(src, UNWIRED_HOOKS)
}

/// The names of `hooks` a source's own `Strategy` impl overrides, sorted and deduplicated: the
/// scan [`unwired_hook_overrides`] runs over `UNWIRED_HOOKS`, with that function's scope and limits.
pub fn hook_overrides(src: &str, hooks: &[&str]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for body in strategy_impl_bodies(src) {
        for line in body.lines() {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed.strip_prefix("fn ") else { continue };
            let Some(paren) = rest.find('(') else { continue };
            let name = rest[..paren].trim();
            if hooks.contains(&name) && !found.iter().any(|f| f == name) {
                found.push(name.to_string());
            }
        }
    }
    found.sort();
    found
}

/// The brace-balanced bodies of every `impl … Strategy<…> for …` block in `src`.
fn strategy_impl_bodies(src: &str) -> Vec<&str> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(rel) = src[at..].find("impl") {
        let i = at + rel;
        at = i + "impl".len();
        // A whole word, so `simple`/`implicit` are not impl blocks. The following byte may be
        // whitespace or `<` (`impl<B: Broker> …`).
        let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
        let after_ok = bytes.get(at).is_some_and(|b| b.is_ascii_whitespace() || *b == b'<');
        if !before_ok || !after_ok {
            continue;
        }
        let Some(brace_rel) = src[i..].find('{') else { break };
        let open = i + brace_rel;
        let header = &src[i..open];
        if !header.contains("Strategy<") || !header.contains(" for ") {
            continue;
        }
        let mut depth = 0usize;
        let mut end = open;
        for (k, b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = k;
                        break;
                    }
                }
                _ => {}
            }
        }
        if end > open {
            out.push(&src[open..end]);
            at = end;
        }
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Render `Cargo.toml.in`'s `{{NAME}}` placeholder plus every `path` dependency back into
/// the REAL checkout — `workspace_root.join("crates").join(…)`, never a compile-time-baked path
/// (see this module's doc for why: a running binary may not resolve a path from the manifest
/// directory the compiler baked into it, which is exactly what a bare `env!("CARGO_MANIFEST_DIR")`
/// here would do).
///
/// `onnx` false drops the template's ONE opt-in line, the `vike-ml-onnx-prediction` dependency
/// ([`ONNX_DEPENDENCY_LINE`]): a file that does not name the ONNX runtime never compiles it (W6).
///
/// ⚠ **The placeholder table only GROWS** (decision 0296): `templates` are the SDK's own (`template/`
/// under `workspace_root`), and every SDK still in the support window renders through this one table, so
/// a placeholder an older SDK's template names must stay here until that SDK retires.
fn render_cargo_toml(
    templates: &Templates,
    crate_name: &str,
    workspace_root: &Path,
    onnx: bool,
    glue: Option<&str>,
) -> String {
    let crate_path = |name: &str| workspace_root.join("crates").join(name).display().to_string();
    // An adapted SDK's `vike-rust-strategy` is the glue laid out beside the rendered crate (`GLUE_DIR`).
    let rust_strategy = glue.map_or_else(|| crate_path("vike-rust-strategy"), str::to_string);
    let template: String = templates
        .strategy_cargo_toml
        .split_inclusive('\n')
        .filter(|line| onnx || !line.starts_with(ONNX_DEPENDENCY_LINE))
        .collect();
    // ⚠ `VIKE_RUST_STRATEGY_PATH` is substituted BEFORE `VIKE_STRATEGY_PATH`, and the order is
    // load-bearing rather than tidy: `{{VIKE_STRATEGY_PATH}}` is not a substring of
    // `{{VIKE_RUST_STRATEGY_PATH}}` (`RUST_` breaks the common prefix), so either order is correct
    // today — but a future placeholder that IS a prefix of another would corrupt the longer one
    // silently, and `render_cargo_toml_leaves_no_placeholder_behind` could not see it because no
    // `{{` would survive. Longest-first is the rule that stays correct without a test noticing, so
    // the lines below run in order of placeholder length, a tie in either order.
    template
        .replace("{{VIKE_ML_ONNX_PREDICTION_PATH}}", &crate_path("vike-ml-onnx-prediction"))
        .replace("{{VIKE_RUST_STRATEGY_PATH}}", &rust_strategy)
        .replace("{{VIKE_ML_PREDICTION_PATH}}", &crate_path("vike-ml-prediction"))
        .replace("{{VIKE_INDICATORS_PATH}}", &crate_path("vike-indicators"))
        .replace("{{VIKE_MARKETDATA_PATH}}", &crate_path("vike-marketdata"))
        .replace("{{VIKE_PORTFOLIO_PATH}}", &crate_path("vike-portfolio"))
        .replace("{{VIKE_ARBITRAGE_PATH}}", &crate_path("vike-arbitrage"))
        .replace("{{VIKE_STRATEGY_PATH}}", &crate_path("vike-strategy"))
        .replace("{{VIKE_OPTIONS_PATH}}", &crate_path("vike-options"))
        .replace("{{VIKE_TACTICS_PATH}}", &crate_path("vike-tactics"))
        .replace("{{VIKE_VENUES_PATH}}", &crate_path("vike-venues"))
        .replace("{{VIKE_MODEL_PATH}}", &crate_path("vike-model"))
        .replace("{{VIKE_UTIL_PATH}}", &crate_path("vike-utility"))
        .replace("{{VIKE_MATH_PATH}}", &crate_path("vike-math"))
        .replace("{{VIKE_MM_PATH}}", &crate_path("vike-market-making"))
        .replace("{{NAME}}", crate_name)
}

/// [`render_cargo_toml`]'s twin for the research STUDY file template (the tree's
/// `template/study_Cargo.toml.in`), longest placeholder first and only growing, for that function's
/// reasons.
fn render_study_cargo_toml(
    templates: &Templates,
    crate_name: &str,
    workspace_root: &Path,
) -> String {
    let crate_path = |name: &str| workspace_root.join("crates").join(name).display().to_string();
    templates
        .study_cargo_toml
        .replace("{{VIKE_USER_RESEARCH_PATH}}", &crate_path("vike-user-research"))
        .replace("{{VIKE_RUST_STRATEGY_PATH}}", &crate_path("vike-rust-strategy"))
        .replace("{{VIKE_ML_PREDICTION_PATH}}", &crate_path("vike-ml-prediction"))
        .replace("{{VIKE_ML_TRAINING_PATH}}", &crate_path("vike-ml-training"))
        .replace("{{VIKE_INDICATORS_PATH}}", &crate_path("vike-indicators"))
        .replace("{{VIKE_MARKETDATA_PATH}}", &crate_path("vike-marketdata"))
        .replace("{{VIKE_ORDERFLOW_PATH}}", &crate_path("vike-orderflow"))
        .replace("{{VIKE_ANALYTICS_PATH}}", &crate_path("vike-analytics"))
        .replace("{{VIKE_PORTFOLIO_PATH}}", &crate_path("vike-portfolio"))
        .replace("{{VIKE_OPTIONS_PATH}}", &crate_path("vike-options"))
        .replace("{{VIKE_SIMULATOR_PATH}}", &crate_path("vike-simulator"))
        .replace("{{VIKE_ARBITRAGE_PATH}}", &crate_path("vike-arbitrage"))
        .replace("{{VIKE_MODEL_PATH}}", &crate_path("vike-model"))
        .replace("{{VIKE_FILLS_PATH}}", &crate_path("vike-fills"))
        .replace("{{VIKE_PATHS_PATH}}", &crate_path("vike-paths"))
        .replace("{{VIKE_MATH_PATH}}", &crate_path("vike-math"))
        .replace("{{VIKE_DATA_PATH}}", &crate_path("vike-data"))
        .replace("{{VIKE_RAG_PATH}}", &crate_path("vike-rag"))
        .replace("{{NAME}}", crate_name)
}

/// Find the `cdylib` `cargo` just produced under `release_dir` — platform-native prefix/suffix
/// (`libNAME.so` / `NAME.dll` / `libNAME.dylib`), never the design's own `<sha>.so`
/// spelling, which is what THIS function's caller copies the file TO, not what it looks for.
fn find_cdylib(release_dir: &Path, crate_name: &str) -> Option<PathBuf> {
    let file_name = format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        crate_name.replace('-', "_"),
        std::env::consts::DLL_SUFFIX
    );
    let candidate = release_dir.join(&file_name);
    if candidate.is_file() { Some(candidate) } else { None }
}

// ⚠ **`pub fn clean_scratch(name, src)` was here and is DELETED.** It removed the scratch
// directory `build_strategy_file` leaves behind, and it had ZERO callers anywhere in the tree — the
// builder service's retention pass prunes ARTIFACTS (`builder::prune_artifacts`), never scratch,
// and `build_strategy_file` deliberately keeps its own directory as cargo's incremental cache for a
// retried build of identical content. It was written for a caller that was never built.
//
// Deleted rather than wired up because calling it would have been WRONG, not merely unimplemented:
// the whole value of the content-addressed scratch tree is that a retry of the same source reuses
// its `target/`, so a caller that swept it after each build would turn every rebuild cold. The
// real cure for the unbounded growth is a STARTUP sweep of the whole tree
// (`vike_paths::scratch::sweep`) plus the deploy-unit write grant this module's header argues for
// — one PR, both halves — and that cure needs no per-build entry point.
//
// It also cost something concrete while it sat here: it was the SECOND of the two grandfathered
// rows in `crates/vike-ops/tests/hygiene/system_temp_gate/pin.rs`'s `SYSTEM_TEMP_PIN`, so a ratchet that may
// only shrink was carrying a row for a function nothing reached. That row is gone with it.

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    /// The checkout's own templates, as a build of it reads them.
    fn checkout_templates() -> Templates {
        SourceTree::open(&test_workspace_root()).map(|tree| tree.templates).expect("the checkout")
    }

    /// `env!("CARGO_MANIFEST_DIR")`, used here only — this INLINE `#[cfg(test)]` module is one of
    /// the ~50 sites `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs`'s own doc names as always
    /// correct ("a test resolving a fixture from its own manifest directory… there is no
    /// deployment"), and that gate's `inline_test_module_start`/`cfg_test_module_files` locators
    /// exclude inline test modules from its scan by construction. Two levels up from this crate's
    /// own manifest dir (`crates/vike-strategy-builder`) is the checkout root.
    pub(super) fn test_workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// The pruner's whole contract in one fixture: a stale entry goes, a fresh one stays, the
    /// caller's own entry stays even when stale, a stale entry with no `Cargo.toml` is aged by its
    /// directory, a stamp in the FUTURE is fresh, and a missing root is zero rather than an error.
    #[test]
    fn prune_stale_scratch_removes_only_entries_untouched_past_the_max_age() {
        let root = tempfile::tempdir().expect("tempdir");
        let now = SystemTime::now();
        let make = |name: &str, age: Duration, with_manifest: bool| {
            let dir = root.path().join(name);
            std::fs::create_dir_all(dir.join("target")).unwrap();
            std::fs::write(dir.join("target").join("big.bin"), b"x").unwrap();
            let stamp = if with_manifest {
                let m = dir.join("Cargo.toml");
                std::fs::write(
                    &m,
                    "[package]
",
                )
                .unwrap();
                std::fs::File::options().write(true).open(&m).unwrap()
            } else {
                std::fs::File::open(&dir).unwrap()
            };
            stamp.set_modified(now - age).unwrap();
            dir
        };
        let day = Duration::from_secs(24 * 60 * 60);
        let stale = make("stale", 3 * day, true);
        let fresh = make("fresh", day, true);
        let mine = make("mine", 30 * day, true);
        let bare_stale = make("bare-stale", 3 * day, false);
        let future = make("future", Duration::ZERO, true);
        std::fs::File::options()
            .write(true)
            .open(future.join("Cargo.toml"))
            .unwrap()
            .set_modified(now + day)
            .unwrap();
        std::fs::write(root.path().join("loose-file"), b"not a dir").unwrap();

        assert_eq!(
            prune_stale_scratch(root.path(), &mine, |_| true, SCRATCH_MAX_AGE, None, now),
            2
        );

        assert!(!stale.exists(), "a 3-day-old entry is gone, target/ and all");
        assert!(!bare_stale.exists(), "a manifest-less entry is aged by its directory");
        assert!(fresh.join("target/big.bin").exists(), "a 1-day-old entry keeps its target/");
        assert!(mine.exists(), "the caller's own entry is never pruned");
        assert!(future.exists(), "a future stamp counts as fresh");
        assert!(root.path().join("loose-file").exists(), "only directories are entries");
        assert_eq!(
            prune_stale_scratch(
                &root.path().join("absent"),
                &mine,
                |_| true,
                SCRATCH_MAX_AGE,
                None,
                now
            ),
            0
        );
    }

    /// The size pass, with a 12-byte cap over six fresh 4-byte entries: the least recently used
    /// go first until the swept entries hold no more than the cap, and it never removes the caller's
    /// own entry, one its `swept` filter does not name (here a `prefetch-…` one, which it does not
    /// count either: the cap is the swept entries') or one whose `build.lock` a build holds.
    #[test]
    fn prune_to_size_removes_least_recently_used_but_never_kept_prefetch_or_locked() {
        let root = tempfile::tempdir().expect("tempdir");
        let now = SystemTime::now();
        let make = |name: &str, minutes_ago: u64| {
            let dir = root.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("build.lock"), b"").unwrap();
            std::fs::write(dir.join("Cargo.toml"), b"1234").unwrap();
            let manifest = std::fs::File::options().write(true).open(dir.join("Cargo.toml"));
            manifest.unwrap().set_modified(now - Duration::from_secs(60 * minutes_ago)).unwrap();
            dir
        };
        let mine = make("mine", 60);
        let prefetch = make("prefetch-abc", 50);
        let locked = make("locked", 40);
        let old = make("old", 30);
        let middle = make("middle", 20);
        let newest = make("newest", 10);
        let held = std::fs::File::open(locked.join("build.lock")).unwrap();
        held.lock().unwrap();

        // 20 swept bytes (the prefetch entry is not swept) against a 12-byte cap: `old` then `middle`
        // go, 12 is within it, `newest` stays.
        let swept: fn(&str) -> bool = |name| !name.starts_with(PREFETCH_PREFIX);
        assert_eq!(
            prune_stale_scratch(root.path(), &mine, swept, SCRATCH_MAX_AGE, Some(12), now),
            2
        );
        assert!(!old.exists() && !middle.exists(), "the least recently used go first");
        assert!(mine.exists() && prefetch.exists() && locked.exists(), "never kept/prefetch/held");
        assert!(newest.exists(), "removal stops once the root is under the cap");
        drop(held);
    }

    /// `CargoHomePin` treats absent-at-startup, present-and-unchanged, and drift (in either
    /// direction — appearing OR changing OR disappearing) as three distinct answers, and
    /// `disabled()` as a fourth that never measures anything. Found missing by an adversarial
    /// review of the deployed jail: a caller's own `build.rs` could rewrite `$CARGO_HOME/config.toml`
    /// directly (no traversal needed — cargo hands it the path) and poison every LATER caller's
    /// build with a hijacked registry or an arbitrary `credential-provider`.
    #[test]
    fn cargo_home_pin_tells_absent_present_and_drift_apart() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Absent at startup, still absent: no drift.
        let pin = CargoHomePin::capture(dir.path());
        assert_eq!(pin.check(), Ok(()));

        // Absent at startup, appears later: drift — an operator's config existing is fine, but
        // ONE THAT ARRIVED AFTER STARTUP is exactly the write this guard exists to catch.
        std::fs::write(dir.path().join("config.toml"), "[source.crates-io]\n").unwrap();
        assert_eq!(pin.check(), Err((None, Some(sha256_hex(b"[source.crates-io]\n")))));

        // A fresh pin over the now-present file: present at startup, unchanged since — no drift,
        // exactly the "an operator's own config.toml is legitimate" case the design must not
        // punish.
        let pin2 = CargoHomePin::capture(dir.path());
        assert_eq!(pin2.check(), Ok(()));

        // Present at startup, its CONTENT changes: drift, never silently accepted as "still a
        // file".
        std::fs::write(dir.path().join("config.toml"), "[registry]\ncredential-provider=[\"x\"]\n")
            .unwrap();
        assert!(pin2.check().is_err(), "a changed file's content must count as drift");

        // Present at startup, removed since: drift in the other direction too.
        let pin3 = CargoHomePin::capture(dir.path());
        std::fs::remove_file(dir.path().join("config.toml")).unwrap();
        assert_matches!(pin3.check(), Err((Some(_), None)));

        // `disabled()` never touches the filesystem and never reports drift, whatever is on disk.
        let disabled = CargoHomePin::disabled();
        assert_eq!(disabled.check(), Ok(()));
        std::fs::write(dir.path().join("config.toml"), "anything").unwrap();
        assert_eq!(disabled.check(), Ok(()), "disabled() must ignore even a real change");
    }

    #[test]
    fn render_cargo_toml_leaves_no_placeholder_behind() {
        let (templates, root) = (checkout_templates(), test_workspace_root());
        for rendered in [
            render_cargo_toml(&templates, "some_strategy_file", &root, false, None),
            render_cargo_toml(&templates, "some_strategy_file", &root, true, None),
            render_study_cargo_toml(&templates, "some_strategy_file", &root),
        ] {
            assert!(!rendered.contains("{{"), "unrendered placeholder survived: {rendered}");
            assert!(rendered.contains("some_strategy_file"));
        }
    }

    /// W6: the ONNX runtime is compiled only for a file that names its crate. A file that does not
    /// gets the strategy template without that ONE line (everything else byte for byte) and the
    /// shared build directory; a file that does gets the whole template and a directory of its own.
    #[test]
    fn only_a_file_naming_the_onnx_crate_builds_with_it_in_a_directory_of_its_own() {
        let (templates, root) = (checkout_templates(), test_workspace_root());
        let (base, onnx) = (
            render_cargo_toml(&templates, "s", &root, false, None),
            render_cargo_toml(&templates, "s", &root, true, None),
        );
        let onnx_lines: Vec<&str> =
            onnx.lines().filter(|l| l.starts_with(ONNX_DEPENDENCY_LINE)).collect();
        assert_eq!(onnx_lines.len(), 1, "the template carries ONE opt-in line: {onnx_lines:?}");
        assert!(!base.lines().any(|l| l.starts_with(ONNX_DEPENDENCY_LINE)), "{base}");
        assert_eq!(onnx.replacen(&format!("{}\n", onnx_lines[0]), "", 1), base);

        let plain = "use vike_model::OrderBroker;\n// vike-ml-onnx-prediction is not named here\n";
        let network = "use vike_ml_onnx_prediction::OnnxModel;\n";
        assert_eq!(template_kind_of(plain), TemplateKind::Strategy);
        assert_eq!(template_kind_of(network), TemplateKind::StrategyOnnx);
        assert_eq!(
            template_kind_of("let m = ::vike_ml_onnx_prediction::OnnxModel::from_bytes;"),
            TemplateKind::StrategyOnnx
        );
        assert_eq!(crate_name(TemplateKind::Strategy), crate_name(TemplateKind::StrategyOnnx));
        let tree = SourceTree::open(&root).expect("the checkout");
        assert!(!tree.cargo_toml(TemplateKind::Strategy).contains(ONNX_DEPENDENCY_LINE));
        assert!(tree.cargo_toml(TemplateKind::StrategyOnnx).contains(ONNX_DEPENDENCY_LINE));
        let profile = Profile::Debug;
        assert_ne!(
            tree.folder(TemplateKind::Strategy, profile),
            tree.folder(TemplateKind::StrategyOnnx, profile),
            "a directory (and a folder) of its own"
        );
    }

    /// A study is refused by name before anything is built: its folder name, a file name that is not
    /// a plain `<stem>.rs`, a file named twice, a folder without its entry file. Its sha is over the
    /// files SORTED, so the order a caller lists them in names no other artifact.
    #[test]
    fn a_study_is_checked_and_named_by_its_sorted_files() {
        let file = |n: &str| (n.to_string(), format!("// {n}\n"));
        let refused = |name: &str, files: &[(String, String)]| {
            assert_matches!(
                check_study(name, files),
                Err(BuildError::InvalidStudy { .. }),
                "{name}"
            );
        };
        refused("Bad-Name", &[file("Bad-Name.rs")]);
        refused("poly_mm", &[file("poly_mm.rs"), file("../escape.rs")]);
        refused("poly_mm", &[file("poly_mm.rs"), file("sub/matrix.rs")]);
        refused("poly_mm", &[file("poly_mm.rs"), file("poly_mm.rs")]);
        refused("poly_mm", &[file("matrix.rs")]);
        let a = check_study("poly_mm", &[file("poly_mm.rs"), file("matrix.rs")]).unwrap();
        let b = check_study("poly_mm", &[file("matrix.rs"), file("poly_mm.rs")]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].0, "matrix.rs");
        assert_eq!(study_sha("poly_mm", &a), study_sha("poly_mm", &b));
        assert_ne!(study_sha("poly_mm", &a), study_sha("poly_mn", &a), "the name is part of it");
        assert_ne!(study_sha("poly_mm", &a), sha256_hex(a[1].1.as_bytes()), "never a strategy's");
    }

    /// Every path dependency the template declares must point at a directory that EXISTS in this
    /// checkout.
    ///
    /// ⚠ A `{{` check alone cannot catch the failure this covers. Add a dependency to the
    /// template and forget its `.replace` line and the placeholder survives — which the test
    /// above does see. Add the `.replace` line with a MISSPELLED crate directory and nothing
    /// survives, the manifest renders clean, and the first report is a cargo error inside a
    /// nested build several minutes later, wearing the builder's `BuildError::Toolchain`
    /// wrapper rather than naming the manifest.
    #[test]
    fn every_rendered_path_dependency_resolves_to_a_real_crate_directory() {
        let root = test_workspace_root();
        let templates = checkout_templates();
        let rendered = render_cargo_toml(&templates, "some_strategy_file", &root, true, None)
            + "\n[end]\n"
            + &render_study_cargo_toml(&templates, "some_study_file", &root);
        // ⚠ Scoped to the `[dependencies]` SECTION, because `[lib]` carries a `path` too
        // (`src/lib.rs`) and the first cut of this test asserted a Cargo.toml lived beside it.
        // Tracked by section header rather than parsed, because this crate carries no `toml`
        // dependency and adding one for a single assertion buys less than it costs; the guard
        // below is what keeps a line scanner honest here — it fails if the section was never
        // entered, which is the way a header rename would otherwise make this pass vacuously.
        let mut in_dependencies = false;
        let mut checked = 0usize;
        for line in rendered.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_dependencies = trimmed == "[dependencies]";
                continue;
            }
            if !in_dependencies {
                continue;
            }
            let Some((_, rest)) = line.split_once("path = \"") else { continue };
            let Some((path, _)) = rest.split_once('"') else { continue };
            checked += 1;
            let manifest = Path::new(path).join("Cargo.toml");
            assert!(
                manifest.is_file(),
                "the rendered template points at `{path}`, which holds no Cargo.toml — a \
                 misspelled crate directory renders a clean manifest and fails minutes later \
                 inside a nested cargo build"
            );
        }
        assert!(
            checked >= 4,
            "expected at least the four user-facing path dependencies, saw {checked} — if the \
             template's dependency table shrank, `crates/vike-ops/tests/docs/user_strategy_surface_gate.rs` \
             is the gate that says whether that was allowed"
        );
    }

    /// rustc's stderr, as a failed nested build hands it back (no trailing newline, to pin the
    /// separator).
    const STDERR: &str = "error[E0432]: unresolved import\nerror: could not compile `x`";

    /// A source still reaching a market-data name through `vike_model` gets rustc's text verbatim
    /// and then ONE golden `hint:` line. Its old spelling is built from parts: the market-data
    /// codemod rewrites the joined one wherever it finds it.
    #[test]
    fn a_compile_failure_of_an_old_market_data_import_carries_the_moved_name_hint() {
        let src = format!("use {}::{{Bar, OrderBroker, Strategy}};\n", "vike_model");
        assert_eq!(
            compile_diagnostics(STDERR.as_bytes(), &src),
            "error[E0432]: unresolved import\nerror: could not compile `x`\nhint: `Bar` moved from \
             `vike_model` to `vike_marketdata`: write `use vike_marketdata::Bar;`, keep the rest in \
             `use vike_model::{…}`, and start any inline path to it with `vike_marketdata::`\n"
        );
    }

    /// The new spelling (and any source naming no moved name) gets rustc's text and nothing else.
    #[test]
    fn a_compile_failure_of_the_new_spelling_is_rustcs_text_alone() {
        let src = format!(
            "use vike_marketdata::Bar;\nuse {}::{{OrderBroker, Strategy}};\n",
            "vike_model"
        );
        assert_eq!(compile_diagnostics(STDERR.as_bytes(), &src), STDERR);
        assert_eq!(compile_diagnostics(b"", "fn x() {}"), "");
    }

    /// A source still naming a renamed seam name (`HftBroker`, `ABI_VERSION` 8) gets rustc's text
    /// and then one golden `hint:` line per kind: the moved name's first, the renamed one's last.
    #[test]
    fn a_compile_failure_of_a_renamed_name_carries_the_renamed_name_hint() {
        let src = format!("use {}::{{Bar, HftBroker, Strategy}};\n", "vike_model");
        let got = compile_diagnostics(STDERR.as_bytes(), &src);
        assert!(
            got.ends_with(
                "`vike_marketdata::`\nhint: `HftBroker` is now `OrderBroker` in `vike_model`: \
                 write the new name wherever the file names the old one\n"
            ),
            "{got}"
        );
    }
    /// A strategy file's identity is its relative `.rs` path under `strategies/`; anything a wire
    /// peer could use to climb out, or that is not a plain path, is refused by name.
    #[test]
    fn only_a_plain_relative_rs_path_names_a_strategy_file() {
        for good in ["momentum.rs", "trend/momentum.rs", "a/b/c_d-e.rs"] {
            assert_eq!(check_strategy_path(good), Ok(()), "{good}");
        }
        for bad in [
            "",
            "momentum",
            "/etc/momentum.rs",
            "../momentum.rs",
            "trend/../../x.rs",
            "trend//x.rs",
            "./x.rs",
            "trend\\x.rs",
            "C:x.rs",
            "x\n.rs",
        ] {
            assert_eq!(
                check_strategy_path(bad),
                Err(BuildError::InvalidPath { path: bad.into() }),
                "{bad:?}"
            );
        }
    }

    /// A rule file's header, with a `[run]` table.
    const RULE_HEADER: &str = "//! ```toml\n//! [strategy]\n//!\n//! [run]\n//! bound_exits = true\n\
                               //! fills_a_day = 3\n//! ```\n";

    /// The entry is read off the text by its column-0 `pub fn`: `rule` alone is the order
    /// language, both is refused, an indented method of the same name is no entry, and a file
    /// exporting neither is a `build` file (rustc then names the missing `build`).
    #[test]
    fn the_entry_is_the_top_level_pub_fn_the_file_exports() {
        let build = "pub fn build<B: vike_model::OrderBroker + 'static>(p: &P) {}\n";
        let rule = format!("{RULE_HEADER}pub fn rule(_params: &vike_model::params::Params) {{}}\n");
        assert_eq!(Entry::of(build), Ok(Entry::Build));
        let run =
            StrategyFileHeaderRun { bound_exits: true, fills_a_day: Some(3), ..Default::default() };
        assert_eq!(Entry::of(&rule), Ok(Entry::Rule(run)));
        let headerless = "pub fn rule(_p: &P) {}\n";
        assert!(
            matches!(Entry::of(headerless), Err(BuildError::Header { .. })),
            "a rule needs one"
        );
        let bare = "//! ```toml\n//! [strategy]\n//! ```\npub fn rule(_p: &P) {}\n";
        assert_eq!(Entry::of(bare), Ok(Entry::Rule(StrategyFileHeaderRun::default())), "no [run]");
        assert_eq!(Entry::of(&format!("{build}{rule}")), Err(BuildError::TwoEntries));
        assert_eq!(Entry::of("impl S {\n    pub fn rule(&self) {}\n}\n"), Ok(Entry::Build));
        assert_eq!(Entry::of("/// pub fn rule(\npub fn ruler() {}\n"), Ok(Entry::Build));
        assert_eq!(Entry::of(""), Ok(Entry::Build));
    }

    /// The rendered `lib.rs` keeps no placeholder, never holds the file's text (the file is its module
    /// `strategy`), and calls in that module the entry the file exports and its optional
    /// `refuse`/`echo` only when it has them.
    #[test]
    fn the_rendered_lib_rs_calls_what_the_file_exports_in_its_module() {
        let template = checkout_templates().strategy_lib_rs;
        let text = format!(
            "{RULE_HEADER}// {{{{ENTRY}}}} in a comment\npub fn rule(_p: &P) {{}}\n\
             pub fn echo(_p: &P) {{}}\n"
        );
        let text = text.as_str();
        let lib = render_lib_rs(&template, text, Entry::of(text).unwrap());
        assert!(!lib.contains("in a comment"), "the text is the module's, never spliced");
        assert!(
            lib.contains(
                "
mod strategy;
"
            ),
            "{lib}"
        );
        assert!(
            lib.contains(
                "RuleRunOptions { bound_exits: true, stacked_closes: false, fills_a_day: Some(3), \
                 touch_at_market: false }"
            ),
            "the header's [run] is the entry's: {lib}"
        );
        assert!(!lib.contains("{{"), "no placeholder may survive");
        assert!(lib.contains("Run::new(strategy::rule(&params), &facts.symbol)"), "{lib}");
        assert!(
            lib.contains("echo_entry(strategy::echo, params, facts)")
                && !lib.contains("refuse(params)"),
            "{lib}"
        );

        let text = "pub fn build<B>(p: &P) {}\npub fn refuse(_p: &P) {}\n";
        let lib = render_lib_rs(&template, text, Entry::of(text).unwrap());
        assert!(!lib.contains("{{"), "no placeholder may survive");
        assert!(
            lib.contains("build_entry(strategy::build::<::vike_rust_strategy::guest::HostBroker>"),
            "{lib}"
        );
        assert!(lib.contains("strategy::refuse(params)") && !lib.contains("strategy::echo"));
    }

    /// The seed keeps exactly the packages `Cargo.lock` names with a `source`, once each, never one
    /// whose name a `path` package (no `source`) also carries.
    #[test]
    fn the_seed_keeps_only_sourced_packages_no_path_package_shares_a_name_with() {
        let lock = "version = 4\n\n\
             [[package]]\nname = \"itoa\"\nversion = \"1.0.0\"\nsource = \"registry+https://x\"\n\n\
             [[package]]\nname = \"itoa\"\nversion = \"0.4.8\"\nsource = \"registry+https://x\"\n\n\
             [[package]]\nname = \"vike-model\"\nversion = \"0.1.0\"\n\n\
             [[package]]\nname = \"twin\"\nversion = \"1.0.0\"\nsource = \"registry+https://x\"\n\n\
             [[package]]\nname = \"twin\"\nversion = \"2.0.0\"\n";
        assert_eq!(third_party_packages(lock).into_iter().collect::<Vec<_>>(), ["itoa"]);
    }

    /// `publish` replaces whatever entry stands at the destination, a symlink included, without
    /// writing through it, and leaves no temporary file behind.
    #[cfg(unix)]
    #[test]
    fn publishing_replaces_a_planted_symlink_rather_than_writing_through_it() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"untouched").unwrap();
        let dst = dir.path().join("abc.so");
        std::os::unix::fs::symlink(&victim, &dst).unwrap();
        publish(&dst, "tag", |f| f.write_all(b"artifact")).expect("publish");
        assert_eq!(std::fs::read(&victim).unwrap(), b"untouched");
        assert!(!std::fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(&dst).unwrap(), b"artifact");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2, "no temporary left");
    }
}

#[path = "render_sdk_tests.rs"]
#[cfg(test)]
mod render_sdk_tests;
