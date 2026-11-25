//! vike-backtesthub — the backtesthub daemon and its CLI, over the `vike-backtest` harness.
//!
//! Two shapes of the same program live here. [`backtest_cli::run`] is the backtesthub CLI as a library
//! function (the `backtesthub` bin and the `vike-backend backtesthub` multicall arm both call it, so the
//! DataFusion closure is linked once); under `--addr` it becomes the backtesthub, whose server is
//! [`backtesthub_server`]: the seven verbs that RUN something, over the same hub protocol `vike-datahub`
//! serves the store on. The Studio verbs (`RunSlice`, `RunParamscan`, `RunWalkforward`) and `RunStudy`
//! run [`studio_run`], the Studio's run pipeline, over `vike-studio-core`'s vocabulary — this crate sits
//! above it (layer 65 against 35), which is the whole reason it left `vike-backtest`: the old home
//! (layer 30) could not name it, so the runners were injected as boxed functions (`StudioRunTable`,
//! `StudyRunFn`) that no longer exist. The pipeline itself was `vike-studio-core`'s until 2026-11-10
//! and moved here because it runs the harness and that crate is what the desktop links: the thin
//! client links no engine (`docs/decisions/0078-one-backtest-path-studios-local-backend-is-deleted.md`'s
//! amendment of that day).
//!
//! The harness itself (profiles, replay, sweeps and searches, walk-forward, the trial ledger) is
//! `vike-backtest`, a plain library with no feature. Nothing from it is re-exported here: name the
//! owner (`vike_backtest::…`, `vike_simulator::…`, `vike_analytics::…`).
//!
//! The pieces:
//!
//! * [`backtesthub_server`] — the daemon.
//! * [`backtest_cli`] — the CLI and the `--addr` daemon arm (`backtest_cli::serve`).
//! * [`studio_run`] — the Studio's run pipeline and the `*_local` entries the daemon serves its
//!   Studio verbs and `RunStudy` with; [`harness_sim`] — the one `StudySim`; [`study_cli`] — the
//!   `vike-study` runner that can FIT.
//! * [`research`] — what the Polymarket measurement bins share.
//! * [`binutil`] — shared bin glue: the history route the bins dial and the stats provenance.
//! * `src/bin/` — the `backtesthub` and `vike-study` shims and the `cheap_np_*` research runners (the
//!   two that drive a shipped strategy file, `cheap_np_run` and `sport_taker_run`, are examples of
//!   `vike-strategy-lab`, which compiles the files).

#![warn(unreachable_pub)]

pub mod backtest_cli;
pub mod backtesthub_server;
// The bars of past UTC days this daemon keeps after a run read them from the datahub, valid per day.
pub mod bar_cache;
pub mod binutil;
// The one implementation of `vike_user_research::StudySim`: a study's one backtest, through the harness.
pub mod harness_sim;
// The NAMED RUN's server half (`docs/decisions/0064-a-named-run-carries-no-source.md`): no strategy is
// compiled into this server (plan 2b), so an armed lane resolves no name.
pub mod named_run;
pub mod research;
// What a run's INPUTS were, and the ADDRESS over them: the backtest producer's own answer, written into
// the run manifest by `backtest_cli`.
pub mod run_fingerprint;
// The Studio's SUMMARIZED runs: the summary a `Request::Summarized` answers, and the trades and points
// this daemon holds for `Request::ReadTrades` / `Request::ReadPoint`.
mod run_summary;
// The versioned strategy SDK's source served to the Studio (decision 0296, verdict 3): `ListSdks` and
// `ReadSdkFile`, over the SDK reader the composition root hands in (`sdk_source::use_reader`).
pub mod sdk_source;
// A Rust strategy file's run goes to a sandboxed child (`vike-backend strategy-sandbox backtest`): the parent's
// relay and the child's half, `strategy_sandbox::serve_child`.
pub mod strategy_sandbox;
// The Studio's run pipeline (a Studio run IS a profile run) and the `*_local` entries the server answers
// `RunSlice`/`RunParamscan`/`RunWalkforward`/`RunStudy` with.
pub mod studio_run;
// The `vike-study` runner that can FIT, as a library function (the bin and `vike-backend study` call it).
pub mod study_cli;
// What this server answers about its studies (studies run only on the server, the owner's ruling of
// 2026-11-19): `RunStudy`'s report.
mod study_files;
// A research study FILE's run goes to a sandboxed child (`vike-backend strategy-sandbox study`), as a
// strategy file's does: the parent's runner and the child's half, `study_sandbox::serve_child`.
pub mod study_sandbox;
/// The loopback backtesthub several crates' tests spawn (`test-support` only: test code).
#[cfg(feature = "test-support")]
pub mod test_support;
