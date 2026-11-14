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
//!    to a scratch directory OUTSIDE this checkout (`std::env::temp_dir()`), so `cargo build
//!    --manifest-path <scratch>/Cargo.toml` treats it as its own single-crate implicit
//!    workspace. A `path = "…"` dependency reaching back INTO this checkout (`vike-model`,
//!    `vike-rust-strategy`) does not drag the scratch crate into the REAL workspace —
//!    Cargo resolves each path dependency's `{ workspace = true }` manifest fields against ITS
//!    OWN enclosing workspace (found by walking up from ITS manifest), not against the crate that
//!    is building it. This is what makes "a sibling crate's `Cargo.toml.in`, rendered outside the
//!    workspace, importing straight from the real crates" work at all without a second
//!    `[workspace]` declaration or an `exclude` entry anywhere.
//! 2. **The template is `.in`, never `.rs`, for two independent reasons, and it lives in
//!    `vike-rust-strategy`'s tree rather than this crate's own — reached below through
//!    `vike_rust_strategy::{TEMPLATE_CARGO_TOML, TEMPLATE_LIB_RS}`, an ordinary Cargo dependency
//!    edge, not a bare `include_str!` file path crossing the crate boundary** (a first cut of this
//!    split did reach them that way, with `include_str!("../../vike-rust-strategy/template/…")`
//!    — a coupling invisible to `cargo tree`/`layer_gate.rs` and only caught by a rename). The
//!    obvious reason the files stay `.in`: an `{{ENTRY}}`/`{{NAME}}` placeholder is not valid
//!    Rust and a workspace member holding one would fail every lane that touches that crate. The
//!    one that is easy to miss: even a *syntactically valid* `template/lib.rs` sitting under
//!    `vike-rust-strategy`'s root would still not be a workspace member (`template/` is not
//!    `src/`), so the `.in` extension is about keeping
//!    `crates/vike-ops/tests/hygiene/unsafe_and_toolchain_gate.rs`'s file walk from ever visiting it too —
//!    that gate's FULL-TREE scan of `vike-rust-strategy` (an `UNSAFE_EXEMPT` crate) covers its
//!    `src/`, `build.rs`, `tests/`/`benches`/`examples`, but `template/` is none of those, and
//!    `.in` is not `.rs` either, so this file is invisible to it on BOTH counts — a property the
//!    dependency-edge fix above leaves unchanged, since `include_str!("../template/…")` still
//!    resolves relative to `vike-rust-strategy/src/lib.rs`'s own location.
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
//! ⚠ **The scratch directory stays under `std::env::temp_dir()`, and a prior draft of this fix
//! round moved it to `<project>/tmp` before reverting — the revert is deliberate, and the reasons
//! are why the row below is a GRANDFATHER rather than a repair.** `<project>/tmp` looked like the
//! same class of fix as `workspace_root` above, but the deployed unit makes it wrong in two ways
//! neither this file nor that draft asked: `deploy/vike-strategy-builder.service`'s
//! `ReadWritePaths=` grants exactly `<root>/bin/rust-toolchain/cargo-home` and
//! `<root>/user_data/strategy-builds` under `ProtectSystem=strict` — and since 2026-09-26 its filesystem
//! jail does not even MOUNT the rest of the project root into the unit's namespace — so `<root>/tmp`
//! is unreachable in the unit's own sandbox and every `build_strategy_file` call would fail with an I/O
//! error the moment it tried to create a directory there; and there is still no OWNERSHIP for the
//! whole scratch tree beyond its age sweep: every build deliberately keeps its build folder's
//! shared `target/` for the next file (`build_strategy_file`).
//! (Since 2026-10-12 every build also removes the entries nothing has built into for
//! [`SCRATCH_MAX_AGE`] — [`prune_stale_scratch`] — which bounds the count BY AGE; before that
//! nothing bounded how many accumulated, and the latency box held 104 GB of them.) Moving that same leak
//! from a directory something else empties (`PrivateTmp=true`'s temp dir, cleared on restart) to a directory nothing empties
//! (a mounted, persistent project volume) is not a fix for
//! `crates/vike-ops/tests/hygiene/system_temp_gate.rs` — it reproduces the exact 215 GB the CI box incident that
//! gate's own doc measures, just on a volume an operator now has to notice and clear by hand. A
//! real cure needs BOTH a startup sweep (`vike_paths::scratch::sweep`, called once by `main`) AND
//! `<root>/tmp` added to the deploy unit's `ReadWritePaths=` and `BindPaths=` — a deploy-affecting
//! change earning
//! its own PR and its own review, not a corner of this fix round. See `SYSTEM_TEMP_PIN` in
//! `crates/vike-ops/tests/hygiene/system_temp_gate.rs` for the grandfather row this leaves behind and the
//! reopening condition written there.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use vike_hub_protocol::hash::sha256_hex;
use vike_rust_strategy::host::UNWIRED_HOOKS;
use vike_rust_strategy::{TEMPLATE_CARGO_TOML, TEMPLATE_LIB_RS};
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

