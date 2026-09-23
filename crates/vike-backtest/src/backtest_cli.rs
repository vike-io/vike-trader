//! `backtest` — the run driver, as a LIBRARY function.
//!
//! ⚠ This was `src/bin/backtest.rs`'s body until the multicall merge. `main` became [`run`], and
//! TWO pieces of ambient state became parameters rather than moving with it:
//!
//! * the ENVIRONMENT, because `crates/vike-ops/tests/settings_registry.rs` asks libraries to take
//!   configuration as parameters and only binaries to read the process;
//! * the CLOCK, because `crates/vike-ops/tests/clock_pin.rs`'s `CLOCK_PIN` is a ratchet keeping
//!   ambient clock reads out of the library tree. A composition root may read a clock; everything
//!   below it takes the timestamp. The bin supplies the real one.
//!
//! Both ratchets are satisfied by threading, never by exemption. Everything below is the binary's
//! own documentation, unchanged.
//!
//! Usage (the operator-facing copy is the `USAGE` const below, which `backtest --help` prints):
//!   backtest --help | --version
//!   backtest --list
//!   backtest run.toml [--store DIR] [--json]
//!   backtest paramscan.toml [--store DIR] [--json] [--rank-by sharpe|return|max_dd|equity|multi]
//!                       [--optimizer grid|euler|tpe|genetic]
//!                       [--euler-depth N] [--trials N] [--seed S]
//!   backtest data <fetch|fetch-starter|seed-demo|export|rm> …   (`backtest data --help`)
//!
//! ⚠ **The five data-management flags are RETIRED (ruling 12).** `--seed-demo`, `--fetch`,
//! `--fetch-starter`, `--export` and `--rm-series` were never about backtesting — they fetch from
//! venues, write the store, export from it and DELETE from it — so the operator-facing spelling is
//! `vike-cli data <sub>` now, and each old flag is refused by name
//! ([`refuse_a_retired_data_flag`]). The WORK did not move and could not: every one of them opens a
//! `DataFusionHist`, and `vike-cli` links no DataFusion at all — so `vike-cli data` SPAWNS this
//! binary as `backtest data <sub>` ([`run_data`]), which is also the spelling to type by hand on a
//! box that has an engine and no `vike-cli`.
//!
//! ⚠ The profile is POSITIONAL (ruling 14 of
//! `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md`), and `--profile PATH` is KEPT as
//! the older spelling because `crates/vike-cli/src/cmd/backtest.rs`'s `--local` arm SPAWNS this
//! binary with it. Giving both is refused rather than resolved — see [`profile_from_args`], which
//! also refuses a lone `data` positional, since [`run`] routes that word to a subcommand.
//!
//! `--list` prints every registered strategy name (`harness::STRATEGIES`) and exits — no store,
//! no profile needed. Otherwise a profile is required: the profile (`BacktestProfile::from_path`)
//! names the data slice (bar or tick mode — see `harness::run_backtest`'s doc comment for how the
//! two modes differ) and the strategy to run. `--store` picks the hist-store root; it falls back
//! to `$VIKE_HIST_STORE`, then `<repo>/market_data/hist` (same convention as `vike-backfill`'s backfill
//! bins, e.g. `eod_backfill`). `--json` prints `BacktestReport` (or, for a sweep, `ParamscanReport`)
//! as pretty JSON instead of the human table.
//!
//! **A finished single run also PERSISTS** (`persist_run` below): a `<run_id>` directory under
//! `<project>/user_data/runs/` gets `manifest.json` — the COMMON, kind-agnostic manifest documented
//! on `vike_model::runs::RunManifest` — beside `report.json`, the same `BacktestReport` `--json`
//! prints. Until this existed the bin saved NOTHING, so there was no history and nothing a UI could
//! list. It is ADDITIVE: the report reaches stdout first and a persist failure is reported on
//! stderr without changing the exit code, so saving a run can never become a way to lose one.
//! `--json` stdout is byte-identical to before.
//!
//! ⚠ **A SEARCH persists too, and this paragraph said the opposite until stage 5.** A grid, euler,
//! tpe or genetic search mints a parent run of its own (`open_search_run`/`write_search_run`) whose
//! `kind` is `trial_ledger::SEARCH_RUN_KIND` and whose `report.json` is a
//! `trial_ledger::TrialsDocument` rather than a `ParamscanReport` — a different document because it
//! is a different question, which is what the separate kind records.
//!
//! ⚠ **A search can also carry ANTI-OVERFITTING STATISTICS in that document, and that costs an
//! opt-in.** `--keep-trials returns` makes each trial retain a bucketed return vector
//! (`harness::sweep::ReturnBuckets`), which is the trial MATRIX
//! `vike_analytics::overfit::pbo_cscv` and `deflated_sharpe_with_effective_n` need and which the
//! search path used to destroy before a row existed; the numbers land as
//! `trial_ledger::OverfitStats` on that `report.json`, where `vike-cli backtest gate --fail-if`
//! names them as `overfit.pbo` and siblings. They are REPORT FIELDS and nothing prints them —
//! that is the owner's ruling, and it is what makes them actionable by a CI step rather than one
//! more line to read. [`KeepTrials`] carries what each mode retains and why `series` is still
//! refused.
//!
//! ⚠ **And since the `series_facts` merge it is content-ADDRESSED like a single run.** It records
//! the same `run_fingerprint::input_fingerprint` over the same facts and carries it in its run id,
//! for the one manifest parse per series it was already spending on its resume witness. The note
//! that used to stand here — a search records `fingerprint: null` and keeps the pid form, because
//! the collector costs two parses per series — described a cost that no longer exists;
//! `collect_data_fingerprint` and `run_detail_data` carry what changed.
//!
//! A profile with a `[paramscan]` table — or its permanent `[sweep]` alias —
//! (`profile.is_paramscan()`) runs `harness::run_paramscan` instead of
//! a single `run_backtest`: the strategy runs once per point in the sweep's cartesian parameter
//! grid, and the result is a ranked `ParamscanReport` table rather than one `BacktestReport`.
//! ⚠ **This BIN now SAYS `[paramscan]` too, and the exclusion that stood here is WITHDRAWN.** It
//! read "this bin's own usage text and refusals still say `[sweep]` and are deliberately untouched
//! by the rename" — true when written, and a surface that tells an operator to write the OLD
//! spelling of a section it has renamed is a rename half done. [`USAGE`] and the
//! no-table refusal name `[paramscan]`; `crates/vike-backtest/tests/optimizer_cli.rs`'s
//! `a_search_on_a_profile_with_no_sweep_table_is_refused_rather_than_ignored` is the test that
//! moved with them.
//!
//! ⚠ **What did NOT change is what the binary ACCEPTS.** `[sweep]` is a PERMANENT serde alias
//! (`harness::profile`'s `#[serde(default, alias = "sweep")]`, whose doc calls removing it a
//! breaking change to every profile ever written, not a tidy-up), and every profile spelling it —
//! this repository's own fixtures included — still loads. This is a rename of what the binary SAYS,
//! never of what it READS.
//! `--rank-by` picks the ranking metric (default `sharpe`). A VALID value is ignored — not an
//! error — on a non-sweep profile, which is what distinguishes it from `--optimizer`: it names how
//! to ORDER results, not what work to do. ⚠ An INVALID value is refused on either profile shape
//! now, where the old ladder swallowed a typo on the non-sweep one. `--rank-by multi` ranks by the
//! composite `crate::objective` multi-metric score instead (`objective::multi_metric` with default
//! `MultiMetricParams`) — rows gain a `score` column/field; the four classic metric names keep the
//! score-clearing classic path on the grid, and their output is byte-identical.
//!
//! **`--optimizer` names the search METHOD, and it is the ONE selector** (ruling 13: there is no
//! `optimize` verb and no `compute` verb — the word lives in the flag). It replaced a hand-written
//! ladder that parsed each flag inside the branch that used it; [`parse_search_flags`] carries what
//! that cost and the four defects it produced. `--search` is RETIRED and refused by name.
//!
//! `--optimizer euler` replaces the exhaustive cartesian grid with a bounded successive-halving
//! refinement (`harness::euler::EulerSearch`): the coarse grid runs once, then the per-axis step is
//! halved up to `--euler-depth` times (default 3) around the running best point. Each completed
//! halving doubles the effective resolution, for a small fraction of the backtests an equally fine
//! grid would cost — at the cost of being a LOCAL refinement (it can only descend into the basin the
//! coarse grid already found) and of requiring every `[sweep]` axis to be numeric AND
//! type-homogeneous (no mixed `[1, 2.5]`). The stderr budget line compares against the grid matching
//! the depth ACTUALLY reached, so a search that ran out of new candidates early reports the smaller,
//! honest saving. Scoring reuses `--rank-by` verbatim (the same objective seam), rows always carry
//! a `score`, and a one-line budget summary goes to stderr.
//!
//! `--optimizer tpe` is the Bayesian (Tree-structured Parzen Estimator) ask/tell search
//! (`harness::tpe::TpeSearch`) — the smart alternative to enumerating the grid, converging on good
//! params in `--trials` backtests (default 64) by modelling which regions score well. `--seed`
//! (default 0) makes it fully reproducible. It reuses `--rank-by` as its objective (rows carry a
//! `score`) and returns the same ranked `ParamscanReport`.
//!
//! `--optimizer genetic` is the population search (`harness::genetic::GeneticSearch`) — the FOURTH
//! method, wired here by the follow-up `crates/vike-backtest/src/harness/genetic.rs` named when it
//! landed without a dispatch. Its genome is INDICES into the authored `[sweep]` axes, so unlike
//! euler and tpe it explores no point BETWEEN the values somebody wrote down; what it buys instead
//! is a combinatorial search whose reachable set is exactly the grid's, at a budget derived from
//! the space and hard-capped at what enumeration would have cost. Every sizing knob
//! (`population`, `generations`, `max_evaluations`) derives from the space and has NO flag —
//! deliberately, for now: this PR wires the method, and each knob is a spending decision that owes
//! its own argument and its own `METHOD_KNOBS` row. `--seed` is the one input it takes, and it is
//! REQUIRED rather than defaulted — [`require_seed`] carries that argument in full.
//!
//! `--optimizer grid` (the default) is byte-identical to before, and deliberately so: under one of
//! the four classic `--rank-by` metrics it is the one combination whose rows carry NO `score`, and
//! it is exactly what `crates/vike-cli/src/cmd/backtest.rs`'s `--local` arm spawns and prints
//! verbatim (that arm absorbed `vike-cli sweep`'s when ruling 13 deleted the second verb).
//! [`run`]'s evaluator construction is where that is kept and argued.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use crate::binutil::{arg, has_flag, store_root};
// The SELECTOR — every method name, every knob's owner, every value parser and every refusal
// sentence — lives in `harness::search_select` since stage 7, because `crate::compute_server`
// resolves the identical rule for a REMOTE run and two copies is the defect that stage killed.
// ⚠ `Optimizer` and `RankMetric` became TEST-ONLY here with that move: the value parsers and the
// `(method, rank)` evaluator match left this file, so the lib half names neither. The test module
// still reads `GridSearch.name()` (which needs the trait in scope) and builds a `RankMetric` to pin
// `--rank-by`'s parse, so the imports are gated rather than deleted.
use crate::harness::search_select::{self, METHOD_KNOBS};
// The two OBSERVER types this file's argv door resolves and `search_select::arm_observers` arms, BY
// MODULE PATH: `harness/mod.rs`'s `pub use` block is the optimizer SEAM's vocabulary and re-exports
// neither, deliberately — an observer a caller arms on an evaluator is the evaluator module's own
// knowledge, the same rule that keeps `genetic::GeneticConfig` at its module path.
use crate::harness::optimize::{ProgressMode, TradeFloor};
use crate::harness::{self, BacktestProfile, RankChoice, SearchMethod, SearchSelection};
#[cfg(test)]
use crate::harness::{Optimizer, RankMetric};
use crate::run_fingerprint;
// Still imported after the value parsers moved into `harness::search_select`: this file's own
// `#[cfg(test)] mod search_flag_tests` drives the euler cap through the REAL argv parser, and it
// reads the cap off the config rather than writing a number.
#[cfg(test)]
use crate::search::EulerConfig;
// ⚠ The method ROSTER and its default are the PROTOCOL crate's, not a local literal: `vike-cli`
// spelling-checks `--optimizer` against the same const, and `vike-datahub-client` is the only crate
// both it and this one take as a normal dependency. `vike_datahub_client::proto`'s
// `SEARCH_METHODS` carries the argument.
use vike_datahub_client::{DEFAULT_SEARCH_METHOD, SEARCH_METHODS};
// The hist ROUTE. ⚠ It was DECLARED here until 2026-09-23 and moved DOWN, because `vike-report`
// became the fourth reader and cannot name this crate: both declare layer 45, and
// `crates/vike-ops/tests/layer_gate.rs` refuses a same-rank edge. The home was arithmetic rather
// than taste — `vike_datahub_client::route`'s own module doc argues it, and the comment above
// states the same fact from the other side: this is the one crate both `vike-cli` and this one
// take as a normal dependency, which is what makes it a place two readers can agree in.
use vike_datahub_client::route::{HistoryRoute, history_route, open_routed_history};
// The search ARTIFACT's documents. UNGATED, so this import costs nothing a default build does not
// already pay — `crate::trial_ledger`'s module doc says why they live outside `harness`.
use crate::trial_ledger;
use vike_model::runs;
// Gated exactly as the module is: a `datafusion-store`-only build has no `starter`, and an
// unconditional import made that configuration fail to compile — a build CI runs and I did not.
#[cfg(feature = "venue-fetch")]
use crate::starter;
use vike_data::DataFusionHist;
// The TRAIT, so the ONE `Arc<dyn HistStore + Send + Sync>` binding above the sweep branch can be
// annotated. The unsize coercion used to happen implicitly at four `Arc::new(store)` call sites,
// one per ladder arm; it happens once now, and the annotation is what performs it.
use vike_data::HistStore;
use vike_data::demo as demo_tape;

// `periods_per_year` (the Sharpe annualization factor) now lives in `harness::report` as the SINGLE
// source of truth, so the single-run path here and the sweep rank an identical profile on the same
// Sharpe scale. Imported below as `harness::report::periods_per_year`.

