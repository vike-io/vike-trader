//! The directory scan: `<project>/user_data/strategies/` → loaded strategies + named diagnostics.
//!
//! PURE in the sense that matters here — it READS the tree and returns what it found, and writes
//! nothing at all (not even the log it renders). See this module's parent for the layout rules and
//! for why a silent skip is the one failure mode this loader may not have.
//!
//! # Three decisions worth stating, because all three look like details and are not
//!
//! **An ABSENT root is not an error; an unreadable one is.** A fresh install has no `user_data/`
//! at all, and `crates/vike-model/src/paths/state_path.rs`'s `project_user_data_dir` returns the path
//! where the directory BELONGS rather than one it found — so "nothing there" is the ordinary state
//! and must stay silent. A root that EXISTS and cannot be listed is the opposite: a permissions
//! bug wearing the "not configured yet" answer looks exactly like a correct fresh install while
//! every strategy silently vanishes. That is the same distinction the credential store draws
//! (`crates/vike-secrets/src/store_locator.rs`), for the same reason, and it is why
//! [`LoadDiagnostic::RootUnreadable`] exists as its own row.
//!
//! **Every script is COMPILED during the scan**, through `crates/vike-studio-core/src/run.rs`'s
//! `build_strategy` — the same call a Run makes. Listing a strategy the Studio cannot actually
//! build would move the failure to the moment the user presses Run, by which time the error has
//! nothing to do with the file they last edited. Going through `build_strategy` rather than
//! reaching for `vike_script::RhaiStrategy` directly is what keeps "it loaded" and "it will run"
//! the same claim.
//!
//! **A folder in `strategies/rust/` named after a BUILT-IN strategy needs no entry file, and
//! reporting one as broken is a bug this module used to have.** The two trees hold two different
//! things (see the parent module's layout section): under `rhai/` the code is in the FILE, so a
//! folder with no `<name>.rhai` really has lost its strategy; under `rust/` the code is in the
//! BINARY — `crates/vike-backtest/src/harness/registry.rs`'s `strategy_by_name` resolves it by
//! NAME — and the folder holds nothing but PRESETS for it. A rust-side loader copying the `rhai`
//! rule verbatim would report every such folder as an error. So the entry-file probe is a property
//! of the TREE, not of the loader: [`StrategyBody`] is the type that says which one a loaded folder
//! is.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vike_model::Severity;
use vike_model::params::{ParamValue, Params};
use vike_model::paths::state_path::{RHAI_SUBDIR, RUST_SUBDIR};

use crate::StrategySpec;

mod compile_log;
mod diagnostics;
mod preset_files;
mod scan;

pub use compile_log::render_compile_log;
pub use diagnostics::LoadDiagnostic;
pub(crate) use diagnostics::stray_target;
use scan::scan_tree;

/// The `rhai` entry-file extension. Compared case-INSENSITIVELY: a Windows editor that saved
/// `SMA.RHAI` wrote a perfectly good script, and the platform this workspace is developed on folds
/// case.
pub(crate) const RHAI_EXT: &str = "rhai";

/// The `rust` entry-file extension, compared the same way as [`RHAI_EXT`].
///
/// ⚠ It is used ONLY to recognise a stray `.rs` loose in the root. A `.rs` inside a strategy folder
/// is never read here: it is an input to `cargo`, consumed long before this scan runs (see
/// `crates/vike-cli/src/cmd/init/content/readmes.rs`'s `RUST_README`, whose first line is about exactly
/// that).
pub(crate) const RUST_EXT: &str = "rs";

/// The TOML preset extension, compared the same way as [`RHAI_EXT`].
const TOML_EXT: &str = "toml";

