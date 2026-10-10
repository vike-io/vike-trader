//! `build_plugin`: render the cdylib TEMPLATE (`vike-strategy-plugin`'s `template/*.in`) around a
//! user strategy's source, invoke `cargo`, and return the compiled artifact's path —
//! content-addressed so an unchanged source is never rebuilt.
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
//! as an ordinary parameter (see [`build_plugin`]'s own doc) — the third one out of this file, the
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
//!    `vike-strategy-plugin`) does not drag the scratch crate into the REAL workspace —
//!    Cargo resolves each path dependency's `{ workspace = true }` manifest fields against ITS
//!    OWN enclosing workspace (found by walking up from ITS manifest), not against the crate that
//!    is building it. This is what makes "a sibling crate's `Cargo.toml.in`, rendered outside the
//!    workspace, importing straight from the real crates" work at all without a second
//!    `[workspace]` declaration or an `exclude` entry anywhere.
//! 2. **The template is `.in`, never `.rs`, for two independent reasons, and it lives in
//!    `vike-strategy-plugin`'s tree rather than this crate's own — reached below through
//!    `vike_strategy_plugin::{TEMPLATE_CARGO_TOML, TEMPLATE_LIB_RS}`, an ordinary Cargo dependency
//!    edge, not a bare `include_str!` file path crossing the crate boundary** (a first cut of this
//!    split did reach them that way, with `include_str!("../../vike-strategy-plugin/template/…")`
//!    — a coupling invisible to `cargo tree`/`layer_gate.rs` and only caught by a rename). The
//!    obvious reason the files stay `.in`: a `{{USER_SOURCE}}`/`{{NAME}}` placeholder is not valid
//!    Rust and a workspace member holding one would fail every lane that touches that crate. The
//!    one that is easy to miss: even a *syntactically valid* `template/lib.rs` sitting under
//!    `vike-strategy-plugin`'s root would still not be a workspace member (`template/` is not
//!    `src/`), so the `.in` extension is about keeping
//!    `crates/vike-ops/tests/hygiene/unsafe_and_toolchain_gate.rs`'s file walk from ever visiting it too —
//!    that gate's FULL-TREE scan of `vike-strategy-plugin` (an `UNSAFE_EXEMPT` crate) covers its
//!    `src/`, `build.rs`, `tests/`/`benches`/`examples`, but `template/` is none of those, and
//!    `.in` is not `.rs` either, so this file is invisible to it on BOTH counts — a property the
//!    dependency-edge fix above leaves unchanged, since `include_str!("../template/…")` still
//!    resolves relative to `vike-strategy-plugin/src/lib.rs`'s own location.
//! 3. **The artifact this crate hands back always ends in `.so`, regardless of the host
//!    platform's own dynamic-library convention.** Locating what `cargo` just built uses
//!    [`std::env::consts::DLL_PREFIX`]/[`std::env::consts::DLL_SUFFIX`] (so this still finds the file on a
//!    non-Linux dev box), but the design's own naming (`<name>-<sha256>.so`) is what every OTHER
//!    piece of this design — this service, the loader, the retention pruner — matches
//!    against, and this workspace's `dlopen` side only ever runs on the Linux prod boxes. Picking
//!    a platform-native suffix here would make the artifact name a THIRD thing (after `name` and
//!    the sha) that varies per box, for a design that assumes one filesystem shared by this
//!    service and a server that both run on the same Linux host.
//!
//! ⚠ **[`build_plugin`]'s `workspace_root` parameter is a fix, not a style choice — read this
//! before changing it back.** An earlier version of [`render_cargo_toml`] computed the sibling
//! `vike-model`/`vike-strategy-plugin` paths from `env!("CARGO_MANIFEST_DIR")`, exactly the defect
//! `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs` exists to catch: that macro expands, AT
//! COMPILE TIME, to the tree this crate was BUILT in — never the tree a RUNNING binary is later
//! deployed into. On a dev box or a lane, the two coincide, which is precisely why the defect is
//! invisible everywhere except in production — the exact shape `crates/bridges/dukascopy/src/
//! exec.rs`'s retired `bridge_jar`/`java_program` functions already convicted themselves of
//! ("finds neither jar nor JVM and degrades to paper, with nothing in the log but 'bridge jar not
//! found'"). Unlike a missing data file degrading a venue to paper, a builder pointed at the wrong
//! `vike-model` source cannot degrade gracefully at all — cargo either finds real crates to build
//! a plugin against or the compile fails — so there is no ladder to fall through to the way
//! `crates/vike-model/src/paths/store_path.rs`'s `resolve_store_root_from` gives a data path a project
//! and per-user rung. The fix is therefore the plain one this gate's own doc asks for first: the
//! caller (`src/bin/vike-strategy-builder.rs`, which owns the one process-environment sweep) reads
//! `VIKE_STRATEGY_BUILDER_WORKSPACE_ROOT` and REFUSES to start without it — the same "refuse rather
//! than silently degrade" shape already used there for the missing-key case — and hands the real
//! checkout root down as an ordinary parameter. `render_cargo_toml` now resolves no global state of
//! its own; the ONLY place `env!("CARGO_MANIFEST_DIR")` still appears in this file is inside the
//! inline `mod tests` at the end of it (cfg-gated on test builds — deliberately not spelled as the
//! literal attribute here: `crates/vike-ops/tests/hygiene/temp_path_gate.rs`'s scope test is a plain
//! substring search for that exact token marking where a file's TEST code begins, so writing it out
//! in a doc comment ABOVE the real one would have pulled its boundary up to this paragraph and put
//! [`build_plugin`] — genuine production code — under that gate's stricter TEST
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
//! `<root>/user_data/plugins` under `ProtectSystem=strict` — and since 2026-09-26 its filesystem
//! jail does not even MOUNT the rest of the project root into the unit's namespace — so `<root>/tmp`
//! is unreachable in the unit's own sandbox and every `build_plugin` call would fail with an I/O
//! error the moment it tried to create a directory there; and there is still no OWNERSHIP for the
//! whole scratch tree and no SWEEP anywhere in this
//! crate — [`ScratchGuard`] below only protects the span before `cargo` runs, by design, so every
//! build still deliberately leaves a whole `target/` behind for the next retry, and nothing ever
//! bounds how many of those accumulate. Moving that same unbounded leak from a directory something
//! else empties (`PrivateTmp=true`'s temp dir, cleared on restart) to a directory nothing empties
//! (a mounted, persistent project volume) is not a fix for
//! `crates/vike-ops/tests/hygiene/system_temp_gate.rs` — it reproduces the exact 215 GB the CI box incident that
//! gate's own doc measures, just on a volume an operator now has to notice and clear by hand. A
//! real cure needs BOTH a startup sweep (`vike_model::scratch::sweep`, called once by `main`) AND
//! `<root>/tmp` added to the deploy unit's `ReadWritePaths=` and `BindPaths=` — a deploy-affecting
//! change earning
//! its own PR and its own review, not a corner of this fix round. See `SYSTEM_TEMP_PIN` in
//! `crates/vike-ops/tests/hygiene/system_temp_gate.rs` for the grandfather row this leaves behind and the
//! reopening condition written there.

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};
use vike_strategy_plugin::host::UNWIRED_HOOKS;
use vike_strategy_plugin::{TEMPLATE_CARGO_TOML, TEMPLATE_LIB_RS};