/// What `--help` prints. A const rather than the module doc above, because only a const can reach a
/// user: that doc is for a reader of this file, `backtest --help` is for everyone else.
const USAGE: &str = "\
usage: backtest PROFILE.toml [--store DIR] [--json]
       backtest PARAMSCAN.toml [--rank-by sharpe|return|max_dd|equity|multi]
                [--optimizer grid|euler|tpe|genetic]
                [--euler-depth N] [--trials N] [--seed S]
                [--keep-trials none|scalars|returns] [--resume ID]
                [--min-trades N] [--progress auto|none|json]
       backtest --addr [HOST:PORT]
       backtest --list
       backtest --list-optimizers [--json]
       backtest data <subcommand> [options]     (see `backtest data --help`)
       backtest trials <search-run-id> [options] (see `backtest trials --help`)

  --addr [ADDR]    STAY ALIVE and serve the seven COMPUTE verbs over the node protocol —
                   RunBacktest, RunSlice, RunParamscan, RunWalkforward, RunParamscanProfile,
                   RunWalkforwardProfile, ListStrategies. The VALUE IS OPTIONAL: a bare --addr
                   serves on the configured address (VIKE_BACKTEST_ADDR, then
                   config.backtest_addr, then 127.0.0.1:7880); --addr HOST:PORT overrides it.
                   The FLAG is what says 'become a daemon' — no profile is ever implied.
                   A non-loopback address is refused unless VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 is
                   set AND node keys are configured: this server compiles Rhai the client sends.
                   The DATA verbs (LoadBars, ListSeries, Backfill, ...) live on the other daemon,
                   `vike-backend datahub`, and are refused here by name
  PROFILE.toml     the harness profile TOML (data slice + engine costs + strategy), as the first
                   POSITIONAL argument. REQUIRED unless --list. A profile with a [paramscan]
                   table — or its permanent [sweep] alias — runs a parameter search instead of
                   one backtest.
  --profile PATH   the same thing, the older spelling. KEPT: `vike-cli backtest run --local` SPAWNS
                   this binary with it. Giving BOTH is an error — they may name two different
                   files, so it is refused rather than resolved.
  --store DIR      hist-store root; falls back to $VIKE_HIST_STORE, then <repo>/market_data/hist
  --archive PATH   read DOWNLOADED `data.vike.io` archive Parquet IN PLACE instead of an imported
                   store — a directory of day files, or one file. No import step: the import is
                   measured at ~16 s per 999k-row row group on a flat-layout day carrying 300 of
                   them, because each group splits across as many destination partitions as it
                   holds distinct token_ids. Refused together with --store: two different stores,
                   and a silent precedence rule is how somebody backtests the wrong data. A run
                   this way records `fingerprint: null` — the part-file witness needs a
                   DataFusionHist store, and the reason is printed
  --json           print the report as pretty JSON instead of the human table
  --list           print every registered strategy name and exit — no store, no profile needed
  --list-optimizers
                   print every parameter-search METHOD, one per line, and exit — no store, no
                   profile, no network. With --json, one object carrying the count and which
                   method runs when --optimizer is absent. The `--optimizer` twin of --list
  data <sub>       get data INTO the store, out of it, and OUT of existence. The operator-facing
                   spelling is `vike-cli data <sub>`, which spawns this; on a box with only the
                   engine, `backtest data <sub>` is the same thing typed directly. It replaced
                   the five flags --seed-demo/--fetch/--fetch-starter/--export/--rm-series, each
                   of which is now refused by name
  trials <id>      list the trials of a finished (or interrupted) parameter SEARCH. Reads the run
                   ARTIFACT only — no store, no profile, no network
  --rank-by M      paramscan ranking metric (default sharpe); `multi` is the composite objective.
                   Ignored — not an error — on a profile with no [paramscan] table
  --optimizer M    the parameter-search METHOD: grid (default, the exhaustive cartesian product),
                   euler (bounded successive-halving refinement), tpe (Bayesian ask/tell) or
                   genetic (a population search over the grid's own points, which needs --seed).
                   Replaces the retired --search, which is now an error naming this flag
  --euler-depth N  euler halving depth (default 3). EULER ONLY — refused, not discarded, under
                   another method; past the cap it is refused rather than silently clamped
  --trials N       tpe trial budget (default 64). TPE ONLY — refused under another method
  --seed S         the reproducibility seed. TPE AND GENETIC — refused under grid and euler. On
                   tpe it defaults to 0; on genetic it is REQUIRED, because a genetic run reports
                   one sample of a distribution and a seed nobody typed is a constant the answer
                   silently depends on
  --keep-trials K  what a SEARCH leaves behind: `scalars` (default) writes one ledger line per
                   trial under <project>/user_data/runs/<id>/trials.jsonl; `none` keeps the parent
                   run and no ledger; `returns` is `scalars` PLUS a bucketed return vector kept
                   per trial in memory, which adds the anti-overfitting statistics (PBO via CSCV,
                   effective trial count, deflated Sharpe) to that run's report.json as
                   `overfit.*` — the keys `backtest gate --fail-if` can name. `series` (a whole
                   equity curve per trial) is still refused by name, on retention grounds; use
                   `returns` for the statistics, or re-run the winning point for a curve
  --resume ID      continue an interrupted SEARCH: re-open that parent run, reuse every trial it
                   already holds, and evaluate only what is missing. Refused if the profile, the
                   store, the optimizer, the seed or the budget differ from the run being resumed
  --min-trades N   the statistical-significance FLOOR: a trial that closed fewer than N trades is
                   UNRANKABLE rather than merely penalised, under every method and every
                   --rank-by. 0 (the default, and also what an explicit `--min-trades 0` means)
                   disarms it, so a sweep matrix can pass the flag unconditionally
  --progress M     which progress stream a SEARCH emits, on stderr and never stdout: auto (the
                   default — the throttled human line, and ONLY when stderr is a terminal), none
                   (silent, nothing counted), json (one newline-delimited event per point,
                   terminal or not, unthrottled)
  -h, --help       print this and exit 0
  -V, --version    print the version and exit 0";

/// What `backtest data --help` prints — the five operations ruling 12 moved, as one subcommand.
///
/// A const of its own rather than five more rows in [`USAGE`], because that is the shape of the
/// move: `backtest --help` now names ONE line where it named five, and the detail lives with the
/// verb that carries it. The wording mirrors `crates/vike-cli/src/cmd/data.rs`'s `USAGE` on
/// purpose — the same five operations, spelled the same way, reached by whichever of the two
/// binaries the operator has.
const DATA_USAGE: &str = "\
usage: backtest data fetch-starter [--store DIR]
       backtest data seed-demo [--store DIR]
       backtest data export VENUE:SYMBOL:INTERVAL --out FILE [--from LABEL] [--to LABEL]
                [--store DIR]
       backtest data rm --kind K --venue V (--symbol S [--interval I] | --group G)
                [--produced-by PREFIX] [--dry-run] [--yes] [--store DIR] [--json]
       backtest data repair --kind K --venue V (--symbol S [--interval I] | --group G)
                [--yes] [--dry-run] [--store DIR] [--json]

⚠ `vike-cli data <sub>` is the operator-facing spelling and takes the same words; it SPAWNS this
  binary, because opening a hist store needs DataFusion and that binary links none. Use this one
  directly on a box that has an engine and no vike-cli.

  fetch SPEC       RETIRED — this binary reaches no venue. It refuses and names the replacement,
                   `vike-cli data fetch VENUE:SYMBOL:INTERVAL --days N [--addr HOST:PORT]`, which
                   asks a DATAHUB to fetch the window into the store. ⚠ Not a drop-in: that needs
                   a reachable datahub, and this command needed no server at all
  fetch-starter    download the PUBLISHED starter dataset (real bars, plain HTTPS, no venue and
                   no credentials) and load it. For a box a venue cannot be reached from —
                   a geoblock, a locked-down network. Verified against its published SHA256SUMS;
                   safe to re-run. Behind the `venue-fetch` feature, whose name now outlives the
                   venue call it was created for — see `crate::starter`
  seed-demo        write the SYNTHETIC demo tape into the store and exit. Venue `demo`, a
                   closed-form curve, NOT market data; it is the slice the shipped
                   `user_data/profiles/backtest.toml` names, so a fresh install can run that
                   profile immediately. Safe to re-run: a second seed writes nothing
  export SPEC      write one series from the store to a standalone Parquet file (--out FILE),
                   optionally bounded by --from/--to — INDEPENDENTLY, unlike fetch's window:
                   an export slices what the store already holds, so one bound alone is
                   meaningful and neither is required. Any venue the store holds, `demo` included
  rm               DELETE stored series, IRREVERSIBLY, and exit. Selects on the four series
                   dimensions: --kind and --venue are REQUIRED, and an omitted
                   --symbol/--group/--interval is a wildcard over that dimension. Prints the
                   PLAN first — the resolved store root and the rung that chose it, then every
                   matched series with its rows/bytes/days and the commit keys that wrote it.
                   --produced-by asserts that EVERY key of EVERY matched series carries that
                   prefix (a producer path from STORE_KINDS resolves to its prefix); one foreign
                   key refuses the whole run and deletes nothing. It is REQUIRED for a sweep and
                   optional for a fully-named series. --dry-run stops after the plan; otherwise
                   --yes, or type `delete N series` at a terminal. There is no --force
  repair           REBUILD one series' manifest from its parts — the repair `read_manifest`
                   names when a series' index is missing or unreadable. Selects ONE series
                   exactly (never a wildcard: --symbol or --group is REQUIRED, and --interval
                   too on `bar`), because the failure it fixes is invisible to every
                   enumeration this store has. REHEARSES BY DEFAULT: it prints what the rebuild
                   would recover and what it would LOSE, writes nothing, takes no lock, and
                   exits 0. --yes performs it; --dry-run wins over --yes. A rebuild that
                   recovers the index but not the idempotency log exits NON-ZERO and says so.
                   It REFUSES while another writer holds the series lock, and never waits

  --store DIR      hist-store root; falls back to $VIKE_HIST_STORE, then <repo>/market_data/hist
  --json           on `rm` and `repair`, print the plan/outcome document instead of the human
                   rendering
  -h, --help       print this and exit 0";

/// The optimizer-listing spelling, typed ONCE: [`run`]'s arm reads it, [`USAGE`] advertises it, and
/// `the_usage_spells_every_progress_mode_the_parser_accepts` holds the two together while
/// `the_optimizer_listing_flag_is_the_spelling_the_vocabulary_declares` holds it against
/// `vike_datahub_client::flag_vocab`'s row. Three surfaces name this flag and none of them may
/// spell it differently — an arm reading `--list-optimizer` would be a flag nothing can reach while
/// every other check stayed green.
const LIST_OPTIMIZERS_FLAG: &str = "--list-optimizers";

/// Run the `backtest` verb.
///
/// ⚠ `studio` is the ruling-7 SEAM, and it is a parameter for a reason a caller cannot see from the
/// type: the three Studio verbs the `--addr` daemon serves run `vike_studio_core`'s slice runners,
/// and that crate sits ABOVE this one in the layer graph (55 against 50 — and it DEPENDS on this
/// crate, so the edge could never point the other way). Only a composition root that can name both
/// may hand them down. `crates/vike/src/main.rs`'s `backtest_main` passes
/// `Some(vike_studio_core::studio_run_table())`; the standalone `src/bin/backtest.rs` passes `None`
/// and the daemon then refuses those three by name while serving the other four. Every non-`--addr`
/// invocation ignores it entirely.
pub fn run(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    now_unix_secs: &dyn Fn() -> i64,
    // ⚠ What the CALLING BINARY can say about its own build — see `vike_model::runs::BuildStamp` for why
    // this is a parameter. `crates/vike/src/main.rs`'s `backtest_main` fills it; the standalone
    // `crates/vike-backtest/src/bin/backtest.rs` cannot and passes `None`.
    build: Option<runs::BuildStamp<'_>>,
    studio: Option<crate::compute_server::StudioRunTable>,
    // ⚠ The STUDY runner, the same INJECTED shape and for the same layer reason — see
    // `crate::compute_server`'s `StudyRunFactory`. ⚠ A FACTORY rather than a built runner: the
    // dispatcher may not resolve a project directory at all
    // (`crates/vike-ops/tests/multicall_gate.rs`'s `the_dispatcher_starts_nothing`), so it NAMES
    // the constructor and the `--addr` arm below — which already owns this daemon's own walk —
    // hands it the runs root and the pinned trainer. Mounted SEPARATELY from `studio` because it is a
    // separately-negotiated capability: `vike-backend backtest --addr` fills both, the standalone
    // bin fills neither.
    study: Option<crate::compute_server::StudyRunFactory>,
) -> ExitCode {
    // ⚠ argv FIRST, and `--help`/`--version` BEFORE `vike_log::init` — answering either must not
    // create a log directory. `--help` was not recognised at all: it fell through to the
    // required-argument check below, so `backtest --help` answered "--profile <path> is required"
    // on stderr with exit 2, telling the user they had forgotten a flag they never meant to pass.

    if has_flag(args, "--help") || has_flag(args, "-h") {
        // stdout + exit 0: help is normal output a user pipes into a pager, not a diagnostic.
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if has_flag(args, "--version") || has_flag(args, "-V") {
        // `<name> <version>` — the shape every `--version` on the box prints (`git version 2.x`).
        // The name is spelled literally because `CARGO_PKG_NAME` is the PACKAGE (`vike-backtest`)
        // while this binary is `backtest`, and a `--version` answering with a name the caller did
        // not invoke is exactly what a bug report cannot use.
        println!("backtest {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    // `project_dir`: default the rolling trace file to `<project>/settings/state/logs` instead of
    // vike-log's `<exe_dir>/logs` last resort (`target/debug/logs/…`, which `cargo clean` deletes).
    // `$VIKE_LOG_DIR` still wins; no project above the CWD still lands beside the exe.
    let _log_guards = vike_log::init(vike_log::LogConfig {
        file_prefix: "backtest".to_string(),
        project_dir: std::env::current_dir()
            .ok()
            .and_then(|cwd| vike_model::state_path::project_log_dir(&cwd)),
        ..Default::default()
    });

    if has_flag(args, "--list") {
        for name in harness::STRATEGIES {
            println!("{name}");
        }
        return ExitCode::SUCCESS;
    }
    // ⚠ **The optimizer LISTING — the door `harness::search_select`'s two renderers said did not
    // exist.** Both carried a "BUILT AND DEFERRED: no door calls this" note naming this flag, and
    // this arm is what withdraws it. It RENDERS
    // `vike_datahub_client::SEARCH_METHODS` through those functions rather than naming a method
    // here, which is the whole content of their "one roster, every door" claim: a fifth method
    // reaches this listing with no edit in this file.
    //
    // Placed beside `--list` and for its reasons, not merely near it: it answers out of a const,
    // needs no store, no profile and no network, and it must sit ABOVE `parse_search_flags` and
    // `profile_from_args` so a listing is never asked for a profile it has no use for.
    //
    // ⚠ `has_flag(args, "--list")` above is EXACT-TOKEN, so this longer spelling does not trip it
    // and the order of the two arms is free — the adjacency landmine
    // `vike_datahub_client::flag_vocab`'s `spec` is anchored against, live in this file's own argv.
    //
    // ⚠ `--json` is read with `has_flag` too, so `--list-optimizers --json=1` prints the PLAIN
    // listing. That is the residual `vike_analytics::binutil`'s `has_flag` declares for every bare
    // boolean in this family, inherited here rather than newly created — closing it is the
    // unknown-argument triage that doc names, on every flag at once.
    //
    // ⚠ TWO residuals declared rather than left to be found, both SHARED with `--list` and neither
    // newly created here. This arm sits ABOVE the `data` and `trials` subcommand routing, so
    // `backtest data rm … --list-optimizers` prints the roster and exits 0 instead of reaching that
    // verb's own triage — exactly as `--list` does today. Moving one of the two below the routing
    // and not the other would leave two sibling listings in two places, which is how the next
    // reader "tidies" the wrong one; closing it for both is the same unknown-argument triage named
    // above. And nothing gates the SPELLING against argv itself: the arm reads
    // [`LIST_OPTIMIZERS_FLAG`], which
    // `the_optimizer_listing_flag_is_the_spelling_the_vocabulary_declares`
    // holds against the vocabulary row and the usage text — three surfaces, one const — but no test
    // drives this binary over the flag, because `run` needs an environment, a clock and a store
    // root. `crates/vike-backtest/tests/help_cli.rs` is where that would live.
    if has_flag(args, LIST_OPTIMIZERS_FLAG) {
        if has_flag(args, "--json") {
            println!("{}", search_select::optimizer_roster_json());
        } else {
            println!("{}", search_select::optimizer_roster_lines());
        }
        return ExitCode::SUCCESS;
    }
    // ⚠ **RULING 12 — the five data-management operations left this binary's FLAG surface.**
    // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0.7: `--fetch`,
    // `--fetch-starter`, `--seed-demo`, `--export` and `--rm-series` fetch from venues, write the
    // store, export from it and DELETE from it — none of which is backtesting — so their
    // operator-facing spelling is `vike-cli data <sub>` now.
    //
    // ⚠ **What moved is the SURFACE, not the code, and that distinction is load-bearing.** Every
    // one of these opens a `DataFusionHist`, and `vike-cli` is DataFusion-FREE by construction
    // (its manifest argues every edge; CI's `light-consumers` lane asserts it). So `vike-cli data`
    // reaches them by SPAWNING this binary — `crates/vike-cli/src/cmd/data.rs`'s `engine_argv` and
    // `rm_engine_argv` build exactly the argv below. Deleting the implementation would delete
    // `vike-cli data fetch|seed-demo|rm` with it. A reader who "finishes" the ruling by removing
    // this arm breaks the verb the ruling moved the work TO.
    //
    // ⚠ It is a SUBCOMMAND rather than five renamed flags for two reasons. The grammar then matches
    // `vike-cli data`'s one-for-one, so the argv translation is a rename rather than a re-shape and
    // both surfaces read as the same words; and it stays reachable BY HAND on a box that has an
    // engine and no `vike-cli` (which is every Windows box — no release publishes a `vike-cli`-less
    // engine, but `crates/vike-cli/src/cmd/engine.rs`'s module doc carries the mirror asymmetry).
    //
    // Placed FIRST among the pre-profile exits so `data` is never read as the POSITIONAL profile
    // (ruling 14) — [`profile_from_args`] refuses a lone `data` positional by name for the same
    // reason, since routing here is what makes that spelling mean something else.
    if args.first().is_some_and(|a| a == DATA_SUBCOMMAND) {
        return run_data(vars, &args[1..], now_unix_secs);
    }
    // ⚠ **ARTIFACT-ONLY**: no store, no profile, no socket. §6 of the CLI-surface design requires
    // the reading verbs to work "on a laptop with neither, on runs minted months ago", so this
    // routes BEFORE `DataFusionHist::open` — the same placement, and for the same reason, as the
    // `data` route above: otherwise `trials` reads as the POSITIONAL profile ([`SUBCOMMANDS`]
    // refuses that spelling).
    //
    // ⚠ It lives on the ENGINE binary rather than on `vike-cli` and that is a DEPENDENCY fact, not
    // a preference: `vike-cli` has no normal dependency on this crate (dev-only). Every document it
    // reads is plain JSON, so the operator-facing `vike-cli backtest trials` can be built later
    // over `serde_json::Value` or over a typed edge — a choice this binary does not make for it.
    if args.first().is_some_and(|a| a == TRIALS_SUBCOMMAND) {
        return run_trials(vars, &args[1..]);
    }
    // …and the five old spellings, refused by name. `crates/vike-backtest/src/backtest_cli.rs`'s
    // `parse_search_flags` retired `--search` the same way and carries the argument: a second
    // spelling that can be given a DIFFERENT value is a resolution nobody can make safely, and an
    // alias would keep the old surface forever. ARGV TRIAGE — nothing is opened, per the rule
    // `a_refused_flag_never_opens_the_store` pins.
    if let Some(code) = refuse_a_retired_data_flag(args) {
        return code;
    }

    // The ONE `std::env::vars()` sweep this binary performs, per the settings-registry rule that a
    // binary reads the environment and everything below it takes the map as a parameter. Two
    // consumers: the user-indicator directory just below, and `store_root` further down.

    // ⚠ **User indicators, installed BEFORE any strategy is compiled.** A profile naming the
    // `rhai` strategy (`crates/vike-backtest/src/harness/registry.rs`'s `strategy_by_name` arm —
    // the create->backtest keystone) compiles the user's OWN script here, and that script may call
    // their OWN indicators. Nothing else installs them: `run_backtest` and every sweep/walkforward
    // sibling reach `RhaiStrategy::compile` with no indicator argument anywhere in the signature,
    // by design (`vike_script::install_user_indicators`' doc argues why the set is process-wide),
    // so a binary that skips this call hands the author a script that COMPILES and then raises
    // function-not-found on every bar until the consecutive-error cap switches the strategy off —
    // a backtest that runs to completion and reports zero trades.
    //
    // Placed after the three informational exits (`--help`/`--version`/`--list`), which answer out
    // of the BUILD and must not read a directory, and before the profile load, which is the first
    // step that can compile a script.
    //
    // Diagnostics go to **stderr**: `--json` writes a machine report on stdout, and one rejected
    // indicator file must not make that unparseable. They are never fatal, for the reason
    // `load_and_install_user_indicators` gives — a half-edited file the profile never calls must
    // not fail the run.
    if let Some(user_data) = std::env::current_dir().ok().and_then(|cwd| {
        // `VIKE_USER_DATA_DIR` names the directory outright and wins over the project walk. Read
        // here — and spelled as a LITERAL — because the settings registry's map-lookup sweep
        // resolves constants CRATE-wide, so importing `vike_model::state_path::USER_DATA_DIR_ENV`
        // would make this read invisible to `crates/vike-ops/tests/settings_registry.rs`. That is
        // the same trade `vike-cli`'s dispatcher makes for the same variable.
        vike_model::state_path::project_user_data_dir_from(
            vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
            &cwd,
        )
    }) {
        for line in vike_script::load_and_install_user_indicators(&user_data) {
            eprintln!("backtest: indicator not loaded — {line}");
        }
    }

    // ⚠ THE DAEMON ARM (ruling 7 of
    // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`). It sits HERE — after
    // the informational exits and after the indicator install, before the profile requirement —
    // because `--addr` is the one invocation that needs no profile and DOES need the user's
    // indicators: this daemon compiles client-supplied Rhai, so the set must be installed before a
    // connection can be accepted, and `install` is once-per-process.
    let addr_flag = match parse_addr_flag(args) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("backtest: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if !matches!(addr_flag, AddrFlag::Absent) {
        return run_serve(vars, args, addr_flag, studio, study);
    }

    // ⚠ **ARGV TRIAGE BEFORE ANY I/O**, and it is a design property rather than tidiness: refusing
    // a command line must not load a profile and must not open (which means CREATE — every
    // `DataFusionHist::open` `create_dir_all`s its root) a store. It is the rule `--help` and
    // `--version` already obey at the top of this function, extended to the flags that name WORK.
    // `crates/vike-backtest/tests/optimizer_cli.rs`'s `a_refused_flag_never_opens_the_store` is what
    // makes it assertable: it names a `--store` path that must not exist afterwards.
    //
    // Placed after every arm that returns without a profile (`--list`, `--seed-demo`, `--rm-series`,
    // `--fetch*`, `--export`, `--addr`), so none of them is newly constrained by it.
    let search = match parse_search_flags(args) {
        Ok(s) => s,
        Err(e) => {
            // ⚠ NO `{USAGE}` here, and that is deliberate. A refusal that names ONE flag answers
            // with one line naming that flag and the paste-ready fix; the help text is what you get
            // when you supplied nothing to talk about (the two arms below and `--addr`). Dumping
            // `USAGE` after a flag error also makes the flag's name unfindable in the noise — and
            // it made five of `optimizer_cli.rs`'s tests pass against the UNFIXED binary, because
            // every flag name they grep for appears somewhere in that const.
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };

    let profile_path = match profile_from_args(args) {
        Ok(Some(p)) => p,
        // The "you gave me nothing" arm KEEPS its usage dump — there is no specific mistake to
        // name, and `crates/vike-backtest/tests/help_cli.rs`'s
        // `a_missing_profile_still_exits_non_zero_on_stderr` pins that this still names `--profile`.
        Ok(None) => {
            eprintln!(
                "backtest: a profile is required — `backtest <profile.toml>` (or --profile PATH, \
                 or --list to show strategies)\n\n{USAGE}"
            );
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };

    // ⚠ The TEXT comes back with the profile and is threaded to the run record — never re-read at
    // persist time. An operator editing a profile while a long backtest runs is ordinary, and a
    // record holding bytes the run did not use is worse than no record: it looks authoritative.
    let (profile, profile_toml) =
        match BacktestProfile::from_path_with_text(&PathBuf::from(&profile_path)) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("backtest: failed to load profile {profile_path:?}: {e}");
                return ExitCode::from(2);
            }
        };

    // ⚠ A SEARCH WAS ASKED FOR AND THERE IS NOTHING TO SEARCH. Every one of these flags used to be
    // read INSIDE `if profile.is_paramscan()` below, so `backtest run.toml --optimizer tpe --trials 500`
    // ran ONE ordinary backtest and exited 0 — 500 trials of Bayesian search requested, one run
    // delivered, no diagnostic anywhere. §7.3 of the design signs off the refusal; ruling 13 is what
    // makes it a check on THIS verb rather than on the `optimize` verb it was written for.
    //
    // `--rank-by` deliberately keeps its DOCUMENTED ignore (this module's doc: a VALID value is
    // "ignored — not an error — on a non-sweep profile"), and the distinction is real rather than a
    // rationalisation: `--rank-by` names how to ORDER results and is meaningless-but-harmless with
    // nothing to order, while `--optimizer` and its per-method knobs name WHAT WORK TO DO.
    //
    // The message names `--optimizer` outright, which it can: a method-owned knob given WITHOUT
    // that flag is already refused above (no knob is owned by the default method), so
    // `search.requested` here proves `--optimizer` was written.
    //
    // ⚠ Since stage 5 the message names WHICH flag asked, because `--keep-trials` and `--resume`
    // also set `requested` and a line that always blamed `--optimizer` would name a flag the
    // operator never wrote. Both new flags describe a search's ARTIFACT, and a profile with no
    // search space has no artifact to keep either.
    //
    // ⚠ **The list this renders is DERIVED, and it was a six-element LITERAL until the observer
    // doors landed.** It is `--optimizer`, then [`METHOD_KNOBS`], then [`SEARCH_PROPERTY_FLAGS`] —
    // the same three sources [`parse_search_flags`] sets `requested` from, in the same order, so
    // the sentence renders exactly the bytes it did before and cannot go short. A literal here was
    // a second copy of "what asks for a search" living in a different function from the code that
    // decides it: `requested` would be true, this filter would match nothing, and the refusal
    // would name no flag at all.
    if search.requested && !profile.is_paramscan() {
        let asked = std::iter::once("--optimizer")
            .chain(METHOD_KNOBS.iter().map(|(flag, _)| *flag))
            .chain(SEARCH_PROPERTY_FLAGS.iter().copied())
            .filter(|f| flag_given(args, f))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!(
            "backtest: {asked} names a parameter SEARCH, but {profile_path:?} has no [paramscan] \
             table — there is nothing to search. Add one, or drop the search flags"
        );
        return ExitCode::from(2);
    }

    // ⚠ `store_root_resolved`, not `store_root`: the RUNG comes with the path here, because a
    // `data.explain` plan leads with both. `store_root` is the same call with the rung dropped
    // (`binutil`'s own doc says so), so the two cannot resolve differently — and "which store
    // answered" is the first question a plan has to settle, exactly as it is for `data rm`.
    // ⚠ The settings load sits HERE and not beside the `--addr` dispatch above, because the ARGV
    // TRIAGE between them refuses a command line WITHOUT PERFORMING ANY I/O, and a settings load
    // is I/O. `load_backtest_settings` is the same function the daemon arm calls; the two arms are
    // mutually exclusive, so one walk happens per process and never two.
    let settings = match load_backtest_settings(vars, "backtest") {
        Ok(s) => s,
        Err(code) => return code,
    };
    let store_flag = arg(args, "--store").map(PathBuf::from);
    let resolved_root = crate::binutil::store_root_resolved(store_flag.clone(), vars);
    let root = resolved_root.root.clone();

    // ⚠ **`--archive` reads downloaded `data.vike.io` Parquet IN PLACE, and it is a DIFFERENT
    // BACKEND rather than a different root.** Until 2026-09-20 this engine could not open one at
    // all: the reader lived in `vike-backfill`, a COLLECTOR, which ranks above this crate — so the
    // only way to point the harness at an archive day was a separate binary in that crate, whose
    // own doc called itself "the leaf binary that can depend on it directly". That is a workaround
    // around an inverted arrow, not a feature, and `vike_data::backtest_store` is where the choice
    // lives now. The binary is deleted with this change.
    //
    // Why it is worth a flag rather than "import it first": reading in place skips the import
    // entirely, and the import is not cheap. MEASURED on the CI box (2026-09-20, one flat-layout day):
    // **16.2 s for ONE row group of 999,420 rows**, and a day carries 300 of them — the cost is
    // driven by how many distinct `token_id`s a row group holds (~450 in the flat layout), because
    // each group is split across that many destination partitions.
    //
    // Giving BOTH is refused rather than resolved: they name two different stores, and a silent
    // precedence rule is how somebody backtests the wrong data and never learns.
    let archive = arg(args, "--archive").map(PathBuf::from);
    if archive.is_some() && store_flag.is_some() {
        eprintln!(
            "backtest: --store and --archive name two different stores; give one. --store DIR \
             reads an imported DataFusionHist root, --archive PATH reads downloaded archive \
             Parquet in place (a directory of day files, or one file)"
        );
        return ExitCode::FAILURE;
    }
    // ⚠ **The default is the WIRE** (`docs/decisions/0084-only-the-datahub-touches-the-store.md`):
    // the hist store has ONE reader, the datahub, and everything else asks it over the wire. Only
    // `--store` ON THE LINE opts out — `$VIKE_HIST_STORE` still answers WHICH root a local read
    // opens and deliberately does NOT select the route, because the deployed unit sets that
    // variable and leaving it in charge would have kept every run local, proving nothing where it
    // matters. `--archive` is a third answer and wins outright: it names a different BACKEND, not
    // a different root, so no route is consulted for it at all.
    //
    // ⚠ The ADDRESS comes from the settings — which means the DATABASE on a migrated box, through
    // the same `StoreLayer` the daemon arm reads. `load_backtest_settings` carries the owner
    // ruling behind that; the short version is that a box has ONE answer to "where is my
    // datahub", and an env-only ladder here would have given this arm a different one from the
    // `--addr` arm two hundred lines up.
    let route = history_route(store_flag.as_deref(), settings.config.datahub_addr.as_deref());
    // NONE when the run reads an archive: the archive backend keeps no per-series manifest, so it
    // has no facts to fingerprint a run with, and the consumers below take the absence as the fact
    // it is. ⚠ That used to be a statement about the TYPE — "there is no `DataFusionHist` behind
    // it" — and the type stopped being the reason: `list_series` and `series_facts` are both on
    // the `HistStore` trait now, so `Some` no longer means "concrete" and this binding is no
    // longer the concrete handle. What it means is "a store that can report its own provenance".
    let concrete: Option<Box<dyn HistStore + Send + Sync>> = match &archive {
        Some(_) => None,
        None => match open_routed_history(&route, &root, vars) {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("backtest: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    // ONE handle, UNSIZED ONCE. `run_backtest` and every evaluator constructor take
    // `Arc<dyn HistStore + Send + Sync>`; the ladder this replaced wrote `Arc::new(store)` at FOUR
    // call sites, one per arm, and coerced at each.
    //
    // ⚠ `Arc::from`, NOT `Arc::new`: the routed opener hands back a `Box<dyn HistStore + …>`, and
    // `Arc::new` over one would build an `Arc<Box<dyn …>>` — a second indirection that still
    // type-checks at every use below, because `Box` derefs to the trait object the methods live
    // on. `Arc::from` consumes the box and keeps ONE pointer.
    //
    // ⚠ The handle is KEPT under its own name rather than shadowed away, so the FINGERPRINT
    // capture can happen below, on the single-run path where the record is written, instead of
    // here above the sweep branch where its whole result is discarded. That reason used to be a
    // TYPE reason — `list_series` and `series_facts` were inherent to `DataFusionHist`, so only
    // the concrete handle could be asked — and it is not one any more: both are on the trait since
    // 0084's seventh verb landed. What survives is the PLACEMENT, which is what the name was
    // really buying. An `Arc` clone costs one refcount and nothing else.
    let concrete: Option<Arc<dyn HistStore + Send + Sync>> = concrete.map(Arc::from);
    let store: Arc<dyn HistStore + Send + Sync> = match (&concrete, archive.clone()) {
        (Some(c), _) => c.clone(),
        // The archive selector REFUSES an empty selection rather than returning a store that
        // answers every scan "no rows" — `HistStore`'s `Ok(vec![])` is a claim of fact, and
        // `crates/vike-data/src/archive_store.rs`'s module doc carries what believing that wrongly
        // once cost (a scalper scoring -2.74% where the truth was -27.64%).
        (None, Some(path)) => {
            match vike_data::backtest_store::open_backtest_store(
                vike_data::backtest_store::BacktestStore::Archive(path),
            ) {
                Ok(s) => s,
                Err(why) => {
                    eprintln!("backtest: {why}");
                    return ExitCode::FAILURE;
                }
            }
        }
        // Unreachable by construction: `concrete` is `None` only in the `--archive` arm above.
        (None, None) => {
            eprintln!("backtest: no store was opened and no --archive was given");
            return ExitCode::FAILURE;
        }
    };

    // ⚠ **`data.explain`: PLAN AND STOP.** Placed here — after the store is open and BEFORE the
    // sweep branch, the clock read and the runs-root walk — because a plan must report what the
    // store actually holds, and because a planning run must mint no run directory, take no
    // `started_at` and evaluate no grid point. It is the local twin of
    // `crate::compute_server`'s `explain_instead_of_running`, and both call the SAME
    // `crate::data_plan::explain_document`: a plan printed on the box and a plan returned over the
    // wire are one document, so `--local` stays a rehearsal for `--addr` here too.
    //
    // ⚠ Exit 0. A plan is a SUCCESS — it answered the question that was asked — and a non-zero rung
    // would make `vike-cli backtest run --explain-data` read as a failed run to every wrapper.
    if profile.data.explain {
        // ⚠ **On the WIRE arm there is NO rung, and `None` is the honest answer rather than a
        // missing one.** The local ladder still resolves a root — the disclosure below needs
        // something to fall back to — but nothing opened it, so reporting "$VIKE_HIST_STORE
        // answered" would credit a rung that decided nothing on this run. The plan leads with the
        // ROUTE instead, which is the question "which store answered" actually has an answer to.
        let rung = match &route {
            HistoryRoute::Local => Some((resolved_root.rung.as_str(), resolved_root.rung.why())),
            HistoryRoute::Wire(_) => None,
        };
        let plan = match crate::data_plan::plan_data(
            &profile,
            store.as_ref(),
            &route.label(&root),
            rung,
        ) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("backtest: {e}");
                return ExitCode::from(2);
            }
        };
        if has_flag(args, "--json") {
            match crate::data_plan::explain_document(&profile, &plan) {
                Ok(doc) => match serde_json::to_string_pretty(&doc) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        eprintln!("backtest: cannot render the data plan: {e}");
                        return ExitCode::from(2);
                    }
                },
                Err(e) => {
                    eprintln!("backtest: {e}");
                    return ExitCode::from(2);
                }
            }
        } else {
            match crate::data_plan::explain_lines(&profile, &plan) {
                Ok(lines) => {
                    for line in lines {
                        println!("{line}");
                    }
                }
                Err(e) => {
                    eprintln!("backtest: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        return ExitCode::SUCCESS;
    }

    // The run's own clock read, taken BEFORE the work: it is both the manifest's `started_at` and
    // the seconds half of the run id, so the directory name and the document inside it cannot
    // disagree about when this run began. Read HERE rather than inside `vike_model::runs`
    // because a crate carrying `backtest == paper == live` contains no ambient clock read at all —
    // `crates/vike-ops/tests/clock_pin.rs`'s `CLOCK_PIN` is the gate, and time is an INPUT.
    //
    // ⚠ Read ABOVE the sweep branch since stage 5: a SEARCH mints its parent run before it
    // evaluates anything (the ledger needs a directory), so both paths need the value here. For a
    // single-run profile the branch below is not entered, so this executes at exactly the point it
    // did before and the value cannot differ.
    let started_at = now_unix_secs();

    // ⚠ The runs root is resolved HERE, in the composition root, from the SAME environment map and
    // the SAME working directory the indicator load above used — so a run cannot land in one
    // project while the strategy that produced it was read from another, and `VIKE_USER_DATA_DIR`
    // moves BOTH. Hoisted above the sweep branch by stage 5 for the same reason `started_at` was:
    // a search needs it before it evaluates anything, and ONE resolution must decide for both
    // paths (`persist_run`'s doc carries why this is a parameter rather than a walk inside the
    // persisting function).
    let runs_root = std::env::current_dir().ok().and_then(|cwd| {
        // Spelled as a LITERAL, matching the indicator load above and for the reason it gives:
        // `crates/vike-ops/tests/settings_registry.rs`'s map-lookup sweep resolves constants
        // CRATE-wide, so importing `vike_model::state_path::USER_DATA_DIR_ENV` would make this read
        // invisible to that gate.
        vike_model::state_path::user_runs_dir_from(
            vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
            &cwd,
        )
    });

    if profile.is_paramscan() {
        // The `(objective, label)` pair, built ONCE. The ladder spelled this block VERBATIM TWICE
        // (the tpe arm and the euler arm) and a THIRD way inline in the grid arm.
        //
        // ⚠ Declared BEFORE the evaluator: `StoreEvaluator::new` borrows the objective for the
        // evaluator's whole life, and locals drop in reverse declaration order.
        // ⚠ `search_select::objective_for`, shared with `crate::compute_server`: a remote run must
        // build the same objective under the same label, or two routes rank one grid differently.
        let (objective, label) = search_select::objective_for(search.rank);
        // ⚠ Taken HERE, before the evaluator: `StoreEvaluator::new` MOVES `label`, so a search's
        // identity cannot read it afterwards. One clone of a short string, so that the label the
        // search is RANKED by and the label its artifact RECORDS are the same value by
        // construction rather than by two matches agreeing.
        let rank_label = label.clone();

        // ⚠ Resolved HERE, once, at the composition root — not inside three library adapters. The
        // concrete adapters (`run_paramscan`/`run_paramscan_with`/`run_paramscan_euler`) still call
        // `ParamscanExec::from_env` themselves for the datahub and their own tests, so the
        // `("vike-backtest", "VIKE_SWEEP_SEQUENTIAL")` row on
        // `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` does not move. Calling the
        // existing `from_env()` from here adds no `env::var` literal for the scanner to find, so
        // that gate sees nothing new either — and it must stay `from_env()`: a `from_vars(&map)`
        // twin would need a SECOND registry row for the same `(name, krate)` pair with a different
        // layer.
        //
        // ⚠ TPE used to be handed a hard-coded `ParamscanExec::Sequential` and now gets whatever this
        // resolves. The output is byte-identical BY THE POOL RULE, not by the exec value:
        // `StoreEvaluator::evaluate` enters rayon only when `exec == Parallel && batch.len() > 1`,
        // and `TpeSearch::search` submits width-1 batches because each proposal reads the whole
        // observation history. Stated rather than assumed, because if that clause ever loosens TPE
        // silently gains a pool per single backtest.
        let exec = harness::ParamscanExec::from_env();

        // ⚠ **ONE evaluator, and WHICH CONSTRUCTOR is a PRESERVATION decision rather than a
        // preference.** The two-input match MOVED to `harness::search_select::evaluator_for` with
        // stage 7 — `crate::compute_server` must make the identical choice for a REMOTE run, and
        // `search_select::uses_classic_evaluator` carries the whole argument (and is assertable
        // without a store, which is why the rule is now a named predicate rather than a comment).
        let eval = match search_select::evaluator_for(
            search.method,
            search.rank,
            &profile,
            store,
            &objective,
            label,
            exec,
        ) {
            Ok(e) => e,
            // The SAME prefix the adapters produced, so nothing an operator greps changes: today
            // the params-not-a-table refusal already comes out of this constructor inside
            // `run_paramscan_exec` and reaches stderr as `backtest: sweep run failed: …`.
            Err(e) => {
                eprintln!("backtest: sweep run failed: {e}");
                return ExitCode::FAILURE;
            }
        };
        // ⚠ **The ONE place the trial MATRIX is armed, and it is off for every mode but
        // `returns`.** A consuming builder, so a search that did not ask retains not one float and
        // is byte-identical to a run from before the mode existed —
        // `harness::sweep::ReturnBuckets` carries the retention arithmetic, and
        // `KeepTrials::return_buckets` is the one mapping from the flag to it. Deliberately NOT a
        // parameter on `search_select::evaluator_for`: that function is shared with
        // `crate::compute_server`, which has no `--keep-trials` and would have to pass a value
        // meaning "unchanged" at a call site that knows nothing about ledgers.
        let eval = eval.with_return_buckets(search.keep.return_buckets());
        // ⚠ **THE OBSERVER DOOR, and it is called UNCONDITIONALLY.**
        // `search_select::arm_observers` is the ONE place the floor and the progress stream are
        // armed on a store-driven evaluator, so this binary and `crate::compute_server` cannot arm
        // them differently and a third surface arms them by calling this rather than by remembering
        // two builders and a budget lookup.
        //
        // ⚠ Unconditional rather than behind an `if`, because that function's own doc states both
        // halves are NO-OPS at their defaults: a `TradeFloor::DISARMED` plus a `ProgressMode::Auto`
        // on a non-terminal stderr hands back the evaluator with neither field set. So a search
        // that wrote neither flag is byte-identical to one from before they had doors, and
        // `crates/vike-backtest/tests/compute_profile_roundtrip.rs`'s
        // `profile_sweep_is_byte_identical_local_and_remote` is unmoved — which is exactly what a
        // condition spelled two ways on two surfaces could not promise.
        //
        // ⚠ `search.method` is passed only so the progress line can ask that METHOD for a total
        // (`budget_hint_for`); the floor is method-agnostic. `SearchMethod` is `Copy`, so this does
        // not consume what `optimizer_for` reads below.
        let eval = search_select::arm_observers(
            eval,
            search.method,
            &profile,
            search.floor,
            search.progress,
        );

        // ⚠ BEFORE `optimizer_for`, which takes `search.method` BY VALUE.
        let (method_name, seed, budget) = search_identity_parts(&search.method);
        // THE ONE STORE READ A SEARCH PERFORMS, and it now answers TWO questions.
        //
        // ⚠ **This used to be a witness-only walk, and the second answer is FREE.** A search has
        // always paid one `list_series` plus one manifest parse per series for its resume witness;
        // [`collect_data_fingerprint`] costs exactly that since `series_facts`
        // merged the coverage and commits reads into one parse. So the RECORD is collected here and
        // BOTH the witness ([`data_witness`], a pure rendering of it) and the run's INPUT ADDRESS
        // fall out of the same walk. Nothing was hoisted: the single-run call site is still below
        // this branch, for the reason its own doc gives.
        //
        // ⚠ A failure is the SENTINEL for the witness and an ABSENCE for the address, and the two
        // postures are different on purpose. A store that cannot be inventoried must never compare
        // EQUAL to anything, including to itself, so a resume against it is refused rather than
        // silently reusing scores over data nobody can witness — that is what the sentinel buys. An
        // ADDRESS is the opposite: an un-inventoried slice must be ABSENT rather than DIFFERENT
        // (`crate::run_fingerprint`'s module doc and `crates/vike-cli/src/cmd/runs/gate.rs`'s both
        // rest on it), so the manifest records `fingerprint: null` and the id keeps its pid form.
        // A store fault therefore costs a search its baseline comparison and never fabricates one.
        //
        // The line names it once; it does not fail the search, which persisting never does.
        let (store_data, search_data, input_addr) = match collect_data_fingerprint(
            concrete.as_deref().map(|h| h as &dyn HistStore),
            &profile,
            &route.label(&root),
        ) {
            Ok(fp) => {
                let addr = run_fingerprint::input_fingerprint(&profile_toml, &fp);
                (data_witness(&fp), Some(fp), Some(addr))
            }
            Err(why) => {
                eprintln!(
                    "backtest: the search's data could not be witnessed — {why}. It is \
                         recorded as `{}`, so this search can be resumed by nothing, and the run \
                         is NOT addressed",
                    trial_ledger::DATA_UNREADABLE
                );
                (trial_ledger::DATA_UNREADABLE.to_string(), None, None)
            }
        };
        let identity = trial_ledger::SearchIdentity {
            // Re-read rather than retained: `BacktestProfile::from_path_with_text` keeps the TEXT
            // for the single-run record, but a search must address the FILE as it is now, and a
            // file that will not re-read cannot be proved unchanged — which is what
            // `trial_ledger::PROFILE_UNREADABLE` means and why `SearchIdentity::differences`
            // refuses it on both sides.
            profile_fnv1a64: std::fs::read(&profile_path)
                .map(|b| trial_ledger::fnv1a64_hex(&b))
                .unwrap_or_else(|_| trial_ledger::PROFILE_UNREADABLE.to_string()),
            profile_name: profile.name.clone(),
            // ⚠ CANONICALIZED, unlike the manifest's `config.path`. `--store store` names two
            // different directories from two different projects, and with a shared runs root
            // (`VIKE_USER_DATA_DIR`) the as-spelled form made those two searches compare EQUAL. The
            // store exists by now — `DataFusionHist::open` above `create_dir_all`s it — so the
            // fallback is for a path that stopped being readable between the two lines.
            store: std::fs::canonicalize(&root)
                .unwrap_or_else(|_| root.clone())
                .display()
                .to_string(),
            store_data,
            build: build.and_then(|b| b.git_sha).map(str::to_string),
            method: method_name.to_string(),
            // `rank_label` is taken above, before `StoreEvaluator::new` moves the original.
            rank_by: rank_label,
            seed,
            budget,
        };

        // ONE construction match. PR 3's genetic method costs exactly one arm here and one in
        // `parse_search_flags`.
        let method = search_select::optimizer_for(search.method);

        // The parent run, opened BEFORE the search — the ledger is written as trials complete, so
        // it needs a directory to be written into.
        let parent = match runs_root
            .as_deref()
            .ok_or_else(|| "no project directory above the working directory".to_string())
            .and_then(|root| {
                open_search_run(
                    identity.clone(),
                    search.keep,
                    search.resume.as_deref(),
                    root,
                    started_at,
                    input_addr.as_deref(),
                )
            }) {
            Ok(run) => Some(run),
            // ⚠ TWO postures, and the difference is what was ASKED FOR. A MINT that fails is a run
            // that cannot be saved — persisting is additive, so it is named and the search proceeds.
            // A RESUME that fails is a request that cannot be honoured: continuing as a fresh
            // search would silently redo the work the operator was trying to avoid, and would look
            // like success.
            Err(why) if search.resume.is_some() => {
                eprintln!("backtest: {why}");
                return ExitCode::from(2);
            }
            Err(why) => {
                eprintln!("backtest: search NOT saved — {why}");
                None
            }
        };

        // ⚠ `None` under `--keep-trials none` OR when the parent could not be opened — in both
        // cases the recorder evaluates exactly as the bare evaluator would and writes nothing.
        //
        // ⚠ **Keyed on [`KeepTrials::keeps_ledger`], never on `== Scalars`.** This matched the
        // `Scalars` variant by NAME until `returns` existed, and the `_ => None` arm would then
        // have silently made a `--keep-trials returns` search write no ledger at all — the one
        // failure that mode must not have, since it is `scalars` PLUS a matrix rather than instead
        // of one. That is also what `reopen_search_run`'s twin check is keyed on, so the writer and
        // the resumer agree by construction.
        let ledger = match parent.as_ref() {
            Some(p) if search.keep.keeps_ledger() => Some(p.path.clone()),
            _ => None,
        };

        // The warm cache. Read from the parent's own ledger — the SAME directory the recorder is
        // about to append to — because a resume continues the run rather than minting a second.
        //
        // ⚠ `latest_by_n` first: a previous resume may have re-evaluated a line its cache missed,
        // and the LATER record is the one that process actually computed.
        let warm = match (search.resume.as_deref(), parent.as_ref()) {
            (Some(_), Some(p)) => match trial_ledger::read_trials(&p.path) {
                Ok(read) => {
                    if !read.unreadable.is_empty() {
                        // Named, never fatal: a torn line costs its own trial a re-evaluation and
                        // nothing else, which is exactly what JSON Lines buys.
                        eprintln!(
                            "backtest: {} ledger line(s) in {} did not parse and will be \
                             re-evaluated",
                            read.unreadable.len(),
                            p.run_id
                        );
                    }
                    let (trials, _superseded) = trial_ledger::latest_by_n(read.trials);
                    harness::warm_from(&trials)
                }
                Err(e) => {
                    eprintln!("backtest: the ledger could not be read — {e}; nothing is reused");
                    std::collections::HashMap::new()
                }
            },
            _ => std::collections::HashMap::new(),
        };
        let resuming = search.resume.is_some();
        let recorder = harness::TrialRecorder::new(&eval, ledger, warm);

        // ONE call, through the ONE door: `optimize` runs `require_overridable_params` and
        // `accepts` before any loop starts, which is what makes `PointEvaluator::evaluate`
        // infallible by type. Nothing may call `Optimizer::search` directly.
        //
        // ⚠ The recorder DECORATES the evaluator rather than replacing it: with an empty warm cache
        // it hands the inner evaluator the original batch verbatim and returns its answer
        // untouched, which is what keeps a recorded search byte-identical to an unrecorded one.
        let harness::Optimized { report: sweep_report, summary } =
            match harness::optimize(method.as_ref(), &profile, &recorder) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!("backtest: sweep run failed: {e}");
                    return ExitCode::FAILURE;
                }
            };
        // Read after the work and before the terminal, so it measures the SEARCH.
        let finished_at = now_unix_secs();
        let tally = recorder.tally();
        if let Some(why) = recorder.first_write_error() {
            // ONE line however many appends failed: a directory that stopped accepting writes fails
            // every one of them, and an operator needs the reason, not the count of repetitions.
            eprintln!(
                "backtest: {} trial(s) were NOT written to the ledger — {why}",
                tally.write_failures
            );
        }

        // **The anti-overfitting statistics, computed ONCE over the RANKED rows.**
        //
        // ⚠ Here rather than inside `build_trials_document`, because the inputs are different: this
        // reads the ranked `ParamscanReport` and the evaluator's retained matrix, both of which are
        // in-memory answers of THIS process, while that function resolves the LEDGER off disk.
        // Folding them together would mean handing a document builder an evaluator.
        //
        // ⚠ Column order is `sweep_report.rows`' order, which is the RANKING — so row 0 is the
        // winner whose Sharpe is deflated, and the statistic cannot depend on the order rayon
        // workers happened to finish in. `harness::optimize::overfit_stats` states that contract.
        //
        // ⚠ `None` under every mode but `--keep-trials returns`, and also whenever the matrix was
        // not measurable (every trial failed, or the range was too short to give `T >= splits`).
        // An absent block is the honest answer: a zeroed one would read as "assessed, and clean".
        //
        // ⚠ Called UNCONDITIONALLY rather than under `if search.keep == Returns`, deliberately:
        // `overfit_stats` keys on what was actually CAPTURED, so the statistic and the retention
        // cannot come to disagree about whether a matrix exists. Under every other mode the
        // evaluator captured nothing, that function's first guard returns `None`, and the whole
        // cost is one lock and one clone of an empty map.
        let overfit = harness::optimize::overfit_stats(
            &sweep_report.rows,
            &eval.captured_returns(),
            harness::optimize::DEFAULT_CSCV_SPLITS,
        );
        if search.keep == KeepTrials::Returns && overfit.is_none() {
            // Named rather than silent: an operator who typed `--keep-trials returns` is owed the
            // reason their report.json carries no `overfit` block, and every cause is a property of
            // the search rather than a fault.
            eprintln!(
                "backtest: no overfit statistics — no trial retained a usable return vector, or \
                 the range gives fewer than {} buckets to split. A resumed search's REUSED trials \
                 carry a score rather than a curve and contribute none",
                harness::optimize::DEFAULT_CSCV_SPLITS
            );
        }

        // ⚠ The ANSWER is resolved before the WRITE and independently of it. On a resume this
        // document IS the output, and while the two were one call a full disk handed the operator
        // empty stdout with exit 0 — see [`build_trials_document`]. The write itself goes back
        // BELOW the print, which is [`persist_run`]'s posture and now this path's too.
        let document = match parent.as_ref() {
            Some(run) => match build_trials_document(run, &identity, search.keep, tally, overfit) {
                Ok(doc) => Some(doc),
                Err(why) => {
                    eprintln!("backtest: the trial ledger could not be resolved — {why}");
                    None
                }
            },
            None => None,
        };

        // The METHOD's own cost line, on stderr so a `--json` stdout stays a clean document, and
        // BEFORE the report — where both hand-written copies printed theirs. euler's is
        // `EulerBudget`'s `Display` and tpe's is the line `TpeSearch::search` now assembles from its
        // own config and its own ranked rows; the grid says nothing, exactly as before.
        if let Some(line) = summary {
            eprintln!("{line}");
        }

        // ONE serialize-or-print tail. The tpe arm copied this block WHOLESALE before its early
        // `return`; that copy and that `return` are gone.
        //
        // ⚠ WHAT A RESUME PRINTS IS DIFFERENT, and the reason is what a reused row HOLDS. A cache
        // hit answers with `report: None` (`harness::trials::WarmTrial` carries a SCORE, not a
        // report — see that type for why rebuilding one from the ledger would print a number the
        // original run did not compute), and `harness::sweep::ParamscanReport`'s `Display` renders any
        // row with `report: None` as `FAILED: {error}`. So a resumed run's sweep report would print
        // every reused trial as a failure. The ledger is the complete answer and this is the same
        // renderer `backtest trials` uses — one implementation, two entry points.
        //
        // A FRESH search's stdout is UNTOUCHED: `crates/vike-cli/src/cmd/backtest.rs`'s `--local`
        // arm prints it verbatim and `scripts/cli_mcp_smoke.sh` asserts on `.rows`.
        if resuming {
            // ⚠ A resume whose ANSWER could not be produced is a FAILURE, never a silent success.
            // Reachable only when the ledger this process just appended to became unreadable — a
            // genuine fault, already named above. Exiting 0 with empty stdout here is what the
            // `--local` arm would forward to a `--json` consumer as a zero-byte document.
            let Some(document) = document.as_ref() else {
                eprintln!(
                    "backtest: --resume produced no answer — the resumed run's ledger could not be \
                     resolved, so there is nothing to print. The search itself ran; \
                     `backtest trials {}` reads whatever reached disk",
                    parent.as_ref().map(|p| p.run_id.as_str()).unwrap_or("<id>")
                );
                return ExitCode::FAILURE;
            };
            if has_flag(args, "--json") {
                match serde_json::to_string_pretty(document) {
                    Ok(json) => println!("{json}"),
                    Err(e) => {
                        eprintln!("backtest: failed to serialize the trials document: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                let rows = sort_trials(document, TrialSort::Score, None);
                print!("{}", render_trials(document, &rows));
            }
        } else if has_flag(args, "--json") {
            match serde_json::to_string_pretty(&sweep_report) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("backtest: failed to serialize sweep report: {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            print!("{sweep_report}");
        }

        // ⚠ **AFTER printing, and never fatal** — [`persist_run`]'s posture, which this path now
        // shares: the search has already handed the operator its numbers by the time this runs.
        let saved = match (parent.as_ref(), document.as_ref()) {
            (Some(run), Some(doc)) => match write_search_run(
                run,
                &profile,
                &profile_path,
                &route.label(&root),
                &identity,
                search.keep,
                build,
                started_at,
                finished_at,
                doc,
                search_data.as_ref(),
                input_addr.as_deref(),
            ) {
                Ok(dir) => Some(dir),
                Err(why) => {
                    eprintln!("backtest: search NOT saved — {why}");
                    None
                }
            },
            _ => None,
        };

        if let Some(dir) = saved.as_ref() {
            // ⚠ The reuse half of this line is what makes `--resume` FALSIFIABLE from the outside:
            // without it every assertion about a resumed ledger would also pass on a binary that
            // ignored the flag. `tally.evaluated` counts every candidate the searcher asked about,
            // reused ones INCLUDED, so the freshly-computed count is the difference.
            let resumed = if resuming {
                format!(
                    ", resumed {} reused / {} evaluated",
                    tally.reused,
                    tally.evaluated - tally.reused
                )
            } else {
                String::new()
            };
            eprintln!("backtest: search saved to {}{resumed}", dir.display());
        }

        return ExitCode::SUCCESS;
    }

    // ⚠ **BELOW the sweep branch, deliberately, and it used to sit above it.** Above it, every byte
    // of a multi-megabyte grouped manifest was parsed and then DISCARDED, before a search that was
    // about to run hundreds of backtests.
    //
    // ⚠ **The sweep branch calls this collector too now, and this line is still not a hoist.** The
    // old argument was arithmetic: the capture asked the CONCRETE store two questions per series and
    // each one parsed that series' manifest INDEPENDENTLY, so a search could not afford it. That
    // double read was the STORE's rather than this caller's, and it is closed —
    // `DataFusionHist::series_facts` answers coverage and commits from one parse, which is exactly
    // what a search was already spending on its data witness. So the branch above collects the
    // record at its own call site and addresses its run from it, this line stays where it is, and
    // the two paths keep their own diagnostics: `run NOT addressed` is the single run's wording and
    // a search has never printed it.
    //
    // ⚠ `None` rather than a wrong answer. The collector refuses rather than guessing when the
    // store cannot be read (see its doc), and a run whose inputs cannot be ADDRESSED still runs and
    // is still saved — it simply records `fingerprint: null` and keeps the pid form of its id.
    //
    // ⚠ Read BEFORE the run, so the record says what the store held when the run STARTED — which is
    // the thing the result depends on.
    let data_fingerprint = match collect_data_fingerprint(
        concrete.as_deref().map(|h| h as &dyn HistStore),
        &profile,
        &route.label(&root),
    ) {
        Ok(fp) => Some(fp),
        Err(why) => {
            eprintln!("backtest: run NOT addressed — {why}");
            None
        }
    };

    // `store` is ALREADY the trait object (one coercion, above the sweep branch), so the
    // single-run path hands it over as-is rather than re-wrapping a concrete handle.
    let result = match harness::run_backtest(&profile, store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("backtest: run failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Read here rather than after printing: `finished_at` must measure the RUN, not the terminal.
    let finished_at = now_unix_secs();

    // ⚠ **The stamp is attached HERE, not inside `from_result`.** That function lives in
    // vike-analytics, which cannot name the gated `BacktestProfile` the stamp is resolved from —
    // so the producer is the door. A report with no stamp reads `realism: None`, which
    // `vike_analytics::report::BacktestReport::realism` documents as "nobody stamped this run" and
    // NOT as "this run was free": the two must not be the same bytes, because the second is the
    // one somebody acts on.
    let report = harness::BacktestReport::from_result(
        profile.name.clone(),
        &result,
        harness::report::periods_per_year(&profile),
    )
    .with_realism(harness::report::realism_stamp(&profile));

    if has_flag(args, "--json") {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("backtest: failed to serialize report: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{report}");
    }

    // ⚠ **AFTER printing, and never fatal.** Persisting is additive: a run whose directory cannot
    // be created has still computed its numbers, and they have already reached stdout by the time
    // this line runs. The failure is NAMED on stderr rather than swallowed — an operator who
    // believes their runs are being saved and finds an empty `user_data/runs/` is worse off than
    // one who was told — but it does not change the exit code, because refusing to exit 0 over a
    // filesystem that would not take a metadata file would make saving a run a new way to lose one.
    //
    // Both lines go to stderr for the reason every other diagnostic in this binary does: `--json`
    // stdout is a machine contract and stays byte-identical.
    //
    // ⚠ `runs_root` was resolved ABOVE the sweep branch — one walk decides for both paths.
    match runs_root.as_deref() {
        Some(runs_root) => {
            // The address over what this run READ, computed once and used twice: it names the
            // manifest's `fingerprint` and it is the middle segment of the run id. `None` when the
            // data half could not be collected — an address over an unknown data slice would be a
            // number that looks authoritative and is not. Declared before `facts`, which borrows
            // it: locals drop in reverse declaration order.
            let input_addr = data_fingerprint
                .as_ref()
                .map(|d| run_fingerprint::input_fingerprint(&profile_toml, d));
            let facts = BacktestRunFacts {
                profile: &profile,
                profile_path: &profile_path,
                profile_toml: &profile_toml,
                store_root: &root,
                runs_root,
                data: data_fingerprint.as_ref(),
                fingerprint: input_addr.as_deref(),
                build,
                started_at,
                finished_at,
            };
            match persist_run(&facts, &result, &report) {
                Ok(dir) => eprintln!("backtest: run saved to {}", dir.display()),
                Err(why) => eprintln!("backtest: run NOT saved — {why}"),
            }
        }
        None => {
            eprintln!("backtest: run NOT saved — no project directory above the working directory")
        }
    }

    ExitCode::SUCCESS
}

// ⚠ `fingerprint_series_ids` MOVED out of this file, to
// `crates/vike-backtest/src/run_fingerprint.rs`'s `planned_series_ids`, and the move is not
// tidiness. It is the ONE answer to "which stored series does this profile resolve to", and a
// SECOND consumer arrived that this file cannot serve: `crate::data_plan` runs the same resolution
// for the coverage gate and the `data.explain` plan, and both must work on the REMOTE route — where
// the store is an `Arc<dyn HistStore>` behind `hist-replay` and this module does not compile at all
// (it needs `datafusion-store`). So the resolver lives at the lower feature level that both can
// reach. No `pub use` shim: every call site here names the canonical path.

/// Read what the store holds for every series this profile will load.
///
/// ⚠ **Called BEFORE the handle is coerced to `Arc<dyn HistStore>`, and it has to be.**
/// `list_series` and `series_facts` are INHERENT to `vike_data::DataFusionHist`
/// rather than `HistStore` methods, and [`run`] shadows the concrete handle with the trait object
/// one line later. The alternative was a required trait method, which would redden at least eight
/// `impl HistStore for` sites — `MemHistStore`, `RemoteHistStore` and the doubles in vike-report,
/// vike-datahub, vike-studio, vike-studio-core and this crate — to serve the one caller in the tree
/// that asks the question.
///
/// # What it costs, and who pays it
///
/// ONE `list_series` walk plus **ONE manifest parse per series** (`series_facts`), plus a
/// `fs::metadata` per part for the byte count. It used to be TWO parses per series — the separate
/// `series_coverage` and `series_commits` calls each folded the same file — and that number was the
/// whole argument for keeping this call BELOW the sweep branch, where a search would have paid it
/// before running hundreds of backtests over a grouped store whose manifest is multi-megabyte.
///
/// ⚠ **BOTH paths call it now, and the placement is unchanged.** The single-run call still sits
/// below the sweep branch; the SEARCH calls it at the point inside that branch where it was already
/// paying one parse per series for [`data_witness`]'s input. So the search's cost is the same
/// parse count it always had, and the run ADDRESS falls out of it — which is why this function was
/// not HOISTED to serve both. Hoisting would also have changed what a search prints: this
/// collector's failure reaches stderr as `run NOT addressed` on the single-run path, and the
/// search's call site keeps its own message naming the witness.
///
/// # ⚠ ABSENT and EMPTY are different answers, and the store alone cannot tell them apart
///
/// `series_facts` folds the series' manifest, and
/// `crates/vike-data/src/datafusion_hist/manifest.rs`'s `read_manifest` maps `ErrorKind::NotFound`
/// to an EMPTY manifest — so a series the store has never heard of comes back as
/// `Ok(SeriesCoverage::default())`, all zeros, indistinguishable from a series that exists and
/// holds nothing. That is not a new observation: `DataFusionHist::series_dir_of`'s own doc records
/// the five-bug class it caused, found by pointing the Data Manager at a real 148 MB tape whose
/// entire 37.5 M-row Polymarket group rendered as `0 rows · 0 B`.
///
/// So presence is decided by the store's INVENTORY instead — one `list_series` walk, membership
/// checked against it — and `coverage: None` means the store holds no such series, which is the
/// state `run_fingerprint::DataFingerprint::canonical` renders as `coverage missing` and the reason
/// that field is an `Option` at all. Before this, that arm was unreachable from the only producer
/// in the tree.
///
/// # ⚠ A store error makes the address ABSENT, never DIFFERENT
///
/// With `NotFound` handled above, the remaining ways `series_facts` can fail are a manifest that
/// will not PARSE and a `MANIFEST_FORMAT` mismatch. Swallowing either into `coverage: None` would
/// make the address BUILD-DEPENDENT — a format bump would flip every series to `coverage missing`
/// and move every stored address, orphaning every baseline — which is precisely the property
/// `canonical`'s module doc exists to guarantee. So an error propagates: the caller records NO
/// fingerprint and NO address, which says "this run's inputs could not be addressed" instead of
/// addressing them wrongly.
///
/// ⚠ **It still cannot FAIL A RUN.** A fingerprint is metadata about a run that has not happened
/// yet; [`run`] prints the reason on stderr and runs the backtest anyway, which is the same
/// discipline `vike_model::runs` states for persisting.
fn collect_data_fingerprint(
    store: Option<&dyn HistStore>,
    profile: &BacktestProfile,
    // ⚠ **What to RECORD as the provenance of this data — a path on a local run, the datahub's
    // ADDRESS on a routed one.** It was `store_root: &Path` until
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` routed this path, and a `&Path`
    // here is no longer a fact the caller has: on the wire arm the resolved root is THIS box's
    // default, a directory the run never opened, and recording it would put a confident falsehood
    // in the one document a later reader uses to reproduce the run. `HistoryRoute::label` renders
    // it, so the record and the run's own disclosure cannot disagree.
    store_label: &str,
) -> Result<run_fingerprint::DataFingerprint, String> {
    // ⚠ `Option`, and the reason CHANGED on 2026-09-23 while the shape did not. It used to be
    // `Option<&DataFusionHist>` because `series_facts` was INHERENT to that type and an archive run
    // had nothing to ask. That verb is on the `HistStore` trait now
    // (`docs/decisions/0084-only-the-datahub-touches-the-store.md`, the seventh verb), so the
    // parameter is a trait object and a ROUTED store can be fingerprinted — which is the whole
    // point of moving it.
    //
    // ⚠ The `Option` SURVIVES, and collapsing it would be a quiet regression. An archive store
    // inherits the trait's refusing default, so passing it would produce an `Err` too — but only
    // AFTER `list_series` below answered `Ok(empty)`, at which point every planned series is
    // recorded as "the store holds no such series". That is a fingerprint asserting the archive
    // held nothing, which is exactly the fabricated witness the paragraph below refuses. `None`
    // keeps the archive case an honest refusal instead.
    //
    // It is reported as the ordinary `Err` both call sites already handle: they print the reason
    // and record `fingerprint: null`, which is exactly the discipline stated above — a fingerprint
    // is metadata about a run that has not happened yet and may never fail one. Returning `Ok` with
    // an empty fingerprint would be the worse answer: a witness claiming the store held nothing.
    let Some(store) = store else {
        return Err("the run reads archive Parquet in place (--archive), and a data fingerprint \
                    needs the per-series part-file list an archive store does not keep"
            .to_string());
    };
    let range = profile.range().unwrap_or_default();
    let held =
        store.list_series().map_err(|e| format!("{store_label} could not list its series: {e}"))?;

    let mut series = Vec::new();
    for id in run_fingerprint::planned_series_ids(profile, &held) {
        if !held.contains(&id) {
            // The store holds no such series. A REAL and reportable state — a tick profile naming a
            // lane that was never recorded — and a DIFFERENT answer from an empty one.
            series.push(run_fingerprint::SeriesFingerprint {
                id,
                coverage: None,
                commits: Vec::new(),
                commits_len: 0,
            });
            continue;
        }
        // ⚠ **ONE manifest parse, not two.** This was `series_coverage` followed by
        // `series_commits`, and each of those folds the series' manifest INDEPENDENTLY — so the
        // collector parsed a multi-megabyte grouped manifest twice per series. `series_facts`
        // answers both from one parse; the two halves are the same values those accessors return,
        // because they share its fold. That halving is what lets the SEARCH path call this
        // collector at all: a search was already paying one parse per series for its data witness,
        // so it now gets the run ADDRESS for no additional read.
        //
        // The diagnostics merged with the calls. They were two messages naming the same series for
        // two failures of the same parse, and nothing asserted on either.
        let (coverage, commits) =
            store.series_facts(&id).map_err(|e| format!("the store could not read {id:?}: {e}"))?;
        // ⚠ BOUNDED, and `run_fingerprint::MAX_COMMIT_KEYS` carries the arithmetic: a grouped
        // series' log is the whole venue's flush log, which is ~6 MB of JSON for a 30-day window
        // and went into EVERY run directory unbounded. A prefix plus the true count, exactly the
        // shape `vike_model::runs::MAX_TRADES` / `RunTrades::source_len` already uses for the ledger.
        let (commits, commits_len) = run_fingerprint::bound_commits(commits);
        series.push(run_fingerprint::SeriesFingerprint {
            id,
            coverage: Some(coverage),
            commits,
            commits_len,
        });
    }

    Ok(run_fingerprint::DataFingerprint {
        schema: run_fingerprint::DATA_FINGERPRINT_SCHEMA,
        store: store_label.to_string(),
        from_ms: range.start,
        to_ms: range.end,
        series,
    })
}

/// Everything [`persist_run`] needs about a run, gathered into one value.
///
/// A struct rather than loose parameters for two reasons: clippy's `too_many_arguments` (the merge
/// gate is `-D warnings`) would refuse the eight this ends up carrying, and a call site that reads
/// as a list of named facts is what lets a later task ADD one without touching every caller.
/// `crates/vike-studio-core/src/study_run.rs`'s `RunFacts` is the same shape for the same reason.
struct BacktestRunFacts<'a> {
    profile: &'a BacktestProfile,
    /// As the operator spelled it on the command line, NOT canonicalized: a canonical path answers
    /// about the box the run happened on; their own spelling is what they can paste back.
    profile_path: &'a str,
    /// The profile's own TEXT, as it was parsed — read ONCE by [`run`], never re-read here. See
    /// `vike_model::runs::CONFIG_FILE` for why the config is text rather than a serialized struct.
    profile_toml: &'a str,
    /// Which hist store the run read. The profile names a slice; only this says where the bytes
    /// behind it came from.
    store_root: &'a Path,
    /// Where run directories live. A PARAMETER — resolved once by [`run`], which already owns the
    /// process environment sweep — so this function is testable at all and so ONE walk decides
    /// which project a process is in.
    runs_root: &'a Path,
    /// What the store held for every series this run reads, captured BEFORE the run — `None` for a
    /// producer that could not ask (nothing in this file today, but the field is an `Option` so a
    /// test can persist without a store).
    data: Option<&'a run_fingerprint::DataFingerprint>,
    /// The run's INPUT ADDRESS — `crate::run_fingerprint::input_fingerprint`. `None` when this
    /// producer had no config text to hash, which no production path is today.
    fingerprint: Option<&'a str>,
    /// See `vike_model::runs::BuildStamp`. `None` from the standalone engine binary.
    build: Option<runs::BuildStamp<'a>>,
    started_at: i64,
    finished_at: i64,
}

/// Write the finished run to `<runs_root>/<run_id>/` — `manifest.json` (the COMMON manifest every
/// producer writes identically) beside `report.json`, `series.json` and `trades.json`.
///
/// ⚠ **The path is NOT resolved here any more, and that is the point.** It used to call
/// `std::env::current_dir()` inside itself, which made this function untestable without mutating
/// the working directory — something `crates/vike-backtest/CLAUDE.md`'s test-grouping rule forbids,
/// and the reason it had no test at all. [`run`] resolves it now, through
/// `vike_model::state_path::user_runs_dir_from`, from the SAME environment sweep and the SAME
/// working directory the indicator load uses.
///
/// ⚠ **The `VIKE_USER_DATA_DIR` residual this function used to declare is CLOSED.** It redirects
/// the runs directory now, because `user_runs_dir_from` exists beside `RUNS_SUBDIR` in `vike-model`
/// — which is exactly where the old doc said the fix belonged. An operator who redirects
/// `user_data` no longer gets their indicators from the override and their runs from the project.
///
/// ⚠ **The RESULT is persisted, not just the report.** `report.json` holds the ten derived scalars;
/// [`runs::RunSeries`] and [`runs::RunTrades`] hold what they were derived FROM, plus the
/// diagnostic counters that tell a zero-trade run apart from a broken one. Every one of those was
/// computed and dropped when this function took only a report.
///
/// Failures come back as a STRING rather than as an error type: every one of them ends up in the
/// same one-line stderr message, and there is no caller that could act on the difference.
fn persist_run(
    facts: &BacktestRunFacts<'_>,
    result: &crate::BacktestResult,
    report: &harness::BacktestReport,
) -> Result<PathBuf, String> {
    // Creating the directory is what MINTS the id, which is why it happens before the manifest is
    // built rather than after — `vike_model::runs::RunManifest::run_id` carries that argument.
    let run = runs::create_run_dir(facts.runs_root, facts.started_at, facts.fingerprint)
        .map_err(|e| e.to_string())?;

    let manifest = runs::RunManifest {
        schema: runs::MANIFEST_SCHEMA,
        run_id: run.run_id.clone(),
        kind: runs::BACKTEST_RUN_KIND.to_string(),
        // The BINARY's name, spelled literally — the same distinction `--version` above makes, and
        // for the same reason: `CARGO_PKG_NAME` is `vike-backtest`, which is not what was invoked.
        produced_by: "backtest".to_string(),
        started_at: runs::utc_rfc3339(facts.started_at),
        finished_at: runs::utc_rfc3339(facts.finished_at),
        git_sha: facts.build.and_then(|b| b.git_sha).map(str::to_string),
        fingerprint: facts.fingerprint.map(str::to_string),
        config: runs::RunConfig {
            path: Some(facts.profile_path.to_string()),
            name: facts.profile.name.clone(),
        },
        // Everything below here is a BACKTEST's business and nests, so a listing that has never
        // heard of a backtest still renders every field above it.
        detail: serde_json::json!({
            "strategy": facts.profile.strategy.name,
            // ⚠ Factored into [`run_detail_data`] so the SEARCH producer describes its slice with
            // the same keys. Byte-identical to the literal it replaced — this is a shared spelling,
            // not a shape change to a document already on people's disks.
            "data": run_detail_data(facts.profile, facts.data),
            // The COST MODEL, as the two keys a listing needs — see [`run_detail_realism`] for why
            // the whole stamp stays in `report.json`. Nothing above this line says what a fill was
            // charged, so a listing of forty runs could not tell the free ones from the costed
            // ones and every comparison across it was unsound.
            "realism": run_detail_realism(facts.profile),
            // Which hist store the run read. The profile names a slice; only this says where the
            // bytes behind it came from, and `--store`/`$VIKE_HIST_STORE`/the repo default are
            // three different answers on one box.
            "store": facts.store_root.display().to_string(),
            // The whole identity line — commit, tree state, build timestamp, rustc and target.
            // NESTS, because only the sha is a question every kind of run answers; `null` when this
            // binary could not name its build, which is the standalone engine's ordinary state.
            "build": facts.build.and_then(|b| b.summary),
        }),
    };

    let series = run_series_from(result);
    let trades = run_trades_from(result);
    let extras = runs::RunExtras {
        config_toml: Some(facts.profile_toml),
        series: Some(&series),
        trades: Some(&trades),
    };
    runs::write_run_with(&run.path, &manifest, report, &extras).map_err(|e| e.to_string())?;
    Ok(run.path)
}

/// The `data` half of a run manifest's `detail` — what slice was loaded and, when the producer
/// could ask, what the store held for it.
///
/// Factored out of [`persist_run`]'s literal rather than written twice: [`finish_search_run`]
/// answers the same question about the same profile, and two copies of these keys would drift the
/// first time either changed.
///
/// ⚠ **`store` is deliberately NOT folded in here**, even though both producers also record it. It
/// sits one level UP, beside `strategy` and `build`, in the document `persist_run` has been writing
/// since before this function existed — moving it inside `data` to make one tidier helper would
/// change the shape of a document already on people's disks for no reader's benefit. The search
/// producer spells the same one-line key at the same level, which is what keeps the two manifests
/// the same SHAPE rather than merely sharing a function.
///
/// `data` is `None` for a producer that could not inventory the store, and for nothing else.
///
/// ⚠ **A SEARCH used to be permanently that producer and is not any more.** The claim here was that
/// `collect_data_fingerprint` costs two manifest parses per series, so a search — which is about to
/// run hundreds of backtests — could not afford it and recorded `data.fingerprint: null` with a
/// pid-form run id. `DataFusionHist::series_facts` removed the second parse, and a search was
/// already paying the first one for `crate::trial_ledger::SearchIdentity::store_data`, so both
/// producers fill this in now for the same one-parse-per-series price. The single-run call site is
/// still BELOW the sweep branch; nothing was hoisted.
///
/// What a resume compares is still `SearchIdentity` rather than this — it witnesses the ingest
/// COMMIT KEYS, which the ADDRESS deliberately excludes, so the two answer different questions and
/// neither replaces the other.
fn run_detail_data(
    profile: &BacktestProfile,
    data: Option<&run_fingerprint::DataFingerprint>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": match profile.data.kind {
            harness::DataKind::Bar => "bar",
            harness::DataKind::Tick => "tick",
        },
        "interval": profile.data.interval,
        "from": profile.data.from,
        "to": profile.data.to,
        // `resolved_series` so BOTH profile spellings — `venue` x `symbols` and the cross-venue
        // `[[data.series]]` array — record the same thing: what was loaded.
        "series": profile
            .data
            .resolved_series()
            .iter()
            .map(|s| format!("{}:{}", s.venue, s.symbol))
            .collect::<Vec<_>>(),
        // The fingerprint NESTS under `data` beside the slice it describes, so a reader that
        // already knows where to look for the window finds the bytes there too. `null` when this
        // producer had no store to ask.
        "fingerprint": data,
    })
}

/// The `realism` half of a run manifest's `detail`: the COST MODEL, in the two keys a LISTING
/// needs.
///
/// ⚠ **The DIGEST, not the whole stamp, and that asymmetry is the point.** The full resolved key
/// set is in `report.json` (`vike_analytics::report::BacktestReport::realism`), which is where a
/// reader goes to compare two runs key by key. The manifest's job is the one a listing does without
/// opening anything else — "is this run comparable to the other forty", and above all "was this one
/// FREE" — so it carries the verdict and a one-line summary and nothing more. Repeating forty keys
/// in a second document per run would buy no reader anything and would make the two documents a
/// pair somebody has to keep in step.
///
/// Factored out for the same reason as [`run_detail_data`]: two producers answer this about the
/// same profile, and two copies of these keys would drift the first time either changed.
fn run_detail_realism(profile: &BacktestProfile) -> serde_json::Value {
    let stamp = harness::report::realism_stamp(profile);
    serde_json::json!({
        // The REASON, or `null`. Not a bool: an operator reading a listing row needs to know WHICH
        // cost channel was off, and `true` sends them back to the profile to work it out.
        "frictionless": stamp.frictionless,
        "digest": stamp.digest(),
    })
}

/// What the store HELD for every series this search's profile resolves to — the witness
/// `crate::trial_ledger::SearchIdentity::store_data` carries, and the input `--resume`'s gate needs
/// in order to be sound at all.
///
/// # Why this exists, in one sentence
///
/// The store PATH is not the store. Without this, a search killed at trial 60 of 200, a backfill or
/// a corrected re-fetch inside the profile's own `[data] from/to`, and a `--resume` would rank 60
/// cached scores computed on dataset A against 140 computed on dataset B — no error, no warning,
/// and no field in any persisted document from which a reader could detect it afterwards.
/// `SearchIdentity`'s own doc carries the argument in full.
///
/// # ⚠ It costs NOTHING, because it is a RENDERING rather than a read
///
/// **This used to walk the store itself** — one `list_series` plus one `series_commits` per series
/// — and the argument for a second walk was that [`collect_data_fingerprint`] cost TWO manifest
/// parses per series where this cost one. That is no longer true: `DataFusionHist::series_facts`
/// answers coverage and commits from ONE parse, so the collector costs exactly what this walk cost,
/// and everything this function needs is already in the record the collector returns. The witness
/// is now a pure function of [`run_fingerprint::DataFingerprint`], which is why it takes one.
///
/// ⚠ **The rendered TEXT is byte-identical to what the walk produced**, and that is load-bearing
/// rather than tidy: `store_data` is compared as a STRING by
/// `crate::trial_ledger::SearchIdentity::differences`, so a search started by an older binary must
/// still be `--resume`-able by this one. `coverage: None` is exactly the `!held` case the walk
/// tested, and `commits`/`commits_len` are the same bounded prefix and true count it wrote.
///
/// The one behaviour that did NOT move with it is the DIAGNOSTIC: the collector's failure reaches
/// stderr as `run NOT addressed` on the single-run path, and the search's call site still prints
/// its own line naming the witness. A search has never printed the single-run wording and still
/// does not.
///
/// # What it CATCHES and what it MISSES — both stated
///
/// It is the INGEST COMMIT KEYS: the store's own record of who wrote each series, appended by every
/// write. A backfill that adds a day, a re-fetch that replaces a corrected day and a delete all
/// move it — including the re-fetch case, which a coverage-based witness (rows, dates, first/last
/// ts) would MISS whenever the corrected day has the same shape as the day it replaced. That is why
/// the commits are the witness here even though
/// `crate::run_fingerprint::SeriesFingerprint::commits` is deliberately NOT part of the run
/// ADDRESS.
///
/// ⚠ **The accepted cost of that choice**, from the same place:
/// `crates/vike-data/src/datafusion_hist/manifest.rs`'s `rebuild_manifest` re-derives keys from part
/// footers and can come back with FEWER than the data was written with. So a manifest rebuild can
/// move this witness without the data moving, and a resume is then REFUSED that need not have been.
/// That is the safe direction — a needless re-run costs time, a wrong reuse costs the answer — and
/// it is why the refusal message names a rebuild-shaped cause rather than asserting a data change.
///
/// ⚠ The keys are bounded by `crate::run_fingerprint::bound_commits`, so a series with a long flush
/// log contributes a bounded prefix plus its true length: an actively-recorded group still moves the
/// witness on its very next write, and the witness itself cannot grow without limit.
fn data_witness(data: &run_fingerprint::DataFingerprint) -> String {
    use std::fmt::Write;
    let mut lines: Vec<String> = Vec::with_capacity(data.series.len());
    for s in &data.series {
        let mut line = format!(
            "series {} {} {} {} {}",
            s.id.kind,
            s.id.venue,
            if s.id.symbol.is_empty() { "-" } else { &s.id.symbol },
            s.id.group.as_deref().unwrap_or("-"),
            s.id.interval.as_deref().unwrap_or("-"),
        );
        match &s.coverage {
            // The store holds no such series — a REAL state, and a DIFFERENT one from a series that
            // exists and is empty. It is also the state a later seed or backfill moves AWAY from,
            // which is exactly what this witness is for. `coverage: None` is the collector's own
            // spelling of it, decided against the store's INVENTORY rather than against an
            // all-zero manifest fold (see [`collect_data_fingerprint`]).
            None => line.push_str(" absent"),
            Some(_) => {
                // ⚠ `commits_len` rather than `commits.len()`: the TRUE count, before
                // `run_fingerprint::bound_commits` truncated the prefix. An actively-recorded group
                // moves the witness on its very next write even past the bound, and the witness
                // itself cannot grow without limit.
                let _ = write!(line, " commits {}", s.commits_len);
                for key in &s.commits {
                    let _ = write!(line, " {key}");
                }
            }
        }
        lines.push(line);
    }
    // SORTED, so the order the loader happened to list series in cannot move the witness — the same
    // rule `run_fingerprint::DataFingerprint::canonical` states for the address.
    lines.sort();
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The parent run of a parameter search, opened BEFORE the search runs.
struct SearchRun {
    run_id: String,
    path: PathBuf,
    /// The id this search was resumed FROM, when it was — see
    /// `crate::trial_ledger::SearchHeader::resumed_from`.
    resumed_from: Option<String>,
}

/// Mint (or, on `--resume`, re-open) the search's parent run directory and write its
/// `crate::trial_ledger::SEARCH_FILE`.
///
/// ⚠ **Called BEFORE `harness::optimize`**, unlike [`persist_run`], and that is forced rather than
/// chosen: the ledger is written as trials complete, so it needs a directory to be written into.
/// The FAILURE posture for a MINT is unchanged — a caller that gets an `Err` prints it and searches
/// anyway. A RESUME is the opposite and its call site says why.
fn open_search_run(
    identity: trial_ledger::SearchIdentity,
    keep: KeepTrials,
    resume: Option<&str>,
    runs_root: &Path,
    started_at: i64,
    fingerprint: Option<&str>,
) -> Result<SearchRun, String> {
    let (run_id, path, resumed_from) = match resume {
        Some(id) => reopen_search_run(runs_root, id, &identity)?,
        None => {
            // Creating the directory is what MINTS the id — `vike_model::runs::RunManifest::run_id`
            // carries that argument, and `crates/vike-backtest/CLAUDE.md` forbids turning it into
            // an exists-test.
            //
            // ⚠ The ADDRESS rides in the id here exactly as it does for a single run, and `None`
            // is a real answer rather than this producer's permanent state: a store that could not
            // be inventoried keeps the pid form, which is what "ABSENT, never DIFFERENT" looks like
            // in a directory name. ⚠ A RESUME mints nothing, so the resumed run keeps the id it was
            // minted with — which is the whole point of resuming one.
            let run = runs::create_run_dir(runs_root, started_at, fingerprint)
                .map_err(|e| e.to_string())?;
            (run.run_id, run.path, None)
        }
    };

    let header = trial_ledger::SearchHeader {
        schema: trial_ledger::TRIAL_LEDGER_SCHEMA,
        run_id: run_id.clone(),
        keep_trials: keep.as_str().to_string(),
        trials_file: trial_ledger::TRIALS_FILE.to_string(),
        identity,
        resumed_from: resumed_from.clone(),
    };
    trial_ledger::write_search_header(&path, &header).map_err(|e| e.to_string())?;
    Ok(SearchRun { run_id, path, resumed_from })
}

/// `--resume`'s half of [`open_search_run`]: re-open an existing search parent after proving it is
/// the SAME search.
///
/// ⚠ It reads `crate::trial_ledger::SEARCH_FILE`, not the manifest, and that is the whole reason
/// that file exists: the manifest is written LAST as the completion marker, so the run an operator
/// most wants to resume — one that was killed — does not have one.
///
/// Every refusal is a REFUSAL rather than a fallback to a fresh search. Silently starting over would
/// look like success while redoing hours of work, and silently reusing scores computed under
/// different inputs would be a wrong answer nobody could see.
fn reopen_search_run(
    runs_root: &Path,
    id: &str,
    identity: &trial_ledger::SearchIdentity,
) -> Result<(String, PathBuf, Option<String>), String> {
    let path = runs_root.join(id);
    if !path.is_dir() {
        return Err(format!(
            "--resume {id:?}: no such run under {}. `backtest trials <id>` and the \
             `search saved to …` line both print the id a run can be resumed by",
            runs_root.display()
        ));
    }
    let header = trial_ledger::read_search_header(&path)
        .map_err(|e| format!("--resume {id:?}: {e}. That run is not a parameter search"))?;
    // ⚠ **Keyed on whether a LEDGER was kept, not on the exact spelling** — this read
    // `!= KeepTrials::Scalars.as_str()` until `returns` existed, at which point a `returns` search
    // (whose ledger is byte-identical to a `scalars` one) would have been refused with "it kept no
    // ledger", which is FALSE, and whose only stated remedy is re-running the whole search. That is
    // the same failure the `--resume` + `--keep-trials none` guard in `parse_search_flags` exists
    // to prevent, arriving from the other side.
    //
    // ⚠ An UNRECOGNISED spelling is refused SEPARATELY and deliberately: it means the run was
    // written by a newer build, so this binary cannot know what its ledger holds. Guessing "it kept
    // one" would resume against a format it has never seen.
    match KeepTrials::from_recorded(&header.keep_trials) {
        Some(mode) if mode.keeps_ledger() => {}
        Some(_) => {
            return Err(format!(
                "--resume {id:?}: that search ran with --keep-trials {}, so it kept no ledger and \
                 there is nothing to resume. Run it again without --resume",
                header.keep_trials
            ));
        }
        None => {
            return Err(format!(
                "--resume {id:?}: that search recorded --keep-trials {:?}, which this build does \
                 not know — it was written by a newer binary, so what its ledger holds cannot be \
                 assumed. Resume it with the build that wrote it, or run the search again without \
                 --resume",
                header.keep_trials
            ));
        }
    }
    let diffs = header.identity.differences(identity);
    if !diffs.is_empty() {
        return Err(format!(
            "--resume {id:?}: this is not the same search. A cached score is an answer to the \
             inputs it was computed under, so a difference is refused rather than resolved:\n  {}",
            diffs.join("\n  ")
        ));
    }
    Ok((header.run_id, path, Some(id.to_string())))
}

/// Resolve the search's LEDGER into the document its `report.json` holds — and, on a resume, into
/// the thing it PRINTS.
///
/// ⚠ **Separated from the write ([`write_search_run`]) deliberately, and the separation is a
/// correctness fix rather than tidiness.** While the two were one function, a resume's output came
/// out of the persist call's `Ok` arm — so a full disk or a directory that lost write permission
/// mid-run gave the operator EMPTY STDOUT with exit 0, and `crates/vike-cli/src/cmd/backtest.rs`'s
/// `--local` arm prints that verbatim, handing a `--json` consumer a zero-byte document and a
/// success. The answer a run computed must not depend on whether its metadata could be saved; that
/// is the same posture `persist_run` states, which the single-run path has always had and this one
/// did not.
///
/// ⚠ `overfit` arrives as a PARAMETER rather than being computed here, and the split is the same
/// one this function's own separation makes: the statistics are an in-memory answer of the process
/// that searched (they need the ranked rows and the evaluator's retained matrix), while everything
/// else here is resolved off DISK from the ledger. `None` for every search that did not opt into
/// `--keep-trials returns`, and for one whose matrix was not measurable.
fn build_trials_document(
    run: &SearchRun,
    identity: &trial_ledger::SearchIdentity,
    keep: KeepTrials,
    tally: harness::RecorderTally,
    overfit: Option<trial_ledger::OverfitStats>,
) -> Result<trial_ledger::TrialsDocument, String> {
    // A ledger that was never written (`--keep-trials none`, or a run whose every append failed) is
    // an EMPTY document rather than an error: the counts below still say what the search spent.
    let read = match trial_ledger::read_trials(&run.path) {
        Ok(read) => read,
        Err(runs::RunReadError::Missing { .. }) => trial_ledger::TrialsRead::default(),
        Err(e) => return Err(e.to_string()),
    };
    let unreadable = read.unreadable.len();
    let (trials, superseded) = trial_ledger::latest_by_n(read.trials);
    let failed = trials.iter().filter(|t| t.error.is_some()).count();

    Ok(trial_ledger::TrialsDocument {
        schema: trial_ledger::TRIAL_LEDGER_SCHEMA,
        run_id: run.run_id.clone(),
        keep_trials: keep.as_str().to_string(),
        identity: identity.clone(),
        evaluated: tally.evaluated,
        reused: tally.reused,
        failed,
        unreadable,
        superseded,
        trials,
        overfit,
    })
}

/// Write the search's `vike_model::runs::REPORT_FILE` (the [`build_trials_document`] result) and
/// then its `vike_model::runs::MANIFEST_FILE` — report first, manifest last, exactly as
/// [`persist_run`] and `runs::write_run` do, so a directory holding a manifest is still a run that
/// finished writing.
///
/// Called AFTER the answer has been printed and never fatal, which is [`persist_run`]'s posture and
/// now this path's too — see [`build_trials_document`] for what it cost while it was not.
#[allow(clippy::too_many_arguments)]
fn write_search_run(
    run: &SearchRun,
    profile: &BacktestProfile,
    profile_path: &str,
    // ⚠ **What to RECORD as the provenance of this data — a path on a local run, the datahub's
    // ADDRESS on a routed one.** It was `store_root: &Path` until
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` routed this path, and a `&Path`
    // here is no longer a fact the caller has: on the wire arm the resolved root is THIS box's
    // default, a directory the run never opened, and recording it would put a confident falsehood
    // in the one document a later reader uses to reproduce the run. `HistoryRoute::label` renders
    // it, so the record and the run's own disclosure cannot disagree.
    store_label: &str,
    identity: &trial_ledger::SearchIdentity,
    keep: KeepTrials,
    build: Option<runs::BuildStamp<'_>>,
    started_at: i64,
    finished_at: i64,
    document: &trial_ledger::TrialsDocument,
    data: Option<&run_fingerprint::DataFingerprint>,
    fingerprint: Option<&str>,
) -> Result<PathBuf, String> {
    let manifest = runs::RunManifest {
        schema: runs::MANIFEST_SCHEMA,
        run_id: run.run_id.clone(),
        kind: trial_ledger::SEARCH_RUN_KIND.to_string(),
        produced_by: "backtest".to_string(),
        started_at: runs::utc_rfc3339(started_at),
        finished_at: runs::utc_rfc3339(finished_at),
        git_sha: build.and_then(|b| b.git_sha).map(str::to_string),
        // ⚠ **The SAME address a single run carries, computed the SAME way** —
        // `run_fingerprint::input_fingerprint` over the config TEXT and the store's own coverage —
        // so a search and a backtest over identical inputs land in one comparable space rather than
        // in two that merely look alike. `None` only when the store could not be inventoried, which
        // is the ABSENT-never-DIFFERENT posture the call site argues.
        //
        // The `search.json` identity is still what a RESUME compares, and it is a different
        // question: it witnesses the ingest COMMIT KEYS, which this address deliberately excludes
        // (`run_fingerprint::SeriesFingerprint::commits` says why an address over a rebuildable
        // log would orphan every baseline).
        fingerprint: fingerprint.map(str::to_string),
        config: runs::RunConfig {
            path: Some(profile_path.to_string()),
            name: profile.name.clone(),
        },
        detail: serde_json::json!({
            "strategy": profile.strategy.name,
            "data": run_detail_data(profile, data),
            // ⚠ The BASE profile's cost model, which every point inherits except on a key its own
            // `[paramscan]` overrides. A grid that sweeps `engine.fee_rate` therefore has points
            // this one row does not describe — and recording nothing would be worse, because then
            // a search's manifest says nothing about costs at all, which is the defect the stamp
            // exists to close. The per-point stamp belongs on the per-point report.
            "realism": run_detail_realism(profile),
            "store": store_label,
            "build": build.and_then(|b| b.summary),
            // What a LISTING needs in order to render a search row without opening the ledger.
            "search": {
                "schema": trial_ledger::TRIAL_LEDGER_SCHEMA,
                "optimizer": identity.method,
                "rank_by": identity.rank_by,
                "seed": identity.seed,
                "budget": identity.budget,
                "resumed_from": run.resumed_from,
            },
            // Read off the DOCUMENT rather than recomputed, so a listing's summary and the report
            // beside it cannot disagree about one search.
            "trials": {
                "file": trial_ledger::TRIALS_FILE,
                "keep": keep.as_str(),
                "evaluated": document.evaluated,
                "reused": document.reused,
                "failed": document.failed,
                "unreadable": document.unreadable,
                "superseded": document.superseded,
            },
        }),
    };

    runs::write_run(&run.path, &manifest, document).map_err(|e| e.to_string())?;
    Ok(run.path.clone())
}

/// The reading verb for a finished search. Spelled once, because [`run`] routes on it and
/// [`SUBCOMMANDS`] refuses it as a POSITIONAL profile for exactly that reason.
const TRIALS_SUBCOMMAND: &str = "trials";

const TRIALS_USAGE: &str = "\
usage: backtest trials <search-run-id> [--sort FIELD] [--top N] [--json]
                [--export-params FILE]

  <search-run-id>  a FULL run id, as `backtest: search saved to …` printed it and as the run
                   directory under <project>/user_data/runs/ is named. Prefixes, `@last` and
                   marks are the design's shared SELECTOR GRAMMAR and are not implemented here
  --sort FIELD     score (default) | n | return | sharpe | max_dd | trades | equity.
                   `score` and `n` come from the ledger; the rest are read out of each trial's
                   recorded metrics. A trial with no metric for the field sorts LAST
  --top N          print only the first N rows after sorting. The document's counts still
                   describe the WHOLE search
  --json           print the run's own TrialsDocument instead of the table — the same schema
                   report.json holds
  --export-params FILE
                   write the top-sorted trial's overrides as a [strategy.params] TOML fragment
  -h, --help       print this and exit 0";

/// Which field `--sort` ordered by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrialSort {
    Score,
    N,
    Return,
    Sharpe,
    MaxDd,
    Trades,
    Equity,
}

impl TrialSort {
    /// ONE table: `(flag spelling, the variant, the `metrics` key it reads, bigger-is-better)`. The
    /// key is `None` for the two fields that come off the record itself rather than out of its
    /// metrics. Driving the name, the parse and the metric lookup from one row is what stops a
    /// seventh field being added to two of the three and forgotten in the third.
    ///
    /// ⚠ The metric keys are `harness::BacktestReport`'s SERIALIZED field names, because that is
    /// what `crate::trial_ledger::TrialRecord::metrics` holds — `serde_json::to_value` of a real
    /// report.
    const ROWS: &'static [(&'static str, TrialSort, Option<&'static str>, bool)] = &[
        ("score", TrialSort::Score, None, true),
        ("n", TrialSort::N, None, false),
        ("return", TrialSort::Return, Some("total_return"), true),
        ("sharpe", TrialSort::Sharpe, Some("sharpe"), true),
        ("max_dd", TrialSort::MaxDd, Some("max_drawdown"), false),
        ("trades", TrialSort::Trades, Some("n_trades"), true),
        ("equity", TrialSort::Equity, Some("final_equity"), true),
    ];

    fn names() -> String {
        Self::ROWS.iter().map(|(n, ..)| *n).collect::<Vec<_>>().join("|")
    }

    fn from_name(s: &str) -> Option<Self> {
        let lower = s.to_ascii_lowercase();
        Self::ROWS.iter().find(|(n, ..)| *n == lower).map(|(_, v, _, _)| *v)
    }

    /// The metric key and direction, or `None` for `score`/`n`.
    fn metric(self) -> Option<(&'static str, bool)> {
        Self::ROWS
            .iter()
            .find(|(_, v, _, _)| *v == self)
            .and_then(|(_, _, k, d)| k.map(|k| (k, *d)))
    }
}

fn parse_trial_sort(args: &[String]) -> Result<TrialSort, String> {
    match required_value(args, "--sort", &format!("expected {}", TrialSort::names()))?.as_deref() {
        None => Ok(TrialSort::Score),
        Some(v) => TrialSort::from_name(v)
            .ok_or_else(|| format!("invalid --sort {v:?} (expected {})", TrialSort::names())),
    }
}

/// Order a document's trials for DISPLAY, and truncate.
///
/// ⚠ The persisted document is always in `n` order; this reorders a COPY. Unrankable rows (a `NaN`
/// score, or a missing metric) sort LAST under every field, which is the same rule
/// `harness::cmp_scores_desc` applies to a sweep report: a failed point must never lead a table.
/// `sort_by` is STABLE, so ties keep evaluation order — the property the whole search path already
/// relies on.
fn sort_trials(
    doc: &trial_ledger::TrialsDocument,
    sort: TrialSort,
    top: Option<usize>,
) -> Vec<trial_ledger::TrialRecord> {
    let mut rows = doc.trials.clone();
    match sort {
        TrialSort::N => rows.sort_by_key(|t| t.n),
        TrialSort::Score => rows.sort_by(|a, b| harness::cmp_scores_desc(a.score, b.score)),
        other => {
            let (key, bigger_is_better) = other.metric().expect("every other variant has a metric");
            rows.sort_by(|a, b| {
                let get = |t: &trial_ledger::TrialRecord| {
                    t.metrics.as_ref().and_then(|m| m.get(key)).and_then(|v| v.as_f64())
                };
                match (get(a), get(b)) {
                    (Some(x), Some(y)) => {
                        let ord = harness::cmp_scores_desc(x, y);
                        if bigger_is_better { ord } else { ord.reverse() }
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            });
        }
    }
    if let Some(n) = top {
        rows.truncate(n);
    }
    rows
}

/// The human table. `rank` is the row's position AFTER sorting; `#n` is its EVALUATION index, and
/// the two are deliberately separate columns — conflating them is how `<id>#<n>` stops being a
/// stable address.
fn render_trials(doc: &trial_ledger::TrialsDocument, rows: &[trial_ledger::TrialRecord]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let seed = doc.identity.seed.map(|v| format!(", seed {v}")).unwrap_or_default();
    let _ = writeln!(
        s,
        "search {} — {}, ranked by {}{} ({} evaluated, {} reused, {} failed)",
        doc.run_id,
        doc.identity.method,
        doc.identity.rank_by,
        seed,
        doc.evaluated,
        doc.reused,
        doc.failed
    );
    if doc.unreadable > 0 || doc.superseded > 0 {
        let _ = writeln!(
            s,
            "  ⚠ {} ledger line(s) unreadable, {} superseded by a later line",
            doc.unreadable, doc.superseded
        );
    }
    if rows.is_empty() {
        let _ =
            writeln!(s, "no trials recorded (keep-trials = {}) — nothing to rank", doc.keep_trials);
        return s;
    }
    for (i, t) in rows.iter().enumerate() {
        let rank = if i == 0 { "*1".to_string() } else { (i + 1).to_string() };
        let overrides =
            t.overrides.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
        let score = if t.score.is_finite() { format!("{:.4}", t.score) } else { "n/a".to_string() };
        match (&t.metrics, &t.error) {
            (Some(m), _) => {
                let f = |k: &str| m.get(k).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
                let _ = writeln!(
                    s,
                    "{rank:<4} #{:<5} {overrides:<30} score={score} ret={:.4} sharpe={:.4} \
                     max_dd={:.4} trades={}",
                    t.n,
                    f("total_return"),
                    f("sharpe"),
                    f("max_drawdown"),
                    m.get("n_trades").and_then(|v| v.as_u64()).unwrap_or(0)
                );
            }
            (None, Some(e)) => {
                let _ = writeln!(s, "{rank:<4} #{:<5} {overrides:<30} FAILED: {e}", t.n);
            }
            (None, None) => {
                let _ = writeln!(s, "{rank:<4} #{:<5} {overrides:<30} (no metrics recorded)", t.n);
            }
        }
    }
    s
}

/// `<project>/user_data/runs/<id>`, or a message saying which of the two things was missing.
///
/// ⚠ `user_runs_dir_from` is the WALK, and this binary may call it:
/// `crates/vike-boot/tests/one_owner.rs`'s `a_crate_that_boots_may_not_walk_again` forbids that call
/// only in a crate that calls `vike_boot::boot`, and this one does not. `vike-cli` DOES, so an
/// operator-facing verb built over this one must derive its root from `Resolved::user_data_dir`
/// joined with `RUNS_SUBDIR` instead.
fn locate_run(
    vars: &std::collections::HashMap<String, String>,
    id: &str,
) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("no working directory: {e}"))?;
    let runs_root = vike_model::state_path::user_runs_dir_from(
        vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
        &cwd,
    )
    .ok_or_else(|| {
        format!(
            "no project directory above {} — a run lives under <project>/user_data/runs/, and \
             this working directory has no project marker above it",
            cwd.display()
        )
    })?;
    let dir = runs_root.join(id);
    if dir.is_dir() { Ok(dir) } else { Err(format!("no run {id:?} under {}", runs_root.display())) }
}

/// The run's `TrialsDocument`, refusing a run that is not a search.
///
/// Prefers the persisted `runs::REPORT_FILE` and falls back to rebuilding from the ledger, which is
/// what an INTERRUPTED search has: its manifest and report were never written, but `search.json`
/// and however many ledger lines completed are on disk. A verb that could only read a finished
/// search would be useless on exactly the run an operator most wants to look at.
fn load_trials_document(dir: &Path, id: &str) -> Result<trial_ledger::TrialsDocument, String> {
    if let Ok(manifest) = runs::read_manifest(dir) {
        if manifest.kind != trial_ledger::SEARCH_RUN_KIND {
            return Err(format!(
                "run {id:?} is a {:?} run, not a {:?} — `trials` lists the child trials of a \
                 parameter SEARCH, and a run of another kind has none",
                manifest.kind,
                trial_ledger::SEARCH_RUN_KIND
            ));
        }
        let path = dir.join(runs::REPORT_FILE);
        if let Ok(text) = std::fs::read_to_string(&path) {
            return serde_json::from_str(&text)
                .map_err(|e| format!("cannot parse {}: {e}", path.display()));
        }
    }
    // No manifest (or no report): an unfinished search. `search.json` is written FIRST precisely so
    // this case has an identity to report.
    let header = trial_ledger::read_search_header(dir).map_err(|e| {
        format!("run {id:?} carries no search header — {e}. It is not a parameter search")
    })?;
    let read = match trial_ledger::read_trials(dir) {
        Ok(read) => read,
        Err(runs::RunReadError::Missing { .. }) => trial_ledger::TrialsRead::default(),
        Err(e) => return Err(e.to_string()),
    };
    let unreadable = read.unreadable.len();
    let (trials, superseded) = trial_ledger::latest_by_n(read.trials);
    let failed = trials.iter().filter(|t| t.error.is_some()).count();
    Ok(trial_ledger::TrialsDocument {
        schema: header.schema,
        run_id: header.run_id,
        keep_trials: header.keep_trials,
        identity: header.identity,
        // An unfinished search has no tally of its own; the ledger's length is what is known.
        evaluated: trials.len(),
        reused: 0,
        failed,
        unreadable,
        superseded,
        trials,
        // ⚠ **`None`, and it cannot be otherwise.** The statistics are computed from the in-memory
        // return matrix of the process that SEARCHED, and the vectors are deliberately not
        // persisted (`crate::trial_ledger::OverfitStats`' doc carries that decision and what it
        // costs) — so a run whose `report.json` is missing has no recoverable matrix. Synthesising
        // a block here from the ledger's scalars would be a different statistic wearing the same
        // key names.
        overfit: None,
    })
}

/// Write the top-sorted trial's overrides as a `[strategy.params]` TOML fragment.
///
/// ⚠ A sweep override's key is a BARE param name — `harness::sweep::profile_with_overrides` inserts
/// each one straight into `profile.strategy.params` — so the fragment is that table and nothing
/// else. `toml::Value`'s `Display` already renders TOML syntax for every value a sweep axis can
/// declare, which is the same rendering the sweep report's own table prints.
fn export_params(
    doc: &trial_ledger::TrialsDocument,
    rows: &[trial_ledger::TrialRecord],
    path: &str,
) -> Result<(), String> {
    let best = rows.first().ok_or_else(|| {
        format!(
            "this search recorded no trials (keep-trials = {}), so there are no params to export",
            doc.keep_trials
        )
    })?;
    let score = if best.score.is_finite() {
        format!("{:.6}", best.score)
    } else {
        "unrankable".to_string()
    };
    let mut out = format!(
        "# exported by `backtest trials {} --export-params`\n# trial #{} of {}, score {}\n\
         [strategy.params]\n",
        doc.run_id,
        best.n,
        doc.trials.len(),
        score
    );
    for (k, v) in &best.overrides {
        out.push_str(&format!("{k} = {v}\n"));
    }
    std::fs::write(path, out).map_err(|e| format!("cannot write {path}: {e}"))
}

/// Run `backtest trials <id>`.
///
/// ⚠ **ARTIFACT-ONLY**: no store, no profile, no socket — §6 of the CLI-surface design requires the
/// reading verbs to work "on a laptop with neither, on runs minted months ago".
fn run_trials(vars: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("{TRIALS_USAGE}");
        return ExitCode::SUCCESS;
    }
    let Some(id) = args.first().filter(|a| !a.starts_with('-')) else {
        eprintln!("backtest: `trials` needs a search run id\n\n{TRIALS_USAGE}");
        return ExitCode::from(2);
    };
    // ⚠ The design's SELECTOR GRAMMAR (`@last`, a unique prefix, `@baseline/NAME`, `<id>#<n>`) is
    // ONE implementation shared by every verb that takes a run, and it is not this binary's to
    // invent half of. Refused by NAME so an operator learns what this verb takes, rather than
    // meeting a "no such directory" about a path they never typed.
    if id.starts_with('@') || !id.contains('-') {
        eprintln!(
            "backtest: {id:?} is not a full run id. This verb takes the FULL id — the one \
             `backtest: search saved to …` printed, and the name of the directory under \
             <project>/user_data/runs/. Prefixes, `@last` and marks are the CLI's shared selector \
             grammar and are not implemented here"
        );
        return ExitCode::from(2);
    }

    let sort = match parse_trial_sort(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };
    let top = match required_value(args, "--top", "expected a positive count") {
        Ok(None) => None,
        Ok(Some(v)) => match v.parse::<usize>() {
            Ok(n) if n > 0 => Some(n),
            _ => {
                eprintln!("backtest: invalid --top {v:?} (expected a positive count)");
                return ExitCode::from(2);
            }
        },
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };
    let export = match required_value(args, "--export-params", "expected an output file path") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };

    let dir = match locate_run(vars, id) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };
    let doc = match load_trials_document(&dir, id) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("backtest: {e}");
            return ExitCode::from(2);
        }
    };

    let rows = sort_trials(&doc, sort, top);

    if let Some(path) = export.as_deref() {
        if let Err(why) = export_params(&doc, &rows, path) {
            eprintln!("backtest: --export-params failed — {why}");
            return ExitCode::FAILURE;
        }
        eprintln!("backtest: params written to {path}");
    }

    if has_flag(args, "--json") {
        // The SAME type `report.json` holds, with `trials` in the requested order — the array order
        // IS the rank, which is why no `rank` key is invented. Every diagnostic goes to stderr, so
        // stdout stays a pure document.
        let shown = trial_ledger::TrialsDocument { trials: rows, ..doc };
        match serde_json::to_string_pretty(&shown) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("backtest: failed to serialize the trials document: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        print!("{}", render_trials(&doc, &rows));
    }
    ExitCode::SUCCESS
}

/// The three method-shaped facts a search's identity records: the method's own name, its seed, and
/// its budget scalar.
///
/// ⚠ Read off the RESOLVED [`SearchMethod`] rather than off argv, so a default counts the same as a
/// written value — `--optimizer tpe` and `--optimizer tpe --trials 64` are the same search and must
/// resume each other. `harness::SearchOutcome::summary` cannot supply this: it is a pre-rendered
/// String, and only two of the four methods have a typed budget at all — neither of which reaches
/// the seam. Making the budget machine-readable at that boundary is a separate change.
///
/// ⚠ DELEGATES to `harness::search_select::identity_parts`, which moved there with the method enum
/// itself: a second reading of a `SearchMethod` is a second chance to disagree about what `budget`
/// means for euler.
fn search_identity_parts(method: &SearchMethod) -> (&'static str, Option<u64>, Option<u64>) {
    search_select::identity_parts(method)
}

/// The run's equity SERIES and its diagnostic counters, bounded — [`runs::MAX_EQUITY_SAMPLES`]
/// carries the argument for the bound and the residual it accepts.
///
/// ⚠ **ONE stride for every curve.** `equity_ts` and each per-symbol curve are thinned with the
/// SAME stride as `equity_curve`, because their meaning is positional: a per-symbol curve thinned
/// independently would still have a plausible length and would no longer line up by index with the
/// equity it is supposed to explain.
///
/// ⚠ **Per-bar returns are deliberately NOT persisted.** `vike_analytics::metrics::returns` SKIPS
/// zero-denominator steps, so a returns vector is not index-alignable with either vector here —
/// persisting one beside them would be a misaligned artifact by construction. A reader derives
/// returns from `equity` with that same function, which is also the only way its numbers and the
/// report's Sharpe are guaranteed to agree. [`runs::RunSeries`] states this at the type.
fn run_series_from(result: &crate::BacktestResult) -> runs::RunSeries {
    let (equity, stride) = runs::decimate(&result.equity_curve, runs::MAX_EQUITY_SAMPLES);

    runs::RunSeries {
        schema: runs::SERIES_SCHEMA,
        equity,
        // A real state, not a gap: `BacktestResult::equity_ts` is documented as empty when the
        // producer did not track timestamps (the vector kernels). `RunSeries`'s invariant admits
        // exactly this and nothing between it and "same length".
        equity_ts: keep_at_stride(&result.equity_ts, stride),
        per_symbol_equity: result
            .per_symbol_curves
            .iter()
            .map(|(sym, curve)| (sym.clone(), keep_at_stride(curve, stride)))
            .collect(),
        stride,
        source_len: result.equity_curve.len(),
        diagnostics: runs::RunDiagnostics {
            warmup: result.warmup,
            intrabar_both_hit: result.intrabar_both_hit,
            stale_deferrals: result.stale_deferrals,
            impact_unpriced: result.impact_unpriced,
            session_deferrals: result.session_deferrals,
            below_min_reversals: result.below_min_reversals,
            // The REALISED maker/taker mix and the commission total. Carried rather than declared
            // dropped (`crates/vike-backtest/tests/run_record_completeness.rs`'s first preference)
            // because neither is recoverable from any other document a run writes: `report.json`
            // carries no fee figure at all, and the per-trade `fees` are empty whenever a kernel
            // ran with `build_trades = false`. A run record that could not say what it was charged
            // is the same defect one layer down from the one
            // `docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`
            // ends on the wire.
            maker_fills: result.maker_fills,
            taker_fills: result.taker_fills,
            fees_paid: result.fees_paid,
            dropped: result
                .dropped
                .iter()
                .map(|(symbol, reason, size, weight)| runs::DroppedOrder {
                    symbol: symbol.clone(),
                    reason: reason.clone(),
                    size: *size,
                    weight: *weight,
                })
                .collect(),
        },
    }
}

/// Thin `v` at a stride somebody else DERIVED, keeping the last element the same way
/// [`runs::decimate`] does.
///
/// ⚠ The stride is an ARGUMENT rather than recomputed, and that is the whole point: `equity_ts` and
/// every per-symbol curve are positional companions of `equity_curve`, so a second `decimate` call
/// deriving its own stride would produce vectors of plausible length that no longer line up by
/// index with the equity they are supposed to explain — and [`runs::RunSeries::is_aligned`] would
/// then be checking an invariant this function had already broken.
///
/// ⚠ **Stride `0` means KEEP NOTHING and must be handled before the `stride == 1` fast path.**
/// [`runs::decimate`]'s own doc makes `0` the "nothing was kept" answer — the shape a
/// `--keep-series none` flag spells — and this function once spelled its fast path `stride <= 1`,
/// which swallowed `0` and returned the WHOLE vector. That disagreement is silent and it is
/// exactly the failure [`runs::RunSeries::is_aligned`] exists to make visible: `decimate` would
/// hand back an EMPTY `equity` while this kept every timestamp, so the document this build wrote
/// would fail its own invariant — and the flag meant to SUPPRESS the series would have written a
/// LARGER file than keeping it.
fn keep_at_stride<T: Copy>(v: &[T], stride: usize) -> Vec<T> {
    if v.is_empty() || stride == 1 {
        return v.to_vec();
    }
    if stride == 0 {
        return Vec::new();
    }
    let mut out: Vec<T> = v.iter().step_by(stride).copied().collect();
    let last = v.len() - 1;
    if !last.is_multiple_of(stride) {
        out.push(v[last]);
    }
    out
}

/// The run's closed-trade LEDGER, bounded as a CHRONOLOGICAL PREFIX —
/// [`runs::MAX_TRADES`] carries the argument for why a ledger may not be sampled.
fn run_trades_from(result: &crate::BacktestResult) -> runs::RunTrades {
    runs::RunTrades {
        schema: runs::TRADES_SCHEMA,
        trades: result.trades.iter().take(runs::MAX_TRADES).cloned().collect(),
        source_len: result.trades.len(),
    }
}

// ─── `backtest data`: the five operations ruling 12 moved off the flag surface ───────────────────

/// The verb that carries them. Spelled once, because [`run`] routes on it and
/// [`profile_from_args`] refuses it as a POSITIONAL for exactly that reason.
const DATA_SUBCOMMAND: &str = "data";

/// The five data-management flags ruling 12 retired, and what each became: `(flag, engine_sub,
/// replacement, tail)`. `tail` is the one sentence that is not shared.
///
/// ⚠ **`--rm-series`' tail says NOTHING WAS DELETED in as many words, and that is the reason this
/// is a table of four columns rather than three.** An operator whose cleanup script now exits 2 must
/// not be left reading the refusal as "it may have partially run" — a delete verb's refusal owes
/// that sentence in a way a fetch's does not.
///
/// ⚠ **`engine_sub` is a COLUMN because deriving it was wrong on the one row that deletes.** It was
/// computed as `flag.trim_start_matches("--")`, which is the [`DATA_SUBS`] name for four of the
/// five rows and `rm-series` for the fifth — a subcommand [`triage_data_argv`] refuses. So the
/// refusal for the retired DELETE flag sent an engine-only operator to `backtest data rm-series`,
/// which answers "unknown `data` subcommand". A `contains` test could not see it either, because
/// `"rm-series"` contains `"rm"`; `the_retired_sub_is_a_real_data_subcommand` compares against
/// [`DATA_SUBS`] instead, which is the table that decides.
///
/// A table rather than five hand-written `if`s so [`refuse_a_retired_data_flag`] and this file's own
/// `every_retired_data_flag_names_its_replacement` iterate the same rows.
const RETIRED_DATA_FLAGS: &[(&str, &str, &str, &str)] = &[
    ("--fetch", "fetch", "vike-cli data fetch VENUE:SYMBOL:INTERVAL --days N", ""),
    ("--fetch-starter", "fetch-starter", "vike-cli data fetch-starter", ""),
    ("--seed-demo", "seed-demo", "vike-cli data seed-demo", ""),
    ("--export", "export", "vike-cli data export VENUE:SYMBOL:INTERVAL --out FILE", ""),
    (
        "--rm-series",
        "rm",
        "vike-cli data rm --kind K --venue V …",
        " NOTHING WAS DELETED — this refusal happened before a store was opened.",
    ),
];

/// Refuse one of the five retired spellings by name, echoing what was written.
///
/// `None` when argv carries none of them, so [`run`] falls through unchanged. Checked in
/// [`RETIRED_DATA_FLAGS`] order, so a line carrying two of them names the first — which is enough:
/// the message tells the operator the whole family moved.
///
/// ⚠ [`flag_given`], not `has_flag`: `--fetch=binance:BTCUSDT:1h` is a spelling `arg` accepts and
/// `has_flag` never sees, and a retirement that missed the inline form would let exactly the
/// scripted invocations through — the ones nobody is watching.
///
/// ⚠ **The adjacent-prefix landmine, checked**: `flag_given(args, "--fetch")` is NOT tripped by
/// `--fetch-starter` (`has_flag` is exact-token and `arg` is anchored on `--fetch=`), which is why
/// the two can be separate rows answering with separate replacements rather than one row that
/// misnames half the traffic. [`flag_given`]'s own doc carries the same check for `--seed`.
fn refuse_a_retired_data_flag(args: &[String]) -> Option<ExitCode> {
    let (flag, sub, replacement, tail) =
        RETIRED_DATA_FLAGS.iter().find(|(f, _, _, _)| flag_given(args, f))?;
    let inline = format!("{flag}=");
    let written = args
        .iter()
        .find(|a| a.as_str() == *flag || a.starts_with(&inline))
        .cloned()
        .unwrap_or_else(|| (*flag).to_string());
    eprintln!(
        "backtest: {flag} is retired — data management moved to `vike-cli data` (ruling 12 of \
         docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md). You wrote \
         {written:?}; write `{replacement}` instead, or `backtest {DATA_SUBCOMMAND} {sub}` on a \
         box with only this engine.{tail}"
    );
    Some(ExitCode::from(2))
}

/// Every flag `backtest data` accepts, and whether it takes a VALUE.
///
/// ⚠ It exists so [`triage_data_argv`] can refuse an unknown option and an inline value on a
/// BOOLEAN — and the second half is a live defect being closed, not tidiness.
/// `vike_analytics::binutil::has_flag` is exact-token by design and its own doc declares the
/// residual: "`--json=1`, `--yes=true` and `--dry-run=yes` are still silently ignored on every bin
/// in this family". On THIS verb that residual DELETES: `--rm-series --yes --dry-run=1` ran the
/// removal, because `--dry-run=1` is not the token `has_flag` looks for while `--yes` is. Ruling 12
/// moves the operator-facing surface onto the parser that already refuses both spellings
/// (`crates/vike-cli/src/cmd/args.rs`'s `no_value`), and this table is what gives the engine's own
/// door the same strength rather than leaving it open behind the CLI's.
const DATA_FLAGS: &[(&str, bool)] = &[
    ("--store", true),
    ("--json", false),
    ("--days", true),
    ("--from", true),
    ("--to", true),
    ("--out", true),
    ("--kind", true),
    ("--venue", true),
    ("--symbol", true),
    ("--group", true),
    ("--interval", true),
    ("--produced-by", true),
    ("--dry-run", false),
    ("--yes", false),
];

/// The `data` subcommands, and whether each takes a `VENUE:SYMBOL:INTERVAL` positional.
///
/// ⚠ **`fetch` is RETIRED and is still a row here, deliberately.** Its dispatcher arm now only
/// refuses, naming `vike-cli data fetch` — and for that refusal to be REACHED, the verb has to
/// parse like any other: [`triage_data_argv`] validates argv against this table before `run_data`
/// dispatches, so deleting the row would make `backtest data fetch binance:BTCUSDT:1h --days 180`
/// fail on its positional instead of on its retirement, which teaches the operator nothing.
/// [`RETIRED_DATA_FLAGS`]'s `--fetch` row also names `"fetch"` as its sub, and
/// `the_retired_sub_is_a_real_data_subcommand` compares the two — so both rows stay or both go.
const DATA_SUBS: &[(&str, bool)] = &[
    ("fetch", true),
    ("fetch-starter", false),
    ("seed-demo", false),
    ("export", true),
    ("rm", false),
    // ⚠ `false` for the same reason `rm` is: a series is (kind, venue, symbol-or-group, interval?)
    // and a colon-string can express neither a GROUPED series (whose symbol is empty) nor a kind.
    // `repair` needs to name one exactly — including one the store cannot ENUMERATE, which is the
    // whole point of it — so it selects with the same named flags `rm` does.
    ("repair", false),
];

/// ARGV TRIAGE for `backtest data <sub>`, returning the one positional the subcommand takes.
///
/// PURE — no store, no profile, no environment, no clock — and it runs BEFORE any arm, so a refused
/// command line opens (which means CREATES — every `DataFusionHist::open` `create_dir_all`s its
/// root) nothing. That is the rule `crates/vike-backtest/tests/optimizer_cli.rs`'s
/// `a_refused_flag_never_opens_the_store` pins, extended to this verb.
///
/// What it judges and what it deliberately does not: it judges the SHAPE of the command line — an
/// unknown option, a boolean given a value, a valued flag given none, a positional where the
/// subcommand takes none (or missing where it does). It judges no VALUE: which venues exist, which
/// kinds the store partitions by and what a window means are the arms' own, and a second roster
/// here would be a second list to keep in step. That is the split
/// `crates/vike-cli/src/cmd/data.rs`'s module doc already draws between shape and roster.
fn triage_data_argv(sub: &str, rest: &[String]) -> Result<Option<String>, String> {
    let (_, takes_spec) = DATA_SUBS.iter().find(|(name, _)| *name == sub).ok_or_else(|| {
        format!(
            "unknown `data` subcommand '{sub}' (expected {})",
            DATA_SUBS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" | ")
        )
    })?;

    let mut spec: Option<String> = None;
    let mut it = rest.iter();
    while let Some(token) = it.next() {
        if token.starts_with("--") {
            let (name, inline) = match token.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v)),
                None => (token.clone(), None),
            };
            let Some((_, valued)) = DATA_FLAGS.iter().find(|(f, _)| *f == name) else {
                return Err(format!("unknown option '{token}' on `data {sub}`"));
            };
            if !*valued {
                // ⚠ The `--dry-run=1` hole, closed. See [`DATA_FLAGS`].
                if inline.is_some() {
                    return Err(format!(
                        "{name} takes no value, and {token:?} was SILENTLY IGNORED before — which \
                         on `data rm` meant a run written as a rehearsal performed the deletion"
                    ));
                }
                continue;
            }
            if inline.is_some() {
                continue;
            }
            match it.next() {
                Some(v) if v.starts_with("--") => {
                    return Err(format!(
                        "{name} requires a value, but the next argument is another flag ({v})"
                    ));
                }
                Some(_) => continue,
                None => return Err(format!("{name} requires a value")),
            }
        }
        match &spec {
            None => spec = Some(token.clone()),
            Some(already) => {
                return Err(format!(
                    "unexpected extra argument '{token}' (the spec is already '{already}')"
                ));
            }
        }
    }

    match (*takes_spec, &spec) {
        (true, None) => Err(format!(
            "`data {sub}` needs a VENUE:SYMBOL:INTERVAL spec, e.g. `backtest data {sub} \
             binance:BTCUSDT:1h …`"
        )),
        (false, Some(extra)) => {
            Err(format!("'{extra}': `data {sub}` takes no VENUE:SYMBOL:INTERVAL spec"))
        }
        _ => Ok(spec),
    }
}

/// Route `backtest data <sub>`. `rest` is everything after the `data` word.
///
/// Each arm is the SAME function the retired flag called, handed the sub-slice — so this is a
/// rename of the door, never a second implementation. The `venue-fetch` refusals move with their
/// arms and name the new spelling.
fn run_data(
    vars: &std::collections::HashMap<String, String>,
    rest: &[String],
    now_unix_secs: &dyn Fn() -> i64,
) -> ExitCode {
    if rest.first().is_none_or(|a| a == "-h" || a == "--help" || a == "help") {
        // stdout + exit 0, the rule `--help` already obeys above: help is output a user pipes into
        // a pager, not a diagnostic. A bare `backtest data` is the same question with no verb.
        println!("{DATA_USAGE}");
        return ExitCode::SUCCESS;
    }
    let sub = rest[0].clone();
    let args = &rest[1..];
    let spec = match triage_data_argv(&sub, args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest data: {e}\n\n{DATA_USAGE}");
            return ExitCode::from(2);
        }
    };
    let _ = (&spec, now_unix_secs);

    match sub.as_str() {
        "seed-demo" => run_seed_demo(vars, args),
        "rm" => run_rm_series(vars, args),
        "repair" => run_repair_series(vars, args),
        // ⚠ **RETIRED, and answered BY NAME rather than falling through to the catch-all.** This
        // arm used to call `run_fetch`, which called `crate::fetch::fetch_into`, which called
        // `vike_binance::data::fetch_klines_range` — the one venue call in the compute plane. It is
        // deleted: history is fetched by the DATA plane, once, into the store.
        //
        // The refusal is spelled out rather than left to `other =>`'s "unknown `data` subcommand"
        // for the same reason [`RETIRED_DATA_FLAGS`] exists: this is a verb operators have typed,
        // it is installed on a box where `vike-backtest.service` runs, and a bare "unknown"
        // teaches nothing. ⚠ The replacement is NOT a drop-in — it needs a reachable datahub,
        // which the direct call did not — so the message says so rather than implying a rename.
        "fetch" => {
            eprintln!(
                "backtest data fetch: RETIRED — this binary no longer reaches a venue at all.\n\
                 \x20 use: vike-cli data fetch VENUE:SYMBOL:INTERVAL --days N [--addr HOST:PORT]\n\
                 \n\
                 That asks a DATAHUB to fetch the window into the store, which is where a fetch \
                 belongs: the datahub collects SIX venues where this path reached one, and it \
                 drops a still-forming candle, which this path never did.\n\
                 \x20 ⚠ It needs a reachable datahub (default 127.0.0.1:7878). This command needed \
                 no server, so that is a real precondition and not a renamed flag.\n\
                 \x20 With no datahub and no network: `backtest data fetch-starter` downloads the \
                 published dataset, and `backtest data seed-demo` needs neither."
            );
            ExitCode::from(2)
        }
        "fetch-starter" => {
            #[cfg(feature = "venue-fetch")]
            {
                run_fetch_starter(vars, args)
            }
            #[cfg(not(feature = "venue-fetch"))]
            {
                eprintln!(
                    "backtest data fetch-starter: this build has no network fetch — it was \
                     compiled without the `venue-fetch` feature. The shipped release binary and \
                     the container image both have it. `backtest data seed-demo` needs no network \
                     and works in every build."
                );
                ExitCode::from(2)
            }
        }
        // ⚠ **UNGATED now, and it should never have been gated on `venue-fetch`.** This arm carried
        // a `#[cfg(feature = "venue-fetch")]` and a refusal blaming that feature for "carrying the
        // Parquet writer's caller". Measured, that was simply false: `export` reaches no network at
        // all — it reads the local store and writes a file — and
        // `DataFusionHist::export_bars_parquet` is an inherent method with no `#[cfg]` of its own
        // beyond `hist-datafusion`, which `datafusion-store` already forwards.
        //
        // There is no `#[cfg(feature = "datafusion-store")]` in its place either, because that
        // would be a tautology: `crates/vike-backtest/src/lib.rs` compiles this whole module only
        // under that feature. A build that can reach this line can already export.
        "export" => run_export(vars, args, spec.as_deref().unwrap_or_default()),
        // Unreachable: [`triage_data_argv`] refused every other spelling above. Spelled as a
        // refusal rather than `unreachable!()` so a new row in [`DATA_SUBS`] with no arm here is a
        // message instead of a panic in a binary an operator is running against their own store.
        other => {
            eprintln!("backtest data: '{other}' has no arm in this build\n\n{DATA_USAGE}");
            ExitCode::from(2)
        }
    }
}

