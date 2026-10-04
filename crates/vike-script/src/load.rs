//! Loads `<project>/user_data/indicators/*.rhai` — the user's own indicators — into compiled
//! [`RhaiIndicator`]s ready to hand to [`crate::RhaiStrategy::compile_with_indicators`].
//!
//! The sibling of `vike_studio_core::user_strategies::load`, and deliberately much smaller: an indicator is ONE file
//! with no presets, so there is no folder-vs-entry-file rule to enforce and no `rust/` tier (a
//! compiled indicator is just an `Indicator` impl in a crate).
//!
//! ## Every rejection is REPORTED, never silent
//!
//! That is the whole reason this returns a report instead of a `Vec`. A user's indicator failing to
//! load is indistinguishable, from inside their strategy, from a typo in the call — both are
//! function-not-found — so the load side is the only place that can say WHICH. Four things get a
//! diagnostic rather than a shrug:
//!
//! - a compile error (naming the rhai message),
//! - a name that would shadow a built-in or a host verb
//!   ([`crate::user_indicator_conflict`]),
//! - a file in a SUBDIRECTORY, which this flat layout does not read — the easy mistake for anyone
//!   who has just organised their strategies into folders,
//! - two files whose stems collide (`ema.rhai` + `ema.RHAI`, which coexist on Linux).
//!
//! ## Order is sorted, not filesystem order
//!
//! `read_dir` order is unspecified and differs between filesystems. Two indicators cannot interact
//! (each holds its own state and cannot see the others), so order does not change any VALUE — but
//! it changes the diagnostic list and the registration order, and a report that reshuffles between
//! machines is one nobody can diff.

use crate::RhaiIndicator;
use std::path::{Path, PathBuf};

/// The indicator entry-file extension, compared case-INSENSITIVELY — the same rule
/// `vike_studio_core::user_strategies::load` uses, and for the same reason: a Windows editor that saved `MyThing.RHAI`
/// wrote a real indicator, and refusing it on case would be a mystery rather than a message.
const RHAI_EXT: &str = "rhai";

/// One successfully loaded user indicator.
pub struct UserIndicator {
    /// The file stem — the name a strategy calls it by.
    pub name: String,
    /// The file it came from, for a diagnostic or an "open in editor".
    pub path: PathBuf,
    /// Compiled and ready to bind. This is a PROTOTYPE: binding clones it per strategy.
    pub indicator: RhaiIndicator,
}

impl std::fmt::Debug for UserIndicator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserIndicator").field("name", &self.name).field("path", &self.path).finish()
    }
}

/// Why one file under `indicators/` did not become a callable indicator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndicatorDiagnostic {
    /// The script did not compile, or has no `fn on_bar(bar)`. `error` is
    /// [`crate::compile_indicator`]'s message verbatim.
    CompileFailed { name: String, path: PathBuf, error: String },
    /// The name would shadow a built-in indicator or a host function — see
    /// [`crate::user_indicator_conflict`] for why that is refused rather than allowed to win.
    NameConflict { name: String, path: PathBuf, reason: String },
    /// A `.rhai` file inside a SUBDIRECTORY of `indicators/`. This layout is flat, so the file is
    /// not loaded; without this the folder would simply do nothing.
    InSubdirectory { path: PathBuf },
    /// Two files share a stem, so one name would win arbitrarily. Neither is loaded — picking one
    /// silently would make the answer depend on directory order.
    DuplicateName { name: String, paths: Vec<PathBuf> },
    /// The directory or a file could not be read (permissions, a broken link).
    Unreadable { path: PathBuf, error: String },
}

impl IndicatorDiagnostic {
    /// A one-line, user-facing message. Every variant names the file, because the user's next
    /// action is to open it.
    pub fn message(&self) -> String {
        match self {
            Self::CompileFailed { path, error, .. } => {
                format!("{}: did not compile — {error}", path.display())
            }
            Self::NameConflict { path, reason, .. } => format!("{}: {reason}", path.display()),
            Self::InSubdirectory { path } => format!(
                "{}: ignored — indicators/ is FLAT, one <name>.rhai per indicator. Move it up one \
                 level.",
                path.display()
            ),
            Self::DuplicateName { name, paths } => format!(
                "`{name}` is claimed by {} files ({}) — neither is loaded, because which one won \
                 would depend on directory order. Rename one.",
                paths.len(),
                paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
            ),
            Self::Unreadable { path, error } => {
                format!("{}: could not be read — {error}", path.display())
            }
        }
    }