/// Which cargo profile [`build_plugin`] compiles the rendered crate under.
///
/// ⚠ **This is not a tuning knob — it is a CORRECTNESS parameter, and it used to be hardcoded to
/// `Release`.** `vike_strategy_plugin::fingerprint::FINGERPRINT` folds Cargo's own
/// `PROFILE`/`OPT_LEVEL` in, and the loader refuses a plugin whose fingerprint differs from the
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
    /// never disagree: a profile renamed in `vike-strategy-plugin`'s `build.rs` changes both
    /// sides at once.
    ///
    /// ⚠ **The builder SERVICE must not use this.** It builds for a DIFFERENT process (the
    /// backtest server), whose profile it cannot observe; `builder.rs` names
    /// [`Profile::Release`] outright and says why.
    pub fn of_this_binary() -> Self {
        if vike_strategy_plugin::fingerprint::FINGERPRINT.contains("profile=release") {
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

/// Why [`build_plugin`] failed to produce an artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    /// `cargo` ran and rustc reported a failure. Carries rustc's OWN diagnostic text VERBATIM
    /// (`cargo build`'s captured stderr), never a summary — a generic "build failed" makes the
    /// user guess at their own syntax error. This is the ONE variant a user's OWN mistake can
    /// produce; the other two are host/environment problems.
    Compile(String),
    /// A filesystem operation failed — writing the rendered source, creating the scratch crate
    /// directory, or moving the finished artifact into `out_dir`.
    Io(String),
    /// `cargo` itself could not be found or could not be started — a toolchain problem rather
    /// than a problem with the user's source (contrast [`BuildError::Compile`], where `cargo` DID
    /// run and rustc reported a failure).
    Toolchain(String),
    /// The source's `Strategy` impl overrides one or more seam methods the plugin vtable does not
    /// carry (`vike_strategy_plugin::host::UNWIRED_HOOKS`). Carries every offending hook name.
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
    /// A `name` this function refuses to turn into a path component — found by an adversarial
    /// review of the deployed jail: `artifact_path` below is `out_dir.join(format!("{name}-{sha}.so"))`
    /// with `name` UNCHECKED (only the separate scratch `crate_name` was ever sanitized), and the
    /// caller is a wire peer who can send any string. MEASURED live: a planted symlink at
    /// `<served-name>-<sha>.so` pointing at `cargo-home/config.toml`, served through the placement
    /// step below, overwrote it. Refused here, before either the scratch crate name or the served
    /// path is built from `name`, rather than sanitized — a silent rewrite would let a build
    /// succeed under a DIFFERENT filename than the one whatever calls back with this `name`+`sha`
    /// expects (`vike_studio_core::run`'s own `{name}-{sha}.so` reconstruction), which a refusal
    /// cannot do.
    InvalidName(String),
    /// [`CargoHomePin::check`] found `$CARGO_HOME/config.toml` no longer matches what this daemon
    /// captured at startup. Carries only PRESENT/ABSENT for each side, never a hash or the file's
    /// content — a hash is still enough entropy about attacker-controlled bytes to be worth
    /// withholding, and "present vs. absent" is all an operator needs to act on this.
    CargoHomeTampered { was_present: bool, now_present: bool },
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Compile(msg) => write!(f, "compile failed:\n{msg}"),
            BuildError::Io(msg) => write!(f, "i/o error: {msg}"),
            BuildError::Toolchain(msg) => write!(f, "toolchain error: {msg}"),
            BuildError::SourceVersionMismatch(msg) => write!(f, "source version mismatch: {msg}"),
            BuildError::InvalidName(name) => write!(
                f,
                "invalid strategy name {name:?}: it becomes a path component \
                 (`out_dir/<name>-<sha>.so`) and must not contain `/`, `\\`, be empty, or be `.`/`..`"
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
            BuildError::UnwiredHook(hooks) => write!(
                f,
                "this strategy overrides {} — a `Strategy` hook the PLUGIN mechanism does not \
                 deliver. Loaded as a plugin it would build, load and handshake cleanly and then \
                 never receive that hook, so the same file would trade differently here than it \
                 does compiled into the binary, with no error and entirely plausible numbers. \
                 Two ways forward: drop the override, or wire the hook (a `PluginVTable` field, \
                 a `loader.rs` bind, an `extern \"C\"` export in the template, and a \
                 `WIRED_HOOKS` entry). ⚠ The three hooks still on this list are the ones whose \
                 payload is a `serde_json::Value` or a RETURNED owned value — see \
                 `vike_strategy_plugin::host::UNWIRED_HOOKS` for why wiring one is a new \
                 ownership contract rather than a fourteenth copy of an existing pattern. The \
                 build-time tier honours every hook today, so a strategy that genuinely needs \
                 one belongs there until it is wired.",
                hooks.iter().map(|h| format!("`{h}`")).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

impl std::error::Error for BuildError {}

/// A directory-name-safe token for the CONTRACT a plugin must satisfy to load into THIS process:
/// `vike_strategy_plugin::abi::ABI_VERSION` plus a hash of
/// `vike_strategy_plugin::fingerprint::FINGERPRINT` (which itself folds rustc's release and
/// commit, the target triple, the profile and the opt-level) — plus a hash of the TEMPLATE
/// itself, both files.
///
/// ⚠ **That last part is NOT what `loader::load` compares, and this doc said "exactly what
/// `loader::load` compares, and nothing else" until the dependency-surface widening.** The two
/// answer different questions and the cache needs both: the loader asks "may this artifact enter
/// THIS process", which a template edit does not change, while a cache key must ask "was this
/// artifact built from what we would build now", which a template edit changes completely. The
/// body carries the failure that distinction prevents.
///
/// ⚠ **It lives HERE, next to [`build_plugin`], because it exists to compensate for
/// [`build_plugin`]'s own cache — and, since [`build_plugin`]'s cache started verifying its own
/// handshake, it compensates for a NARROWER slice of that cache than it used to.** The ABI-version
/// and fingerprint half of "an artifact built under a DIFFERENT contract is returned unrebuilt on
/// a hit" is CLOSED now: the cache check itself opens the artifact and asks
/// `vike_strategy_plugin::loader::verify_handshake` (see [`build_plugin`]'s own comment at the
/// check), and a mismatch there triggers a rebuild rather than being handed to the loader to
/// refuse. What this tag still compensates for is the piece `verify_handshake` structurally
/// CANNOT see: a TEMPLATE-content edit that changes neither the ABI version nor the fingerprint —
/// `vike_plugin_abi_version()`/`vike_plugin_fingerprint()` answer the same numbers either side of
/// such an edit, because both are properties of the CONTRACT (the vtable shape, the toolchain),
/// not of what the generated code inside the template actually does. So a caller that owns its own
/// output directory (every test in this workspace) still keys it on this token, which folds the
/// template's own bytes in; a reader of the cache should meet what's still open at the check
/// itself, in the same file.
///
/// ⚠ **PRODUCTION uses NEITHER this tag NOR a template-content check, and that absence is now the
/// one open piece rather than the whole gap.** The builder service writes
/// `user_data/plugins/<name>-<sha>.so`, which is the design's WIRE contract — Studio sends that sha
/// back with its Run — so the name may not grow a tag and the directory is not the service's to
/// choose. A template edit deployed to a box with a warm cache and an unchanged `ABI_VERSION`
/// still serves the PREVIOUS template's artifact for an unchanged source, same as before this
/// branch — this paragraph names the residual rather than leaving it to be rediscovered, the same
/// way the ABI/fingerprint gap was named before it had a fix. It is narrower than that gap was: a
/// template edit is rarer than a toolchain upgrade (the trigger this branch closes) and, per this
/// module's own convention (`ABI_VERSION` bumped for "the set or the signatures change"), a
/// template edit that changes DISPATCH behaviour is supposed to bump the ABI version anyway — so
/// the case this leaves open is specifically a template edit that changes generated logic WITHOUT
/// touching the vtable shape, which `verify_handshake` cannot see by construction (it only asks
/// the two handshake exports, and neither answers "what does the rest of this file do").
///
/// ⚠ **One spelling, because there were two and a missing third.** `vike-strategy-plugin`'s two
/// harnesses each hand-wrote this rule; `vike-studio-core`'s `plugin_run.rs` did not have it and
/// went red in CI on a warm runner cache after an `ABI_VERSION` bump. All three call this now.
/// It is not defined in `vike-strategy-plugin` itself for a concrete reason: `plugin_run.rs`'s
/// `this_file_reaches_the_plugin_only_through_the_production_build_strategy` REFUSES to let that
/// file name `vike_strategy_plugin` in code at all, and that gate is worth more than the
/// convenience of a shorter path.
///
/// `DefaultHasher`'s output is not stable across Rust versions. Harmless, and in the SAFE
/// direction: a changed hash means a new directory, i.e. a rebuild.
pub fn artifact_dir_tag() -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    vike_strategy_plugin::fingerprint::FINGERPRINT.hash(&mut h);
    // ⚠ **The TEMPLATE is hashed in too, and it was not until the dependency-surface widening.**
    // The fingerprint answers "was this built by the same toolchain under the same profile"; it
    // says nothing about WHAT was built. A template edit changes the artifact's contents —
    // `Cargo.toml.in` its whole dependency closure, `lib.rs.in` every export — while the
    // fingerprint, the ABI version and (for an unchanged user file) the source sha all stay put,
    // so `build_plugin`'s `artifact_path.exists()` cache hands back the PREVIOUS template's `.so`
    // and the loader accepts it, because there is nothing wrong with it except that it is old.
    // That is the silent half of the same failure `ABI_VERSION` 2 -> 3 produced loudly.
    TEMPLATE_CARGO_TOML.hash(&mut h);
    TEMPLATE_LIB_RS.hash(&mut h);
    format!("abi{}-fp{:016x}", vike_strategy_plugin::abi::ABI_VERSION, h.finish())
}

/// The ONLY variables [`build_plugin`]'s `cargo` child is handed, each with the reason it is there.
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
/// password database without it, and it costs a registry row in `vike_ops::settings::SETTINGS`);
/// proxies and CA paths (an operator sets those in `$CARGO_HOME/config.toml`, which cargo reads
/// without being told); `LANG`/`LC_*`; and every `VIKE_*` and credential variable, which is the
/// point.
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
         `rustc`, and the loader refuses a plugin built by a different one",
    ),
    (
        "RUSTUP_HOME",
        "set only where `rustc` is a rustup proxy (a verification lane, a dev box): the proxy \
         needs it to find the toolchain, and it names a directory, not a credential",
    ),
    (
        "RUSTUP_TOOLCHAIN",
        "set by rustup's own proxy for the process it launched; forwarding it keeps the nested \
         `rustc` the one the caller runs on, so a plugin a TEST builds carries the test's own \
         toolchain fingerprint",
    ),
    (
        "TMPDIR",
        "rustc and the linker write their temporaries here; a verification lane points it at a \
         dedicated disk, and without it they fall back to `/tmp`",
    ),
];

/// The environment [`build_plugin`] hands its `cargo` child, built from a caller-supplied map:
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

/// **Found by an adversarial review of the deployed jail, independent of [`BuildError::InvalidName`]
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

    /// A pin that never refuses — for the many `build_plugin` callers (this crate's and its
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

/// Lowercase hex SHA-256 of `bytes` — the content-address every artifact name and cache check is
/// keyed on. `pub` so callers that already have the source text (the builder service, recording
/// which sha it just answered) never need to re-derive the naming rule by hand.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Guards the scratch directory `build_plugin` builds against for exactly the span between its
/// creation and the point `cargo` has genuinely been invoked — never the whole function.
///
/// ⚠ **A fix for `crates/vike-ops/tests/hygiene/journal_scratch_gate/tree_rule.rs`'s
/// `no_new_unguarded_temp_paths_outside_vike_core`, not a full migration.** That gate correctly
/// flagged this file: `build_plugin` mints a directory under `env::temp_dir()` and writes into it
/// with no self-deleting handle at all, so a PANIC partway through (a bug in `render_cargo_toml`,
/// an I/O edge case) left it orphaned forever, and nothing in this crate ever cleans one up.
/// ⚠ This sentence used to add "not even `clean_scratch`" — a `pub fn` that removed exactly this
/// directory and that NOTHING in the tree ever called. It has been deleted rather than left as an
/// unreached hedge; the leak is the same either way, and its real cure is the sweep named in this
/// module's header. Reused NEITHER of the two ready-made guards this workspace already has
/// (`vike_model::scratch::ScratchDir`, `tempfile::TempDir`) because both unconditionally delete on
/// drop, which is wrong for this SPECIFIC scratch dir: it is deliberately content-addressed
/// (`<crate_name>-<sha>`, not a fresh per-call name) so a RETRY of identical source reuses cargo's
/// own incremental `target/` — the module doc's whole point. `ScratchDir::create_in` doubly
/// cannot be reused: its path is `<tag>-<pid>-<n>`, which changes every process and defeats
/// cross-invocation caching outright, and its private `create_at` unconditionally
/// `remove_dir_all`s a pre-existing directory at its path before returning — exactly the
/// incremental cache this function exists to keep.
///
/// So: a hand-rolled guard (the gate's own second sanctioned shape, `impl Drop`), held only until
/// [`Self::keep`] is called right after `cargo` has actually run. Everything BEFORE that call —
/// writing the rendered `Cargo.toml`/`lib.rs`, or a bug in either write — cleans up on any exit,
/// panic included, because nothing valuable could exist yet. Everything from `cargo` having run
/// onward (a compile failure worth caching for the next retry, a missing cdylib, a successful
/// build) is UNCHANGED from before this fix: the directory persists exactly as it always has,
/// because `keep` was already called before any of those returns.
///
/// ⚠ **This guard covers the WRITE-side panic-safety half only, and it is deliberately narrow.**
/// It does not own the whole scratch tree, and nothing in this crate ever sweeps it — see the
/// module doc's closing paragraph for why the OS temp directory this guards is a bounded leak
/// (something else empties it) rather than an unbounded one, and why moving it to a directory
/// nothing empties would need a sweep this crate does not yet have.
struct ScratchGuard {
    path: PathBuf,
    keep: bool,
}

impl ScratchGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, keep: false }
    }

    /// Give up ownership: `Drop` becomes a no-op. Named and documented the same way
    /// `vike_model::scratch::ScratchDir::keep` is, for the same reason — a decision at the call
    /// site, not a missing cleanup.
    fn keep(&mut self) {
        self.keep = true;
    }
}