/// `data seed-demo` — the SYNTHETIC tape, and the answer to an empty store that needs no network.
///
/// ⚠ THE EMPTY-STORE ANSWER, and it needs no profile and no strategy registry, only a store root.
///
/// A clean install has an empty hist store, so the shipped example profile — which names a slice —
/// reports a run with no trades, and nothing distinguishes that from a strategy that never fired.
/// `vike_data::demo` writes a tape the shipped profile already names;
/// `crates/vike-cli/tests/demo_tape_profile.rs` holds the two in agreement, so this writes not
/// "some data" but THE data the next command reads.
///
/// Deliberately on this binary rather than in `vike-cli`: writing a store needs `DataFusionHist`,
/// and vike-cli is DataFusion-free BY CONSTRUCTION (the `light-consumers` CI lane asserts it). The
/// tool that CONSUMES hist data is the honest place for the command that creates some — which is
/// why ruling 12 moved the SPELLING and not this function.
fn run_seed_demo(vars: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    let seeded = match demo_tape::seed(&store) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: seeding {} failed: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    // The provenance line FIRST, because a synthetic tape that is not announced as one is the
    // hazard this whole feature carries: a result computed on invented prices reads exactly like a
    // result computed on real ones.
    println!(
        "seeded SYNTHETIC demo bars (venue `{}` — a closed-form curve, NOT market data) into {}",
        demo_tape::DEMO_VENUE,
        root.display()
    );
    let mut written = 0usize;
    for done in &seeded {
        written += done.rows;
        println!(
            "  {}/{} {}  {} bars{}",
            demo_tape::DEMO_VENUE,
            done.slice.symbol,
            done.slice.interval,
            done.slice.len(),
            if done.rows == 0 { "  (already present — nothing written)" } else { "" }
        );
    }
    if written == 0 {
        println!("this store already held the demo tape; nothing was written.");
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod data_subcommand_tests {
    use super::*;

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    /// Every retired flag names a live replacement, in both written spellings, and refuses. The
    /// TABLE is what is iterated, so a sixth retirement joins by adding a row.
    #[test]
    fn every_retired_data_flag_names_its_replacement() {
        for (flag, _, replacement, _) in RETIRED_DATA_FLAGS {
            assert!(
                replacement.starts_with("vike-cli data "),
                "{flag} must name the verb the ruling moved it to, got {replacement:?}"
            );
            for written in [(*flag).to_string(), format!("{flag}=x")] {
                assert!(
                    refuse_a_retired_data_flag(std::slice::from_ref(&written)).is_some(),
                    "{written} must be refused, not read as absent"
                );
            }
        }
        assert!(
            refuse_a_retired_data_flag(&argv(&["--json"])).is_none(),
            "an unrelated flag falls through"
        );
    }

    /// ⚠ The adjacent-prefix pair, pinned: `--fetch-starter` must refuse under its OWN row, naming
    /// its own replacement, rather than under `--fetch`'s. `has_flag` is exact-token and `arg` is
    /// anchored on `--fetch=`, so this holds — and it is the property that would break first if
    /// either helper were loosened.
    #[test]
    fn fetch_starter_is_not_swallowed_by_the_fetch_row() {
        let args = argv(&["--fetch-starter"]);
        let hit = RETIRED_DATA_FLAGS.iter().find(|(f, _, _, _)| flag_given(&args, f));
        assert_eq!(hit.map(|(f, _, _, _)| *f), Some("--fetch-starter"));
    }

    /// ⚠ **The engine subcommand a retirement names must be one [`DATA_SUBS`] actually serves.**
    ///
    /// It was DERIVED (`flag.trim_start_matches("--")`), which is right for four rows and wrong for
    /// the fifth: `--rm-series` yielded `rm-series`, so the refusal for the one retired flag that
    /// DELETES told an engine-only operator to run `backtest data rm-series`, which
    /// [`triage_data_argv`] answers with "unknown `data` subcommand". The end-to-end test could not
    /// catch it — it asserts the message CONTAINS the sub, and `"rm-series"` contains `"rm"` — so
    /// the check has to be against the table that decides rather than against the message.
    #[test]
    fn the_retired_sub_is_a_real_data_subcommand() {
        for (flag, sub, _, _) in RETIRED_DATA_FLAGS {
            assert!(
                DATA_SUBS.iter().any(|(name, _)| name == sub),
                "{flag} names `backtest data {sub}`, which is not a subcommand this binary serves \
                 ({:?})",
                DATA_SUBS.iter().map(|(n, _)| *n).collect::<Vec<_>>()
            );
        }
    }

    /// The triage refuses SHAPE and nothing else: an unknown option, a boolean given a value, a
    /// valued flag given none, and the positional's presence-or-absence per subcommand.
    #[test]
    fn the_triage_refuses_shape_and_defers_every_value() {
        assert_eq!(
            triage_data_argv("fetch", &argv(&["binance:BTCUSDT:1h", "--days", "180"])).unwrap(),
            Some("binance:BTCUSDT:1h".to_string())
        );
        assert_eq!(triage_data_argv("seed-demo", &argv(&[])).unwrap(), None);
        // The value is not judged here — a nonsense venue and a nonsense window both pass, and the
        // arm's own error names what it could not do.
        assert_eq!(
            triage_data_argv("fetch", &argv(&["nope:NOPE:99z", "--days", "-4"])).unwrap(),
            Some("nope:NOPE:99z".to_string())
        );

        assert!(
            triage_data_argv("nope", &argv(&[])).unwrap_err().contains("unknown `data` subcommand")
        );
        assert!(
            triage_data_argv("rm", &argv(&["--bogus"])).unwrap_err().contains("unknown option")
        );
        assert!(
            triage_data_argv("rm", &argv(&["--kind"])).unwrap_err().contains("requires a value")
        );
        assert!(
            triage_data_argv("rm", &argv(&["--kind", "--venue"]))
                .unwrap_err()
                .contains("another flag")
        );
        assert!(
            triage_data_argv("export", &argv(&["--out", "x"])).unwrap_err().contains("needs a")
        );
        assert!(triage_data_argv("seed-demo", &argv(&["x:y:z"])).unwrap_err().contains("takes no"));
    }

    /// ⚠ **The `--dry-run=1` hole, and it is the reason this triage exists at all.** `has_flag` is
    /// exact-token, so `--dry-run=1` was silently ignored while `--yes` beside it was honoured —
    /// a command line written as a rehearsal performed the deletion. It is a REFUSAL now, and the
    /// message says what it used to do.
    #[test]
    fn a_boolean_given_a_value_is_refused_rather_than_ignored() {
        for spelling in ["--dry-run=1", "--yes=true", "--json=1"] {
            let err = triage_data_argv("rm", &argv(&["--kind", "bar", "--venue", "x", spelling]))
                .unwrap_err();
            assert!(err.contains("takes no value"), "{spelling}: {err}");
        }
        let err = triage_data_argv(
            "rm",
            &argv(&["--kind", "bar", "--venue", "x", "--dry-run=1", "--yes"]),
        )
        .unwrap_err();
        assert!(err.contains("rehearsal"), "the message says what it used to do: {err}");
    }
}

/// `data fetch-starter` — the published dataset, for a box that cannot reach a venue.
///
/// ⚠ This doc said "see [`crate::starter`] for why the same feature covers BOTH" — both being this
/// and `data fetch`. There is no both any more: `data fetch` and the `crate::fetch` module it
/// called are DELETED, because a compute-plane binary reaching an exchange directly is the thing
/// that change removed. This verb never touched a venue — it is a plain HTTPS GET of a prepared
/// dataset — and it is now the only thing `venue-fetch` gates that fetches anything at all.
/// [`crate::starter`] still carries what the download does and does not verify.
#[cfg(feature = "venue-fetch")]
fn run_fetch_starter(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
) -> ExitCode {
    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    // ⚠ `<project>/tmp`, NEVER the operating system's temp directory — production scratch must land
    // where the project folder is, and `crates/vike-ops/tests/system_temp_gate.rs` refuses
    // otherwise. Two reasons it gives, both about a box that is not this one: inside the container
    // the system temp is not the host's and does not survive a restart, so the same path resolves
    // somewhere else on an operator's machine, silently; and the system temp is emptied by
    // something we do not control, while a leak in it is invisible until a filesystem fills (26,851
    // leaked scratch directories, 215 GB, measured on the build box).
    //
    // `ScratchDir` owns what it creates and removes it on every path out of here, error ones
    // included — a download that leaks a Parquet file per attempt is the same leak wearing our name.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let tmp_root = vike_model::state_path::project_tmp_dir_from(
        vars.get("VIKE_SETTINGS_DIR").map(String::as_str),
        &cwd,
    )
    // No project above the working directory — a bare checkout, or a binary run from elsewhere.
    // The store's own parent is then the honest fallback: it is where this command is writing
    // anyway, so a file that briefly appears beside it cannot surprise anyone.
    .unwrap_or_else(|| root.parent().unwrap_or(&root).join("tmp"));
    if let Err(e) = std::fs::create_dir_all(&tmp_root) {
        eprintln!("backtest: cannot create {}: {e}", tmp_root.display());
        return ExitCode::from(2);
    }
    let scratch = match vike_model::scratch::ScratchDir::create_in(&tmp_root, "starter") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot create a scratch directory in {}: {e}", tmp_root.display());
            return ExitCode::from(2);
        }
    };
    println!("downloading the starter dataset into {} …", root.display());
    match starter::fetch_into(&store, scratch.path(), |m| println!("  {m}")) {
        Ok(done) => {
            let mut rows = 0usize;
            for d in &done {
                rows += d.rows;
                println!(
                    "  {} — {} bytes, sha256 {}, {} rows{}",
                    d.file,
                    d.bytes,
                    d.sha256,
                    d.rows,
                    if d.rows == 0 { "  (already present)" } else { "" }
                );
            }
            if rows == 0 {
                println!("this store already held the starter dataset; nothing was written.");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("backtest: {e}");
            ExitCode::from(2)
        }
    }
}

