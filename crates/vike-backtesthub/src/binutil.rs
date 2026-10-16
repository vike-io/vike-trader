//! `binutil` — the shared hist-store-root glue for this crate's bins,
//! plus the run-level provenance stamp their JSON carries
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
//! Feature-free on purpose (like `objective`): the module itself compiles in every build, and so does the store-driven
//! bins' code, since this crate links `vike-store` unconditionally.
//!
//! **The bins read their argv with `clap`** (decision 0155), each through its own `#[derive(Parser)]`
//! struct and `vike_util::cli::Program`; what they share lives here: [`bin_version_line`] and
//! [`CommaList`], the comma-separated flag value. The hand-rolled scanners
//! (`vike_util::argv::{arg, f64_arg, has_flag, parse_num}`) they used to share are no longer named
//! by any bin. Only [`store_root`] stayed a resolver: it is the one function here that resolves the
//! hist-store root, and the workspace settings registry (`vike_config::settings::SETTINGS`) keys
//! that declaration on `(name, krate)` — moving it would have retagged the row's crate. It takes the
//! explicit `--store` as a parameter and still falls through on a BLANK one, as a guard for a caller
//! that did not read its argv through `Program` (which refuses a blank value for every argument).
//!
//! [`store_root`] is now PURE: it takes the environment as an already-collected MAP and the four
//! bins that call it do the `std::env::vars()` sweep themselves. It used to read the
//! `XDG_DATA_HOME`/`HOME`/`LOCALAPPDATA` platform trio with `std::env::var` — inside a LIBRARY file, so all four carried `Layer::Library` rows on the
//! settings registry's STEP-2 work-list even though every caller is a binary in this same crate.
//! The lift also removes a real hazard the old unit test documented in its own comment: it had to
//! be ONE test function because it MUTATED the process environment, and a sibling test asserting
//! the unset-fallback concurrently would have raced its `set_var`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
/// `crates/vike-store/src/datafusion_hist/codec/book.rs`'s `BOOK_SCHEMA_VERSION` and its siblings, stamped
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
/// Only the percentiles. `crates/vike-backtesthub/src/bin/cheap_np_askgate/main.rs`'s `stderr` fold is
/// a separate convention that was deliberately left alone, and a key called `stats_method` would
/// have silently spoken for it. Nor is there a `git_sha`: naming a commit means `vike-buildinfo`,
/// which resolves it at COMPILE time, and this crate does not depend on it —
/// `crates/vike-runs/src/lib.rs`'s `RunManifest` writes `None` for exactly that reason.
pub fn stats_provenance() -> serde_json::Value {
    serde_json::json!({
        "percentile_method": vike_analytics::metrics::PERCENTILE_METHOD,
        "note": PERCENTILE_NOTE,
    })
}

/// The hist-store root: `explicit` (`--store`) > `<repo>/market_data/hist` when this box still has
/// that checkout > the project's own `<project>/market_data/hist` > the per-user `…/vike-data`. No
/// variable is a rung (decision 0111 retired `VIKE_HIST_STORE`); these bins open no settings
/// database, so `--store` names a store outright.
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
/// is its opposite: the walk requires a project MARKER (`crates/vike-paths/src/state_path.rs`'s
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

