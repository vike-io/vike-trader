# Golden parity fixtures

The FROZEN oracle for the Rust port. Each round was exported from the Python app
(vike-trader-app) while it was still the source of truth. The exporters
(`scripts/export_r0_fixtures.py` through `scripts/export_r6_fixtures.py`) were deleted with
the Python purge (commit 751de662), so **the committed bytes in this tree are the only
oracle** — regeneration is not a supported operation, and a changed fixture byte is a changed
contract, not a refresh. Each exporter-generated file carries a `manifest`;
`manifest.source_sha` is recorded provenance (the oracle SHA the export ran at) — a few
consumers assert it is present, none compares it to a pinned value.

Floats are IEEE-754 hex bit patterns — `"3ff0000000000000"` is 1.0 — decoded by
`crates/vike-model/src/lib.rs`'s `f64_from_hex_bits`. Parity tiers: pure
{+, −, ×, ÷, sqrt, abs, compare} paths assert **bit-identical** results;
erf/exp/log-derived outputs (`crates/vike-options`) gate at ≤1e-12 relative.
**Never widen a tolerance to make a test pass** — investigate the divergence.

Every file in this tree is replayed by at least one test —
`crates/vike-ops/tests/fixture_consumption_gate.rs` fails CI on any fixture nothing consumes —
so this table cannot silently rot again (its predecessor still named the pre-rename `vt-*`
crates and an r2 round whose consumer had been deleted):

| dir | consumed by |
|---|---|
| `r0/` | `crates/vike-model/tests/fill_parity.rs` (compute_fill), `crates/vike-exec/tests/parity/account_parity.rs` (account_scenarios), `crates/vike-exec/tests/parity/cross_venue_equity_parity.rs` (portfolio_scenario) |
| `r1/` | `crates/vike-backtest/tests/parity/r1_parity.rs` — broker_sim, order_fill_price, order_fill_price_granular, fill_models, fill_resolution, consolidators; linked into the grouped `--test parity` binary by `crates/vike-backtest/tests/parity.rs` |
| `r3/` | `crates/vike-backtest/tests/parity/r3_parity.rs` (fastsim_portfolio — the portfolio kernel; the single-asset kernel and its fixture left with the engine unification) |
| `r4/` | `crates/vike-backtest/tests/parity/r4_parity.rs` (mse_runs — StrategyEngine golden runs; same grouped binary) |
| `r5/` | `crates/vike-exec/tests/parity/r5_parity.rs` (fsm, risk, coid, bus, hub) + `crates/vike-core/tests/runtime_smoke.rs` (hub, replayed through the runtime channels) |
| `r6/` | `crates/bridges/binance/tests/offline/r6_binance_parity.rs` (signer, format, mapper, reconcile), `offline/r6_binance_userdata.rs` (ws_auth), `offline/r6_binance_perp_parity.rs` (perp), `crates/bridges/bybit/tests/offline/r6_bybit_parity.rs` (bybit), `crates/bridges/okx/tests/offline/r6_okx_parity.rs` (okx), `crates/bridges/deribit/tests/offline/r6_deribit_parity.rs` (deribit) |
| `recon/` | `crates/vike-exec/tests/recon/recon_fixtures.rs`'s `recon_golden_fixtures` — auto-discovers every `fixtures/recon/<scenario>/` directory. Hand-written scenarios, not exporter output: no `manifest`, and NEW scenarios may be added (that is authoring a new expectation, not regenerating an oracle) |
| `hl_cohort/` | ⚠ a different, LIVE oracle — not part of the frozen set above; see below. `loader.json` and the README beside it are consumed by `crates/vike-backfill/tests/vikedata_loader_parity.rs`; the other two exports have NO in-repo consumer since 2026-08-25 and each carries an `ORPHANED_EXCEPTIONS` row — see below |


⚠ **`hl_cohort/`'s three exports no longer have one home between them, and that is stated rather
than tidied away.** `crates/vike-research/` was dissolved on 2026-08-25 and its cohort study left
for the author's own `user_data/research/` (R1), taking two of the three parity suites with it as
`#[cfg(test)]` modules — so the feature-matrix and signal exports are replayed from a
BYTE-IDENTICAL copy that travels beside the study in a gitignored tree, and are replayed by nothing
under `crates/`. The bytes stay committed HERE because that tree is gitignored: this is the only
version-controlled copy of the oracle, and deleting it would leave it in git history alone.
`crates/vike-ops/tests/fixture_consumption_gate.rs`'s `ORPHANED_EXCEPTIONS` carries a written row
for each, which is the sanctioned way to say "orphaned, deliberately" — the `r2/` precedent below
is the opposite case, where the consumer left and nothing replaced it anywhere.

The third export, `loader.json`, kept an in-repo consumer because its subject did:
`docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md` moved label normalisation, the
junk filters and the notional snap into the COLLECTOR, so the oracle's evidence about the wire is
gated at `crates/vike-backfill/tests/vikedata_loader_parity.rs`.

There is no `r2/`: its consumer left when `SingleSymbolEngine` was retired (commit 2d821d5f),
and the orphaned files were deleted rather than left to read as coverage.

`fixtures/hl_cohort/` is not frozen and is not the retired vike-trader-app oracle: it holds three
files (`loader.json`, `features.json`, `signal.json`) exported from a SEPARATE, still-live
source — the `analysis.hl_cohort_research` package in the `vike_db_data_jobs` repo, which the
Python purge (`751de662`) never touched, so it stays regenerable there today. None of the "frozen
oracle" rules above apply to it. `fixtures/hl_cohort/README.md` is the authority on the export
command and the source's reproducibility properties — restated nowhere else.

NOTE (hard-won): CPython ≥3.12 builtin `sum()` over floats is **Neumaier-compensated** — ported
sites use `vike_model::py_sum` (`crates/vike-model/src/pysum.rs`), while Python's explicit `+=`
loops stay naive folds. Mirror per-site; a blanket choice diverges either way.
