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
`crates/vike-ops/tests/venues/fixture_consumption_gate.rs` fails CI on any fixture nothing consumes —
so this table cannot silently rot again (its predecessor still named the pre-rename `vt-*`
crates and an r2 round whose consumer had been deleted):

| dir | consumed by |
|---|---|
| `r0/` | `crates/vike-model/tests/fill_parity.rs` (compute_fill), `crates/vike-execution/tests/parity/account_parity.rs` (account_scenarios), `crates/vike-execution/tests/parity/cross_venue_equity_parity.rs` (portfolio_scenario) |
| `r1/` | `crates/vike-simulator/tests/parity/r1_parity.rs` — broker_sim, order_fill_price, order_fill_price_granular, fill_models, fill_resolution, consolidators; linked into the grouped `--test integration` binary by `crates/vike-simulator/tests/integration.rs` |
| `r3/` | `crates/vike-simulator/tests/parity/r3_parity.rs` (fastsim_portfolio — the portfolio kernel; the single-asset kernel and its fixture left with the engine unification) |
| `r4/` | `crates/vike-simulator/tests/parity/r4_parity.rs` (mse_runs — StrategyEngine golden runs; same grouped binary) |
| `r5/` | `crates/vike-execution/tests/parity/r5_parity.rs` (fsm, risk, order client id, bus, hub) + `crates/vike-core/tests/runtime_smoke/headless_path.rs` (hub, replayed through the runtime channels) |
| `r6/` | `crates/bridges/binance/tests/offline/r6_binance_parity.rs` (signer, format, mapper, reconcile), `offline/r6_binance_userdata.rs` (ws_auth), `offline/r6_binance_perp_parity.rs` (perp), `crates/bridges/bybit/tests/offline/r6_bybit_parity.rs` (bybit), `crates/bridges/okx/tests/offline/r6_okx_parity.rs` (okx), `crates/bridges/deribit/tests/offline/r6_deribit_parity.rs` (deribit) |
| `recon/` | `crates/vike-execution/tests/recon/reconcile_fixtures.rs`'s `reconcile_golden_fixtures` — auto-discovers every `fixtures/recon/<scenario>/` directory. Hand-written scenarios, not exporter output: no `manifest`, and NEW scenarios may be added (that is authoring a new expectation, not regenerating an oracle) |
| `datahub_wire/` | The `v7_backtest_plane_*.jsonl` pair and the two `v7_wrong_plane_refusals_*.jsonl` files freeze, as exact bytes, what a move of the backtest protocol must not change: `crates/vike-backtesthub-client/tests/backtest_plane_wire_pin.rs`, and the refusals in `crates/vike-datahub/tests/default_build/plane_split.rs` and `crates/vike-backtesthub/tests/backtest_plane.rs`. Beside them, TWO consumers of two kinds. The `v7_paramscan_rename_*.frames` pair ⇒ `crates/vike-backtesthub-client/tests/wire_tag_fixtures.rs` — ⚠ a different KIND of oracle again, and the only BINARY one here: real length-prefixed protocol frames captured from a pre-rename peer, not exporter output and not a value comparison. See below. The `v1_strat_requests.jsonl` / `v1_strat_responses.jsonl` pair ⇒ `crates/vike-datahub-client/tests/strategy_lane_wire.rs`: one JSON frame BODY per line, the strategy lane's two positional frames (`StratOpen`, `StratOpened`), authored with the lane in wave 2 of `vike-gatewayhub` (a golden of THIS build's tags, not a capture of a peer; a new lane verb ships its line or the coverage test refuses it). The `v1_options_refresh_requests.jsonl` / `v1_options_refresh_responses.jsonl` pair ⇒ `crates/vike-datahub-client/tests/options_chain_negotiation.rs`: the options-listing refresh's request, its reply and the error a listing that could not be had is answered with, authored with the verb (the same kind of golden) |
| `hyperliquid_signed/` | TWO files of OPPOSITE kinds, which is why this row is the longest here. `signed_payloads.json` ⇒ `crates/bridges/hyperliquid/tests/signed_payload_fixtures.rs` — ⚠ a STABILITY witness, not an oracle at all, and the one entry in this table that says so on its own face: those `/exchange` bodies were RECORDED from this implementation because neither official Hyperliquid SDK publishes a test vector for `usdClassTransfer` or `approveBuilderFee`. It proves the signed payload did not change; it does not prove it was ever right. `venue_certified_user_signed.json` ⇒ `crates/bridges/hyperliquid/tests/hyperliquid_venue_certified_vectors.rs` — the opposite: `(key, nonce, action) -> signature` triples HYPERLIQUID ITSELF confirmed on testnet, by naming back the address it recovered from each exact signature, and each stored with the date, that address and the venue's verbatim reply. A user-signed action carries no sender field, so that echo is an ECDSA recovery over the venue's OWN EIP-712 digest and the match certifies the whole typed-data construction. ⚠ Its provenance is the PERISHABLE kind — authoritative because the venue confirmed it once, not because anyone published it — so the no-regeneration rule below is not merely as strict here, it is the only thing keeping it a correctness gate: a value refreshed from our own output silently becomes a stability pin. Only another testnet run can move one |
| `hl_cohort/` | ⚠ a different, LIVE oracle — not part of the frozen set above; see below. `loader.json` and the README beside it are consumed by `crates/vike-backfill/tests/vikedata_loader_parity.rs`; the other two exports have NO in-repo consumer since 2026-08-25 and each carries an `ORPHANED_EXCEPTIONS` row — see below |


