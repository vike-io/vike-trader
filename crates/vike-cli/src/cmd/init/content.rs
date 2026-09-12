//! The FILES `vike-cli init` ships — every README, example strategy, profile, result and notebook,
//! as static text plus the two functions that compose the derived ones.
//!
//! # Why the content is a table and not a directory of files copied at build time
//!
//! `vike-cli` is a single binary that must scaffold a working tree on a machine with no source
//! checkout — that is the whole point of `strategies/rhai/` existing (see [`RHAI_README`]). There
//! is nowhere to copy FROM on such a box, so the examples are compiled in. A table also gives
//! `--reset` something to compare against: "restore the sample" is only answerable while the
//! pristine bytes are still known, which a copied-once file cannot be.
//!
//! # Every folder ships an example, and that is a decision rather than an oversight
//!
//! An empty folder reads as setup somebody abandoned half-way, and a feature that leaves no trace
//! on disk cannot be discovered by the person the feature is for. So each folder carries at least
//! one runnable artifact, deliberately in preference to creating them on demand the first time
//! something needs them.
//!
//! ⚠ **`logs/` is the one exception, and only for `logs/`.** A committed example log would be
//! FICTION — invented timestamps describing a run that never happened — and `compile.log` is
//! rewritten on the next start anyway, so a sample there would be both false and short-lived. The
//! folder is still created, and its README says what appears in it and when.
//!
//! # The examples must RUN, which constrains what they may contain
//!
//! `crates/vike-script/src/engine.rs`'s `RHAI_INDICATORS` is the host-bound indicator set — the
//! names `register_indicators` actually registers, DERIVED from `vike_indicators::registry()`
//! rather than hand-listed, so the binding and the advertisement cannot drift. A script calling a
//! name OUTSIDE it hits a function-not-found error EVERY bar and self-disables after the
//! consecutive-error cap — a strategy that looks mounted and silently never trades, which a
//! backtest reports as a flat curve, i.e. as "no signal".
//!
//! Compiling is not evidence of any of that (rhai resolves a name when the line RUNS), so
//! `crates/vike-cli/tests/init_cli.rs` mounts every shipped script through the real `RhaiStrategy`,
//! drives real bars, and asserts an order reaches the broker. No count of the bound set appears
//! here or in the shipped READMEs: it is derived, and `vike-cli indicators` prints it.

/// The single-line-per-folder MAP of `user_data/`.
///
/// ⚠ **ONE definition, two surfaces.** This is printed by `vike-cli init` AND embedded verbatim in
/// the `user_data/README.md` it writes — the command's whole reason for printing a tree is that the
/// map should exist before the user opens a file manager, and a map that disagreed with the README
/// beside it would be worse than no map. `map_is_embedded_verbatim_in_the_readme` pins that they
/// cannot drift.
pub const MAP: &str = "\
user_data/               your work — nothing here is ever overwritten once you have edited it
  strategies/rhai/       interpreted strategies: edit, save, backtest. No toolchain needed.
  strategies/rust/       compiled strategies. SOURCE CHECKOUT ONLY — a release binary cannot build them.
  indicators/            your own indicators, one <name>.rhai each. Callable from any strategy.
  profiles/              run profiles: which data, which costs, which strategy
  backtest_results/      saved reports. One sample ships, so the notebooks work before your first run.
  notebooks/             analysis over the results above
  logs/                  compile.log — every strategy load, pass and fail. Look here first.
";

