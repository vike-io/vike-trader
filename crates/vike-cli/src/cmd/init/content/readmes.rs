//! The README each `user_data` folder ships, one const per file `vike-cli init` writes.

// ─────────────────────────────────────────────────────────────────────────────────────────────
// strategies/
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `strategies/rhai/README.md` — the host surface a script may call, the layout rule, and the ONE
/// runtime rule that silently ruins a script that breaks it (see the indicator warning below).
pub const RHAI_README: &str = r##"# Rhai strategies

Interpreted strategies. They are read at startup, so they work on **every** install — a release
binary needs no toolchain to run one. This is the folder to use unless you specifically need Rust.

## Layout: one folder per strategy, entry file named after the folder

```text
sma_cross/
  sma_cross.rhai     the strategy — the name matches the folder
  fast.toml          a preset: a set of params for THIS strategy
  slow.toml
```

The entry file matching the folder name is what makes a folder holding two scripts unambiguous. A
`.toml` beside it is a **preset** for that strategy — presets live with their strategy so
`rm -r sma_cross/` removes the whole thing, and two strategies' presets can never be confused.

⚠ A preset is a param table you paste into a profile's `[strategy.params]`. Nothing loads one
automatically yet.

## Run one

```sh
vike-cli backtest run --profile ../../profiles/backtest.toml --script sma_cross/sma_cross.rhai
vike-cli backtest params --script sma_cross/sma_cross.rhai         # its knobs, offline
```

`--script` injects the file's source into the profile's `[strategy.params].src`, so the script
travels with the request and the profile stays about the DATA.

## The host surface — this is all of it

**Read the bar / account**

| call | returns |
|---|---|
| `open()` `high()` `low()` `close()` `volume()` | this bar |
| `position()` | signed position, your strategy's symbol |
| `price()` | the current mark |
| `equity()` | account equity |
| `index()` | bar number, from 0 |
| `now()` | engine clock, epoch ms |

**Place orders**

| call | does |
|---|---|
| `buy(qty)` / `sell(qty)` | market order, that side |
| `market(side, qty)` | market order; `side` is `1` or `-1` |
| `limit(side, qty, price)` | resting limit order |

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

**Hooks** — `on_start()`, `on_bar()`, `on_stop()`, all zero-argument. Define the ones you need.

## ⚠ The one rule that will silently ruin a strategy

**Call every indicator on EVERY bar, unconditionally, before any branch or early return.**

An indicator only advances when your script calls it. Call `sma(20)` inside an `if` and it never
sees the bars where the branch was not taken — so it is no longer a 20-bar average of anything, and
it will not tell you. Every shipped example reads its indicators on the first lines of `on_bar()`
for exactly this reason.

```rhai
fn on_bar() {
    let f = sma(10);            // ✅ always, every bar, first
    let s = sma(30);
    if s.is_nan() { return; }   // then warm-up, then logic
    ...
}

fn on_bar() {
    if position() == 0.0 {
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
  `../../logs/compile.log` is where it says otherwise, and `vike-cli indicators` is the list of
  names that will not do this to you.
- **No ternary.** There is no `?:` in Rhai — `if` is an expression, so write
  `let target = if f > s { qty } else { -qty };`, as the examples do. Beyond the calls in the
  tables above, the host binds the standard arithmetic set — `abs`, `sqrt`, `floor`, `ceiling`,
  `round`, `to_int`, `is_nan`, `min`, `max` — and the usual operators.

## When a script misbehaves

A hook that errors places no orders that bar and the run continues. After 10 consecutive errors the
strategy switches itself off for the rest of the run. Both are recorded in `../../logs/compile.log`
— look there first when a strategy loaded but never traded.

## In a source checkout

`crates/vike-script/src/engine/bindable.rs`'s `RHAI_INDICATORS` is the authoritative set of bound indicator
names — `vike-cli indicators` prints it, so that command and this page never need updating when an
indicator is added — and `register_indicators` in the same file is the authority for the CALL SHAPE
(if it ever grows a richer one, the paragraph above is the stale half). `register_reads` and
`register_verbs`, also there, are the authoritative read and order surfaces; and
`crates/vike-script/src/lib.rs`'s own module doc is the authority for the call-unconditionally rule
above, down to why `engine::indicator_value`'s fed-once-per-bar cache makes a conditional call
silently wrong. If this README and those files ever disagree, they are right.
"##;

/// `strategies/rust/README.md`.
///
/// ⚠ **The FIRST LINE is the whole point of this file.** A user who drops a `.rs` file into a
/// release install gets no error, no log line and no strategy — nothing scans this directory at
/// runtime, because a `.rs` file is an input to `cargo`, not to the program. "I dropped a file in
/// and nothing happened" is unanswerable unless the answer is the first thing they read.
pub const RUST_README: &str = r##"# Rust strategies

**These compile only in a source checkout.** A `.rs` file here is built into the binary by `cargo`;
if you installed a release binary, nothing in this folder will ever run — nothing reads it at
startup, and no error is printed, because there is no point at which anything looks. If you are not
building from source, use `../rhai/` instead: those are read at runtime on every install.

Still here? Then you have the toolchain, and this is the folder for strategies that need real Rust:
your own data structures, crates from the registry, or performance the interpreter cannot reach.

## Layout: one folder per strategy, entry file named after the folder

```text
my_experiment/
  my_experiment.rs     the strategy — the name matches the folder
