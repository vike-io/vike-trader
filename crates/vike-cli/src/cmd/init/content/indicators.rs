//! The user-indicator README and the two example indicators `vike-cli init` ships.

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
loaded under **✏ My indicators** in the chart's ƒx picker, tagged `·user`. Adding one works like
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
