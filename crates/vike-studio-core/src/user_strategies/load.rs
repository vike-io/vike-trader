//! The directory scan: `<project>/user_data/strategies/` → loaded strategies + named diagnostics.
//!
//! PURE in the sense that matters here — it READS the tree and returns what it found, and writes
//! nothing at all (not even the log it renders). See this module's parent for the layout rules and
//! for why a silent skip is the one failure mode this loader may not have.
//!
//! # Three decisions worth stating, because all three look like details and are not
//!
//! **An ABSENT root is not an error; an unreadable one is.** A fresh install has no `user_data/`
//! at all, and `crates/vike-model/src/state_path.rs`'s `project_user_data_dir` returns the path
//! where the directory BELONGS rather than one it found — so "nothing there" is the ordinary state
//! and must stay silent. A root that EXISTS and cannot be listed is the opposite: a permissions
//! bug wearing the "not configured yet" answer looks exactly like a correct fresh install while
//! every strategy silently vanishes. That is the same distinction the credential store draws
//! (`crates/vike-secrets/src/dotenv.rs`), for the same reason, and it is why
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
//! NAME — and the folder holds nothing but PRESETS for it. That is exactly the shape
//! `crates/vike-studio-core/src/user_strategies/migrate.rs` writes for every migrated native row,
//! and its module doc warned in advance that a rust-side loader copying the `rhai` rule verbatim
//! would report every one of them as an error. So the entry-file probe is a property of the TREE,
//! not of the loader: [`StrategyBody`] is the type that says which one a loaded folder is.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vike_model::state_path::{RHAI_SUBDIR, RUST_SUBDIR};

use crate::{build_strategy, StrategySpec};

/// The `rhai` entry-file extension. Compared case-INSENSITIVELY: a Windows editor that saved
/// `SMA.RHAI` wrote a perfectly good script, and the platform this workspace is developed on folds
/// case.
pub(crate) const RHAI_EXT: &str = "rhai";

/// The `rust` entry-file extension, compared the same way as [`RHAI_EXT`].
///
/// ⚠ It is used ONLY to recognise a stray `.rs` loose in the root. A `.rs` inside a strategy folder
/// is never read here: it is an input to `cargo`, consumed long before this scan runs (see
/// `crates/vike-cli/src/cmd/init/content.rs`'s `RUST_README`, whose first line is about exactly
/// that).
pub(crate) const RUST_EXT: &str = "rs";

/// The TOML preset extension, compared the same way as [`RHAI_EXT`].
const TOML_EXT: &str = "toml";

/// The optional preset sub-directory — the filed position, honoured beside the flat one.
const PRESETS_SUBDIR: &str = "presets";

/// The one key a preset may NOT define: it is the strategy's SOURCE, not one of its knobs.
///
/// `crates/vike-backtest/src/harness/registry.rs`'s `rhai_overrides` already treats it as reserved
/// (it is filtered out of the `param()` override map), and `crates/vike-cli/src/cmd/backtest.rs`'s
/// `inject_script_src` is what legitimately sets it. A preset that carries one would let a params
/// file smuggle a whole script past `--script`, so it is refused rather than resolved by a
/// precedence rule nobody can see.
const RESERVED_SRC_KEY: &str = "src";

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
/// `params` is a `toml::Value` table for the same reason `crates/vike-studio-core/src/spec.rs`'s
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
    /// The parsed table. A TOML DOCUMENT is always a table, so this is never a bare scalar.
    pub params: toml::Value,
}

/// Whether a diagnostic stopped a strategy from loading.
///
/// Two levels, and the line between them is exactly "did the user lose a strategy": a broken
/// preset leaves the strategy runnable with its other presets, everything else means a folder the
/// user believes is installed is not.
///
/// ⚠ Shared with `crates/vike-studio-core/src/listing.rs`, whose rows ask that same question about
/// a RUN or a STUDY. One severity vocabulary across the `user_data/` scans rather than a
/// two-variant enum per tree: the levels mean the same thing in both, and two copies could only
/// ever drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Nothing loaded from this folder/file.
    Error,
    /// The strategy loaded; something beside it did not.
    Warning,
}

/// Everything the scan could not use, NAMED — one variant per mistake a person actually makes.
///
/// Each carries the path it is about and enough context to act: [`LoadDiagnostic::MissingEntry`]
/// lists the scripts it DID find (so "you named it `strat.rhai` inside `sma-cross/`" is visible
/// without opening the folder), [`LoadDiagnostic::DuplicateName`] names both sides and says which
/// one won.
#[derive(Debug, Clone, PartialEq)]
pub enum LoadDiagnostic {
    /// The strategies root exists but could not be listed. Deliberately distinct from "no root",
    /// which is silent — see this module's doc.
    RootUnreadable { root: PathBuf, error: String },

    /// A `rhai/` folder holds no `<name>.rhai`. `scripts` is every `.rhai` file that IS there,
    /// which is what makes the common misnaming self-explaining.
    MissingEntry { name: String, dir: PathBuf, scripts: Vec<String> },

    /// Presets for a BUILT-IN strategy, filed in the `rhai/` tree.
    ///
    /// The folder's name is a registry strategy, so it needs no script — but it is in the tree
    /// where a script is exactly what a folder means. [`LoadDiagnostic::MissingEntry`]'s "rename
    /// the script to match its folder" is the WRONG advice here (there is no script to rename and
    /// none is wanted), which is why this is its own row rather than a wording tweak: the fix is to
    /// move the folder to `strategies/rust/`, where `migrate.rs` already files native presets.
    NativePresetsMisfiled { name: String, dir: PathBuf, expected_dir: PathBuf },

