//! The run profiles `vike-cli init` ships, and the one demo-seeding command they quote.

use super::SMA_CROSS_RHAI;

/// The command that writes the demo tape [`PROFILE_BACKTEST`] names — ONE spelling, printed by
/// `vike-cli init` as its first next step and quoted verbatim in that profile's `[data]` comment.
///
/// ⚠ It is a const because the spelling has already moved twice under hand-typed copies:
/// `backtest --seed-demo` (the engine flag ruling 12 retired) and then `vike-cli data seed-demo`
/// (the pre-group verb the `--source` axis absorbed). The second move left this profile and
/// `init`'s printed next step both teaching a spelling the binary refuses, and the test that
/// checked the profile kept passing, because it held a third copy of the same stale words.
/// `crate::cmd::init`'s tests drive this through the real parser and hold the profile's copy equal
/// to it.
pub const SEED_DEMO: &str = "vike-cli data hist fetch --source demo";

/// `profiles/backtest.toml` — one run, script supplied by `--script`.
pub const PROFILE_BACKTEST: &str = r#"# One backtest run.
#
#   vike-cli backtest run --profile backtest.toml \
#       --script ../strategies/rhai/sma_cross/sma_cross.rhai
#
# The strategy is NOT named here: `--script` injects the file's source into
# [strategy.params].src, so this file stays about the data and the costs.

name = "sma_cross — demo BTCUSDT 1h"

[data]
kind = "bar"
interval = "1h"
# ⚠ THIS IS THE DEMO TAPE — synthetic bars from a closed-form curve, NOT market data. It is here
# so a fresh install has a profile that RUNS; a result computed on it says nothing about any
# market. Seed it with `vike-cli data hist fetch --source demo`, which writes exactly this slice.
#
# For real data, point this at a slice your store actually holds and change `venue`. An empty
# slice is a run with no trades, which looks identical to a strategy that never fired.
venue = "demo"
symbols = ["BTCUSDT"]
from = "2025-01-01T00"
to = "2025-07-01T00"

[engine]
cash = 10000.0
# 0.1% per side. ⚠ A guess about your venue, not a fact — and the number that decides whether a
# marginal strategy is profitable.
fee_rate = 0.001
slippage = 0.0

[strategy]
# "rhai" runs an interpreted script; its source arrives via --script.
name = "rhai"

# Overrides for the script's own `param()` defaults. Delete a line to use the script's default.
[strategy.params]
fast = 10.0
slow = 30.0
qty = 1.0
"#;

/// `profiles/sweep.toml` — composed, because the script is inlined (see [`profile_sweep`]).
///
/// The `{src}` slot takes `SMA_CROSS_RHAI` verbatim, so a fresh scaffold cannot ship a sweep whose
/// inlined script differs from the one in `strategies/rhai/sma_cross/`.
pub fn profile_sweep() -> String {
    format!(
        r#"# A parameter sweep: the same strategy over a grid, ranked.
#
#   vike-cli backtest run --profile sweep.toml --rank-by sharpe
#
# There is no separate `sweep` verb: a profile carrying a [paramscan] table IS a parameter search, and
# --optimizer picks the method. ⚠ The script is INLINE below rather than passed with --script, so
# this file is self-contained and can be handed to a remote daemon as one document. `vike-cli init`
# writes it from the same source as ../strategies/rhai/sma_cross/sma_cross.rhai; edit one, edit both.

name = "sma_cross sweep — BTCUSDT 1h"

[data]
kind = "bar"
interval = "1h"
venue = "binance"
symbols = ["BTCUSDT"]
from = "2025-01-01T00"
to = "2025-07-01T00"

[engine]
cash = 10000.0
fee_rate = 0.001
slippage = 0.0

[strategy]
name = "rhai"

[strategy.params]
qty = 1.0
src = '''
{src}'''

# Each key names a [strategy.params] field; every combination is run and the results ranked.
# ⚠ This grid is 3 x 3 = 9 runs. Cost multiplies fast — a fourth axis of 5 values is 45.
# ⚠ Use floats (10.0, not 10): a non-numeric value is skipped in silence.
[paramscan]
fast = [5.0, 10.0, 20.0]
slow = [30.0, 50.0, 100.0]
"#,
        src = SMA_CROSS_RHAI
    )
}

/// `profiles/walkforward.toml` — composed for the same reason as [`profile_sweep`].
pub fn profile_walkforward() -> String {
    format!(
        r#"# An anchored walk-forward: the same parameters over successive out-of-sample windows.
#
#   vike-cli backtest run --profile walkforward.toml
# (the [walkforward] table below is what selects the walk — there is no walkforward verb)
#
# What it answers: "was this profitable in every part of the sample, or in one lucky stretch?"
# ⚠ What it does NOT do is re-fit per window. These parameters are fixed and identical in all
# windows, so this is an out-of-sample STABILITY check, not an optimize-then-test protocol.
#
# ⚠ The script is INLINE below. That is history rather than a limit: this file was written when
# `walkforward` was a separate verb with no --script flag. See sweep.toml.

name = "sma_cross walk-forward — BTCUSDT 1h"

[data]
kind = "bar"
interval = "1h"
# ⚠ ONE symbol. Walk-forward splits a single bar series by index and rejects a multi-series profile.
venue = "binance"
symbols = ["BTCUSDT"]
from = "2024-01-01T00"
to = "2026-01-01T00"

[engine]
cash = 10000.0
fee_rate = 0.001
slippage = 0.0

[strategy]
name = "rhai"

[strategy.params]
fast = 10.0
slow = 30.0
qty = 1.0
src = '''
{src}'''

[walkforward]
# 4 anchored windows over the last 4/5 of the slice. Each starts fresh at [engine] cash.
n_splits = 4
"#,
        src = SMA_CROSS_RHAI
    )
}
