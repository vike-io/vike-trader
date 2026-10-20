//! vike-backtesthub — the backtesthub daemon and its CLI, over the `vike-backtest` harness.
//!
//! Two shapes of the same program live here. [`backtest_cli::run`] is the backtesthub CLI as a library
//! function (the `backtesthub` bin and the `vike-backend backtesthub` multicall arm both call it, so the
//! DataFusion closure is linked once); under `--addr` it becomes the backtesthub, whose server is
//! [`backtesthub_server`]: the seven verbs that RUN something, over the same hub protocol `vike-datahub`
//! serves the store on. The Studio verbs (`RunSlice`, `RunParamscan`, `RunWalkforward`) and `RunStudy`
//! call `vike-studio-core`'s runners DIRECTLY — this crate sits above it (layer 65 against 35), which
//! is the whole reason it left `vike-backtest`: the old home (layer 30) could not name them, so they
//! were injected as boxed functions (`StudioRunTable`, `StudyRunFn`) that no longer exist.
//!
//! The harness itself (profiles, replay, sweeps and searches, walk-forward, the trial ledger) is
//! `vike-backtest`, a plain library with no feature. Nothing from it is re-exported here: name the
//! owner (`vike_backtest::…`, `vike_sim::…`, `vike_analytics::…`).
//!
//! The pieces:
//!
//! * [`backtesthub_server`] — the daemon.
//! * [`backtest_cli`] — the CLI and the `--addr` daemon arm (`backtest_cli::serve`).
//! * [`research`] — what the Polymarket measurement bins share.
//! * [`starter`] — the published starter dataset, a plain HTTPS download.
//! * [`binutil`] — shared bin glue: the history route the bins dial and the stats provenance.
//! * `src/bin/` — the `backtesthub` shim and the `cheap_np_*` / `sport_taker_run` research runners.

#![warn(unreachable_pub)]

pub mod backtest_cli;
pub mod backtesthub_server;
pub mod binutil;
pub mod research;
pub mod starter;
