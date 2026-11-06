//! `integration` — vike-sim's grouped integration suites as ONE test binary: the golden-fixture /
//! second-oracle suite (`tests/parity/`), the fill-realism knob gates (`tests/fills/`), the
//! backtest-side law gates (`tests/laws/`) and the live-`RiskGate` <-> `SimBroker` reconciliation
//! (`tests/riskgate_simbroker_parity/`). They were four binaries (`parity`, `fills`, `laws`,
//! `riskgate_simbroker_parity`) of one configuration (no feature but the self dev-dependency's
//! `test-support`), so one link where four were.
//!
//! **Test-binary consolidation (CI cost, not behavior).** The workspace ships hundreds of integration
//! test FILES, and cargo links a separate test BINARY per top-level `tests/*.rs`. Each former
//! `tests/<name>.rs` of the three groups is `tests/{parity,fills,laws}/<name>.rs`, included below as a
//! plain module: the tests, their names, their bodies and their fixtures are byte-unchanged, only the
//! binary they link into. Run one by name exactly as before, with this binary in the `--test` slot:
//!
//! ```sh
//! cargo test -p vike-sim --test integration                          # every suite here
//! cargo test -p vike-sim --test integration -- broker_sim_bit_parity  # one test by name, as before
//! ```
//!
//! The group members are FLAT modules, so their names did not change; `riskgate_simbroker_parity` keeps
//! its own module (its children share items of its root through `use super::*`), so its tests are named
//! `riskgate_simbroker_parity::<child>::<test>`. Its children stayed where they were, so a proptest
//! failure seed of `riskgate_simbroker_parity/props.rs` would still sit beside it as
//! `riskgate_simbroker_parity/props.proptest-regressions` (proptest derives the file from the source).
//!
//! # The parity members (each keeps its own module doc: read those for what each gate proves)
//!
//! - `r1_parity` / `r3_parity` / `r4_parity` — the R1/R3/R4 golden gates against the FROZEN
//!   fixtures under `fixtures/r{1,3,4}/` (paths are `CARGO_MANIFEST_DIR`-anchored, so the move is
//!   inert). Those bytes were exported from the Python app and every exporter was deleted by
//!   `751de662`, so the committed bytes ARE the oracle: what these three prove is that THIS code
//!   has not changed its arithmetic unnoticed, NOT that anything still agrees with a running
//!   Python — `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`. (All three
//!   described themselves as "parity vs the Python oracle" until 2026-08-28; the fixtures'
//!   provenance is unchanged, the ongoing-comparison reading was never true after `751de662`.)
//!   ⚠ Their bit-exactness is portable by CONSTRUCTION, which is NOT the usual reason a pinned
//!   constant survives a platform change.
//!   `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md` is about a
//!   constant that holds only because two platform libms agreed on the inputs it happened to
//!   sample. That hazard does not reach these three: as of 2026-08-28 every module they drive —
//!   `vike-fills`' cost scalars, `crates/vike-marketdata/src/consolidator.rs`,
//!   `crates/vike-model/src/orders/fill_trigger.rs`, `crates/vike-model/src/money/fill.rs`,
//!   `crates/vike-model/src/scalar.rs`, `crates/vike-sim/src/engine.rs`'s `StrategyEngine`,
//!   `crates/vike-sim/src/vector_engine.rs`'s `fast_portfolio_backtest`,
//!   `crates/vike-sim/src/ref_strategies.rs`, `crates/vike-analytics/src/sizing.rs` and
//!   `crates/vike-exec/src/account.rs` — reaches no transcendental and no `powi` at any arity,
//!   only `+ - * /`, which IEEE 754 requires to be correctly rounded. Grep those modules for a
//!   libm-class call before trusting this paragraph; introducing one is what would make these
//!   fixtures platform-sensitive, and nothing else in the suite would say so.
//! - `engine_kernel_parity` — the event engine (`StrategyEngine`) reconciled against the vectorized
//!   kernel (`VectorBacktestEngine`): the workspace's second oracle, as a gate.
//! - `rhai_parity` — the SP1 acceptance gate: a Rhai script and its Rust twin produce a
//!   byte-identical `BacktestResult`.
//! - `multi_symbol_read_parity` — the READ half of the multi-symbol lane: `position`/`price`/`bars`
//!   answer about the named symbol in BOTH the backtest engine and the live core.
//!
//! # The fills members
//!
//! The opt-in FILL-REALISM knob gates and the fill-path gates beside them: `impact_slippage` (bar lane),
//! `impact_lanes` (the TICK and L2 replay lanes), `latency_model`, `queue_model`, `stale_price_wait`,
//! `properties_fills`, `fee_schedule_engine`, `equity_sampling`, `knob_composition` (the knobs COMPOSED,
//! so a pair cannot silently cancel out), `fill_gate_sequence` (ONE gate sequence for the frozen and the
//! queued tick lanes), `funding_source`, `book_replay`, `maker_backtest` (`vike-mm`'s `SpreadMaker`
//! through `SimBroker`'s `HftBroker` impl).
//!
//! # The laws members
//!
//! The backtest-side LAW gates: `liquidation_law`, `session_law`, `trigger_law`, `variation_margin`,
//! `tif_expiry_oms`, `option_expiry_settlement`, `resolution_settlement`, `limit_orders_are_gated`,
//! `below_min_reversal_count`, `schedule_time_rule`, `reduce_only_stop`, `paper_tier_preflight`,
//! `post_only_law` (each module doc says what it proves).
//!
//! # What may join, and what stays out
//!
//! Every member is plain: no `#![cfg]` feature gate, no `#[ignore]`d live/heavy test, and no
//! process-global mutation (no `set_var`, no `set_current_dir`). That matters because a plain
//! `cargo test` runs a binary's tests as THREADS in one process: merging a file that mutates process
//! state would turn independent binaries into a race. Three files stay their own binary on purpose
//! (`crates/vike-sim/CLAUDE.md`'s "Test-binary consolidation"): `tests/r7_gate.rs` and
//! `tests/controller_harness.rs` poll the live core against a wall-clock deadline, and
//! `tests/libm_platform_probe.rs` is the per-crate probe decision 0074 names by that path.
//!
//! The tick, bar and calendar builders the members share are compiled once in the lib, behind
//! `test-support`; a member reaches one as `vike_sim::test_support::quote`.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod r1_parity;` would resolve
// against `tests/` (the root's own directory), not `tests/parity/`.

