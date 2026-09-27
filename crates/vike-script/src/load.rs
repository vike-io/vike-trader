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
/// — ⚠ every MENTION, not every caller: `vike-app` appears there while deliberately NOT using it
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
        load_user_indicators(&user_data_dir.join(vike_model::state_path::INDICATORS_SUBDIR));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Indicator;
    use vike_model::Bar;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!("vike-user-ind-{tag}-{nanos}"));
            std::fs::create_dir_all(&p).expect("scratch");
            Self(p)
        }
        fn write(&self, rel: &str, src: &str) -> PathBuf {
            let p = self.0.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, src).unwrap();
            p
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const GOOD: &str = "fn on_bar(bar) { bar.close }";

    fn bar(c: f64) -> Bar {
        Bar {
            ts: 0,
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 1.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    #[test]
    fn loads_a_flat_directory_and_the_prototype_actually_computes() {
        let s = Scratch::new("flat");
        s.write("passthrough.rhai", GOOD);
        let r = load_user_indicators(s.path());
        assert!(r.is_clean(), "{:?}", r.diagnostics);
        assert_eq!(r.indicators.len(), 1);
        assert_eq!(r.indicators[0].name, "passthrough");

        let mut proto = r.prototypes().remove(0);
        assert_eq!(proto.on_bar(&bar(7.5))[0], 7.5);
    }

    /// An absent directory is the ordinary unconfigured state — see `load_user_indicators`' doc.
    #[test]
    fn an_absent_directory_is_a_clean_empty_report_not_an_error() {
        let s = Scratch::new("absent");
        let r = load_user_indicators(&s.path().join("nope"));
        assert!(r.is_clean() && r.indicators.is_empty());
    }

    #[test]
    fn an_empty_directory_is_also_clean() {
        let s = Scratch::new("empty");
        let r = load_user_indicators(s.path());
        assert!(r.is_clean() && r.indicators.is_empty());
    }

    #[test]
    fn a_compile_error_is_reported_against_its_file_and_the_rest_still_load() {
        let s = Scratch::new("broken");
        s.write("fine.rhai", GOOD);
        s.write("broken.rhai", "fn on_bar(bar) { this. }");
        let r = load_user_indicators(s.path());
        assert_eq!(r.indicators.len(), 1, "the good one still loads");
        assert_eq!(r.indicators[0].name, "fine");
        assert!(matches!(r.diagnostics[0], IndicatorDiagnostic::CompileFailed { .. }));
        assert!(r.diagnostics[0].message().contains("broken.rhai"));
    }

    /// The most likely real mistake: a strategy hook copied into an indicator file.
    #[test]
    fn a_file_with_no_on_bar_is_a_compile_error_not_a_silent_skip() {
        let s = Scratch::new("noonbar");
        s.write("empty_thing.rhai", "fn init() { #{} }");
        let r = load_user_indicators(s.path());
        assert!(r.indicators.is_empty());
        assert!(r.diagnostics[0].message().contains("on_bar"), "{:?}", r.diagnostics[0]);
    }

    #[test]
    fn a_builtin_name_is_refused_by_name_before_its_body_is_even_compiled() {
        let s = Scratch::new("shadow");
        // Deliberately UNCOMPILABLE: if the conflict check ran second, the reported diagnostic
        // would be the compile error and would send the author to fix the wrong thing.
        s.write("sma.rhai", "this is not rhai at all (");
        let r = load_user_indicators(s.path());
        assert!(r.indicators.is_empty());
        match &r.diagnostics[0] {
            IndicatorDiagnostic::NameConflict { name, reason, .. } => {
                assert_eq!(name, "sma");
                assert!(reason.contains("built-in indicator"), "{reason}");
            }
            d => panic!("expected a NameConflict, got {d:?}"),
        }
    }

    #[test]
    fn a_host_verb_name_is_refused_too() {
        let s = Scratch::new("verb");
        s.write("close.rhai", GOOD);
        let r = load_user_indicators(s.path());
        assert!(r.indicators.is_empty());
        assert!(r.diagnostics[0].message().contains("host function"), "{:?}", r.diagnostics[0]);
    }

    /// `var` is a rhai reserved word, so a file named for it could never be CALLED — the refusal
    /// has to happen here, since nothing downstream could report it.
    #[test]
    fn a_name_rhai_cannot_parse_is_refused_at_load() {
        let s = Scratch::new("reserved");
        s.write("var.rhai", GOOD);
        let r = load_user_indicators(s.path());
        assert!(r.indicators.is_empty());
        assert!(!r.is_clean());
    }

    /// Flat means flat — but the file must be MENTIONED, or the folder silently does nothing.
    #[test]
    fn a_rhai_file_in_a_subdirectory_is_reported_rather_than_silently_ignored() {
        let s = Scratch::new("nested");
        s.write("mine/deep.rhai", GOOD);
        let r = load_user_indicators(s.path());
        assert!(r.indicators.is_empty(), "a nested file is NOT loaded");
        match &r.diagnostics[0] {
            IndicatorDiagnostic::InSubdirectory { path } => {
                assert!(path.ends_with("deep.rhai"));
            }
            d => panic!("expected InSubdirectory, got {d:?}"),
        }
        assert!(r.diagnostics[0].message().contains("FLAT"));
    }

    #[test]
    fn a_non_rhai_file_is_ignored_without_a_diagnostic() {
        let s = Scratch::new("readme");
        s.write("README.md", "notes");
        s.write("thing.rhai", GOOD);
        let r = load_user_indicators(s.path());
        assert!(r.is_clean());
        assert_eq!(r.indicators.len(), 1);
    }

    #[test]
    fn the_extension_is_matched_case_insensitively() {
        let s = Scratch::new("case");
        s.write("Shouty.RHAI", GOOD);
        let r = load_user_indicators(s.path());
        assert!(r.is_clean(), "{:?}", r.diagnostics);
        assert_eq!(r.indicators[0].name, "Shouty");
    }

    /// Order must not decide which of two colliding files wins, so NEITHER does.
    #[test]
    fn two_files_claiming_one_name_load_neither_and_say_so() {
        let s = Scratch::new("dupe");
        s.write("dup.rhai", GOOD);
        let second = s.write("dup.RHAI", GOOD);
        // A case-insensitive filesystem (Windows/macOS) collapses these into ONE file, so the
        // collision cannot be constructed there — skip rather than assert a platform's behaviour.
        if std::fs::read_dir(s.path()).unwrap().filter_map(Result::ok).count() < 2 {
            assert!(second.exists());
            return;
        }
        let r = load_user_indicators(s.path());
        assert!(r.indicators.is_empty(), "neither is loaded");
        match &r.diagnostics[0] {
            IndicatorDiagnostic::DuplicateName { name, paths } => {
                assert_eq!(name, "dup");
                assert_eq!(paths.len(), 2);
            }
            d => panic!("expected DuplicateName, got {d:?}"),
        }
    }

    /// `read_dir` order is unspecified; a report that reshuffles between machines cannot be diffed.
    #[test]
    fn indicators_come_back_sorted_by_name() {
        let s = Scratch::new("sorted");
        for n in ["zulu", "alpha", "mike"] {
            s.write(&format!("{n}.rhai"), GOOD);
        }
        let r = load_user_indicators(s.path());
        let names: Vec<&str> = r.indicators.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mike", "zulu"]);
    }

    /// A `fn outputs()` declaration survives the LOAD path, so the prototype the loader hands on
    /// carries the lines the accessors are generated from. Without this the declaration would be
    /// provable only through `compile_indicator` directly, which no binary calls.
    #[test]
    fn a_declared_output_list_reaches_the_loaded_prototype() {
        let s = Scratch::new("outputs");
        s.write(
            "my_bands.rhai",
            "fn outputs() { [\"upper\", \"mid\", \"lower\"] } \
             fn on_bar(bar) { [bar.close + 1.0, bar.close, bar.close - 1.0] }",
        );
        s.write("plain.rhai", GOOD);
        let r = load_user_indicators(s.path());
        assert!(r.is_clean(), "{:?}", r.diagnostics);
        let protos = r.prototypes();
        // Sorted by name: my_bands, plain.
        assert_eq!(protos[0].outputs(), ["upper".to_string(), "mid".into(), "lower".into()]);
        assert_eq!(
            crate::user_line_accessors(&protos[0]).into_iter().map(|(_, f)| f).collect::<Vec<_>>(),
            vec!["my_bands_upper", "my_bands_mid", "my_bands_lower"]
        );
        // ...and the file next to it, which declares nothing, is untouched: one line, no accessors.
        assert_eq!(protos[1].outputs(), ["plain".to_string()]);
        assert!(crate::user_line_accessors(&protos[1]).is_empty());
    }

    /// A bad `outputs()` is a COMPILE diagnostic against its own file, so the author is sent to the
    /// line to rename rather than to a function-not-found in some strategy.
    ///
    /// ⚠ The file name and the line are DERIVED — `crates/vike-script/src/engine.rs`'s
    /// `registry_name_a_user_line_could_spell` picks a registry name containing a `_` whose stem is
    /// itself a free indicator name, so the loader reaches `compile_indicator` instead of refusing
    /// the file for its own NAME first. Hand-writing the pair is how this test came to claim that
    /// `pos.rhai` declaring a line `ition` shadows the host read `position`: `line_fn_name` joins
    /// with `_`, so it spells `pos_ition`, the file loaded clean, and the test failed.
    #[test]
    fn a_bad_output_declaration_is_reported_against_its_file() {
        let (stem, line) = crate::engine::registry_name_a_user_line_could_spell();
        let taken = crate::line_fn_name(stem, line);
        let s = Scratch::new("badoutputs");
        s.write("fine.rhai", GOOD);
        s.write(
            &format!("{stem}.rhai"),
            &format!("fn outputs() {{ [\"{line}\", \"x\"] }} fn on_bar(bar) {{ [1.0, 2.0] }}"),
        );
        let r = load_user_indicators(s.path());
        assert_eq!(r.indicators.len(), 1, "the good one still loads: {:?}", r.indicators);
        assert_eq!(r.indicators[0].name, "fine");
        let msg = r.diagnostics[0].message();
        assert!(msg.contains(&format!("{stem}.rhai")), "{msg}");
        assert!(msg.contains(&taken), "the message names the accessor it would shadow: {msg}");
    }

    /// Two prototypes from one load must not share state — they are separate indicators.
    #[test]
    fn two_loaded_indicators_stream_independently() {
        let s = Scratch::new("indep");
        s.write("count_a.rhai", "fn init() { #{ n: 0 } } fn on_bar(bar) { this.n += 1; this.n }");
        s.write("count_b.rhai", "fn init() { #{ n: 0 } } fn on_bar(bar) { this.n += 10; this.n }");
        let r = load_user_indicators(s.path());
        let mut protos = r.prototypes();
        assert_eq!(protos.len(), 2);
        for _ in 0..3 {
            protos[0].on_bar(&bar(1.0));
        }
        protos[1].on_bar(&bar(1.0));
        assert_eq!(protos[0].value()[0], 3.0);
        assert_eq!(protos[1].value()[0], 10.0);
    }
}
