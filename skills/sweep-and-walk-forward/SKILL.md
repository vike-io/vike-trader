---
name: sweep-and-walk-forward
description: Optimize a strategy's parameters honestly on the Vike backtest daemon - expand a [paramscan] grid with run_sweep, rank it (rank_by sharpe/return/max_dd/equity/multi) under a chosen optimizer (grid, euler, tpe, genetic), then prove the winner out of sample with run_walk_forward's anchored windows and read wf_consistency before believing the rank. Use when asked to sweep, grid-search, tune or optimize parameters, to run a walk-forward or out-of-sample test, to check whether a backtest result is overfit or curve-fit, to interpret oos_return, oos_sharpe, wf_consistency or n_splits, or when asked which search methods exist (--list-optimizers), how to disqualify a candidate with too few trades (--min-trades) or how to watch a long search (--progress).
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

All three tools here are remote compute runs (`readOnlyHint: true`, a backtest mutates nothing).
None is an order write, so there is no `confirm` / `preview_token` gate in this skill. Every
`Run*` verb is control scope on the server because it compiles client-supplied Rhai, so a keyed
server refuses these tools at connect rather than one round trip later. An unreachable one is the
clean error `cannot connect to the backtest daemon at <addr>` (`crates/vike-cli/src/cmd/mcp.rs`'s
`tool_run_paramscan`), never a panic.

