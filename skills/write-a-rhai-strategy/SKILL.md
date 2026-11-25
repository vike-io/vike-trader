---
name: write-a-rhai-strategy
description: Author a Rhai trading strategy for Vike offline — start from a starter template, check every indicator name against the host-bound roster, declare param() knobs, compile it (compile IS validation), read the knobs back, then backtest it. Use when asked to write, create, draft, edit, fix, validate, compile or lint a Rhai strategy or .rhai script, when a strategy "compiles but never trades", when you need the list of indicators a strategy can call (sma, rsi, bollinger_mid …), the strategy templates, or a strategy's tunable parameters, and before handing a script to run_backtest or run_sweep.
metadata:
  tools: "list_templates list_indicators discover_params validate_strategy run_backtest"
  source: "ai/trader/authoring ai/trader/tools/validate_strategy ai/trader/tools/discover_params ai/trader/tools/list_templates ai/trader/tools/list_indicators trader/tutorials/rhai-strategy-from-scratch"
---

# Write a Rhai strategy

Four of the five tools here run entirely inside the `vike-mcp` process — no `--addr`, no
`--node`, no key, no socket — so you can loop on them freely. Only `run_backtest` needs a reachable
datahub. None of the five is an order-write tool: none takes `confirm` or `preview_token`, and
nothing in this procedure can place, cancel or modify an order.

## What a script is

- Tunable knobs are `param(name, default)` calls at the **top level**; the trading logic lives in
  `fn on_bar(strategy)`, whose one parameter is the `strategy` object every order goes through.
  `fn on_tick(strategy)`, `fn on_stop(strategy)` (reads only) and `fn after_fill(strategy)` are
  optional beside it. The zero-argument hooks `on_start()` / `on_bar()` / `on_tick()` / `on_stop()`
  still run (the reads, the indicators, `print`) but place no order; a hook is defined one way.
- The top level runs **exactly once, at compile**, and never again — so a `param()` buried inside a
  hook is invisible to discovery.
- A script holds no cross-bar mutable state: a hook's `let` locals die with the call. State is
  host-side — the position, the `strategy` object's named orders and the indicator caches.
- Host surface: the global reads (`crates/vike-rhai/src/engine/host.rs`'s `HOST_FN_NAMES`) `close` /
  `open` / `high` / `low` / `volume` / `position` / `price` / `equity` / `index` / `now`, and `param`.
  Orders are the `strategy` object's (`crates/vike-rhai/src/tactics.rs`'s `STRATEGY_FN_NAMES`):
  `strategy.entry(name, OrderSide::Long)` (or `OrderSide::Short`; an entry the other way also closes the
  position it meets, an entry whose side is held sends nothing) and `strategy.order(name, side)` (one
  plain order), each with `.size(qty)` (1.0 unless set) `.stop(level)` `.limit(level)`;
  `strategy.close(name)`, `strategy.close_all()`; `strategy.exit(name).stop_pct(p).take_pct(p)`;
  `strategy.cancel(name)`; the reads `strategy.position()`, `strategy.avg_cost()`, `strategy.lots()`.
  Naming an order again replaces it, so two orders in one bar need two names.
