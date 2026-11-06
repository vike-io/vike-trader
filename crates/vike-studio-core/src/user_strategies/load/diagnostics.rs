//! The named rows a scan reports instead of failing, and the one line each renders as.
use std::path::{Path, PathBuf};

use vike_model::Severity;

use super::RHAI_EXT;

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
    /// move the folder to `strategies/rust/`, where presets for a built-in strategy belong.
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
    /// Did this cost the user a strategy? [`Severity::Error`]: nothing loaded from this
    /// folder/file. [`Severity::Warning`]: the strategy loaded and something beside it did not.
    ///
    /// The line between the two is exactly "did the user lose a strategy": a broken preset leaves
    /// the strategy runnable with its other presets, everything else means a folder the user
    /// believes is installed is not. `crates/vike-studio-core/src/listing.rs`'s
    /// `ListingDiagnostic::severity` asks the same question about a RUN or a STUDY, in the same
    /// vocabulary — `vike_model`'s refused-or-accepted-with-a-remark [`Severity`], one enum for every
    /// finding in the workspace rather than a two-variant copy per scan.
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
