//! The three starter Rhai strategy FILES `vike-cli init` writes into `<project>/strategies/`, each
//! with its header (`vike_strategy::file`'s format: every knob it reads, declared), and their presets.
//! The Rust starter is the release's shipped `other/example.rs`, which the same `init` seeds.

// ── the three shipped Rhai strategies ────────────────────────────────────────────────────────
//
// Three SHAPES, not three indicators: always-in reversal, mean-reversion entered from flat, and a
// long-only filter whose flat periods are real. They are deliberately NOT a tour of the indicator
// catalog — the host binds nearly all of it and `vike-cli indicators` prints the roster, so three
// examples could never be a sample of it. What an example can teach is how a number becomes a
// POSITION, and each of these does it differently.
//
// Each reads its indicators on the first lines of `fn on_bar(strategy)` and places its orders
// through the `strategy` object — see the ⚠ rule in `RHAI_README`, and
// `crates/vike-cli/tests/cli_verbs/init_cli.rs`'s
// `every_shipped_rhai_strategy_reads_its_indicators_before_it_branches`, which gates it.

/// `trend/sma_cross.rhai` — two moving averages, always in the market, long or short.
pub const SMA_CROSS_RHAI: &str = r#"// sma_cross — the classic two-moving-average crossover.
//
// Long while the fast average is above the slow one, short while it is below. Always in the
// market: it reverses rather than flattening.
//
// The header: every knob this file reads, with its type. A key it does not declare is refused.
// ```toml
// [strategy]
//
// [params.fast]
// type = "number"
// default = 10.0
// [params.slow]
// type = "number"
// default = 30.0
// [params.qty]
// type = "number"
// default = 1.0
// ```
//
// Knobs (override from a profile's [strategy.params], or grid them in a [paramscan]):
let fast = param("fast", 10.0);
let slow = param("slow", 30.0);
let qty  = param("qty", 1.0);

fn on_bar(strategy) {
    // ⚠ BOTH indicators are read here, on EVERY bar, before any branch or return. An indicator
    // only advances when the script calls it, so a call inside an `if` would skip bars and
    // quietly stop being an average of the last N. This ordering is the rule, not a style.
    let f = sma(fast.to_int());
    let s = sma(slow.to_int());

    // Warm-up: the slow average is NaN until it has `slow` bars. Nothing to decide yet.
    if s.is_nan() { return; }

    // One entry a side. An entry the other way also closes the position it meets (it adds that
    // size to its own), and an entry whose side is already held sends no order at all.
    if f > s {
        strategy.entry("Long", OrderSide::Long).size(qty);
    } else {
        strategy.entry("Short", OrderSide::Short).size(qty);
    }
}
"#;

/// `mean_reversion/rsi_meanrev.rhai` — fade the extremes.
pub const RSI_MEANREV_RHAI: &str = r#"// rsi_meanrev — buy oversold, sell overbought.
//
// The opposite instinct to sma_cross: this one bets that a stretched market snaps back. Running
// both over the same slice is a quick way to see what a market has been doing lately.
//
// ```toml
// [strategy]
//
// [params.len]
// type = "number"
// default = 14.0
// [params.lo]
// type = "number"
// default = 30.0
// [params.hi]
// type = "number"
// default = 70.0
// [params.qty]
// type = "number"
// default = 1.0
// ```
//
// Knobs:
let len = param("len", 14.0);
let lo  = param("lo", 30.0);   // below this: oversold, go long
let hi  = param("hi", 70.0);   // above this: overbought, go short
let qty = param("qty", 1.0);

fn on_bar(strategy) {
    // ⚠ Read first, every bar, before any branch — see sma_cross.rhai for why this matters.
    let r = rsi(len.to_int());
    if r.is_nan() { return; }

    // The position guards keep this from stacking size: buy only from flat or short, sell only
    // from flat or long. `strategy.order` is one plain order of `qty`: from the other side it
    // closes that position, from flat it opens one.
    if r < lo && strategy.position() <= 0.0 { strategy.order("buy", OrderSide::Long).size(qty); }
    if r > hi && strategy.position() >= 0.0 { strategy.order("sell", OrderSide::Short).size(qty); }
}
"#;