    /// A `rust/` folder holding presets whose name resolves to no built-in strategy.
    ///
    /// The presets cannot reach anything: `strategy_by_name` is a lookup by NAME, so a folder
    /// naming nothing in the registry has no strategy for its params to configure. A folder with no
    /// presets is NOT this — that is an unregistered source strategy, which is the ordinary state
    /// of the shipped `my_experiment/` template and produces no diagnostic at all.
    UnregisteredNative { name: String, dir: PathBuf, presets: Vec<String> },

    /// The entry file is there but could not be read as text — permissions, or not UTF-8.
    UnreadableEntry { name: String, path: PathBuf, error: String },

    /// `vike_script::RhaiStrategy::compile` rejected it, via
    /// `crates/vike-studio-core/src/run.rs`'s `build_strategy`. `error` is the script author's
    /// message, verbatim.
    CompileFailed { name: String, path: PathBuf, error: String },

    /// A `.toml` beside a strategy could not be read, parsed, or used as a params table. The
    /// strategy still loads — [`Severity::Warning`].
    BadPreset { name: String, path: PathBuf, error: String },

    /// The same preset name in BOTH positions (flat and `presets/`). The flat one wins, because
    /// it is the documented primary position; the other is ignored rather than silently merged.
    ShadowedPreset { name: String, preset: String, kept: PathBuf, ignored: PathBuf },

    /// Two folders resolve to the same strategy name.
    ///
    /// ⚠ Reachable two ways. Case folding is a property of the filesystem, not of the name:
    /// `SmaCross/` and `smacross/` cannot coexist on Windows or macOS but happily do on Linux, so a
    /// tree that is fine on the box it was authored on silently loses a strategy on the box it is
    /// copied to. And across the two TREES the name is the only handle a caller has —
    /// `resolve_preset` looks a strategy up by name — so `rhai/grid/` and `rust/grid/` are a
    /// genuine collision even though the filesystem is happy.
    DuplicateName { name: String, kept: PathBuf, ignored: PathBuf },

    /// A script directly in a strategies root instead of in a folder of its own — the single most
    /// likely first-time mistake, and the one a silent skip makes unanswerable.
    StrayScript { path: PathBuf },
}

impl LoadDiagnostic {
    /// Did this cost the user a strategy?
    pub fn severity(&self) -> Severity {
        match self {
            LoadDiagnostic::BadPreset { .. } | LoadDiagnostic::ShadowedPreset { .. } => {
                Severity::Warning
            }
            _ => Severity::Error,
        }
    }

    /// The strategy this is about, when there is one. `None` for the two whole-tree rows
    /// ([`LoadDiagnostic::RootUnreadable`], [`LoadDiagnostic::StrayScript`]) — a stray script has
    /// no strategy yet, which is the whole point of the diagnostic.
    pub fn strategy(&self) -> Option<&str> {
        match self {
            LoadDiagnostic::MissingEntry { name, .. }
            | LoadDiagnostic::NativePresetsMisfiled { name, .. }
            | LoadDiagnostic::UnregisteredNative { name, .. }
            | LoadDiagnostic::UnreadableEntry { name, .. }
            | LoadDiagnostic::CompileFailed { name, .. }
            | LoadDiagnostic::BadPreset { name, .. }
            | LoadDiagnostic::ShadowedPreset { name, .. }
            | LoadDiagnostic::DuplicateName { name, .. } => Some(name),
            LoadDiagnostic::RootUnreadable { .. } | LoadDiagnostic::StrayScript { .. } => None,
        }
    }
}

impl std::fmt::Display for LoadDiagnostic {
    /// ONE line each, and every line names the fix. These are read by a person in a log file who
    /// has already tried the obvious thing once.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadDiagnostic::RootUnreadable { root, error } => write!(
                f,
                "{}: the strategies directory exists but cannot be read ({error}) — no user \
                 strategy was loaded; fix its permissions",
                root.display()
            ),
            LoadDiagnostic::MissingEntry { name, dir, scripts } => {
                let found = if scripts.is_empty() {
                    "the folder holds no .rhai file at all".to_string()
                } else {
                    format!("found: {}", scripts.join(", "))
                };
                write!(
                    f,
                    "{name}: no entry file — expected {}, {found}. Rename the script to match its \
                     folder, or the folder to match the script.",
                    dir.join(format!("{name}.{RHAI_EXT}")).display()
                )
            }
            LoadDiagnostic::NativePresetsMisfiled { name, dir, expected_dir } => write!(
                f,
                "{name}: '{name}' is a BUILT-IN strategy — its code is in the binary, so {} needs \
                 no script, but it is filed under the tree for authored ones. Move it to {}, where \
                 presets for built-in strategies live.",
                dir.display(),
                expected_dir.display()
            ),
            LoadDiagnostic::UnregisteredNative { name, dir, presets } => write!(
                f,
                "{name}: {} holds {} preset(s) ({}) for a strategy that does not exist — nothing \
                 named '{name}' is in the built-in registry. Rename the folder to the built-in it \
                 presets, or register it in \
                 crates/vike-backtest/src/harness/registry.rs's strategy_by_name.",
                dir.display(),
                presets.len(),
                presets.join(", ")
            ),
            LoadDiagnostic::UnreadableEntry { name, path, error } => write!(
                f,
                "{name}: {} cannot be read ({error}) — check its permissions and that it is UTF-8",
                path.display()
            ),
            LoadDiagnostic::CompileFailed { name, path, error } => {
                write!(f, "{name}: {} failed to compile — {error}", path.display())
            }
            LoadDiagnostic::BadPreset { name, path, error } => write!(
                f,
                "{name}: preset {} ignored — {error}. The strategy loaded without it.",
                path.display()
            ),
            LoadDiagnostic::ShadowedPreset { name, preset, kept, ignored } => write!(
                f,
                "{name}: preset '{preset}' is defined twice — kept {}, ignored {}. Delete one.",
                kept.display(),
                ignored.display()
            ),
            LoadDiagnostic::DuplicateName { name, kept, ignored } => write!(
                f,
                "{name}: duplicate strategy name — kept {}, ignored {}. A strategy is addressed by \
                 NAME, so two folders claiming one name (in either tree, or differing only by \
                 letter case, which Windows and macOS cannot even hold) leave it ambiguous; rename \
                 one.",
                kept.display(),
                ignored.display()
            ),
            LoadDiagnostic::StrayScript { path } => write!(
                f,
                "{}: a script directly in the strategies root is NOT loaded — move it to {}",
                path.display(),
                stray_target(path).display()
            ),
        }
    }
}

