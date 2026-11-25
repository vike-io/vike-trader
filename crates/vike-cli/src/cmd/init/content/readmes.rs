//! The README each `user_data` folder ships, one const per file `vike-cli init` writes.

// ─────────────────────────────────────────────────────────────────────────────────────────────
// strategies/
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `<project>/strategies/README.md` — the folder's layout, the host surface a Rhai script may call,
/// and the ONE runtime rule that silently ruins a script that breaks it (the indicator warning below).
///
/// ⚠ It carries ONE slot, [`DEFAULT_RUN_CASH_SLOT`], which `init` fills with
/// `vike_model::run_request::profile::DEFAULT_RUN_CASH` when it writes the file: a raw string full of
/// Rhai braces cannot be a `format!` template.
pub const RHAI_README: &str = r##"# Strategies

One FILE per strategy, subfolders as you like: `trend/sma_cross.rhai` is that file's name
everywhere (a profile's `[strategy] file`, `backtest params --strategy`). Copy a file in and run it:
nothing is rebuilt and nothing registers it.

## Layout

```text
trend/sma_cross.rhai            a starter `vike-cli init` wrote: long or flat, on two averages
trend/ema_trend.rhai            long-only trend filter
mean_reversion/rsi_meanrev.rhai fades the extremes
other/example.rs                the release's smallest Rust file (copied in by `vike-cli init`)
```

Every file opens with a **header**: a fenced `toml` block in its leading comment lines, declaring
where it may run (`[strategy] runs_on`, `paper` when absent) and every knob it reads, each with its
type (`[params.<key>]`). A profile key the header does not declare is REFUSED, naming the keys it
does, so a typo cannot quietly run at a default. A `.rhai` file runs in the engine's own process
on every install; a `.rs` file is compiled by the strategy builder and runs in a sandboxed process,
on a Linux box that has one.

Presets (param tables for these three) are in `../user_data/presets/`.

## Run one

```sh
vike-cli backtest run --local --strategy trend/sma_cross.rhai \
    --venue <VENUE> --symbol <SYMBOL> --interval <INTERVAL> \
    --from <FROM> --to <TO> --cash <CASH>                            # over bars you loaded
vike-cli backtest params --strategy trend/sma_cross.rhai             # its knobs, offline
```

`<FROM>` and `<TO>` are UTC hours, `YYYY-MM-DDTHH` (or epoch-ms), inside the bars you loaded;
`<CASH>` is the starting balance. The window is required, a run has no default one; leave `--cash`
out and the run starts with {DEFAULT_RUN_CASH}.

## The host surface

`vike-cli backtest script-api` prints all of it, read off the binding itself, with a line on each
name. The shape:

```rhai
let equity_percent = param("equity_percent", 10.0);   // a knob, at the top level
fn on_bar(strategy) {                    // the one hook a strategy needs
    let e = ema(20);                     // indicators first, every bar
    if e.is_nan() { return; }
    if close() > e {                     // a share of the equity, never a fixed coin quantity
        strategy.entry("Long", OrderSide::Long).size(strategy.equity() * equity_percent / 100.0 / close());
    } else {
        strategy.close("Long");
    }
}
```

**Read the bar / account**

| call | returns |
|---|---|
| `open()` `high()` `low()` `close()` `volume()` | this bar |
| `position()` | signed position, your strategy's symbol |
| `price()` | the current mark |
| `equity()` | account equity |
| `index()` | bar number, from 0 |
| `now()` | engine clock, epoch ms |

**Place orders — only through `strategy`**, the argument your hooks take. Every order has a NAME:
naming one again while it still rests replaces it rather than sending a second, so two different
orders in one bar need two names. A side is `OrderSide::Long` or `OrderSide::Short`.

- `strategy.entry(name, side)` — an entry: a market order unless `.stop`/`.limit` sets a level. It
  closes an opposite position first, and sends nothing while that entry is already held.
- `strategy.order(name, side)` — a plain order of exactly the size stated, neither an entry nor a
  close.
- `strategy.close(name)` / `strategy.close_all()` — close what the entry `name` opened / the whole
  position, at market.
- `strategy.exit(name)` — a stop, a target and a trail protecting what the entry `name` opened.
- `strategy.cancel(name)` / `strategy.cancel_all()` — cancel the orders under `name` / every order.

Options chain onto the verb. On an entry or an order: `.size(qty)` (without it, the header's size,
1 by default), `.stop(level)`, `.limit(level)`, `.group(name)`. On a close: `.size(qty)`, `.here()`.
On an exit: `.from(name)`, `.size(qty)`, `.stop(level)`, `.take(level)`, `.stop_pct(pct)`,
`.take_pct(pct)`, `.trail(#{ after_ticks, by_ticks })`, `.group(name)` — e.g.
`strategy.exit("Long").stop_pct(1.0).take_pct(2.0)`. The object reads too: `strategy.position()`,
`strategy.avg_cost()`, `strategy.equity()`, `strategy.price()`, `strategy.lots()`, and more that
`script-api` lists.

⚠ **`buy`, `sell`, `market` and `limit` were removed.** A script that still calls one is REFUSED
AT LOAD, and the error names the spelling that replaced it — `buy(qty)` is now
`strategy.order("buy", OrderSide::Long).size(qty)` inside `fn on_bar(strategy)`. Check a file before you
run it:

```sh
vike-cli backtest script-check --script trend/sma_cross.rhai
```

**Indicators — the catalog, not a handful.**

```sh
vike-cli indicators                       # every name this build binds, by category
vike-cli indicators --category momentum   # one family
```

Call one the way the examples do — `sma(20)`, `rsi(len.to_int())`: numbers in, one number out.
Every listed indicator accepts anything from NO arguments (all of its defaults) up to its full
parameter list, and the listing prints those parameters with the values you get by leaving them out.

⚠ **A name that command does not print is not callable** — and a few of them are real
`vike-indicators` names held back on purpose, not typos. Ask which:

```sh
vike-cli indicators --name bollinger
```

That answers with the reason, so a missing name never has to be guessed at. What calling one anyway
looks like is the ⚠ rule below, and it does not look like an error.

**Knobs** — `param(name, default)` returns the default, or the value a sweep injected. Call it at
the TOP LEVEL (outside any `fn`); that is where `backtest params` and a `[paramscan]` grid look.

**Hooks** — `fn on_bar(strategy)` runs once per bar and is the one a strategy needs. Beside it,
each optional:

- `fn on_tick(strategy)` — a live quote, at most ten times a second, from the first bar on (a
  backtest on bars never calls it). Read `price()` there; keep every indicator call in `on_bar`.
- `fn after_fill(strategy)` — right after one of your own fills.
- `fn on_stop(strategy)` — as the strategy stops. It may only READ: an order there is refused.
- `fn defaults() { #{ size: 1.0, pyramiding: 1 } }` — the size an order takes when it states none,
  and how many entries may add up.
- `fn inputs() { ["1d"] }` — other series, read with `strategy.input(name)`.

The zero-argument hooks run too — `fn on_start()` once before the first bar, `fn on_bar()` once
per bar, `fn on_tick()` on a quote, `fn on_stop()` once after the last bar. They read, compute and
`print`, and place NO order: orders go through `strategy`, whose hooks are the ones above. A script
defines each hook one way (`fn on_bar()` or `fn on_bar(strategy)`, not both).

## ⚠ The one rule that will silently ruin a strategy

**Call every indicator on EVERY bar, unconditionally, before any branch or early return.**

An indicator only advances when your script calls it. Call `sma(20)` inside an `if` and it never
sees the bars where the branch was not taken — so it is no longer a 20-bar average of anything, and
it will not tell you. Every shipped example reads its indicators on the first lines of
`on_bar(strategy)` for exactly this reason.

```rhai
fn on_bar(strategy) {
    let f = sma(10);            // ✅ always, every bar, first
    let s = sma(30);
    if s.is_nan() { return; }   // then warm-up, then logic
    ...
}

fn on_bar(strategy) {
    if strategy.position() == 0.0 {
        let f = sma(10);        // ❌ skips every bar you hold a position
    }
}
```

Smaller rules that follow from the same place:

- **Warm up.** An indicator returns `NaN` until it has enough bars. Guard with `is_nan()` — the
  examples all do.
- **A period may be written either way.** `param()` hands you a float and an indicator takes a
  float or a whole number, so `sma(fast)` and `sma(fast.to_int())` both work; the examples convert
  because a period reads better as `20` than `20.0`. A STRING does not work — `sma("20")` is `NaN`
  for the whole run rather than a silent `20`, deliberately, so a typo cannot look like a number.
- ⚠ **A function that does not exist still compiles.** Names are resolved when the line RUNS, so a
  call to something unbound is not a load error — it fails on every bar, and after ten consecutive
  failures the strategy switches itself off. **A misspelled indicator therefore looks exactly like
  a strategy that saw no signal**: a flat equity curve, no error, nothing to notice.
  `../user_data/logs/compile.log` is where it says otherwise, and `vike-cli indicators` is the list of
  names that will not do this to you. (The four removed order functions are the exception: those
  are refused at load, above.)
- **No ternary.** There is no `?:` in Rhai — `if` is an expression, so write
  `let size = if f > s { qty } else { qty / 2.0 };`. Beyond the calls above, the host binds the
  standard arithmetic set — `abs`, `sqrt`, `floor`, `ceiling`, `round`, `to_int`, `is_nan`, `min`,
  `max` — and the usual operators.

## When a script misbehaves

A hook that errors places no orders that bar and the run continues. After 10 consecutive errors the
strategy switches itself off for the rest of the run. Both are recorded in `../user_data/logs/compile.log`
— look there first when a strategy loaded but never traded.

## In a source checkout

`crates/vike-rhai/src/engine/bindable.rs`'s `RHAI_INDICATORS` is the authoritative set of bound indicator
names — `vike-cli indicators` prints it, so that command and this page never need updating when an
indicator is added — and `crates/vike-rhai/src/engine/builtin.rs`'s `register_indicators` is the
authority for the CALL SHAPE (if it ever grows a richer one, the paragraph above is the stale half).
`crates/vike-rhai/src/engine/host.rs`'s `register_reads` is the authoritative read surface,
`crates/vike-rhai/src/tactics.rs`'s `register` the `strategy` object (its module doc is the
spelling), and `crates/vike-rhai/src/strategy.rs`'s `REMOVED_FUNCTIONS` the removed names; and
`crates/vike-rhai/src/lib.rs`'s own module doc is the authority for the call-unconditionally rule
above, down to why `engine::indicator_value`'s fed-once-per-bar cache makes a conditional call
silently wrong. If this README and those files ever disagree, they are right.
"##;