/// `data export` — one series out of the store as a standalone Parquet file.
///
/// The producer half of the starter dataset (`scripts/publish_starter_data.sh` calls exactly this),
/// and useful on its own: handing somebody a slice without handing them the store's internals.
///
/// ⚠ Gated on `venue-fetch` until 2026-09-19, which was wrong in both directions: this function
/// reaches no venue and no network, and the feature it named is about reaching one. See the
/// `"export"` arm in [`run_data`] for the measurement and for why nothing gates it now.
fn run_export(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    raw_spec: &str,
) -> ExitCode {
    let Some(out) = arg(args, "--out") else {
        eprintln!("backtest: --export needs --out FILE");
        return ExitCode::from(2);
    };
    // The same VENUE:SYMBOL:INTERVAL grammar `--fetch` takes, but WITHOUT its venue roster: this
    // reads the store, so any venue the store holds is exportable, including `demo`.
    let parts: Vec<&str> = raw_spec.split(':').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        eprintln!("backtest: --export takes VENUE:SYMBOL:INTERVAL, not {raw_spec:?}");
        return ExitCode::from(2);
    }
    let parse_ts = |s: &str| crate::harness::profile::parse_ts(s).map_err(|e| e.to_string());
    let range = match (arg(args, "--from"), arg(args, "--to")) {
        (None, None) => vike_data::hist::TsRange::all(),
        (from, to) => {
            let conv = |v: Option<String>| -> Result<Option<i64>, String> {
                v.map(|s| parse_ts(&s)).transpose()
            };
            match (conv(from), conv(to)) {
                (Ok(start), Ok(end)) => vike_data::hist::TsRange { start, end },
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("backtest: {e}");
                    return ExitCode::from(2);
                }
            }
        }
    };

    let root = store_root(arg(args, "--store").map(PathBuf::from), vars);
    let store = match DataFusionHist::open(&root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", root.display());
            return ExitCode::from(2);
        }
    };
    match store.export_bars_parquet(Path::new(&out), parts[0], parts[1], parts[2], range) {
        Ok(rows) => {
            // The row count is the load-bearing half of this line: a publishing script that
            // exported an EMPTY slice would otherwise upload a valid file nobody can use.
            println!("exported {rows} bars of {}/{} {} to {out}", parts[0], parts[1], parts[2]);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("backtest: exporting {raw_spec} failed: {e}");
            ExitCode::from(2)
        }
    }
}