impl Drop for ScratchGuard {
    /// Best-effort, matching `ScratchDir`'s own `Drop`: a failed removal must not panic during an
    /// unwind, which would abort the process and turn the very panic this guard exists to survive
    /// into a crash instead.
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// The file [`verify_source_version`] reads at the ROOT of `workspace_root` — never a name a
/// caller chooses, always this one, so the release packaging step
/// (`.github/workflows/release.yml`'s "Package the strategy builder's workspace source" step) and
/// this reader can never spell it two ways. Not derivable across a YAML step and a Rust constant,
/// so the two sides simply have to agree — the same shape `deploy_layout_gate.rs`'s `PLUGINS_REL`/
/// `STORE_REL` literals already carry for a fact duplicated across languages;
/// `crates/vike-strategy-builder/tests/build_errors.rs`'s
/// `the_release_workflow_stamps_the_spelling_this_crate_reads` pins the two together.
pub const SOURCE_VERSION_STAMP_FILE: &str = "SOURCE_GIT_SHA";

/// Refuse to build against a `workspace_root` whose own version is PRESENT and does not match this
/// binary's — the cure for a gap the toolchain fingerprint and `abi_version` leave wide open.
///
/// **What those two guards catch, and what they do not.** `vike_strategy_plugin::fingerprint::
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
/// existing caller of [`build_plugin`] (this crate's own tests, `vike-strategy-plugin`'s
/// equivalence/panic/params harnesses, `vike-studio-core`'s `plugin_run.rs`) points `workspace_root`
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

/// Build (or reuse the cached build of) `src` under strategy `name`, dropping the finished
/// `.so` into `out_dir` as `<name>-<sha256 of src>.so`. `workspace_root` is the checkout root
/// (the directory holding `crates/vike-model`, `crates/vike-strategy-plugin`, …) the rendered
/// scratch crate's `path` dependencies resolve against — see this module's doc for why it is a
/// caller-supplied parameter rather than a path this function resolves from its own compile-time
/// location. `cargo_bin` is the `cargo` executable to invoke — Cargo's own `CARGO` variable when
/// this whole tree is itself built by cargo, `"cargo"` (off `PATH`) otherwise; a THIRD caller-supplied
/// parameter for the same reason as `workspace_root`, not a style echo of it: see this module's
/// doc for why a bare `std::env::var("CARGO")` here used to escape `LIBRARY_PIN`'s ratchet
/// entirely through a filename coincidence this file no longer has.
///
/// `child_env` is the WHOLE environment that `cargo` — and therefore every `build.rs` and
/// proc-macro it runs — receives: the spawn clears the calling process's environment first and
/// adds back exactly these pairs plus `CARGO_TARGET_DIR`. Build it with [`child_env`];
/// [`CHILD_ENV_ALLOWLIST`] carries why each name is there and why everything else is not.
///
/// The cache is `Path::exists` on that content-addressed name, checked BEFORE any `cargo`
/// invocation: `src` hashes to the existing file's name iff it is byte-identical to what produced
/// it, so a hit is provably the same code — never a mtime or "looks unchanged" heuristic, and
/// never a rebuild an operator has to remember to skip. ⚠ **A hit is then VERIFIED, not just
/// found**: the file is opened and its two handshake exports are checked against this host's own
/// `ABI_VERSION`/toolchain fingerprint before it is served, and a mismatch (or a file that will
/// not even `dlopen`) is REBUILT rather than handed back — see the check itself, below, for the
/// cost this adds and what it still does not cover.
pub fn build_plugin(
    src: &str,
    name: &str,
    out_dir: &Path,
    workspace_root: &Path,
    cargo_bin: &str,
    child_env: &[(String, String)],
    profile: Profile,
    cargo_home_pin: &CargoHomePin,
) -> Result<PathBuf, BuildError> {
    // FIRST of all: `name` becomes a path component two lines below (`artifact_path`) and a
    // scratch crate name further down, and it is a wire-supplied string an authenticated caller
    // fully controls. `sanitize_crate_name` only ever covered the scratch identifier — this is
    // the separate guard for the SERVED path. Refused rather than sanitized: see
    // `BuildError::InvalidName`'s doc for why a silent rewrite is the wrong shape here.
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(BuildError::InvalidName(name.to_string()));
    }