/// Why [`build_strategy_file`] failed to produce an artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// `cargo` ran and rustc reported a failure. Carries rustc's OWN diagnostic text VERBATIM
    /// (`cargo build`'s captured stderr), never a summary — a generic "build failed" makes the
    /// user guess at their own syntax error. This is the ONE variant a user's OWN mistake can
    /// produce; the other two are host/environment problems. When the source still reaches a
    /// market-data name through `vike_model`, one `hint:` line FOLLOWS rustc's text, which is
    /// otherwise untouched (`compile_diagnostics`).
    Compile(String),
    /// A filesystem operation failed — writing the rendered source, creating the scratch crate
    /// directory, or moving the finished artifact into its build folder.
    Io(String),
    /// `cargo` itself could not be found or could not be started — a toolchain problem rather
    /// than a problem with the user's source (contrast [`BuildError::Compile`], where `cargo` DID
    /// run and rustc reported a failure).
    Toolchain(String),
    /// The source's `Strategy` impl overrides one or more seam methods the strategy file vtable does not
    /// carry (`vike_rust_strategy::host::UNWIRED_HOOKS`). Carries every offending hook name.
    ///
    /// ⚠ **The only reason this is an ERROR rather than a note.** Such a source compiles, links,
    /// loads and handshakes perfectly, and then never receives the hook — so the SAME file
    /// produces different, entirely plausible numbers depending on which mechanism the host used
    /// to reach it, with no signal at any layer. That is the failure the design's whole "one
    /// folder, the host decides the mechanism" verdict rests on not happening. Refusing here puts
    /// the signal in front of the author while they are standing at the editor, which is the only
    /// place it can be acted on cheaply.
    UnwiredHook(Vec<String>),
    /// [`verify_source_version`]'s refusal — the `workspace_root`'s version stamp (absent, or a
    /// different commit) could not be POSITIVELY CONFIRMED to match this running binary's own
    /// `vike_buildinfo::GIT_SHA`. Carries the fully-composed message, both values named, in the
    /// same shape the three variants above already use.
    ///
    /// ⚠ **The only variant this crate refuses to SOFTEN — never downgraded to a warning and a
    /// build.** `abi_version` and the toolchain fingerprint catch a deliberate vtable bump or a
    /// different rustc/target/profile — SHAPE drift. Neither catches a stale `workspace_root` whose
    /// `vike-model`/`vike-strategy`/`vike-indicators` LOGIC changed release-to-release with no ABI
    /// bump at all: that skew compiles clean, loads clean, and produces DIFFERENT NUMBERS with
    /// nothing loud anywhere. A loud refusal here is the cure — and "loud" means the JOURNAL as
    /// well as the peer: the service says every refused `Build` on stderr
    /// (`crates/vike-strategy-builder/src/builder.rs`'s `build_refusal_line`), because a refusal
    /// only the caller sees left a daemon that refused every request reading exactly like an idle
    /// one. See [`verify_source_version`] for how the tree can still drift now that a deploy
    /// refreshes it.
    SourceVersionMismatch(String),
    /// A strategy file path that is not a plain relative `.rs` path under `strategies/`
    /// (`check_strategy_path`): absolute, climbing out with `..`, or carrying a `\`, a `:` or a
    /// control character. The path is a wire peer's string, so it is refused, never rewritten. (The
    /// artifact's own name is the text's sha, so no peer string reaches a path any more; the old
    /// `<name>-<sha>.so` shape is where a planted symlink once overwrote the cargo home's config.)
    InvalidPath(String),
    /// The file exports both entries, `build` and `rule` (`Entry`); a strategy file exports one.
    TwoEntries,
    /// A `rule` file whose header does not hold (`vike_strategy::file::StrategyFileHeader::parse`'s
    /// one-line reason): its `[run]` options are rendered into its entry, so it is not built without
    /// them.
    Header(String),
    /// [`CargoHomePin::check`] found `$CARGO_HOME/config.toml` no longer matches what this daemon
    /// captured at startup. Carries only PRESENT/ABSENT for each side, never a hash or the file's
    /// content — a hash is still enough entropy about attacker-controlled bytes to be worth
    /// withholding, and "present vs. absent" is all an operator needs to act on this.
    CargoHomeTampered { was_present: bool, now_present: bool },
    /// This box cannot jail a build (`vike_sandbox::probe`, `crate::jail::grants`), so nothing is
    /// built — never an unjailed build instead (decision 0273, no override). Carries the reason.
    Unsandboxed(String),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Compile(msg) => write!(f, "compile failed:\n{msg}"),
            BuildError::Io(msg) => write!(f, "i/o error: {msg}"),
            BuildError::Toolchain(msg) => write!(f, "toolchain error: {msg}"),
            BuildError::SourceVersionMismatch(msg) => write!(f, "source version mismatch: {msg}"),
            BuildError::InvalidPath(path) => write!(
                f,
                "invalid strategy file path {path:?}: a strategy file is named by its path under \
                 the project's `strategies/` folder, relative and ending in `.rs` \
                 (`trend/momentum.rs`), with no `..`, no `\\` and no `:`"
            ),
            BuildError::TwoEntries => write!(
                f,
                "this file exports both `pub fn build` and `pub fn rule`: a strategy file exports \
                 one entry — `build` for a strategy, `rule` for the order language"
            ),
            BuildError::Header(why) => write!(
                f,
                "this `rule` file's header does not hold: {why}. The header is the first ```toml \
                 block in the file's leading comment lines, and its `[run]` table says how the rule \
                 trades"
            ),
            BuildError::CargoHomeTampered { was_present, now_present } => write!(
                f,
                "$CARGO_HOME/config.toml changed since this daemon started (was {}, is now {}) — \
                 refusing to build against a cargo configuration nobody here approved. Every \
                 caller shares this file; a Build's own build.rs can write it with no traversal \
                 needed. Restart the daemon to re-pin the current file, after confirming its \
                 content is the operator's own.",
                if *was_present { "present" } else { "absent" },
                if *now_present { "present" } else { "absent" }
            ),
            BuildError::Unsandboxed(why) => write!(
                f,
                "this box cannot build a Rust strategy safely, so it builds none: {why}. A build \
                 here runs only inside a jail that keeps it from the settings database, credential \
                 files and the network, and that needs Linux 6.7 or later. Rhai strategies and the \
                 built-in ones still run."
            ),
            // A user reads this when a Build is refused (Studio shows it): plain words, no
            // internal name of the mechanism. How a hook gets wired is `UNWIRED_HOOKS`' doc.
            BuildError::UnwiredHook(hooks) => write!(
                f,
                "this strategy overrides {} — a `Strategy` hook a Rust strategy built in Studio \
                 is never sent. It would build and run cleanly and simply never receive that \
                 hook, so the same file would trade differently there than compiled into the \
                 program, \
                 with no error and plausible numbers. Two ways forward: drop the override, or \
                 put the file in the build-time tier (`user_data/strategies/rust/<name>/`, \
                 compiled into the program), which honours every hook.",
                hooks.iter().map(|h| format!("`{h}`")).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

impl std::error::Error for BuildError {}

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

/// The directory under `std::env::temp_dir()` that holds every content-addressed scratch crate
/// `build_strategy_file` mints. One name, so the builder and [`prune_stale_scratch`] cannot disagree.
const SCRATCH_ROOT_DIR: &str = "vike-rust-strategy-build";

/// How long a scratch crate may go UNTOUCHED before [`prune_stale_scratch`] removes it: two days,
/// the retention the the CI box and the latency box CI scratch is operated at. Each is ONE build folder's shared
/// cargo `target/` (a per-source one measured 2.9–4.8 GB on the latency box), so every file built inside the
/// window reuses the compiled dependency closure — the reason the directory outlives its build.
pub const SCRATCH_MAX_AGE: Duration = Duration::from_secs(2 * 24 * 60 * 60);

/// Remove every scratch crate under `root` that nothing has built into for longer than `max_age`,
/// except `keep`; returns how many it removed.
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
fn prune_stale_scratch(root: &Path, keep: &Path, max_age: Duration, now: SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(root) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep || !path.is_dir() {
            continue;
        }
        let stamp = fs::metadata(path.join("Cargo.toml")).or_else(|_| fs::metadata(&path));
        let Ok(modified) = stamp.and_then(|m| m.modified()) else { continue };
        // A stamp in the future (clock step) is "fresh", not an error.
        let stale = now.duration_since(modified).is_ok_and(|age| age > max_age);
        if stale && fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The file [`verify_source_version`] reads at the ROOT of `workspace_root` — never a name a
/// caller chooses, always this one, so the release packaging step
/// (`.github/workflows/release.yml`'s "Package the strategy builder's workspace source" step) and
/// this reader can never spell it two ways. Not derivable across a YAML step and a Rust constant,
/// so the two sides simply have to agree — the same shape `deploy_layout_gate.rs`'s `STRATEGY_FILES_RELATIVE`/
/// `STORE_REL` literals already carry for a fact duplicated across languages;
/// `crates/vike-strategy-builder/tests/build_errors.rs`'s
/// `the_release_workflow_stamps_the_spelling_this_crate_reads` pins the two together.
pub const SOURCE_VERSION_STAMP_FILE: &str = "SOURCE_GIT_SHA";

/// Refuse to build against a `workspace_root` whose own version is PRESENT and does not match this
/// binary's — the cure for a gap the toolchain fingerprint and `abi_version` leave wide open.
///
/// **What those two guards catch, and what they do not.** `vike_rust_strategy::fingerprint::
/// FINGERPRINT` and `abi::ABI_VERSION` catch SHAPE drift: a deliberate vtable bump, a different
/// rustc, target triple or profile. Neither says anything about WHETHER the `vike-model`/
/// `vike-strategy`/`vike-indicators` source under `workspace_root` is the same commit the running
/// binary's own build-time tier was compiled from. A stale `workspace_root` whose LOGIC changed
/// release-to-release — a fee formula, a signal threshold, a rounding rule — compiles clean under
/// an unchanged ABI, loads clean, handshakes clean, and produces DIFFERENT NUMBERS than the
/// build-time tier would for the identical user file, with nothing loud anywhere. That is the
/// exact "same file, two mechanisms, different answer" failure this whole design exists to close,
/// reopened by the one asset this crate's OWN cache and the loader's OWN checks cannot see.
///
/// **How the two can still drift, now that a deploy moves both.** The BINARY is refreshed on every
/// deploy (`deploy/sbin/vike-trader-ci-deploy`'s `DEPLOY_UNITS`), and so — since this check turned
/// out to refuse EVERY build after every release — is the extracted SOURCE TREE: the helper's
/// `RUNTIME_TOOLS` carries it as a directory row, staged beside the live tree and swapped in only
/// inside a window where this daemon is STOPPED (that file's `swap_tree`), because a live `cargo`
/// invocation may be reading it mid-build. Before that, the tree was deliberately left to an
/// operator, and on the first box to run this check that meant a builder refusing every request
/// from the moment a release landed. What can still put the two out of step, and what this check
/// is therefore still for: a deploy HELPER installed before that row existed (it is installed out
/// of band, by hand, and a stale copy silently skips a row it does not know), a hand-maintained
/// checkout, a hand fetch of the wrong tag, and a hand-driven rollback of the binary alone.
/// Nothing but this function notices any of them.
///
/// **The mechanism**, mirroring the fingerprint's own shape: `.github/workflows/release.yml`
/// stamps the released tarball's root with `git rev-parse --short HEAD` at
/// [`SOURCE_VERSION_STAMP_FILE`] — the SAME command `crates/vike-buildinfo/build.rs` runs to
/// produce [`vike_buildinfo::GIT_SHA`], in the SAME job, against the SAME commit, so the two are
/// byte-identical by construction whenever both genuinely came from one release.
///
/// ⚠ **Fails OPEN on an ABSENT stamp, fails CLOSED on a PRESENT, mismatched one — deliberately two
/// different postures, not an oversight.** An absent stamp is the status quo this function did not
/// change: a hand-maintained checkout (`deploy/vike-strategy-builder.service`'s header still names
/// one as sanctioned) is not what the deploy refreshes — only the release-asset tree is — so
/// nothing refreshes it automatically either way, there is no NEW drift for an absent stamp to
/// introduce, and every
/// existing caller of [`build_strategy_file`] (this crate's own tests, `vike-rust-strategy`'s
/// equivalence/panic/params harnesses, `vike-studio-core`'s `strategy_file_run.rs`) points `workspace_root`
/// at a real dev checkout with no stamp file, proving the mechanism against the real toolchain and
/// the real crates rather than a copy. Refusing THOSE outright would have made this fix cost every
/// one of them a rewrite to fabricate a matching stamp — solving a problem that does not exist in a
/// dev checkout to close one that only exists in the auto-refreshed release-asset shape. A PRESENT
/// stamp is a different claim: somebody (the release pipeline, or an operator who chose to write
/// one by hand) asserted a specific commit, and a mismatch against that assertion is exactly the
/// silent-drift failure this function exists to catch — so that case, and that case only, refuses
/// loudly, both values named. An `UNKNOWN` binary provenance (built with no git available, e.g.
/// from a source tarball) against a PRESENT stamp also refuses: a stamped tree asserted a specific
/// commit and this binary cannot honestly say whether it matches, which is not a "cannot tell,
/// proceed" case the same way total absence is — nobody asserted anything for a shrug to default
/// past, but somebody asserted THIS.
fn verify_source_version(workspace_root: &Path) -> Result<(), BuildError> {
    let stamp_path = workspace_root.join(SOURCE_VERSION_STAMP_FILE);
    let stamp = match fs::read_to_string(&stamp_path) {
        Ok(s) => s,
        // No stamp at all: unverifiable, and — per this function's own doc — that is the STATUS
        // QUO for every workspace_root nothing auto-refreshes, not a new hole this function opens.
        // ⚠ SAID, not silent: a guard that declines to check and tells nobody is the same defect
        // this crate already removed `tracing` for (see this crate's `Cargo.toml` and
        // `builder.rs`'s module doc) — an operator watching the journal should be able to tell
        // "no skew check ran" from "a skew check ran and passed", and a bare `Ok(())` here made
        // both look identical.
        Err(_) => {
            eprintln!(
                "vike-strategy-builder: no version stamp at {} — proceeding without a \
                 source-version skew check (see render::verify_source_version's own doc for why \
                 an absent stamp is not a refusal).",
                stamp_path.display()
            );
            return Ok(());
        }
    };
    let stamp = stamp.trim();
    if vike_buildinfo::GIT_SHA == vike_buildinfo::UNKNOWN {
        return Err(BuildError::SourceVersionMismatch(format!(
            "the workspace_root at {} carries a version stamp (`{stamp}`), but this binary's own \
             git commit is UNKNOWN (built with no git available, e.g. from a source tarball), so \
             it cannot honestly confirm or deny a match. Refusing rather than trusting a stamped \
             claim this binary cannot verify.",
            workspace_root.display()
        )));
    }
    if stamp != vike_buildinfo::GIT_SHA {
        return Err(BuildError::SourceVersionMismatch(format!(
            "this binary was built from git commit `{}`, but the workspace_root at {} is stamped \
             `{stamp}` — a DIFFERENT commit. Building against it would link \
             vike-model/vike-strategy/vike-indicators logic the running binary's own build-time \
             tier may disagree with, with no ABI change to catch it: same load, same handshake, \
             different numbers. A deploy refreshes this tree together with the binary; if one \
             just ran and this still fires, the deploy helper installed on the box predates that \
             and must be reinstalled. By hand: re-fetch the matching release's strategy-source \
             asset (`scripts/fetch_release_tools.sh strategy-source --tag <this release>`), or \
             update the hand-maintained checkout to this exact commit.",
            vike_buildinfo::GIT_SHA,
            stamp_path.display()
        )));
    }
    Ok(())
}

/// The package name every rendered strategy file builds under. ONE package in ONE build directory per
/// build folder ([`build_strategy_file`]), so a new file rebuilds only its own crate and its link:
/// the dependency closure stays compiled in the shared target. Two artifacts built under this one
/// name never meet: a host opens each `dlopen`-local, and the sandbox opens one.
const CRATE_NAME: &str = "strategy_file";

/// Build (or reuse the build of) the strategy FILE whose text is `text` and whose path under the
/// project's `strategies/` folder is `path` (`trend/momentum.rs`), and return its artifact:
/// `<builds_dir>/<folder>/<sha256 of text>.so`, the folder being
/// `vike_rust_strategy::artifact::build_folder` of `profile` and this binary's
/// `vike_buildinfo::GIT_SHA`. The caller reads the file and hands its text over; this function reads
/// no `strategies/` folder (plan 2b, section 3). `path` names the file in refusals only, and is
/// refused when it is not a plain relative `.rs` path ([`BuildError::InvalidPath`]).
///
/// `workspace_root` is the source tree the rendered crate's `path` dependencies resolve against and
/// `cargo_bin` the `cargo` to run — caller-supplied for the reasons this module's doc gives.
/// `child_env` is the WHOLE environment `cargo` (and every build script it runs) receives; build it
/// with [`child_env`].
///
/// **The cache.** A text hashing to an artifact already in its folder is served without `cargo`,
/// after the artifact is opened and its handshake checked against this host (a mismatch, or a file
/// that will not even `dlopen`, is REBUILT rather than served). A text whose last build failed IN
/// ITS OWN CRATE is answered from `<sha256>.err` beside it, rustc's words and the hints as they were:
/// that failure is the text's, and the same text under the same folder fails the same way. A failure
/// of anything else (a crate missing from the cargo home, the toolchain, the jail) is never recorded,
/// so it is retried.
///
/// **The shared build directory and the queue.** Every build of one folder from one
/// `workspace_root` uses ONE scratch crate, `<scratch root>/<folder>-<root tag>/`, whose `target/`
/// keeps the dependency closure compiled between files, and the per-build jail's write grant is that
/// directory alone (so a build never writes the artifacts beside it). Builds into it run one at a
/// time, across threads and processes: each holds an exclusive lock on its `build.lock` from
/// writing `lib.rs` until its artifact is published, and looks at the cache again once it holds it.
/// The age sweep ([`prune_stale_scratch`]) retires the directory of a folder no build used for
/// [`SCRATCH_MAX_AGE`].
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
        return Err(BuildError::UnwiredHook(unwired));
    }
    let entry = Entry::of(text)?;

    let sha = sha256_hex(text.as_bytes());
    let folder = build_folder(profile);
    let artifact = vike_rust_strategy::artifact::artifact_path(builds_dir, &folder, &sha);
    let failure = artifact.with_extension("err");
    if let Some(hit) = cached(&artifact, &failure, workspace_root, cargo_bin, child_env)? {
        return Ok(hit);
    }

    // The version-skew guard (`verify_source_version`'s own doc carries the whole argument) runs
    // HERE — after the cache, not before it: a hit serves what was already built and reads nothing
    // from `workspace_root`. Still ahead of every write and of `cargo`, which is what a GENUINE
    // (re)build needs. The cargo-home pin likewise (`CargoHomePin`'s doc).
    verify_source_version(workspace_root)?;
    if let Err((was, now)) = cargo_home_pin.check() {
        return Err(BuildError::CargoHomeTampered {
            was_present: was.is_some(),
            now_present: now.is_some(),
        });
    }

    let folder_dir = builds_dir.join(&folder);
    fs::create_dir_all(&folder_dir)
        .map_err(|e| BuildError::Io(format!("creating {}: {e}", folder_dir.display())))?;

    // ⚠ The build directory's name carries the WORKSPACE ROOT as well as the folder: the scratch
    // root is one directory per BOX, while the rendered `Cargo.toml`'s `path` dependencies point
    // into `workspace_root`, so two checkouts building into one directory would re-point each
    // other's dependencies (this workspace's CI runs several runner instances on one box). Hashed
    // because a path is not a directory-name-safe string.
    let root_tag = &sha256_hex(workspace_root.display().to_string().as_bytes())[..12];
    let build_dir = scratch_root().join(format!("{folder}-{root_tag}"));
    let source_dir = build_dir.join("src");
    fs::create_dir_all(&source_dir)
        .map_err(|e| BuildError::Io(format!("creating {}: {e}", source_dir.display())))?;
    let lock_path = build_dir.join("build.lock");
    let lock = fs::File::create(&lock_path)
        .and_then(|f| f.lock().map(|()| f))
        .map_err(|e| BuildError::Io(format!("locking {}: {e}", lock_path.display())))?;
    // Another holder of the lock may have built this very text while this one waited.
    if let Some(hit) = cached(&artifact, &failure, workspace_root, cargo_bin, child_env)? {
        return Ok(hit);
    }

    // Re-written on every build: the stamp `prune_stale_scratch` ages a build directory by.
    fs::write(build_dir.join("Cargo.toml"), render_cargo_toml(CRATE_NAME, workspace_root))
        .map_err(|e| BuildError::Io(format!("writing Cargo.toml: {e}")))?;
    prune_stale_scratch(&scratch_root(), &build_dir, SCRATCH_MAX_AGE, SystemTime::now());
    fs::write(source_dir.join("lib.rs"), render_lib_rs(text, entry))
        .and_then(|()| fs::write(source_dir.join("strategy.rs"), text))
        .map_err(|e| BuildError::Io(format!("writing the scratch crate's source: {e}")))?;

    let output = cargo_build_jailed(&build_dir, workspace_root, cargo_bin, child_env, profile)?;
    if !output.status.success() {
        // rustc's own words, verbatim — never re-summarized; the hints, when the source draws one,
        // come AFTER them (`compile_diagnostics`).
        let diagnostics = compile_diagnostics(&output.stderr, text);
        let own_crate = format!("could not compile `{CRATE_NAME}`");
        if String::from_utf8_lossy(&output.stderr).contains(&own_crate) {
            publish(&failure, root_tag, |dst| dst.write_all(diagnostics.as_bytes()))?;
        }
        return Err(BuildError::Compile(diagnostics));
    }

    let built_dir = build_dir.join("target").join(profile.target_subdir());
    let built = find_cdylib(&built_dir, CRATE_NAME).ok_or_else(|| {
        BuildError::Io(format!(
            "cargo reported success but no cdylib for `{CRATE_NAME}` was found under {}",
            built_dir.display()
        ))
    })?;
    let mut from = fs::File::open(&built)
        .map_err(|e| BuildError::Io(format!("reading {}: {e}", built.display())))?;
    publish(&artifact, root_tag, |dst| io::copy(&mut from, dst).map(drop))?;
    drop(lock);
    Ok(artifact)
}