// ─── `data rm`: emptying the store ─────────────────────────────────────────────────────────────

/// `backtest data rm` — DELETE stored series, irreversibly.
///
/// # Why this verb is on the ENGINE
///
/// For `data export`'s reason, one step further: emptying a store needs `DataFusionHist`, and
/// `vike-cli` is DataFusion-FREE by construction (its manifest argues every edge; CI's
/// `light-consumers` lane asserts it). `vike-cli data rm` is a route to this arm, exactly as
/// `vike-cli data fetch` is a route to `data fetch`. It is a first-class verb here rather than
/// only a spawn target because an operator ON the box — which is where a cleanup happens, and
/// where the 2026-09-07 the CI box cleanup DID happen — should not need a datahub to empty their own
/// store.
///
/// # ⚠ The plan LEADS with the store, and it leads on STDOUT
///
/// `binutil::store_root` already logs the resolved root and the rung that chose it, through
/// `tracing::info!` — which `RUST_LOG` silences. "Which store" is the question a destructive verb
/// must answer before "which series", and a store does not MERGE: a resolution that moved is
/// invisible until it has destroyed the wrong tree. So the first line of every run, dry or not,
/// carries `StoreRoot`'s `Display` — the path and the sentence saying why it is that path.
///
/// # The confirmation
///
/// `--yes`, or a TERMINAL on which the operator types `delete N series` for the N this plan
/// matched. Binding the confirmation to a fact of the plan is what makes a line copied from a
/// previous run against a different plan fail to match, with no token and no state.
///
/// ⚠ **No `--yes` and no terminal is a REFUSAL, never a read.** `yes | backtest data rm …` is
/// the exact failure this exists to prevent, and a pipe is indistinguishable from a person once you
/// have decided to read one.
fn run_rm_series(vars: &std::collections::HashMap<String, String>, args: &[String]) -> ExitCode {
    use std::io::IsTerminal;
    use vike_data::removal::SeriesSelector;

    // ⚠ FIRST, before the store is opened: the root AND the rung that chose it, on stdout.
    let resolved =
        crate::binutil::store_root_resolved(arg(args, "--store").map(PathBuf::from), vars);
    let json = has_flag(args, "--json");
    // The store LEADS, on stdout for a human and on stderr under `--json` (where stdout is the
    // document and the same fact rides `store_root`/`store_rung` inside it).
    if json {
        eprintln!("store: {resolved}");
    } else {
        println!("store: {resolved}");
    }

    let selector = SeriesSelector {
        kind: arg(args, "--kind").unwrap_or_default(),
        venue: arg(args, "--venue").unwrap_or_default(),
        symbol: arg(args, "--symbol"),
        group: arg(args, "--group"),
        interval: arg(args, "--interval"),
    };
    if let Err(e) = selector.validate_shape() {
        eprintln!("backtest data rm: {e}\n\n{DATA_USAGE}");
        return ExitCode::from(2);
    }
    // Resolved BEFORE the store is opened: a `--produced-by` spelling is a fact about the argument,
    // and refusing a typo without touching a store is the cheaper failure.
    let produced_by =
        match arg(args, "--produced-by").map(|s| vike_data::store_kind::resolve_produced_by(&s)) {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => {
                eprintln!("backtest data rm: {e}");
                return ExitCode::from(2);
            }
            None => None,
        };
    // ⚠ The SWEEP rule, and it is the engine's as much as the CLI's: this arm is reachable directly.
    if selector.is_sweep() && produced_by.is_none() {
        eprintln!(
            "backtest data rm: `{}` matches more than one series, so --produced-by is \
             REQUIRED. Deleting a whole sweep by name alone is what that assertion exists to \
             replace; name every dimension instead, or pass the commit-key prefix the rows carry.",
            selector.describe()
        );
        return ExitCode::from(2);
    }

    let store = match DataFusionHist::open(&resolved.root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("backtest: cannot open the hist store at {}: {e}", resolved.root.display());
            return ExitCode::from(2);
        }
    };
    let plan = match vike_data::removal::plan_removal(&store, &selector, produced_by.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("backtest data rm: {e}");
            return ExitCode::from(2);
        }
    };

    let dry_run = has_flag(args, "--dry-run");
    // ⚠ Under `--json` the plan goes to STDERR, not nowhere. Stdout is the document and nothing
    // else, but a run that is about to ask a human to type `delete N series` must have SHOWN them
    // what N is made of — and without this the operator was prompted having seen no plan at all.
    // Same stream every diagnostic in this workspace uses, and the same shape
    // `crates/vike-cli/src/cmd/engine.rs`'s `run_capturing_stdout` already gives the engine's own
    // lines.
    for line in plan.lines() {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    // A provenance refusal is reported and deletes nothing, whether or not this was a dry run.
    if let Err(refusals) = plan.verdict() {
        if json {
            println!("{}", rm_series_json(&resolved, &plan, None, Some(&refusals)));
        }
        eprintln!("backtest data rm: provenance REFUSED — nothing was deleted");
        return ExitCode::from(2);
    }
    // ⚠ `--dry-run` WINS over `--yes`: a rehearsal must not require stripping a flag.
    if dry_run {
        if json {
            println!("{}", rm_series_json(&resolved, &plan, None, None));
        } else {
            println!("--dry-run: nothing was deleted");
        }
        return ExitCode::SUCCESS;
    }
    // "Nothing matched" is a SUCCESS on the same rung as a delete: `delete_series` is idempotent,
    // and a cleanup that fails on re-run is a cleanup nobody re-runs.
    if plan.matched() == 0 {
        if json {
            println!("{}", rm_series_json(&resolved, &plan, Some(&Default::default()), None));
        }
        return ExitCode::SUCCESS;
    }
    if let Err(e) = confirm_removal(&plan, has_flag(args, "--yes"), std::io::stdin().is_terminal())
    {
        eprintln!("backtest data rm: {e}");
        return ExitCode::from(2);
    }

    match vike_data::removal::execute_removal(&store, &plan) {
        Ok(outcome) => {
            if json {
                println!("{}", rm_series_json(&resolved, &plan, Some(&outcome), None));
            } else {
                for id in &outcome.deleted {
                    println!("deleted {}", vike_data::removal::describe_id(id));
                }
                for (id, why) in &outcome.failed {
                    eprintln!("FAILED {}: {why}", vike_data::removal::describe_id(id));
                }
                println!(
                    "{} of {} series deleted",
                    outcome.deleted.len(),
                    outcome.deleted.len() + outcome.failed.len()
                );
            }
            // One broken series is one SKIPPED series (`run_maintenance`'s rule) — reported, the
            // rest continue, and the exit is non-zero so a wrapper knows to look.
            if outcome.is_clean() { ExitCode::SUCCESS } else { ExitCode::from(2) }
        }
        Err(e) => {
            eprintln!("backtest data rm: {e}");
            ExitCode::from(2)
        }
    }
}

/// The confirmation gate — PURE over its two inputs, so both branches are unit-tested rather than
/// only reachable from a terminal.
///
/// `stdin_is_terminal` is a PARAMETER for that reason; the caller reads the real one.
fn confirm_removal(
    plan: &vike_data::removal::RemovalPlan,
    yes: bool,
    stdin_is_terminal: bool,
) -> Result<(), String> {
    if yes {
        return Ok(());
    }
    if !stdin_is_terminal {
        return Err(format!(
            "refusing to delete {} series without --yes: stdin is not a terminal, so there is \
             nobody to confirm. A confirmation read from a PIPE is not a confirmation — \
             `yes | backtest data rm …` is exactly what this refuses.",
            plan.matched()
        ));
    }
    let want = format!("delete {} series", plan.matched());
    eprintln!("type `{want}` to confirm, or anything else to abort:");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| format!("reading the confirmation: {e}"))?;
    if line.trim() == want {
        Ok(())
    } else {
        Err(format!("not confirmed (expected `{want}`) — nothing was deleted"))
    }
}

/// The `--json` document for `data rm`.
///
/// ⚠ It carries the RESOLVED store root and its RUNG, and that is the one fact that cannot survive
/// a prose round trip: `vike-cli data rm --json` wraps this document rather than re-deriving the
/// root, because the CLI resolves nothing (the engine runs in another process) and a path it
/// guessed at would be the confident-sounding wrong answer.
fn rm_series_json(
    resolved: &vike_model::store_path::StoreRoot,
    plan: &vike_data::removal::RemovalPlan,
    outcome: Option<&vike_data::removal::RemovalOutcome>,
    refusals: Option<&[String]>,
) -> String {
    let doc = serde_json::json!({
        "store_root": resolved.root.display().to_string(),
        "store_rung": resolved.rung.as_str(),
        "store_rung_why": resolved.rung.why(),
        "selector": plan.selector,
        "produced_by": plan.produced_by,
        "matched": plan.matched(),
        "rows": plan.rows(),
        "bytes": plan.bytes(),
        "series": plan.series,
        // `null` for a dry run and for a refusal — the two cases where nothing was attempted.
        "outcome": outcome,
        // `null` when the assertion held; the per-series refusals otherwise.
        "refused": refusals,
    });
    serde_json::to_string_pretty(&doc).expect("a tree of plain data; serialization is total")
}

// ─── `data repair`: giving the manifest rebuild an operator ────────────────────────────────────

/// `backtest data repair` — rebuild ONE series' manifest from its parts.
///
/// # Why this verb exists at all
///
/// `crates/vike-data/src/datafusion_hist.rs`'s `DataFusionHist::rebuild_series_manifest` has been
/// the repair for a lost or unreadable index since the manifest became a cache rather than ground
/// truth, and until this arm **no binary called it** — `git grep` found tests and doc comments and
/// nothing else. `crates/vike-data/src/datafusion_hist/manifest.rs`'s `read_manifest` names it in
/// the error an operator actually reads, so the failure message prescribed a cure no command
/// dispensed. The only move an operator had was deleting `_manifest.delta` by hand, which throws
/// away every commit since the last fold.
///
/// # ⚠ REHEARSAL IS THE DEFAULT, and that is not the same rule `rm` has
///
/// `data rm` REFUSES without `--yes`, because somebody typing `rm` means to delete and the danger
/// is doing it unseen. `data repair` PRINTS THE PLAN AND EXITS 0 without `--yes`, because the
/// danger here is the opposite one: a rebuild can succeed and still cost the series its idempotency
/// log, so the fact an operator needs is the VERDICT, and the verdict is only knowable by running
/// the same pass the write runs. The rehearsal IS that pass, lock-free and writing nothing, so it
/// costs a live writer exactly nothing. `--dry-run` spells the default explicitly and WINS over
/// `--yes`, which is `rm`'s rule unchanged: a rehearsal must never require stripping a flag.
///
/// ⚠ A rehearsal that exits 0 having written nothing is the one place this arm could hand somebody
/// a false green, so it says `NOTHING WAS WRITTEN` in as many words and the `--json` document
/// carries a `written` field that is `null` rather than `true`.
///
/// # ⚠ ONE series, named exactly — there is deliberately no `--all`
///
/// Three reasons, and the first alone settles it:
///
/// 1. **The headline failure is invisible to enumeration.** `DataFusionHist::list_series` finds
///    leaves by the presence of `_manifest.json`, so a series whose base was deleted — precisely
///    what `read_manifest` refuses and this verb repairs — is in no `list_series`, no `inventory()`
///    and no `vike-cli data list`. An `--all` built on enumeration would answer "0 series
///    repaired" on the exact state it exists for.
/// 2. **The lock hold is per-series and unbounded.** A rebuild reads every part footer with that
///    series' lock HELD; `--all` turns one bounded critical section into N of them with no operator
///    in front of any.
/// 3. **The verdict is per-series.** `parts_without_keys` and `parts_unreadable` are decisions an
///    operator takes one series at a time, and an `--all` would fold N of them into one exit code.
///
/// So a multi-series repair is N invocations, each with its own rehearsal — the same answer
/// `vike_data::removal`'s `SeriesSelector` gives for a cleanup spanning venues.
///
/// # ⚠ `open_read_only`, never `open`
///
/// `DataFusionHist::open` performs two writes: it creates the root (a typo'd `--store` becomes a
/// fresh empty store) and it runs the WAL recovery sweep, which takes each affected series' lock.
/// Both are wrong here, and the second is disqualifying: `recover()` calls `read_manifest` and
/// propagates its error out of `open`, so a series with BOTH a base-less delta log and a
/// `_wal.arrow` makes `open` fail for the whole store — on the very error whose repair needs a
/// handle. `open_read_only` creates nothing and recovers nothing, and still has every `append_*`
/// verb, which is the combination this verb wants.
fn run_repair_series(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
) -> ExitCode {
    use vike_data::removal::SeriesSelector;

    // ⚠ FIRST, before the store is touched: the root AND the rung that chose it — `run_rm_series`'
    // rule, and for its reason. A store does not merge, so "which store" is the question a
    // write-shaped verb answers before "which series".
    let resolved =
        crate::binutil::store_root_resolved(arg(args, "--store").map(PathBuf::from), vars);
    let json = has_flag(args, "--json");
    if json {
        eprintln!("store: {resolved}");
    } else {
        println!("store: {resolved}");
    }

    let selector = SeriesSelector {
        kind: arg(args, "--kind").unwrap_or_default(),
        venue: arg(args, "--venue").unwrap_or_default(),
        symbol: arg(args, "--symbol"),
        group: arg(args, "--group"),
        interval: arg(args, "--interval"),
    };
    // The same two-stage validation `data rm` runs, and the same split: shape first (facts about
    // the command line), then the layout table (facts about what this store partitions by).
    if let Err(e) = selector.validate_shape() {
        eprintln!("backtest data repair: {e}\n\n{DATA_USAGE}");
        return ExitCode::from(2);
    }
    if let Err(e) = selector.validate_against_kinds() {
        eprintln!("backtest data repair: {e}");
        return ExitCode::from(2);
    }
    // ⚠ A WILDCARD is refused rather than expanded — see this function's doc for why there is no
    // `--all`. `is_sweep` is the SELECTOR's own answer and is kind-aware (`--symbol X` fully names
    // a tick series and wildcards every interval of a bar one), so this refusal cannot ask for an
    // `--interval` on a kind that has no `interval=` segment.
    if selector.is_sweep() {
        eprintln!(
            "backtest data repair: `{}` names more than one series, and a repair names exactly \
             one. A rebuild holds that series' lock across every part footer it reads, and its \
             verdict — what came back and what did not — is per-series, so a wildcard would fold N \
             unbounded critical sections and N verdicts into one exit code. Name the series: \
             --symbol S (plus --interval I on `bar`) or --group G. For several, run this verb \
             several times.",
            selector.describe()
        );
        return ExitCode::from(2);
    }
    let id = match (&selector.symbol, &selector.group) {
        (_, Some(group)) => vike_data::SeriesId::grouped(&selector.kind, &selector.venue, group),
        (Some(symbol), None) => vike_data::SeriesId::per_symbol(
            &selector.kind,
            &selector.venue,
            symbol,
            selector.interval.clone(),
        ),
        // Unreachable: `is_sweep` is true whenever neither is named. Spelled as a refusal rather
        // than `unreachable!()` so a future loosening of that predicate is a message instead of a
        // panic in a binary an operator is running against their own store.
        (None, None) => {
            eprintln!("backtest data repair: neither --symbol nor --group named a series");
            return ExitCode::from(2);
        }
    };

    // ⚠ `open_read_only`, not `open` — see this function's doc. Its absent-root error is also the
    // right answer for a typo'd `--store`: a REPAIR that invented an empty store and then reported
    // nothing to repair would be the confident wrong answer.
    let store = match DataFusionHist::open_read_only(&resolved.root) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "backtest data repair: cannot open the hist store at {}: {e}",
                resolved.root.display()
            );
            return ExitCode::from(2);
        }
    };
    let plan = match store.plan_series_manifest_rebuild(&id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("backtest data repair: {e}");
            return ExitCode::from(2);
        }
    };
    // ⚠ The leaf must EXIST. `rebuild_manifest` reads an absent directory as an empty series, and
    // `SeriesLock::acquire` would create it — so a mistyped selector reaching the write publishes
    // an empty manifest at a path nothing ever wrote, minting a phantom series `list_series` then
    // enumerates forever. Refused here, before the plan is even printed.
    if !plan.leaf_present {
        eprintln!(
            "backtest data repair: no series directory at {} — there is nothing here to rebuild \
             FROM. Check the selector; note that a series whose base manifest was deleted is \
             missing from `vike-cli data list` while its DIRECTORY is still on disk, so an ABSENT \
             directory means the selector is wrong rather than the series being the broken one.",
            plan.series_dir
        );
        return ExitCode::from(2);
    }

    let dry_run = has_flag(args, "--dry-run");
    let write = repair_writes(has_flag(args, "--yes"), dry_run);
    // The plan is SHOWN either way, and under `--json` it goes to STDERR rather than nowhere —
    // `run_rm_series`' rule: stdout is the document and nothing else, but a run about to rebuild an
    // index must have shown the operator what it would cost.
    for line in plan.lines() {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    if !write {
        if json {
            println!("{}", repair_json(&resolved, &plan, None, None));
        } else {
            // ⚠ ONE sentence for BOTH spellings, unlike `run_rm_series`' `--dry-run:` prefix — and
            // the difference is that there a rehearsal is the exception while here it is the
            // DEFAULT. `vike-cli data repair` forwards `--dry-run` for a line that carried neither
            // flag (so the child is told what that side decided rather than inheriting a default
            // spelled twice), which means a `--dry-run:`-prefixed message would quote back a flag
            // the operator never typed.
            println!(
                "NOTHING WAS WRITTEN — this was a rehearsal. Re-run with --yes to perform it."
            );
        }
        return ExitCode::SUCCESS;
    }

    match store.rebuild_series_manifest_if_uncontended(&id) {
        // ⚠ THE LIVE-WRITER DECISION, and it is a refusal rather than a wait. See
        // `rebuild_series_manifest_if_uncontended`'s doc for why WINNING a contended lock is the
        // outcome to avoid: the rebuild's critical section is every part footer in the series, and
        // a `RecorderSink` whose flush spins that budget out DISCARDS its buffer.
        Ok(None) => {
            if json {
                println!("{}", repair_json(&resolved, &plan, None, Some(REPAIR_CONTENDED)));
            }
            eprintln!(
                "backtest data repair: {REPAIR_CONTENDED} Remedy: stop the writer on this store \
                 (the recorder daemon, a `vike-backend datahub --record`, or a backfill) and \
                 re-run — or re-run now if what you collided with was a passing compaction. ⚠ The \
                 reverse does NOT hold: a lock this verb DOES get is not proof the series is idle, \
                 because a recorder holds it only while committing."
            );
            ExitCode::from(2)
        }
        Ok(Some(report)) => {
            // The verdict is computed from the report the WRITE returned, over the plan's own
            // `orphan_commits` — the one loss no `RebuildReport` field can carry, because a rebuild
            // derives keys from part footers and an orphan is a key no part has.
            let done = vike_data::datafusion_hist::RepairPlan { report, ..plan.clone() };
            if json {
                println!("{}", repair_json(&resolved, &done, Some(true), None));
            } else {
                for line in done.outcome_lines() {
                    println!("{line}");
                }
            }
            // ⚠ **A LOSSY SUCCESS IS NOT A CLEAN EXIT.** The index is rebuilt and the rows are
            // readable, so this is not a failure — but exiting 0 over a store that just lost its
            // idempotency log is what sends an operator straight into a backfill that duplicates
            // rows. The exit code is the only thing a wrapper reads, so it is what says so.
            // `run_rm_series`' partial-failure rung, for the same reason.
            if done.is_lossless() {
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "backtest data repair: the rebuild SUCCEEDED and was LOSSY — see the LOSSY \
                     lines above. The index is rebuilt and the rows read again; what did NOT come \
                     back is named there, with what to do about it."
                );
                ExitCode::from(2)
            }
        }
        Err(e) => {
            // ⚠ **An `Err` here is NOT "nothing happened".** The publish and the delta-log clear
            // are two steps (`manifest::fold_base`), so a failure in the second returns `Err` with
            // a new base already durable on disk. Saying so is the difference between an operator
            // re-running a rehearsal (right) and assuming the store is untouched (wrong).
            eprintln!(
                "backtest data repair: {e}\n⚠ this is NOT necessarily 'nothing happened': the \
                 rebuild publishes a new base and THEN clears the delta log, so a failure in the \
                 second step leaves the new base in place. Re-run this verb WITHOUT --yes to see \
                 the store's current state before deciding anything."
            );
            ExitCode::from(2)
        }
    }
}

/// The contention refusal's first sentence, spelled once because two surfaces carry it — stderr for
/// a human, and the `--json` document's `refused` field for a wrapper that must branch on it.
const REPAIR_CONTENDED: &str = "another writer holds this series' lock, so the rebuild was NOT \
                                attempted and NOTHING WAS WRITTEN.";

/// Does this `data repair` line WRITE? PURE over its two flags, so both branches are unit-tested
/// rather than only reachable through a store.
///
/// ⚠ `--dry-run` WINS over `--yes`, which is `run_rm_series`' rule unchanged: a rehearsal must not
/// require stripping a flag, because the line an operator re-runs is the line already in their
/// shell history.
fn repair_writes(yes: bool, dry_run: bool) -> bool {
    yes && !dry_run
}

/// The `--json` document for `data repair`.
///
/// ⚠ It carries the RESOLVED store root and its RUNG for `rm_series_json`'s reason: `vike-cli data
/// repair --json` wraps this document rather than re-deriving the root, because that binary
/// resolves nothing (the engine runs in another process) and a path it guessed at would be the
/// confident-sounding wrong answer.
///
/// `written` is `null` for a rehearsal and for the contention refusal — the two cases where nothing
/// was attempted — and `true` for a performed rebuild. `lossless` is what a wrapper branches on,
/// and it is the same verdict the non-zero exit encodes.
fn repair_json(
    resolved: &vike_model::store_path::StoreRoot,
    plan: &vike_data::datafusion_hist::RepairPlan,
    written: Option<bool>,
    refused: Option<&str>,
) -> String {
    let doc = serde_json::json!({
        "store_root": resolved.root.display().to_string(),
        "store_rung": resolved.rung.as_str(),
        "store_rung_why": resolved.rung.why(),
        "plan": plan,
        "parts_seen": plan.parts_seen(),
        "lossless": plan.is_lossless(),
        "losses": plan.losses(),
        "notes": plan.notes(),
        "written": written,
        "refused": refused,
    });
    serde_json::to_string_pretty(&doc).expect("a tree of plain data; serialization is total")
}

#[cfg(test)]
mod repair_series_tests {
    use super::*;

    /// ⚠ **`--dry-run` WINS over `--yes`.** The rehearsal must not require stripping a flag, so the
    /// one combination an operator reaches by ADDING a flag to a line they already have must be the
    /// safe one — and the BARE form, the one somebody types first, must rehearse.
    #[test]
    fn dry_run_wins_over_yes_and_the_bare_form_rehearses() {
        assert!(repair_writes(true, false), "--yes alone performs the rebuild");
        assert!(!repair_writes(true, true), "--dry-run WINS over --yes");
        assert!(!repair_writes(false, true), "--dry-run alone rehearses");
        assert!(!repair_writes(false, false), "the BARE form rehearses — this verb's default");
    }
}

#[cfg(test)]
mod rm_series_tests {
    use super::*;
    use vike_data::removal::{RemovalPlan, SeriesSelector};

    fn plan(matched: usize) -> RemovalPlan {
        let series = (0..matched)
            .map(|i| {
                vike_data::removal::PlannedSeries::new(
                    vike_data::SeriesId::per_symbol(
                        "bar",
                        "hyperliquid",
                        format!("S{i}"),
                        Some("1h".to_string()),
                    ),
                    Default::default(),
                    vec!["panel_bars:1".to_string()],
                    Some("panel_bars:"),
                )
            })
            .collect();
        RemovalPlan {
            selector: SeriesSelector::new("bar", "hyperliquid"),
            produced_by: Some("panel_bars:".to_string()),
            series,
        }
    }

    /// `--yes` is the non-interactive form and the ONLY one — a deliberate, greppable token in
    /// shell history.
    #[test]
    fn yes_confirms_with_or_without_a_terminal() {
        confirm_removal(&plan(3), true, false).unwrap();
        confirm_removal(&plan(3), true, true).unwrap();
    }

    /// ⚠ **The rule this verb exists to keep.** No `--yes` and no terminal REFUSES; it never falls
    /// back to reading, because `yes | backtest data rm …` is indistinguishable from a person
    /// once you have decided to read a pipe.
    #[test]
    fn no_yes_and_no_terminal_refuses_rather_than_reading() {
        let err = confirm_removal(&plan(3), false, false).unwrap_err();
        assert!(err.contains("not a terminal"), "{err}");
        assert!(err.contains("3 series"), "the refusal names what it did not delete: {err}");
    }
}

/// What `--addr` asked for. Three states, because the flag takes an OPTIONAL value.
///
/// ⚠ **The FLAG is what says "become a daemon", never the absence of a profile** — the owner's
/// distinction when they refused `--serve`, `backtest serve`, `backtest listen` and
/// `backtest daemon` on 2026-09-10. `--addr` names a THING (the socket to bind), the way `--out`
/// names a file, and being given one is what makes this process stay alive. So a bare `backtest`
/// with no profile is still the old argument error, not an accidental daemon.
#[derive(Debug, PartialEq, Eq)]
enum AddrFlag {
    /// No `--addr` at all — run one backtest and exit, exactly as before ruling 7.
    Absent,
    /// `--addr` with no value: serve on the CONFIGURED address. This is the normal use, and it is
    /// the whole point of the value being optional — the owner's *"MAY WE SET THIS SOMEWHERE AND
    /// NOT MENTION IT ALL THE TIME??"*.
    Configured,
    /// `--addr <host:port>` (or `--addr=<host:port>`): serve THERE, overriding every lower rung.
    Explicit(String),
}

/// Parse the OPTIONAL-VALUE `--addr` flag out of argv.
///
/// The rule for "did a value follow", spelled out because an optional-value flag is where CLIs
/// usually get this wrong: the next token is the VALUE only when it exists and does not begin with
/// `-`. So `backtest --addr` and `backtest --addr --json` both mean [`AddrFlag::Configured`], while
/// `backtest --addr 0.0.0.0:9999` means [`AddrFlag::Explicit`]. The `=` form is accepted too,
/// because an operator who writes `--addr=1.2.3.4:9` in a systemd `ExecStart=` should not discover
/// at runtime that this binary takes only the spaced form.
///
/// ⚠ A BLANK value is an ERROR rather than a fall-through to the configured rung. `--addr ""` in a
/// unit file is a mistake — an empty string cannot be a socket — and silently reading it as "the
/// configured address" would bind somewhere the operator did not name and never say so.
fn parse_addr_flag(args: &[String]) -> Result<AddrFlag, String> {
    for (i, a) in args.iter().enumerate() {
        if let Some(rest) = a.strip_prefix("--addr=") {
            if rest.trim().is_empty() {
                return Err("--addr= was given an empty value".to_string());
            }
            return Ok(AddrFlag::Explicit(rest.to_string()));
        }
        if a == "--addr" {
            return match args.get(i + 1) {
                Some(v) if !v.starts_with('-') => {
                    if v.trim().is_empty() {
                        Err("--addr was given an empty value".to_string())
                    } else {
                        Ok(AddrFlag::Explicit(v.clone()))
                    }
                }
                _ => Ok(AddrFlag::Configured),
            };
        }
    }
    Ok(AddrFlag::Absent)
}

// ---------------------------------------------------------------------------------------------
// The parameter-search flags: ONE pure parser, run before any I/O.
//
// Ruling 13 of `docs/superpowers/specs/2026-09-09-optimizer-trait-design.md` refused a separate
// `optimize` verb — the flags stay on `backtest` and the word "optimizer" lives in the FLAG. What
// this replaced was a hand-written ladder in [`run`] that parsed each flag INSIDE the branch that
// used it, so any flag belonging to a branch not taken was never read and never validated. That is
// one cause with four faces, and every one of them exited 0:
//
//   * `--optimizer tpe --search bogus` ran tpe (the `--optimizer` arm returned before `--search`);
//   * `--search euler --trials 0` ignored a `0` the tpe arm treats as fatal;
//   * `--search grid --euler-depth 99` silently discarded a euler-only flag — and so did
//     `--euler-depth abc`, because the malformed-value check lived in the untaken branch;
//   * `--optimizer=tpe` ran the GRID, because `binutil::arg` matched an exact token only.
//
// The rule this file now encodes, in one sentence: **every knob belongs to exactly one method, and
// a knob handed to a method that does not own it is a REFUSAL rather than a silent discard.**
// ---------------------------------------------------------------------------------------------

/// What `--keep-trials` selected.
///
/// ⚠ **`returns` is the mode that unblocked the anti-overfitting statistics, and it is NOT
/// `series` under another name.** `vike_analytics::overfit::pbo_cscv` and
/// `deflated_sharpe_with_effective_n` need an N-trial matrix of per-observation performance, and a
/// trial's equity curve is destroyed before a sweep row exists:
/// `crates/vike-backtest/src/harness/sweep.rs`'s `row_from_outcome` consumes the `BacktestResult`
/// to derive `BacktestReport`'s scalars and drops `equity_curve`, `equity_ts`, `trades` and
/// `per_symbol_curves`. The measurement that dissolved the deadlock is that the STATISTIC does not
/// want the curve: `pbo_cscv` splits `T` observations into `n_splits` contiguous blocks, so it
/// needs `T >= n_splits` and nothing more. `returns` therefore retains a FIXED-SIZE bucketed return
/// vector per trial (`harness::sweep::ReturnBuckets`, 512 buckets = 4 KB a trial, ~2 MB over a
/// 500-point grid against ~80 MB of decimated curves) and writes the resulting
/// `crate::trial_ledger::OverfitStats` into the search's `report.json`, where
/// `vike-cli backtest gate --fail-if` can name `overfit.pbo` and its siblings.
///
/// ⚠ **`series` IS STILL REFUSED BY NAME, and the refusal is narrower than it was.** What it asks
/// for is the whole CURVE per trial, which is a retention question rather than a statistical one:
/// at `vike_model::runs::MAX_EQUITY_SAMPLES` it is forty times `returns`' cost on a box whose sweep
/// worker cap is already `harness::sweep::DEFAULT_SWEEP_THREADS` because each concurrent point
/// materialises its own data slice — and no consumer in this tree reads a per-trial curve. Keeping
/// a SINGLE run's curve is what [`persist_run`] already does, and re-running the winner on its own
/// is how an operator gets one.
///
/// ⚠ **`returns` writes the same LEDGER as `scalars`**, which is what makes it resumable:
/// `reopen_search_run` keys its refusal on whether a ledger was kept ([`KeepTrials::keeps_ledger`]),
/// never on the exact spelling. A resumed search's REUSED trials contribute no column (a warm-cache
/// answer is a score, not a report — `harness::trials::WarmTrial` says why), so the matrix covers
/// the freshly-evaluated trials and `OverfitStats::excluded` reports the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeepTrials {
    None,
    Scalars,
    /// `scalars`, PLUS the in-memory bucketed return matrix the overfit statistics are computed
    /// from. See this type's doc.
    Returns,
}

impl KeepTrials {
    /// The spelling written into `search.json` and `report.json`.
    fn as_str(self) -> &'static str {
        match self {
            KeepTrials::None => "none",
            KeepTrials::Scalars => "scalars",
            KeepTrials::Returns => "returns",
        }
    }

    /// Whether this mode writes a `crate::trial_ledger::TRIALS_FILE` at all — the ONE question
    /// `--resume` actually asks, and the reason that check is not `== Scalars` any more: a
    /// `returns` search keeps a byte-identical ledger, so refusing to resume it would send an
    /// operator to "run the whole search again" for a reason that is not true.
    fn keeps_ledger(self) -> bool {
        match self {
            KeepTrials::None => false,
            KeepTrials::Scalars | KeepTrials::Returns => true,
        }
    }

    /// The recorded spelling, back as a mode. `None` for a spelling this binary does not know,
    /// which is a document written by a NEWER build rather than a corrupt one — the caller must
    /// refuse rather than guess, because guessing "it kept a ledger" would resume against a file
    /// whose format this binary has never seen.
    fn from_recorded(s: &str) -> Option<Self> {
        [KeepTrials::None, KeepTrials::Scalars, KeepTrials::Returns]
            .into_iter()
            .find(|k| k.as_str() == s)
    }

    /// The bucket capture this mode arms on the evaluator. DISARMED for every mode but `returns`,
    /// so nobody who did not ask for the statistics retains a single float.
    fn return_buckets(self) -> harness::sweep::ReturnBuckets {
        match self {
            KeepTrials::Returns => harness::sweep::ReturnBuckets::DEFAULT,
            KeepTrials::None | KeepTrials::Scalars => harness::sweep::ReturnBuckets::DISARMED,
        }
    }
}

/// The accepted `--keep-trials` values, rendered for a usage line — ONE roster, so the accepting
/// match and the message it refuses with cannot name different sets.
const KEEP_TRIALS_EXPECTED: &str = "expected none|scalars|returns";

/// `--keep-trials none|scalars|returns`, defaulting to `scalars`.
fn parse_keep_trials(args: &[String]) -> Result<KeepTrials, String> {
    match required_value(args, "--keep-trials", KEEP_TRIALS_EXPECTED)?.as_deref() {
        None => Ok(KeepTrials::Scalars),
        Some(v) if v.eq_ignore_ascii_case("none") => Ok(KeepTrials::None),
        Some(v) if v.eq_ignore_ascii_case("scalars") => Ok(KeepTrials::Scalars),
        Some(v) if v.eq_ignore_ascii_case("returns") => Ok(KeepTrials::Returns),
        // A NAMED refusal, not the generic invalid-value line below: `series` is a word the design
        // documents, so an operator who wrote it made no typo and needs the reason.
        //
        // ⚠ **THIS SENTENCE WAS REWRITTEN, and the rewrite is the point.** It used to argue that a
        // per-trial curve was unreachable BECAUSE `row_from_outcome` drops it — which stopped being
        // the whole truth the moment `ReturnBuckets` reached into that same function. A refusal
        // that has become false is worse than no refusal: an operator reading the old text would
        // conclude the statistics were impossible, when the mode that computes them is one word
        // away. So this now refuses the CURVE on cost grounds and names what IS available.
        //
        // ⚠ Both numbers are INTERPOLATED, never typed: the worker cap from
        // `harness::sweep::DEFAULT_SWEEP_THREADS` and the per-trial bucket count from
        // `harness::sweep::ReturnBuckets::DEFAULT_BUCKETS`. They are consts in another file, and a
        // hardcoded copy here would rot silently — `crates/vike-backtest/CLAUDE.md` cites the first
        // as a SYMBOL for exactly that reason.
        Some(v) if v.eq_ignore_ascii_case("series") => Err(format!(
            "--keep-trials series keeps a whole equity CURVE per trial and is still not available \
             — but what it was usually wanted FOR now is: `--keep-trials returns` retains a \
             {}-bucket return vector per trial and writes the anti-overfitting statistics (PBO via \
             CSCV, effective trial count, deflated Sharpe) into the search's report.json, where \
             `backtest gate --fail-if overfit.pbo:+10%` can name them. A curve is ~40x that \
             retention per trial, on a box whose sweep worker cap is already {} because each \
             concurrent point materialises its own data slice, and nothing in this tree reads a \
             per-trial curve. Use `--keep-trials returns` for the statistics, or re-run the \
             winning point on its own for a curve",
            harness::sweep::ReturnBuckets::DEFAULT_BUCKETS,
            harness::sweep::DEFAULT_SWEEP_THREADS
        )),
        Some(v) => Err(format!("invalid --keep-trials {v:?} ({KEEP_TRIALS_EXPECTED})")),
    }
}