    // Before the cache and before cargo: a source overriding a hook the vtable does not
    // carry is refused outright. Ahead of the cache deliberately — an artifact built before this
    // rule existed must not buy a source past it, and a refusal that depends on whether a file
    // happens to be on disk is not a rule.
    let unwired = unwired_hook_overrides(src);
    if !unwired.is_empty() {
        return Err(BuildError::UnwiredHook(unwired));
    }

    let sha = sha256_hex(src.as_bytes());
    // ⚠ The artifact name carries the source sha and NOT the profile, because that name is the
    // design's wire contract (`<name>-<sha>.so`, the sha Studio sends back with its Run). So one
    // `out_dir` holds ONE profile's artifacts: a caller that builds the same source both ways
    // must hand each a directory of its own, or the second build finds the first's artifact,
    // returns it, and the loader refuses it on the fingerprint. The service only ever builds
    // release; the tests in this workspace tag their own directory with the profile.
    let artifact_path = out_dir.join(format!("{name}-{sha}.so"));

    // ⚠ **CLOSED — the cache used to be keyed on the SOURCE and on nothing else, which was a real
    // hole.** The sha covers `src`; it never covered `vike_strategy_plugin::abi::ABI_VERSION`, the
    // toolchain fingerprint, or the profile. So an artifact built before any of those changed was
    // returned UNREBUILT on a bare `Path::exists()`, and `loader::load` then refused it on exactly
    // the guard that changed — `AbiMismatch` or `FingerprintMismatch`. MEASURED: the `ABI_VERSION`
    // 2 -> 3 bump did this to both of this crate's sibling `build_plugin` tests on a warm scratch
    // directory, on code that was correct. And it stopped being a DEV-BOX-ONLY exposure once the
    // builder joined `deploy/sbin/vike-trader-ci-deploy`'s `DEPLOY_UNITS` (the "builder ships"
    // work): a box with a warm PRODUCTION cache now exists, and an operator upgrading this
    // service's ABI/toolchain on one used to get `Ok(sha)` for a stale artifact and a refused Run,
    // with no way to force a rebuild but deleting the file by hand.
    //
    // The cure: OPEN the artifact and ask it, before serving it —
    // `vike_strategy_plugin::loader::verify_handshake` does exactly what a real [`load`] does up to
    // (and no further than) the two handshake exports, so a hit is judged by the SAME guards the
    // loader will apply, never a proxy for them. A mismatch (or a file that fails to `dlopen` at
    // all — corruption is not a case genuinely different from drift; both mean "not what would be
    // built now") falls through to a REBUILD rather than an error: the cache existing at all must
    // never make a request fail that a cold cache would have satisfied.
    //
    // ⚠ **Cost, MEASURED rather than argued (this function's own module doc carries the exact
    // numbers and how they were taken).** A hit used to cost one `exists()` syscall. It now costs
    // one `dlopen` plus the two `dlsym`s the handshake needs — still nothing close to a `cargo`
    // invocation, which is the property the whole cache exists to preserve (an unchanged strategy
    // must Run instantly).
    //
    // ⚠ **Still narrower than "verify everything the loader will ask for later"** — deliberately.
    // `verify_handshake` resolves NOTHING beyond the two handshake exports (not the other fifteen
    // dispatch symbols `load` binds), because a cache decision only needs "is this artifact current
    // and openable", not "can this specific process run it end to end" — the latter is what the
    // REAL `load` call the backtest server makes later already proves, in a different process, and
    // duplicating fifteen more `dlsym`s here would not change this function's own verdict once the
    // two REQUIRED exports already agree (the template emits every dispatch symbol
    // unconditionally, and an artifact built against an older vtable is already caught on its
    // `ABI_VERSION` alone).
    //
    // ⚠ **What this does NOT close: a template-content edit that changes neither the ABI version
    // nor the fingerprint.** `artifact_dir_tag`'s own doc carries that residual — it is
    // structurally invisible to a check that only asks the two handshake exports what CONTRACT
    // they speak, never what the rest of the file does.
    //
    // ⚠ **The one path by which a caller's code runs INSIDE this daemon rather than under cargo,
    // and the cleared child environment below does not reach it.** `dlopen` runs the artifact's
    // initializers in this process, with this process's environment and memory — the service key
    // included. Stated as a residual rather than closed: only this daemon writes `out_dir`, and it
    // writes there only what an AUTHENTICATED caller's Build produced, so the code that could run
    // here is code whose author already holds the key. Closing it means verifying in a child with
    // a cleared environment, which costs a process per cache hit.
    if artifact_path.exists() {
        match vike_strategy_plugin::loader::verify_handshake(&artifact_path) {
            Ok(()) => return Ok(artifact_path),
            Err(mismatch) => {
                // Not silent: an operator watching the journal should be able to tell "a cache hit
                // was invalidated and rebuilt" from "cargo ran because nothing was cached yet" —
                // the same "said, not silent" argument `verify_source_version`'s own absent-stamp
                // branch already makes in this file.
                eprintln!(
                    "vike-strategy-builder: cached artifact at {} no longer matches this host \
                     ({mismatch}) — rebuilding rather than serving it",
                    artifact_path.display()
                );
            }
        }
    }