    /// The indicator name this diagnostic is about, when it has one.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::CompileFailed { name, .. }
            | Self::NameConflict { name, .. }
            | Self::DuplicateName { name, .. } => Some(name),
            Self::InSubdirectory { .. } | Self::Unreadable { .. } => None,
        }
    }
}

/// What one `indicators/` directory yielded.
pub struct IndicatorLoadReport {
    /// The directory scanned, echoed back so a caller can print where it looked.
    pub root: PathBuf,
    /// Loaded and callable, sorted by name.
    pub indicators: Vec<UserIndicator>,
    /// Everything that did not load, sorted by path — see the module doc.
    pub diagnostics: Vec<IndicatorDiagnostic>,
}

impl IndicatorLoadReport {
    /// The compiled prototypes, in the shape [`crate::RhaiStrategy::compile_with_indicators`] takes.
    pub fn prototypes(&self) -> Vec<RhaiIndicator> {
        self.indicators.iter().map(|i| i.indicator.clone()).collect()
    }

    /// The same load, in the shape `vike_chart::indicators::install_user_studies` takes — the
    /// sibling of [`IndicatorLoadReport::prototypes`] for the CHART rather than for a strategy.
    ///
    /// ⚠ LEAKS one descriptor per indicator ([`crate::user_meta`]), so a binary calls this ONCE at
    /// startup beside its `install_user_indicators`, never per frame and never per window.
    pub fn chart_studies(&self) -> Vec<&'static vike_indicators::IndicatorMeta> {
        self.indicators.iter().map(|i| crate::user_meta(&i.indicator)).collect()
    }

    /// True when nothing was rejected. Distinct from "loaded nothing": an empty directory is a
    /// clean load.
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

impl std::fmt::Debug for IndicatorLoadReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndicatorLoadReport")
            .field("root", &self.root)
            .field("indicators", &self.indicators)
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

/// True when `p` has the `.rhai` extension, ignoring case.
fn is_rhai(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(RHAI_EXT))
}

/// Load `<user_data>/indicators/*.rhai` and INSTALL the result process-wide — **the whole of what a
/// composition root has to do** — returning every message that root must SURFACE, in report order.
///
/// [`load_user_indicators`] and [`crate::install_user_indicators`] stay separate functions and this
/// changes neither: the installer still takes ALREADY-COMPILED prototypes and performs no I/O (its
/// doc is the authority on why — a library that resolved this directory for itself would be reading
/// state its caller can neither see nor override), and the CALLER still names the directory here.
/// What this removes is the three-statement dance every binary would otherwise re-derive: the
/// `INDICATORS_SUBDIR` join, the diagnostic loop, and the double-install message that is easy to
/// drop on the floor because it is `Err` on a call whose value nobody wants.
///
/// ## Messages come back as DATA, and that is load-bearing
///
/// The roots need them at different MOMENTS and on different STREAMS, and there are at least two
/// incompatible shapes: a root whose STDOUT is a protocol must print on stderr and cannot let a
/// library choose (`vike-cli mcp`), and a root that logs through `tracing` cannot emit anything at
/// all until `vike_log::init` has built a subscriber. A function that wrote to a caller's stderr on
/// its own initiative could serve neither — the same argument `vike_config` makes for returning its
/// own warnings rather than logging them.
///
/// (Which binaries call this is not written down here and does not need to be:
/// `git grep -l load_and_install_user_indicators -- crates` IS the roster of files naming this pair
/// — ⚠ every MENTION, not every caller: `vike-desktop` appears there while deliberately NOT using it
/// (it has two consumers of one load and needs the report, not just the messages), and every
/// prose copy of such a list in this repo has gone stale.)
///
/// ## Nothing here is a reason to FAIL
///
/// From inside a strategy, an indicator that failed to load is indistinguishable from a typo in the
/// call — both are function-not-found — so this is the only place that can say which. Aborting
/// instead would let ONE half-edited file block a command with nothing to do with it. An empty
/// return is therefore the normal outcome, including for a project that has no `indicators/`
/// directory at all.
pub fn load_and_install_user_indicators(user_data_dir: &Path) -> Vec<String> {
    let report =
        load_user_indicators(&user_data_dir.join(vike_model::paths::state_path::INDICATORS_SUBDIR));
    let mut messages: Vec<String> = report
        .diagnostics
        .iter()
        .map(|d| format!("indicator not loaded — {}", d.message()))
        .collect();
    // Only reachable when something else in this process installed first — a once-per-process call
    // made twice means two places believe they own the set, and the message says so.
    if let Err(e) = crate::install_user_indicators(report.prototypes()) {
        messages.push(e);
    }
    messages
}

