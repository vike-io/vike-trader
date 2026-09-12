//! `binutil` — the shared argv + hist-store-root glue for this crate's bins (and
//! `vike-report`'s `tearsheet`), plus the run-level provenance stamp their JSON carries
//! ([`stats_provenance`]) — one home, so two bins publishing the same statistic cannot end up
//! describing it two ways, which is the defect the stamp itself exists to close.
//!
//! Every store-driven CLI in this family (`backtest`, `cheap_np_run`, `cheap_np_depth`,
//! `cheap_np_askgate`, `sport_taker_run`, plus `vike-report`'s `tearsheet`) used to hand-copy
//! the same `arg`/`has_flag` helpers, and the three `cheap_np_*` bins CWD-anchored their
//! hist-store default (`PathBuf::from("market_data/hist")`) — run from any directory but the repo
//! root, `DataFusionHist::open` would CREATE an empty store there and the run reported a
//! silent "0 windows" instead of failing. This module is the single home for that glue;
//! [`store_root`] anchors the default at the REPO root (`CARGO_MANIFEST_DIR` ancestors —
//! never CWD), the same convention `vike-backfill`'s backfill bins use.
//!
//! Feature-free on purpose (like `objective`): the store-driven bins sit behind
//! `datafusion-store`, but the module itself compiles in every build.
//!
//! **The four PURE argv parsers now live in [`vike_analytics::binutil`]** and are re-exported
//! below, so every existing `vike_backtest::binutil::…` path resolves unchanged. Only
//! [`store_root`] stayed: it is the one function here that resolves the hist-store root, and the
//! workspace settings registry (`vike_ops::settings::SETTINGS`) keys that declaration on
//! `(name, krate)` — moving it would have retagged the row's crate.
//! `vike-report`'s DataFusion-free `tearsheet` bin needs only the argv half, which is exactly
//! why that half moved down: it can now reach it without linking this crate at all.
//!
//! [`store_root`] is now PURE: it takes the environment as an already-collected MAP and the four
//! bins that call it do the `std::env::vars()` sweep themselves. It used to read four variables
//! (`VIKE_HIST_STORE` plus the `XDG_DATA_HOME`/`HOME`/`LOCALAPPDATA` platform trio) with
//! `std::env::var` — inside a LIBRARY file, so all four carried `Layer::Library` rows on the
//! settings registry's STEP-2 work-list even though every caller is a binary in this same crate.
//! The lift also removes a real hazard the old unit test documented in its own comment: it had to
//! be ONE test function because it MUTATED the process environment, and a sibling test asserting
//! the unset-fallback concurrently would have raced its `set_var`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use vike_analytics::binutil::{arg, f64_arg, has_flag, parse_num};

/// The `VIKE_HIST_STORE` override, spelled once for this crate's map lookup.
const HIST_STORE_VAR: &str = "VIKE_HIST_STORE";

/// The sentence [`stats_provenance`] carries about its own numbers, and the ONE place the ABSENCE
/// rule is written down for somebody holding the FILE rather than this source.
///
/// ⚠ **A stamp can only ever be added going forward, so the scheme has to work by absence** — the
/// JSON files an operator already has on disk cannot be retro-stamped, and there is no version they
/// could be said to carry. So the rule a reader applies is: *a document with a `stats_provenance`
/// object states its convention; a document with none predates the naming and is nearest-rank.*
/// That rule is useless if it lives only in the code, because the person comparing two files is
/// reading files — hence this note ships INSIDE the new one, where it is legible beside the very
/// numbers it disqualifies the old file from being compared against. Same device as
/// `crates/vike-studio-core/src/study_run.rs`'s `STUDY_METRICS_NOTE`, which rides in a study run's
/// own `detail` for the same reason: *"a manifest is also read ALONE"*.
///
/// ASCII-only and one paragraph on purpose (the constraint
/// `crates/vike-buildinfo/src/lib.rs`'s `summary` states for its own line): this lands in pasted
/// terminal output, an issue body and a chat window, and every one of those is grepped.
pub const PERCENTILE_NOTE: &str = "Percentiles in this document come from \
     vike_analytics::metrics::percentile - numpy linear interpolation (method=linear) between the \
     two closest ranks. A JSON document from these bins that carries NO stats_provenance object at \
     all was produced BEFORE the convention was named, and its percentiles are nearest-rank: the \
     observed sample at round((n - 1) * q), never an interpolant. The two disagree in BOTH \
     directions on one sample, so no scale factor converts an old file into a new one - re-run the \
     bin rather than comparing across the gap.";