/// The build folder this binary builds `profile` artifacts into
/// (`vike_rust_strategy::artifact::build_folder` over this binary's `vike_buildinfo::GIT_SHA`).
pub fn build_folder(profile: Profile) -> String {
    vike_rust_strategy::artifact::build_folder(profile.target_subdir(), vike_buildinfo::GIT_SHA)
}

/// What a build of this text already answered in its folder: `Ok(Some(artifact))` for a current
/// artifact, `Err(Compile)` for a recorded failure of its own crate, `Ok(None)` for neither.
///
/// ⚠ **A hit is VERIFIED, not just found.** The artifact is opened and its two handshake exports
/// are checked against this host's `ABI_VERSION` and toolchain fingerprint
/// (`vike_rust_strategy::loader::verify_handshake`, one `dlopen` and two `dlsym`s, nothing close to a
/// `cargo` run); a mismatch, or a file that will not `dlopen`, falls through to a REBUILD, said on
/// stderr, never an error: the cache existing must never make a request fail that a cold one
/// would have satisfied.
///
/// ⚠ **The one path by which a caller's code runs INSIDE this process rather than under cargo.**
/// `dlopen` runs the artifact's initializers here, with this process's memory. In the deployed
/// daemon that is inside the build jail (`crate::builder::run` jailed the serving thread before it
/// served a connection); on a box that CANNOT jail, a hit is refused the way a fresh build is,
/// rather than `dlopen`ed unjailed.
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
            .map_err(BuildError::Unsandboxed)?;
        match vike_rust_strategy::loader::verify_handshake(artifact) {
            Ok(()) => return Ok(Some(artifact.to_path_buf())),
            Err(mismatch) => eprintln!(
                "vike-strategy-builder: cached artifact at {} no longer matches this host \
                 ({mismatch}) — rebuilding rather than serving it",
                artifact.display()
            ),
        }
    }
    match fs::read_to_string(failure) {
        Ok(diagnostics) => Err(BuildError::Compile(diagnostics)),
        Err(_) => Ok(None),
    }
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
        BuildError::Io(format!("writing {}: {e}", dst.display()))
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
    if plain { Ok(()) } else { Err(BuildError::InvalidPath(path.to_string())) }
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
                .map_err(BuildError::Header),
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

