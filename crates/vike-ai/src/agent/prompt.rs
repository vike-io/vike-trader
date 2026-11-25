//! The model-facing contract: the Rhai system prompt and the two tool specs the loop advertises.

use serde_json::json;

use crate::client::ToolSpec;

/// The Rhai strategy API contract handed to the model as the system prompt.
pub const STRATEGY_SYSTEM_PROMPT: &str = r#"You write trading strategies in Rhai (a Rust-like scripting language) for the vike backtester.
Contract:
- Define `fn on_bar(strategy) { ... }`, called once per bar. Every order goes through `strategy`.
- Reads: strategy.position() -> current signed position; price() -> last close; close() open()
  high() low() volume() -> this bar;
  indicators -- sma(n), ema(n), rsi(n), atr(n), adx(n), cci(n) and most of the vike-indicators
  registry, each NaN until warmed up. Call one with no argument to take its default period
  (sma() == sma(20)). Multi-output indicators (macd, bollinger, stochastic) are NOT callable:
  a single number cannot say which line you meant. A name that is not bound raises on the first
  call rather than returning a value, so a guess fails loudly.
- Orders, on `strategy`; a side is OrderSide::Long or OrderSide::Short, a number a float (1.0, not 1):
  strategy.entry(name, side) enters (default size 1.0; an entry against an open position also
  closes it, and one already held sends nothing), chain .size(qty) .stop(px) .limit(px);
  strategy.order(name, side).size(qty) places exactly that order; strategy.close(name) and
  strategy.close_all() flatten; strategy.exit(name).stop_pct(p).take_pct(p) puts a stop and a
  target on entry `name`. An order under a name still resting amends it: two orders, two names.
  buy/sell/market/limit were removed and are refused at load; a zero-argument `fn on_bar()`
  still runs, but it places no order.
  There is no cross-bar mutable state in the script -- all state (position, indicator windows) is
  host-side; read it fresh each bar.
- Example: `fn on_bar(strategy) { let f = sma(5); let s = sma(20); if s.is_nan() { return; }
  if f > s { strategy.entry("Long", OrderSide::Long); } else { strategy.entry("Short", OrderSide::Short); } }`
- Parameters: `let x = param("x", default);` at the top level declares a sweepable knob.
- Guard indicators: `if s.is_nan() { return; }` before using sma/ema/rsi.
Call the `submit_strategy` tool with your complete script in `code` and a one-line `explanation`.
If your script fails to compile or does not trade, you'll get the error -- fix it and resubmit."#;

pub(super) fn submit_tool() -> ToolSpec {
    ToolSpec {
        name: "submit_strategy".into(),
        description: "Submit a complete Rhai strategy script (code) plus a one-line explanation."
            .into(),
        input_schema: json!({"type":"object","properties":{"code":{"type":"string"},"explanation":{"type":"string"}},"required":["code","explanation"]}),
    }
}

/// The `record_learning` tool — advertised ONLY when a ledger path is configured. A tool that
/// provably discards its input is worse than an absent one: it burns model turns and teaches the
/// model to trust a note that was never written. With no ledger there is nowhere to write, so the
/// tool is simply not offered.
pub(super) fn record_learning_tool() -> ToolSpec {
    ToolSpec {
        name: "record_learning".into(),
        description: "Record a durable note about this market/slice for FUTURE sessions to read \
                      (e.g. \"wide-spread regimes on this symbol cluster 00:00-04:00 UTC\"). \
                      `scope` is \"global\" for a note that holds anywhere, or \"slice\" (the \
                      default) for one that only applies to the instrument being worked on. This \
                      does NOT submit a strategy."
            .into(),
        input_schema: json!({"type":"object","properties":{"scope":{"type":"string","enum":["global","slice"]},"text":{"type":"string"}},"required":["text"]}),
    }
}