    // The version-skew guard (`verify_source_version`'s own doc carries the whole argument) runs
    // HERE — after the cache hit above, not before it. A cache hit serves a `.so` that was already
    // built from WHATEVER `workspace_root` held at build time, so today's `workspace_root` state
    // is irrelevant to what gets served; checking it first would refuse requests the cache could
    // have satisfied with no compilation and nothing new read from `workspace_root` at all. It is
    // still ahead of every filesystem write below and ahead of `cargo` itself, which is the
    // property this function's callers actually need: a GENUINE (re)build never proceeds against
    // an unconfirmed or mismatched source tree.
    verify_source_version(workspace_root)?;

    // Same "ahead of the real work, never behind it" placement as the version check just above —
    // and for the same reason: a cache hit above never invokes cargo, so it needs neither check,
    // but a genuine (re)build must never proceed against a cargo configuration that drifted since
    // this daemon started. See `CargoHomePin`'s doc for what drift here means and why.
    if let Err((was, now)) = cargo_home_pin.check() {
        return Err(BuildError::CargoHomeTampered {
            was_present: was.is_some(),
            now_present: now.is_some(),
        });
    }

    fs::create_dir_all(out_dir)
        .map_err(|e| BuildError::Io(format!("creating {}: {e}", out_dir.display())))?;