/// The one key a preset may NOT define: it is the strategy's SOURCE, not one of its knobs.
///
/// `crates/vike-backtest/src/harness/registry.rs`'s `rhai_overrides` already treats it as reserved
/// (it is filtered out of the `param()` override map), and `crates/vike-cli/src/cmd/backtest/profile.rs`'s
/// `inject_script_src` is what legitimately sets it. A preset that carries one would let a params
/// file smuggle a whole script past `--script`, so it is refused rather than resolved by a
/// precedence rule nobody can see.
///
/// ⚠ **The spelling is `vike_model::RESERVED_SRC_KEY` and this is an ALIAS, not a second copy** —
/// see that constant for why the fact moved down to vike-model and what forced it.
const RESERVED_SRC_KEY: &str = vike_model::RESERVED_SRC_KEY;

/// The container key a preset must NOT wrap its knobs in — see [`check_preset_shape`].
const PARAMS_WRAPPER_KEY: &str = "params";

/// Where a loaded strategy's CODE lives — the difference between the two trees, as a type.
///
/// This is what makes a preset-only folder under `strategies/rust/` a first-class shape instead of
/// a missing entry file (see this module's doc).
#[derive(Debug, Clone, PartialEq)]
pub enum StrategyBody {
    /// The code is in the FILE. `entry` is `<dir>/<name>.rhai`, `source` its text verbatim, and it
    /// was PROVEN to compile during the scan.
    Rhai { entry: PathBuf, source: String },
    /// The code is in the BINARY: the folder's name IS a
    /// `vike_backtest::harness::registry::STRATEGIES` entry (carried in [`UserStrategy::name`] in
    /// the registry's own spelling), and the folder holds nothing but presets for it.
    Native,
}

/// One user-authored strategy folder, loaded and — for the [`StrategyBody::Rhai`] half — PROVEN to
/// compile.
#[derive(Debug, Clone, PartialEq)]
pub struct UserStrategy {
    /// The folder name. For a [`StrategyBody::Rhai`] strategy it is also the entry file's stem (the
    /// parent module's rule 1); for a [`StrategyBody::Native`] one it is the REGISTRY name, in the
    /// registry's own spelling, so it resolves through `strategy_by_name` whatever case the folder
    /// was created with.
    pub name: String,
    /// The folder itself, so a caller can open it, watch it, or delete the whole strategy.
    pub dir: PathBuf,
    /// Where this strategy's code lives.
    pub body: StrategyBody,
    /// Every readable `.toml` beside the entry and under `presets/`, in name order.
    pub presets: Vec<Preset>,
}

impl UserStrategy {
    /// The runnable spec this strategy denotes — the bridge to
    /// `crates/vike-studio-core/src/run.rs`'s `run_slice`.
    ///
    /// The Rhai arm is the same value the scan already built once to prove the script compiles. The
    /// native arm is the registry name with an EMPTY params table, which is the shape every
    /// registry `from_params` reader tolerates; a preset's params are applied through
    /// [`UserStrategy::run_with`], not here.
    pub fn spec(&self) -> StrategySpec {
        match &self.body {
            StrategyBody::Rhai { source, .. } => StrategySpec::Rhai(source.clone()),
            StrategyBody::Native => StrategySpec::native_default(&self.name),
        }
    }

    /// `<dir>/<name>.rhai`, or `None` for a built-in strategy (whose code is in the binary and has
    /// no file here at all).
    pub fn entry(&self) -> Option<&Path> {
        match &self.body {
            StrategyBody::Rhai { entry, .. } => Some(entry),
            StrategyBody::Native => None,
        }
    }

    /// The entry file's text, or `None` for a built-in strategy — see [`UserStrategy::entry`].
    pub fn source(&self) -> Option<&str> {
        match &self.body {
            StrategyBody::Rhai { source, .. } => Some(source),
            StrategyBody::Native => None,
        }
    }

    /// Is this a built-in (registry) strategy that the folder only supplies PRESETS for?
    pub fn is_native(&self) -> bool {
        matches!(self.body, StrategyBody::Native)
    }
}

