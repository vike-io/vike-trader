---
name: write-a-rhai-strategy
description: Author a Rhai trading strategy for Vike offline — start from a starter template, check every indicator name against the host-bound roster, declare param() knobs, compile it (compile IS validation), read the knobs back, then backtest it. Use when asked to write, create, draft, edit, fix, validate, compile or lint a Rhai strategy or .rhai script, when a strategy "compiles but never trades", when you need the list of indicators a strategy can call (sma, rsi, bollinger_mid …), the strategy templates, or a strategy's tunable parameters, and before handing a script to run_backtest or run_sweep.
metadata:
  tools: "list_templates list_indicators discover_params validate_strategy run_backtest"
  source: "ai/trader/authoring ai/trader/tools/validate_strategy ai/trader/tools/discover_params ai/trader/tools/list_templates ai/trader/tools/list_indicators trader/tutorials/rhai-strategy-from-scratch"
---

# Write a Rhai strategy

Four of the five tools here run entirely inside the `vike-cli mcp` process — no `--addr`, no
`--node`, no key, no socket — so you can loop on them freely. Only `run_backtest` needs a reachable
datahub. None of the five is an order-write tool: none takes `confirm` or `preview_token`, and
nothing in this procedure can place, cancel or modify an order.

## What a script is

- Tunable knobs are `param(name, default)` calls at the **top level**; the trading logic lives in
  `fn on_bar()`. The hooks `on_start` / `on_bar` / `on_stop` are zero-argument `fn`s, all optional.
- The top level runs **exactly once, at compile**, and never again — so a bare top-level `buy(1.0)`
  never re-fires, and a `param()` buried inside a hook is invisible to discovery.
- A script holds no cross-bar mutable state: a hook's `let` locals die with the call. State is
  host-side — `position()` and the indicator caches.
- Host surface (`crates/vike-script/src/engine.rs`'s `HOST_FN_NAMES`): the reads `close` / `open`
  / `high` / `low` / `volume` / `position` / `price` / `equity` / `index` / `now`; the verbs `buy` /
  `sell` / `limit` / `market` (`buy(qty)` is `market(1, qty)`, `sell(qty)` is `market(-1, qty)`,
  `limit(side, qty, price)` rests one); and `param`.
- `import` is refused, `print`/`debug` go to the log, and each hook call is bounded (operations,
  call depth, string and array sizes). A hook that errors emits zero orders that bar; after **10
  consecutive** errors the strategy self-disables and silently never trades.

## Procedure

1. **Start from a template.** Call `list_templates` (no arguments). It returns
   `templates: [{name, code}, …]` — SMA cross, RSI reversion, Donchian breakout — each already
   parameterized via `param()`. Read them as *shapes*: knobs at the top level, an `is_nan()`
   warm-up early return, trading inside `fn on_bar()`. Do not trust the Donchian starter's name:
   it uses `sma` because a bare `donchian(len)` is not bound (its first output line is `upper`,
   not the namesake); the per-line accessors `donchian_upper()` / `donchian_mid()` /
   `donchian_lower()` are what work.

2. **Check every indicator name against the bound roster, never memory.** Call `list_indicators`
   with no arguments for the compact roster (`indicators: [{name, category}]`, `count`, `note`).
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
   - Order verbs are concretely typed: `market` is `(i64, f64)`, so `market(1, 1)` is a
     function-not-found on every bar while `market(1, 1.0)` works. Indicators and `param` take
     `Dynamic`, so `sma(5)` and `sma(5.0)` both resolve; only a non-numeric argument (`sma("20")`)
     fails, as a named runtime error.
   - Size from `position()`: compute a target, take `delta = target - position()`, and send
     `market(1, delta)` / `market(-1, -delta)` only when `|delta|` exceeds a small epsilon.

4. **Compile — compile IS validation.** Call `validate_strategy` with `script: "<source>"`.
   Returns `{ok: true}` or `{ok: false, error: "…"}`. `ok: false` is an **answer**, not a tool
   failure — read the compiler's message, fix the script, call again. Only a missing `script`
   argument is an `isError` result.
   ⚠ The check compiles the source and runs its **top level exactly once**
   (`crates/vike-script/src/strategy.rs`'s `discover_params`, the same one-time run the daemon's
   `RhaiStrategy::compile_with_params` performs). Rhai resolves a registered function when the
   line *runs*, so **nothing inside `fn on_bar()` is checked**: an unbound name, a typo, a wrong
   arity and a mistyped verb argument all compile cleanly and then raise on every bar until the
   strategy disables itself. That is why step 2 is not optional.

5. **Read the knobs back.** Call `discover_params` with `script: "<source>"`. Returns
   `params: [{name, default}, …]` in **declaration order**; defaults are always numbers. These are
   exactly the keys a profile's `[strategy.params]` or a `[sweep]` table may override. Unlike
   step 4, a compile error here *is* a tool error (`rhai compile error: …`) — call
   `validate_strategy` when you want a verdict, this when you want the knobs. If a knob you
   declared is missing, it is not at the top level.

6. **Run it.** Call `run_backtest` with `profile: "<backtest profile TOML>"` (required) and
   `script: "<source>"`. The script is injected as `[strategy.params].src`
   (`crates/vike-cli/src/cmd/backtest.rs`'s `inject_script_src`, which creates the
   `[strategy]`/`[strategy.params]` tables if absent and leaves every other key intact); the
   profile's `[strategy]` names `rhai`, and `[data]` names the series the server holds. The tool
   returns `{report: <BacktestReport JSON>}`; it needs a reachable datahub, and a connect failure
   is a clean tool error.
   - Read `n_trades` first. **Zero trades with a populated `zero_trade` diagnosis is the shape a
     self-disabled strategy leaves behind** — `zero_trade.causes[].code` of `no-orders` means
     nothing gated your orders because there were none: go back to steps 2–3 (an unbound name,
     a conditional indicator read, a comparison that can never be true, or a symbol that does
     not match the one the data is tagged with).
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

fn on_bar() {
    let m = sma(len.to_int());          // read first, every bar, unconditionally
    if m.is_nan() { return; }           // warm-up, AFTER every read
    let target = if close() > m { qty } else { 0.0 };
    let delta  = target - position();
    if delta >  1e-9 { market( 1,  delta); }
    if delta < -1e-9 { market(-1, -delta); }
}
```

## Diagnosing "it compiles but never trades"

| Symptom | Likely cause | Check |
| --- | --- | --- |
| `ok: true`, zero trades, `zero_trade` code `no-orders` | unbound indicator name / wrong arity / `market(1, 1)` | `list_indicators` with `name`; verb argument types |
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