/// The flags that ask for a SEARCH without naming a METHOD: every one of them a property of the
/// search itself rather than of a searcher.
///
/// ⚠ **None is a [`METHOD_KNOBS`] row and none may become one.** That table answers "may this flag
/// be written at all under the named method", and every one of these is meaningful under all four:
/// two describe the search's ARTIFACT (`harness::trial_ledger`'s documents, which every method
/// mints) and two arm OBSERVERS on the evaluator every method drives. A row there would refuse
/// `--min-trades 50` under three of the four methods for no reason at all —
/// `harness::search_select`'s `resolve_min_trades` argues it and that module's
/// `the_trade_floor_is_owned_by_no_method` pins it.
///
/// ⚠ **ONE array, WALKED by the parser and RENDERED by the refusal** — the shape
/// `crate::data_plan`'s `OnGap::NAMES` established here. [`parse_search_flags`] sets `requested`
/// by iterating it, and [`run`]'s no-`[paramscan]`-table refusal renders `--optimizer`, then
/// [`METHOD_KNOBS`], then this. Until the observer doors landed those were two hand-written lists
/// in two functions, and a flag added to the parser's side alone would have produced a refusal
/// that named NO flag while `requested` was true.
const SEARCH_PROPERTY_FLAGS: &[&str] = &["--keep-trials", "--resume", "--min-trades", "--progress"];

/// Everything argv said about the search, resolved and validated in one pure act.
#[derive(Debug)]
struct SearchFlags {
    method: SearchMethod,
    rank: RankChoice,
    /// `--keep-trials`. See [`KeepTrials`].
    keep: KeepTrials,
    /// `--resume <id>` — the search run whose ledger warms this one. See
    /// `harness::trials::TrialRecorder`.
    resume: Option<String>,
    /// `--min-trades` — the statistical-significance floor, resolved by
    /// `harness::search_select::resolve_min_trades`. [`TradeFloor::DISARMED`] when unwritten, which
    /// is what makes threading it unconditionally byte-identical to not having it.
    floor: TradeFloor,
    /// `--progress` — which progress stream, resolved by
    /// `harness::search_select::resolve_progress`. `ProgressMode::Auto` when unwritten, which emits
    /// only when stderr is a terminal.
    progress: ProgressMode,
    /// Whether argv asked for a SEARCH at all — an explicit `--optimizer`, any method-owned knob,
    /// or any [`SEARCH_PROPERTY_FLAGS`] member. A profile with no `[paramscan]` table then refuses
    /// instead of silently running one backtest. ⚠ `--rank-by` is deliberately NOT counted: it
    /// names how to ORDER results, not what work to do, and its ignore on a non-sweep profile is
    /// documented behaviour.
    requested: bool,
}

/// Whether `flag` was WRITTEN at all, in either valued spelling.
///
/// ⚠ Neither half alone is enough, and both misses are live: `has_flag` is exact-token so it never
/// sees `--euler-depth=99`, and `arg` answers `None` for a TRAILING bare `--search` with no value
/// token after it. An ownership rule written with one of them leaks exactly the argv the other
/// catches.
///
/// Local to this file rather than a fifth parser in `crates/vike-analytics/src/binutil.rs`: that
/// module is a layer-20 home shared by six bins, and this is a one-file need.
///
/// ⚠ The adjacent-name landmine, checked: `flag_given(args, "--seed")` is NOT tripped by
/// `--seed-demo` — `has_flag` is exact-token and `arg` is anchored on `--seed=`. Same for
/// `--fetch`/`--fetch-starter`.
fn flag_given(args: &[String], flag: &str) -> bool {
    has_flag(args, flag) || arg(args, flag).is_some()
}

/// The value of a VALUED flag, refusing the written-but-value-less spelling instead of reading it
/// as absent.
///
/// ⚠ **This is defect (d)'s last spelling, and it is the same failure — a DIFFERENT ANSWER rather
/// than a refusal.** [`arg`] answers `None` for a TRAILING bare `--optimizer`: the token is found,
/// there is no `=`, and there is no next token. So `backtest sweep.toml --optimizer` read as "no
/// `--optimizer` flag", fell to the `GridSearch` default, ran the exhaustive grid to completion and
/// exited 0 with no diagnostic — and on a profile with no `[paramscan]` table it also slipped past the
/// defect-(e) refusal, which keys on `SearchFlags::requested`. Its two siblings were both caught:
/// bare `--search` by [`flag_given`], and `--optimizer=` because [`arg`] answers `Some("")` there
/// deliberately. The selector was the one spelling with neither guard.
///
/// The same hole silently DEFAULTED every knob under the method that owns it — `--optimizer tpe
/// --trials` ran `TpeConfig::DEFAULT_TRIALS`, `--optimizer euler --euler-depth` ran
/// `EulerConfig::DEFAULT_MAX_DEPTH`, `--seed` ran `0` — while the SAME argv under a non-owning
/// method was refused, because the ownership check goes through [`flag_given`]. One flag, two
/// fates, decided by which optimizer was named: defect (b)'s shape wearing a different flag.
///
/// It is a script's spelling, not just a typo: `--optimizer $METHOD` with `METHOD` unset collapses
/// to the bare token, exactly as `--optimizer="$METHOD"` collapses to `--optimizer=`.
///
/// [`has_flag`] rather than [`flag_given`] in the guard: the `arg` half of that predicate has just
/// answered `None`, so what is left to detect is exactly the bare token.
fn required_value(args: &[String], flag: &str, expected: &str) -> Result<Option<String>, String> {
    match arg(args, flag) {
        Some(v) => Ok(Some(v)),
        None if has_flag(args, flag) => Err(format!(
            "{flag} was written with no value ({expected}). A trailing `{flag}` read as ABSENT \
             before, which silently ran the default instead of refusing — write `{flag} <value>`"
        )),
        None => Ok(None),
    }
}

/// Parse `--rank-by`, `--optimizer` and the per-method knobs out of argv. PURE — no store, no
/// profile, no environment, no clock.
fn parse_search_flags(args: &[String]) -> Result<SearchFlags, String> {
    // ⚠ `--search` IS RETIRED, IT IS REFUSED RATHER THAN ALIASED, AND IT IS CHECKED FIRST.
    //
    // The argument, at the call site because the brief asked for a decision and this is where it is
    // made. An alias cannot be made safe here: both spellings can be given with DIFFERENT values,
    // so every resolution of `--optimizer tpe --search euler` is a guess — and today's guess
    // ("`--optimizer` wins, silently") IS the first defect. An alias either keeps that precedence,
    // in which case the defect survives under a new name, or adds a conflict error, at which point
    // the operator must learn `--optimizer` anyway and the alias bought a permanent second spelling
    // for nothing. Deleting the second selector fixes it BY CONSTRUCTION rather than by adding a
    // check: after this there is only one selector, so there is nothing left for it to disagree
    // with. It also costs no compatibility — nothing in this tree passes `--search`, and the two
    // arm that SPAWNS this binary (`crates/vike-cli/src/cmd/backtest.rs`) sends only
    // `--profile`/`--store`/`--rank-by`/`--optimizer` and the method knobs — never `--search`.
    //
    // The refusal echoes what the operator typed and names the replacement, which is
    // `vike_config::refuse_removed_env`'s shape applied to a flag. Checked FIRST, so
    // `--optimizer tpe --search bogus` refuses on `--search` — that argv IS the first defect.
    if flag_given(args, "--search") {
        let written = args
            .iter()
            .find(|a| *a == "--search" || a.starts_with("--search="))
            .cloned()
            .unwrap_or_else(|| "--search".to_string());
        return Err(format!(
            "--search is retired — the flag is now --optimizer grid|euler|tpe|genetic. You wrote \
             {written:?}; write `--optimizer <method>` instead"
        ));
    }

    // ⚠ ONE widening worth naming: an INVALID `--rank-by` value is now refused on a profile with no
    // `[paramscan]` table too, where the ladder — which lived inside `if profile.is_paramscan()` — ignored
    // it. A VALID value's documented ignore is untouched; what changed is that a typo is no longer
    // silently swallowed on one of the two profile shapes. Nothing spawns a bogus one:
    // `crates/vike-cli/src/cmd/backtest.rs` validates the value in its own parser before it builds
    // an argv at all.
    //
    // ⚠ RESOLVED BY `harness::search_select`, not here: the same five names and the same refusal
    // sentence reach a REMOTE run through `crate::compute_server`, and one message for two routes
    // is what that module exists for.
    let rank = search_select::resolve_rank(arg(args, "--rank-by").as_deref())?;

    // The method NAME. The default is read off `GridSearch` rather than spelled, so the flag's
    // default and `Optimizer::name` cannot drift into two literals.
    //
    // ⚠ Through [`required_value`], NOT through `arg`: a trailing bare `--optimizer` answers `None`
    // there, which is "no flag given" and therefore the GRID — the flag's default silently
    // overruling the flag. That doc carries the argument; this is the one line that has to use it.
    //
    // ⚠ The `expected …` text is RENDERED from `vike_datahub_client::SEARCH_METHODS`, the one
    // roster four surfaces read, so this message and the check cannot name different sets.
    let expected = format!("expected {}", SEARCH_METHODS.join("|"));
    let named = required_value(args, "--optimizer", &expected)?;
    let mut requested = named.is_some();
    let name = named.clone().unwrap_or_else(|| DEFAULT_SEARCH_METHOD.to_string());

    // ⚠ OWNERSHIP BEFORE VALUE, on argv PRESENCE — the one half of the rule that cannot move into
    // `search_select`, because presence is an argv concept (`flag_given` sees both `--trials 8` and
    // `--trials=8`, and a bare trailing `--trials` is refused by `required_value` below). The TABLE
    // and the MESSAGE are the shared ones, so there is no second rule — only a second detector, and
    // `search_select::resolve` performs the same check over `Option` presence for the wire.
    for (flag, owners) in METHOD_KNOBS {
        if flag_given(args, flag) {
            requested = true;
            if !owners.iter().any(|o| name.eq_ignore_ascii_case(o)) {
                return Err(search_select::refuse_unowned_knob(flag, owners, &name));
            }
        }
    }

    // ⚠ NOT in `METHOD_KNOBS`: neither flag is owned by a METHOD — both are properties of the
    // search's ARTIFACT, and every method has one. They still set `requested`, so
    // `backtest run.toml --resume …` on a profile with no `[paramscan]` table refuses instead of
    // silently running one backtest — defect (e)'s rule, which is about what work was ASKED FOR.
    let keep = parse_keep_trials(args)?;
    let resume = required_value(args, "--resume", "expected a search run id")?;
    if let Some(id) = resume.as_deref()
        && id.trim().is_empty()
    {
        return Err(
            "--resume was given an empty run id — write `--resume <search-run-id>`, which \
                    `backtest trials` and the `search saved to …` line both print"
                .to_string(),
        );
    }
    // ⚠ **THE COMBINATION THAT DESTROYS THE ARTIFACT, refused HERE and not deeper.** `--resume`
    // re-opens a parent run and `open_search_run` then rewrites its `search.json`; with
    // `--keep-trials none` that header would come back saying the search kept no ledger, while
    // `trials.jsonl` sat intact beside it. The lasting damage is not the self-contradictory
    // document — it is that EVERY later `--resume` of that id is then refused with "that search ran
    // with --keep-trials none, so it kept no ledger and there is nothing to resume", which is
    // FALSE, and whose only stated remedy is to re-run the whole search: precisely the hours
    // `--resume` exists to save. One plausible argv — a shell alias, a script that always passes
    // the flag — burns the artifact with exit 0 and no warning.
    //
    // ⚠ Refused rather than SILENTLY PRESERVING the reopened header's `keep_trials`, which was the
    // other available fix. Preserving it obeys NEITHER flag: the operator wrote `--keep-trials
    // none` and the run keeps trials anyway — a DIFFERENT ANSWER rather than a refusal, which is
    // the defect class [`required_value`]'s doc argues against and the one this file has already
    // been bitten by. And the request is genuinely contradictory: a resume whose evaluations are
    // not appended can never complete the ledger it is continuing, so there is no reading under
    // which it does what it says. Refusing at argv triage also means nothing is opened and nothing
    // is rewritten — the artifact cannot be damaged even by the attempt.
    //
    // ⚠ Keyed on [`flag_given`], never on the resolved value: `KeepTrials::None` is reachable only
    // by WRITING the flag, and a DEFAULT must never be refused.
    if resume.is_some() && keep == KeepTrials::None && flag_given(args, "--keep-trials") {
        return Err(
            "--resume and --keep-trials none contradict each other: a resume continues a search's \
             LEDGER, and `none` writes no ledger to continue. It would also overwrite the resumed \
             run's search.json to say it kept no trials, after which every later --resume of that \
             id is refused for a reason that is not true. Drop one of the two flags"
                .to_string(),
        );
    }
    // ⚠ **THE TWO OBSERVERS, and neither is a [`METHOD_KNOBS`] row** — [`SEARCH_PROPERTY_FLAGS`]
    // carries why. Read through [`required_value`] like every other valued flag here, so a trailing
    // bare `--min-trades` is refused rather than read as absent and silently defaulted: the exact
    // defect that doc calls "(d)'s last spelling", and the one a script writing
    // `--min-trades $FLOOR` with `FLOOR` unset produces.
    //
    // ⚠ Both VALUES are resolved by `harness::search_select`, not here, for the reason that module
    // exists: the floor's "`0` disarms" rule and the progress refusal that RENDERS
    // `ProgressMode::NAMES` are one implementation, so a third surface arming these cannot answer
    // differently. This file owns only the argv PRESENCE half.
    //
    // ⚠ The `expected …` text for `--progress` is RENDERED from that roster rather than typed, the
    // same property `--optimizer`'s `expected` line buys from `SEARCH_METHODS`: a fourth mode
    // cannot be accepted by the parser and missing from the value-less refusal.
    let min_trades = required_value(args, "--min-trades", "expected a non-negative integer")?;
    let floor = search_select::resolve_min_trades(min_trades.as_deref())?;
    let progress_expected = format!("expected {}", ProgressMode::NAMES.join("|"));
    let progress_written = required_value(args, "--progress", &progress_expected)?;
    let progress = search_select::resolve_progress(progress_written.as_deref())?;

    // ⚠ DERIVED from [`SEARCH_PROPERTY_FLAGS`], never re-spelled: `run`'s refusal renders the same
    // array, so a member that set `requested` here and was missing there would make that sentence
    // name no flag at all.
    requested = requested || SEARCH_PROPERTY_FLAGS.iter().any(|f| flag_given(args, f));

    // The three knob VALUES, read through `required_value` so a written-but-value-less spelling is
    // refused here rather than read as absent, then handed to the ONE resolver — which repeats the
    // ownership check over `Option` presence (the only presence a wire frame has) and owns every
    // value parser and every refusal sentence, so a `--local` run and an `--addr` run answer the
    // same way.
    let euler_depth = required_value(args, "--euler-depth", "expected an integer")?;
    let trials = required_value(args, "--trials", "expected a positive integer")?;
    let seed = required_value(args, "--seed", "expected a u64")?;

    let method = search_select::resolve(&SearchSelection {
        optimizer: named.as_deref(),
        euler_depth: euler_depth.as_deref(),
        trials: trials.as_deref(),
        seed: seed.as_deref(),
    })?;

    Ok(SearchFlags { method, rank, requested, keep, resume, floor, progress })
}

/// The value-taking flags REACHABLE on the profile path, so a flag's VALUE is never counted as the
/// positional profile.
///
/// ⚠ Deliberately not every flag this file knows. `--kind`/`--venue`/`--symbol`/`--group`/
/// `--interval`/`--produced-by`/`--out`/`--from`/`--to`/`--days` all belong to [`run_data`]'s
/// subcommand, which has already RETURNED by the time [`profile_from_args`] runs — a `data` line
/// never reaches the profile path at all. `--addr` cannot appear at all either:
/// [`parse_addr_flag`] answers `AddrFlag::Absent` only when argv holds no `--addr`
/// token in EITHER spelling, so reaching the profile path proves there is none. That is what keeps
/// this table short and lets it carry no optional-value concept — the one thing a positional
/// scanner cannot express.
const PROFILE_PATH_VALUED: &[&str] = &[
    "--profile",
    "--store",
    "--rank-by",
    "--optimizer",
    "--euler-depth",
    "--trials",
    "--seed",
    // ⚠ Every one of these is VALUED, so every one must be here or its value is read as the
    // positional profile — `no_flag_value_can_be_mistaken_for_the_positional_profile` iterates this
    // table, so adding the row is what buys the coverage.
    //
    // ⚠ `--min-trades 50` is the argv that makes it concrete: without its row the scan sees `50` as
    // a bare token, calls it the profile, and `backtest sweep.toml --min-trades 50` is refused for
    // giving the profile TWICE — naming a file the operator never typed. `--progress json` is worse
    // still, because `json` looks like a filename.
    "--keep-trials",
    "--resume",
    "--min-trades",
    "--progress",
];

/// The words that are SUBCOMMANDS here and therefore can never be read as the positional profile:
/// `(word, what follows it)`. Ruling 12's `data` and stage 5's `trials` collide with ruling 14's
/// positional profile on exactly one token each, and [`run`] resolves that by routing only when the
/// word comes FIRST. This table is the OTHER half — the refusal for the case that reaches
/// [`profile_from_args`].
const SUBCOMMANDS: &[(&str, &str)] = &[
    (DATA_SUBCOMMAND, "<fetch|fetch-starter|seed-demo|export|rm|repair>"),
    (TRIALS_SUBCOMMAND, "<id>"),
];

/// The profile, in either spelling: `backtest my.toml` (ruling 14) or `backtest --profile my.toml`.
///
/// ⚠ **`--profile` CANNOT retire, and that is a wire fact rather than a preference.**
/// `crates/vike-cli/src/cmd/backtest.rs`'s local arm builds `vec!["--profile".into(), …]` and
/// SPAWNS this binary, and `scripts/cli_mcp_smoke.sh` runs it. So the positional is ADDITIVE.
/// (It used to be TWO arms; `vike-cli sweep` was deleted by ruling 13 and its search flags moved
/// onto `vike-cli backtest`, which spawns the same way.)
///
/// ⚠ **BOTH is a REFUSAL, not a precedence rule.** Two spellings that may name two different files
/// is the same defect class as two searcher selectors: picking a winner silently answers a question
/// the operator did not know they had asked.
///
/// `Ok(None)` means "none given" — the caller's own arm, because that is the one case with no
/// specific mistake to name and therefore the one that still prints the usage text.
///
/// The scan skips a valued flag's VALUE by table rather than by "does the next token start with
/// `-`", because `--store dir` puts a bare token in argv that is emphatically not a profile.
fn profile_from_args(args: &[String]) -> Result<Option<String>, String> {
    let flagged = arg(args, "--profile");
    let mut bare: Vec<&String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        // A bare valued flag consumes the next token. An INLINE `--flag=v` consumes nothing, and is
        // skipped by the `starts_with('-')` arm below like any other flag token.
        if PROFILE_PATH_VALUED.contains(&a.as_str()) {
            let _ = it.next();
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        bare.push(a);
    }
    match (flagged, bare.as_slice()) {
        // ⚠ `data` IS a positional, so ruling 12's subcommand and ruling 14's profile collide on
        // exactly one word — and [`run`] resolves it by routing only when `data` comes FIRST. That
        // leaves this case: `backtest --optimizer tpe data rm …` reaches here with `data` as the
        // profile, and without this arm it loads a file called `data`, fails on the open and names
        // a path the operator never typed. Refused by name instead, saying where the word goes.
        //
        // The declared cost, per word: a profile literally NAMED `data` or `trials` (no extension)
        // is no longer reachable positionally. `--profile data` still reaches it, which is what the
        // arm says. TABLE-DRIVEN since stage 5, so a third subcommand joins by adding a row rather
        // than a fourth match arm.
        (None, [p]) if SUBCOMMANDS.iter().any(|(w, _)| *w == p.as_str()) => {
            let (word, shape) =
                SUBCOMMANDS.iter().find(|(w, _)| *w == p.as_str()).expect("just matched");
            Err(format!(
                "`{word}` is a SUBCOMMAND here, not a profile, and it must come FIRST — write \
                 `backtest {word} {shape} …`. If you really meant a profile file called `{word}`, \
                 spell it `--profile {word}`"
            ))
        }
        (Some(p), []) => Ok(Some(p)),
        (None, [p]) => Ok(Some((*p).clone())),
        (None, []) => Ok(None),
        (Some(p), [b]) => Err(format!(
            "the profile was given twice — --profile {p:?} and the positional {b:?}. They may name \
             two different files, so this is refused rather than resolved: give it once"
        )),
        (flagged, extra) => Err(format!(
            "more than one profile was given: {}{extra:?}. Give exactly one",
            flagged.map(|p| format!("--profile {p:?} and ")).unwrap_or_default()
        )),
    }
}

/// The `--addr` value LADDER, mirroring `datahub`'s exactly: an explicit flag value, then
/// `VIKE_BACKTEST_ADDR`, then `config.backtest_addr`, then `vike_config::DEFAULT_BACKTEST_ADDR`.
///
/// ⚠ The env rung is not read here, and that is the point. `vike_config::Config::apply_env` folds
/// `VIKE_BACKTEST_ADDR` OVER the file layer before this function sees it, so `configured` already
/// carries whichever of the two won — which keeps this workspace's "one loader decides precedence"
/// property intact and stops a second, subtly different ladder existing inside a binary. Pure, so
/// the ladder is unit-testable without a settings directory and without an environment.
fn resolve_serve_addr(flag: &AddrFlag, configured: Option<&str>) -> String {
    match flag {
        AddrFlag::Explicit(v) => v.clone(),
        _ => configured
            .map(str::to_string)
            .unwrap_or_else(|| vike_config::DEFAULT_BACKTEST_ADDR.to_string()),
    }
}

/// This binary's settings — the DATABASE first, the files only on a box that has none.
///
/// ⚠ **Owner ruling, 2026-09-23: the datahub address comes from the settings DATABASE** — *"it had
/// to get address from sqlite, no files; all settings have to live in sqlite"*. That is what the
/// `StoreLayer` below already does, and what a narrower ladder would have broken: reading
/// `$VIKE_DATAHUB_ADDR` and falling through to the compiled default would have left a configured
/// address unread on exactly the boxes that were migrated on 2026-09-14
/// (`docs/decisions/0054-settings-move-into-one-database.md`).
///
/// ⚠ **A settings directory is OPTIONAL** (a checkout run from anywhere has none) **but a
/// MALFORMED store is FATAL.** That rule used to be argued for the socket alone — *"on the path
/// that opens a socket, a config this box has and cannot parse must never be silently skipped"* —
/// and it now governs the RUN path too, which is a real behaviour change: a plain `backtest run`
/// on a box with a broken settings store used to ignore it and run on defaults. It is the same
/// argument and it is stronger here, not weaker — a run that silently used default settings would
/// publish a result nobody could reproduce, and its own record would name settings it never read.
///
/// ⚠ **THROUGH THE SETTINGS SOURCE, and this is an UNDECLARED composition root** (it walks for its
/// own settings directory). It is invisible to `crates/vike-boot/tests/one_owner.rs` only because
/// that gate's `production_half` truncates this file at its `#[cfg(test)]` module. The hole is
/// real and this is what fell into it: a store-blind `vike_config::load` on a box that has run
/// `vike-cli config adopt` reads `config.*` out of files nothing resolves from. Hoisting did not
/// CLOSE that hole — it kept it at ONE, which is the most this change can honestly claim.
///
/// `who` prefixes every line, so each arm still says which invocation is talking.
fn load_backtest_settings(
    vars: &std::collections::HashMap<String, String>,
    who: &str,
) -> Result<vike_config::Settings, ExitCode> {
    // ⚠ `VIKE_SETTINGS_DIR` names the directory outright and beats the walk, and it is spelled as a
    // LITERAL rather than through `vike_config`'s constant: the settings registry's map-lookup
    // sweep resolves constants CRATE-WIDE, so importing one would make this read invisible to
    // `crates/vike-ops/tests/settings_registry.rs`.
    let settings_override = vars.get("VIKE_SETTINGS_DIR").map(String::as_str);
    let settings_dir = std::env::current_dir()
        .ok()
        .and_then(|cwd| vike_model::state_path::project_settings_dir_from(settings_override, &cwd));
    let store = settings_dir.as_deref().map(vike_secrets::read_settings_in);
    let mut store_refusal = String::new();
    let source = vike_config::StoreLayer::of(store.as_ref(), &mut store_refusal);
    let settings = match vike_config::load_with_source(
        settings_dir.as_deref(),
        source,
        vars,
        &vike_config::CliOverrides::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{who}: {e}");
            return Err(ExitCode::from(2));
        }
    };
    for w in &settings.warnings {
        eprintln!("{who}: {w}");
    }
    Ok(settings)
}