⚠ **`hl_cohort/`'s three exports no longer have one home between them, and that is stated rather
than tidied away.** `crates/vike-research/` was dissolved on 2026-08-25 and its cohort study left
for the author's own `user_data/research/` (R1), taking two of the three parity suites with it as
`#[cfg(test)]` modules — so the feature-matrix and signal exports are replayed from a
BYTE-IDENTICAL copy that travels beside the study in a gitignored tree, and are replayed by nothing
under `crates/`. The bytes stay committed HERE because that tree is gitignored: this is the only
version-controlled copy of the oracle, and deleting it would leave it in git history alone.
`crates/vike-ops/tests/venues/fixture_consumption_gate.rs`'s `ORPHANED_EXCEPTIONS` carries a written row
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

`fixtures/datahub_wire/` is neither the retired Python oracle nor a value comparison. It holds the
compute wire's **own bytes**: `write_frame`'s length-prefixed frames (a big-endian `u32` byte
length, then the UTF-8 JSON body) exactly as an already-deployed `vike-datahub`/`vike-backend` peer
sends them. The subject is `crates/vike-datahub-client/src/proto.rs`'s five
`#[serde(rename = "...")]` attributes, which froze the wire's spelling when the Rust identifiers
moved to `paramscan`. It lives HERE, outside `crates/`, for one reason: the guard for those pins
used to be a test in `proto.rs` itself, and a rename pass rewrote a pin's argument and that test's
expected string in one edit — a silent, permanent wire break that passed a 1,097-test run and a
full `verify-branch`, and was found by a human reading the diff. An expectation a single `sed` over
the source can move with its subject is not an expectation.

Frozen for the same reason as `r0/`–`r6/` but by a different argument: these are not a recording of
a source that has gone away, they are a recording of peers that are still out there. Regeneration
is not a supported operation and there is no generator — a changed byte here is a changed protocol.
The replaying test asserts SUBSET, not byte-equality, so a legitimately ADDITIVE field never forces
a regeneration; its module doc argues why that asymmetry is the safer rule.

## Owner-ruled corrections

A frozen byte changes only when the owner rules that the oracle pinned a bug. Each such value is
changed ALONE (no other byte of its file moves), its old bits are kept in a `*_ruling` key beside
it, and it gets a row here. Nothing else in this tree may be "corrected".