/// `user_data/README.md` — the map, plus the ownership rule that explains why this directory is not
/// inside `settings/`.
///
/// Composed rather than stored so [`MAP`] has exactly one definition; see its doc.
pub fn readme() -> String {
    format!(
        r##"# user_data

Everything in this directory is **yours**. You author it, you back it up, you delete it. No part of
this workspace rewrites a file here — `vike-cli init` creates what is missing and stops.

```text
{MAP}```

## Why this is not inside `settings/`

`settings/` is the machine's half: four TOMLs the program READS and a `state/` directory it WRITES.
It also holds `secrets.env`, your live venue API keys.

`user_data/` is the half a program must never author. Keeping them apart is what makes this
directory safe to copy between machines, commit to your own repository, or share — none of which is
true of a directory that holds credentials.

Two more consequences worth knowing:

- **Lifecycle.** Delete `settings/state/` and the program rewrites it. Delete a strategy here and
  your work is gone.
- **Blast radius.** A directory you are invited to drop files into should not sit beside the file
  holding every key on the box.

## Where this directory is

`<project>/user_data`, resolved by walking up from the working directory for the project root — the
same walk that finds `settings/`, so the two can never answer with different projects.

`VIKE_USER_DATA_DIR` names it outright and skips the walk. It is a separate variable from
`VIKE_SETTINGS_DIR` on purpose: pointing the app at a strategy library on another disk is a
different question from relocating a deployment's settings, and one variable for both would force
them to move together.

## Getting started

```sh
vike-cli init                 # create anything missing (safe to re-run; never overwrites)
vike-cli init --reset         # restore the shipped samples you have edited or deleted
vike-cli init --dry-run       # print what WOULD change, touch nothing
```

Then read `strategies/rhai/README.md` — it is the shortest path from here to a backtest.

## What is deliberately NOT here

- **`data/`** — the history store is measured in hundreds of gigabytes and resolves on its own
  (`VIKE_HIST_STORE`). Filing it under a directory you are told to back up would be wrong in both
  directions.
- **A `.rs` indicator.** `indicators/` takes Rhai files only, and the Rust half is **out of scope**
  rather than deferred — "later" would leave you waiting for something that is not coming.
  `vike_indicators::registry` is a compile-time table whose every entry is held to a bit-parity law
  (streaming `on_bar` must equal batch `vectorize` bit-for-bit, gated over the whole registry), so
  adding one is a change to that crate, never a file you drop in here. `strategies/rust/` is honest
  about the same constraint in its own first line.

  What you CAN write is a Rhai indicator, which is a real indicator: see `indicators/README.md`, and
  `vike-cli indicators` for the built-in names already callable.
"##
    )
}

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
vike-cli backtest --profile ../../profiles/backtest.toml --script sma_cross/sma_cross.rhai
vike-cli backtest --list-params --script sma_cross/sma_cross.rhai    # its knobs, offline
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
the TOP LEVEL (outside any `fn`); that is where `--list-params` and a `[sweep]` grid look.

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

`crates/vike-script/src/engine.rs`'s `RHAI_INDICATORS` is the authoritative set of bound indicator
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

Read `my_experiment/my_experiment.rs` for the shape, or copy the folder and rename it — folder,
file and the name you put in a profile must all match.
"##;

// ── the three shipped Rhai strategies ────────────────────────────────────────────────────────
//
// Three SHAPES, not three indicators: always-in reversal, mean-reversion entered from flat, and a
// long-only filter whose flat periods are real. They are deliberately NOT a tour of the indicator
// catalog — the host binds nearly all of it and `vike-cli indicators` prints the roster, so three
// examples could never be a sample of it. What an example can teach is how a number becomes a
// POSITION, and each of these does it differently.
//
// Each reads its indicators on the first lines of `on_bar()` — see this module's doc and the ⚠ rule
// in `RHAI_README`, and `crates/vike-cli/tests/init_cli.rs`'s
// `every_shipped_rhai_strategy_reads_its_indicators_before_it_branches`, which gates it.

/// `sma_cross/sma_cross.rhai` — two moving averages, always in the market, long or short.
pub const SMA_CROSS_RHAI: &str = r#"// sma_cross — the classic two-moving-average crossover.
//
// Long while the fast average is above the slow one, short while it is below. Always in the
// market: it reverses rather than flattening.
//
// Knobs (override from a profile's [strategy.params], or grid them in a [sweep]):
let fast = param("fast", 10.0);
let slow = param("slow", 30.0);
let qty  = param("qty", 1.0);

fn on_bar() {
    // ⚠ BOTH indicators are read here, on EVERY bar, before any branch or return. An indicator
    // only advances when the script calls it, so a call inside an `if` would skip bars and
    // quietly stop being an average of the last N. This ordering is the rule, not a style.
    let f = sma(fast.to_int());
    let s = sma(slow.to_int());

    // Warm-up: the slow average is NaN until it has `slow` bars. Nothing to decide yet.
    if s.is_nan() { return; }

    // Target position, then trade the DIFFERENCE — so a bar that agrees with the position we
    // already hold sends no order at all.
    let target = if f > s { qty } else { -qty };
    let delta  = target - position();
    if delta >  1e-9 { market( 1,  delta); }
    if delta < -1e-9 { market(-1, -delta); }
}
"#;