/// Where a stray root-level script BELONGS: `<root>/<stem>/<stem>.<ext>`, keeping its own
/// extension. Split out so the message above states the fix as a path the user can copy rather than
/// as a sentence about a convention.
pub(crate) fn stray_target(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = path.parent().unwrap_or(Path::new(""));
    parent.join(&stem).join(format!("{stem}.{ext}"))
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
/// `crates/vike-model/src/state_path.rs`'s `user_rhai_strategies_dir` / `user_rust_strategies_dir`
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
/// `crates/vike-model/src/state_path.rs`'s `user_rhai_strategies_dir`.
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

/// One tree's worth of scanning, appending to a shared `claimed` namespace and one `report`.
fn scan_tree(
    root: &Path,
    tree: Tree,
    claimed: &mut BTreeMap<String, PathBuf>,
    report: &mut LoadReport,
) {
    let listing = match std::fs::read_dir(root) {
        Ok(listing) => listing,
        // The ordinary un-configured state: no `user_data/` yet, or no `strategies/<tree>` in it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            report.diagnostics.push(LoadDiagnostic::RootUnreadable {
                root: root.to_path_buf(),
                error: e.to_string(),
            });
            return;
        }
    };

    let mut folders: Vec<(String, PathBuf)> = Vec::new();
    let mut strays: Vec<PathBuf> = Vec::new();
    // A per-ENTRY error carries no name to attach a diagnostic to — the `DirEntry` itself failed
    // to materialise, so we do not even know which file it was. It is an OS-level anomaly with no
    // actionable text, unlike the root failure above, which names the directory.
    for entry in listing.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        // Dot-entries are tool droppings (`.git`, `.DS_Store`), not user content — skipping them
        // is a rule, not a silent loss.
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            folders.push((name, path));
        } else if has_ext(&path, tree.ext()) {
            strays.push(path);
        }
    }
    folders.sort();
    strays.sort();

    for path in strays {
        report.diagnostics.push(LoadDiagnostic::StrayScript { path });
    }

    for (name, dir) in folders {
        let key = name.to_lowercase();
        if let Some(kept) = claimed.get(&key) {
            report.diagnostics.push(LoadDiagnostic::DuplicateName {
                name,
                kept: kept.clone(),
                ignored: dir,
            });
            continue;
        }
        // A folder that loaded NOTHING claims no name: a later folder with the same name is a real
        // strategy, not a duplicate of a failure.
        if let Some(loaded) = load_one(&dir, &name, tree, &mut report.diagnostics) {
            claimed.insert(key, dir);
            report.strategies.push(loaded);
        }
    }
}

/// Load ONE folder, appending a diagnostic for whatever stopped it. `None` means nothing loaded —
/// and in that case at least one diagnostic was pushed, WITH one deliberate exception named in the
/// `Rust` arm below.
fn load_one(
    dir: &Path,
    name: &str,
    tree: Tree,
    diags: &mut Vec<LoadDiagnostic>,
) -> Option<UserStrategy> {
    match tree {
        Tree::Rhai => load_rhai_folder(dir, name, diags),
        Tree::Rust => load_rust_folder(dir, name, diags),
    }
}

/// A `rhai/` folder: the entry file IS the strategy, so its absence is a real loss.
fn load_rhai_folder(
    dir: &Path,
    name: &str,
    diags: &mut Vec<LoadDiagnostic>,
) -> Option<UserStrategy> {
    let entry = dir.join(format!("{name}.{RHAI_EXT}"));
    if !entry.is_file() {
        // A folder named after a BUILT-IN strategy is not a broken script folder — it is a preset
        // folder in the wrong tree, and "rename the script" is advice about a script that should
        // not exist. Say what it actually is. (Only when it holds presets: an empty folder named
        // `grid/` is more likely someone starting a Rhai strategy than misfiling nothing.)
        if let Some(registry) = registry_name(name) {
            let presets = preset_names(dir);
            if !presets.is_empty() {
                diags.push(LoadDiagnostic::NativePresetsMisfiled {
                    name: registry.to_string(),
                    dir: dir.to_path_buf(),
                    expected_dir: rust_twin(dir, registry),
                });
                return None;
            }
        }
        diags.push(LoadDiagnostic::MissingEntry {
            name: name.to_string(),
            dir: dir.to_path_buf(),
            scripts: entries_in(dir, RHAI_EXT),
        });
        return None;
    }

    let source = match std::fs::read_to_string(&entry) {
        Ok(text) => text,
        Err(e) => {
            diags.push(LoadDiagnostic::UnreadableEntry {
                name: name.to_string(),
                path: entry,
                error: e.to_string(),
            });
            return None;
        }
    };

    // The same call a Run makes — see the module doc on why this is not `RhaiStrategy::compile`
    // reached for directly. The built strategy is dropped: what we wanted was the verdict.
    if let Err(e) = build_strategy(&StrategySpec::Rhai(source.clone())) {
        diags.push(LoadDiagnostic::CompileFailed {
            name: name.to_string(),
            path: entry,
            error: e.to_string(),
        });
        return None;
    }

    let presets = collect_presets(dir, name, diags);
    Some(UserStrategy {
        name: name.to_string(),
        dir: dir.to_path_buf(),
        body: StrategyBody::Rhai { entry, source },
        presets,
    })
}

