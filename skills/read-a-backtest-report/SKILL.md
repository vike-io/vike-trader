---
name: read-a-backtest-report
description: Read a vike backtest report honestly — every BacktestReport row (final_equity, total_return, n_trades, win_rate, sharpe, max_drawdown, profit_factor, funding_paid, per_symbol_pnl, zero_trade), what the Sharpe annualization factor is for the profile's interval, the overfitting checks (n_candidates, deflated Sharpe, PBO, walk-forward consistency) to run before believing a Sharpe, and what the compact report cannot tell you (no trade list, no equity curve, no Monte Carlo). Use when a user asks to interpret, explain, evaluate, trust, compare or summarize backtest results, a Sharpe ratio, a drawdown, a win rate, a zero-trade run, or asks "is this strategy overfit" / "is this Sharpe real" after run_backtest.
metadata:
  tools: "run_backtest"
  source: "trader/guides/read-a-report trader/tutorials/monte-carlo-and-overfitting ai/trader/tools/run_backtest"
---

# Read a backtest report

A finished backtest returns one flat summary. Its numbers are trustworthy for one reason:
`crates/vike-analytics/src/report.rs`'s `BacktestReport` does no metric maths of its own — every
field is a pass-through from the run result or a single `metrics::` call — so a wrong number is a
wiring bug, not a maths bug. What the report is NOT is a verdict: the Sharpe is the maximum of
however many configurations were searched, and the equity path is the order the trades happened to
land in. This procedure reads the rows, names the scale they are on, and lists the checks that stand
between a number and a belief.

## Procedure

### 1. Get the report (or take the one the user already has)

Call `run_backtest` with:

- `profile` (string, **required**) — the backtest profile TOML: `[data]`, `[strategy]`, optional
  `[engine]`. A missing `profile` is a tool error before anything connects.
- `script` (string, optional) — Rhai source, injected as `[strategy.params].src`. Existing params
  are preserved; any pre-existing inline `src` is overwritten. An injected script only RUNS when
  `strategy.name = "rhai"`; under any other name it is carried along as an unread param while the
  built-in strategy runs.

The tool is `readOnlyHint: true` — a backtest mutates nothing — so there is no preview/confirm
gate. It returns `{ "report": … }`, the `BacktestReport` exactly as the server serialized it.

Two failure shapes to recognise, both clean tool errors rather than reports:
- `cannot connect to datahub at …` — the server named by `--addr` is unreachable.
- `profile parse/validate failed: …` — the server's own parser (the only one in the loop) rejected
  the profile: a bad range, an unknown `strategy.name`, a `[data]` table naming a series the store
  does not hold. This arrives as an `isError` result.

Reference: `https://vike.io/docs/ai/trader/tools/run_backtest`.

### 2. Read `zero_trade` FIRST on a disappointing run

`zero_trade` is present **only** when the run closed no trades AND equity never moved off the
starting cash. Then it is a ranked list of probable causes, most-probable first — not an empty stats
block — composed by `crates/vike-analytics/src/zero_trade.rs`'s `ZeroTradeReport::analyze` from
counters the engine already accumulated. Each cause carries a stable kebab-case code:
`no-data`, `warmup-shortfall`, `orders-denied`, `stale-price`, `session-closed`, `no-orders`
(the catch-all, so the list is never empty). If this field is present, report its top cause and
stop: "total return 0%, Sharpe NaN, max drawdown 0%" says nothing about why.

It is inert by construction: a run that closed a trade OR moved equity gets no `zero_trade`. An
open-and-hold position (zero closed trades, real marked position) is deliberately not flagged —
its equity curve moved.

### 3. Read every row, in this order

| Field | What it is | How to read it |
| --- | --- | --- |
| `name` | the profile's free-form `name`, or `null` | identification only |
| `final_equity` | closing equity | the absolute number `total_return` is relative to |
| `total_return` | fractional return, first to last equity-curve point | a fraction, not a percent, in JSON |
| `n_trades` | closed trade count | the denominator of everything below; a small count makes every ratio noise |
| `win_rate` | fraction of trades with positive PnL | says nothing about size — pair it with `profit_factor` |
| `sharpe` | annualized Sharpe of per-bar (or per-tick) returns | on the scale in step 4 — the row most often misread |
| `max_drawdown` | largest peak-to-trough drop, a POSITIVE fraction of the peak | one path's drawdown; see step 6 |
| `profit_factor` | gross profit over gross loss | `null` when non-finite: no losing trades but some profit is "no meaningful ratio", not a number. In-struct sentinel is `INFINITY` for that case and `0.0` when there is neither profit nor loss. Not printed in the human table — it exists for ranking objectives and JSON consumers |
| `funding_paid` | net perp funding cashflow, received-positive, paid-negative | `0.0` for a spot run; prints in the human table only when nonzero |
| `per_symbol_pnl` | per-symbol PnL | populated on multi-symbol event runs only; empty for single-symbol and vector runs |

The human table (`vike-cli backtest`) prints `total_return`, `win_rate` and `max_drawdown` as
percentages at four decimals and `sharpe` at four; the JSON carries fractions. Do not mix the two in
a write-up. Reference: `https://vike.io/docs/trader/guides/read-a-report`.

### 4. Name the annualization factor before quoting the Sharpe

