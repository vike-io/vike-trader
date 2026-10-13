//! **The run-request vocabulary: the spellings and rosters a backtest request is written in,
//! below every surface that reads or validates them.**
//!
//! A run request crosses three layers that must not disagree about a word: the CLI parses and
//! validates flags without linking an engine crate, the compute server and the engine resolve the
//! same words, and the published surface (`cli.json`, the profile asset) renders them. Each roster
//! used to live in the engine (or in the datahub client), with a hand-written copy on the CLI side;
//! they live here, pure and once, so the copy is a read. The sides that share them: the CLI client
//! (`vike-cli`), the standalone engine (`vike-compute`'s `backtest_cli`), the engine library
//! (`vike-backtest`) and the datahub wire (`vike-datahub-client`).
//!
//! Pure data and pure functions over `&str` (no I/O, no environment, no clock). The cure for two
//! sides that must not disagree is a vocabulary BELOW both, which is what this crate is.
//!
//! * [`flag_vocab`] — the `backtest` RUN surface's flags, their arity, value rosters and routes;
//! * [`gap`] — `data.on_gap` ([`gap::OnGap`]) and `data.universe` ([`gap::UniverseMode`]), each
//!   with its `NAMES` roster and pure parse;
//! * [`method_knobs`] — which search METHODS own each method-specific flag
//!   ([`method_knobs::METHOD_KNOBS`]);
//! * [`paramscan`] — the two section spellings a parameter-search grid is written under
//!   ([`paramscan::PARAMSCAN_SECTIONS`]) and the presence rule over them
//!   ([`paramscan::declares_a_grid`]);
//! * [`progress`] — `--progress` ([`progress::ProgressMode`] and its `NAMES`);
//! * [`search_methods`] — the parameter-search METHOD names and the default, the `--optimizer`
//!   roster.
//!
//! What stays out: anything that needs an engine type or does I/O. The resolvers that turn these
//! words into a run (`DataCfg::on_gap`, `resolve_progress`, `resolve`), the refusal sentences
//! and the code that switches on the variants are `vike-backtest`'s; this module owns spellings.

pub mod flag_vocab;
pub mod gap;
pub mod method_knobs;
pub mod paramscan;
pub mod progress;
pub mod search_methods;
