---
name: run-a-backtest
description: Run a backtest of a strategy over stored history on the vike-datahub data service and read the report back. Use when the user asks to backtest, test a strategy on history, run a profile, check whether a strategy trades or is profitable on past bars, try a Rhai script or a built-in strategy against BTCUSDT (or any stored series), or asks what data and strategies the server has to backtest with. Covers writing the backtest profile TOML, discovering series and strategy names first (list_series, list_strategies), calling run_backtest, and reading BacktestReport fields (final_equity, total_return, sharpe, max_drawdown, zero_trade). Needs no credentials.
metadata:
  tools: "list_strategies list_series run_backtest"
  source: "ai/trader/backtesting ai/trader/tools/run_backtest ai/trader/tools/list_series ai/trader/tools/list_strategies trader/tutorials/rsi-mean-reversion-backtest"
---

# Run a backtest

A backtest runs a strategy over history held by a `vike-datahub` server and returns a
`BacktestReport`. It mutates nothing: the three tools this skill uses are all read-only, and the
zero-credentials promise holds end to end — no venue account and no API key are needed at any
step, and the create+backtest tools keep working when no node keys are configured at all.
(`https://vike.io/docs/ai/trader/backtesting`, `https://vike.io/docs/trader/tutorials/rsi-mean-reversion-backtest`,
`crates/vike-cli/src/cmd/mcp.rs`'s module doc.)

## What a profile is

The input to `run_backtest` is a **backtest profile TOML** string with a `[data]` table (which
series to run over), a `[strategy]` table (which strategy, and its params) and an optional
`[engine]` table. Minimal shape, from the docs:

```toml
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "2024-01-01T00"
to = "2024-06-01T00"

[strategy]
name = "buy_hold"
```

The profile crosses the wire **verbatim**; the server's `BacktestProfile::from_toml_str` is the
only parser. So a shape error — a bad range, an unknown `strategy.name`, a `[data]` table naming
a series the store does not hold — comes back from the server as `profile parse/validate failed:
…` after the connect, as an `isError` tool result. One parser, one error source.
(`https://vike.io/docs/ai/trader/tools/run_backtest`.)

## Procedure

### 1. Find out what data exists — `list_series`

Call `list_series` with no arguments (`arguments: {}`; its `inputSchema` has no properties). It
returns `series`, one row per stored series:
`{kind, venue, symbol, interval, first_ts, last_ts, rows}`.

- `interval` is present for bar series and `null` for tick series.
- `first_ts`/`last_ts` are the coverage span (the page's example shows epoch-millisecond values);
  pick the profile's `from`/`to` inside it instead of guessing.
- A blank `symbol` means a **grouped** series (many symbols in one part file), not "no symbol".
- Requires a reachable datahub at `--addr` (default `127.0.0.1:7878`). If nothing is listening,
  the result is a clean error beginning `cannot connect to datahub at` — not a panic.

(`https://vike.io/docs/ai/trader/tools/list_series`; `crates/vike-cli/src/cmd/mcp.rs`'s `tool_list_series`.)

If the store is empty, tell the user how to fill it from the CLI: `backtest --seed-demo` writes a
credential-free demo tape under venue `demo` (BTCUSDT hourly, 2025-01-01 to 2025-07-01, plus the
last seven days at 1m), and `backtest --fetch binance:BTCUSDT:1h --from … --to …` pulls real
public bars with no credentials and no venue account.
(`https://vike.io/docs/trader/tutorials/rsi-mean-reversion-backtest`.)

### 2. Find out which strategies the server can name — `list_strategies`

Call `list_strategies` with no arguments. It returns `strategies`, a flat list of the compiled
**native** strategy names a profile's `strategy.name` can resolve, in the server's declared
order. Call it rather than assuming a roster: the list belongs to the server's build.

Two things it deliberately does not include:

- `rhai` is **not** listed. It is the arm a script runs through, but it needs a `src` param, so
  it is not resolvable with default params. Scripts are the other route to the same engine.
- A user strategy compiled from `user_data/strategies/rust` can resolve on the server but is not
  in this answer; the list is the built-in roster only.

An unknown `strategy.name` fails at profile load, so one call here before authoring is worth it.
(`https://vike.io/docs/ai/trader/tools/list_strategies`; `crates/vike-cli/src/cmd/mcp.rs`'s
`tool_list_strategies`.)

### 3. Write the profile

Take `venue`, `symbols`, `kind` and `interval` from a `list_series` row and a `from`/`to` inside
its `first_ts..last_ts`. Set `strategy.name`:

- a name from `list_strategies` for a built-in strategy, with its knobs in `[strategy.params]`;
- `name = "rhai"` when you will inject a script (step 4).

A params table is a READER, not a schema: keys the strategy does not read are ignored in silence
and every unread knob keeps its default. Check the knobs you set are ones the strategy reads.
(`https://vike.io/docs/trader/tutorials/rsi-mean-reversion-backtest`.)

### 4. Run it — `run_backtest`

Arguments (from `tools_spec`'s `inputSchema`):

| argument | type | required | meaning |
| --- | --- | :---: | --- |
| `profile` | string | yes | the backtest profile TOML |
| `script` | string | no | Rhai source, injected as `[strategy.params].src` |

- A missing `profile` is a tool error before anything connects.
- With `script`, the client parses the profile as TOML (a malformed one is a clean error),
  creates `[strategy]`/`[strategy.params]` if absent, sets `src`, preserves existing params and
  overwrites any pre-existing inline `src` — then ships the result.
- **An injected script only runs if `strategy.name` is `"rhai"`.** Inject under
  `name = "buy_hold"` and the script rides along as an unread param while the built-in runs.
- Connect failure is a clean `cannot connect to datahub at …` error. A datahub advertising the
  `auth` feature refuses at connect with an actionable message (every run verb is control scope);
  the default key-less datahub is unaffected.

(`crates/vike-cli/src/cmd/mcp.rs`'s `tool_run_backtest` and `tools_spec`;
`https://vike.io/docs/ai/trader/tools/run_backtest`.)

### 5. Read the report

The result is `{ "report": … }`, the `BacktestReport` as the server serialized it:

| field | meaning |
| --- | --- |
| `name` | the profile's free-form `name`, or `null` |
| `final_equity` | closing equity |
| `total_return` | fractional return, first to last equity-curve point |
| `n_trades` · `win_rate` | trade count, and the fraction with positive PnL |
| `sharpe` | annualized Sharpe of per-bar returns (252 for daily bars) |
| `max_drawdown` | largest peak-to-trough drop, a positive fraction of the peak |
| `profit_factor` | gross profit over gross loss; **`null` when non-finite** |
| `funding_paid` | net perp funding cashflow, received-positive; `0.0` for spot |
| `per_symbol_pnl` | multi-symbol event runs only; empty otherwise |
| `zero_trade` | present **only** when no trades closed and equity never moved |

Read `zero_trade` first on a disappointing run: it is a ranked list of probable causes (gate
drops, stale-price deferrals, session skips, warm-up), not an empty stats block. On a
mean-reversion strategy the usual cause is thresholds the tape never reached.

Never report one number as the verdict. The tutorial's own demo run shows a Sharpe of 1.6844 on a
run that lost fourteen times the account — Sharpe is computed on per-bar returns and says nothing
about whether the account survived, which is why drawdown and equity sit beside it. Also state
that one symbol over one range is one sample; the next step is a walk-forward, not a conclusion.
(`https://vike.io/docs/ai/trader/tools/run_backtest`, `https://vike.io/docs/trader/tutorials/rsi-mean-reversion-backtest`.)

## Failure checklist

- `cannot connect to datahub at …` — no server at `--addr`; nothing to retry until one is running.
- `profile parse/validate failed: …` — fix the profile shape from the message (unknown strategy,
  bad range, series not in the store); re-run `list_series`/`list_strategies` if unsure.
- A flat curve from a Rhai script — a misspelled indicator errors on every bar and, after ten
  consecutive errors, the strategy switches itself off and looks mounted but never trades.
- Silent wrong result — a params key the strategy never reads (see step 3).