/// `rsi_meanrev/rsi_meanrev.rhai` — fade the extremes.
pub const RSI_MEANREV_RHAI: &str = r#"// rsi_meanrev — buy oversold, sell overbought.
//
// The opposite instinct to sma_cross: this one bets that a stretched market snaps back. Running
// both over the same slice is a quick way to see what a market has been doing lately.
//
// Knobs:
let len = param("len", 14.0);
let lo  = param("lo", 30.0);   // below this: oversold, go long
let hi  = param("hi", 70.0);   // above this: overbought, go short
let qty = param("qty", 1.0);

fn on_bar() {
    // ⚠ Read first, every bar, before any branch — see sma_cross.rhai for why this matters.
    let r = rsi(len.to_int());
    if r.is_nan() { return; }

    // `position()` guards keep this from stacking size: enter only from flat or the other side.
    if r < lo && position() <= 0.0 { market(1, qty); }
    if r > hi && position() >= 0.0 { market(-1, qty); }
}
"#;

/// `ema_trend/ema_trend.rhai` — long-only trend filter.
pub const EMA_TREND_RHAI: &str = r#"// ema_trend — hold while price is above its moving average, otherwise hold nothing.
//
// LONG-ONLY, which makes it the useful contrast to sma_cross: its flat periods are real. If a
// long-only rule beats an always-in one, the short side was costing you money.
//
// Knobs:
let len = param("len", 20.0);
let qty = param("qty", 1.0);

fn on_bar() {
    // ⚠ Read first, every bar, before any branch — see sma_cross.rhai for why this matters.
    let e = ema(len.to_int());
    if e.is_nan() { return; }

    // Target is `qty` or nothing — never negative.
    let target = if close() > e { qty } else { 0.0 };
    let delta  = target - position();
    if delta >  1e-9 { market( 1,  delta); }
    if delta < -1e-9 { market(-1, -delta); }
}
"#;

/// `sma_cross/fast.toml` — a preset. Presets are param tables, one file each.
pub const PRESET_SMA_FAST: &str = r#"# Preset: fast — a short-horizon sma_cross, for intraday bars (1m–15m).
#
# Load it with: vike-cli backtest --preset sma_cross/fast
# The keys are FLAT — they are merged into [strategy.params] key for key, so a wrapping
# [params] header would make the strategy receive a table called "params" and nothing else.
fast = 5.0
slow = 15.0
qty = 1.0
"#;

/// `sma_cross/slow.toml` — the second preset, so the FOLDER shows that presets are plural.
pub const PRESET_SMA_SLOW: &str = r#"# Preset: slow — a position-trading sma_cross, for daily bars.
#
# Load it with: vike-cli backtest --preset sma_cross/slow
# Keys are FLAT — see fast.toml.
fast = 20.0
slow = 100.0
qty = 1.0
"#;

/// `rsi_meanrev/default.toml`.
pub const PRESET_RSI_DEFAULT: &str = r#"# Preset: default — the textbook 14/30/70 RSI settings.
#
# Load it with: vike-cli backtest --preset rsi_meanrev/default
# Keys are FLAT — see sma_cross/fast.toml.
len = 14.0
lo = 30.0
hi = 70.0
qty = 1.0
"#;

/// `ema_trend/default.toml`.
pub const PRESET_EMA_DEFAULT: &str = r#"# Preset: default — a 20-period trend filter.
#
# Load it with: vike-cli backtest --preset ema_trend/default
# Keys are FLAT — see sma_cross/fast.toml.
len = 20.0
qty = 1.0
"#;

/// `rust/my_experiment/my_experiment.rs` — the template to copy.
///
/// A COMPLETE, compiling strategy rather than a sketch: the point of a template is that the first
/// edit is a change to working code, not a hunt for what is missing.
pub const MY_EXPERIMENT_RS: &str = r##"//! `my_experiment` — the Rust strategy template. Copy this folder and rename it.
//!
//! ⚠ This compiles only in a SOURCE CHECKOUT — see ../README.md's first line. On a release install
//! nothing reads this directory.
//!
//! # Wiring it up: there is none
//!
//! The build scans this tree (`crates/vike-user-strategies`'s build script): a folder whose
//! `<name>.rs` exports the `build` function below is resolvable from any profile BY NAME after
//! the next `cargo build`. No registry edit — the folder is the registration. Built-in names
//! always win over yours, and live mounting additionally needs `live = true` in a `strategy.toml`
//! beside this file (absent = sim-only).
//!
//! # What you are implementing
//!
//! `vike_model::Strategy<B>`, generic over the broker. Generic is load-bearing: a strategy written
//! against `B: Broker` runs on the simulator AND against a live venue with no port. Write
//! `impl Strategy<SimBroker>` instead and you have built something that can only ever be
//! backtested.
//!
//! Every hook has a default no-op, so implement only the ones you use.