/// [`store_root`] keeping the [`StoreRoot`](vike_paths::store_path::StoreRoot) — the path AND the
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
) -> vike_paths::store_path::StoreRoot {
    let cwd = std::env::current_dir().ok();
    // ⚠ A BLANK `--store` falls through instead of resolving the store to `""`.
    //
    // Reachable since `vike_util::argv::arg` learned the inline `=` spelling: `--store=`
    // used to match nothing and read as absent, and now answers `Some("")` — deliberately, so a
    // caller can refuse a blank by name. But `vike_paths::store_path::resolve_store_root` honours an
    // EXPLICIT path unfiltered (the `Explicit` rung returns before the
    // `.filter(|s| !s.trim().is_empty())` that guards the configured rung), so `""` would reach
    // `DataFusionHist::open`, which `create_dir_all`s whatever it is given — a store minted in the
    // working directory, and a run reporting zero rows.
    //
    // Here rather than at each bin because all five store-driven bins share this ONE funnel, and
    // the same rule the configured rung already applies is the one an operator expects of a blank
    // flag.
    let explicit =
        explicit.filter(|p| !p.as_os_str().is_empty() && !p.to_string_lossy().trim().is_empty());
    let resolved = resolve_with(explicit, repo_default().as_deref(), cwd.as_deref(), vars);
    // `tracing` is a plain dependency of this crate since the 2026-09-27 feature collapse (it was
    // optional, pulled in by `hist-replay`/`bench-hist`, both since deleted), so this log is
    // unconditional now.
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
/// ⚠ The macro is a string literal in the binary, and `release.yml` builds the multicall that links
/// it (`vike-backend`; a standalone `backtest` asset too, when this was measured) `--release` on a
/// runner whose checkout path names the runner account. The literal survived `--remap-path-prefix`
/// (that flag rewrites what the COMPILER emits, never what a crate embeds itself), so the release's
/// box-path guard
/// (`scripts/refuse_box_paths.sh`) refused both assets by name. A release build therefore carries
/// no rung at all and resolves from the project walk down — the same directory, by a different rung
/// name, for anyone standing in a checkout; `vike_paths::store_path`'s module doc (rung 4) argues
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
) -> vike_paths::store_path::StoreRoot {
    vike_paths::store_path::resolve_store_root_from(explicit, None, repo_default, cwd, vars)
}

/// **The history `route` names, opened with the datahub hub keys of the credential store
/// [`vike_secrets::open_credential_store`] opens out of `vars`** — the
/// one-shot readers' half of `docs/decisions/0084-only-the-datahub-touches-the-store.md`. The keys
/// are the DATAHUB family's alone (`vike_venues::credential_keys::is_datahub_pair_key`), and an
/// UNREADABLE store warns and continues (`vike_datahub_client::route::routed_hub_keys`, which
/// argues that disposition); the `--addr` daemon reads its keys itself, under a stricter one.
///
/// # Errors
/// `vike_secrets::ScopeError` when `$VIKE_NO_LIVE_KEYS` is set to anything but `true` or `false`:
/// the caller refuses to start on it, printing the message as is.
pub fn routed_history(
    route: &vike_datahub_client::route::HistoryRoute,
    vars: &HashMap<String, String>,
) -> Result<Box<dyn vike_data::HistStore + Send + Sync>, vike_secrets::ScopeError> {
    let store = vike_secrets::open_credential_store(vars)?;
    let keys = vike_datahub_client::route::routed_hub_keys(
        store.hub_keys(vike_venues::credential_keys::is_datahub_pair_key),
        route.hub(),
    );
    Ok(vike_datahub_client::route::open_routed_history(route, keys))
}

/// The `--version` line of a bin of this crate: `<bin> <package version>`, the shape every other
/// `--version` on the box prints. (No build identity: this crate cannot name `vike-buildinfo`,
/// `crates/vike-buildinfo/tests/identity_adoption.rs`'s `WITHOUT_IDENTITY` says why.) Each bin builds
/// its own `vike_util::cli::Program` from it, because only a binary reads the process's argv.
pub fn bin_version_line(bin: &str) -> String {
    format!("{bin} {}", env!("CARGO_PKG_VERSION"))
}

/// A comma-separated list flag (`--live-ms 954,4229`, `--sizes 10,50,100`) as ONE `clap` value.
///
/// A newtype rather than `Vec<T>` so that `clap`'s derive reads the flag as a single value that is
/// split here, instead of as a repeatable flag: `--sizes 10,50` and `--sizes=10,50` are the
/// spellings, and a repeated `--sizes` is refused like every repeated flag. Parts are trimmed, an
/// EMPTY part is skipped (`1,,2`, a trailing comma — what the hand-rolled splitters did), and a
/// part that is not a `T` is a refusal naming it. (They used to be dropped by `filter_map` in
/// `--sizes`/`--delays-ms` and refused by hand in `--live-ms`/`--latency-ms`: one rule now.)
#[derive(Clone, Debug, PartialEq)]
pub struct CommaList<T>(pub Vec<T>);

impl<T: std::str::FromStr> std::str::FromStr for CommaList<T> {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, String> {
        raw.split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| part.parse::<T>().map_err(|_| format!("bad list entry {part:?}")))
            .collect::<Result<Vec<T>, String>>()
            .map(CommaList)
    }
}

#[path = "binutil_tests.rs"]
#[cfg(test)]
mod binutil_tests;