/// Loads every `<name>.rhai` directly inside `root`.
///
/// An ABSENT `root` is an empty, CLEAN report — not a diagnostic. A user who has never written an
/// indicator is in the ordinary state, and the same walk that finds the directory returns `None`
/// for a project that has none; warning about it would make a fresh install look broken. A root
/// that EXISTS and cannot be READ is a diagnostic, which is the same distinction the credential
/// store draws.
pub fn load_user_indicators(root: &Path) -> IndicatorLoadReport {
    let mut indicators = Vec::new();
    let mut diagnostics = Vec::new();

    if !root.exists() {
        return IndicatorLoadReport { root: root.to_path_buf(), indicators, diagnostics };
    }

    // stem -> every file claiming it, so a collision is reportable rather than order-dependent.
    let mut claims: indexmap::IndexMap<String, Vec<PathBuf>> = indexmap::IndexMap::new();
    collect(root, root, &mut claims, &mut diagnostics);

    for (name, paths) in &claims {
        if paths.len() > 1 {
            let mut paths = paths.clone();
            paths.sort();
            diagnostics.push(IndicatorDiagnostic::DuplicateName { name: name.clone(), paths });
            continue;
        }
        let path = paths[0].clone();
        // The conflict check comes BEFORE compiling: a file named `sma.rhai` is refused for its
        // NAME whether or not its body is valid, and reporting a compile error for it would send
        // the author to fix the wrong thing.
        if let Some(reason) = crate::user_indicator_conflict(name) {
            diagnostics.push(IndicatorDiagnostic::NameConflict {
                name: name.clone(),
                path,
                reason,
            });
            continue;
        }
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                diagnostics.push(IndicatorDiagnostic::Unreadable { path, error: e.to_string() });
                continue;
            }
        };
        match crate::compile_indicator(name, &src) {
            Ok(indicator) => indicators.push(UserIndicator { name: name.clone(), path, indicator }),
            Err(e) => diagnostics.push(IndicatorDiagnostic::CompileFailed {
                name: name.clone(),
                path,
                error: e.to_string(),
            }),
        }
    }

    indicators.sort_by(|a, b| a.name.cmp(&b.name));
    diagnostics.sort_by_key(|d| format!("{d:?}"));
    IndicatorLoadReport { root: root.to_path_buf(), indicators, diagnostics }
}

/// One directory level. Recurses ONLY to report a `.rhai` file that a flat layout will not read —
/// it never loads one, so the layout stays flat while the mistake stays visible.
fn collect(
    root: &Path,
    dir: &Path,
    claims: &mut indexmap::IndexMap<String, Vec<PathBuf>>,
    diagnostics: &mut Vec<IndicatorDiagnostic>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            diagnostics.push(IndicatorDiagnostic::Unreadable {
                path: dir.to_path_buf(),
                error: e.to_string(),
            });
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, claims, diagnostics);
            continue;
        }
        if !is_rhai(&path) {
            continue;
        }
        if path.parent() != Some(root) {
            diagnostics.push(IndicatorDiagnostic::InSubdirectory { path });
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            diagnostics.push(IndicatorDiagnostic::Unreadable {
                path: path.clone(),
                error: "the file name is not valid UTF-8".into(),
            });
            continue;
        };
        claims.entry(stem.to_string()).or_default().push(path);
    }
}

#[path = "load_tests.rs"]
#[cfg(test)]
mod load_tests;