use vike_model::{Bar, Broker, Strategy};

/// Hold a fixed position while the close is above the entry price of the run's first bar.
///
/// Deliberately trivial — it exists to be replaced. What it demonstrates is the shape: state on
/// the struct, decisions in `on_bar`, orders through the broker.
pub struct MyExperiment {
    /// Units to hold when long. Read from the profile in a real strategy.
    qty: f64,
    /// The first close this strategy ever saw. `None` until the first bar arrives.
    reference: Option<f64>,
}

impl MyExperiment {
    pub fn new(qty: f64) -> Self {
        MyExperiment { qty, reference: None }
    }
}

impl Default for MyExperiment {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl<B: Broker> Strategy<B> for MyExperiment {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        // The symbol comes from the BAR, never from a hard-coded string: one strategy instance can
        // be mounted on any series, and the engine may run several at once.
        let symbol = bar.symbol.clone().unwrap_or_default();
        if symbol.is_empty() {
            return;
        }

        // First bar: record the reference and do nothing else.
        let reference = *self.reference.get_or_insert(bar.close);

        // Trade the DIFFERENCE between where we want to be and where we are, so a bar that agrees
        // with the current position sends no order.
        let target = if bar.close > reference { self.qty } else { 0.0 };
        let delta = target - broker.position(&symbol);
        if delta.abs() > 1e-9 {
            broker.submit_market(&symbol, if delta > 0.0 { 1 } else { -1 }, delta.abs());
        }
    }
}

/// The entry point the build scan resolves — THE contract: exactly this signature, exported from
/// the folder-named file. Params arrive as the profile's `[strategy.params]` table; read them
/// leniently (a missing or mistyped key falls back to your default — nothing in-tree gates a user
/// strategy's keys, so the defaults ARE the safety net).
pub fn build<B: vike_model::HftBroker + 'static>(
    params: &toml::Value,
) -> Box<dyn vike_model::Strategy<B> + Send> {
    let qty = params
        .get("qty")
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(1.0);
    Box::new(MyExperiment::new(qty))
}
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
vike-cli backtest    --profile backtest.toml --script ../strategies/rhai/sma_cross/sma_cross.rhai
vike-cli backtest    --profile sweep.toml       --rank-by sharpe
vike-cli walkforward --profile walkforward.toml
```

Add `--addr HOST:PORT` to reach a datahub server that is not on `127.0.0.1:7878`, and `--json` for
machine-readable output.

## ⚠ Two things to change before your first run

1. **The data slice must exist in your store.** These profiles name `binance` `BTCUSDT` 1-hour bars
   over a sample range. Point `[data]` at something you actually have; a slice with no data is a
   run with no trades, which reads like a bad strategy rather than an empty query.
2. **The costs are a guess about your venue, not a fact.** `fee_rate` here is a plain 0.1% taker.
   Too low and a dead strategy looks alive.

## ⚠ Why two of these inline the script and one does not

Only `backtest` takes `--script`. `sweep` and `walkforward` do not, so their profiles carry the
script inline as `[strategy.params].src` instead. The inlined copy is written by `vike-cli init`
from the same source as `../strategies/rhai/sma_cross/sma_cross.rhai`, so the two match on a fresh
scaffold — but if you EDIT one, edit the other. Nothing keeps them in step after that.

## Fields worth knowing

- `[data] kind` — `"bar"` or `"tick"`. `interval` applies to bars.
- `[data] from` / `to` — `YYYY-MM-DDTHH` UTC, or a bare epoch-ms integer as a string.
- `[engine] cash` — starting equity, and the denominator of every percentage in the report.
- `[strategy] name` — `"rhai"` for a script; a registry name for a compiled strategy.
- `[strategy.params]` — knobs. ⚠ NOT validated: a misspelled key is ignored in silence and the
  strategy's own default applies. Check a surprising result here first.
- `[sweep]` — each key names a `[strategy.params]` field and lists values to cross-product.
- `[walkforward] n_splits` — how many anchored out-of-sample windows to walk.
"##;

/// `profiles/backtest.toml` — one run, script supplied by `--script`.
pub const PROFILE_BACKTEST: &str = r#"# One backtest run.
#
#   vike-cli backtest --profile backtest.toml \
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
# market. Seed it with `vike-cli data seed-demo`, which writes exactly this slice.
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
#   vike-cli backtest --profile sweep.toml --rank-by sharpe
#
# There is no separate `sweep` verb: a profile carrying a [sweep] table IS a parameter search, and
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
[sweep]
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
#   vike-cli walkforward --profile walkforward.toml
#
# What it answers: "was this profitable in every part of the sample, or in one lucky stretch?"
# ⚠ What it does NOT do is re-fit per window. These parameters are fixed and identical in all
# windows, so this is an out-of-sample STABILITY check, not an optimize-then-test protocol.
#
# ⚠ The script is INLINE below — `walkforward` has no --script flag. See sweep.toml.

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

// ─────────────────────────────────────────────────────────────────────────────────────────────
// backtest_results/ and notebooks/
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `backtest_results/README.md`.
pub const RESULTS_README: &str = r##"# Backtest results

Reports you chose to keep. Nothing writes here on its own — `--json` prints to stdout, and saving
it is your decision:

```sh
vike-cli backtest --profile ../profiles/backtest.toml \
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