```

The same rule as `../rhai/`, for the same reason. A `.toml` beside the entry file is a preset for
that strategy; a `strategy.toml` with `live = true` opts it into LIVE mounting (absent = sim-only,
the same default-deny posture as the built-in `LIVE_CAPABLE` table).

## What a strategy is

One type implementing `vike_model::Strategy<B>` over a generic broker:

```rust
impl<B: Broker> Strategy<B> for MyExperiment { ... }
```

Generic over `B`, never written against the simulator — that single detail is what lets the same
type run in a backtest and against a live venue without being ported. Every hook has a default
no-op, so implement only what you use: `on_bar` alone is a complete strategy.

Start by copying `my_experiment/`.

## Wiring it up: there is none

The build scans this folder (`crates/vike-user-strategies`'s build script) and every
`<name>/<name>.rs` whose file exports

```rust
pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send>
```

is resolvable BY NAME from any profile after the next `cargo build` — backtest and (with
`live = true` in `strategy.toml`) the live daemon alike. No registry edit, no source-tree change:
the folder IS the registration. Built-in names always win, so a folder cannot shadow one; your
code never enters the repo (this tree is gitignored) and compiles against the platform's public
API only. ⚠ Params reach `build` as a lenient TOML table: a typo'd key silently reads as your
default — the in-tree param gates cover only the built-in strategies.

## ⚠ Read every param as EITHER an integer or a float

A knob you read with `as_integer()` alone will silently fall back to your default whenever it is
swept. A grid point's overrides are `f64` values, so every swept knob arrives as a TOML **float** —
`fast = 5` written by hand is an integer, and `fast` swept over `5, 10, 20` is `5.0`. The two are
different TOML types and `as_integer()` answers `None` for the second.

That matters most in the Studio's `Plugin (Rust)` mode, where a sweep override is the ONLY way to
set a knob at all today (there is no param editor for that tier yet), so an integer-only reader
means a strategy that ignores every value you sweep and reports one flat line of identical results.

```rust
let window = params
    .get("window")
    .and_then(|v| v.as_integer().or_else(|| v.as_float().map(|f| f as i64)))
    .map(|i| i.max(1) as usize)
    .unwrap_or(20);
```

The shipped `my_experiment/` reads its own knob the same way (float first, because a quantity is a
float), and so does every built-in `from_params`.

Read `my_experiment/my_experiment.rs` for the shape, or copy the folder and rename it — folder,
file and the name you put in a profile must all match.
"##;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// profiles/
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `profiles/README.md`.
pub const PROFILES_README: &str = r##"# Run profiles

A profile answers three questions: **which data**, **which costs**, **which strategy**. It is the
whole input to a run, so a result is reproducible by keeping the file that produced it.

```text
backtest.toml       one run
sweep.toml          the same strategy over a grid of parameters, ranked
walkforward.toml    the same strategy over successive out-of-sample windows
```

## Run them

```sh
vike-cli backtest run --profile backtest.toml --script ../strategies/rhai/sma_cross/sma_cross.rhai
vike-cli backtest run --profile sweep.toml    --rank-by sharpe
vike-cli backtest run --profile walkforward.toml
```

Add `--addr HOST:PORT` to reach a COMPUTE daemon (`vike-backend backtest --addr`) that is not on
`127.0.0.1:7880`, and `--json` for machine-readable output.

## ⚠ Two things to change before your first run

1. **The data slice must exist in your store.** These profiles name `binance` `BTCUSDT` 1-hour bars
   over a sample range. Point `[data]` at something you actually have; a slice with no data is a
   run with no trades, which reads like a bad strategy rather than an empty query.
2. **The costs are a guess about your venue, not a fact.** `fee_rate` here is a plain 0.1% taker.
   Too low and a dead strategy looks alive.

## ⚠ Why two of these inline the script and one does not

Every profile here runs through `vike-cli backtest run`, which takes `--script`. These two inline
the script as `[strategy.params].src` because they were written when `sweep` and `walkforward` were
SEPARATE verbs that had no such flag; both folded into `backtest run`, so the inlining is now
history rather than a limit. The inlined copy is written by `vike-cli init`
from the same source as `../strategies/rhai/sma_cross/sma_cross.rhai`, so the two match on a fresh
scaffold — but if you EDIT one, edit the other. Nothing keeps them in step after that.

## Fields worth knowing

- `[data] kind` — `"bar"` or `"tick"`. `interval` applies to bars.
- `[data] from` / `to` — `YYYY-MM-DDTHH` UTC, or a bare epoch-ms integer as a string.
- `[engine] cash` — starting equity, and the denominator of every percentage in the report.
- `[strategy] name` — `"rhai"` for a script; a registry name for a compiled strategy.
- `[strategy.params]` — knobs. ⚠ NOT validated: a misspelled key is ignored in silence and the
  strategy's own default applies. Check a surprising result here first.
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
vike-cli backtest run --profile ../profiles/backtest.toml \
    --script ../strategies/rhai/sma_cross/sma_cross.rhai \
    --json > sma_cross_2025h1.json
```

## The sample

`sample_sma_cross.json` ships with the scaffold so `../notebooks/` opens and runs on a fresh
install, before you have a store or a first result. It is **illustrative data from a demonstration
run** — not a claim about this strategy, this market, or what you should expect. Replace it with
your own output as soon as you have one.

## The shape

One JSON object per run:

| field | meaning |
|---|---|
| `name` | the profile's `name` |
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

`backtest_report.ipynb` reads `../backtest_results/sample_sma_cross.json` and prints a summary. It
runs on a fresh install — that is why a sample result ships — so you can open it before you have
run anything, see the shape, and point it at your own file by changing one line.

```sh
jupyter lab backtest_report.ipynb
```

The first cells use only the Python standard library, so they run in any interpreter with no
install. The plotting cell needs `matplotlib` and says so; skip it if you do not have it.

⚠ The sample is illustrative data, not a recorded run. Any conclusion drawn from it is a conclusion
about numbers someone typed.

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