/// The RUN-LEVEL provenance block a JSON-emitting bin in this family stamps on its summary object,
/// so that two files can be told apart by what produced them rather than by when somebody remembers
/// having run them:
///
/// ```text
/// "stats_provenance": { "percentile_method": <PERCENTILE_METHOD>, "note": <PERCENTILE_NOTE> }
/// ```
///
/// (Spelled as the two constants rather than as their values: a doc example carrying the literal
/// would be a third hand copy of the very fact this block exists to keep honest.)
///
/// # Where it goes, and where it must not
///
/// **On the ONE run-level summary object each bin prints, never on a row.** These bins emit
/// per-entry rows — the per-size curve arrays, the per-anchor lanes, and the `--out` CSV beside
/// them; a marker repeated per row is bytes spent restating a fact that is constant for the whole
/// document. The nesting argument is `crates/vike-studio-core/src/study_run.rs`'s, inverted:
/// metrics NEST there because a reader must be made to ask which scorer produced them, and this
/// block sits at the TOP because a reader must not have to go looking for the answer.
///
/// The bins' human-readable lane prints the same percentiles and so names the same convention on
/// its header line, rendered from [`vike_analytics::metrics::PERCENTILE_METHOD`] rather than from
/// this block: terminal output gets pasted into an issue, where it has the JSON's problem and none
/// of its structure. It gets the NAME and not the note — a paragraph on every run is noise, and
/// the reader of a pasted terminal line is reading it beside the person who ran it.
///
/// # Why a method NAME and no schema version
///
/// The name is [`vike_analytics::metrics::PERCENTILE_METHOD`], which lives beside the algorithm it
/// names — that constant's doc carries the argument for a name over a number.
///
/// No schema version rides beside it, and the omission is a decision rather than an oversight. This
/// tree does version a payload where a DECODER has to know how to read bytes it did not write —
/// `crates/vike-data/src/datafusion_hist/codec.rs`'s `BOOK_SCHEMA_VERSION` and its siblings, stamped
/// into Parquet key-value metadata. Nothing decodes THESE documents: the readers are a human and an
/// ad-hoc `jq` filter selecting named keys, both of which tolerate an added key and neither of which
/// can act on an integer. Meanwhile these summaries have grown keys repeatedly with nothing bumped
/// (the third anchor, the live-law lanes, the delta-integrity block), so a version pinned here would
/// have been stale on arrival — and a stale version is worse than no version, because it is a claim
/// somebody will trust. Naming each convention gives the discrimination a version was wanted for,
/// per statistic, and additively: a future `stderr_method` is one more key, and a reader keys on
/// the NAME.
///
/// # What it deliberately does NOT claim
///
/// Only the percentiles. `crates/vike-backtest/src/bin/cheap_np_askgate.rs`'s `stderr` fold is a
/// separate convention that was deliberately left alone, and a key called `stats_method` would have
/// silently spoken for it. Nor is there a `git_sha`: naming a commit means `vike-buildinfo`, which
/// resolves it at COMPILE time, and this crate does not depend on it —
/// `crates/vike-backtest/src/runs.rs`'s `RunManifest` writes `None` for exactly that reason.
pub fn stats_provenance() -> serde_json::Value {
    serde_json::json!({
        "percentile_method": vike_analytics::metrics::PERCENTILE_METHOD,
        "note": PERCENTILE_NOTE,
    })
}

/// The hist-store root: `explicit` (`--store`) > `$VIKE_HIST_STORE` > `<repo>/market_data/hist` when this
/// box still has that checkout > the project's own `<project>/market_data/hist` > the per-user
/// `…/vike-data`.
///
/// `vars` is the process environment as the CALLER collected it (`std::env::vars().collect()`),
/// per the settings-registry rule that libraries take configuration as parameters.
///
/// The repo fallback is resolved from `CARGO_MANIFEST_DIR` ancestors — NEVER the current
/// working directory. A CWD-relative default is exactly the bug this helper replaces: run a
/// bin from outside the repo and `DataFusionHist::open("market_data/hist")` silently CREATES an
/// empty store wherever you happen to stand, so the run reports zero data instead of
/// failing.
///
/// ⚠ The project rung IS resolved from the working directory, which looks like that same bug and
/// is its opposite: the walk requires a project MARKER (`crates/vike-model/src/state_path.rs`'s
/// `nearest_project_marker`), so standing somewhere that is not a project yields `None` and falls
/// through. A CWD-relative LITERAL creates a store wherever you stand; a walk for a marker either
/// finds the project or admits there is none. `$VIKE_SETTINGS_DIR` pins that project outright, and
/// `resolve_store_root_from` honours it here without this crate naming it.
///
/// **The resolution is LOGGED**, root and rung both. A store does not merge: if the default moves,
/// the old store is simply no longer read and the run reports zero rows — which reads exactly like
/// "there is no data for that range". One line at startup is the difference between an operator
/// seeing that and hunting it.
pub fn store_root(explicit: Option<PathBuf>, vars: &HashMap<String, String>) -> PathBuf {
    store_root_resolved(explicit, vars).into_path()
}