/// One preset: a named parameter set belonging to exactly one strategy.
///
/// `params` is a free-form [`Params`] table for the same reason `crates/vike-studio-core/src/spec.rs`'s
/// `StrategySpec` carries one — there is no `ParamSpec` seam in `vike-backtest` to validate
/// against, so the params table IS the vocabulary. A Rhai script's knobs are its `param(name,
/// default)` calls (`vike_script::discover_params`); a preset simply supplies values for them.
///
/// ⚠ **The table is FLAT: a preset IS the params table**, the one a profile's `[strategy.params]`
/// receives key for key. [`check_preset_shape`] is where that is enforced, and its doc carries the
/// argument.
#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    /// The file stem — `fast.toml` is the preset `fast`.
    pub name: String,
    pub path: PathBuf,
    /// The parsed table. A TOML DOCUMENT is always a table, and a `Params` cannot be anything else.
    pub params: Params,
}

/// What one scan found: the strategies, and everything it could not use.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadReport {
    /// The root that was scanned — carried so the rendered log says WHICH tree it is about, which
    /// matters the moment `VIKE_USER_DATA_DIR` is in play. [`load_user_strategies`] reports the
    /// `strategies/` directory it was given, not either of the two trees under it.
    pub root: PathBuf,
    /// Loaded strategies, in scan order: the `rhai/` tree in name order, then the `rust/` one.
    pub strategies: Vec<UserStrategy>,
    pub diagnostics: Vec<LoadDiagnostic>,
}

impl LoadReport {
    /// How many diagnostics cost the user a strategy.
    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.severity() == Severity::Error).count()
    }

    /// How many diagnostics left the strategy runnable.
    pub fn warning_count(&self) -> usize {
        self.diagnostics.iter().filter(|d| d.severity() == Severity::Warning).count()
    }

    /// The loaded strategy called `name`, matched case-INSENSITIVELY for the same reason
    /// [`RHAI_EXT`] is: the folder name came off a filesystem that may or may not fold case, and a
    /// caller typing `SMA-Cross` means the one strategy of that name.
    pub fn strategy(&self, name: &str) -> Option<&UserStrategy> {
        self.strategies.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }
}

/// Which tree a folder was found in, and therefore what it MEANS — see this module's doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tree {
    /// `strategies/rhai` — the code is in the file.
    Rhai,
    /// `strategies/rust` — the code is in the binary (or in a `.rs` cargo already consumed).
    Rust,
}

impl Tree {
    /// The entry-file extension a STRAY file in this tree's root would have.
    fn ext(self) -> &'static str {
        match self {
            Tree::Rhai => RHAI_EXT,
            Tree::Rust => RUST_EXT,
        }
    }
}

/// Scan `<project>/user_data/strategies` — BOTH trees, `rhai/` then `rust/` — from
/// `crates/vike-model/src/paths/state_path.rs`'s `user_rhai_strategies_dir` / `user_rust_strategies_dir`
/// minus their leaf.
///
/// ONE report over both, because a strategy is addressed by NAME (that is what
/// `crates/vike-studio-core/src/user_strategies/preset.rs`'s `resolve_preset` looks up), so the two
/// trees share one namespace: a name claimed in `rhai/` wins, and the `rust/` folder claiming it
/// too is reported as a [`LoadDiagnostic::DuplicateName`] rather than silently shadowed.
///
/// Never fails, for the same reason [`load_rhai_strategies`] does not — every failure is a row.
pub fn load_user_strategies(strategies_root: &Path) -> LoadReport {
    let mut report = LoadReport {
        root: strategies_root.to_path_buf(),
        strategies: Vec::new(),
        diagnostics: Vec::new(),
    };
    // Case-folded and shared across BOTH trees: the collision that matters is the one that leaves
    // a NAME ambiguous, and that is exactly what a caller resolves a preset through.
    let mut claimed: BTreeMap<String, PathBuf> = BTreeMap::new();
    scan_tree(&strategies_root.join(RHAI_SUBDIR), Tree::Rhai, &mut claimed, &mut report);
    scan_tree(&strategies_root.join(RUST_SUBDIR), Tree::Rust, &mut claimed, &mut report);
    report
}

