//! `parity` — the golden-fixture / second-oracle suite, ONE test binary over what used to be five.
//!
//! **Test-binary consolidation (CI cost, not behavior).** The workspace ships ~276 integration test
//! FILES, and cargo links a separate test BINARY per top-level `tests/*.rs` — vike-backtest was the
//! worst offender at 38. Each former `tests/<name>.rs` here is now `tests/parity/<name>.rs`,
//! included below as a plain module: the tests, their names, their bodies, and their fixtures are
//! byte-unchanged — only the binary they link into. Five link steps become one.
//!
//! Run one by name exactly as before, with the group binary in the `--test` slot:
//!
//! ```sh
//! cargo test -p vike-backtest --test parity                  # the whole group
//! cargo test -p vike-backtest --test parity -- r1_broker_sim  # one test by name, as before
//! ```
//!
//! Members (each keeps its own module doc — read those, not this list, for what each gate proves):
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
//!   `vike-fills`' cost scalars, `crates/vike-model/src/consolidator.rs`,
//!   `crates/vike-model/src/fill_trigger.rs`, `crates/vike-model/src/fill.rs`,
//!   `crates/vike-model/src/scalar.rs`, `crates/vike-backtest/src/engine.rs`'s `StrategyEngine`,
//!   `crates/vike-backtest/src/vector_engine.rs`'s `fast_portfolio_backtest`,
//!   `crates/vike-backtest/src/ref_strategies.rs`, `crates/vike-analytics/src/sizing.rs` and
//!   `crates/vike-exec/src/account.rs` — reaches no transcendental and no `powi` at any arity,
//!   only `+ - * /`, which IEEE 754 requires to be correctly rounded. Grep those modules for a
//!   libm-class call before trusting this paragraph; introducing one is what would make these
//!   fixtures platform-sensitive, and nothing else in the suite would say so.
//! - `engine_kernel_parity` — the event engine (`StrategyEngine`) reconciled against the vectorized
//!   kernel (`VectorBacktestEngine`): the workspace's second oracle, as a gate.
//! - `rhai_parity` — the SP1 acceptance gate: a Rhai script and its Rust twin produce a
//!   byte-identical `BacktestResult`.
//!
//! Grouped by KIND, and deliberately conservative: every member here is plain (no `#![cfg]` feature
//! gate, no `#[ignore]`d live/heavy test, no `proptest` regression sidecar, and no process-global
//! mutation — no `set_var`, no `set_current_dir`). That matters because the feature lanes and the
//! hist job run plain `cargo test`, whose harness runs a binary's tests as THREADS in one process:
//! merging a file that mutates process state would turn independent binaries into a race. Files
//! that fail any of those checks were left as their own binary on purpose.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod r1_parity;` would resolve
// against `tests/` (the root's own directory), not `tests/parity/`.
#[path = "parity/engine_kernel_parity.rs"]
mod engine_kernel_parity;
#[path = "parity/r1_parity.rs"]
mod r1_parity;
#[path = "parity/r3_parity.rs"]
mod r3_parity;
#[path = "parity/r4_parity.rs"]
mod r4_parity;
#[path = "parity/rhai_parity.rs"]
mod rhai_parity;