/// A `rust/` folder: the code is in the BINARY, so there is no entry file to miss.
///
/// ⚠ The one place this loader returns `None` WITHOUT a diagnostic, and it is deliberate: a folder
/// naming no built-in strategy and holding no presets is a source-checkout Rust strategy that has
/// not been registered yet — which is the shipped `my_experiment/` template's exact state, and
/// `crates/vike-cli/src/cmd/init/content.rs`'s `RUST_README` already says in its first line that
/// nothing reads that tree at runtime. Nothing was skipped, because there was nothing here for a
/// runtime scan to load. The moment the folder holds PRESETS that changes — they are addressed by
/// a name that resolves to nothing, and that IS a loss.
fn load_rust_folder(
    dir: &Path,
    name: &str,
    diags: &mut Vec<LoadDiagnostic>,
) -> Option<UserStrategy> {
    let Some(registry) = registry_name(name) else {
        let presets = preset_names(dir);
        if !presets.is_empty() {
            diags.push(LoadDiagnostic::UnregisteredNative {
                name: name.to_string(),
                dir: dir.to_path_buf(),
                presets,
            });
        }
        return None;
    };
    // The REGISTRY's spelling, not the folder's: `strategy_by_name` matches exactly, and a folder
    // created as `Buy_Hold/` on a case-folding filesystem still means `buy_hold`.
    let presets = collect_presets(dir, registry, diags);
    Some(UserStrategy {
        name: registry.to_string(),
        dir: dir.to_path_buf(),
        body: StrategyBody::Native,
        presets,
    })
}

/// The built-in strategy `folder` names, in the REGISTRY's own spelling — or `None` if it names
/// none.
///
/// Case-insensitive for the same reason [`RHAI_EXT`] is: the folder name came off a filesystem, and
/// a Windows checkout that folded `Grid/` to `grid/` (or the other way) has not changed which
/// strategy the user meant. The roster is `crates/vike-studio-core/src/spec.rs`'s
/// `native_strategies`, which is `vike_backtest::harness::registry::STRATEGIES` itself — so a
/// strategy added to the registry becomes a legal preset folder for free.
fn registry_name(folder: &str) -> Option<&'static str> {
    crate::spec::native_strategies().iter().copied().find(|n| n.eq_ignore_ascii_case(folder))
}

/// Where a misfiled `rhai/<name>/` preset folder belongs: the same name under the sibling `rust/`
/// tree. Falls back to a bare `rust/<name>` when `dir` has no grandparent to hang it off, which
/// only happens for a hand-constructed root — the message is still readable.
fn rust_twin(dir: &Path, name: &str) -> PathBuf {
    dir.parent()
        .and_then(Path::parent)
        .map(|strategies| strategies.join(RUST_SUBDIR).join(name))
        .unwrap_or_else(|| Path::new(RUST_SUBDIR).join(name))
}

/// Every file name with extension `ext` in `dir`, sorted — what [`LoadDiagnostic::MissingEntry`]
/// shows so a misnaming explains itself.
fn entries_in(dir: &Path, ext: &str) -> Vec<String> {
    let Ok(listing) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<String> = listing
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && has_ext(p, ext))
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    found.sort();
    found
}

/// Every preset STEM in `dir` and its `presets/` sub-directory, sorted — the "what was found"
/// half of the two diagnostics that report a folder whose presets reach nothing.
fn preset_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = toml_files(dir)
        .into_iter()
        .chain(toml_files(&dir.join(PRESETS_SUBDIR)))
        .filter_map(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The presets for one strategy: the flat position first, then `presets/`.
///
/// The flat position WINS a name clash, because it is the documented primary one — and the loser
/// is reported rather than dropped, since two files claiming one preset name is a mistake in the
/// tree, not a preference to be silently resolved.
fn collect_presets(dir: &Path, strategy: &str, diags: &mut Vec<LoadDiagnostic>) -> Vec<Preset> {
    let mut presets: Vec<Preset> = Vec::new();
    let mut claimed: BTreeMap<String, PathBuf> = BTreeMap::new();

    for path in toml_files(dir).into_iter().chain(toml_files(&dir.join(PRESETS_SUBDIR))) {
        let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let key = name.to_lowercase();
        if let Some(kept) = claimed.get(&key) {
            diags.push(LoadDiagnostic::ShadowedPreset {
                name: strategy.to_string(),
                preset: name,
                kept: kept.clone(),
                ignored: path,
            });
            continue;
        }
        match read_preset(&path) {
            Ok(params) => {
                claimed.insert(key, path.clone());
                presets.push(Preset { name, path, params });
            }
            Err(error) => {
                diags.push(LoadDiagnostic::BadPreset { name: strategy.to_string(), path, error })
            }
        }
    }
    presets
}

/// Read + parse + shape-check one preset. All three failure modes collapse into one message on
/// purpose: to the user they are the same event — "this preset is not usable, here is why" — and
/// splitting them would buy diagnostic variants that read identically.
fn read_preset(path: &Path) -> Result<toml::Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let value = toml::from_str::<toml::Value>(&text).map_err(|e| e.to_string())?;
    check_preset_shape(&value)?;
    Ok(value)
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
///   Rejecting is also what makes the FLAT shape checkable: `migrate.rs`'s `preset_toml` already
///   writes flat, and `[strategy.params]` is flat by definition.
/// * **A `src` key** — see [`RESERVED_SRC_KEY`].
///
/// Only the EXACT wrapper shape is refused (a lone `params` key whose value is a table), so a
/// legitimately nested knob is untouched: `funding_carry` reads a `[venues]` table straight out of
/// `[strategy.params]` (`crates/vike-strategy/src/registry.rs`'s `controller_harness` — that helper
/// moved DOWN with the portable half of the registry), and
/// a preset supplying one is correct.
fn check_preset_shape(value: &toml::Value) -> Result<(), String> {
    let Some(table) = value.as_table() else {
        // Unreachable through `toml::from_str::<Value>` on a document (a TOML document IS a table),
        // but this function is the shape authority and must not assume its only caller.
        return Err("a preset must be a table of parameters".to_string());
    };
    if table.len() == 1 && table.get(PARAMS_WRAPPER_KEY).is_some_and(toml::Value::is_table) {
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
             its knobs — that is set by the entry file (or by `vike-cli backtest --script`). \
             Delete the '{RESERVED_SRC_KEY}' key"
        ));
    }
    Ok(())
}