/// [`store_root`] keeping the [`StoreRoot`](vike_model::store_path::StoreRoot) — the path AND the
/// rung that chose it.
///
/// ⚠ It exists because a DESTRUCTIVE verb has to answer "which store" on STDOUT before it answers
/// "which series", and the `tracing::info!` inside [`store_root`] is silenced by `RUST_LOG`. A
/// store does not merge: a resolution that moved is invisible until it has destroyed the wrong
/// tree. `backtest data rm` leads its plan with `StoreRoot`'s `Display`, which is exactly this
/// value.
///
/// [`store_root`] is the same call with the rung dropped, so the two cannot resolve differently —
/// which is the whole reason this is an accessor rather than a second resolution at the call site.
pub fn store_root_resolved(
    explicit: Option<PathBuf>,
    vars: &HashMap<String, String>,
) -> vike_model::store_path::StoreRoot {
    let cwd = std::env::current_dir().ok();
    // ⚠ A BLANK `--store` falls through instead of resolving the store to `""`.
    //
    // Reachable since `vike_analytics::binutil::arg` learned the inline `=` spelling: `--store=`
    // used to match nothing and read as absent, and now answers `Some("")` — deliberately, so a
    // caller can refuse a blank by name. But `vike_model::store_path::resolve_store_root` honours an
    // EXPLICIT path unfiltered (the `Explicit` rung returns before the
    // `.filter(|s| !s.trim().is_empty())` that guards the env rung), so `""` would reach
    // `DataFusionHist::open`, which `create_dir_all`s whatever it is given — a store minted in the
    // working directory, and a run reporting zero rows.
    //
    // Here rather than at each bin because all five store-driven bins share this ONE funnel, and
    // the same rule the env rung already applies is the one an operator expects of a blank flag.
    let explicit =
        explicit.filter(|p| !p.as_os_str().is_empty() && !p.to_string_lossy().trim().is_empty());
    let resolved = resolve_with(explicit, repo_default().as_deref(), cwd.as_deref(), vars);
    // ⚠ `tracing` is an OPTIONAL dependency of this crate (`dep:tracing`, pulled in by `hist-replay`
    // / `bench-hist`), so the log carries the same gate. Nothing is lost by it: every bin that calls
    // this function declares `required-features = ["datafusion-store"]`, which turns `hist-replay`
    // on — a build where this line is compiled out is a build with no store-opening bin in it.
    #[cfg(any(feature = "hist-replay", feature = "bench-hist"))]
    tracing::info!(
        store = %resolved.root.display(),
        rung = resolved.rung.as_str(),
        "hist store root resolved: {}",
        resolved.rung.why()
    );
    resolved
}

/// `<repo>/market_data/hist` for THIS crate — `CARGO_MANIFEST_DIR` ancestors, never the CWD —
/// **in a debug build; `None` in a release build.**
///
/// ⚠ The macro is a string literal in the binary, and `release.yml` builds the `backtest` asset
/// (and the `vike` multicall that links it) `--release` on a runner whose checkout path names the
/// runner account. The literal survived `--remap-path-prefix` (that flag rewrites what the COMPILER
/// emits, never what a crate embeds itself), so the release's box-path guard
/// (`scripts/refuse_box_paths.sh`) refused both assets by name. A release build therefore carries
/// no rung at all and resolves from the project walk down — the same directory, by a different rung
/// name, for anyone standing in a checkout; `vike_model::store_path`'s module doc (rung 4) argues
/// it. The attribute rather than `cfg!()`, because a runtime `if` still compiles the literal in.
fn repo_default() -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    {
        Some(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("market_data")
                .join("hist"),
        )
    }
    #[cfg(not(debug_assertions))]
    {
        None
    }
}