    // A content-addressed SCRATCH directory too (not a fresh tempdir per call): a retried build
    // of the identical source reuses cargo's own incremental target cache instead of starting a
    // cold `vike-model` compile every time. It is never read as a cache on its own — the ONLY
    // cache check is the `artifact_path.exists()` above — so a half-finished scratch build left
    // over from a killed process cannot be mistaken for a finished artifact.
    //
    // ⚠ Deliberately `std::env::temp_dir()`, not `<project>/tmp` — see this module's doc for why
    // that move was reverted and what a real cure needs (a sweep, and a write grant the deployed
    // unit does not have today).
    //
    // ⚠ **The name carries the WORKSPACE ROOT as well as the source sha, and that is a collision
    // fix rather than tidiness.** `std::env::temp_dir()` is one shared directory per BOX, while
    // `workspace_root` is a per-CHECKOUT parameter that the rendered `Cargo.toml`'s `path`
    // dependencies point at — and the sha covers only `src`. So two processes building the same
    // source out of DIFFERENT checkouts resolved the same scratch path, and the second one's
    // `fs::write` of `Cargo.toml` re-pointed those path dependencies underneath the first one's
    // running cargo. Cargo's own target-dir lock serializes the two BUILDS and cannot see a
    // manifest rewritten between them.
    //
    // That stopped being hypothetical when `tests/equivalence.rs` was un-ignored: this
    // workspace's CI runs several runner instances on ONE box, so two concurrent PR jobs now
    // build the identical committed fixture from two checkouts as a matter of course. Including
    // the root keeps every checkout's incremental cache intact and separate, which is the whole
    // point of content-addressing it in the first place. Hashed rather than embedded verbatim
    // because a path is not a directory-name-safe string.
    let crate_name = sanitize_crate_name(name);
    let root_hash = sha256_hex(workspace_root.display().to_string().as_bytes());
    let root_tag = &root_hash[..12];
    let scratch_dir = std::env::temp_dir()
        .join("vike-strategy-plugin-build")
        .join(format!("{crate_name}-{sha}-{root_tag}"));
    let src_dir = scratch_dir.join("src");
    fs::create_dir_all(&src_dir)
        .map_err(|e| BuildError::Io(format!("creating {}: {e}", src_dir.display())))?;
    // See `ScratchGuard`'s own doc for why this exists and why it is `keep`-ed, not left to run
    // its `Drop`, the moment cargo has genuinely been invoked below.
    let mut scratch_guard = ScratchGuard::new(scratch_dir.clone());