| file, scenario, field | was → is | ruling |
|---|---|---|
| `r5/hub.json`, `lifecycle_full`, `expect.total_exposure` | `405939999999999a` (100.9) → `407f880000000000` (504.5) | owner 2026-11-09: the fixture pinned a bug, `\|size\|·mark` without the contract unit (the scenario runs at multiplier 5, and its own `equity_all` already counts it); corrected to `ContractValue::gross_notional`, as `crates/vike-execution/src/execution_engine/queries.rs`'s `total_exposure` now computes it. The r5 parity test (`crates/vike-execution/tests/parity/r5_parity.rs`) is its only reader. |
| `r5/risk.json`, `cases[15]`, `price` | `40646570a3d70a3e` (163.17000000000002) → `40646570a3d70a3d` (163.17) | P5.1 point 2, below |
| `r5/risk.json`, `cases[21]`, `qty` | `4066033333333334` (176.10000000000002) → `4066033333333333` (176.1) | P5.1 point 2, below |
| `r5/risk.json`, `cases[27]`, `qty` | `405ca66666666667` (114.60000000000001) → `405ca66666666666` (114.6) | P5.1 point 2, below |
| `r5/risk.json`, `cases[41]`, `price` | `4060766666666667` (131.70000000000002) → `4060766666666666` (131.7) | P5.1 point 2, below |
| `r5/risk.json`, `cases[57]`, `price` | `4055666666666667` (85.60000000000001) → `4055666666666666` (85.6) | P5.1 point 2, below |
| `r5/risk.json`, `cases[57]`, `qty` | `407cfb3333333334` (463.70000000000005) → `407cfb3333333333` (463.7) | P5.1 point 2, below |
| `r5/risk.json`, `cases[58]`, `price` | `405a3f5c28f5c290` (104.99000000000001) → `405a3f5c28f5c28f` (104.99) | P5.1 point 2, below |
| `r5/risk.json`, `cases[61]`, `qty` | `404cff5c28f5c290` (57.995000000000005) → `404cff5c28f5c28f` (57.995) | P5.1 point 2, below |
| `r5/risk.json`, `cases[64]`, `price` | `4065d66666666667` (174.70000000000002) → `4065d66666666666` (174.7) | P5.1 point 2, below |
| `r5/risk.json`, `cases[75]`, `qty` | `402c666666666667` (14.200000000000001) → `402c666666666666` (14.2) | P5.1 point 2, below |
| `r5/risk.json`, `cases[80]`, `price` | `4058e66666666667` (99.60000000000001) → `4058e66666666666` (99.6) | P5.1 point 2, below |
| `r5/risk.json`, `cases[95]`, `qty` | `4075eb3333333334` (350.70000000000005) → `4075eb3333333333` (350.7) | P5.1 point 2, below |
| `r5/risk.json`, `cases[110]`, `qty` | `406c366666666667` (225.70000000000002) → `406c366666666666` (225.7) | P5.1 point 2, below |
| `r5/risk.json`, `cases[111]`, `qty` | `406d166666666667` (232.70000000000002) → `406d166666666666` (232.7) | P5.1 point 2, below |
| `r5/risk.json`, `cases[121]`, `qty` | `4078a33333333334` (394.20000000000005) → `4078a33333333333` (394.2) | P5.1 point 2, below |
| `r5/risk.json`, `cases[139]`, `qty` | `4069766666666667` (203.70000000000002) → `4069766666666666` (203.7) | P5.1 point 2, below |
| `r5/risk.json`, `cases[146]`, `qty` | `402d333333333334` (14.600000000000001) → `402d333333333333` (14.6) | P5.1 point 2, below |
| `r5/risk.json`, `cases[153]`, `price` | `4054b66666666667` (82.85000000000001) → `4054b66666666666` (82.85) | P5.1 point 2, below |
| `r5/risk.json`, `cases[156]`, `qty` | `40730e6666666667` (304.90000000000003) → `40730e6666666666` (304.9) | P5.1 point 2, below |
| `r5/risk.json`, `cases[175]`, `price` | `405ef33333333334` (123.80000000000001) → `405ef33333333333` (123.8) | P5.1 point 2, below |
| `r5/risk.json`, `cases[224]`, `qty` | `40716204189374bd` (278.12600000000003) → `40716204189374bc` (278.126) | P5.1 point 2, below |
| `r5/risk.json`, `cases[233]`, `qty` | `405264dd2f1a9fbf` (73.57600000000001) → `405264dd2f1a9fbe` (73.576) | P5.1 point 2, below |
| `r5/risk.json`, `cases[239]`, `qty` | `4072f66666666667` (303.40000000000003) → `4072f66666666666` (303.4) | P5.1 point 2, below |
| `r5/risk.json`, `rounding` row `0.35` on step `0.1`, `result` | `3fd3333333333334` (0.30000000000000004) → `3fd3333333333333` (0.3) | P5.1 point 2, below |
| `r5/hub.json`, `normalization`, `expect.submissions[1]` qty (`n2`) | `3fd3333333333334` (0.30000000000000004) → `3fd3333333333333` (0.3) | P5.1 point 2, below (old bits in `expect.submissions_ruling`) |

**P5.1 point 2 (owner 2026-11-13, the 25 rows marked "P5.1 point 2").** The order snap
(`crates/vike-model/src/scalar.rs`'s `round_to_step`, under the risk gate's `round_to`) now reads its tick count back through
`vike_marketdata::TickFraction`, the rule `L2Book` reads its levels by, so a snapped price or size is the double nearest
its decimal and the same number the book names at that level. The oracle held `count * step`, which sits one ulp off
the decimal for these 25 values (each moved by exactly 1 ulp, every one to the shorter decimal; no verdict, reason or
state of any r5 case moved). The owner allowed them as corrections value by value; each value's old bits are in a
`*_ruling` key beside it (`price_ruling`, `qty_ruling`, `result_ruling`; the hub's submission sits in an array, so its
key is `expect.submissions_ruling`). No tolerance was widened: `assert_bits` still compares bits.

NOTE (hard-won): CPython ≥3.12 builtin `sum()` over floats is **Neumaier-compensated** — ported
sites use `vike_model::py_sum` (`crates/vike-model/src/pysum.rs`), while Python's explicit `+=`
loops stay naive folds. Mirror per-site; a blanket choice diverges either way.