/// `backtest_results/sample_sma_cross.json` — the committed sample the notebook reads.
///
/// ⚠ **Plausible illustrative numbers, not a recorded run**, and the README beside it and the
/// notebook that reads it both say so in those words. The alternative — shipping nothing — means
/// `notebooks/` cannot run until the user has a store, a slice and a first result, which is the
/// failure the sample exists to prevent. Internally consistent on purpose (`total_return` agrees
/// with `final_equity` against the profile's `cash = 10000.0`), so nobody debugs arithmetic that
/// was never meant to be checked.
pub const SAMPLE_RESULT_JSON: &str = r#"{
  "name": "sma_cross — BTCUSDT 1h (illustrative sample, not a recorded run)",
  "final_equity": 10731.42,
  "total_return": 0.073142,
  "n_trades": 48,
  "win_rate": 0.4375,
  "sharpe": 0.8213,
  "max_drawdown": 0.0914,
  "profit_factor": 1.3106,
  "funding_paid": 0.0,
  "per_symbol_pnl": []
}
"#;

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

/// `notebooks/backtest_report.ipynb` — a minimal, valid nbformat-4 notebook.
///
/// Standard library only in the cells that matter, so it runs in any interpreter without an
/// install; the one cell needing `matplotlib` is last and announces itself. Kept small on purpose —
/// it is a starting point to edit, not a dashboard to maintain.
///
/// ⚠ The literal is delimited `r####"…"####` because the notebook is JSON whose STRINGS are
/// markdown: a cell beginning `"## Compare several runs` contains `"##`, which closes an `r##"`
/// literal in the middle of the file. Anything shorter than four hashes is a compile error waiting
/// for the next heading somebody adds.
pub const NOTEBOOK_IPYNB: &str = r####"{
 "cells": [
  {
   "cell_type": "markdown",
   "metadata": {},
   "source": [
    "# Backtest report\n",
    "\n",
    "Reads one saved result from `../backtest_results/` and prints a summary.\n",
    "\n",
    "It opens the shipped **sample** so this notebook runs on a fresh install. Change `RESULT` below\n",
    "to your own file as soon as you have one.\n",
    "\n",
    "⚠ The sample is illustrative data, not a recorded run."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "metadata": {},
   "outputs": [],
   "source": [
    "import json\n",
    "from pathlib import Path\n",
    "\n",
    "RESULT = Path('../backtest_results/sample_sma_cross.json')\n",
    "\n",
    "report = json.loads(RESULT.read_text(encoding='utf-8'))\n",
    "report"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "metadata": {},
   "outputs": [],
   "source": [
    "# A readable summary. Percentages are stored as fractions, so 0.0731 -> 7.31%.\n",
    "def pct(x):\n",
    "    return 'n/a' if x is None else f'{x * 100:.2f}%'\n",
    "\n",
    "def num(x, places=4):\n",
    "    return 'n/a' if x is None else f'{x:.{places}f}'\n",
    "\n",
    "rows = [\n",
    "    ('name',          report.get('name')),\n",
    "    ('final equity',  num(report.get('final_equity'), 2)),\n",
    "    ('total return',  pct(report.get('total_return'))),\n",
    "    ('trades',        report.get('n_trades')),\n",
    "    ('win rate',      pct(report.get('win_rate'))),\n",
    "    ('sharpe',        num(report.get('sharpe'))),\n",
    "    ('max drawdown',  pct(report.get('max_drawdown'))),\n",
    "    ('profit factor', num(report.get('profit_factor'))),\n",
    "]\n",
    "width = max(len(label) for label, _ in rows)\n",
    "for label, value in rows:\n",
    "    print(f'{label:<{width}}  {value}')\n",
    "\n",
    "# profit_factor is null when there is no meaningful ratio (no losing trades, or no trades).\n",
    "# sharpe is annualized with 252 periods unless the run was on daily bars: compare runs to each\n",
    "# other, not to a published number."
   ]
  },
  {
   "cell_type": "markdown",
   "metadata": {},
   "source": [
    "## Compare several runs\n",
    "\n",
    "Once you have saved more than one result, this reads the whole folder at once."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "metadata": {},
   "outputs": [],
   "source": [
    "results = []\n",
    "for path in sorted(Path('../backtest_results').glob('*.json')):\n",
    "    data = json.loads(path.read_text(encoding='utf-8'))\n",
    "    results.append((path.name, data.get('total_return'), data.get('sharpe'), data.get('n_trades')))\n",
    "\n",
    "print(f\"{'file':<34}{'return':>10}{'sharpe':>10}{'trades':>9}\")\n",
    "for name, ret, sharpe, trades in results:\n",
    "    r = 'n/a' if ret is None else f'{ret * 100:.2f}%'\n",
    "    s = 'n/a' if sharpe is None else f'{sharpe:.3f}'\n",
    "    print(f'{name:<34}{r:>10}{s:>10}{trades:>9}')"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "metadata": {},
   "outputs": [],
   "source": [
    "# Optional: needs matplotlib (`pip install matplotlib`). Skip this cell if you do not have it.\n",
    "import matplotlib.pyplot as plt\n",
    "\n",
    "names = [r[0] for r in results]\n",
    "returns = [(r[1] or 0.0) * 100 for r in results]\n",
    "\n",
    "fig, ax = plt.subplots(figsize=(8, 0.5 * len(names) + 1.5))\n",
    "ax.barh(names, returns)\n",
    "ax.set_xlabel('total return (%)')\n",
    "ax.axvline(0, linewidth=0.8, color='black')\n",
    "plt.tight_layout()"
   ]
  }
 ],
 "metadata": {
  "kernelspec": {"display_name": "Python 3", "language": "python", "name": "python3"},
  "language_info": {"name": "python", "version": "3"}
 },
 "nbformat": 4,
 "nbformat_minor": 5
}
"####;

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

// ─────────────────────────────────────────────────────────────────────────────────────────────
// indicators/
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `indicators/README.md` — the contract (one required function, the rest optional — `outputs()`
/// joined it), the ONE rule that silently ruins an
/// indicator, and what this seam deliberately does not do.
pub const INDICATORS_README: &str = r##"# Your own indicators

One file per indicator, flat: `indicators/<name>.rhai` is callable from any strategy as `<name>()`.

No folder each — an indicator has no presets to keep beside it, so a folder would be an empty
wrapper around one file.

## The contract: one required function, the rest optional

```rhai
fn init()  { #{ n: 0 } }     // OPTIONAL. The state you keep. Default: an empty map #{}.
fn on_bar(bar) { ... }       // REQUIRED. Called exactly once per bar, in order.
fn warmup() { 20 }           // OPTIONAL. Bars before your value means anything. Default 0.
fn outputs() { ["hi","lo"] } // OPTIONAL. Your output lines. Default: one, named after the file.
```

Inside `on_bar`, **`this` is your state**, and what you write to it is still there next bar. That is
the whole point of this folder: a plain function in a strategy has no memory, so "the average of the
last 20 closes" cannot be written as one. Here it can.

`bar` is a map: `ts`, `open`, `high`, `low`, `close`, `volume`.

Return a number. Return `()` while you are still warming up — that reads as `NaN`, exactly like a
built-in that is not warm yet, and every shipped strategy template already guards for it.

Anything above a `fn` runs **once**, at load, and stays visible inside `on_bar` — so a top-level
`let` is your knob:

```rhai
let lookback = 20;                       // <- change this, save, re-run
fn init() { #{ buf: [] } }
fn warmup() { lookback - 1 }
fn on_bar(bar) {
    this.buf.push(bar.close);
    if this.buf.len() > lookback { this.buf.remove(0); }
    if this.buf.len() < lookback { return (); }
    let s = 0.0;
    for v in this.buf { s += v; }
    s / lookback
}
```

## Parameters: `param()`, exactly as in a strategy

A top-level `let` is a knob you edit. If you want the SAME indicator at two settings in one strategy,
declare it with `param(name, default)` — the same function a strategy uses for its sweepable knobs,
so there is nothing new to learn:

```rhai
// indicators/my_mean.rhai
let lookback = param("lookback", 20);
fn init() { #{ buf: [] } }
fn warmup() { lookback - 1 }
fn on_bar(bar) { ... }
```

```rhai
// ...and in a strategy, two settings side by side:
fn on_bar() {
    let fast = my_mean(10);
    let slow = my_mean(50);
    if fast > slow { buy(1.0) }
}
```

Arguments are **positional**, in the order your `param()` calls run — the first argument sets the
first knob. Leave one off and it takes its default, so `my_mean()` is still the 20-bar mean.

Each distinct setting is its own instance with its own state, so `my_mean(10)` and `my_mean(50)`
never read each other's window. And passing more arguments than you declared is an **error naming
the knobs you do have**, not a value quietly dropped.

## Several lines out: `fn outputs()`

A band has three numbers, not one. Say so, and each line gets its own call:

```rhai
// indicators/my_bands.rhai
let width = param("width", 2.0);

fn outputs() { ["upper", "mid", "lower"] }

fn init() { #{ buf: [] } }

fn on_bar(bar) {
    // ... whatever you compute ...
    [bar.close + width, bar.close, bar.close - width]   // one value per declared line, in order
}
```

```rhai
// ...and in a strategy:
fn on_bar() {
    let mid = my_bands_mid();
    let up  = my_bands_upper(3.0);       // the knobs work on a line accessor too
    if close() > up { sell(1.0) }
}
```

The call is `<file>_<line>()`, which is exactly how the built-in bands are spelled
(`bollinger_mid`, `macd_signal`, `stochastic_k` — `vike-cli indicators --json` prints them).
`vike-cli indicators` prints YOURS too, one row per callable spelling with the line it returns
named — so the listing is the answer to "what may I type", including when the plain name is not one
of the answers.

Worth knowing:

- **`on_bar` must return exactly as many values as you declared.** Too few or too many is an error
  naming both counts — never a padded NaN you would read as a warm-up, never a dropped line.
- **`()` is still warm-up, and covers every line at once.** A single line can warm up on its own,
  too: `[upper, (), lower]`.
- **All the lines are one indicator.** Reading three of them in a bar runs your `on_bar` ONCE and
  reads three entries of what it returned — not three copies of your recurrence.
- **The plain name is only bound when line 0 is named after the file.** `my_bands()` would hand back
  the *upper* band to somebody who read it as the middle, so it is refused with the accessors offered
  instead. Name your first line after the file — `fn outputs() { ["macdish", "signal"] }` in
  `macdish.rhai` — and `macdish()` is that first line, exactly like the built-in `macd()`.

Leave `outputs()` out and nothing changes: one line, one number, called by the file's own name.

## ⚠ Call it on EVERY bar

This is the one rule that ruins an indicator silently. It is fed when your strategy calls it, so a
call inside an `if` skips the bars the branch was false on, and your "average of the last 20" quietly
becomes the average of the last 20 bars *it happened to see*. No error, just a wrong number.

```rhai
fn on_bar() {
    let m = my_mean();                   // GOOD: every bar, unconditionally
    if position() == 0.0 && close() > m { buy(1.0) }
}
```

```rhai
fn on_bar() {
    if position() == 0.0 && close() > my_mean() { buy(1.0) }   // BAD: skips bars
}
```

Read it into a variable at the top of `on_bar` and use the variable. The same rule applies to the
built-in indicators.

## Plotting it on a chart

Your indicators are also chart studies: the app compiles this folder at startup and lists what
loaded under **✎ My indicators** in the chart's ƒx picker, tagged `·user`. Adding one works like
adding a built-in — the settings ⚙ gives you a `DragValue` per `param()` knob, and a saved workspace
remembers it by file name.

By default a user indicator draws in its **own pane**, because nothing here knows what scale your
number is on and a z-score plotted against the price axis would flatten the candles. If yours IS a
price — a level, a band edge, a moving average — say so, and it draws over them instead:

```rhai
fn overlay() { true }
```

It is read once, at load. A strategy calling the same indicator is unaffected either way.

⚠ A file that failed to compile is simply an ABSENT row in the picker — there is nothing to click
and nothing to hover. The reason is in the app's log, one line per rejected file, which is the only
place it exists.

## Names

Your file may not take the name of a built-in indicator (`sma`, `rsi`, …), of one of their line
accessors (`bollinger_mid`, `stochastic_k`, …) or of a host function (`close`, `buy`, `position`, …).
The same goes for a line accessor your own `outputs()` would generate. The load log says so by name
when it happens, and says which line to rename.

The reason: `sma(20)` should mean the same thing in every strategy on every machine, and a local file
quietly redefining it is a bug you would spend an afternoon on. `vike-cli indicators` prints the
built-in names, which is also the list to avoid.

## What this does not do

- **No render style.** You name your lines; you do not say whether one is a band or a histogram. A
  built-in declares that for the chart, and a user indicator is called from a strategy, so it would
  be a setting nothing reads.
- **No trading, and no reading the account.** `buy`, `sell`, `position`, `equity` and the built-in
  indicators are all absent inside an indicator file. An indicator is a function of the bars it was
  given and nothing else, which is also what lets a backtest replay it and get exactly what the live
  path computed.
"##;

/// `indicators/donchian_high.rhai` — the first shipped example, and the one that justifies the
/// folder: a rolling window is EXACTLY what a strategy script cannot express on its own.
///
/// Deliberately not a wrapper around a built-in (that would demonstrate nothing) and deliberately
/// stateful (that is the seam). Uses all three functions, including `warmup`.
pub const DONCHIAN_HIGH_RHAI: &str = r#"// The highest high of the last `lookback` bars.
//
// This is the shape a strategy script cannot write on its own: it has to remember earlier bars.
//
// `param()` makes `lookback` settable from the call site — `donchian_high(50)` — while keeping 20 as
// the default, so `donchian_high()` still works. Anything above a `fn` runs once per setting.

let lookback = param("lookback", 20);

// This one IS a price, so on a chart it belongs over the candles rather than in a pane of its own
// (the default). Delete this to see the difference — nothing else about the indicator changes, and
// a strategy calling it cannot tell either way.
fn overlay() {
    true
}

fn init() {
    #{ highs: [] }
}

// Until the window is full there is no "highest of 20", so say so with () -> NaN.
fn warmup() {
    lookback - 1
}

fn on_bar(bar) {
    this.highs.push(bar.high);
    if this.highs.len() > lookback {
        this.highs.remove(0);
    }
    if this.highs.len() < lookback {
        return ();
    }
    let hi = this.highs[0];
    for h in this.highs {
        if h > hi { hi = h; }
    }
    hi
}
"#;

/// `indicators/streak.rhai` — the second example, chosen for a DIFFERENT shape of state: a pure
/// recurrence, two numbers, no buffer.
///
/// Two examples earn their place by being unlike each other. This one shows why a user indicator
/// costs the same per bar however long the series is.
pub const STREAK_RHAI: &str = r#"// Consecutive up-closes as a positive count, consecutive down-closes as a negative one.
//
// Contrast donchian_high.rhai: no window, no buffer — two numbers of state and the same work per
// bar however long the series.

fn init() {
    #{ prev: (), run: 0 }
}

// One bar, not zero: "up or down versus the previous close" cannot mean anything on the first bar,
// because there is no previous close yet. warmup() is the index of the first bar your value is real
// on, so leaving it at the default 0 here would be a claim that is one bar wrong.
fn warmup() {
    1
}

fn on_bar(bar) {
    if this.prev == () {
        this.prev = bar.close;
        return ();
    }
    if bar.close > this.prev {
        this.run = if this.run > 0 { this.run + 1 } else { 1 };
    } else if bar.close < this.prev {
        this.run = if this.run < 0 { this.run - 1 } else { -1 };
    } else {
        this.run = 0;
    }
    this.prev = bar.close;
    this.run
}
"#;