    let cargo_toml = render_cargo_toml(&crate_name, workspace_root);
    fs::write(scratch_dir.join("Cargo.toml"), cargo_toml)
        .map_err(|e| BuildError::Io(format!("writing Cargo.toml: {e}")))?;

    let lib_rs = TEMPLATE_LIB_RS.replace("{{USER_SOURCE}}", src);
    fs::write(src_dir.join("lib.rs"), lib_rs)
        .map_err(|e| BuildError::Io(format!("writing lib.rs: {e}")))?;

    let target_dir = scratch_dir.join("target");
    let mut cmd = Command::new(cargo_bin);
    // ⚠ `env_clear()` FIRST, then only the allowlist. This process's environment carries this
    // service's own key (or the path to it), `CREDENTIALS_DIRECTORY`, and anything an operator put
    // in the unit's `.env`; `cargo` would hand all of it to a caller's `build.rs`.
    // `crates/vike-strategy-builder/tests/child_env.rs` proves it against the real daemon — a
    // planted variable and the key both stay out of the child — and deleting this call is the
    // mutation it goes red on.
    cmd.env_clear().envs(child_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    cmd.arg("build");
    if let Some(flag) = profile.cargo_flag() {
        cmd.arg(flag);
    }
    let output = cmd
        .arg("--manifest-path")
        .arg(scratch_dir.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target_dir)
        .output()
        .map_err(|e| BuildError::Toolchain(format!("could not run `{cargo_bin} build`: {e}")))?;

    // `cargo` genuinely ran, whether it went on to succeed or fail to compile — from here on the
    // scratch dir (including whatever `target/` state it now holds) is worth keeping for a retry
    // of the same content, exactly as it always has been. Every remaining exit below is therefore
    // unguarded again, on purpose: this is where the deliberate persistent-cache behaviour takes
    // back over from the panic-safety this guard exists to add.
    scratch_guard.keep();

    if !output.status.success() {
        // rustc's own words, verbatim — never re-summarized. `cargo build`'s diagnostics (and its
        // own "could not compile … due to previous error" trailer) are on stderr; stdout carries
        // only build-script/test-harness noise this crate never produces.
        return Err(BuildError::Compile(String::from_utf8_lossy(&output.stderr).into_owned()));
    }

    let built_dir = target_dir.join(profile.target_subdir());
    let built = find_cdylib(&built_dir, &crate_name).ok_or_else(|| {
        BuildError::Io(format!(
            "cargo reported success but no cdylib for `{crate_name}` was found under {}",
            built_dir.display()
        ))
    })?;

    // ⚠ **`remove_file` first, and the fallback below never `fs::copy`s — found by the same
    // review as `BuildError::InvalidName`, and closed even though that refusal already covers the
    // only way `artifact_path` was reachable outside `out_dir`'s own entries.** `out_dir` is a
    // directory an authenticated caller's OWN prior Build (or the jail's write grant generally)
    // can plant a symlink into — e.g. `<name>-<sha>.so -> ../../bin/rust-toolchain/cargo-home/config.toml`
    // — and the OLD `fs::copy` fallback follows a symlink at its destination and overwrites
    // whatever it points to. MEASURED live: on the deployed layout `built` (this process's own
    // `PrivateTmp`) and `out_dir` (a separate bind mount) are on different filesystems, so `rename`
    // always returns `EXDEV` and the fallback is the path ALWAYS taken in production, never a rare
    // corner case. `remove_file` unlinks whatever directory entry is at `artifact_path` —
    // symlink or not — without ever following it (POSIX `unlink(2)` never dereferences the final
    // component), which is also what the legitimate case above needs: a stale cached artifact
    // that just failed `verify_handshake` falls through to here and must be replaced. Then
    // `create_new` (`O_EXCL`) refuses to open a path anything already occupies — including a
    // symlink re-planted in the gap between the `remove_file` and here — rather than silently
    // writing through it.
    let _ = fs::remove_file(&artifact_path);
    match fs::rename(&built, &artifact_path) {
        Ok(()) => {}
        Err(_) => {
            let mut dst =
                fs::OpenOptions::new().write(true).create_new(true).open(&artifact_path).map_err(
                    |e| BuildError::Io(format!("creating {}: {e}", artifact_path.display())),
                )?;
            let mut src = fs::File::open(&built)
                .map_err(|e| BuildError::Io(format!("reading {}: {e}", built.display())))?;
            io::copy(&mut src, &mut dst).map_err(|e| {
                BuildError::Io(format!(
                    "copying {} to {}: {e}",
                    built.display(),
                    artifact_path.display()
                ))
            })?;
        }
    }

    Ok(artifact_path)
}

/// The `vike_strategy_plugin::host::UNWIRED_HOOKS` a source's own `Strategy` impl overrides,
/// sorted and deduplicated — empty for a source the plugin mechanism can carry faithfully.
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
pub fn unwired_hook_overrides(src: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for body in strategy_impl_bodies(src) {
        for line in body.lines() {
            let trimmed = line.trim_start();
            let Some(rest) = trimmed.strip_prefix("fn ") else { continue };
            let Some(paren) = rest.find('(') else { continue };
            let name = rest[..paren].trim();
            if UNWIRED_HOOKS.contains(&name) && !found.iter().any(|f| f == name) {
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

/// Render `Cargo.toml.in`'s `{{NAME}}` placeholder plus the two `path` dependencies back into
/// the REAL checkout — `workspace_root.join("crates").join(…)`, never a compile-time-baked path
/// (see this module's doc for why: a running binary may not resolve a path from the manifest
/// directory the compiler baked into it, which is exactly what a bare `env!("CARGO_MANIFEST_DIR")`
/// here would do).
fn render_cargo_toml(crate_name: &str, workspace_root: &Path) -> String {
    let crate_path = |name: &str| workspace_root.join("crates").join(name).display().to_string();
    // ⚠ `VIKE_STRATEGY_PLUGIN_PATH` is substituted BEFORE `VIKE_STRATEGY_PATH`, and the order is
    // load-bearing rather than tidy: `{{VIKE_STRATEGY_PATH}}` is not a substring of
    // `{{VIKE_STRATEGY_PLUGIN_PATH}}` (the braces close differently), so either order is correct
    // today — but a future placeholder that IS a prefix of another would corrupt the longer one
    // silently, and `render_cargo_toml_leaves_no_placeholder_behind` could not see it because no
    // `{{` would survive. Longest-first is the rule that stays correct without a test noticing.
    TEMPLATE_CARGO_TOML
        .replace("{{NAME}}", crate_name)
        .replace("{{VIKE_STRATEGY_PLUGIN_PATH}}", &crate_path("vike-strategy-plugin"))
        .replace("{{VIKE_INDICATORS_PATH}}", &crate_path("vike-indicators"))
        .replace("{{VIKE_STRATEGY_PATH}}", &crate_path("vike-strategy"))
        .replace("{{VIKE_MODEL_PATH}}", &crate_path("vike-model"))
}

/// A Cargo package name built from a strategy `name`: keep `[A-Za-z0-9_-]`, replace everything
/// else with `_`, and force a leading letter/underscore (a bare digit is not a valid crate-name
/// start). Defensive rather than load-bearing — every existing strategy folder name is already a
/// valid identifier — but this function's OWN input is a network-supplied string once the builder
/// service is in front of it, so it must not be able to escape the scratch directory naming
/// scheme or hand cargo a manifest field it will refuse to parse.
fn sanitize_crate_name(name: &str) -> String {
    let is_safe = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut out: String = name.chars().map(|c| if is_safe(c) { c } else { '_' }).collect();
    let starts_ok = out.starts_with(|c: char| c.is_ascii_alphabetic()) || out.starts_with('_');
    if out.is_empty() || !starts_ok {
        out.insert_str(0, "plugin_");
    }
    out
}

/// Find the `cdylib` `cargo` just produced under `release_dir` — platform-native prefix/suffix
/// (`libNAME.so` / `NAME.dll` / `libNAME.dylib`), never the design's own `<name>-<sha>.so`
/// spelling, which is what THIS function's caller renames the file TO, not what it looks for.
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
// directory `build_plugin` leaves behind, and it had ZERO callers anywhere in the tree — the
// builder service's retention pass prunes ARTIFACTS (`builder::prune_artifacts`), never scratch,
// and `build_plugin` deliberately keeps its own directory as cargo's incremental cache for a
// retried build of identical content. It was written for a caller that was never built.
//
// Deleted rather than wired up because calling it would have been WRONG, not merely unimplemented:
// the whole value of the content-addressed scratch tree is that a retry of the same source reuses
// its `target/`, so a caller that swept it after each build would turn every rebuild cold. The
// real cure for the unbounded growth is a STARTUP sweep of the whole tree
// (`vike_model::scratch::sweep`) plus the deploy-unit write grant this module's header argues for
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

    #[test]
    fn sha256_hex_is_stable_and_content_addressed() {
        assert_eq!(sha256_hex(b"a"), sha256_hex(b"a"));
        assert_ne!(sha256_hex(b"a"), sha256_hex(b"b"));
        // The empty-string SHA-256, a widely published constant — pins this against a second
        // implementation, not merely against its own past output.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sanitize_crate_name_produces_a_valid_cargo_identifier() {
        assert_eq!(sanitize_crate_name("fixture_hold"), "fixture_hold");
        assert_eq!(sanitize_crate_name("my-strategy"), "my-strategy");
        assert_eq!(sanitize_crate_name("weird name!"), "weird_name_");
        assert_eq!(sanitize_crate_name("123leading"), "plugin_123leading");
        assert_eq!(sanitize_crate_name(""), "plugin_");
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
        let rendered = render_cargo_toml("some_plugin", &test_workspace_root());
        assert!(!rendered.contains("{{"), "unrendered placeholder survived: {rendered}");
        assert!(rendered.contains("some_plugin"));
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
        let rendered = render_cargo_toml("some_plugin", &root);
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
}
