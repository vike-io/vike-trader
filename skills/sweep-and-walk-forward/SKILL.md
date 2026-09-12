---
name: sweep-and-walk-forward
description: Optimize a strategy's parameters honestly on the Vike datahub - expand a [sweep] grid with run_sweep, rank it (rank_by sharpe/return/max_dd/equity), then prove the winner out of sample with run_walk_forward's anchored windows and read wf_consistency before believing the rank. Use when asked to sweep, grid-search, tune or optimize parameters, to run a walk-forward or out-of-sample test, to check whether a backtest result is overfit or curve-fit, or to interpret oos_return, oos_sharpe, wf_consistency or n_splits.
metadata:
  tools: "run_sweep run_walk_forward run_backtest"
  source: "ai/trader/tools/run_sweep ai/trader/tools/run_walk_forward trader/guides/sweep-on-the-service trader/tutorials/sweep-and-walk-forward"
---

# Sweep, then walk forward

A sweep answers "which parameters scored best on this slice". A walk-forward answers a different
question - "was that profitable across the whole sample, or in one lucky stretch" - and answers it
without optimizing anything. Running the second after the first is the whole procedure; running
only the first is how a curve-fit ships. The in-sample winner's score is the maximum of N draws on
the bars it was chosen from, not an estimate of what those parameters do next.

All three tools here are remote datahub runs (`readOnlyHint: true`, a backtest mutates nothing).
None is an order write, so there is no `confirm` / `preview_token` gate in this skill. Every
`Run*` verb is control scope on the server because it compiles client-supplied Rhai, so a keyed
datahub refuses these tools at connect rather than one round trip later. An unreachable server is
the clean error `cannot connect to datahub at <addr>` (`crates/vike-cli/src/cmd/mcp.rs`'s
`tool_run_sweep`), never a panic. Reach a remote datahub through a tunnel
(`ssh -L 7878:localhost:7878 <host>`); the default address is `127.0.0.1:7878`.

## Step 1 - write the sweep profile

The profile is an ordinary backtest profile TOML with `[data]`, `[strategy]`, optional `[engine]`,
plus ONE extra table. Each `[sweep]` key names a `strategy.params` field and each value is an
array to cross-product over:

```toml
[sweep]
fast = [5.0, 10.0, 20.0]
slow = [30.0, 50.0, 100.0]
```

Rules that bite:

- **Write the values as floats.** A non-numeric value is skipped in silence.
- **Cost multiplies fast.** 3 x 3 is 9 runs; a fourth axis of five values is 45. Points run on a
  bounded rayon pool (`min(4, available_parallelism())` by default, raised on the server by
  `VIKE_SWEEP_THREADS`, pinned to one at a time by `VIKE_SWEEP_SEQUENTIAL=1`). Every point
  re-scans and fully materializes its own data slice, so peak memory is `N_threads x slice` -
  and these sweeps often run on a box that also hosts live trading. Keep the grid small on a
  large recorded tick slice.
- **Point `[data]` at a slice the store actually holds.** An empty slice is a run with no trades,
  which reads like a bad strategy rather than an empty query.
- A profile with no `[sweep]` table (or an empty one) shipped as a sweep is a server-side error
  that quotes the fix - the tool does not fall back to a single run.
- If the strategy is a Rhai script, either inline it as `[strategy.params].src` or pass it as the
  tool's `script` argument, which injects it there for you.

## Step 2 - call `run_sweep`

Arguments (from `tools_spec`):

| argument | type | required | meaning |
| --- | --- | :---: | --- |
| `profile` | string | yes | the profile TOML text with `[data]`/`[strategy]`/`[sweep]` (+ optional `[engine]`) |
| `script` | string | no | Rhai source, injected as `[strategy.params].src` |
| `rank_by` | string | no | `sharpe` (default), `return`, `max_dd` or `equity` - applied server-side |

The TOML ships verbatim; the server's `BacktestProfile::from_toml_str` is the only parser, so a
shape error (missing table, bad range, unknown strategy) arrives from the server as a
`profile parse/validate failed: ...` error result. Only two things fail before a socket opens: a
missing `profile`, or a `script` injected into TOML that does not parse.

`rank_by` direction is folded in: `sharpe`, `return` and `equity` sort descending, `max_dd`
ascending (smaller drawdown is better). An unrecognized value is a clean error naming the valid
set, never a silent fallback. A NaN key (a Sharpe over a zero-variance curve) sorts last among
the successes, and rows whose run failed always sort behind every success.

## Step 3 - read the `SweepReport`

The result is `{ "sweep": { "rows": [...], "rank_by": "..." } }`. Each row carries:

- `overrides` - the `(key, value)` pairs that produced this point;
- `report` **or** `error` - never both. A failed point records its error string and is not
  dropped, so the tail of `rows` is what broke;
- `score` - present only when a pluggable objective ranked the run.

Read it in this order:

1. **Trade counts before returns.** A Sharpe computed over two trades is a number, not a
   measurement; the same caution applies to a winner with a few dozen.
2. **Look for a one-axis result.** If every point on one value of an axis outranks every point on
   another (every `lo = 30` above every `lo = 20`), the grid found one threshold the slice
   rewarded and the other axis only shuffles rows within it.
3. **Keep the losers in view.** Ranking is not filtering: negative rows print last because a grid
   point that failed is evidence about the grid.