/// Scan `root` — `<project>/user_data/strategies/rhai`, from
/// `crates/vike-model/src/paths/state_path.rs`'s `user_rhai_strategies_dir`.
///
/// The Rhai tree ALONE. [`load_user_strategies`] is the both-trees scan, and is what a caller that
/// wants to resolve a preset by name should use — presets for BUILT-IN strategies live in the
/// `rust/` tree and this function cannot see them.
///
/// Never fails: everything that could be an `Err` is a named row in
/// [`LoadReport::diagnostics`] instead, because a loader that returns `Err` on the first broken
/// folder loses the nineteen good ones behind it. An absent root is an EMPTY report with no
/// diagnostic (see the module doc).
///
/// Deterministic: directory order is whatever the OS feels like, so entries are sorted by name
/// before anything is loaded. That is what makes "which duplicate won" a rule rather than a race.
pub fn load_rhai_strategies(root: &Path) -> LoadReport {
    let mut report =
        LoadReport { root: root.to_path_buf(), strategies: Vec::new(), diagnostics: Vec::new() };
    let mut claimed: BTreeMap<String, PathBuf> = BTreeMap::new();
    scan_tree(root, Tree::Rhai, &mut claimed, &mut report);
    report
}

/// **A preset IS the params table** — FLAT, the keys a profile's `[strategy.params]` receives one
/// for one. This is where that is enforced, and it refuses the two shapes that would otherwise be
/// accepted and then quietly do nothing.
///
/// * **A `[params]` wrapper.** `{ params = { fast = 5 } }` merged into `[strategy.params]` gives a
///   strategy a key called `params` that no reader looks at, so every knob silently keeps its
///   default — the worst outcome available, because the user did everything else right. The
///   alternative, silently unwrapping a lone `params` key, would make TWO shapes legal and pick
///   between them by an invisible rule; this workspace refuses that trade wherever it has come up.
///   Rejecting is also what makes the FLAT shape checkable: `[strategy.params]` is flat by
///   definition.
/// * **A `src` key** — see [`RESERVED_SRC_KEY`].
///
/// Only the EXACT wrapper shape is refused (a lone `params` key whose value is a table), so a
/// legitimately nested knob is untouched: `funding_carry` reads a `[venues]` table straight out of
/// `[strategy.params]` (`crates/vike-strategy/src/registry.rs`'s `controller_harness` — that helper
/// moved DOWN with the portable half of the registry), and
/// a preset supplying one is correct.
fn check_preset_shape(table: &Params) -> Result<(), String> {
    // "A preset is a TABLE" is the type now: a `Params` cannot be a bare scalar, so the read that
    // produced it (`toml::from_str::<Params>` on a document) has already refused anything else.
    if table.len() == 1 && table.get(PARAMS_WRAPPER_KEY).is_some_and(ParamValue::is_table) {
        return Err(format!(
            "it wraps its knobs in a [{PARAMS_WRAPPER_KEY}] table, so the strategy would receive \
             one parameter called '{PARAMS_WRAPPER_KEY}' that nothing reads and every knob would \
             keep its default. A preset IS the params table: delete the [{PARAMS_WRAPPER_KEY}] \
             header and leave the keys at the top level"
        ));
    }
    if table.contains_key(RESERVED_SRC_KEY) {
        return Err(format!(
            "it defines '{RESERVED_SRC_KEY}', which is the strategy's SOURCE rather than one of \
             its knobs — that is set by the entry file (or by `vike-cli backtest run --script`). \
             Delete the '{RESERVED_SRC_KEY}' key"
        ));
    }
    Ok(())
}

/// Case-insensitive extension test — see [`RHAI_EXT`].
pub(crate) fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests;