/// Every `.toml` file directly in `dir`, sorted by name. A missing directory is an empty list —
/// `presets/` is optional by design.
fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(listing) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<PathBuf> = listing
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && has_ext(p, TOML_EXT))
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    found.sort();
    found
}

/// Case-insensitive extension test — see [`RHAI_EXT`].
pub(crate) fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Render one scan as the text a caller appends to `user_data/logs/compile.log`
/// (`crates/vike-model/src/state_path.rs`'s `user_logs_dir`).
///
/// **Passes are logged too, not only failures.** That directory's own contract says "every
/// strategy load, pass and fail", and the reason is that a user debugging "why is my strategy not
/// in the list" needs to see the strategies that ARE — an absent name is the answer, and it is
/// only visible against the present ones.
///
/// `stamp` is a CALLER-SUPPLIED time string and this function reads no clock: a library that
/// stamps its own output cannot be tested for its output, and ambient time is the defect
/// `crates/vike-ops/tests/clock_pin.rs` ratchets down elsewhere in this tree. One stamp per LINE,
/// not per block, because the file is appended to once per app start and `grep <name>` must still
/// answer "when".
///
/// Ends with a newline, so appending twice cannot splice two runs onto one line.
pub fn render_compile_log(report: &LoadReport, stamp: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("{stamp} info  scanning {}\n", report.root.display()));
    for s in &report.strategies {
        let presets = match s.presets.len() {
            0 => String::new(),
            1 => format!(", 1 preset ({})", s.presets[0].name),
            n => format!(
                ", {n} presets ({})",
                s.presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        };
        // A built-in strategy has no file to name; saying so is the point, since the folder's
        // presets are all there is to see.
        let what = match s.entry() {
            Some(entry) => entry.file_name().unwrap_or_default().to_string_lossy().into_owned(),
            None => "built-in".to_string(),
        };
        out.push_str(&format!("{stamp} ok    {} [{what}{presets}]\n", s.name));
    }
    for d in &report.diagnostics {
        let level = match d.severity() {
            Severity::Error => "ERROR",
            Severity::Warning => "WARN ",
        };
        out.push_str(&format!("{stamp} {level} {d}\n"));
    }
    out.push_str(&format!(
        "{stamp} info  {} loaded, {} error(s), {} warning(s)\n",
        report.strategies.len(),
        report.error_count(),
        report.warning_count()
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal script that compiles and defines the one hook the engine calls.
    const OK_SCRIPT: &str = "fn on_bar() {}\n";

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Build `<root>/<name>/<name>.rhai` with `src`.
    fn strategy_dir(root: &Path, name: &str, src: &str) -> PathBuf {
        let dir = root.join(name);
        write(&dir.join(format!("{name}.rhai")), src);
        dir
    }

    /// The happy path, INCLUDING presets in BOTH honoured positions — flat beside the entry and
    /// filed under `presets/`. Both are the strategy's, so both load, in one name-ordered list.
    #[test]
    fn a_folder_loads_with_its_source_and_presets_from_both_positions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
        write(&dir.join("fast.toml"), "fast = 5\nslow = 20\n");
        write(&dir.join("presets").join("slow.toml"), "fast = 50\nslow = 200\n");

        let report = load_rhai_strategies(root);

        assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
        assert_eq!(report.strategies.len(), 1);
        let s = &report.strategies[0];
        assert_eq!(s.name, "sma-cross");
        assert_eq!(s.source(), Some(OK_SCRIPT), "the entry file's text must be verbatim");
        assert_eq!(s.entry(), Some(dir.join("sma-cross.rhai").as_path()));
        assert_eq!(
            s.presets.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["fast", "slow"],
            "both positions are honoured, in name order"
        );
        assert_eq!(s.presets[0].params.get("fast").and_then(toml::Value::as_integer), Some(5));
        assert_eq!(s.presets[1].params.get("slow").and_then(toml::Value::as_integer), Some(200));
        assert_eq!(s.spec(), StrategySpec::Rhai(OK_SCRIPT.to_string()));
    }

    /// THE naming rule: the entry file is the one matching the folder. A folder whose script is
    /// named anything else is NOT loaded — and the diagnostic lists what it found, so the user can
    /// see the mismatch without opening the folder.
    #[test]
    fn an_entry_file_that_does_not_match_its_folder_is_named_not_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join("sma-cross").join("strat.rhai"), OK_SCRIPT);

        let report = load_rhai_strategies(root);

        assert!(report.strategies.is_empty());
        assert_eq!(report.diagnostics.len(), 1);
        match &report.diagnostics[0] {
            LoadDiagnostic::MissingEntry { name, scripts, .. } => {
                assert_eq!(name, "sma-cross");
                assert_eq!(scripts, &vec!["strat.rhai".to_string()]);
            }
            other => panic!("expected MissingEntry, got {other:?}"),
        }
        let msg = report.diagnostics[0].to_string();
        assert!(msg.contains("sma-cross.rhai"), "the message must name the expected path: {msg}");
        assert!(msg.contains("strat.rhai"), "…and what was found instead: {msg}");
        assert_eq!(report.diagnostics[0].severity(), Severity::Error);
    }

    /// A folder holding the entry file PLUS other scripts is fine — the extras are the script's
    /// own helpers. This is the property the folder/entry naming rule exists to buy, so it is
    /// pinned: extra scripts must produce NO diagnostic.
    #[test]
    fn extra_scripts_beside_the_entry_are_not_a_diagnostic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
        write(&dir.join("helpers.rhai"), "fn helper() {}\n");

        let report = load_rhai_strategies(root);

        assert_eq!(report.strategies.len(), 1);
        assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
    }

    /// A script that does not compile is reported with the SCRIPT AUTHOR'S error, not a generic
    /// "failed to load" — the message is the whole value of the diagnostic.
    #[test]
    fn a_script_that_fails_to_compile_names_the_folder_and_the_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        strategy_dir(root, "broken", "fn on_bar( {\n");

        let report = load_rhai_strategies(root);

        assert!(report.strategies.is_empty());
        assert_eq!(report.diagnostics.len(), 1);
        match &report.diagnostics[0] {
            LoadDiagnostic::CompileFailed { name, error, .. } => {
                assert_eq!(name, "broken");
                assert!(!error.is_empty(), "the rhai error must be carried through");
            }
            other => panic!("expected CompileFailed, got {other:?}"),
        }
    }

    /// Two folders differing only by case: legal on Linux, impossible on Windows/macOS. One wins
    /// DETERMINISTICALLY (name order) and the other is named — a tree that quietly loads a
    /// different strategy depending on the machine is the failure this catches.
    #[test]
    fn two_folders_differing_only_by_case_are_reported_as_a_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        strategy_dir(root, "SmaCross", OK_SCRIPT);
        let second = root.join("smacross");
        // On a case-folding filesystem the two are ONE directory and the second `create_dir_all`
        // is a no-op — in which case there is no duplicate to find and this test has nothing to
        // assert. Skip rather than assert a platform property.
        std::fs::create_dir_all(&second).unwrap();
        if !second.join("smacross.rhai").exists() {
            std::fs::write(second.join("smacross.rhai"), OK_SCRIPT).unwrap();
        }
        if root.join("SmaCross").join("smacross.rhai").exists() {
            return; // case-folding filesystem: the two folders are the same directory
        }

        let report = load_rhai_strategies(root);

        assert_eq!(report.strategies.len(), 1, "exactly one of the two loads");
        assert_eq!(
            report.strategies[0].name, "SmaCross",
            "name order decides, not directory order"
        );
        let dup = report
            .diagnostics
            .iter()
            .find(|d| matches!(d, LoadDiagnostic::DuplicateName { .. }))
            .expect("the loser must be reported");
        assert_eq!(dup.severity(), Severity::Error);
        assert!(dup.to_string().contains("rename"), "the message must state the fix");
    }

    /// The first-time mistake: a script dropped straight into `strategies/rhai/`. Reported, with
    /// the path it should have — never silently ignored.
    #[test]
    fn a_script_loose_in_the_root_is_reported_with_where_it_belongs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(&root.join("mystrat.rhai"), OK_SCRIPT);

        let report = load_rhai_strategies(root);

        assert!(report.strategies.is_empty());
        assert_eq!(report.diagnostics.len(), 1);
        assert!(matches!(report.diagnostics[0], LoadDiagnostic::StrayScript { .. }));
        let msg = report.diagnostics[0].to_string();
        assert!(
            msg.contains(&format!("mystrat{}mystrat.rhai", std::path::MAIN_SEPARATOR)),
            "the message must show the target path: {msg}"
        );
    }

    /// A broken preset is a WARNING: the strategy still loads, with its other presets. Losing a
    /// whole strategy over one bad params file would be the wrong trade.
    #[test]
    fn a_malformed_preset_is_a_warning_and_the_strategy_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
        write(&dir.join("good.toml"), "fast = 5\n");
        write(&dir.join("broken.toml"), "fast = = 5\n");

        let report = load_rhai_strategies(root);

        assert_eq!(report.strategies.len(), 1, "the strategy must survive its broken preset");
        assert_eq!(report.strategies[0].presets.len(), 1);
        assert_eq!(report.strategies[0].presets[0].name, "good");
        assert_eq!(report.diagnostics.len(), 1);
        assert_eq!(report.diagnostics[0].severity(), Severity::Warning);
        assert!(matches!(report.diagnostics[0], LoadDiagnostic::BadPreset { .. }));
    }

    /// The same preset name in BOTH positions: the flat one wins (the documented primary
    /// position) and the shadowed one is reported rather than silently dropped.
    #[test]
    fn a_preset_defined_in_both_positions_keeps_the_flat_one_and_reports_the_other() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
        write(&dir.join("fast.toml"), "fast = 5\n");
        write(&dir.join("presets").join("fast.toml"), "fast = 999\n");

        let report = load_rhai_strategies(root);

        let s = &report.strategies[0];
        assert_eq!(s.presets.len(), 1);
        assert_eq!(
            s.presets[0].params.get("fast").and_then(toml::Value::as_integer),
            Some(5),
            "the flat position wins"
        );
        match &report.diagnostics[..] {
            [LoadDiagnostic::ShadowedPreset { preset, .. }] => assert_eq!(preset, "fast"),
            other => panic!("expected one ShadowedPreset, got {other:?}"),
        }
        assert_eq!(report.diagnostics[0].severity(), Severity::Warning);
    }

    /// An ABSENT root is the ordinary state of a fresh install: an empty report and NO diagnostic.
    /// The opposite ruling would greet every new user with an error about a directory they have
    /// never heard of.
    #[test]
    fn a_missing_root_is_an_empty_report_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("user_data").join("strategies").join("rhai");
        assert!(!root.exists(), "precondition");

        let report = load_rhai_strategies(&root);

        assert!(report.strategies.is_empty());
        assert!(report.diagnostics.is_empty(), "absence is not a failure");

        // …and the same for the both-trees scan, whose `strategies/` may not exist either.
        let both = load_user_strategies(&tmp.path().join("user_data").join("strategies"));
        assert!(both.strategies.is_empty());
        assert!(both.diagnostics.is_empty());
    }

    /// Dot-entries are tool droppings, not user content.
    #[test]
    fn dot_entries_are_skipped_without_a_diagnostic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        strategy_dir(root, "sma-cross", OK_SCRIPT);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        write(&root.join(".DS_Store"), "junk");

        let report = load_rhai_strategies(root);

        assert_eq!(report.strategies.len(), 1);
        assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
    }

    /// The log records PASSES as well as failures — the directory's own contract — and every line
    /// carries the caller's stamp so one appended file stays greppable.
    #[test]
    fn the_compile_log_names_every_pass_and_every_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
        write(&dir.join("fast.toml"), "fast = 5\n");
        strategy_dir(root, "broken", "fn on_bar( {\n");

        let report = load_rhai_strategies(root);
        let log = render_compile_log(&report, "2026-08-06T10:00:00Z");

        assert!(log.contains("ok    sma-cross"), "a pass must be logged: {log}");
        assert!(log.contains("1 preset (fast)"), "…with its presets: {log}");
        assert!(log.contains("ERROR broken:"), "a failure must be logged: {log}");
        assert!(log.contains("1 loaded, 1 error(s), 0 warning(s)"), "summary: {log}");
        assert!(log.ends_with('\n'), "an appended log must not splice runs onto one line");
        for line in log.lines() {
            assert!(line.starts_with("2026-08-06T10:00:00Z "), "every line is stamped: {line}");
        }
    }

    // ---- the `rust/` tree: presets for strategies whose code is in the BINARY ------------------

    /// Build `<strategies>/rust/<name>/<preset>.toml`.
    fn native_preset(strategies: &Path, name: &str, preset: &str, body: &str) -> PathBuf {
        let dir = strategies.join("rust").join(name);
        write(&dir.join(format!("{preset}.toml")), body);
        dir
    }

    /// ⚠ THE regression this module's third decision exists for: a folder named after a BUILT-IN
    /// strategy, holding presets and NO entry file, is legitimate — the code is in the binary. It
    /// must load, with its presets, and produce NO diagnostic. Before this it was reported as
    /// `MissingEntry` at ERROR severity, advising the user to rename a script that must not exist.
    #[test]
    fn a_preset_only_folder_for_a_builtin_strategy_loads_with_no_diagnostic() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        native_preset(strategies, "buy_hold", "aggressive", "size = 3\nsymbol = \"BTCUSDT\"\n");

        let report = load_user_strategies(strategies);

        assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
        assert_eq!(report.strategies.len(), 1);
        let s = &report.strategies[0];
        assert_eq!(s.name, "buy_hold");
        assert!(s.is_native(), "the code is in the binary");
        assert_eq!(s.entry(), None, "there is no entry file to find");
        assert_eq!(s.source(), None);
        assert_eq!(s.spec(), StrategySpec::native_default("buy_hold"));
        assert_eq!(s.presets.len(), 1);
        assert_eq!(s.presets[0].name, "aggressive");
    }

    /// The shipped `my_experiment/` template: a `rust/` folder that is NOT a registry name and
    /// holds no presets. Nothing was lost (its `.rs` is a cargo input, consumed long before this
    /// scan), so it must produce no strategy AND no diagnostic — an error on the file `vike-cli
    /// init` itself writes would be the worst possible first impression.
    #[test]
    fn an_unregistered_rust_folder_without_presets_is_silent() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        write(
            &strategies.join("rust").join("my_experiment").join("my_experiment.rs"),
            "// a strategy cargo compiles\n",
        );
        write(&strategies.join("rust").join("README.md"), "# Rust strategies\n");

        let report = load_user_strategies(strategies);

        assert!(report.strategies.is_empty());
        assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
    }

    /// …but the moment that same folder holds PRESETS, they are addressed by a name that resolves
    /// to nothing, and THAT is a loss worth naming — with both fixes in the message.
    #[test]
    fn an_unregistered_rust_folder_with_presets_is_reported_with_both_fixes() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        native_preset(strategies, "my_experiment", "tuned", "size = 2\n");

        let report = load_user_strategies(strategies);

        assert!(report.strategies.is_empty());
        match &report.diagnostics[..] {
            [LoadDiagnostic::UnregisteredNative { name, presets, .. }] => {
                assert_eq!(name, "my_experiment");
                assert_eq!(presets, &vec!["tuned".to_string()]);
            }
            other => panic!("expected one UnregisteredNative, got {other:?}"),
        }
        let msg = report.diagnostics[0].to_string();
        assert!(msg.contains("tuned"), "names the presets that reach nothing: {msg}");
        assert!(msg.contains("strategy_by_name"), "names where to register it: {msg}");
        assert_eq!(report.diagnostics[0].severity(), Severity::Error);
    }

    /// A registry-named preset folder filed under `rhai/` gets the RIGHT advice: move it, not
    /// "rename the script". The old `MissingEntry` wording told the user to rename a script that
    /// should not exist in the first place.
    #[test]
    fn builtin_presets_misfiled_under_rhai_name_the_folder_they_belong_in() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        let dir = strategies.join("rhai").join("buy_hold");
        write(&dir.join("aggressive.toml"), "size = 3\n");

        let report = load_user_strategies(strategies);

        assert!(report.strategies.is_empty());
        match &report.diagnostics[..] {
            [LoadDiagnostic::NativePresetsMisfiled { name, expected_dir, .. }] => {
                assert_eq!(name, "buy_hold");
                assert_eq!(*expected_dir, strategies.join("rust").join("buy_hold"));
            }
            other => panic!("expected one NativePresetsMisfiled, got {other:?}"),
        }
        let msg = report.diagnostics[0].to_string();
        assert!(msg.contains("BUILT-IN"), "must say why there is no script: {msg}");
        assert!(!msg.contains("Rename the script"), "the wrong advice must be gone: {msg}");
    }

    /// A folder named after a built-in but holding NOTHING is more likely a Rhai strategy someone
    /// just started than a misfiled preset folder, so it keeps the ordinary `MissingEntry` row.
    #[test]
    fn an_empty_registry_named_rhai_folder_is_still_a_missing_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        std::fs::create_dir_all(strategies.join("rhai").join("grid")).unwrap();

        let report = load_user_strategies(strategies);

        assert!(matches!(report.diagnostics[..], [LoadDiagnostic::MissingEntry { .. }]));
    }

    /// The folder's CASE does not decide which strategy it presets — the registry's spelling is
    /// what `strategy_by_name` matches, so that is the name the loaded strategy carries.
    #[test]
    fn a_case_folded_folder_still_names_the_registry_strategy() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        native_preset(strategies, "Buy_Hold", "aggressive", "size = 3\n");

        let report = load_user_strategies(strategies);

        assert_eq!(report.strategies.len(), 1);
        assert_eq!(report.strategies[0].name, "buy_hold", "the REGISTRY's spelling");
        assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
    }

    /// One namespace across both trees: a strategy is addressed by NAME, so `rhai/grid/` and
    /// `rust/grid/` are a genuine collision. The `rhai` one is scanned first and wins; the other is
    /// NAMED rather than silently shadowed.
    #[test]
    fn a_name_claimed_in_both_trees_is_a_duplicate_not_a_silent_shadow() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        strategy_dir(&strategies.join("rhai"), "grid", OK_SCRIPT);
        native_preset(strategies, "grid", "tight", "step = 1.0\n");

        let report = load_user_strategies(strategies);

        assert_eq!(report.strategies.len(), 1);
        assert!(!report.strategies[0].is_native(), "the rhai tree is scanned first and wins");
        match &report.diagnostics[..] {
            [LoadDiagnostic::DuplicateName { name, .. }] => assert_eq!(name, "grid"),
            other => panic!("expected one DuplicateName, got {other:?}"),
        }
    }

    /// A folder that loaded NOTHING must not claim its name: the `rust/` half here is a real
    /// strategy and has to load even though a broken `rhai/` folder of the same name was scanned
    /// first.
    #[test]
    fn a_failed_folder_does_not_claim_the_name_for_a_good_one() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        // a `rhai/buy_hold/` with a script that does not compile
        strategy_dir(&strategies.join("rhai"), "buy_hold", "fn on_bar( {\n");
        native_preset(strategies, "buy_hold", "aggressive", "size = 3\n");

        let report = load_user_strategies(strategies);

        assert_eq!(report.strategies.len(), 1);
        assert!(report.strategies[0].is_native());
        assert!(
            report.diagnostics.iter().all(|d| !matches!(d, LoadDiagnostic::DuplicateName { .. })),
            "a failure is not a claim: {:?}",
            report.diagnostics
        );
    }

    /// The compile log names a built-in strategy too, and says where its code is.
    #[test]
    fn the_compile_log_marks_a_builtin_strategy_as_built_in() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        native_preset(strategies, "buy_hold", "aggressive", "size = 3\n");

        let log = render_compile_log(&load_user_strategies(strategies), "2026-08-06T10:00:00Z");

        assert!(log.contains("ok    buy_hold [built-in, 1 preset (aggressive)]"), "{log}");
    }

    // ---- the preset SHAPE rule -----------------------------------------------------------------

    /// ⚠ A preset wrapping its knobs in `[params]` is REFUSED, loudly, naming the fix — it would
    /// otherwise merge into `[strategy.params]` as one key nothing reads while every knob silently
    /// kept its default.
    #[test]
    fn a_params_wrapped_preset_is_refused_with_the_fix_in_the_message() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
        write(&dir.join("fast.toml"), "[params]\nfast = 5.0\nslow = 15.0\n");

        let report = load_rhai_strategies(root);

        assert_eq!(report.strategies.len(), 1, "the strategy survives its unusable preset");
        assert!(report.strategies[0].presets.is_empty());
        match &report.diagnostics[..] {
            [LoadDiagnostic::BadPreset { error, .. }] => {
                assert!(error.contains("[params]"), "names the offending header: {error}");
                assert!(error.contains("top level"), "names the fix: {error}");
            }
            other => panic!("expected one BadPreset, got {other:?}"),
        }
        assert_eq!(report.diagnostics[0].severity(), Severity::Warning);
    }

    /// …but a genuinely NESTED knob is untouched: `funding_carry` reads a `[venues]` table
    /// straight out of `[strategy.params]`, so only the exact lone-`params`-table shape is refused.
    #[test]
    fn a_nested_knob_table_is_a_perfectly_good_preset() {
        let tmp = tempfile::tempdir().unwrap();
        let strategies = tmp.path();
        native_preset(
            strategies,
            "funding_carry",
            "cross",
            "cooldown_ms = 500\n[venues]\nBTCUSDT = \"binance\"\n",
        );

        let report = load_user_strategies(strategies);

        assert!(report.diagnostics.is_empty(), "unexpected: {:?}", report.diagnostics);
        let p = &report.strategies[0].presets[0];
        assert!(p.params.get("venues").is_some_and(toml::Value::is_table));
    }

    /// `src` is the strategy's SOURCE, not a knob — a preset carrying one could smuggle a whole
    /// script past `--script`, so it is refused rather than resolved by an invisible precedence.
    #[test]
    fn a_preset_that_defines_src_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let dir = strategy_dir(root, "sma-cross", OK_SCRIPT);
        write(&dir.join("sneaky.toml"), "src = \"fn on_bar() { buy(99.0); }\"\n");

        let report = load_rhai_strategies(root);

        assert!(report.strategies[0].presets.is_empty());
        match &report.diagnostics[..] {
            [LoadDiagnostic::BadPreset { error, .. }] => {
                assert!(error.contains("src"), "names the offending key: {error}")
            }
            other => panic!("expected one BadPreset, got {other:?}"),
        }
    }
}
