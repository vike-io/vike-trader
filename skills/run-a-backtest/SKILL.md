---
name: run-a-backtest
description: Run a strategy over stored history on the Vike backtest daemon (series inventory from a vike-datahub) and read the report. Use when the user asks to backtest, test a strategy on history, run a profile, check whether a strategy trades or profits on past bars, or try a Rhai script or a built-in strategy against BTCUSDT (or any stored series). Also use when the user asks whether the server actually HAS the history a profile names, wants to see what a run will read first, asks to plan or dry-run a backtest, wants a run refused when the window is not covered, asks about missing bars, gaps or survivorship bias in a symbol list, or asks how a multi-symbol step is decided. Covers the profile TOML, discovering series and strategy names (list_series, list_strategies), planning with data.explain, arming the coverage gate (require_coverage, max_gap, on_gap, universe, engine.decide), calling run_backtest, and reading BacktestReport (final_equity, total_return, sharpe, max_drawdown, zero_trade). Needs no credentials.
metadata:
  tools: "list_strategies list_series run_backtest"
  source: "ai/trader/backtesting ai/trader/tools/run_backtest ai/trader/tools/list_series ai/trader/tools/list_strategies trader/tutorials/rsi-mean-reversion-backtest"
---

# Run a backtest

A backtest runs a strategy over stored history and returns a `BacktestReport`. ⚠ TWO servers answer
this skill and they are separate processes: `list_series` reads a `vike-datahub`'s inventory, while
`run_backtest` runs on the COMPUTE daemon at its own address — step 4 is where that starts costing
you something. It mutates nothing: the three tools this skill uses are all read-only, and the
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

