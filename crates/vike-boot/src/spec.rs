//! The sequence's vocabulary: what a root declares ([`BootSpec`]) and gets back ([`Booted`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_config::Settings;

/// What a binary calls itself in its first log line: `<name> <version> (<build identity>)`.
///
/// Both halves come from the ROOT's own `env!("CARGO_PKG_NAME")`/`env!("CARGO_PKG_VERSION")` —
/// those macros expand in the crate they are written in, so this crate cannot supply them and must
/// not try.
#[derive(Debug, Clone, Copy)]
pub struct Identity<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

/// Step 1: whether this root refuses a REMOVED environment variable.
#[derive(Debug, Clone, Copy)]
pub enum RemovedEnv {
    /// Refuse to start, naming what replaces it. Every root that loads settings.
    Refuse,
    /// This root does not refuse, and says why. A reason rather than a bare `false`: turning a
    /// startup refusal off is exactly the change that should have to justify itself in a diff.
    Ignore(&'static str),
}

/// Step 4: whether this root loads the settings for its own use.
#[derive(Debug, Clone, Copy)]
pub enum SettingsLoad {
    Load,
    /// Load them, and **REFUSE TO BOOT when the settings SEAL is unsound** —
    /// [`vike_config::Settings::seal_refusal`] set.
    ///
    /// ⚠ **This arm REVERSES `docs/decisions/0013-degrade-vs-refuse.md` for one state, and the
    /// narrowness is the whole of its justification.** 0013's rule is that a fault in a CAPABILITY
    /// degrades and announces rather than refusing — a daemon that will not start helps nobody.
    /// Here what degrades is the CEILING: a box whose seal is unsound resolves
    /// `policy.max_notional_per_order` from the compiled-in default, which is `None`, which is NO
    /// SIZE CAP on the edge every remote control command funnels through. The degrade is toward
    /// MORE exposure, and it is silent.
    ///
    /// So: a root that can PLACE OR GATE AN ORDER takes this arm; everything else takes
    /// [`SettingsLoad::Load`] and reports the mark. The repair verbs must keep running — the
    /// reason the refusal is a mark in the first place — so `vike-cli` does NOT take this arm even
    /// though it gates orders: it refuses per-VERB instead (`vike_cli::SEAL_REFUSING_VERBS`),
    /// keeping `config check`, `config show` and `config set` alive.
    ///
    /// ⚠ What this COSTS: the unit restart-loops. That is louder than a silent uncapped daemon and
    /// is the intended trade, but the box needs an operator, and `vike-cli config check` tells them
    /// which row is wrong.
    LoadAndRefuseUnsoundSeal,
    /// This root consumes no setting, and says why. [`Booted::settings`] is then the compiled-in
    /// defaults — never a half-loaded tree, so a consumer cannot accidentally read one.
    Skip(&'static str),
}

/// Step 3: the credential map.
pub enum Credentials<'a> {
    /// Load them HERE, through the ROOT's own loader, and hand the map back as
    /// [`Booted::credentials`].
    ///
    /// A function rather than a value so the OFF path never opens the store, and rather than a
    /// loader of this crate's own so every root keeps its exact resolution and log output. It is
    /// called AT MOST ONCE.
    LoadWith(&'a dyn Fn() -> HashMap<String, String>),
    /// This root does not need credentials at boot, and says why: a read invented here would open
    /// the store on a path that deliberately does not.
    Deferred(&'static str),
}

/// Step 5's input: where the rolling log file goes by default.
#[derive(Debug, Clone, Copy)]
pub enum LogHome {
    /// `<settings dir>/state/logs`, off the ONE walk this boot performed — the layer BELOW the
    /// `config.log_dir` setting and `$VIKE_LOG_DIR`, and ABOVE `vike-log`'s `<exe_dir>/logs` last
    /// resort.
    UnderSettings,
    /// This root does not take its log home from the settings walk, and says why — it derives one
    /// of its own (`vike-tradehub`, whose `$VIKE_STATE_ROOT` relocates the whole state tree) or it
    /// builds no subscriber at all (`vike-cli`). [`Booted::log_home`] is then `None`.
    Elsewhere(&'static str),
}

/// Step 6: whether the startup disclosure is rendered.
#[derive(Debug, Clone, Copy)]
pub enum Disclosure {
    /// Render [`vike_config::boot_lines`]. It re-derives every row's ORIGIN, by design (an origin
    /// cannot be recovered from a merged [`Settings`]), so it is worth skipping on a short-lived
    /// command-line invocation and worth paying for on a daemon.
    ///
    /// ⚠ **The settings STORE is NOT read a second time**, and that is a correctness property
    /// rather than an optimisation: [`boot`](crate::boot) hands the arm it already read into
    /// `boot_lines`, so the block cannot describe a source this process did not resolve from
    /// (`crates/vike-config/src/boot.rs`'s module doc argues it).
    Render,
    /// This root renders no disclosure, and says why.
    Skip(&'static str),
}

/// One composition root's declaration of its own startup.
pub struct BootSpec<'a> {
    /// The single `std::env::vars()` sweep the BINARY owns. This crate reads no environment.
    pub env: &'a HashMap<String, String>,
    /// Where the project WALK starts — the binary's `std::env::current_dir()`.
    ///
    /// ⚠ `None` (an unreadable working directory) disables the WALK, not the resolution: a
    /// `$VIKE_SETTINGS_DIR` in [`BootSpec::env`] NAMES the directory and is still honoured, because
    /// a name needs nowhere to start. `None` on BOTH rungs is what means no project can be found,
    /// and that is the legitimate answer the disclosure states out loud.
    ///
    /// A FIELD rather than a read of this crate's own: this crate reads no process state, and a
    /// test can then reach the no-working-directory arm without the process-global
    /// `std::env::set_current_dir`. `vike_secrets::project_settings_dir_for` takes it the same way.
    pub cwd: Option<&'a Path>,
    pub identity: Identity<'a>,
    pub removed_env: RemovedEnv,
    pub settings: SettingsLoad,
    pub credentials: Credentials<'a>,
    pub log_home: LogHome,
    pub disclosure: Disclosure,
}

/// Everything the sequence resolved — the value a binary destructures and keeps.
pub struct Booted {
    /// The resolved settings, or the compiled-in defaults under [`SettingsLoad::Skip`].
    ///
    /// ⚠ Its `warnings` are the loader's own non-fatal resolutions and are returned, never logged
    /// (this crate runs before a subscriber). The binary emits them — see the crate doc.
    pub settings: Settings,
    /// `<project>/settings` as the ONE resolution answered — `$VIKE_SETTINGS_DIR` if it named one,
    /// else the walk — or `None` when NEITHER rung could. Every other path in this struct hangs off
    /// it. It answers WHERE; [`Booted::settings_dir_override`] answers WHICH RUNG.
    pub settings_dir: Option<PathBuf>,
    /// The `$VIKE_SETTINGS_DIR` value that was HONOURED — trimmed, and `None` when blank or unset,
    /// which is the resolver's own rule (an empty `Environment=VIKE_SETTINGS_DIR=` line must fall
    /// through to the walk rather than resolve settings to the working directory).
    ///
    /// Returned because two roots need to distinguish "named" from "walked" and neither may read
    /// the environment for itself: `vike-cli config check` fails on a NAMED directory that is not
    /// on disk (set-but-unhonoured) while merely warning on a walked one, and `vike-datahub`
    /// threads the override into its alerting credential read.
    ///
    /// ⚠ **It is the RUNG, not a spare copy of the directory.** `Some` here implies `Some` in
    /// [`Booted::settings_dir`], holding the same path — a name is honoured whether or not there is
    /// a working directory.
    pub settings_dir_override: Option<String>,
    /// `<settings dir>/state` — the PROGRAM-WRITTEN state root, off the same one walk.
    ///
    /// Returned so no root re-derives it: `vike_model::paths::state_path::project_state_dir(&cwd)`
    /// is `$VIKE_SETTINGS_DIR`-BLIND, so the log home, `alerts.json`, the telegram at-most-once
    /// ledger and the strategy-state sidecars would hang off a SECOND walk that can answer with a
    /// different project from the one the settings and credentials came from.
    ///
    /// ⚠ **A root with its own state-root variable still layers it ON TOP.** `$VIKE_STATE_ROOT`
    /// relocates the whole state tree and outranks this; that read stays in the binary that owns it
    /// (see [`LogHome::Elsewhere`]), and this is the rung below it.
    pub state_dir: Option<PathBuf>,
    /// The credential map under [`Credentials::LoadWith`], `None` under [`Credentials::Deferred`].
    pub credentials: Option<HashMap<String, String>>,
    /// `<settings dir>/state/logs` under [`LogHome::UnderSettings`], else `None`. Goes straight
    /// into `vike_log::LogConfig::project_dir`.
    pub log_home: Option<PathBuf>,
    /// `<name> <version> (<build identity>)` — WHICH BINARY IS THIS, the first line in the log and
    /// the same string `--version` prints, so the answer is readable from the log alone and from
    /// the binary alone and the two cannot disagree.
    pub identity_line: String,
    /// The startup disclosure, or empty under [`Disclosure::Skip`]. Kept SEPARATE from
    /// [`Booted::identity_line`] because two roots interleave other lines between them.
    pub boot_lines: Vec<String>,
}