/// Become the COMPUTE daemon: resolve the address, load the node keys, classify the bind, open the
/// store and serve the seven verbs forever.
///
/// The composition order is the data daemon's, deliberately (`vike_datahub::datahub_cli`'s `run`):
/// settings, then keys, then the BIND DECISION — before the store is opened and before a listener
/// exists — then the listener, then `serve_authed`. A refused configuration must cost nothing and
/// touch nothing.
///
/// ⚠ **A refusal EXITS 2** rather than degrading, and that is this invocation's own rule rather than
/// the workspace's: serving is the only thing `--addr` was asked to do, so "do not bind" and "do not
/// run" are the same decision. Exit 2 is the refused-configuration family this binary already uses
/// for a rejected argument — not `FAILURE`, because nothing was tried and failed.
///
/// `studio` is the injected [`crate::compute_server::StudioRunTable`] the composition root handed
/// down; see this function's mount comment for why a `None` here is a build fact rather than an
/// omission.
fn run_serve(
    vars: &std::collections::HashMap<String, String>,
    args: &[String],
    flag: AddrFlag,
    studio: Option<crate::compute_server::StudioRunTable>,
    study: Option<crate::compute_server::StudyRunFactory>,
) -> ExitCode {
    use std::net::{TcpListener, ToSocketAddrs};

    use vike_datahub_client::bind::{BindDecision, ServerAuth, bind_decision};

    // ⚠ `VIKE_SETTINGS_DIR` names the directory outright and beats the walk — the ONE fact this
    // root pulls out of the swept map to find its project, and spelled as a LITERAL for the reason
    // the indicator read above gives (the settings registry's map-lookup sweep resolves constants
    // crate-wide, so importing the constant would make the read invisible to the gate).
    let settings_override = vars.get("VIKE_SETTINGS_DIR").map(String::as_str);
    // ⚠ **The walk and the load moved into `load_backtest_settings`, which the RUN path calls too.**
    // They used to be spelled out right here, which made the `--addr` daemon the ONLY arm of this
    // binary that could see a configured `config.datahub_addr`; the run path now needs the same
    // key to know where its history comes from. ONE function walks for the settings directory, so
    // there is still one place that does it — but it is CALLED from each arm at that arm's own
    // moment, because they are not the same moment: this one is before a socket opens, and the run
    // path's is after the argv triage that refuses a command line without performing any I/O. The
    // two arms are mutually exclusive, so at most one call ever runs. That function's doc carries
    // what this comment used to: why the walk happens, why a malformed store is fatal, and which
    // gate cannot see any of it.
    let settings = match load_backtest_settings(vars, "backtest --addr") {
        Ok(s) => s,
        Err(code) => return code,
    };
    // **`preferences.sweep_threads` reaches the sweep pool from HERE** — this arm INSTALLS it even
    // though it no longer LOADS it, and the halves are deliberately apart: installing is
    // process-wide and once, so it belongs where a daemon is about to serve, while the load now
    // serves both arms. ⚠ The RUN path therefore still leaves this key inert — unchanged by the
    // hoist, and the state `0057`s Phase 0 already names as owed; wiring it would be a parallelism
    // change riding a routing PR. The value is the loader's resolved answer
    // (`env > store > file > default`), and
    // `crate::harness::install_sweep_threads` is the process-wide handle the pool reads at
    // construction; without this call the FILE key would be inert on this daemon and only
    // `VIKE_SWEEP_THREADS` would bite, which is the state
    // `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 0 names as
    // owed. Before the listener binds, so the first served sweep already has it.
    crate::harness::install_sweep_threads(settings.preferences.sweep_threads);
    let addr = resolve_serve_addr(&flag, settings.config.backtest_addr.as_deref());

    // The node keys, from the NODE store (`node.env`, falling back to the venue-key file only while
    // a box has not migrated) — the same pair, under the same domain separator, the data daemon
    // authenticates with (`crate::compute_server`'s `serve_authed` carries why one pair rather than
    // two). `vike_secrets`, not `vike_bridge_core::credentials`: the canonical wrapper drags the
    // ureq/tungstenite/rustls transport stack, and nothing here needs a transport.
    //
    // ⚠ A store that EXISTS and cannot be READ is NOT "no credentials". The first is a permissions
    // bug that silently drops this server to unauthenticated; the second is the ordinary
    // unconfigured state. They must never look the same to an operator — and on a NON-LOOPBACK bind
    // the difference decides whether this process starts at all, so the two refusals below say
    // which case they are.
    //
    // ⚠ The probe is the DATAHUB family (this daemon authenticates with that pair), not all four
    // platform names: `resolve_node_keys` answers WHICH FILE, so a wide predicate lets the
    // TRADEHUB pair's migration decide where this one is read from — a `node.env` holding only that
    // pair would answer `NodeFile` and a datahub key still in `secrets.env` would resolve to
    // nothing, dropping this server to unauthenticated with nothing said about why.
    let mut store_unreadable = false;
    let credentials: std::collections::HashMap<String, String> =
        match vike_secrets::resolve_node_keys(
            settings_override,
            vike_model::credential_keys::is_datahub_node_key,
        ) {
            Ok((resolved, _source)) => {
                if let Some(w) = &resolved.warning {
                    eprintln!("backtest --addr: {w}");
                }
                if let Some(w) = &resolved.legacy {
                    eprintln!("backtest --addr: {w}");
                }
                resolved.secrets.into_map()
            }
            Err(e) => {
                eprintln!(
                    "backtest --addr: credential store PRESENT but UNREADABLE ({e}) — any \
                     configured node keys were NOT loaded. On a LOOPBACK bind this server is about \
                     to serve UNAUTHENTICATED behind the bind guard alone; on a NON-LOOPBACK one it \
                     will REFUSE TO START. Either way the cause is that file: fix its permissions \
                     and restart"
                );
                store_unreadable = true;
                Default::default()
            }
        };
    let keys = vike_node_proto::auth::node_keys_from_vars(&credentials);

    // Classify the bind BEFORE the store opens or the listener binds — the same guard, from the
    // same function, the data daemon runs (`vike_datahub_client::bind`). It matters at least as
    // much here: what a key-less non-loopback bind would expose on THIS daemon is a Rhai compiler
    // running source the client supplied.
    //
    // The opt-in keeps the datahub's spelling, `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1`, on purpose: it
    // consents to "this box's node protocol may be reached off-box", which is one posture decision
    // for one box, and a second variable would let an operator believe they had answered it while
    // the other daemon still refused.
    let allow_public = vars.get("VIKE_DATAHUB_ALLOW_PUBLIC_BIND").map(String::as_str) == Some("1");
    let resolved_addrs: Vec<std::net::SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    match bind_decision(&resolved_addrs, allow_public, ServerAuth::of(keys.as_ref())) {
        BindDecision::Proceed => {}
        BindDecision::ProceedExposed(exposed) => {
            eprintln!(
                "backtest --addr: binding a NON-LOOPBACK address {exposed} \
                 (VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1). Node keys ARE configured, so every connection \
                 must authenticate — but the handshake is PLAINTEXT and authenticates the \
                 CONNECTION, not each frame, so keep an SSH tunnel or a VPN in front of it"
            );
        }
        BindDecision::RefuseUnauthenticated(exposed) if store_unreadable => {
            eprintln!(
                "backtest --addr: {exposed} is NOT loopback and this server could not READ its \
                 credential store, so it has no node keys to authenticate with — refusing to \
                 start. This is NOT a missing configuration: the store is present and its keys may \
                 well be correct. Fix the file's permissions (`vike-cli secrets path` prints it) \
                 and restart"
            );
            return ExitCode::from(2);
        }
        BindDecision::RefuseUnauthenticated(exposed) => {
            eprintln!(
                "backtest --addr: {exposed} is NOT loopback and this server has NO node keys — \
                 refusing to start. VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 consents to being REACHABLE; \
                 it is not consent to compile and run RHAI THE CLIENT SUPPLIES, unauthenticated, \
                 for anyone who can open a socket. Either set VIKE_DATAHUB_OBSERVE_KEY and \
                 VIKE_DATAHUB_CONTROL_KEY in the credential store, or put the address back on \
                 127.0.0.1 and reach it with `ssh -L`"
            );
            return ExitCode::from(2);
        }
        BindDecision::Refuse(exposed) => {
            eprintln!(
                "backtest --addr: {exposed} is NOT loopback — refusing to start. This server is \
                 meant to be reached over an SSH tunnel (its handshake is plaintext even when node \
                 keys ARE set). If this box genuinely must listen on a trusted network, set \
                 VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1 — and set the node keys first, so what is \
                 exposed is authenticated"
            );
            return ExitCode::from(2);
        }
    }

    // ---- WHERE THE HISTORY COMES FROM -----------------------------------------------------------
    //
    // ⚠ **This daemon used to OPEN the store, and that was the second reader
    // `docs/decisions/0084-only-the-datahub-touches-the-store.md` forbids.** The comment that stood
    // here argued the opposite and was right at the time: "compute-to-data means the profile
    // crosses the wire and the history does not, which only holds if this process opens the same
    // root the data daemon serves". It did open the same root — on the deployed box both daemons
    // point at one directory — which is exactly the arrangement 0084 rules out: two processes with
    // the files open, one wire that is therefore optional, and a verb set that drifts because the
    // consumer who needs it can go around.
    //
    // So the DEFAULT is now the wire. The dial is loopback on the deployed box (the data daemon is
    // the same machine), so what this costs is a socket hop, and what it buys is that the datahub
    // is the only thing holding the files.
    //
    // ⚠ **`--store` on the LINE is the local escape, and the ENVIRONMENT no longer chooses.**
    // `VIKE_HIST_STORE` still answers WHICH root a local read uses; it does not select the route.
    // That is a deliberate break: the deployed unit sets that variable, so leaving it in charge
    // would have kept the live daemon local and this change would have proved nothing where it
    // matters. A flag on the line is a decision somebody made for one invocation; an inherited
    // variable is one nobody remembers making.
    //
    // ⚠ **The availability coupling is REAL and is the point, not a side effect**: a datahub outage
    // now takes every compute run with it. That is what one reader means. `--store` is the
    // documented way to compute against local files when the data daemon is down.
    let local_root = arg(args, "--store").map(PathBuf::from);
    let route = history_route(local_root.as_deref(), settings.config.datahub_addr.as_deref());
    let store: std::sync::Arc<dyn vike_data::HistStore + Send + Sync> = match &route {
        HistoryRoute::Local => {
            let root = store_root(local_root.clone(), vars);
            match DataFusionHist::open(&root) {
                Ok(h) => std::sync::Arc::new(h),
                Err(e) => {
                    eprintln!("backtest --addr: failed to open hist store at {root:?}: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        // `with_keys` whenever a pair resolved, `new` only when none did — `with_keys` degrades to
        // a plain unauthenticated connect against a KEY-LESS server, so one spelling works against
        // the keyed production datahub and a bare dev one.
        HistoryRoute::Wire(hub) => match keys.clone() {
            Some(k) => std::sync::Arc::new(vike_datahub_client::RemoteHistStore::with_keys(hub, k)),
            None => std::sync::Arc::new(vike_datahub_client::RemoteHistStore::new(hub)),
        },
    };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("backtest --addr: failed to bind {addr}: {e}");
            return ExitCode::FAILURE;
        }
    };
    // ⚠ The route is DISCLOSED, and it names the address rather than a path on the remote arm: the
    // resolved root is the SERVER's, and a directory printed client-side would be a guess about
    // another box's filesystem. `crates/vike-cli/src/cmd/data.rs`'s remote arm withholds it for
    // the same reason.
    match &route {
        HistoryRoute::Local => eprintln!(
            "backtest --addr: listening on {addr}, store {} (LOCAL — `--store` was given)",
            store_root(local_root.clone(), vars).display()
        ),
        HistoryRoute::Wire(hub) => eprintln!(
            "backtest --addr: listening on {addr}, history over the wire from the datahub at {hub}"
        ),
    }

    // ⚠ `studio` is `None` on a bare `cargo run -p vike-backtest --bin backtest -- --addr`, and
    // `Some` under `vike-backend backtest --addr`. That is a LAYER fact, not an omission: the three
    // Studio verbs run `vike_studio_core`'s slice runners, and that crate sits ABOVE this one in the
    // layer graph, so only a composition root that can name both can hand them down.
    // `crates/vike/src/main.rs`'s `backtest_main` does. A daemon without the table advertises none
    // of the three and refuses them by name — the `FEATURE_BACKFILL` shape, applied to a mount
    // rather than to a feature.
    // ⚠ THE STUDY RUNNER IS BUILT HERE, from THIS daemon's own walk. The composition root named
    // the constructor and resolved nothing (`crates/vike-ops/tests/multicall_gate.rs`'s
    // `the_dispatcher_starts_nothing` forbids it a `state_path::` call at all), so the two paths
    // the runner captures are resolved on the rung that already resolved this daemon's settings —
    // one walk, one answer. Both are the DAEMON's configuration and neither is a wire field: a
    // client may not steer a filesystem it cannot see, which is what `vike-cli research study`
    // refuses `--store` and `--lightgbm` by name for.
    //
    // ⚠ An ABSENT trainer is a WORKING state, not an error: the study refuses BY NAME
    // (`vike_user_research::StudyError::NoLearner`) and a run made with no learner is a different
    // experiment rather than a failed one. So a box with no `bin/lightgbm/` still serves
    // non-fitting studies.
    let study = study.map(|build| {
        let cwd = std::env::current_dir().ok();
        let runs_root = cwd
            .as_deref()
            .and_then(|c| {
                vike_model::state_path::user_runs_dir_from(
                    vars.get("VIKE_USER_DATA_DIR").map(String::as_str),
                    c,
                )
            })
            .unwrap_or_else(|| PathBuf::from("user_data").join("runs"));
        let trainer = cwd
            .as_deref()
            .and_then(|c| vike_model::state_path::project_bin_dir_from(settings_override, c))
            .map(|bin| {
                bin.join("lightgbm").join(if cfg!(windows) { "lightgbm.exe" } else { "lightgbm" })
            })
            .filter(|p| p.is_file());
        build(runs_root, trainer)
    });

    // The NAMED-RUN lane (`docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8),
    // resolved HERE out of the sweep this function already owns rather than inside the server.
    // Two reasons, and the second is a gate: a library that reads the process environment is
    // configuration its caller can neither see nor override, and
    // `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` is a RATCHET that may only
    // shrink, so a new `Layer::Library` row would redden CI rather than merely being untidy.
    let named_run = crate::named_run::NamedRunLane::from_vars(vars);
    if named_run.armed() {
        // Said ONCE at startup, beside the auth verdict, because arming this lane is the act that
        // lets a read-only credential spend this box's CPU — and publishes the operator's own
        // compiled-in strategy names. An operator who did not mean to should see it in the log.
        tracing::info!(
            "backtest --addr: the NAMED-RUN lane is ARMED ({}=1) — an OBSERVE-scope peer may run \
             one strategy from this daemon's own roster over one bounded window, and may enumerate \
             that roster. It carries no source and writes nothing \
             (docs/decisions/0064-a-named-run-carries-no-source.md)",
            crate::named_run::NAMED_RUN_ENV
        );
    }

    match crate::compute_server::serve_authed(listener, store, studio, study, keys, named_run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("backtest --addr: serve loop ended with an error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The PURE half of the search-flag gate. `crates/vike-backtest/tests/optimizer_cli.rs` is the
/// shipped-binary half and is where the four defects are proven as an operator experiences them;
/// these are the table-driven pins a spawned-binary test cannot buy — a forgotten
/// [`METHOD_KNOBS`] or [`PROFILE_PATH_VALUED`] row reddens HERE, by name, rather than turning into
/// a silently accepted knob or a flag value read as a profile path.
#[cfg(test)]
mod search_flag_tests {
    use super::*;

    fn argv(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    /// Every name `--optimizer` accepts, paired with the argv that makes the SELECTION COMPLETE.
    ///
    /// Only `genetic` needs anything, and the pair exists because of it: its `--seed` is REQUIRED
    /// ([`require_seed`]), so `["--optimizer", "genetic"]` alone is an error and a bare list of
    /// names could no longer drive these tests. Written as a table rather than special-cased at
    /// four call sites, so a fifth method with its own required input joins by adding a row.
    ///
    /// ⚠ **The NAMES are no longer written here.** They come from
    /// `vike_datahub_client::SEARCH_METHODS` — the ONE roster the engine, `vike-cli`, the wire and
    /// the MCP schema all read (§15.1 of
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`). What stays local is the
    /// per-method EXTRA argv, which is a FIXTURE rather than a roster: a fifth method with its own
    /// required input joins by adding one arm to [`extra_argv_for`], and a fifth method with none
    /// joins by adding nothing at all.
    fn extra_argv_for(method: &str) -> &'static [&'static str] {
        if method == "genetic" { &["--seed", "7"] } else { &[] }
    }

    /// The roster paired with each spelling's completing argv.
    fn methods() -> Vec<(&'static str, &'static [&'static str])> {
        SEARCH_METHODS.iter().map(|name| (*name, extra_argv_for(name))).collect()
    }

    /// [`methods`] with the extra argv dropped — for the checks that only need the NAMES, and
    /// where naming a method incompletely is the point (an ownership refusal fires before any
    /// method is constructed, so it must not depend on the selection being complete).
    fn method_names() -> Vec<&'static str> {
        SEARCH_METHODS.to_vec()
    }

    /// Every method named in [`METHOD_KNOBS`] is a method [`parse_search_flags`] actually accepts.
    ///
    /// ⚠ **This became load-bearing when the owner column became a SET, and it did not exist
    /// before.** With one owner per flag a typo'd name ("tpee") merely made the flag universally
    /// refused, and the matrix test below still passed — every method reaches its `else` branch and
    /// the refusal names the typo, so the assertion holds. In a TWO-element set the same typo is
    /// worse and just as quiet: one method keeps its access, the other silently loses it, and the
    /// matrix test is satisfied either way because it reads the owner set as the definition of
    /// truth. Checking the names against the selector closes the loop.
    #[test]
    fn every_method_flag_names_only_real_methods() {
        for (flag, owners) in METHOD_KNOBS {
            assert!(!owners.is_empty(), "{flag} must be owned by at least one method");
            for owner in owners.iter() {
                let extra = methods()
                    .iter()
                    .find(|(name, _)| name == owner)
                    .unwrap_or_else(|| panic!("{flag} names {owner:?}, which is not a method"))
                    .1;
                let mut a: Vec<&str> = vec!["--optimizer", owner];
                a.extend_from_slice(extra);
                let flags = parse_search_flags(&argv(&a))
                    .unwrap_or_else(|e| panic!("{flag}'s owner {owner:?} must select: {e}"));
                assert_eq!(
                    search_select::optimizer_for(flags.method).name(),
                    *owner,
                    "{flag} must name the method whose Optimizer::name is {owner:?}"
                );
            }
        }
    }

    /// Every method name `--optimizer` accepts builds the method whose `Optimizer::name` answers
    /// with that same string — and the absent flag builds the grid.
    ///
    /// ⚠ This is what REPLACES reading `EulerSearch::NAME` and `TpeSearch::NAME`, which are private
    /// consts deliberately (`harness/mod.rs`: a method's knowledge belongs in the method's file).
    /// Widening them for the sake of a match would reverse that; pinning the round trip keeps the
    /// two literals in [`parse_search_flags`] from drifting away from the methods they name.
    #[test]
    fn every_optimizer_spelling_builds_the_method_it_names() {
        for (name, extra) in &methods() {
            let mut a: Vec<&str> = vec!["--optimizer", name];
            a.extend_from_slice(extra);
            let flags = parse_search_flags(&argv(&a)).expect("a valid method");
            assert_eq!(
                search_select::optimizer_for(flags.method).name(),
                *name,
                "--optimizer {name} must build the method whose name() is {name:?}"
            );
            assert!(flags.requested, "an explicit --optimizer is a REQUESTED search");
        }
        let default = parse_search_flags(&argv(&[])).expect("no flags is the default");
        assert_eq!(search_select::optimizer_for(default.method).name(), DEFAULT_SEARCH_METHOD);
        assert_eq!(
            harness::sweep::GridSearch.name(),
            DEFAULT_SEARCH_METHOD,
            "…and the protocol's default names the method this crate implements"
        );
        assert!(!default.requested, "no search flag is no search REQUEST");
    }

    /// Every knob is refused by every method that does not own it, and accepted by EVERY method
    /// that does — driven from [`METHOD_KNOBS`] × [`methods`], so a fifth method or a fifth knob
    /// joins this check by adding one row rather than by somebody remembering.
    ///
    /// ⚠ **The widening to owner SETS is exactly where this test could have stopped meaning
    /// anything, so read what it now asserts.** For each flag, the method list is partitioned by
    /// the flag's own owner set: every owner must ACCEPT it and every non-owner must REFUSE it.
    /// A single-owner flag therefore gets the identical three-refusals-one-accept check it always
    /// had — `--trials` and `--euler-depth` are here to keep proving the rule did not soften into
    /// "somebody owns it, so let it through" — while `--seed` gets two accepts and two refusals.
    /// The refusal must name EVERY owner, not just the first: a two-owner flag whose refusal named
    /// one method would be a worse message than the one it replaced.
    ///
    /// ⚠ The values are VALID for the owning method on purpose (`3`, `8`, `7`), so nothing but the
    /// ownership rule can produce these refusals: a "fix" that merely hoisted the range checks out
    /// of the branches would leave this red.
    ///
    /// ⚠ The owning-method ACCEPT is spelled with that method's completing argv from [`methods`],
    /// because `--optimizer genetic --seed 7` is a complete selection and `--optimizer genetic`
    /// alone is not. The REFUSAL half deliberately is NOT: ownership is checked before any method
    /// is constructed, so an incomplete selection must still produce the ownership error — which is
    /// the ordering `a_knob_another_method_owns_is_refused_when_genetic_was_named` pins from the
    /// shipped binary's side.
    #[test]
    fn a_knob_is_refused_by_every_method_that_does_not_own_it() {
        for (flag, owners) in METHOD_KNOBS {
            let value = if *flag == "--euler-depth" { "3" } else { "8" };
            for (name, extra) in &methods() {
                let owns = owners.contains(name);
                let mut a: Vec<&str> = vec!["--optimizer", name];
                // The method's completing argv — unless the knob under test IS that argv, in which
                // case adding both would put one flag in argv twice.
                if owns && !extra.contains(flag) {
                    a.extend_from_slice(extra);
                }
                a.extend_from_slice(&[flag, value]);
                let got = parse_search_flags(&argv(&a));
                if owns {
                    assert!(got.is_ok(), "{flag} must be accepted by its owner {name}: {got:?}");
                } else {
                    let msg = got.expect_err(&format!("{flag} under {name} must be refused"));
                    assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
                    for owner in owners.iter() {
                        assert!(
                            msg.contains(owner),
                            "…and EVERY method that owns it, {owner} included: {msg}"
                        );
                    }
                }
            }
            // No `--optimizer` at all is the GRID, and a knob for an unchosen method is still one.
            if !owners.contains(&"grid") {
                assert!(parse_search_flags(&argv(&[flag, value])).is_err());
            }
            // The INLINE spelling too — the exact place an ownership rule written with `has_flag`
            // leaks, and a bare trailing knob, the place one written with `arg` alone leaks. Both
            // under a method that does NOT own the flag, picked out of the method list rather than
            // hard-coded, because `--seed`'s arrival means no single name is a non-owner of
            // everything any more.
            let stranger = method_names()
                .into_iter()
                .find(|n| !owners.contains(n))
                .expect("no knob is owned by every method");
            assert!(
                parse_search_flags(&argv(&["--optimizer", stranger, &format!("{flag}={value}")]))
                    .is_err()
            );
            assert!(parse_search_flags(&argv(&["--optimizer", stranger, flag])).is_err());
        }
    }

    /// `--trials 0` is refused under EVERY method. Under `tpe` the reason is the positive-integer
    /// check (which was always right); under `grid`/`euler` it is the ownership refusal. The point
    /// of the test is that the ANSWER is uniform — before this, the same value was fatal on one
    /// path and silently discarded on two.
    #[test]
    fn a_zero_trial_budget_is_never_silently_accepted() {
        for name in method_names() {
            let msg = parse_search_flags(&argv(&["--optimizer", name, "--trials", "0"]))
                .expect_err("--trials 0 must be refused under every method");
            assert!(msg.contains("--trials"), "the refusal must name the flag: {msg}");
        }
    }

    /// **The selector's own value-less spelling, and every knob's under its OWNING method.**
    ///
    /// `arg` answers `None` for a trailing bare token, so `--optimizer` with nothing after it read
    /// as "no `--optimizer` flag" and therefore as the GRID: the flag's default silently overruling
    /// the flag. Its two siblings were already covered — bare `--search` by
    /// `the_retired_search_flag_is_refused_in_every_spelling`, `--optimizer=` by `arg` answering
    /// `Some("")` — which is exactly why this one was easy to miss.
    ///
    /// The knob half is the same hole and had the same shape as defect (b): under a NON-owning
    /// method a bare trailing knob was already refused (ownership goes through [`flag_given`]),
    /// while under its OWNER it silently ran the default. Driven from [`METHOD_KNOBS`], so a fourth
    /// method's knob joins by adding a row.
    #[test]
    fn a_value_less_flag_is_refused_rather_than_read_as_absent() {
        let msg = parse_search_flags(&argv(&["--optimizer"]))
            .expect_err("a bare --optimizer must not read as the GRID");
        assert!(msg.contains("--optimizer"), "the refusal must name the flag: {msg}");
        for method in method_names() {
            assert!(msg.contains(method), "…and the methods it takes: {msg}");
        }
        // …and it is refused wherever it sits, not only as the last token: a profile after it is
        // eaten as its VALUE and refused as a bogus method, which is also not a silent grid.
        assert!(parse_search_flags(&argv(&["--optimizer", "my.toml"])).is_err());

        // ⚠ EVERY owner, not the first: a two-owner knob has two arms that could each default it
        // silently, and `--seed` is the one where an owner genuinely has no default to fall back
        // on — a bare `--seed` under `genetic` must refuse as a value-less FLAG, never collapse
        // into `require_seed`'s missing-seed message, because those say different things.
        for (flag, owners) in METHOD_KNOBS {
            for owner in owners.iter() {
                let msg = parse_search_flags(&argv(&["--optimizer", owner, flag]))
                    .expect_err("a value-less knob under its own method must not default silently");
                assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
                assert!(
                    msg.contains("no value"),
                    "…and say the flag was written WITHOUT ONE, rather than reporting it as \
                     absent: {msg}"
                );
            }
        }
    }

    /// **`genetic` requires `--seed`; `tpe` still defaults one.** The ONE place the two owners of
    /// a single flag deliberately differ, pinned as a PAIR so neither can drift onto the other's
    /// disposition unnoticed — a "consistency" edit that gave genetic a default, or one that made
    /// tpe refuse, reddens here rather than in a shipped wire. [`require_seed`] carries the
    /// argument for the asymmetry.
    ///
    /// ⚠ The `0` row is the reason [`parse_seed`] answers `Option` instead of baking the default
    /// in: a WRITTEN `--seed 0` and an ABSENT `--seed` are different argv that must stay
    /// distinguishable, and a `u64`-returning parser collapses them before any arm can tell.
    #[test]
    fn genetic_requires_a_seed_and_tpe_still_defaults_one() {
        let msg = parse_search_flags(&argv(&["--optimizer", "genetic"]))
            .expect_err("a genetic run with no seed must be refused");
        assert!(msg.contains("--seed"), "the refusal must name the flag: {msg}");
        assert!(msg.contains("genetic"), "…and the method that requires it: {msg}");

        // The operator's value REACHES the config, and a different value reaches it differently —
        // the pin against a hard-coded `GeneticConfig::new(0)` that every other test here passes.
        for seed in [0u64, 7, u64::MAX] {
            let a = ["--optimizer", "genetic", "--seed", &seed.to_string()].map(str::to_string);
            let flags = parse_search_flags(&a).expect("an explicit seed completes the selection");
            assert_eq!(
                flags.method,
                SearchMethod::Genetic(harness::genetic::GeneticConfig::new(seed)),
                "--seed {seed} must be the seed the searcher is built with"
            );
        }

        // …and tpe's absent-seed default is UNCHANGED at 0. Not a preference: it is a shipped wire
        // — `crates/vike-cli/src/cmd/backtest.rs` forwards `--seed` only when the operator wrote
        // one, so a spawned `--optimizer tpe --trials 128` arrives here with no seed at all.
        assert_eq!(
            parse_search_flags(&argv(&["--optimizer", "tpe"])).expect("tpe needs no seed").method,
            SearchMethod::Tpe(vike_ml::tpe::TpeConfig::new(
                vike_ml::tpe::TpeConfig::DEFAULT_TRIALS,
                0
            ))
        );
    }

    /// `--search` is refused in every spelling, including the bare trailing one that `arg` alone
    /// answers `None` for — and including the argv that IS the first defect.
    #[test]
    fn the_retired_search_flag_is_refused_in_every_spelling() {
        for a in [
            vec!["--optimizer", "tpe", "--search", "bogus"],
            vec!["--search", "euler"],
            vec!["--search=grid"],
            vec!["--search"],
        ] {
            let msg = parse_search_flags(&argv(&a)).expect_err("--search is retired");
            assert!(msg.contains("--search"), "the refusal echoes what was written: {msg}");
            assert!(msg.contains("--optimizer"), "…and names the replacement: {msg}");
        }
    }

    /// The euler depth range check, on both edges of the cap. `0` is legal (coarse-grid-only).
    #[test]
    fn an_out_of_range_euler_depth_is_refused_rather_than_clamped() {
        for depth in ["0", "1"] {
            assert!(
                parse_search_flags(&argv(&["--optimizer", "euler", "--euler-depth", depth]))
                    .is_ok()
            );
        }
        let cap = EulerConfig::MAX_DEPTH_CAP.to_string();
        assert!(
            parse_search_flags(&argv(&["--optimizer", "euler", "--euler-depth", &cap])).is_ok()
        );
        let over = (EulerConfig::MAX_DEPTH_CAP + 1).to_string();
        let msg = parse_search_flags(&argv(&["--optimizer", "euler", "--euler-depth", &over]))
            .expect_err("past the cap is a refusal, not a clamp");
        assert!(msg.contains(&cap), "the refusal must name the cap: {msg}");
        assert!(
            parse_search_flags(&argv(&["--optimizer", "euler", "--euler-depth", "abc"])).is_err()
        );
    }

    /// `--rank-by` is NOT a search request: it names how to ORDER results, and its ignore on a
    /// non-sweep profile is documented behaviour this PR deliberately leaves alone.
    #[test]
    fn rank_by_is_parsed_but_does_not_count_as_a_search_request() {
        let flags = parse_search_flags(&argv(&["--rank-by", "return"])).expect("a valid metric");
        assert_eq!(flags.rank, RankChoice::Metric(RankMetric::TotalReturn));
        assert!(!flags.requested, "--rank-by alone must not make a non-sweep profile refuse");
        assert_eq!(
            parse_search_flags(&argv(&["--rank-by", "MULTI"])).expect("case-insensitive").rank,
            RankChoice::Multi
        );
        assert!(parse_search_flags(&argv(&["--rank-by", "bogus"])).is_err());
    }

    /// The positional profile, and the table that keeps a flag's VALUE from being read as one.
    ///
    /// ⚠ The `PROFILE_PATH_VALUED` loop is the point: a valued flag added to this file without a
    /// row there would silently turn its value into a profile path, and that reddens HERE with the
    /// flag named rather than at a user's terminal.
    #[test]
    fn no_flag_value_can_be_mistaken_for_the_positional_profile() {
        assert_eq!(profile_from_args(&argv(&["my.toml"])).unwrap().as_deref(), Some("my.toml"));
        assert_eq!(
            profile_from_args(&argv(&["--profile", "my.toml"])).unwrap().as_deref(),
            Some("my.toml"),
            "the older spelling two `crates/vike-cli/` arms SPAWN with must keep working"
        );
        assert_eq!(profile_from_args(&argv(&[])).unwrap(), None, "none given is not an error here");
        assert_eq!(
            profile_from_args(&argv(&["--json", "my.toml"])).unwrap().as_deref(),
            Some("my.toml"),
            "a valueless toggle consumes nothing"
        );

        // ⚠ `--profile` is skipped: its value IS the profile, which is the case asserted above.
        // Every OTHER row is a flag whose value must never be mistaken for one.
        for flag in PROFILE_PATH_VALUED.iter().filter(|f| **f != "--profile") {
            assert_eq!(
                profile_from_args(&argv(&[flag, "VALUE"])).unwrap(),
                None,
                "{flag}'s VALUE must never be read as the profile"
            );
            // …and the profile still resolves when it sits after that flag's pair.
            assert_eq!(
                profile_from_args(&argv(&[flag, "VALUE", "my.toml"])).unwrap().as_deref(),
                Some("my.toml"),
                "{flag} VALUE my.toml"
            );
        }
    }

    /// Two profiles — in either combination of spellings — is a REFUSAL naming both, never a
    /// precedence rule. Picking a winner silently answers a question the operator did not know they
    /// had asked, which is the same defect class as two searcher selectors.
    #[test]
    fn giving_the_profile_twice_is_refused_and_names_both() {
        let msg = profile_from_args(&argv(&["--profile", "a.toml", "b.toml"]))
            .expect_err("both spellings is a refusal");
        assert!(msg.contains("a.toml") && msg.contains("b.toml"), "names both: {msg}");
        let msg = profile_from_args(&argv(&["a.toml", "b.toml"]))
            .expect_err("two positionals is a refusal");
        assert!(msg.contains("a.toml") && msg.contains("b.toml"), "names both: {msg}");
    }

    // ───────────────────────────────────────────────────── stage 5: the search ARTIFACT's flags

    /// `--keep-trials` takes `none|scalars|returns`, defaults to `scalars`, and `series` is a NAMED
    /// refusal rather than an invalid-value one — an operator who asked for curves must learn WHY
    /// they are not there, not merely that the word was wrong.
    #[test]
    fn keep_trials_defaults_to_scalars_and_refuses_series_by_name() {
        assert_eq!(parse_keep_trials(&argv(&[])).unwrap(), KeepTrials::Scalars);
        assert_eq!(parse_keep_trials(&argv(&["--keep-trials", "none"])).unwrap(), KeepTrials::None);
        assert_eq!(
            parse_keep_trials(&argv(&["--keep-trials=SCALARS"])).unwrap(),
            KeepTrials::Scalars,
            "the inline spelling and the case are both accepted, like every sibling flag"
        );
        assert_eq!(
            parse_keep_trials(&argv(&["--keep-trials", "RETURNS"])).unwrap(),
            KeepTrials::Returns,
            "the mode that arms the trial matrix, case-insensitively like its siblings"
        );

        let msg =
            parse_keep_trials(&argv(&["--keep-trials", "series"])).expect_err("series is refused");
        assert!(msg.contains("equity CURVE"), "says WHAT is still refused: {msg}");
        assert!(
            !msg.contains("expected none|scalars|returns"),
            "and it is NOT the generic invalid-value line, which would read as a typo: {msg}"
        );

        let msg = parse_keep_trials(&argv(&["--keep-trials", "everything"])).expect_err("typo");
        assert!(
            msg.contains("expected none|scalars|returns"),
            "a real typo gets the value line, and it names the NEW roster: {msg}"
        );

        let msg = parse_keep_trials(&argv(&["--keep-trials"])).expect_err("a bare flag");
        assert!(
            msg.contains("written with no value"),
            "a trailing bare flag is refused rather than read as ABSENT — `required_value`'s rule"
        );
    }

    /// ⚠ **A refusal that has become FALSE is worse than no refusal**, so the rewritten `series`
    /// message is pinned on both sides: it must NAME the mode that now does the job an operator was
    /// reaching for, and it must no longer claim the statistics are unreachable because
    /// `row_from_outcome` drops the curve — which stopped being the whole truth the moment
    /// `harness::sweep::ReturnBuckets` reached into that same function.
    #[test]
    fn the_series_refusal_names_what_is_now_possible() {
        let msg = parse_keep_trials(&argv(&["--keep-trials", "series"])).expect_err("refused");
        assert!(msg.contains("--keep-trials returns"), "points at the mode that works: {msg}");
        assert!(
            msg.contains("PBO") && msg.contains("deflated Sharpe"),
            "…and says what that mode produces, which is why series was wanted: {msg}"
        );
        assert!(
            msg.contains("overfit.pbo"),
            "…and the gateable KEY, so a CI step can act on it: {msg}"
        );
        assert!(
            !msg.contains("row_from_outcome"),
            "the old argument — 'the curve is dropped before a row exists, so this is impossible' \
             — is no longer true and must not be repeated: {msg}"
        );
        assert!(
            msg.contains(&harness::sweep::ReturnBuckets::DEFAULT_BUCKETS.to_string())
                && msg.contains(&harness::sweep::DEFAULT_SWEEP_THREADS.to_string()),
            "both numbers are INTERPOLATED from their consts, never typed: {msg}"
        );
    }

    /// ⚠ **`returns` writes the same LEDGER as `scalars`, and a resume must know that.** The
    /// resume check keys on [`KeepTrials::keeps_ledger`] rather than on `== Scalars`; if it went
    /// back to comparing spellings, a `returns` search would be refused with "it kept no ledger",
    /// which is false and whose only stated remedy is re-running hours of work.
    #[test]
    fn returns_keeps_a_ledger_and_only_none_does_not() {
        assert!(!KeepTrials::None.keeps_ledger());
        assert!(KeepTrials::Scalars.keeps_ledger());
        assert!(KeepTrials::Returns.keeps_ledger(), "a returns search IS resumable");

        // The recorded spelling round-trips for every mode, and an unknown one is `None` rather
        // than a guess — `reopen_search_run` turns that into its own refusal.
        for mode in [KeepTrials::None, KeepTrials::Scalars, KeepTrials::Returns] {
            assert_eq!(KeepTrials::from_recorded(mode.as_str()), Some(mode));
        }
        assert_eq!(KeepTrials::from_recorded("series"), None);
        assert_eq!(KeepTrials::from_recorded("Scalars"), None, "the DOCUMENT spelling is exact");
    }

    /// ⚠ **Only `returns` retains anything**, which is what makes every other invocation
    /// byte-identical to one from before the mode existed.
    #[test]
    fn only_the_returns_mode_arms_the_capture() {
        assert!(KeepTrials::Returns.return_buckets().is_armed());
        assert_eq!(
            KeepTrials::Returns.return_buckets().buckets(),
            harness::sweep::ReturnBuckets::DEFAULT_BUCKETS
        );
        assert!(!KeepTrials::Scalars.return_buckets().is_armed(), "the DEFAULT pays nothing");
        assert!(!KeepTrials::None.return_buckets().is_armed());
    }

    /// `--keep-trials` and `--resume` both ASK FOR A SEARCH, so a profile with no `[paramscan]` table
    /// refuses instead of silently running one backtest — defect (e)'s rule, widened.
    #[test]
    fn the_new_search_flags_count_as_requesting_a_search() {
        assert!(parse_search_flags(&argv(&["--keep-trials", "none"])).unwrap().requested);
        assert!(parse_search_flags(&argv(&["--resume", "1756000000-1-0"])).unwrap().requested);
        assert!(
            !parse_search_flags(&argv(&["--rank-by", "sharpe"])).unwrap().requested,
            "…and --rank-by still does not, which is its documented behaviour"
        );
        let msg = parse_search_flags(&argv(&["--resume", "  "]))
            .expect_err("an empty id is refused rather than read as a run");
        assert!(msg.contains("--resume"), "names the flag: {msg}");
    }

    /// A sample value for a [`SEARCH_PROPERTY_FLAGS`] member. The ONE place these tests know a
    /// member's grammar; the ROSTER is that array, which the parser and [`run`]'s refusal both
    /// read, so the two cannot drift.
    ///
    /// The catch-all PANICS rather than defaulting, the idiom
    /// `crates/vike-cli/src/cmd/backtest.rs`'s `sample_value_for` uses: a fifth member added
    /// without a grammar here stops the suite with the reason instead of being skipped.
    fn sample_for_property_flag(flag: &str) -> &'static str {
        match flag {
            "--keep-trials" => "scalars",
            "--resume" => "1756000000-1-0",
            "--min-trades" => "50",
            "--progress" => "none",
            other => panic!(
                "{other} joined SEARCH_PROPERTY_FLAGS without a sample value here — add its arm, \
                 do not delete the check"
            ),
        }
    }

    /// ⚠ **EVERY flag that sets `requested` is one the no-`[paramscan]` refusal can NAME**, and
    /// that holds only because the parser and [`run`]'s `asked` line read ONE array.
    ///
    /// Both directions are driven: each [`SEARCH_PROPERTY_FLAGS`] member sets `requested` through
    /// the real parser, and the rendered roster — `--optimizer`, then [`METHOD_KNOBS`], then that
    /// array, which is exactly what [`run`] chains — contains every member, so the sentence cannot
    /// go short. It is the defect stage 5 fixed once BY HAND for `--keep-trials` and `--resume`,
    /// closed structurally this time.
    ///
    /// ⚠ The mutation this fails on, in PRODUCTION: drop `"--min-trades"` from
    /// [`SEARCH_PROPERTY_FLAGS`]. `requested` then stays false for `--min-trades 50`, the first
    /// assertion reddens, and a run on a profile with no `[paramscan]` table would have executed
    /// one ordinary backtest while silently discarding the floor. Dropping `--optimizer` from the
    /// chain below instead reddens the second block.
    #[test]
    fn every_search_requesting_flag_is_one_the_refusal_can_name() {
        for flag in SEARCH_PROPERTY_FLAGS {
            let value = sample_for_property_flag(flag);
            let parsed = parse_search_flags(&argv(&[*flag, value]))
                .unwrap_or_else(|e| panic!("{flag} {value} must parse: {e}"));
            assert!(parsed.requested, "{flag} asks for a search and must set `requested`");
        }

        // The roster `run` renders, assembled the way `run` assembles it.
        let rendered: Vec<&str> = std::iter::once("--optimizer")
            .chain(METHOD_KNOBS.iter().map(|(flag, _)| *flag))
            .chain(SEARCH_PROPERTY_FLAGS.iter().copied())
            .collect();
        assert_eq!(rendered[0], "--optimizer", "the selector leads, as the shipped message does");
        for (flag, _) in METHOD_KNOBS {
            assert!(rendered.contains(flag), "a method knob sets `requested` and must be named");
        }
        for flag in SEARCH_PROPERTY_FLAGS {
            assert!(rendered.contains(flag), "{flag} sets `requested` and must be named");
        }
        assert_eq!(
            rendered.len(),
            1 + METHOD_KNOBS.len() + SEARCH_PROPERTY_FLAGS.len(),
            "the rendered roster is the three sources and nothing else"
        );
    }

    /// **The `--min-trades` door.** The value reaches `search_select::resolve_min_trades` through
    /// the REAL argv parser, in both spellings, and `0` stays the DISARMED answer rather than being
    /// refused — which is the half a sweep matrix depends on (`--min-trades $FLOOR` with `FLOOR=0`
    /// for the control arm).
    ///
    /// ⚠ The mutation this fails on, in PRODUCTION: delete the `--min-trades` read from
    /// [`parse_search_flags`], or stop threading its answer onto [`SearchFlags::floor`]. Every
    /// assertion below then reads `TradeFloor::DISARMED` for an argv that armed a floor of 50 —
    /// which is exactly what this binary did before the door existed, silently.
    #[test]
    fn the_trade_floor_reaches_the_parser_in_both_spellings() {
        for spelling in [vec!["--min-trades", "50"], vec!["--min-trades=50"]] {
            let f = parse_search_flags(&argv(&spelling)).expect("a count parses");
            assert!(f.floor.is_armed(), "{spelling:?} must arm the floor");
            assert_eq!(f.floor.min_trades(), 50, "{spelling:?}");
        }
        assert_eq!(
            parse_search_flags(&argv(&[])).expect("no flags").floor,
            TradeFloor::DISARMED,
            "unwritten is disarmed, which is what makes arming it unconditionally a no-op"
        );
        assert_eq!(
            parse_search_flags(&argv(&["--min-trades", "0"])).expect("0 parses").floor,
            TradeFloor::DISARMED,
            "an explicit 0 IS disarmed — refusing it would force a conditional argv"
        );
        // …and the REFUSAL is the resolver's, naming the flag and what 0 means.
        let e = parse_search_flags(&argv(&["--min-trades", "-1"])).expect_err("not a count");
        assert!(e.contains("--min-trades"), "{e}");
        assert!(e.contains("0 disarms the floor"), "…and which end of the range: {e}");
    }

    /// **The `--progress` door.** Every spelling `ProgressMode::NAMES` offers reaches
    /// `search_select::resolve_progress` through the real parser, in any ASCII case, and a typo is
    /// refused with a sentence that RENDERS the roster.
    ///
    /// ⚠ The mutation this fails on, in PRODUCTION: delete the `--progress` read from
    /// [`parse_search_flags`]. Every mode then resolves to `ProgressMode::Auto` and the first
    /// assertion block reddens on `none`, which is the mode whose whole job is to be different from
    /// the default on a terminal.
    #[test]
    fn the_progress_stream_reaches_the_parser_and_a_typo_is_refused() {
        assert_eq!(
            parse_search_flags(&argv(&[])).expect("no flags").progress,
            ProgressMode::Auto,
            "unwritten is auto — on IF a human is watching, which is not the same as on"
        );
        for name in ProgressMode::NAMES {
            let want = ProgressMode::from_str_ci(name).expect("a NAMES member parses");
            for spelling in [name.to_string(), name.to_ascii_uppercase()] {
                let f = parse_search_flags(&argv(&["--progress", spelling.as_str()]))
                    .unwrap_or_else(|e| panic!("--progress {spelling} must parse: {e}"));
                assert_eq!(f.progress, want, "--progress {spelling}");
            }
            // …and the inline spelling, which `has_flag` alone could never see.
            let inline = format!("--progress={name}");
            let f =
                parse_search_flags(&argv(&[inline.as_str()])).expect("the inline spelling parses");
            assert_eq!(f.progress, want, "{inline}");
        }
        let e = parse_search_flags(&argv(&["--progress", "verbose"])).expect_err("not a mode");
        assert!(e.contains("--progress"), "{e}");
        for name in ProgressMode::NAMES {
            assert!(e.contains(name), "the refusal must offer {name}: {e}");
        }
    }

    /// ⚠ **A value-less observer flag is REFUSED, not read as absent** — [`required_value`]'s whole
    /// argument, applied to the two flags that joined this parser last. A trailing `--min-trades`
    /// is a script's spelling (`--min-trades $FLOOR` with `FLOOR` unset), and reading it as "no
    /// flag" would run the DISARMED default while the operator believed a floor was armed.
    ///
    /// ⚠ The mutation this fails on, in PRODUCTION: swap either `required_value` call in
    /// [`parse_search_flags`] for a bare `arg(args, flag)`. `arg` answers `None` for a trailing
    /// bare token, the parse then succeeds with the default, and both assertions redden.
    #[test]
    fn a_value_less_observer_flag_is_refused_rather_than_read_as_absent() {
        for flag in ["--min-trades", "--progress"] {
            let msg = parse_search_flags(&argv(&[flag]))
                .expect_err("a trailing bare observer flag must not run the default silently");
            assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
            assert!(
                msg.contains("no value"),
                "…and say the flag was written WITHOUT ONE, rather than reporting it as absent: \
                 {msg}"
            );
            // …and the INLINE empty spelling is refused by the RESOLVER instead, because `arg`
            // answers `Some("")` there deliberately — a different message for a different mistake,
            // and neither of them a silent default.
            let inline = format!("{flag}=");
            let msg = parse_search_flags(&argv(&[inline.as_str()]))
                .expect_err("an inline empty value must not run the default either");
            assert!(msg.contains(flag), "the refusal must name the flag: {msg}");
        }
    }

    /// ⚠ **The shared vocabulary's `--keep-trials` roster, held against what THIS parser accepts —
    /// and it was SHORT BY ONE when this test was written.**
    ///
    /// `vike_datahub_client::flag_vocab`'s row declared `["none", "scalars"]` while
    /// [`parse_keep_trials`] has accepted `returns` since the anti-overfitting statistics shipped,
    /// so `flag_vocab::accepts_value("--keep-trials", "returns")` answered FALSE about a spelling
    /// this binary accepts and documents in [`KEEP_TRIALS_EXPECTED`]. Nothing held the two
    /// together: the row is `Route::EngineOnly`, so no client parser exercised it, and the
    /// vocabulary crate cannot name [`KeepTrials`] — it sits BELOW this one.
    ///
    /// ⚠ The mutation this fails on, in PRODUCTION: delete the `"returns"` arm from
    /// [`parse_keep_trials`], or take `"returns"` back out of that `values` row. Both directions
    /// are checked, because both have a live failure mode — a member the parser refuses would let a
    /// client accept a value this binary then rejects after a spawn, and a value the parser accepts
    /// that the roster omits is the defect this test found.
    ///
    /// ⚠ `series` is deliberately NOT asserted as a member: it is refused BY NAME with its own
    /// reason, so it belongs in neither the roster nor this loop. The last assertion pins that its
    /// named refusal still fires rather than falling back to the generic invalid-value line.
    #[test]
    fn the_keep_trials_roster_is_exactly_what_the_parser_accepts() {
        let roster = vike_datahub_client::flag_vocab::value_roster("--keep-trials");
        assert!(!roster.is_empty(), "an empty roster would make this test vacuous");
        for &name in roster {
            let parsed = parse_keep_trials(&argv(&["--keep-trials", name]))
                .unwrap_or_else(|e| panic!("{name} is on the roster and must parse: {e}"));
            assert_eq!(parsed.as_str(), name, "{name} must round-trip to its own spelling");
        }
        // …and the other direction: every spelling the parser accepts is on the roster. Driven off
        // [`KeepTrials::as_str`], which is an exhaustive `match` on the variant, so a fourth mode
        // cannot exist without a spelling here — the idiom
        // `harness::optimize`'s `every_progress_spelling_parses_and_the_roster_is_complete` uses,
        // where the array is what makes the assertion ITERATE and the exhaustive match is what
        // makes a new variant a COMPILE error.
        const ALL_KEEP: [KeepTrials; 3] =
            [KeepTrials::None, KeepTrials::Scalars, KeepTrials::Returns];
        for mode in ALL_KEEP {
            assert!(
                roster.contains(&mode.as_str()),
                "{mode:?} spells {:?}, which the shared vocabulary does not offer — a client would \
                 refuse a value this binary accepts",
                mode.as_str()
            );
        }
        // The LENGTH agreement is what makes the first loop non-vacuous in the other direction: a
        // roster longer than the mode set carries a spelling nothing accepts, and `series` — which
        // is refused BY NAME with its own reason — must not be one of its members.
        assert_eq!(
            roster.len(),
            ALL_KEEP.len(),
            "the roster and the accepted mode set must be the same size; `series` belongs to \
             neither, because it is refused by name"
        );
        let e = parse_keep_trials(&argv(&["--keep-trials", "series"]))
            .expect_err("series is refused by name");
        assert!(e.contains("returns"), "…and the named refusal points at what IS available: {e}");
    }

    /// ⚠ **[`USAGE`] spells every `--progress` mode the parser accepts.** A const cannot call a
    /// function, so that text is the one hand copy of `ProgressMode::NAMES` in this file — and this
    /// is what stops it going stale: a fourth mode added to the roster reddens here rather than
    /// being accepted by a parser whose own help text does not mention it.
    ///
    /// ⚠ The mutation this fails on, in PRODUCTION: add a mode to `ProgressMode::NAMES` (and its
    /// `from_str_ci` arm) without editing the `--progress M` entry in [`USAGE`]. Deleting `json`
    /// from that entry reddens it too.
    ///
    /// The same property for `--optimizer` and `--rank-by` rides on their own rosters one crate
    /// over; this covers the one whose roster lives in this crate and had no usage line at all
    /// until its door landed.
    #[test]
    fn the_usage_spells_every_progress_mode_the_parser_accepts() {
        for name in ProgressMode::NAMES {
            assert!(USAGE.contains(name), "--progress accepts {name} and the usage must offer it");
        }
        assert!(USAGE.contains("--progress"), "the flag itself is advertised");
        assert!(USAGE.contains("--min-trades"), "…and so is its sibling observer");
        assert!(USAGE.contains(LIST_OPTIMIZERS_FLAG), "…and the optimizer listing");
    }

    /// ⚠ **THE OPTIMIZER LISTING IS THREE SURFACES AND ONE SPELLING.** [`run`]'s arm reads
    /// [`LIST_OPTIMIZERS_FLAG`], [`USAGE`] advertises it (asserted just above), and
    /// `vike_datahub_client::flag_vocab` carries the ROW that says which route it is reachable on.
    /// Nothing but this holds the arm's spelling against the vocabulary's, and the failure it
    /// prevents is silent in the worst way: a flag the help text promises, the published vocabulary
    /// describes, and the binary answers "a profile is required" to.
    ///
    /// ⚠ The mutation this fails on, in PRODUCTION: change either spelling — the const here or the
    /// `flag` field of that row — without changing the other.
    ///
    /// It also pins the two facts a triage reads off the row, because a listing that was declared
    /// `Valued` would tell one to eat the next token as its value: on
    /// `backtest --list-optimizers --json` that token is `--json`, and the JSON half of the listing
    /// would stop working.
    #[test]
    fn the_optimizer_listing_flag_is_the_spelling_the_vocabulary_declares() {
        let row =
            vike_datahub_client::flag_vocab::spec(LIST_OPTIMIZERS_FLAG).unwrap_or_else(|| {
                panic!("{LIST_OPTIMIZERS_FLAG} is parsed by `run` and needs a vocabulary row")
            });
        assert_eq!(row.flag, LIST_OPTIMIZERS_FLAG, "one spelling, three surfaces");
        assert_eq!(row.arity, vike_datahub_client::flag_vocab::Arity::Bare, "it names no value");
        assert!(row.values.is_empty(), "a bare switch declares no roster");
        // …and it is NOT a truncation of `--list`, whose row sits beside it: both are in the table
        // now, so this is the first pair in it where one flag's name is a prefix of another's.
        assert_ne!(LIST_OPTIMIZERS_FLAG, "--list");
        assert!(LIST_OPTIMIZERS_FLAG.starts_with("--list"), "…which is why `spec` is exact-match");
    }

    /// ⚠ `--resume` with `--keep-trials none` is refused at ARGV TRIAGE, so the combination that
    /// would rewrite a resumed run's header to say it kept no ledger never reaches a filesystem.
    /// The DEFAULT must not be refused — only the written flag.
    #[test]
    fn resuming_while_keeping_no_trials_is_refused_and_the_default_is_not() {
        let msg =
            parse_search_flags(&argv(&["--resume", "1756000000-1-0", "--keep-trials", "none"]))
                .expect_err("the combination is refused");
        assert!(msg.contains("--resume") && msg.contains("--keep-trials"), "names both: {msg}");
        assert!(msg.contains("search.json"), "…and says what it would have damaged: {msg}");

        assert!(
            parse_search_flags(&argv(&["--resume", "1756000000-1-0"])).is_ok(),
            "a resume with the DEFAULT keep-trials is the ordinary case and must not be refused"
        );
        assert!(
            parse_search_flags(&argv(&["--resume", "1756000000-1-0", "--keep-trials", "scalars"]))
                .is_ok(),
            "…and so is one that writes the flag with the value it already had"
        );
        assert!(
            parse_search_flags(&argv(&["--keep-trials", "none"])).is_ok(),
            "`none` without a resume is untouched — it is how a search keeps no ledger"
        );
    }

    /// ⚠ Every VALUED flag added since ruling 14 made the profile positional must join
    /// [`PROFILE_PATH_VALUED`], or its value is read as that positional. This asserts membership
    /// directly; the matrix in `no_flag_value_can_be_mistaken_for_the_positional_profile` then
    /// covers the behaviour for free, because it iterates that table.
    ///
    /// ⚠ It is DRIVEN from [`SEARCH_PROPERTY_FLAGS`] rather than from a second literal list — every
    /// member of that array is valued, and the array is the one the parser and the refusal already
    /// share, so a fifth search-property flag joins this check by existing.
    ///
    /// ⚠ The mutation this fails on, in PRODUCTION: delete `"--min-trades"` from
    /// [`PROFILE_PATH_VALUED`]. `backtest sweep.toml --min-trades 50` then reads `50` as a second
    /// profile and is refused for giving the profile twice, naming a file nobody typed.
    #[test]
    fn every_new_valued_flag_is_declared_to_the_positional_scanner() {
        for flag in SEARCH_PROPERTY_FLAGS {
            assert!(
                PROFILE_PATH_VALUED.contains(flag),
                "{flag} takes a value, so its VALUE would be read as the positional profile \
                 without a row in PROFILE_PATH_VALUED"
            );
        }
    }

    /// A SUBCOMMAND word is never read as the profile — `data` since ruling 12, `trials` since
    /// stage 5, and the refusal is now table-driven so a third joins by adding a row.
    #[test]
    fn a_subcommand_word_is_never_read_as_the_profile() {
        for (word, shape) in SUBCOMMANDS {
            let msg =
                profile_from_args(&argv(&[word])).expect_err("a subcommand word is not a profile");
            assert!(msg.contains(word), "names the word: {msg}");
            assert!(msg.contains(shape), "and the shape that follows it: {msg}");
            assert!(
                msg.contains("--profile"),
                "and the escape hatch for a file of that name: {msg}"
            );
        }
    }

    /// `--sort` names one field of ONE table, so the parse, the spelling list and the metric key
    /// cannot drift apart.
    #[test]
    fn the_trial_sort_table_is_the_one_authority_for_its_own_fields() {
        assert_eq!(parse_trial_sort(&argv(&[])).unwrap(), TrialSort::Score, "the default");
        for (name, variant, key, _) in TrialSort::ROWS {
            assert_eq!(
                parse_trial_sort(&argv(&["--sort", name])).unwrap(),
                *variant,
                "{name} must parse to its own row's variant"
            );
            assert_eq!(variant.metric().map(|(k, _)| k), *key, "{name}'s metric key");
            assert!(TrialSort::names().contains(name), "{name} must appear in the usage list");
        }
        let msg = parse_trial_sort(&argv(&["--sort", "nonsense"])).expect_err("an unknown field");
        assert!(msg.contains("nonsense") && msg.contains("score"), "names both: {msg}");
    }
}

#[cfg(test)]
mod persist_tests {
    use super::*;

    /// A minimal profile that parses and validates. Kept close to the fixtures
    /// `crates/vike-backtest/src/harness/profile.rs` already carries.
    const PROFILE_TOML: &str = r#"
name = "sma cross"

[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1h"
from = "2026-01-01T00"
to = "2026-02-01T00"

[engine]
cash = 100000.0
fee_rate = 0.001

[strategy]
name = "sma_cross"
"#;

    fn a_result() -> crate::BacktestResult {
        crate::BacktestResult {
            trades: vec![vike_model::Trade {
                entry_price: 100.0,
                exit_price: 110.0,
                size: 1.0,
                pnl: 10.0,
                fees: 0.2,
                entry_ts: 1_756_000_000_000,
                exit_ts: 1_756_000_060_000,
                symbol: "BTCUSDT".to_string(),
                mae: -1.0,
                mfe: 12.0,
                is_long: true,
            }],
            equity_curve: vec![100_000.0, 100_010.0, 100_009.8],
            final_equity: 100_009.8,
            n_trades: 1,
            intrabar_both_hit: 2,
            per_symbol_pnl: vec![("BTCUSDT".to_string(), 10.0)],
            per_symbol_curves: vec![("BTCUSDT".to_string(), vec![0.0, 10.0, 9.8])],
            equity_ts: vec![1_756_000_000_000, 1_756_000_060_000, 1_756_000_120_000],
            stale_deferrals: 3,
            impact_unpriced: 4,
            session_deferrals: 5,
            dropped: vec![("BTCUSDT".to_string(), "volume_cap".to_string(), 2.0, 1.0)],
            below_min_reversals: 6,
            warmup: 20,
            funding_paid: -1.5,
            maker_fills: 0,
            taker_fills: 2,
            fees_paid: 0.2,
        }
    }

    fn a_report(
        profile: &BacktestProfile,
        result: &crate::BacktestResult,
    ) -> harness::BacktestReport {
        harness::BacktestReport::from_result(
            profile.name.clone(),
            result,
            harness::report::periods_per_year(profile),
        )
    }

    /// ⚠ **§13a, as a property.** Everything the run computed and the report could not hold comes
    /// back off disk: the curve WITH its timestamps, the ledger, the per-symbol curves and every
    /// diagnostic counter. Before this, `result` was dropped when `run` returned and nothing saved
    /// could be re-tearsheeted, bootstrapped or compared curve-to-curve.
    #[test]
    fn a_persisted_run_keeps_the_curve_the_ledger_and_every_diagnostic_counter() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let result = a_result();
        let report = a_report(&profile, &result);
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &runs_root,
            data: None,
            fingerprint: None,
            build: None,
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let dir = persist_run(&facts, &result, &report).unwrap();

        let series = runs::read_series(&dir).unwrap();
        assert!(series.is_aligned(), "a persisted series must satisfy its own invariant");
        assert_eq!(series.equity, result.equity_curve);
        assert_eq!(series.equity_ts, result.equity_ts);
        assert_eq!(series.stride, 1, "a three-sample curve is under every cap");
        assert_eq!(series.source_len, 3);
        assert_eq!(series.per_symbol_equity, result.per_symbol_curves);
        assert_eq!(series.diagnostics.warmup, 20);
        assert_eq!(series.diagnostics.intrabar_both_hit, 2);
        assert_eq!(series.diagnostics.stale_deferrals, 3);
        assert_eq!(series.diagnostics.impact_unpriced, 4);
        assert_eq!(series.diagnostics.session_deferrals, 5);
        assert_eq!(series.diagnostics.below_min_reversals, 6);
        assert_eq!(series.diagnostics.dropped.len(), 1);
        assert_eq!(series.diagnostics.dropped[0].reason, "volume_cap");
        assert_eq!(series.diagnostics.dropped[0].symbol, "BTCUSDT");

        let trades = runs::read_trades(&dir).unwrap();
        assert_eq!(trades.source_len, 1);
        assert_eq!(trades.trades.len(), 1);
        assert_eq!(trades.trades[0].pnl, 10.0);
        assert_eq!(trades.trades[0].symbol, "BTCUSDT");
    }

    /// The manifest and the report keep doing exactly what they did — this stage ADDS documents and
    /// changes neither of the two that were already there.
    #[test]
    fn persisting_still_writes_the_manifest_and_the_report_it_always_did() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let result = a_result();
        let report = a_report(&profile, &result);
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &runs_root,
            data: None,
            fingerprint: None,
            build: None,
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let dir = persist_run(&facts, &result, &report).unwrap();
        let manifest = runs::read_manifest(&dir).unwrap();

        assert_eq!(manifest.kind, runs::BACKTEST_RUN_KIND);
        assert_eq!(manifest.produced_by, "backtest");
        // ⚠ RE-DERIVED, not adjusted to whatever came back. 1_756_000_000 unix seconds is
        // 2025-08-24T01:46:40Z: 1_735_689_600 is 2025-01-01T00:00:00Z, the difference is
        // 20_310_400 s = 235 days + 6_400 s, and day 235 of 2025 is 24 August. Python's
        // `datetime.fromtimestamp(1756000000, timezone.utc)` agrees, so `utc_rfc3339` is right and
        // the number this assertion first carried was not.
        assert_eq!(manifest.started_at, "2025-08-24T01:46:40Z");
        assert_eq!(manifest.config.path.as_deref(), Some("profiles/sma.toml"));
        assert_eq!(manifest.config.name.as_deref(), Some("sma cross"));
        assert_eq!(manifest.detail["strategy"], serde_json::json!("sma_cross"));
        assert!(dir.join(runs::REPORT_FILE).is_file(), "the report is still written");
    }

    /// The retention cap, exercised through the real producer rather than through `decimate` alone:
    /// a curve over the cap comes back thinned, says so, and still ENDS where the run ended.
    #[test]
    fn a_curve_over_the_cap_is_persisted_thinned_and_still_ends_where_the_run_ended() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let n = runs::MAX_EQUITY_SAMPLES * 3 + 7;
        let mut result = a_result();
        result.equity_curve = (0..n).map(|i| 100_000.0 + i as f64).collect();
        result.equity_ts = (0..n).map(|i| 1_756_000_000_000 + i as i64 * 60_000).collect();
        result.per_symbol_curves = vec![("BTCUSDT".to_string(), result.equity_curve.clone())];
        result.final_equity = *result.equity_curve.last().unwrap();
        let report = a_report(&profile, &result);
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &runs_root,
            data: None,
            fingerprint: None,
            build: None,
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let dir = persist_run(&facts, &result, &report).unwrap();
        let series = runs::read_series(&dir).unwrap();

        assert!(series.stride > 1, "a curve three times the cap must be thinned");
        assert_eq!(series.source_len, n, "the record must say how long the real curve was");
        assert!(series.equity.len() <= runs::MAX_EQUITY_SAMPLES + 1);
        assert_eq!(
            *series.equity.last().unwrap(),
            result.final_equity,
            "the last sample must be the run's outcome, or a reader deriving total_return from the \
             file disagrees with report.json"
        );
        assert!(series.is_aligned(), "thinning must thin BOTH vectors at the same stride");
        assert_eq!(
            series.per_symbol_equity[0].1.len(),
            series.equity.len(),
            "a per-symbol curve thinned at a different stride would no longer line up by index"
        );
    }

    /// A ledger over the cap is a PREFIX and says so — never a sample, which would make win rate
    /// and profit factor computed from the file fiction.
    #[test]
    fn a_ledger_over_the_cap_is_a_prefix_that_declares_its_true_length() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let mut result = a_result();
        let one = result.trades[0].clone();
        result.trades = (0..runs::MAX_TRADES + 5)
            .map(|i| {
                let mut t = one.clone();
                t.pnl = i as f64;
                t
            })
            .collect();
        result.n_trades = result.trades.len();
        let report = a_report(&profile, &result);
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &runs_root,
            data: None,
            fingerprint: None,
            build: None,
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let dir = persist_run(&facts, &result, &report).unwrap();
        let trades = runs::read_trades(&dir).unwrap();

        assert_eq!(trades.trades.len(), runs::MAX_TRADES);
        assert_eq!(trades.source_len, runs::MAX_TRADES + 5);
        assert_eq!(trades.trades[0].pnl, 0.0, "a prefix keeps the FIRST trades, in order");
        assert_eq!(trades.trades[1].pnl, 1.0);
    }

    /// Persisting is ADDITIVE, never a new way to fail: a runs root that cannot be created comes
    /// back as a message naming the path, so the caller prints the numbers it already computed.
    #[test]
    fn a_runs_root_that_cannot_be_created_is_reported_rather_than_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let blocked = tmp.path().join("blocked");
        std::fs::write(&blocked, b"a file where the runs directory would go").unwrap();
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let result = a_result();
        let report = a_report(&profile, &result);
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &blocked,
            data: None,
            fingerprint: None,
            build: None,
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let why = persist_run(&facts, &result, &report).unwrap_err();

        assert!(why.contains("blocked"), "the message must name the path: {why}");
    }

    /// The bar lane resolves ONE id per symbol, sub-partitioned by the bar step — which is a real
    /// path segment in this store, not a column.
    #[test]
    fn a_bar_profile_fingerprints_one_bar_series_per_symbol_at_its_interval() {
        let toml =
            PROFILE_TOML.replace(r#"symbols = ["BTCUSDT"]"#, r#"symbols = ["BTCUSDT", "ETHUSDT"]"#);
        let profile = BacktestProfile::from_toml_str(&toml).unwrap();

        let ids = run_fingerprint::planned_series_ids(&profile, &[]);

        assert_eq!(ids.len(), 2, "one per symbol: {ids:?}");
        assert!(ids.iter().all(|i| i.kind == "bar"));
        assert!(ids.iter().all(|i| i.interval.as_deref() == Some("1h")));
        assert!(ids.iter().any(|i| i.symbol == "BTCUSDT"));
        assert!(ids.iter().any(|i| i.symbol == "ETHUSDT"));
    }

    /// A whole-lane tick series reads THREE kinds, because that is what the replay loader's own
    /// `SeriesKind::Tick` admits — and a fingerprint that named only one would claim a run depended
    /// on less data than it did.
    #[test]
    fn a_whole_lane_tick_profile_fingerprints_every_lane_the_loader_admits() {
        let toml = PROFILE_TOML
            .replace(r#"kind = "bar""#, r#"kind = "tick""#)
            .replace(r#"venue = "binance""#, r#"venue = "polymarket""#);
        let profile = BacktestProfile::from_toml_str(&toml).unwrap();

        let ids = run_fingerprint::planned_series_ids(&profile, &[]);
        let kinds: Vec<&str> = ids.iter().map(|i| i.kind.as_str()).collect();

        assert_eq!(ids.len(), 3, "quote + trade + book: {ids:?}");
        for want in ["quote", "trade", "book"] {
            assert!(kinds.contains(&want), "the {want} lane is missing: {kinds:?}");
        }
        assert!(ids.iter().all(|i| i.interval.is_none()), "a tick series has no bar step");
    }

    /// The fingerprint reaches the manifest's kind-specific subtree, versioned, with the window and
    /// the store the run actually read — so a reader holding the file alone can say which bytes
    /// produced these numbers.
    #[test]
    fn the_data_fingerprint_lands_under_the_manifests_detail_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let result = a_result();
        let report = a_report(&profile, &result);
        let data = run_fingerprint::DataFingerprint {
            schema: run_fingerprint::DATA_FINGERPRINT_SCHEMA,
            store: tmp.path().display().to_string(),
            from_ms: Some(1_756_000_000_000),
            to_ms: Some(1_756_999_000_000),
            series: vec![run_fingerprint::SeriesFingerprint {
                id: vike_data::SeriesId::per_symbol(
                    "bar",
                    "binance",
                    "BTCUSDT",
                    Some("1h".to_string()),
                ),
                coverage: Some(vike_data::SeriesCoverage {
                    first_ts: 1_756_000_000_000,
                    last_ts: 1_756_999_000_000,
                    rows: 277,
                    bytes: 4_096,
                    parts: 3,
                    dates: 2,
                }),
                commits: vec!["binance:BTCUSDT:1h:0-1".to_string()],
                commits_len: 1,
            }],
        };
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &runs_root,
            data: Some(&data),
            fingerprint: None,
            build: None,
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let dir = persist_run(&facts, &result, &report).unwrap();
        let manifest = runs::read_manifest(&dir).unwrap();

        let fp = &manifest.detail["data"]["fingerprint"];
        assert_eq!(fp["schema"], serde_json::json!(1));
        assert_eq!(fp["series"][0]["coverage"]["rows"], serde_json::json!(277));
        assert_eq!(fp["series"][0]["commits"][0], serde_json::json!("binance:BTCUSDT:1h:0-1"));
        assert_eq!(
            manifest.detail["data"]["interval"],
            serde_json::json!("1h"),
            "the fingerprint NESTS under `data` and does not displace what was already there"
        );
    }

    /// The run directory holds the config VERBATIM — comments, ordering and all — which is what a
    /// later `promote` or a re-run needs, and what the input address was taken over.
    #[test]
    fn a_persisted_run_holds_the_config_text_that_drove_it_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let result = a_result();
        let report = a_report(&profile, &result);
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &runs_root,
            data: None,
            fingerprint: None,
            build: None,
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let dir = persist_run(&facts, &result, &report).unwrap();

        assert_eq!(runs::read_config_toml(&dir).unwrap(), PROFILE_TOML);
    }

    /// The address reaches BOTH places it has to: the manifest, which is the authority a `diff` or
    /// a `gate` compares on, and the directory NAME, which is what a human scanning
    /// `user_data/runs/` reads.
    #[test]
    fn a_run_with_an_address_carries_it_in_the_manifest_and_in_its_directory_name() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let result = a_result();
        let report = a_report(&profile, &result);
        let addr = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &runs_root,
            data: None,
            fingerprint: Some(addr),
            build: None,
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let dir = persist_run(&facts, &result, &report).unwrap();
        let manifest = runs::read_manifest(&dir).unwrap();

        assert_eq!(
            manifest.fingerprint.as_deref(),
            Some(addr),
            "the manifest carries the WHOLE digest — the name only carries a prefix"
        );
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("1756000000-9f86d081884c7d65-"), "{name}");
    }

    /// A producer that CAN name its build fills the common `git_sha` field and nests the rest,
    /// because the sha is the one fact a listing renders a column from and the rest is detail.
    #[test]
    fn a_run_from_a_binary_that_knows_its_build_records_the_commit_and_the_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let result = a_result();
        let report = a_report(&profile, &result);
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &runs_root,
            data: None,
            fingerprint: None,
            build: Some(runs::BuildStamp {
                git_sha: Some("62ccdd8e"),
                summary: Some(
                    "git 62ccdd8e clean, built 2026-08-09T09:41:07Z, rustc 1.96.0, \
                     x86_64-unknown-linux-gnu",
                ),
            }),
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let dir = persist_run(&facts, &result, &report).unwrap();
        let manifest = runs::read_manifest(&dir).unwrap();

        assert_eq!(manifest.git_sha.as_deref(), Some("62ccdd8e"));
        assert!(
            manifest.detail["build"].as_str().unwrap().contains("rustc 1.96.0"),
            "the full stamp nests: {}",
            manifest.detail["build"]
        );
    }

    /// A producer that CANNOT name its build still writes both keys as `null`, for the reason
    /// `RunManifest::git_sha`'s own doc gives: "does not know" and "wrote no such field" are
    /// different answers, and the standalone `backtest` binary is the common path on Linux.
    #[test]
    fn a_run_from_a_binary_that_cannot_name_its_build_writes_null_rather_than_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let runs_root = tmp.path().join("runs");
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let result = a_result();
        let report = a_report(&profile, &result);
        let facts = BacktestRunFacts {
            profile: &profile,
            profile_path: "profiles/sma.toml",
            profile_toml: PROFILE_TOML,
            store_root: tmp.path(),
            runs_root: &runs_root,
            data: None,
            fingerprint: None,
            build: None,
            started_at: 1_756_000_000,
            finished_at: 1_756_000_012,
        };

        let dir = persist_run(&facts, &result, &report).unwrap();
        let text = std::fs::read_to_string(dir.join(runs::MANIFEST_FILE)).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(raw["git_sha"], serde_json::Value::Null);
        assert_eq!(raw["detail"]["build"], serde_json::Value::Null);
    }

    /// ⚠ **Stride `0` means KEEP NOTHING, and the companion vectors must agree with `decimate`
    /// about it.** `decimate(_, 0)` returns `(vec![], 0)`; `keep_at_stride` once spelled its fast
    /// path `stride <= 1` and so returned the WHOLE vector for the same stride. The result is a
    /// document THIS BUILD wrote that fails its own `is_aligned` invariant — and a
    /// `--keep-series none`-shaped flag, whose whole purpose is to suppress the series, writing a
    /// LARGER file than keeping it. Reverting either clause below turns this red.
    #[test]
    fn a_stride_of_zero_keeps_nothing_in_the_companion_vectors_too() {
        let curve = vec![10_000.0, 10_100.0, 9_950.0];
        let ts = vec![1_756_000_000_000i64, 1_756_000_060_000, 1_756_000_120_000];

        let (equity, stride) = runs::decimate(&curve, 0);
        assert!(equity.is_empty(), "decimate's own contract");
        assert_eq!(stride, 0, "...and the stride it reports for it");

        assert!(
            keep_at_stride(&ts, stride).is_empty(),
            "a timestamp vector kept at stride 0 while the equity was dropped is a MISALIGNED \
             document, written by this build"
        );
        assert!(keep_at_stride(&curve, stride).is_empty(), "and the same for a per-symbol curve");

        // The property those two add up to, asserted through the type that declares it.
        let series = runs::RunSeries {
            schema: runs::SERIES_SCHEMA,
            equity,
            equity_ts: keep_at_stride(&ts, stride),
            per_symbol_equity: vec![("BTCUSDT".to_string(), keep_at_stride(&curve, stride))],
            stride,
            source_len: curve.len(),
            diagnostics: runs::RunDiagnostics::default(),
        };
        assert!(series.is_aligned(), "a suppressed series must still satisfy its own invariant");
        assert_eq!(series.source_len, 3, "...and must still say what the run actually produced");
    }

    /// The other end of the same clause, so the fix cannot be "return empty always": stride 1 is
    /// the EXACTNESS claim and must keep every sample.
    #[test]
    fn a_stride_of_one_keeps_every_companion_sample() {
        let ts = vec![1i64, 2, 3];

        assert_eq!(keep_at_stride(&ts, 1), ts);
        assert_eq!(keep_at_stride::<i64>(&[], 1), Vec::<i64>::new());
        assert_eq!(keep_at_stride::<i64>(&[], 0), Vec::<i64>::new(), "empty is empty either way");
    }

    /// ⚠ **ABSENT and EMPTY are different answers, and until this the only producer in the tree
    /// could never say ABSENT.** `series_facts` folds a manifest, and `read_manifest` maps
    /// `NotFound` to an EMPTY manifest — so a series the store has never held came back as
    /// `Ok(SeriesCoverage::default())`, `Some(all-zeros)`, rendering `coverage 0 0 0 0`. The
    /// `coverage: None` arm and the `coverage missing` rendering were both unreachable, and the
    /// test that "proved" the distinction built BOTH states by hand. Driven here through the real
    /// collector against a real (empty) store, which is the thing that was never true.
    /// ⚠ **A wire-routed run must not record a DIRECTORY, and the failure mode is silent.** The
    /// local ladder still resolves a root on that arm — the fallback needs one — so the old
    /// `store_root.display()` would have written this box's default path into the one document a
    /// later reader uses to reproduce the run, naming a directory nothing opened. Nothing would
    /// have failed; the record would simply have been false.
    ///
    /// Driven through the REAL collector with the REAL label renderer, not by asserting the two
    /// strings match: composing them here is the whole claim.
    #[test]
    fn a_wire_run_records_the_datahub_rather_than_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let store = DataFusionHist::open(&root).unwrap();
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();

        let label = super::HistoryRoute::Wire("<host>:7878".to_string()).label(&root);
        let fp =
            collect_data_fingerprint(Some(&store as &dyn HistStore), &profile, &label).unwrap();

        assert_eq!(fp.store, "the datahub at <host>:7878");
        assert!(
            !fp.store.contains(&root.display().to_string()),
            "the record named a local directory the run never opened: {}",
            fp.store
        );
    }

    #[test]
    fn a_series_the_store_does_not_hold_is_recorded_as_missing_rather_than_as_all_zeros() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let store = DataFusionHist::open(&root).unwrap();
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();

        let fp = collect_data_fingerprint(
            Some(&store as &dyn HistStore),
            &profile,
            &root.display().to_string(),
        )
        .unwrap();

        assert_eq!(fp.series.len(), 1, "one bar series: {:?}", fp.series);
        assert!(
            fp.series[0].coverage.is_none(),
            "an empty store HOLDS no series — `Some(all-zeros)` would be the old silent answer: \
             {:?}",
            fp.series[0]
        );
        assert!(
            fp.canonical().contains("coverage missing"),
            "...and the address must SAY so rather than rendering a zero row: {}",
            fp.canonical()
        );
        assert_ne!(
            fp.canonical(),
            {
                let mut empty = fp.clone();
                empty.series[0].coverage = Some(vike_data::SeriesCoverage::default());
                empty.canonical()
            },
            "absent and empty must not address the same"
        );
    }

    /// ⚠ **THE GROUPED-STORE HOLE.** All three tick kinds are `grouped: true` and the readers union
    /// every `group=` directory under `kind=…/venue=…`, so on a grouped store every row the run
    /// replays lives in the group and the per-symbol path does not exist. Naming only the
    /// per-symbol id made the data half of the address a CONSTANT — a backfill could add a month
    /// and the address would not move.
    #[test]
    fn a_tick_profile_fingerprints_the_grouped_series_the_reader_actually_unions() {
        let toml = PROFILE_TOML
            .replace(r#"kind = "bar""#, r#"kind = "tick""#)
            .replace(r#"venue = "binance""#, r#"venue = "polymarket""#);
        let profile = BacktestProfile::from_toml_str(&toml).unwrap();
        let held = vec![
            vike_data::SeriesId::grouped("book", "polymarket", "btc-5m"),
            vike_data::SeriesId::grouped("quote", "polymarket", "btc-5m"),
            // A different VENUE — the reader lists groups under `kind=…/venue=…`, so this one is
            // not in the union and must not be in the address.
            vike_data::SeriesId::grouped("trade", "bybit", "x"),
            // `bar` is `grouped: false` in `store_kind.rs` and `load_bars` reads the per-symbol
            // directory alone, so a `group=` leaf of that kind is not the bar lane's business.
            vike_data::SeriesId::grouped("bar", "polymarket", "never"),
        ];

        let ids = run_fingerprint::planned_series_ids(&profile, &held);

        assert!(ids.contains(&vike_data::SeriesId::grouped("book", "polymarket", "btc-5m")));
        assert!(ids.contains(&vike_data::SeriesId::grouped("quote", "polymarket", "btc-5m")));
        assert!(!ids.iter().any(|i| i.venue == "bybit"), "another venue's group: {ids:?}");
        assert!(!ids.iter().any(|i| i.kind == "bar"), "a bar lane is never grouped: {ids:?}");
        assert_eq!(
            ids.iter().filter(|i| i.group.is_none()).count(),
            3,
            "the three per-symbol lanes are still named: {ids:?}"
        );
    }

    /// Two symbols of one venue pull the SAME grouped directories, and the reader reads each group
    /// once — so the record must name it once too, or a group's rows would be counted twice by
    /// anyone folding the fingerprint.
    #[test]
    fn one_grouped_series_is_named_once_however_many_symbols_pull_it() {
        let toml = PROFILE_TOML
            .replace(r#"kind = "bar""#, r#"kind = "tick""#)
            .replace(r#"symbols = ["BTCUSDT"]"#, r#"symbols = ["A", "B", "C"]"#);
        let profile = BacktestProfile::from_toml_str(&toml).unwrap();
        let held = vec![vike_data::SeriesId::grouped("quote", "binance", "g1")];

        let ids = run_fingerprint::planned_series_ids(&profile, &held);

        assert_eq!(
            ids.iter().filter(|i| i.group.as_deref() == Some("g1")).count(),
            1,
            "three symbols, one group, one row: {ids:?}"
        );
    }

    /// A bar profile takes no grouped series even when the store holds one of that kind — the
    /// symmetry that keeps the tick fix from widening the bar lane's address.
    #[test]
    fn a_bar_profile_takes_no_grouped_series_even_when_the_store_holds_one() {
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();
        let held = vec![vike_data::SeriesId::grouped("bar", "binance", "g1")];

        let ids = run_fingerprint::planned_series_ids(&profile, &held);

        assert_eq!(ids.len(), 1, "{ids:?}");
        assert!(ids[0].group.is_none());
    }

    /// The collector-level FLOOR for the commit bound: whatever the store held, every series it
    /// produces obeys [`run_fingerprint::MAX_COMMIT_KEYS`] and its true count is never below the
    /// prefix it kept.
    ///
    /// ⚠ **This does NOT prove the truncation, and saying so is the point.** The bound binds only
    /// on a log of hundreds of keys, which needs a store a unit test cannot cheaply build — so this
    /// passed with the truncation REMOVED (measured: the whole 819-test suite did). The truncation
    /// itself is proved on the pure primitive, `run_fingerprint`'s
    /// `a_commit_log_over_the_bound_becomes_a_prefix_that_declares_its_true_length`, which is why
    /// that primitive is a function rather than two lines here.
    #[test]
    fn every_series_the_collector_produces_obeys_the_commit_bound() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("store");
        let store = DataFusionHist::open(&root).unwrap();
        let profile = BacktestProfile::from_toml_str(PROFILE_TOML).unwrap();

        let fp = collect_data_fingerprint(
            Some(&store as &dyn HistStore),
            &profile,
            &root.display().to_string(),
        )
        .unwrap();

        assert!(!fp.series.is_empty(), "the floor: a harvest that produced nothing proves nothing");
        for s in &fp.series {
            assert!(
                s.commits.len() <= run_fingerprint::MAX_COMMIT_KEYS,
                "{:?} carries {} keys, over the bound",
                s.id,
                s.commits.len()
            );
            assert!(s.commits_len >= s.commits.len(), "the true count is never below the prefix");
        }
    }
}

#[cfg(test)]
mod history_route_tests {
    //! ⚠ **These cover `vike_datahub_client::route`, which is no longer this crate's code, and
    //! they stayed here because this is the only place they RUN.** The roster lane builds that
    //! crate with DEFAULT features, which compile none of the module; the lanes that do enable
    //! `hist-route` reach it as a DEPENDENCY, and cargo compiles no `#[cfg(test)]` module of a
    //! dependency at all. There is no `-p vike-datahub-client --features hist-route` lane, so
    //! moving these down with the code would have turned all six off while reading green. The
    //! `backtest-datafusion-store` lane (`scripts/ci_feature_suite.sh`) executes them here.
    use std::path::Path;

    // ⚠ These four are named at their new home rather than through `super::`, and two of them
    // have to be: `DATAHUB_ADDR_ENV` and `datahub_addr_for_bin` have no PRODUCTION caller in this
    // file, so a module-level `use` of either would be an unused import in every non-test build
    // and `-D warnings` would refuse the crate.
    use vike_datahub_client::route::{
        DATAHUB_ADDR_ENV, HistoryRoute, datahub_addr_for_bin, history_route,
    };

    /// The price of spelling the name twice — see [`DATAHUB_ADDR_ENV`]'s own note for why the
    /// import that would remove the duplication also removes the DECLARING crate from the sweep.
    /// A drift here would silently point the bins at a variable nobody sets, and every other test
    /// in this file would stay green: they pass the address in directly.
    #[test]
    fn datahub_addr_env_matches_the_config_crate() {
        assert_eq!(DATAHUB_ADDR_ENV, vike_config::config::DATAHUB_ADDR_ENV);
    }

    /// A bare bin's address rung is the environment and nothing else — the shorter ladder
    /// [`datahub_addr_for_bin`] documents. The BLANK case is deliberately NOT special-cased
    /// there: it reaches [`history_route`], which owns that filter for every caller, so a bin
    /// cannot come to disagree with the daemon about what an empty address means.
    #[test]
    fn a_bin_reads_its_address_from_the_sweep_alone() {
        let mut vars = std::collections::HashMap::new();
        assert_eq!(datahub_addr_for_bin(&vars), None, "unset is absent, not a default");
        vars.insert(DATAHUB_ADDR_ENV.to_string(), "<host>:7878".to_string());
        assert_eq!(datahub_addr_for_bin(&vars), Some("<host>:7878"));
        assert_eq!(
            history_route(None, datahub_addr_for_bin(&vars)),
            HistoryRoute::Wire("<host>:7878".to_string()),
            "and it feeds the SAME route decision the compute daemon uses"
        );
    }

    /// What the `cheap_np` bins PRINT. The wire arm must not leak a path: the local root it is
    /// handed is THIS box's resolved default, and naming it would be a guess about the SERVER's
    /// filesystem — the disclosure bug the label exists to prevent.
    #[test]
    fn the_label_names_a_path_locally_and_an_address_on_the_wire() {
        let root = Path::new("/srv/vike/market_data/hist");
        assert_eq!(HistoryRoute::Local.label(root), "/srv/vike/market_data/hist");
        let wire = HistoryRoute::Wire("<host>:7878".to_string()).label(root);
        assert_eq!(wire, "the datahub at <host>:7878");
        assert!(!wire.contains("market_data"), "the wire arm may name NO local directory");
    }

    /// ⚠ The DEFAULT is the wire, and that is the assertion the whole of
    /// `docs/decisions/0084-only-the-datahub-touches-the-store.md` turns on for this daemon: with
    /// no flag it must not open files, whatever the environment says.
    #[test]
    fn no_flag_is_the_wire() {
        assert_eq!(
            history_route(None, Some("<host>:7878")),
            HistoryRoute::Wire("<host>:7878".to_string())
        );
    }

    /// ...and an UNCONFIGURED address is still the wire, at the default peer — never a silent
    /// fallback to opening the files.
    #[test]
    fn an_unconfigured_address_is_still_the_wire() {
        assert_eq!(
            history_route(None, None),
            HistoryRoute::Wire(vike_config::DEFAULT_DATAHUB_ADDR.to_string())
        );
        // A BLANK line is absent, not an address: dialling "" would be a connect nobody asked for.
        assert_eq!(
            history_route(None, Some("   ")),
            HistoryRoute::Wire(vike_config::DEFAULT_DATAHUB_ADDR.to_string())
        );
    }

    /// `--store` on the LINE is the only opt-out, and it wins over any configured peer.
    #[test]
    fn the_flag_is_the_only_way_local() {
        assert_eq!(
            history_route(Some(Path::new("/tmp/hist")), Some("<host>:7878")),
            HistoryRoute::Local
        );
    }
}