`[data]` holds more than the keys that locate a series. It also carries the ones that answer *is
there enough history here to believe this run* — `explain`, `require_coverage`, `max_gap`, `on_gap`
and `universe` — and they matter most on THIS route, because the store sits on the server where you
cannot look at it. Absent, every one of them is byte-identical to a profile written before they
existed. Step 4 is the sequence that uses them.
(`crates/vike-backtest/src/harness/profile.rs`'s `DataCfg`.)

⚠ At a shell those same five are FLAGS on `vike-cli backtest run` — `--explain-data`,
`--require-coverage`, `--max-gap`, `--on-gap`, `--universe` — sugar that writes exactly these keys,
so the two routes cannot answer differently:

```sh
vike-cli backtest run --profile run.toml --explain-data
vike-cli backtest run --profile run.toml --require-coverage --max-gap 1d --on-gap warn
```

Over a TOOL CALL there are no flags: the profile text is the entire input, which is why these are
keys at all. Do not look for a tool argument named after one, and do not put a `--flag` in the
TOML. (`--max-gap` and `--on-gap` are refused without `--require-coverage` — by the PROFILE, on the
far side, because they are the same keys; the whole flag table is published at
`crates/vike-cli/tests/fixtures/cli.json`.)

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

If the store is empty, tell the user how to fill it from the CLI: `vike-cli data seed-demo` writes
a credential-free demo tape under venue `demo` (BTCUSDT hourly, 2025-01-01 to 2025-07-01, plus the
last seven days at 1m), and `vike-cli data fetch binance:BTCUSDT:1h --from … --to …` pulls real
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
- `name = "rhai"` when you will inject a script (step 5).

A params table is a READER, not a schema: keys the strategy does not read are ignored in silence
and every unread knob keeps its default. Check the knobs you set are ones the strategy reads.
(`https://vike.io/docs/trader/tutorials/rsi-mean-reversion-backtest`.)

**More than one symbol? Then `[engine] decide` is a decision you are making either way.** Absent
(`"sequential"`) walks the symbols in the order `data.symbols` lists them — a list order nobody
chose, no profile records and no report shows. `"simultaneous"` makes the answer a property of the
instrument SET instead: the cross-sectional folds and the shared-cash allocator's tie-break walk a
canonical, name-derived order that no permutation of your list can change.

It is not free packaging, and the profile refuses the three combinations that would let a run claim
a cross-section it did not compute:

- it **implies `engine.cash_gate`**, so the whole bar step routes through the shared-cash admission
  phase and the granular sub-bar lane is off — `data.detail_interval` is refused beside it, because
  the detail tape would be loaded, paid for and ignored;
- `kind = "tick"` is refused: the allocator it orders is reached only from the bar step, so a tick
  profile setting it would change nothing;
- an armed `[risk]` table or `engine.leverage` is refused — a DECLARED GAP rather than a
  preference. With a pre-trade gate armed the symbol judged first faces the whole step budget and
  later ones face the remainder, so the DECISION is still taken in list order; closing it needs a
  step-boundary budget snapshot that is not built.

Each of those comes back as `profile parse/validate failed: …` carrying that reason, before
anything computes. At a shell the same key is `--decide sequential|simultaneous`.
(`crates/vike-backtest/src/harness/profile.rs`'s `EngineCfg` and its `refusals`.)

### 4. Plan before you compute — `data.explain`

⚠ **`list_series` is not looking at the store the backtest will read.** That call goes to the
datahub at `--addr`; `run_backtest` goes to the COMPUTE daemon at `--backtest-addr`, a separate
process with its own `--store` / `VIKE_HIST_STORE`. A series the inventory listed is therefore not
by itself evidence the backtest daemon holds it. `data.explain` is how you ask the side that will
actually run.

Set it and call `run_backtest` exactly as you would to run:

```toml
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "2024-01-01T00"
to = "2024-06-01T00"
explain = true

[strategy]
name = "buy_hold"
```

Nothing computes. The profile is still parsed and validated first — a plan for a profile that could
not run is a plan for nothing — and then the server returns the PLAN as the run's report document:
`{ "report": { "explained": true, … } }`. **Test `explained` to tell a plan from a report**, since
both arrive under the same key. It needs no wire verb of its own for the same reason the flags are
keys: only the profile text crosses.
(`crates/vike-backtest/src/compute_server.rs`'s `explain_instead_of_running`.)

The document carries its own rendered SENTENCES as well as its fields, because this client has no
renderer for a shape it has never seen. Read `report.explain` — an array of lines — before the
fields:

| line | what it answers |
| --- | --- |
| `store: …` | which store answered. ⚠ On this route it names no path and no rung: the daemon was handed an already-open store and resolved no root, so the sentence points at the daemon's own `--store`/`VIKE_HIST_STORE` instead. A path printed here would be the most confidently wrong field in the document |
| `window: <from> .. <to>` | the resolved window, an unbounded side spelled `-inf`/`+inf` |
| `slice: kind=… interval=… · N series, M held · R rows` | how many series this run opens, and how many the store actually holds |
| `note: …` | what is not per-series — an un-enumerable store, a `[walkforward]` table that will slice this window, a gap query the store refused |
| `series kind=… venue=… symbol=… interval=… [role]` | one per series (`group=…` in place of `symbol=…` for a grouped one), with its role (`price`, `detail`, `funding`, `properties`), its rows/days/parts/bytes and its recorded span — or `NOT HELD`, or `PRESENT AND EMPTY` |
| `    MISSING <kind> <span> (<len>)` | indented under its series: what the window asked for and that series does not hold |
| `total: …` | the shortfall, ⚠ SUMMED ACROSS SERIES — one day short on two series reads as two days |

`M held` beside `N series` is the line most worth reading first: a run whose price lane is
`NOT HELD` has nothing to compute, and nothing in a report would have said so. The gate below is
what stops it. (`crates/vike-backtest/src/data_plan.rs`'s `DataPlan`.)

#### Arm the gate, and see its verdict before you obey it

`require_coverage = true` arms the coverage gate: a run whose window the store does not cover is
refused, with the missing spans named per series. It exists for one failure — a window with a
complete trade tape and no book at all runs to completion and **reports fills**, while per series
nothing looks wrong. Two keys tune it, and **both are refused without it**, because a disposition
or a tolerance for a gate nobody armed is a key that reads as though it stopped something:

- `max_gap = "1d"` — how much ONE missing span may be before it is a finding; absent is zero
  tolerance. Per SPAN, not a total — and a series the store does not hold AT ALL is exempt under
  every value, because that is a lane that was never recorded rather than a gap of some length. A
  bar count (`"200bars"`) and a calendar span (`"3mo"`) are refused by name: write fixed time.
- `on_gap = "refuse" | "warn" | "run"` — what an armed gate DOES. `refuse` is the default, because
  a gate that only warns is invisible on every run nobody reads. `run` leaves it inert while
  keeping the file's statement of what this run is supposed to require.

⚠ **`warn` reaches the DAEMON'S LOG, not your tool result.** The findings go out through
`tracing::warn!` on the server; the report comes back unchanged and carries nothing about them —
and the same is true of `universe = "covered"`. An agent that arms `warn` and reads the report has
armed nothing it can see. That is the whole reason to plan first rather than run and read.
(`crates/vike-backtest/src/harness/run.rs`'s `run_backtest`.)

`universe` is the point-in-time membership rule, and a different question from coverage: `declared`
(the default) takes the symbol list verbatim, `covered` names every member whose tape does not span
the window and runs anyway, `strict` refuses that run. It judges the ENDS of the window — a member
that was not there when it opened, or that stopped before it closed — where the coverage gate
judges interior holes. The bias it catches is the one that bites: a symbol list chosen TODAY is a
list of survivors. ⚠ **None of the three ever drops a member**, deliberately — editing the universe
stays your act, because a universe a run narrowed silently would not be the one the profile names.

So the sequence is three calls, and the middle one is the point:

1. `explain = true` alone — what does the store hold.
2. `explain = true` **beside** `require_coverage = true` (and `max_gap` / `on_gap` / `universe`) —
   the same plan, plus a `coverage` field whenever the gate is armed and a `universe` field
   whenever the rule is not `declared`. When either has a finding, `report.explain` grows a
   `-- data.require_coverage would report --` or `-- data.universe would report --` section saying
   what it is. Still nothing computes: the explain door is the first thing the server does, so it
   answers BEFORE the gate could refuse. This is how you read a verdict without obeying it.
3. Drop `explain`, call `run_backtest` again. Now the gate enforces and the run happens.

(`crates/vike-backtest/src/data_plan.rs`'s `explain_document` and `enforce`.)

### 5. Run it — `run_backtest`

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
- Connect failure is a clean error naming the daemon it could not reach and how to start one —
  `cannot connect to the backtest daemon at …`, ending in the sentence that names
  `vike-backend backtest --addr` as the thing to start.
  ⚠ That is a DIFFERENT address from the one `list_series` dials: the inventory comes from
  the datahub at `--addr`, this run from the compute daemon at `--backtest-addr`, so one being up
  says nothing about the other.
- A daemon advertising the `auth` feature refuses at connect with an actionable message (every run
  verb is control scope); the default key-less one is unaffected.

(`crates/vike-cli/src/cmd/mcp.rs`'s `tool_run_backtest` and `tools_spec`;
`https://vike.io/docs/ai/trader/tools/run_backtest`.)

### 6. Read the report

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

- `cannot connect to datahub at …` — no server at `--addr` (`list_series`); nothing to retry until
  one is running. `cannot connect to the backtest daemon at …` is the other one, at
  `--backtest-addr`, and the two are separate processes.
- `profile parse/validate failed: …` — fix the profile shape from the message (unknown strategy,
  bad range, series not in the store); re-run `list_series`/`list_strategies` if unsure.
- `profile parse/validate failed: … data.on_gap is set without data.require_coverage = true …` (or
  the same for `data.max_gap`) — arm the gate, or remove the key. It is refused rather than
  ignored precisely because it reads as though it stopped something.
- `harness data error: data.require_coverage is armed …` (ending `Nothing ran.`) — the gate fired,
  and the missing spans are named in the message. Fill them, widen `max_gap` if the gap is
  genuinely acceptable, or set `on_gap = "warn"` — and re-plan with `explain = true` rather than
  guessing which of the three you meant.
- `harness data error: data.universe = "strict" …` (ending `Nothing ran.`) — move `from` to a date
  every member covers, drop the members that were not there, or set `universe = "covered"` to run
  with the finding on the record. Nothing narrows the slice for you.
- A flat curve from a Rhai script — a misspelled indicator errors on every bar and, after ten
  consecutive errors, the strategy switches itself off and looks mounted but never trades.
- Silent wrong result — a params key the strategy never reads (see step 3).