The factor is DERIVED from the profile's `[data].interval`, by
`crates/vike-backtest/src/harness/report.rs`'s `periods_per_year` — the single source of truth, so
the single-run binary and a sweep rank on the same scale:

- daily bars anchor at **252**;
- every other interval scales off that by how many fit in a day:
  `252 · (86_400_000 / interval_ms)` — so `1h` is 6,048 and `1m` is 362,880;
- a **tick** profile keeps the default factor: a tick stream has no fixed period and no honest
  observation count to derive one from. Say so when quoting a tick run's Sharpe.

Two stated limits to carry into the write-up: 252 is the equity convention of 252 trading days,
while these markets trade 24/7 (a defensible crypto anchor would be 365 — a separate decision, not
applied). And the factor used to be 252 for everything, which understated every intraday Sharpe by
about 37.9 (`sqrt(1440)`) while still looking like a Sharpe; a report from before that fix is on a
different scale from one after it.

### 5. Run the overfitting checks before believing the Sharpe

From ONE run you can compute the probabilistic Sharpe and nothing more. The deflated Sharpe and the
probability of backtest overfitting (PBO) both need the **set of trials** the winner was picked
from. So, before calling a Sharpe real:

1. **Ask how many configurations were tried.** `n_candidates` is the first number to check when a
   result looks too good. A report handed over alone has an unknown trial count; say that.
2. **Feed the candidates, not the survivors.** `crates/vike-analytics/src/overfit.rs`'s
   `audit_selection` takes the full candidate arrays PLUS the selected index; a survivors-only call
   reports `n_candidates == 1` and says so in the verdict, and inconsistent inputs return `None` —
   an unauditable selection is not a passing one. `effective_n_trials` and `pbo_cscv` are only as
   good as the trial set they are given, and the failure is silent.
3. **Never hand `overfit::` an annualized Sharpe.** `sharpe_moments(equity_curve)` is the one
   sanctioned entry, producing `sr_per_obs`, `n_obs`, `skew`, `kurt`; the annualized figure is a
   display quantity for a different reader.
4. **Read `selected_trades` beside the Sharpe.** A mean-reversion family dies by starvation (too
   few divergences) before it dies by unprofitability, and a Sharpe-ranked sweep cannot see that.
5. **Score the verdict** with `overfit_verdict(pbo, deflated_sr, wf_consistency)`: PBO above 0.5
   scores two points, above 0.2 one; deflated Sharpe below 0.5 scores two, below 0.9 one;
   walk-forward consistency below 0.5 scores one. Three or more is `High`, one or two `Medium`,
   zero `Low`. A `NaN` PBO is NOT assessed — it gets its own reason row, not a vote toward `Low`.

Producing the trial set honestly is the job of sweeps and walk-forward
(`https://vike.io/docs/trader/backtesting/sweeps`, `https://vike.io/docs/trader/backtesting/walk-forward`); reference for the
checks: `https://vike.io/docs/trader/tutorials/monte-carlo-and-overfitting`.

### 6. Ask how much of the path was ordering (Monte Carlo)

`vike_analytics::montecarlo` resamples the observed trade list into synthetic equity paths:
- `Shuffle` reorders the trades — same terminal equity on every path, what varies is the SHAPE and
  so the drawdown;
- `Bootstrap` samples with replacement — terminal equity varies too.

`mc_summary(trade_pnls, start_equity, n_sims, seed, ruin_pct)` is the one-call shuffle version:
terminal equity at the 5th/50th/95th percentiles, max drawdown at the 50th/95th, `prob_loss` (share
of paths ending at or below start) and `risk_of_ruin` (share ending at or below
`ruin_pct × start_equity`). Decide and STATE which P&L was fed: `Trade.pnl` is gross price P&L,
`Trade.fees` is the round-trip cost carried separately — `pnl − fees` is the one you can spend. The
seed reproduces run-to-run within this implementation only; there is no golden fixture.

### 7. Say what the report cannot tell you

- **No trade list, no equity curve.** `BacktestReport` is deliberately compact — the human table,
  the `--json` schema and the sweep's ranking source. Steps 5 and 6 read `BacktestResult`'s
  `trades` and `equity_curve` in process, from the caller that ran the backtest — NOT from the
  report `run_backtest` returns and not from a persisted `report.json`.
- **Only the single-run path persists.** A sweep, a euler refinement or a TPE run answers with a
  `SweepReport` — a different document — and writes nothing.
- **One `max_drawdown` is one path.** The distribution is step 6's answer, not the report's.
- **A Sharpe with no trial count is unaudited.** State the annualization factor and that
  `n_candidates` is unknown if it is.
- **The live `tearsheet` is a different tool.** It renders a live session's command journal, not
  a backtest; its `--seed CASH` (default 10000) sets the denominator of every return, and its
  realized-per-trade curve makes `--periods-per-year` (default 252) approximate. Do not read one as
  the other.

## Write-up template

State, in this order: `zero_trade` cause if present; `n_trades`; `total_return` and
`final_equity`; `sharpe` WITH its factor ("annualized at 6,048 for 1h bars"); `max_drawdown`;
`profit_factor` (or "no meaningful ratio" when `null`); `funding_paid` if nonzero; the trial count
(or "unknown — unaudited"); which P&L any Monte Carlo used. Numbers without those qualifiers are
not a reading, they are a table.