/// The slot [`RHAI_README`] carries where the default run cash goes, filled when `init` writes it.
pub(crate) const DEFAULT_RUN_CASH_SLOT: &str = "{DEFAULT_RUN_CASH}";

// ─────────────────────────────────────────────────────────────────────────────────────────────
// profiles/
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `profiles/README.md`. No profile ships beside it (the owner's ruling of 2026-11-13): a profile is
/// the user's own run recipe.
pub const PROFILES_README: &str = r##"# Run profiles

A profile answers three questions: **which data**, **which costs**, **which strategy**. It is the
whole input to a run, so a result is reproducible by keeping the file that produced it.

**None ships.** A profile is YOUR run recipe: the Studio saves one, or you write one. A run needs
none — flags build it, and `--write-profile FILE` keeps what they built:

```sh
vike-cli backtest run --local --strategy trend/sma_cross.rhai \
    --venue <VENUE> --symbol <SYMBOL> --interval <INTERVAL> \
    --from <FROM> --to <TO> --cash <CASH> --write-profile mine.toml
vike-cli backtest run --local --profile mine.toml
```

The strategy is a FILE: `[strategy] file = "trend/sma_cross.rhai"`, a path under the project's
`strategies/` folder (`../../strategies/` from here).

Add `--addr HOST:PORT` to reach a backtesthub (`vike-backend backtesthub --addr`) that is not on
`127.0.0.1:7880`, and `--json` for machine-readable output.

## ⚠ Two things to get right

1. **The data slice must exist in your store.** A fresh install holds NO market data and none ships:
   load real bars first (`vike-cli data history import`, a venue fetch, or `--source starter`). A slice
   with no data is a run with no trades, which reads like a bad strategy rather than an empty query.
2. **The costs are a guess about your venue, not a fact.** A run charges the strategy's own
   commission and slippage, else none: set `[strategy.params] commission` and `slippage` to your
   venue's. Too low and a dead strategy looks alive.

## Fields worth knowing

- `[data] kind` — `"bar"` or `"tick"`. `interval` applies to bars.
- `[data] from` / `to` — `YYYY-MM-DDTHH` UTC, or a bare epoch-ms integer as a string.
- `[engine] cash` — starting equity, and the denominator of every percentage in the report.
- `[strategy] file` — the strategy file, a path under `strategies/`: a `.rhai` file runs in the
  engine's own process, a `.rs` file in a sandboxed one.
- `[strategy.params]` — knobs. Each must be declared in the file's header with its type: a key the
  header does not declare is refused, naming the keys it does.
- `[paramscan]` — each key names a `[strategy.params]` field and lists values to cross-product.
- `[walkforward] n_splits` — how many anchored out-of-sample windows to walk.
"##;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// backtest_results/ and notebooks/
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `backtest_results/README.md`.
pub const RESULTS_README: &str = r##"# Backtest results

Reports you chose to keep. Nothing writes here on its own — `--json` prints to stdout, and saving
it is your decision:

```sh
vike-cli backtest run --local --strategy trend/sma_cross.rhai \
    --venue <VENUE> --symbol <SYMBOL> --interval <INTERVAL> \
    --from <FROM> --to <TO> --cash <CASH> --json > sma_cross.json
```

No result ships: a result nobody ran would be invented numbers. This folder fills as you save runs.

## The shape

One JSON object per run:

| field | meaning |
|---|---|
| `name` | the run's `name` |
| `final_equity` | equity at the last bar |
| `total_return` | fraction, first to last equity point — `0.0731` is 7.31% |
| `n_trades` | closed round trips |
| `win_rate` | fraction of trades with positive PnL |
| `sharpe` | annualized, from per-bar returns |
| `max_drawdown` | largest peak-to-trough drop, positive fraction |
| `profit_factor` | gross profit / gross loss. `null` when there is no meaningful ratio. |
| `funding_paid` | net perp funding; `0.0` for spot |
| `per_symbol_pnl` | `[symbol, pnl]` pairs, multi-symbol runs only |

⚠ **`sharpe` is annualized with 252 periods for anything that is not daily bars.** On 1-hour or
1-minute data the number is understated by a large factor. Compare runs to each other; do not
compare one to a published Sharpe.
"##;

/// `notebooks/README.md`.
pub const NOTEBOOKS_README: &str = r##"# Notebooks

Analysis over the files in `../backtest_results/`.

`backtest_report.ipynb` reads the newest result saved in `../backtest_results/` and prints a
summary. No result ships, so it says so until you have saved one; point it at another file by
changing one line.

```sh
jupyter lab backtest_report.ipynb
```

The first cells use only the Python standard library, so they run in any interpreter with no
install. The plotting cell needs `matplotlib` and says so; skip it if you do not have it.

## Notebooks are yours

Nothing here is read by the program. This folder is a place to keep your analysis beside the
results it analyses, so a notebook and its input travel together when you copy `user_data/`.
"##;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// logs/
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `logs/README.md` — the ONE folder that ships no example.
///
/// See this module's doc: a committed sample log would be invented timestamps describing a run that
/// never happened, and `compile.log` is rewritten on the next start regardless. The README is what
/// keeps the folder discoverable.
pub const LOGS_README: &str = r##"# Logs

Logs for work **you** ran. The program's own runtime log lives elsewhere (under `settings/state/`);
this folder is yours to read and delete.

This folder is empty until something runs. That is expected — there is no sample here, because a
committed example log would be invented timestamps describing a run that never happened.

## `compile.log`

**Every strategy load, pass and fail. Look here first when a strategy does not appear.**

Written whenever strategies are loaded, and replaced on each start. A strategy that is missing from
the app, or that mounted but never traded, has its reason recorded here — a parse error with a line
number, an unknown indicator name, or the point at which a script errored ten times in a row and
switched itself off.

It is always written: it is bounded by how many strategies you have, not by how long anything runs.

## Backtest run traces

Opt-in, and off by default — their size grows with runtime, and an unbounded log in this workspace
has previously reached gigabytes in an hour. Turn one on per run when you need it.

⚠ `$VIKE_LOG_DIR` does not redirect this folder. That variable moves the APP log; if your run
history followed it, setting it for a service would silently relocate your backtests.
"##;