- **When an order placed on a bar fills** is the file's own word, in its header's `[strategy]` table:
  `fill = "next_open"` (the default: at the first price after that bar, the next bar's open in a backtest)
  or `fill = "close"` (at that bar's close, a TradingView `process_orders_on_close` script). The backtest
  and a paper account read it from there; any other word is refused when the file is read.
- ⚠ `buy`, `sell`, `market` and `limit` were REMOVED: a script that calls one is refused at compile,
  by name, with the spelling that replaced it.
- `import` is refused, `print`/`debug` go to the log, and each hook call is bounded (operations,
  call depth, string and array sizes). A hook that errors emits zero orders that bar; after **10
  consecutive** errors the strategy self-disables and silently never trades.
- A script that places named orders through the `strategy` object (`fn on_bar(strategy)`) also reads
  beyond its own bars:
  - **Inputs.** Declare every series it reads in a top-level `fn inputs() { ["1d"] }` (`"1d"` alone is
    its own symbol's daily bars; `VENUE:SYMBOL:INTERVAL` names another series). A backtest loads only
    what is declared, and refuses to start when the store lacks one. `strategy.input("1d").close()`
    (and `.open()`, `.high()`, `.low()`, `.volume()`, `.time()`) is the input's last bar CLOSED by
    this bar's close: `()` before its first closes and for an undeclared name, never NaN, so test
    `if c == () { return; }` before any arithmetic.
  - **Other symbols.** Declare the symbols it trades beside its own in a top-level
    `fn trades() { ["ETHUSDT"] }`. `strategy.on("ETHUSDT")` makes one current until the next `on` or
    the hook's end (each hook starts on its own symbol, `""`), so `strategy.on("ETHUSDT").entry("L",
    OrderSide::Long)` trades it, under the tag `ETHUSDT/L#<n>`; the reads after it are that symbol's.
    `strategy.position_of(symbol)` and `strategy.price_of(symbol)` read any declared one. An
    undeclared symbol errors the hook, by name. Live, the mount declares what `fn trades()` names, so
    those orders trade there; one on any other symbol is refused (a `Denied`), never traded as its own.
  - **Closed trades.** `strategy.closed_count()`, `strategy.closed_trade(i)` and
    `strategy.closed_net()` read the run's closed-trade ledger as the engine booked it. They are `()`
    on a paper or live mount, which keeps no ledger, and that is not "nothing closed".
  - **Walk a long history one item at a time.** `input.count()` with `input.bar(i)`, and
    `strategy.closed_count()` with `strategy.closed_trade(i)`, are OLDEST FIRST (`0` the oldest, `-1`
    the newest, `()` outside them), one map an answer, so any length walks inside the sandbox. The
    array forms `input.bars(n)`, `input.since(ts)` and `strategy.closed_trades()` hold at most 682
    bars or 315 trades (the sandbox counts every field of every map in an array) and refuse more by
    name.
- On the `strategy` object an entry or an order can rest as a pegged order, `.chase()` (at its own
  side of the touch, the best bid for a buy and the best ask for a sell, following it), or as an
  iceberg, `.limit(level).iceberg(visible)` (only `visible` of its size shows). A chase with a level,
  or an iceberg without a limit or with a `visible` not above zero, is refused by name: a `Denied`
  order. Live, each meets the venue's capability row as any order of its kind does, and where the
  venue holds neither the engine works it with one plain limit at a time. A backtest runs that same
  law: a chase follows the book's touch, so a run on bars alone ends it unfilled, and an iceberg on a
  venue that shows the slice itself fills as its whole limit would.

## Procedure

1. **Start from a template.** Call `list_templates` (no arguments). It returns
   `templates: [{name, code}, …]` — SMA cross, RSI reversion, Donchian breakout, SMA cross with stop
   and target — each already parameterized via `param()`. Read them as *shapes*: knobs at the top
   level, an `is_nan()` warm-up early return, orders through `strategy` inside
   `fn on_bar(strategy)`. Do not trust the Donchian starter's name:
   it uses `sma` because a bare `donchian(len)` is not bound (its first output line is `upper`,
   not the namesake); the per-line accessors `donchian_upper()` / `donchian_mid()` /
   `donchian_lower()` are what work.

2. **Check every indicator name against the bound roster, never memory.** Call `list_indicators`
   with no arguments for the compact roster (`indicators: [{name, category}]`, `count`, `note`), and
   the user's own installed indicators in full under `user_indicators` (callable the same way).
   Then, for each indicator you intend to call, call it with `name: "<indicator>"` — the detail row
   carries `params` (`name`, `default`, `min`, `max`), `outputs`, `accessors` (`line` + `call`)
   and `bare_call`. Pass `category: "<family>"` (case-insensitive; the roster's `category` value)
   for a whole family instead. If both are passed, `name` wins.
   - `bare_call: false` with non-empty `accessors` is the band-indicator shape: `bollinger(20)`
     is refused, `bollinger_mid(20)` is the spelling. `name` must be the registry name — asking
     for `bollinger_mid` does not resolve; ask for `bollinger` and read its `accessors`.
   - A `name` or `category` nobody can serve is an `isError` result, not an empty list. For a
     real catalogue name the host holds back, the error quotes the host's reason (a Rhai reserved
     word such as `var` — use `stddev`; a host-function collision; `batch_only` — `ichimoku` /
     `zigzag` / `williams_fractal`; more parameters than the host binds, e.g. `kst`; a
     multi-output whose line 0 is not the namesake, which names the accessor that works).
   - The host binds one call form per argument count, from zero (all defaults) up to the
     parameter count, so omitted trailing arguments take the `default` printed in `params`.
   - The tool lists the **built-in** catalogue only. An indicator you wrote into
     `user_data/indicators` is callable by the same spelling but never appears here.

3. **Write the script.** Rules the engine will not forgive:
   - Declare every knob at the top level: `let len = param("len", 20.0);`. The default may be an
     integer or a float (`param("fast", 5)` and `param("fast", 5.0)` both work); convert at the
     call site when the indicator wants a count, e.g. `sma(len.to_int())`. First-seen wins if a
     name is declared twice.
   - **Call every indicator unconditionally, on the first lines of `on_bar`, before any branch or
     early return.** An indicator advances only when the script calls it; a call behind an `if`
     skips bars and returns a silently wrong value, never an error. Put the `is_nan()` warm-up
     check after all the reads, never instead of one.
   - An order's numbers are concretely typed: `.size` takes an `f64`, so `.size(1)` is a
     function-not-found on every bar while `.size(1.0)` works. Indicators and `param` take
     `Dynamic`, so `sma(5)` and `sma(5.0)` both resolve; only a non-numeric argument (`sma("20")`)
     fails, as a named runtime error.
   - Hold a position with `strategy.entry`: an always-in cross is one entry a side (the entry the
     other way reverses), a long-only filter is an entry and `strategy.close(name)`. Step one unit
     at a time with `strategy.order`, one name per side.

4. **Compile — compile IS validation.** Call `validate_strategy` with `script: "<source>"`.
   Returns `{ok: true}` or `{ok: false, error: "…"}`. `ok: false` is an **answer**, not a tool
   failure — read the compiler's message, fix the script, call again. Only a missing `script`
   argument is an `isError` result.
   ⚠ The check compiles the source and runs its **top level exactly once**
   (`crates/vike-rhai/src/strategy.rs`'s `discover_params`, the same one-time run the daemon's
   `RhaiStrategy::compile_with_params` performs). Rhai resolves a registered function when the
   line *runs*, so **nothing inside a hook is checked**: an unbound name, a typo, a wrong arity and a
   mistyped order argument all compile cleanly and then raise on every bar until the strategy
   disables itself. That is why step 2 is not optional. The one exception is a removed order
   function (`buy`, `sell`, `market`, `limit`): `ok: false` names it and the spelling that replaced
   it.

5. **Read the knobs back.** Call `discover_params` with `script: "<source>"`. Returns
   `params: [{name, default}, …]` in **declaration order**; defaults are always numbers. These are
   the keys a profile's `[strategy.params]` or a `[paramscan]` table may override (its old name
   `[sweep]` is refused at load) — every one you can name and be sure of. Unlike
   step 4, a compile error here *is* a tool error (`rhai compile error: …`) — call
   `validate_strategy` when you want a verdict, this when you want the knobs. If a knob you
   declared is missing, it is not at the top level: an override naming it still REACHES it (`param`
   reads the override map on every call, and a grid key is not filtered against the declared set),
   so what you lose is any way to know it is there.

6. **Run it.** A strategy runs as a FILE: the script goes into the backtest daemon's
   `strategies/` folder as a `.rhai` file (a person puts it there; nothing on this surface can put a
   file on that box), and the profile names it, `[strategy] file = "<path under strategies/>"`, with
   `[data]` naming the series the server holds. Then call `run_backtest` with
   `profile: "<backtest profile TOML>"`. There is no inline script: a call carrying `script` is
   refused before anything connects (`error [backtest.script_unsupported]`). The tool returns
   `{report: <TradingResults JSON>}`; it needs a reachable datahub, and a connect failure is a
   clean tool error.
   - Read `strategy_failures` first. Present, a hook raised and the result is NOT trustworthy: each
     of its `messages` is `<hook>: <Rhai's error>`, naming the unbound name, the wrong arity or the
     mistyped order argument, and `disabled: true` means the script switched itself off after ten
     errors in a row and placed no order after it (no earlier than `first_at`). Say so, fix the
     script and run it again.
   - Then read `n_trades`. Zero trades with `zero_trade.causes[].code` of `no-orders` and no
     `strategy_failures` means the hooks ran clean and nothing gated your orders because there were
     none: go back to steps 2–3 (a conditional indicator read, a comparison that can never be true,
     or a symbol that does not match the one the data is tagged with).
   - The indicator FILES are never shipped with the profile: the server uses its own
     `user_data/indicators` set, so a script calling a user indicator returns whatever that
     server has installed, and nothing in the response says so.
   - One backtest is the weakest evidence the platform produces: one symbol, one range, one
     parameter point, in-sample. Do not tune a knob until the number turns green — hand the
     `discover_params` names to run_sweep / run_walk_forward instead.

## Minimal shape to copy

```rust
let len = param("len", 20.0);
let qty = param("qty", 1.0);

fn on_bar(strategy) {
    let m = sma(len.to_int());          // read first, every bar, unconditionally
    if m.is_nan() { return; }           // warm-up, AFTER every read
    if close() > m {
        strategy.entry("Long", OrderSide::Long).size(qty);   // nothing while it is held
    } else {
        strategy.close("Long");                         // nothing while flat
    }
}
```

## Diagnosing "it compiles but never trades"

| Symptom | Likely cause | Check |
| --- | --- | --- |
| `ok: false`, `buy(qty) was removed: …` (or another removed name) | a script written for the removed order functions | rewrite it as the message says, through `strategy` |
| `ok: true`, zero trades, `strategy_failures` in the report | unbound indicator name / wrong arity / `.size(1)` | its `messages`; `list_indicators` with `name`; order argument types |
| value looks wrong, no error | indicator read behind an `if` or after an early `return` | move every read to the top of `on_bar` |
| knob missing from `discover_params` | `param()` called inside a hook | declare it at the top level |
| entry never fires on any tape | comparison against a level that already includes this bar (e.g. `close() > donchian_high(n)`) | compute the level from the bars *before* this one |

## References

- `https://vike.io/docs/ai/trader/authoring` — the four offline authoring tools.
- `https://vike.io/docs/ai/trader/tools/validate_strategy`, `https://vike.io/docs/ai/trader/tools/discover_params`,
  `https://vike.io/docs/ai/trader/tools/list_templates`, `https://vike.io/docs/ai/trader/tools/list_indicators`.
- `https://vike.io/docs/trader/tutorials/rhai-strategy-from-scratch` — the worked build, zero-trade run
  included.
- `https://vike.io/docs/trader/scripting` and `https://vike.io/docs/trader/scripting/cheatsheet` — the language surface.
