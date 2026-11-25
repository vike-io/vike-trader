//! **The run-request vocabulary: the spellings and rosters a backtest request is written in,
//! below every surface that reads or validates them.**
//!
//! A run request crosses three layers that must not disagree about a word: the CLI parses and
//! validates flags without linking an engine crate, the backtesthub and the engine resolve the
//! same words, and the published surface (`cli.json`, the profile asset) renders them. Each roster
//! used to live in the engine (or in the datahub client), with a hand-written copy on the CLI side;
//! they live here, pure and once, so the copy is a read. The sides that share them: the CLI client
//! (`vike-cli`), the standalone engine (`vike-backtesthub`'s `backtest_cli`), the engine library
//! (`vike-backtest`) and the backtest wire (`vike-backtesthub-client`).
//!
//! Pure data and pure functions over `&str` (no I/O, no environment, no clock). The cure for two
//! sides that must not disagree is a vocabulary BELOW both, which is what this crate is.
//!
//! * [`decide`] — `engine.decide` ([`decide::DecideMode`]: sequential or simultaneous) and the
//!   cross-section order each walks;
//! * [`flag_vocab`] — the `backtest` RUN surface's flags, their arity, value rosters and routes;
//! * [`gap`] — `data.on_gap` ([`gap::OnGap`]) and `data.universe` ([`gap::UniverseMode`]), each
//!   with its `NAMES` roster and pure parse;
//! * [`keep_trials`] — `--keep-trials` ([`keep_trials::KeepTrials`] and its words);
//! * [`method_knobs`] — which search METHODS own each method-specific flag
//!   ([`method_knobs::METHOD_KNOBS`]);
//! * [`paramscan`] — the two section spellings a parameter-search grid is written under
//!   ([`paramscan::PARAMSCAN_SECTIONS`]) and the presence rule over them
//!   ([`paramscan::declares_a_grid`]);
//! * [`profile`] — a profile's top-level keys ([`profile::PROFILE_TOP_LEVEL_KEYS`]) and the cash a
//!   run starts with when none is written ([`profile::DEFAULT_RUN_CASH`]);
//! * [`progress`] — `--progress` ([`progress::ProgressMode`] and its `NAMES`);
//! * [`rank_report`] — each `--rank-by` word's report key and direction
//!   ([`rank_report::RANK_METRIC_REPORTS`]);
//! * [`replay`] — bar or tick, the profile's `data.kind` and the Studio slice's
//!   ([`replay::ReplayKind`]);
//! * [`search_methods`] — the parameter-search METHOD names and the default, the `--optimizer`
//!   roster.
//!
//! What stays out: anything that needs an engine type or does I/O. The resolvers that turn these
//! words into a run (`ProfileData::on_gap`, `resolve_progress`, `resolve`), the refusal sentences
//! and the code that switches on the variants are `vike-backtest`'s; this module owns spellings.

pub mod decide;
pub mod flag_vocab;
pub mod gap;
pub mod keep_trials;
pub mod method_knobs;
pub mod paramscan;
pub mod profile;
pub mod progress;
pub mod rank_report;
pub mod replay;
pub mod search_methods;