4. **Do not report rank 1 as a result.** All points were scored on the same bars and the top row
   was chosen by looking at those scores. That gap is what Step 4 measures.

## Step 4 - write the walk-forward profile

Take the winning row's `overrides`, put them in `[strategy.params]` of a profile with the SAME
`[data]` and `[engine]` block, delete the `[sweep]` table, and add:

```toml
[walkforward]
n_splits = 4
```

There is no split-count argument on the tool - `n_splits` rides in the profile. `n_splits` must be
at least 1; `0` is rejected at load. Constraints checked before the window loop:

- **Bar mode, one series.** The splitter divides a single bar series by index, so a tick-mode or
  multi-symbol profile is a clean validation error, never a silent fallback to the first symbol.
- **The mode is always anchored** and the annualization is derived from the bar interval; only
  the split count is yours to set.
- The whole `[engine]` surface is honoured (fee schedule, `[engine.impact]`,
  `[engine.resolution]`, `[risk]`, `snap_to_properties`, `attach_funding`), and each window is a
  fresh run at `engine.cash`, so the walk-forward cannot disagree with a single backtest about
  what the profile means.

## Step 5 - call `run_walk_forward`

Arguments (from `tools_spec`):

| argument | type | required | meaning |
| --- | --- | :---: | --- |
| `profile` | string | yes | the profile TOML text with `[data]`/`[strategy]`/`[walkforward]` (+ optional `[engine]`) |
| `script` | string | no | Rhai source, injected as `[strategy.params].src` |

Failure modes are identical to `run_sweep`: verbatim TOML, server-side parse/validate errors,
`cannot connect to datahub at ...` when unreachable.

## Step 6 - read the `WalkForwardReport`

The result is `{ "walkforward": {...} }` with:

| field | meaning |
| --- | --- |
| `windows` | one entry per OOS window: `test_range` as `(start, end)` bar indices and that window's `oos_return` |
| `oos_equity_curve` | every window's curve rebased onto one running equity |
| `oos_return` | the stitched fractional return over the whole walk |
| `oos_sharpe` | Sharpe of the stitched curve, under the derived annualization |
| `wf_consistency` | the fraction of windows that were profitable out of sample |

How the windows were cut: `chunk = n_bars / (n_splits + 1)`; window `s` tests
`[s*chunk, (s+1)*chunk)` and the last window runs to the end. Under anchored mode every window's
training range starts at index 0, so **the first chunk is never tested**.

**Read `wf_consistency` first.** It counts a window as profitable on a strictly positive
`oos_return` (a flat window counts against it). It answers "how many windows worked", where
`oos_return` compounds the windows and answers "how much they made". A strong stitched return
with `wf_consistency` near 0.25 is one window carrying the strategy - and the second number is
the one that should stop you. It is the INPUT the overfitting verdict consumes
(`https://vike.io/docs/trader/analytics/overfitting`), not a verdict itself.

Then apply these caveats to whatever you report:

- **The parameters never re-fit.** They are fixed and identical in every window. This is an
  out-of-sample STABILITY check of the sweep's winner, not an optimise-then-test protocol.
- **`oos_return` is not comparable with the sweep's in-sample return.** The sweep scored the whole
  slice; the walk-forward scored the last `n_splits / (n_splits + 1)` of it. The two landing near
  each other is mildly reassuring and nothing more.
- **A losing window is the result, not a run to repeat.** Report it as such.
- **Few windows, few observations.** With `n_splits = 4`, 75% and 100% are one window apart, and
  over six months of hourly bars each window is about five weeks - short enough for one trend
  leg to fill it. Raising `n_splits` buys more windows, not more data.
- **This is not a forward test.** The parameters were chosen by looking at these same bars, so the
  selection has already seen every window it is now scored on. The honest reading is narrow:
  with these parameters held fixed, this slice produced K up windows and N-K down ones.

## Optional - `run_backtest` on the winner

Every expanded sweep point is itself a plain, runnable single-backtest profile. To get the
winner's full `BacktestReport` (rather than the row embedded in the sweep), take the same
`[strategy.params]` profile without `[sweep]` or `[walkforward]` and call `run_backtest` with
`profile` (required) and optional `script`; the result is `{ "report": ... }`. It runs under the
same parser and the same `[engine]`, so its numbers equal the sweep row's - it adds detail, not
evidence. Only the walk-forward adds evidence.

## Reporting template

State, in this order: the grid size and `rank_by`; the top rows with their `overrides`, return,
Sharpe, max drawdown and trade count; which rows failed and why; then the walk-forward's
`n_splits`, each window's `oos_return`, `wf_consistency`, stitched `oos_return` and `oos_sharpe`;
and finally the narrow reading above. Never present the in-sample rank alone as a result.

Human twins: `vike-cli backtest --profile <run.toml> --rank-by sharpe` (a profile carrying a
`[sweep]` table IS a search — there is no separate `sweep` verb) and
`vike-cli walkforward --profile <run.toml>` (`https://vike.io/docs/trader/guides/sweep-on-the-service`,
`https://vike.io/docs/trader/tutorials/sweep-and-walk-forward`). Tool references:
`https://vike.io/docs/ai/trader/tools/run_sweep`, `https://vike.io/docs/ai/trader/tools/run_walk_forward`.