/// `trend/ema_trend.rhai` — long-only trend filter.
pub const EMA_TREND_RHAI: &str = r#"// ema_trend — hold while price is above its moving average, otherwise hold nothing.
//
// LONG-ONLY, which makes it the useful contrast to sma_cross: its flat periods are real. If a
// long-only rule beats an always-in one, the short side was costing you money.
//
// ```toml
// [strategy]
//
// [params.len]
// type = "number"
// default = 20.0
// [params.qty]
// type = "number"
// default = 1.0
// ```
//
// Knobs:
let len = param("len", 20.0);
let qty = param("qty", 1.0);

fn on_bar(strategy) {
    // ⚠ Read first, every bar, before any branch — see sma_cross.rhai for why this matters.
    let e = ema(len.to_int());
    if e.is_nan() { return; }

    // Hold `qty` or nothing — never short. The entry sends nothing while it is held, and the
    // close sends nothing while flat.
    if close() > e {
        strategy.entry("Long", OrderSide::Long).size(qty);
    } else {
        strategy.close("Long");
    }
}
"#;

/// `presets/sma_cross/fast.toml` (under user_data) — a preset. Presets are param tables, one file each.
pub const PRESET_SMA_FAST: &str = r#"# Preset: fast — a short-horizon sma_cross, for intraday bars (1m–15m).
#
# Load it with: vike-cli backtest run --local --strategy trend/sma_cross.rhai --preset presets/sma_cross/fast.toml \
#   --venue <VENUE> --symbol <SYMBOL> --interval <INTERVAL> --from <FROM> --to <TO> --cash <CASH>
# The keys are FLAT — they are merged into [strategy.params] key for key, so a wrapping
# [params] header would make the strategy receive a table called "params" and nothing else.
fast = 5.0
slow = 15.0
qty = 1.0
"#;

/// `presets/sma_cross/slow.toml` — the second preset, so the FOLDER shows that presets are plural.
pub const PRESET_SMA_SLOW: &str = r#"# Preset: slow — a position-trading sma_cross, for daily bars.
#
# Load it with: vike-cli backtest run --local --strategy trend/sma_cross.rhai --preset presets/sma_cross/slow.toml \
#   --venue <VENUE> --symbol <SYMBOL> --interval <INTERVAL> --from <FROM> --to <TO> --cash <CASH>
# Keys are FLAT — see fast.toml.
fast = 20.0
slow = 100.0
qty = 1.0
"#;

/// `presets/rsi_meanrev/default.toml`.
pub const PRESET_RSI_DEFAULT: &str = r#"# Preset: default — the textbook 14/30/70 RSI settings.
#
# Load it with: vike-cli backtest run --local --strategy mean_reversion/rsi_meanrev.rhai --preset presets/rsi_meanrev/default.toml \
#   --venue <VENUE> --symbol <SYMBOL> --interval <INTERVAL> --from <FROM> --to <TO> --cash <CASH>
# Keys are FLAT — see sma_cross/fast.toml.
len = 14.0
lo = 30.0
hi = 70.0
qty = 1.0
"#;

/// `presets/ema_trend/default.toml`.
pub const PRESET_EMA_DEFAULT: &str = r#"# Preset: default — a 20-period trend filter.
#
# Load it with: vike-cli backtest run --local --strategy trend/ema_trend.rhai --preset presets/ema_trend/default.toml \
#   --venue <VENUE> --symbol <SYMBOL> --interval <INTERVAL> --from <FROM> --to <TO> --cash <CASH>
# Keys are FLAT — see sma_cross/fast.toml.
len = 20.0
qty = 1.0
"#;