// The golden-fixture / second-oracle suite.
#[path = "parity/engine_kernel_parity.rs"]
mod engine_kernel_parity;
#[path = "parity/multi_symbol_read_parity.rs"]
mod multi_symbol_read_parity;
#[path = "parity/r1_parity.rs"]
mod r1_parity;
#[path = "parity/r3_parity.rs"]
mod r3_parity;
#[path = "parity/r4_parity.rs"]
mod r4_parity;
#[path = "parity/rhai_parity.rs"]
mod rhai_parity;

// The fill-realism knob gates and the fill-path gates.
#[path = "fills/book_replay.rs"]
mod book_replay;
#[path = "fills/equity_sampling.rs"]
mod equity_sampling;
#[path = "fills/fee_schedule_engine.rs"]
mod fee_schedule_engine;
#[path = "fills/fill_gate_sequence.rs"]
mod fill_gate_sequence;
#[path = "fills/funding_source.rs"]
mod funding_source;
#[path = "fills/impact_lanes.rs"]
mod impact_lanes;
#[path = "fills/impact_slippage.rs"]
mod impact_slippage;
#[path = "fills/knob_composition.rs"]
mod knob_composition;
#[path = "fills/latency_model.rs"]
mod latency_model;
#[path = "fills/maker_backtest.rs"]
mod maker_backtest;
#[path = "fills/properties_fills.rs"]
mod properties_fills;
#[path = "fills/queue_model.rs"]
mod queue_model;
#[path = "fills/stale_price_wait.rs"]
mod stale_price_wait;

// The backtest-side law gates.
#[path = "laws/below_min_reversal_count.rs"]
mod below_min_reversal_count;
#[path = "laws/limit_orders_are_gated.rs"]
mod limit_orders_are_gated;
#[path = "laws/liquidation_law.rs"]
mod liquidation_law;
#[path = "laws/option_expiry_settlement.rs"]
mod option_expiry_settlement;
#[path = "laws/paper_tier_preflight.rs"]
mod paper_tier_preflight;
#[path = "laws/post_only_law.rs"]
mod post_only_law;
#[path = "laws/reduce_only_stop.rs"]
mod reduce_only_stop;
#[path = "laws/resolution_settlement.rs"]
mod resolution_settlement;
#[path = "laws/schedule_time_rule.rs"]
mod schedule_time_rule;
#[path = "laws/session_law.rs"]
mod session_law;
#[path = "laws/tif_expiry_oms.rs"]
mod tif_expiry_oms;
#[path = "laws/trigger_law.rs"]
mod trigger_law;
#[path = "laws/variation_margin.rs"]
mod variation_margin;

// The live `RiskGate` <-> backtest `SimBroker` reconciliation matrix (with its property arms).
#[path = "riskgate_simbroker_parity/mod.rs"]
mod riskgate_simbroker_parity;