/// The WIRING, with `repo_default` and `cwd` injected so a test can reach the rungs a `cargo test`
/// run can never otherwise see.
///
/// Both are hard-coded facts of the process in [`store_root`] — the compile-time repo path always
/// EXISTS during `cargo test`, so the dev-checkout hinge always fires and the project/user rungs are
/// unreachable from a test of the public function. Taking them as parameters is what lets
/// `the_call_site_reaches_the_project_rung_when_this_box_has_no_checkout` prove that this crate's
/// call site is wired to the shared precedence and not to a local restatement of it.
///
/// ⚠ `resolve_store_root_from`, never the bare ladder: the ladder's project/user defaults are two
/// adjacent `Option<PathBuf>`s that a transposition would swap SILENTLY. Passing the working
/// directory and the environment map instead leaves no two arguments here of the same type.
fn resolve_with(
    explicit: Option<PathBuf>,
    repo_default: Option<&Path>,
    cwd: Option<&Path>,
    vars: &HashMap<String, String>,
) -> vike_model::store_path::StoreRoot {
    vike_model::store_path::resolve_store_root_from(
        explicit,
        vars.get(HIST_STORE_VAR).cloned(),
        repo_default,
        cwd,
        vars,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// The whole precedence chain. No longer ONE test function out of necessity: the old version
    /// had to be, because it MUTATED the process environment and a sibling test asserting the
    /// unset-fallback concurrently would have raced its `set_var`. The map parameter removes both
    /// the race and the mutation.
    #[test]
    fn store_root_precedence_explicit_then_env_then_repo_root() {
        // 1. Explicit (--store) wins over everything, env set or not.
        assert_eq!(
            store_root(Some(PathBuf::from("explicit")), &env(&[("VIKE_HIST_STORE", "env-store")])),
            PathBuf::from("explicit")
        );

        // 2. The env var, when no explicit path is given.
        assert_eq!(
            store_root(None, &env(&[("VIKE_HIST_STORE", "env-store")])),
            PathBuf::from("env-store")
        );

        // 3. Neither: the REPO-root default `<repo>/market_data/hist`, anchored two levels above this
        //    crate's manifest dir — never CWD (the cheap_np bins' original bug).
        let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("market_data")
            .join("hist");
        assert_eq!(store_root(None, &env(&[])), expected);
    }

    /// **A BLANK `--store=` must fall through, not resolve the store to `""`.**
    ///
    /// This became reachable the moment `vike_analytics::binutil::arg` learned the inline `=`
    /// spelling: before that, `--store=` matched nothing and the flag read as absent. Now it
    /// answers `Some("")` — deliberately, so a caller can refuse it by name — and
    /// `vike_model::store_path::resolve_store_root` honours an EXPLICIT path unfiltered (its
    /// `Explicit` rung returns before the `.filter(|s| !s.trim().is_empty())` that guards the env
    /// rung). So without this filter, `backtest --store=` opens `""` and `DataFusionHist::open`
    /// creates a store in the working directory.
    ///
    /// The filter lives HERE rather than at each bin, because all five store-driven bins share this
    /// one funnel and only one of them was going to remember.
    #[test]
    fn a_blank_explicit_store_falls_through_like_a_blank_env_var() {
        for blank in ["", "   "] {
            assert_eq!(
                store_root(Some(PathBuf::from(blank)), &env(&[("VIKE_HIST_STORE", "env-store")])),
                PathBuf::from("env-store"),
                "--store={blank:?} must fall through to the next rung, not resolve to itself"
            );
        }
        // …and a non-blank explicit path is untouched, so the filter buys nothing by refusing more.
        assert_eq!(
            store_root(Some(PathBuf::from("explicit")), &env(&[("VIKE_HIST_STORE", "env-store")])),
            PathBuf::from("explicit")
        );
    }

    /// **Behaviour preservation across the map lift.** A dev checkout (this repo, where the
    /// compile-time repo root exists) resolves to `<repo>/market_data/hist` no matter what the platform
    /// trio says — the compatibility hinge in `resolve_store_root`. This is the case every existing
    /// the CI box workflow takes, and it must be untouched on a unix-shaped AND a windows-shaped map.
    #[test]
    fn a_dev_checkout_is_unaffected_by_the_platform_trio_on_either_platform_shape() {
        let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .join("market_data")
            .join("hist");
        // Spelled through `vike_model::store_path`'s constants, not as bare literals: that crate is
        // the ONE place these three variable names appear, and a fixture that re-spelled them here
        // would put an incidental `vike-backtest` row back on the settings registry for a read this
        // crate no longer performs.
        use vike_model::store_path::{HOME_VAR, LOCALAPPDATA_VAR, XDG_DATA_HOME_VAR};
        let unix = env(&[(XDG_DATA_HOME_VAR, "/xdg"), (HOME_VAR, "/home/u")]);
        let windows =
            env(&[(LOCALAPPDATA_VAR, "C:\\Users\\u\\AppData\\Local"), (HOME_VAR, "C:\\Users\\u")]);
        assert_eq!(store_root(None, &unix), expected);
        assert_eq!(store_root(None, &windows), expected);
    }

    /// **THIS crate's call site, on the rung a `cargo test` run cannot otherwise reach.** The
    /// compile-time repo path always exists while testing, so the dev-checkout hinge always fires
    /// and `store_root` can never be observed resolving to a project. Injecting a `repo_default`
    /// that exists on no machine exposes the rest of the ladder through the SAME wiring the bins
    /// take.
    ///
    /// It is the transposition guard for this crate: the project and per-user rungs are made
    /// unmistakably different (a scratch project vs a fake `$HOME`), so a wiring that swapped them
    /// reddens on the value. Verified by mutation — swapping the two inside
    /// `vike_model::store_path::resolve_store_root_from` turns this red with the `$HOME` path.
    #[test]
    fn the_call_site_reaches_the_project_rung_when_this_box_has_no_checkout() {
        use vike_model::store_path::{
            HOME_VAR, LOCALAPPDATA_VAR, StoreRootRung, XDG_DATA_HOME_VAR,
        };

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let scratch = std::env::temp_dir().join(format!("vike-binutil-store-{nanos}"));
        let project = scratch.join("proj");
        let fake_home = scratch.join("home");
        std::fs::create_dir_all(project.join("settings")).unwrap();

        let no_checkout = Path::new("/definitely/not/a/real/build/machine/path/market_data/hist");
        let vars = env(&[
            (HOME_VAR, fake_home.to_str().unwrap()),
            (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
            (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
        ]);

        let got = resolve_with(None, Some(no_checkout), Some(&project), &vars);
        let user = vike_model::store_path::user_data_dir_from_vars(&vars).unwrap();
        let _ = std::fs::remove_dir_all(&scratch);

        assert_eq!(
            got.root,
            project.join("market_data").join("hist"),
            "the PROJECT's own data folder"
        );
        assert_eq!(got.rung, StoreRootRung::Project);
        assert_ne!(got.root, user, "the two rungs must be distinguishable in this fixture");
    }

    /// …and the same call site still honours everything stated ABOVE the project rung — the half
    /// that proves the injection above did not quietly bypass the shared precedence.
    #[test]
    fn the_call_site_still_prefers_what_was_stated() {
        let no_checkout = Path::new("/definitely/not/a/real/build/machine/path/market_data/hist");
        let vars = env(&[("VIKE_HIST_STORE", "/y/env")]);
        assert_eq!(
            resolve_with(Some(PathBuf::from("/x/explicit")), Some(no_checkout), None, &vars).root,
            PathBuf::from("/x/explicit")
        );
        assert_eq!(
            resolve_with(None, Some(no_checkout), None, &vars).root,
            PathBuf::from("/y/env"),
            "the map key this crate reads must still be VIKE_HIST_STORE"
        );
    }

    /// **The marker is PRESENT and it NAMES the convention actually in force.** Compared to the
    /// constant rather than to a second copy of the string: `vike_analytics::metrics` carries the
    /// one literal pin, beside the body being named, so a convention change is typed once.
    #[test]
    fn the_provenance_block_names_the_current_percentile_method() {
        let v = stats_provenance();
        assert_eq!(
            v["percentile_method"],
            serde_json::json!(vike_analytics::metrics::PERCENTILE_METHOD),
            "the block must publish the method the shared percentile actually computes"
        );
        assert_eq!(v["note"], serde_json::json!(PERCENTILE_NOTE));
        // Exactly these two keys: an added one is a deliberate act, and a reader of an OLD file has
        // to be able to trust that what this block does NOT say, it never said.
        let obj = v.as_object().expect("the block is an object");
        assert_eq!(obj.len(), 2, "unexpected keys in the provenance block: {obj:?}");
    }

    /// The note must carry the ABSENCE rule, because that is the half a reader cannot derive: an
    /// old file says nothing at all, so the new file is the only place the comparison can be
    /// refused. Asserted on the substance, not the punctuation — the sentence may be reworded, and
    /// may not quietly lose a fact.
    #[test]
    fn the_note_states_the_absence_rule_and_names_both_conventions() {
        for fact in [
            // the key whose absence IS the signal, spelled so a reader can grep for it
            "stats_provenance",
            // ...and what its absence means
            "nearest-rank",
            // the new convention, by name and by algorithm
            "percentile",
            "interpolat",
            // the reason a conversion is not on offer
            "no scale factor",
            "re-run",
        ] {
            assert!(PERCENTILE_NOTE.contains(fact), "the note dropped {fact:?}: {PERCENTILE_NOTE}");
        }
        assert!(
            PERCENTILE_NOTE.is_ascii(),
            "the note is pasted into terminals and grepped; keep it ASCII"
        );
    }
}