/// The template's `lib.rs` for the file `text`: the entry it exports and its optional `refuse` and
/// `echo`, each called in the module `strategy`, which is the file itself (`src/strategy.rs`,
/// written beside it by [`build_strategy_file`], VERBATIM).
fn render_lib_rs(text: &str, entry: Entry) -> String {
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
    let echo = if exports(text, "echo") { "Some(strategy::echo(params))" } else { none };
    TEMPLATE_LIB_RS
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
    vike_sandbox::probe().map_err(BuildError::Unsandboxed)?;
    let grants = crate::jail::grants(workspace_root, &[scratch_dir], cargo_bin, child_env)
        .map_err(BuildError::Unsandboxed)?;
    let tmp = scratch_dir.join("tmp");
    fs::create_dir_all(&tmp)
        .map_err(|e| BuildError::Io(format!("creating {}: {e}", tmp.display())))?;
    let run = || {
        let mut cmd = Command::new(cargo_bin);
        // ⚠ `env_clear()` FIRST, then only the allowlist. This process's environment carries this
        // service's own key (or the path to it), `CREDENTIALS_DIRECTORY`, and anything an operator
        // put in the unit's `.env`; `cargo` would hand all of it to a caller's `build.rs`.
        // `crates/vike-strategy-builder/tests/child_env.rs` proves it against the real daemon — a
        // planted variable and the key both stay out of the child — and deleting this call is the
        // mutation it goes red on.
        cmd.env_clear().envs(child_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd.args(["build", "--offline"]);
        if let Some(flag) = profile.cargo_flag() {
            cmd.arg(flag);
        }
        cmd.arg("--manifest-path")
            .arg(scratch_dir.join("Cargo.toml"))
            .env("CARGO_TARGET_DIR", scratch_dir.join("target"))
            .env("TMPDIR", &tmp)
            // ⚠ FORCE OFF any `rustc-wrapper` a `config.toml` sets (`sccache` on a dev or CI box):
            // a wrapper runs AS the compiler and sccache talks to its server over a SOCKET, which
            // the jail refuses — so a wrapper here breaks the build outright, not merely "no cache".
            // An empty `RUSTC_WRAPPER` disables it and OVERRIDES the config file, which `env_clear`
            // alone cannot (the wrapper is a config setting, not an inherited variable). The
            // deployed cargo home sets none; this makes the jail hold on a box that does.
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
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
    vike_sandbox::run_jailed(&grants, run)
        .map_err(BuildError::Unsandboxed)?
        .map_err(|e| BuildError::Toolchain(format!("could not run `{cargo_bin} build`: {e}")))
}

/// Fill the cargo home with every crate the cdylib template can reach — `cargo fetch` over the
/// template rendered around an empty source — so a jailed build, which has no network, finds them
/// (`cargo_build_jailed`'s `--offline`). The builder runs it ONCE, at start, before it jails itself
/// (`crate::builder::run`): cargo's own code over the FIXED template manifest, so no source a
/// caller sent is read and no build script runs. It still runs jailed, with the network allowed:
/// it reads and writes only what a build does.
///
/// # Errors
/// cargo's own words when the fetch fails; the builder says them and goes on (a warm cargo home
/// still builds).
pub fn prefetch(
    workspace_root: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
) -> Result<(), String> {
    let root_hash = sha256_hex(workspace_root.display().to_string().as_bytes());
    let dir = scratch_root().join(format!("prefetch-{}", &root_hash[..12]));
    let src = dir.join("src");
    fs::create_dir_all(&src).map_err(|e| format!("creating {}: {e}", src.display()))?;
    fs::write(dir.join("Cargo.toml"), render_cargo_toml("prefetch", workspace_root))
        .and_then(|()| fs::write(src.join("lib.rs"), ""))
        .map_err(|e| format!("writing the template under {}: {e}", dir.display()))?;
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

/// The directory every scratch crate (and the prefetch's) lives under: one per box, in the
/// temporary directory. The builder creates it before it jails itself, so its jail can name it.
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
/// Empty for every source while `UNWIRED_HOOKS` is (since `ABI_VERSION` 8 every hook crosses);
/// [`hook_overrides`] is the scan itself, over any list.
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
fn render_cargo_toml(crate_name: &str, workspace_root: &Path) -> String {
    let crate_path = |name: &str| workspace_root.join("crates").join(name).display().to_string();
    // ⚠ `VIKE_RUST_STRATEGY_PATH` is substituted BEFORE `VIKE_STRATEGY_PATH`, and the order is
    // load-bearing rather than tidy: `{{VIKE_STRATEGY_PATH}}` is not a substring of
    // `{{VIKE_RUST_STRATEGY_PATH}}` (`RUST_` breaks the common prefix), so either order is correct
    // today — but a future placeholder that IS a prefix of another would corrupt the longer one
    // silently, and `render_cargo_toml_leaves_no_placeholder_behind` could not see it because no
    // `{{` would survive. Longest-first is the rule that stays correct without a test noticing, so
    // the lines below run in order of placeholder length, a tie in either order.
    TEMPLATE_CARGO_TOML
        .replace("{{VIKE_RUST_STRATEGY_PATH}}", &crate_path("vike-rust-strategy"))
        .replace("{{VIKE_INDICATORS_PATH}}", &crate_path("vike-indicators"))
        .replace("{{VIKE_MARKETDATA_PATH}}", &crate_path("vike-marketdata"))
        .replace("{{VIKE_STRATEGY_PATH}}", &crate_path("vike-strategy"))
        .replace("{{VIKE_TACTICS_PATH}}", &crate_path("vike-tactics"))
        .replace("{{VIKE_VENUES_PATH}}", &crate_path("vike-venues"))
        .replace("{{VIKE_MODEL_PATH}}", &crate_path("vike-model"))
        .replace("{{VIKE_UTIL_PATH}}", &crate_path("vike-utility"))
        .replace("{{VIKE_MM_PATH}}", &crate_path("vike-market-making"))
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

    /// `env!("CARGO_MANIFEST_DIR")`, used here only — this INLINE `#[cfg(test)]` module is one of
    /// the ~50 sites `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs`'s own doc names as always
    /// correct ("a test resolving a fixture from its own manifest directory… there is no
    /// deployment"), and that gate's `inline_test_module_start`/`cfg_test_module_files` locators
    /// exclude inline test modules from its scan by construction. Two levels up from this crate's
    /// own manifest dir (`crates/vike-strategy-builder`) is the checkout root.
    fn test_workspace_root() -> PathBuf {
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

        assert_eq!(prune_stale_scratch(root.path(), &mine, SCRATCH_MAX_AGE, now), 2);

        assert!(!stale.exists(), "a 3-day-old entry is gone, target/ and all");
        assert!(!bare_stale.exists(), "a manifest-less entry is aged by its directory");
        assert!(fresh.join("target/big.bin").exists(), "a 1-day-old entry keeps its target/");
        assert!(mine.exists(), "the caller's own entry is never pruned");
        assert!(future.exists(), "a future stamp counts as fresh");
        assert!(root.path().join("loose-file").exists(), "only directories are entries");
        assert_eq!(
            prune_stale_scratch(&root.path().join("absent"), &mine, SCRATCH_MAX_AGE, now),
            0
        );
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
        let rendered = render_cargo_toml("some_strategy_file", &test_workspace_root());
        assert!(!rendered.contains("{{"), "unrendered placeholder survived: {rendered}");
        assert!(rendered.contains("some_strategy_file"));
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
        let rendered = render_cargo_toml("some_strategy_file", &root);
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
                Err(BuildError::InvalidPath(bad.into())),
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
        assert!(matches!(Entry::of(headerless), Err(BuildError::Header(_))), "a rule needs one");
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
        let text = format!(
            "{RULE_HEADER}// {{{{ENTRY}}}} in a comment\npub fn rule(_p: &P) {{}}\n\
             pub fn echo(_p: &P) {{}}\n"
        );
        let text = text.as_str();
        let lib = render_lib_rs(text, Entry::of(text).unwrap());
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
        assert!(lib.contains("Some(strategy::echo(params))") && !lib.contains("refuse(params)"));

        let text = "pub fn build<B>(p: &P) {}\npub fn refuse(_p: &P) {}\n";
        let lib = render_lib_rs(text, Entry::of(text).unwrap());
        assert!(!lib.contains("{{"), "no placeholder may survive");
        assert!(
            lib.contains("build_entry(strategy::build::<::vike_rust_strategy::guest::HostBroker>"),
            "{lib}"
        );
        assert!(lib.contains("strategy::refuse(params)") && !lib.contains("echo(params)"));
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