⚠ **These three dial the BACKTEST daemon, which is a different process on a different port from
the datahub.** `vike-cli mcp` takes `--backtest-addr` for them and `--addr` for the market-data
tools; the defaults are `127.0.0.1:7880` and `127.0.0.1:7878`
(`crates/vike-config/src/config.rs`'s `DEFAULT_BACKTEST_ADDR`). So the tunnel a sweep needs is
`ssh -L 7880:localhost:7880 <host>`, and the far side is started with
`vike-backend backtest --addr` - which is what that connect error tells you to run.

## Step 1 - write the sweep profile

The profile is an ordinary backtest profile TOML with `[data]`, `[strategy]`, optional `[engine]`,
plus ONE extra table. Each `[paramscan]` key names a `strategy.params` field and each value is an
array to cross-product over:

```toml
[paramscan]
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
- A profile with no `[paramscan]` table (or an empty one) shipped as a sweep is a server-side error
  that quotes the fix - the tool does not fall back to a single run.
- If the strategy is a Rhai script, either inline it as `[strategy.params].src` or pass it as the
  tool's `script` argument, which injects it there for you.

## Step 2 - call `run_sweep`

Arguments (from `tools_spec`):

| argument | type | required | meaning |
| --- | --- | :---: | --- |
| `profile` | string | yes | the profile TOML text with `[data]`/`[strategy]`/`[paramscan]` (+ optional `[engine]`); the legacy `[sweep]` spelling still loads |
| `script` | string | no | Rhai source, injected as `[strategy.params].src` |
| `rank_by` | string | no | `sharpe` (default), `return`, `max_dd`, `equity` or `multi` (the composite objective) - applied server-side |
| `optimizer` | string | no | the search METHOD: `grid` (default, exhaustive), `euler` (successive halving), `tpe` (Bayesian) or `genetic` |
| `trials` | integer | no | tpe's trial budget. tpe ONLY - refused by name under any other optimizer |
| `seed` | integer | no | reproducibility seed. tpe and genetic ONLY; genetic REQUIRES it |
| `euler_depth` | integer | no | euler's successive-halving depth. euler ONLY |

The TOML ships verbatim; the server's `BacktestProfile::from_toml_str` is the only parser, so a
shape error (missing table, bad range, unknown strategy) arrives from the server as a
`profile parse/validate failed: ...` error result. Only two things fail before a socket opens: a
missing `profile`, or a `script` injected into TOML that does not parse.

`rank_by` direction is folded in: `sharpe`, `return` and `equity` sort descending, `max_dd`
ascending (smaller drawdown is better). An unrecognized value is a clean error naming the valid
set, never a silent fallback. A NaN key (a Sharpe over a zero-variance curve) sorts last among
the successes, and rows whose run failed always sort behind every success. If you would rather see
the `optimizer` roster than trust a list, the engine prints it - the engine section below.

⚠ **Case is free on the two SELECTORS at the command line, and it was not.** Both go through
`crates/vike-datahub-client/src/flag_vocab.rs`'s `accept_value`, which matches a roster member
ASCII-case-insensitively and forwards the MEMBER's own spelling, so
`vike-cli backtest run --profile sweep.toml --rank-by SHARPE --optimizer TPE` runs `sharpe`/`tpe`
and every downstream reader - a spawned engine, the wire frame, the run artifact's identity - sees
one spelling. The engine always accepted those; the client used to refuse them locally with exit 2,
which is the disagreement that closed. Over THIS tool send the member spelling anyway: `optimizer`
is declared as a JSON-schema `enum` of the lower-case roster, so an upper-case token is a call a
strict client can reject before it reaches a server that would have understood it. And case widens
the SPELLINGS of a member, never the membership - `--rank-by sharp` is still a usage error, and its
message renders the whole roster
(`invalid --rank-by "sharp" (expected sharpe|return|max_dd|equity|multi)`).

## Step 3 - read the `SweepReport`

The result is `{ "sweep": { "rows": [...], "rank_by": "..." } }`. Each row carries:

- `overrides` - the `(key, value)` pairs that produced this point;
- `report` **or** `error` - never both. A failed point records its error string and is not
  dropped, so the tail of `rows` is what broke;
- `score` - present only when a pluggable objective ranked the run.

Read it in this order:

1. **Trade counts before returns.** A Sharpe computed over two trades is a number, not a
   measurement; the same caution applies to a winner with a few dozen. Over these tools that is a
   rule you apply by hand, reading each top row's `report.n_trades` before you read its score - the
   engine can enforce it instead, and the engine section below is how.
2. **Look for a one-axis result.** If every point on one value of an axis outranks every point on
   another (every `lo = 30` above every `lo = 20`), the grid found one threshold the slice
   rewarded and the other axis only shuffles rows within it.
3. **Keep the losers in view.** Ranking is not filtering: negative rows print last because a grid
   point that failed is evidence about the grid.
4. **Do not report rank 1 as a result.** All points were scored on the same bars and the top row
   was chosen by looking at those scores. That gap is what Step 4 measures.

## Step 4 - write the walk-forward profile

Take the winning row's `overrides`, put them in `[strategy.params]` of a profile with the SAME
`[data]` and `[engine]` block, delete the `[paramscan]` table, and add:

```toml
[walkforward]
n_splits = 4
```

There is no split-count argument on the tool - `n_splits` rides in the profile. `n_splits` must be
at least 1; `0` is rejected at load. Constraints checked before the window loop:

- **Bar mode, one series.** The splitter divides a single bar series by index, so a tick-mode or
  multi-symbol profile is a clean validation error, never a silent fallback to the first symbol.
- **The annualization is DERIVED from the bar interval**, never a flat daily anchor, so an hourly
  profile is not scored on a 252 factor.
- **The window shape is the profile's business, not the tool's.** `n_splits` is one of two forms -
  a `train`/`test` duration pair is the other - and `[walkforward].mode` (`anchored` | `rolling`)
  is settable too. Under the FIXED-parameter walk the mode cannot MOVE the report: the two modes
  differ in `train_start` alone and this driver discards the training half, an invariance that is
  pinned by a test rather than promised here
  (`crates/vike-backtest/src/harness/walkforward.rs`'s `run_walkforward`).
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
`[strategy.params]` profile without `[paramscan]` or `[walkforward]` and call `run_backtest` with
`profile` (required) and optional `script`; the result is `{ "report": ... }`. It runs under the
same parser and the same `[engine]`, so its numbers equal the sweep row's - it adds detail, not
evidence. Only the walk-forward adds evidence.

## Optional - at the engine: the roster, the trade floor, the progress stream

Three things are typed at the standalone `backtest` engine and at no other door. They are not
arguments of `run_sweep`, and they are not flags of `vike-cli backtest run` either - that parser
answers an unknown flag by name. `crates/vike-datahub-client/src/proto.rs`'s `WireSearch` carries
four fields (`optimizer`, `euler_depth`, `trials`, `seed`) and neither observer below is one of
them, so there is nothing for a remote caller to have asked for. Say that, rather than inventing an
argument for a tool.

**Which methods exist - `--list-optimizers`.** It answers out of a const: no store, no profile, no
network, and no positional profile is even looked for.

```sh
backtest --list-optimizers
```

```
grid
euler
tpe
genetic
```

`--list-optimizers --json` prints one object on one line instead, carrying the two facts the plain
listing deliberately omits: `count`, and `default` - which method runs when `--optimizer` is
absent. The plain form has no header, no count and no default marker, so a pipeline strips nothing.
Both halves render `crates/vike-datahub-client/src/proto.rs`'s `SEARCH_METHODS`, the same const the
engine's `--optimizer` spelling check, the wire frame's own field and this skill's `optimizer`
argument all read - so the listing cannot advertise a method the search would refuse, and a fifth
method would appear here with nothing edited.

⚠ **There is deliberately no `--list-optimizers` on `vike-cli`.** A client with no engine answers
the same question from the PUBLISHED surface asset instead: `vike-cli surface --out DIR` writes
`cli.json`, whose `rosters` carry an `optimizers` row (and a `rank_metrics` one). Read the asset;
do not go hunting for a flag that is not there.

**The trade floor - `--min-trades N`.** Step 3's first reading rule, enforced rather than
remembered: a trial that closed fewer than N trades is UNRANKABLE, under every method and every
`--rank-by`.

```sh
backtest sweep.toml --rank-by sharpe --min-trades 30
```

It is not the soft trade-count penalty inside the `multi` objective, and the difference is the
whole point of having it: that penalty is floored so a thin-but-promising point stays VISIBLE, and
it is not applied at all under the four classic metrics - which includes the default `sharpe`, the
invocation almost everybody actually types. A floor that can be out-earned is not a floor. `0` is
the default AND what an explicit `--min-trades 0` means, so a sweep matrix can pass the flag
unconditionally without a special case.

⚠ **A floored row is not dropped, and that is how you see why it fell.** Its score becomes NaN -
this crate's written value for "unrankable", which sorts behind every finite score under every
method - while its `report` is left untouched, so `n_trades` stays readable in the `--json`
document. Nothing is written into `error`: a row carries a report XOR an error, never both
(`crates/vike-backtest/src/harness/optimize.rs`'s `apply_trade_floor`).

**The progress stream - `--progress auto|none|json`.** On stderr, and structurally never stdout, so
a `--json` report stays parseable while a search is being watched.

| mode | what it emits |
| --- | --- |
| `auto` (default) | the throttled human line, and ONLY when stderr is a terminal |
| `none` | silent - nothing constructed, nothing counted |
| `json` | one newline-delimited event per point, terminal or not, unthrottled |

`auto` is not "on": it is "on IF a human is watching". The terminal probe happens once, at
construction, never per point - and the default is that way because appending a line per backtest
to a captured log nobody reads is how a log directory in this workspace once reached 341 GB
(`crates/vike-backtest/src/harness/optimize.rs`'s `ProgressMode`).

Watching Step 1's nine-point grid, a human sees lines like

```
backtest: searched 4/9 points in 6.8s, eta 8.5s — best 1.8342
```

while a machine asks for the same events as newline-delimited JSON on stderr, which leaves stdout a
clean report:

```sh
backtest sweep.toml --json --progress json 2>progress.ndjson
```

```
{"event":"search_progress","completed":4,"total":9,"elapsed_s":6.812,"eta_s":8.515,"best":1.8342}
```

Reading that event:

- `total` is the GRID SIZE under `grid` and your own `--trials` under `tpe`. Under `euler` and
  `genetic` it is `null`, because neither can promise an evaluation count before it runs - and
  `eta_s` is `null` with it rather than a number nobody can stand behind.
- `best` is the best score AFTER the trade floor, so a progress line can never advertise a leader
  the printed report will bury. It is `null` until something is rankable and STAYS `null` through a
  run where nothing is - which is exactly what an over-tight `--min-trades` looks like from outside.
- The human line is throttled to about twice a second; the JSON stream is not throttled at all. The
  final event of a run with a known total always passes the throttle, so the last line reports the
  finished count instead of freezing at 8/9 above a printed report.

⚠ **Never assert on the stream.** Under the default parallel pool events arrive in COMPLETION order
with wall-clock times on them, so two runs of one search emit the same events in a different order
carrying different numbers. Nothing in it reaches the report, which is what makes a clock inside a
byte-compared seam admissible at all.

⚠ **Both flags name a SEARCH, and neither travels.** On a profile with no `[paramscan]` table the
engine refuses before it runs anything - `backtest: --min-trades names a parameter SEARCH, but
"run.toml" has no [paramscan] table` - rather than ignoring the flag, which is what a lone
`--rank-by` does there. And neither crosses the wire: a remote sweep has no floor and no progress
stream, so over `run_sweep` you apply Step 3's rule by reading `n_trades` yourself and you wait
without a counter.

## Reporting template

State, in this order: the grid size and `rank_by`; the top rows with their `overrides`, return,
Sharpe, max drawdown and trade count; which rows failed and why; then the walk-forward's
`n_splits`, each window's `oos_return`, `wf_consistency`, stitched `oos_return` and `oos_sharpe`;
and finally the narrow reading above. Never present the in-sample rank alone as a result.

Human twins: `vike-cli backtest run --profile <run.toml> --rank-by sharpe` (a profile carrying a
`[paramscan]` table IS a search — there is no separate `sweep` verb) and
`vike-cli backtest run --profile <run.toml>` on a profile carrying a `[walkforward]` table (there
is no separate `walkforward` verb either — walking forward is how a run is VALIDATED, and the
profile's own window declaration selects it; declaring both sections composes)
(`https://vike.io/docs/trader/guides/sweep-on-the-service`,
`https://vike.io/docs/trader/tutorials/sweep-and-walk-forward`).

⚠ **Do not carry `--rank-by` or `--optimizer` over onto the walk-forward command.** On a profile
that declares a `[walkforward]` table each is refused BY NAME, exit 2, before anything is sent -
that wire verb carries the profile TOML and nothing else, so a selector typed there would configure
nothing at all, and being told so beats being ignored. A window's ranking is
`[walkforward].rank_by`, in the profile, read by the one parser that runs it. That is a different
case from a lone `--rank-by` on a profile with no grid at all, which is deliberately IGNORED rather
than refused: it says how to order results, not what work to do.

Tool references:
`https://vike.io/docs/ai/trader/tools/run_sweep`, `https://vike.io/docs/ai/trader/tools/run_walk_forward`.
