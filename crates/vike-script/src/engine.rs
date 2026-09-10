//! Rhai engine wiring: `build_engine` constructs a resource-limited `rhai::Engine` and registers
//! the order-verb host functions (`buy`/`sell`/`limit`/`market`) that a strategy script calls.
//! Each closure captures a cloned `SharedCtx` (never a borrowed `&mut Broker`) and records an
//! `Intent` into `ScriptCtx::intents` — no broker/strategy wiring happens here; `strategy.rs`
//! drains the recorded intents into real orders each bar.
//!
//! It also owns the INDICATOR BRIDGE and, with it, the answer to "which of
//! `vike_indicators::registry()` can a script actually call". That answer is DERIVED from the
//! registry, never hand-listed: [`RHAI_INDICATORS`] is `registry()` minus [`unbound_reason`]'s
//! exclusions, and [`register_indicators`] iterates the same predicate, so the advertised set and
//! the bound set cannot drift apart. The exclusions exist because a bound name must be
//! CORRECT, not merely constructible — see `exclusion` for the four rules and why each one is a
//! silent-wrong-answer hazard rather than a matter of taste.

use crate::ctx::{Intent, SharedCtx};
use std::sync::LazyLock;
use vike_indicators::IndicatorMeta;

/// The `tracing` target every script diagnostic lands on, whichever of this crate's two engines
/// produced it. One name so an operator can turn the whole surface up or down with a single
/// `RUST_LOG=vike_script=debug`, and so a `target:` typo cannot silently orphan one tier.
pub(crate) const SCRIPT_LOG_TARGET: &str = "vike_script";

/// Point a script's `print`/`debug` at the `tracing` facade instead of the HOST PROCESS'S STDOUT.
///
/// ⚠ **This is a correctness line, not a tidy-up, and the host it protects signs real orders.**
/// `rhai::Engine::new()` wires both hooks to `println!` (rhai-1.25.1 `src/engine.rs`'s `new`:
/// `engine.print = Some(Box::new(|s| println!("{s}")))`), so on a default engine a user's
/// `print("hi")` writes to whatever stream the embedding binary owns. This crate is embedded in
/// binaries whose stdout is a PROTOCOL rather than a console:
///
/// * `vike-cli mcp` speaks newline-JSON-RPC over `io::stdout()`, and two of its tools
///   (`validate_strategy`, `discover_params`) compile a caller-supplied script in-process —
///   `crates/vike-cli/src/cmd/mcp.rs`'s `tool_validate_strategy` reaches
///   [`crate::discover_params`], whose one-time top-level run is exactly where an author writes
///   `print`. A stray line there is a malformed frame, not noise.
/// * `vike-tradehub` mounts a `RhaiStrategy` on the LIVE core (`[strategy] rhai = …`) and keeps
///   its own stdout protocol-only for its newline-JSON control channel.
///
/// Nothing in this workspace READS a script's `print` off stdout — no tool, no test, no template
/// and no `.rhai` file in the tree emits one — so there is no consumer to break, only streams to
/// stop corrupting. (`vike-backtest`'s `backtest` bin is the third stream at risk and the mildest:
/// its stdout is a RESULT, not a protocol, so a stray line garbled a table rather than a frame.)
///
/// ⚠ **What this MOVES rather than removes is the VOLUME, and it moves it onto disk.** A `print`
/// inside `on_bar` fires once per bar either way; what changes is that a `tracing` event at INFO
/// reaches vike-log's FILE layer, which defaults to `trace` and rolls daily — so a chatty script
/// over a long backtest now writes into `<project>/settings/state/logs` instead of scrolling past a
/// terminal. That is the hazard `VIKE_LOG_FILE_LEVEL` exists for (CLAUDE.md's Logging section
/// carries the measurements, one of them a run that nearly filled a live trading node's disk), and
/// a script's diagnostics are subject to it like anything else. INFO rather than TRACE is
/// deliberate — a diagnostic the author explicitly asked for should be visible at the default
/// console level — but neither stream makes a per-bar `print` free, and this one leaves a file.
///
/// The study tier decided this first and is the spelling this one matches:
/// `crates/vike-studio-core/src/rhai_study/bind.rs`'s `build_engine`, whose module doc carries the
/// same argument. Both hooks are REDIRECTED rather than removed — a script's diagnostics are worth
/// keeping, just never on a stream somebody else owns.
///
/// `tier` distinguishes the two engines this crate builds (`build_engine` for a strategy,
/// `crates/vike-script/src/indicator.rs`'s `indicator_engine` for a user indicator) on one shared
/// target. It is a PARAMETER rather than two copies of these four lines precisely because missing
/// the second engine is the easy mistake here — `crates/vike-script/tests/module_import_refused.rs`
/// exists because the previous default-engine capability was closed in one of them first.
/// `crates/vike-script/tests/script_print_is_not_stdout.rs` gates both.
pub(crate) fn redirect_script_output(engine: &mut rhai::Engine, tier: &'static str) {
    engine.on_print(move |s| tracing::info!(target: SCRIPT_LOG_TARGET, tier, "{s}"));
    engine.on_debug(move |s, source, pos| {
        tracing::debug!(
            target: SCRIPT_LOG_TARGET,
            tier,
            source = source.unwrap_or(""),
            position = %pos,
            "{s}"
        );
    });
}

/// Builds a fresh `rhai::Engine` wired for one [`crate::RhaiStrategy`]: bounded resource limits
/// (opt-in on a plain `rhai::Engine::new()` — default is UNLIMITED) so a runaway or malicious
/// script cannot hang the strategy loop, plus the full read/verb/indicator host-function surface
/// registered against `ctx`. The limits are PER-HOOK-INVOCATION (each `call_fn` gets a fresh
/// operation counter), not cumulative across the strategy's lifetime.
///
/// Called ONCE per mounted strategy (`RhaiStrategy::compile_with_params` and `discover_params`),
/// never per bar — `run_hook` reuses the stored engine. That is what makes registering the whole
/// bound indicator set here affordable: rhai's function map is an identity-hashed `u64` table with
/// a per-call-site resolution cache, so the registration count changes neither per-call lookup cost
/// nor anything on the bar path.
///
/// ⚠ **`set_module_resolver` is the load-bearing line in this function, not a tidy-up.**
/// `rhai::Engine::new()` installs a `FileModuleResolver` rooted at the process's working directory
/// (rhai-1.25.1 `src/engine.rs`'s `new`), so `import "…" as m;` READS A FILE FROM DISK on a default
/// engine — the daemon's working directory being `<project>`, that is the operator's settings tree,
/// their credential store included. It is a capability NOBODY GRANTED: it appears in no
/// registration list because it is the interpreter's default rather than anything this host handed
/// over, which is exactly the kind of surface an allow-list cannot see.
/// `docs/decisions/0024-rhai-strategies-live.md` names "network, filesystem, process, or credential
/// access from a script" as a thing that would REOPEN the verdict that lets a script mount live,
/// and rests rail 4 on the live engine surface being exactly the backtest one — so closing this
/// keeps a premise that record already relies on rather than narrowing a decided grant.
/// `DummyModuleResolver` makes every `import` a refusal;
/// `crates/vike-script/tests/module_import_refused.rs`'s
/// `a_strategy_cannot_import_a_module_because_that_would_be_a_file_verb` is the gate.
/// The study tier closed the same hazard first — `crates/vike-studio-core/src/rhai_study/bind.rs`'s
/// `build_engine` is the spelling this one matches.
pub(crate) fn build_engine(ctx: &SharedCtx) -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    engine.set_max_operations(2_000_000);
    engine.set_max_call_levels(64);
    engine.set_max_string_size(8 * 1024);
    engine.set_max_array_size(4096);
    engine.set_max_map_size(4096);
    // Never the host's stdout — see `redirect_script_output`, and note that this daemon-hosted
    // engine is the tier where that matters most.
    redirect_script_output(&mut engine, "strategy");
    register_reads(&mut engine, ctx);
    register_verbs(&mut engine, ctx);
    register_indicators(&mut engine, ctx);
    // The INSTALLED user indicators, if a binary installed any — see `install_user_indicators`.
    // Registered BEFORE `register_user_indicators`' explicit set so an explicitly-passed indicator
    // wins on a name collision (`register_fn` replaces), which is what makes a test able to
    // override the process-wide set.
    if let Some(installed) = INSTALLED.get() {
        register_user_indicators(&mut engine, ctx, installed);
    }
    // param(name, default): a sweepable knob. Records (name, default) for discovery (first-seen
    // wins) and returns the injected override if present, else the default. Called at top level
    // (`let fast = param("fast", 5.0);`) so compile's one-time top-level run bakes the value in.
    let c = ctx.clone();
    // ⚠ The default is `Dynamic`, not `f64`. Registered as `f64` it accepted `param("fast", 5.0)`
    // and REJECTED `param("fast", 5)` with a rhai "Function not found" naming an i64 — a knob that
    // works or does not depending on whether the author typed a decimal point. Every shipped
    // template happens to write `5.0`, which is why it went unnoticed.
    engine.register_fn(
        "param",
        move |name: &str, default: rhai::Dynamic| -> Result<f64, Box<rhai::EvalAltResult>> {
            let d = arg(&default).ok_or_else(|| bad_arg("param", 2, &default))?;
            let mut g = c.write().unwrap();
            g.params_seen.entry(name.to_string()).or_insert(d);
            Ok(g.overrides.get(name).copied().unwrap_or(d))
        },
    );
    engine
}

/// Every NON-indicator host function name `build_engine` registers — the reads
/// ([`register_reads`]), the order verbs ([`register_verbs`]) and `param`.
///
/// This exists for the indicator bridge, not for documentation: `rhai::Engine::register_fn`
/// REPLACES a previous registration of the same name and arity rather than erroring, so binding a
/// registry indicator that happened to be called `close` would silently shadow `close()` for every
/// script in the workspace. `exclusion` therefore refuses to bind such a name. Nothing collides in
/// today's registry (its names are all technical-analysis terms) — this rule is green now and
/// bites only a future registry addition, the same shape as a `deny.toml` ban.
///
/// ⚠ A name added to `register_reads`/`register_verbs` must be added HERE too;
/// `host_fn_names_are_all_callable` catches a stale entry (a name listed but no longer
/// registered), not an unlisted new one — rhai exposes no "what did I register" query without its
/// `metadata` feature, which this workspace does not enable.
pub(crate) const HOST_FN_NAMES: &[&str] = &[
    "close", "open", "high", "low", "volume", "position", "price", "equity", "index", "now", "buy",
    "sell", "limit", "market", "param",
];

/// The highest argument count [`register_indicators`] registers a form for. Every bound indicator's
/// full parameter surface is reachable at this value today (`alma` is one of the deepest), and
/// `bound_indicator_arity_never_exceeds_the_registered_forms` FAILS if a future registry entry
/// needs one more, naming the arm to add. Without that gate a deeper indicator would bind quietly
/// with its tail parameters permanently pinned to the registry defaults — a knob that looks
/// present and is not.
pub(crate) const MAX_INDICATOR_ARITY: usize = 3;

/// Registers the order-verb host functions onto `engine`. Each verb pushes an `Intent` into
/// `ctx.intents` via a cloned `Arc` — the script never sees or mutates `ScriptCtx` directly.
/// Every name registered here must also appear in [`HOST_FN_NAMES`].
pub(crate) fn register_verbs(engine: &mut rhai::Engine, ctx: &SharedCtx) {
    let c = ctx.clone();
    engine.register_fn("market", move |side: i64, qty: f64| {
        c.write().unwrap().intents.push(Intent::Market { side: side as i32, qty });
    });
    let c = ctx.clone();
    engine.register_fn("buy", move |qty: f64| {
        c.write().unwrap().intents.push(Intent::Market { side: 1, qty });
    });
    let c = ctx.clone();
    engine.register_fn("sell", move |qty: f64| {
        c.write().unwrap().intents.push(Intent::Market { side: -1, qty });
    });
    let c = ctx.clone();
    engine.register_fn("limit", move |side: i64, qty: f64, price: f64| {
        c.write().unwrap().intents.push(Intent::Limit { side: side as i32, qty, price });
    });
}

/// Registers the snapshot-read host functions onto `engine`: `close`/`open`/`high`/`low`/
/// `volume`/`position`/`price`/`equity`/`index`/`now`. Each is a pure getter over one
/// `ScriptCtx` field via a cloned `Arc` — the read-only counterpart to `register_verbs` above.
/// Every name registered here must also appear in [`HOST_FN_NAMES`].
pub(crate) fn register_reads(engine: &mut rhai::Engine, ctx: &SharedCtx) {
    macro_rules! read_f64 {
        ($name:literal, $field:expr) => {{
            let c = ctx.clone();
            engine.register_fn($name, move || -> f64 {
                let g = c.read().unwrap();
                $field(&g)
            });
        }};
    }
    read_f64!("close", |g: &crate::ctx::ScriptCtx| g.cur_bar.close);
    read_f64!("open", |g: &crate::ctx::ScriptCtx| g.cur_bar.open);
    read_f64!("high", |g: &crate::ctx::ScriptCtx| g.cur_bar.high);
    read_f64!("low", |g: &crate::ctx::ScriptCtx| g.cur_bar.low);
    read_f64!("volume", |g: &crate::ctx::ScriptCtx| g.cur_bar.volume);
    read_f64!("position", |g: &crate::ctx::ScriptCtx| g.position);
    read_f64!("price", |g: &crate::ctx::ScriptCtx| g.price);
    read_f64!("equity", |g: &crate::ctx::ScriptCtx| g.equity);
    let c = ctx.clone();
    engine.register_fn("index", move || -> i64 { c.read().unwrap().index });
    let c = ctx.clone();
    engine.register_fn("now", move || -> i64 { c.read().unwrap().now });
}

/// Reads one Rhai argument as an indicator parameter.
///
/// Rhai dispatches on argument TYPE, so the indicator host functions take `rhai::Dynamic` and
/// convert here. That is ONE registration per arity instead of one per int/float permutation
/// (a 3-parameter indicator would otherwise need eight), and it is also the only shape under which
/// a MIXED call resolves: `alma(9, 0.85, 6)` is the natural spelling of an integer period, a float
/// offset and an integer sigma, and every shipped template writes periods as bare integers
/// (`crates/vike-studio-core/src/templates.rs`'s `SMA_CROSS` calls `sma(fast.to_int())`).
///
/// A non-numeric argument yields `None`, which every caller turns into a raised
/// [`bad_arg`] — see that function for why it is an error rather than a value. It deliberately does
/// NOT fall through to the registry default: an OMITTED argument already means "take the default"
/// (`vike_indicators::coerce` maps a missing/NaN entry to `ParamSpec::default`), so `sma("20")`
/// silently becoming `sma(20)` would hand a script author a plausible number for a typo.
fn arg(v: &rhai::Dynamic) -> Option<f64> {
    v.as_float().ok().or_else(|| v.as_int().ok().map(|i| i as f64))
}

/// A wrong-TYPE indicator argument, as a Rhai runtime error rather than a value.
///
/// ⚠ Returning `NaN` for `sma("20")` was the first shape of this and it is the wrong one, because
/// NaN is exactly what an indicator returns while it is WARMING UP. Every shipped template opens
/// `if h.is_nan() { return; }`, so a typed-wrong argument would make a strategy return on every bar
/// — silently, forever, looking like a warm-up that never finishes. The previous three-name binding
/// took a concrete `i64` and so failed loudly (no registration matched, function-not-found); moving
/// to `Dynamic` to reach multi-arity and fractional params gave that up, and this hands it back.
///
/// An error is loud in the way this engine already handles: `RhaiStrategy` counts consecutive
/// failures and self-disables, so the author sees a stopped strategy rather than a silent one.
/// An OMITTED argument is still "take the registry default" — that is `coerce`'s job and is
/// unaffected.
fn bad_arg(name: &str, position: usize, got: &rhai::Dynamic) -> Box<rhai::EvalAltResult> {
    format!(
        "{name}: argument {position} must be a number, got {}. An indicator parameter is numeric; \
         omit it to take the registry default.",
        got.type_name()
    )
    .into()
}

/// Why a `vike_indicators::registry()` entry is deliberately NOT bound as a Rhai host function —
/// `None` when it IS bound.
///
/// Pure over the four facts that decide it (rather than taking an `IndicatorMeta`) so that every
/// rule is unit-testable, INCLUDING the two that exclude nothing in today's registry. A rule that
/// can only be exercised by the data it happens to receive is a rule nobody can prove works — this
/// tree has already watched declaration-pinning tests stay green through real regressions, and a
/// dormant rule is the easiest place for that to happen again.
///
/// The rules, each a silent-WRONG-ANSWER hazard rather than a matter of taste:
///
/// 1. **Not callable from Rhai.** `var` (Variance) is a Rhai RESERVED WORD
///    (rhai-1.25.1 `src/tokenizer.rs`'s `RESERVED_LIST` holds `("var", true, false, false)`, whose
///    second flag — "may be called normally as a function" — is `false`), so `var(20)` fails at
///    PARSE time with a message about rhai's grammar, naming nothing an author can act on.
///    `Engine::register_fn` performs no name validation, so binding it would SUCCEED silently and
///    fail only when somebody wrote the call. Determined by asking rhai itself rather than by a
///    hand list, so a future rhai release that reserves another word cannot leave an
///    advertised-but-uncallable name behind.
/// 2. **Collides with a host function.** See [`HOST_FN_NAMES`].
/// 3. **`batch_only`.** `ichimoku`/`zigzag`/`williams_fractal` read FUTURE bars, so their streamed
///    value is a causal best-effort that the batch path later revises — a backtest-vs-chart
///    divergence that shows up as a wrong number, never as an error. (`ichimoku`'s line 0 is the
///    strictly-causal tenkan and would in fact be safe, but it is multi-output and excluded by
///    rule 4 anyway; excluding all three on the flag keeps ONE rule instead of a per-indicator
///    judgement.)
/// 4. **Multi-output UNDER ITS BARE NAME.** The bare-name form returns one scalar — `on_bar`'s line
///    0 — and for the BAND indicators that is the wrong line outright: `bollinger`/`donchian`/
///    `keltner`/`envelopes`/`std_error_bands` all declare `out!{"upper", "mid", "lower"}`, so
///    `bollinger(20)` would return the UPPER band to an author who read it as the middle.
///
///    ⚠ This rule NARROWED. It used to reject every multi-output indicator outright, and that was
///    too broad in one direction and not a solution in the other. Too broad, because on six of them
///    — `macd`, `adx`, `fisher`, `kst`, `kvo`, `supertrend` — line 0's own `OutSpec::name` IS the
///    indicator's name, so `macd()` was always the macd line and was refused for a hazard that does
///    not apply to it. Not a solution, because refusing `bollinger` left its middle band
///    unreachable by any spelling at all.
///
///    So the test is now NAMESAKE, not arity: a bare name binds when line 0 is named after the
///    indicator, and every line of every multi-output indicator is separately reachable through the
///    per-line accessors [`line_fn_name`] generates. `bollinger` is still refused under its bare
///    name — `bollinger_mid()` is the spelling — and that refusal is now a signpost rather than a
///    dead end.
fn exclusion(
    name: &str,
    batch_only: bool,
    line0: Option<&str>,
    outputs: usize,
    params: usize,
    callable_in_rhai: bool,
) -> Option<&'static str> {
    if !callable_in_rhai {
        return Some(
            "the name is a Rhai reserved word or symbol, so a script calling it fails at PARSE \
             time (today: `var` -> Variance; call it from Rust, or use `stddev`)",
        );
    }
    if HOST_FN_NAMES.contains(&name) {
        return Some(
            "the name collides with a host read/verb, and binding it would SHADOW that function \
             for every script",
        );
    }
    if batch_only {
        return Some(
            "batch_only: it reads FUTURE bars, so the streamed value is retroactively revised and \
             disagrees with the chart",
        );
    }
    if params > MAX_INDICATOR_ARITY {
        return Some(
            "more parameters than the bridge can express: `register_indicators` writes forms up to              MAX_INDICATOR_ARITY, so binding this would pin every parameter past that to its              registry default — knobs that look present and are not (today: `kst`, 9 parameters).              Construct it from Rust, or add the arity arms and raise the constant",
        );
    }
    if !bare_name_binds(name, outputs, line0) {
        return Some(
            "multi-output whose line 0 is NOT the namesake line, so a bare call would return a \
             line the caller did not ask for (`bollinger` returns `upper` first). Every line has \
             its own accessor — call `<name>_<line>()`, e.g. `bollinger_mid()`",
        );
    }
    None
}

/// Whether an indicator's BARE name binds, and therefore IS line 0 — [`exclusion`]'s rule 4, and
/// the one function both sides of that rule ask.
///
/// A single-output indicator's bare name is its one line, so it always binds. A multi-output one's
/// binds only when line 0 is the NAMESAKE line, because otherwise the bare call returns a line the
/// caller did not ask for (`bollinger()` handing back `upper`), and every line is separately
/// reachable through [`line_fn_name`] either way.
///
/// ⚠ It sanitises the INDICATOR name as well as the line, which the registry side never needed:
/// every registry name is already a bare lowercase identifier, while a USER file's stem is whatever
/// the filesystem allowed (`Shouty.RHAI` loads today — `crates/vike-script/src/load.rs`'s
/// `the_extension_is_matched_case_insensitively`). Sharing ONE function is therefore only
/// behaviour-preserving for the built-ins while `sanitize_line` is the IDENTITY on a registry name
/// — a claim about DATA, so it is a test, `one_namesake_rule_serves_the_registry_and_the_user_files`,
/// which fails the day an entry takes a capital, a dash or a `%` rather than silently flipping
/// which bare names bind.
pub(crate) fn bare_name_binds(name: &str, outputs: usize, line0: Option<&str>) -> bool {
    outputs == 1 || line0.map(sanitize_line) == Some(sanitize_line(name))
}

/// One output line's name, as the suffix of a Rhai host-function name.
///
/// Lowercases and replaces every character a Rhai identifier cannot carry with `_`. This exists for
/// exactly one family and it is not hypothetical: `stochastic`, `stochf` and `stochrsi` declare
/// their lines as **`%K`** and **`%D`** (`crates/vike-indicators/src/registry.rs`'s `out` macro),
/// and `stochastic_%K` is not a name a script can call — it is a PARSE error about rhai's grammar,
/// which is the same dead end [`exclusion`]'s first rule refuses `var` for.
///
/// ⚠ Sanitising is lossy in principle: two distinct line names can map onto one suffix (`%K` and
/// `K`), which would silently give one accessor two meanings. That is a silent-wrong-answer hazard,
/// so it is GATED rather than assumed away — `generated_line_names_are_unambiguous` proves the whole
/// generated set is collision-free, and it is written to fail on the future indicator that breaks
/// it rather than on today's registry, which is clean.
pub(crate) fn sanitize_line(line: &str) -> String {
    line.chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9') => c,
            // Everything else — `%`, a space, a dash — becomes `_`, then the trim below drops any
            // that ended up leading or trailing. `%K` therefore reads as `k`, not `_k`.
            _ => '_',
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// The Rhai host-function name for one output line: `"<indicator>_<line>"`.
///
/// `bollinger_mid`, `macd_signal`, `adx_plus_di`, `stochastic_k`. The separator is `_` because that
/// is what the registry's own multi-word line names already use (`plus_di`, `aroon_up`,
/// `bull_power`), so the composite reads as one identifier rather than as two conventions joined.
pub fn line_fn_name(indicator: &str, line: &str) -> String {
    format!("{indicator}_{}", sanitize_line(line))
}

/// Every per-line accessor name the BUILT-IN bridge registers (`bollinger_mid`, `stochastic_k`, …),
/// built once per process from the same [`line_accessors`] derivation `register_indicators` binds
/// from.
///
/// ⚠ These are FUNCTION names a script resolves, not registry entries, so
/// `registry().iter().any(|m| m.name == n)` does not see them — which is how a user file called
/// `stochastic_k.rhai` could silently take over the built-in accessor of that name. Registration
/// order makes it certain rather than lucky: `build_engine` registers the user set AFTER
/// `register_indicators`, and `register_fn` REPLACES. So this set joins the refusals in
/// [`user_indicator_conflict`] and [`user_line_conflict`].
static BUILTIN_LINE_FNS: LazyLock<std::collections::HashSet<String>> = LazyLock::new(|| {
    vike_indicators::registry()
        .iter()
        .flat_map(|m| line_accessors(m.name).into_iter().map(|(_, f)| f))
        .collect()
});

/// The registry entries the Rhai host deliberately leaves unbound, name -> reason. Built ONCE per
/// process (this is a `LazyLock`, not per-`build_engine` work), because the callable-in-Rhai half of
/// [`exclusion`] is answered by compiling `<name>()` on a throwaway `rhai::Engine::new_raw()` — a
/// raw engine parses identically to the one `build_engine` returns (reserved words are a tokenizer
/// property; `build_engine` disables no symbols and defines no custom keywords) and skips the
/// standard packages, and rhai's default `OptimizationLevel::Simple` never eagerly evaluates a call,
/// so the probe measures the GRAMMAR and nothing else.
static UNBOUND: LazyLock<indexmap::IndexMap<&'static str, &'static str>> = LazyLock::new(|| {
    let probe = rhai::Engine::new_raw();
    vike_indicators::registry()
        .iter()
        .filter_map(|m| {
            let callable = probe.compile(format!("{}()", m.name)).is_ok();
            let line0 = m.outputs.first().map(|o| o.name);
            exclusion(m.name, m.batch_only, line0, m.outputs.len(), m.params.len(), callable)
                .map(|why| (m.name, why))
        })
        .collect()
});

/// The EXACT indicator names [`register_indicators`] binds as Rhai host functions — the
/// HOST-BOUND callable set. Exported (re-exported at the crate root) so any surface that advertises
/// "the indicators a script can call" (vike-cli's MCP `list_indicators` tool) lists THIS set and
/// never `vike_indicators::registry()` wholesale: a script calling an unbound registry name hits a
/// Rhai function-not-found error every bar and self-disables after the strategy's
/// consecutive-error cap (see `strategy.rs`).
///
/// DERIVED, not written down: it is `registry()` in registry order, minus every entry
/// [`unbound_reason`] rejects. `register_indicators` filters on the same predicate, so the binding
/// cannot drift from the advertisement in either direction, and a new registry indicator becomes
/// callable the moment it lands — no list to remember. That is why this is a `LazyLock<Vec<..>>`
/// rather than the `&'static [&'static str]` const it used to be (`registry()` is built at run
/// time, so no `const` can name its contents); ⚠ a consumer iterating it directly needs
/// `RHAI_INDICATORS.iter()` — `LazyLock` derefs for method calls but does not implement
/// `IntoIterator`.
///
/// The count is deliberately not stated here. `RHAI_INDICATORS.len()` is the answer, and every
/// prose copy of a number in this workspace has rotted.
pub static RHAI_INDICATORS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    vike_indicators::registry()
        .iter()
        .map(|m| m.name)
        .filter(|n| !UNBOUND.contains_key(n))
        .collect()
});

/// Why `name` — a `vike_indicators::registry()` indicator — is NOT callable from a Rhai script, or
/// `None` when it is bound (and also `None` for a name that is in no registry at all: an unknown
/// name is a typo, not an exclusion).
///
/// Exported so an advertising surface can say WHY a real indicator is missing instead of silently
/// omitting it, and so a downstream test needing a guaranteed-unbound name can ask rather than
/// hard-code one that later becomes bound.
pub fn unbound_reason(name: &str) -> Option<&'static str> {
    UNBOUND.get(name).copied()
}

/// Looks up (or lazily constructs via `IndicatorMeta::make_with`) the streaming indicator cached
/// under `"<name>:<coerced params>"` in `ctx.indicators`, feeds it `ctx.cur_bar` at most once per
/// bar (guarded by `ctx.fed_this_bar`), and returns its scalar output (`on_bar`/`value`'s index
/// `[0]`, which is the ONLY line — [`exclusion`] refuses every multi-output indicator precisely so
/// that this `.first()` cannot be a truncation). A second reference to the same indicator with the
/// same parameters within one bar returns the cached `value()[0]` without re-advancing.
///
/// `meta` is the `&'static IndicatorMeta` captured by the host closure at registration, so the bar
/// path performs no registry lookup at all and an unknown name is unrepresentable here — a script
/// naming something unbound gets a Rhai function-not-found error, which is what
/// [`RHAI_INDICATORS`]' contract promises.
///
/// The key is built from the COERCED parameters (`vike_indicators::coerce`, the registry's one
/// value-coercion site) rather than from the raw arguments, so the cache key is the instance's real
/// identity: `sma()` and `sma(20)` are the same indicator and share one entry — and therefore one
/// `fed_this_bar` slot — instead of streaming two copies of it side by side.
fn indicator_value(ctx: &SharedCtx, meta: &'static IndicatorMeta, raw: &[f64], line: usize) -> f64 {
    let params = vike_indicators::coerce(meta.params, raw);
    let key = format!("{}:{params:?}", meta.name);
    let mut g = ctx.write().unwrap();
    if !g.indicators.contains_key(&key) {
        // `make_with` coerces again internally; `coerce` is idempotent (a coerced value is
        // non-NaN and already inside [min, max]), and the registry's own doc asks callers outside
        // it to coerce first, so passing the coerced slice is the documented call shape.
        g.indicators.insert(key.clone(), meta.build_with(&params));
    }
    if g.fed_this_bar.insert(key.clone()) {
        let bar = g.cur_bar.clone();
        let out = g.indicators.get_mut(&key).unwrap().on_bar(&bar);
        return out.get(line).copied().unwrap_or(f64::NAN);
    }
    // already fed this bar -> return cached value without re-advancing
    g.indicators.get(&key).unwrap().value().get(line).copied().unwrap_or(f64::NAN)
}

/// The process-wide user-indicator set, installed once by a BINARY — see
/// [`install_user_indicators`].
static INSTALLED: LazyLock<std::sync::OnceLock<Vec<crate::RhaiIndicator>>> =
    LazyLock::new(std::sync::OnceLock::new);

/// Installs the user's indicators (`user_data/indicators/`) process-wide, so EVERY strategy this
/// process compiles can call them — including through the six `harness::run_*` entry points and the
/// Studio runners, none of which take an indicator argument.
///
/// ## Why a process-wide set rather than a parameter on every path
///
/// The alternative is threading `&[RhaiIndicator]` through `run_backtest`, `run_sweep`,
/// `run_sweep_with`, `run_walkforward`, `run_sweep_euler`, the Studio's `build_strategy` funnel and
/// the datahub proto verbs that call them — public signatures, all of which exist to describe a
/// PROFILE, none of which has anything to do with where a user keeps their files.
///
/// The shape is not new here: `vike_indicators::registry()` — the ~140 BUILT-IN indicators every
/// script already calls — is itself a process-wide static that no run path passes around. This makes
/// the user's own indicators the same kind of thing as the built-ins, which is also how an author
/// thinks of them.
///
/// ## The rule that keeps it honest: only a BINARY may call this
///
/// The precedent is `vike_log::init` — binaries call it, libraries use the facade. A library that
/// installed indicators would be reaching for a directory its caller can neither see nor override,
/// which is the exact defect `crates/vike-ops/tests/settings_registry.rs`'s `CREDENTIAL_STORE_PIN`
/// ratchets down. So this takes ALREADY-COMPILED prototypes: the caller owns the directory
/// resolution and the file I/O (`crates/vike-script/src/load.rs`'s `load_user_indicators` +
/// `vike_model::state_path::user_indicators_dir`), and this function performs none.
///
/// ⚠ That is a property of THIS function, not of the crate: its caller-side pair
/// `crates/vike-script/src/load.rs`'s `load_and_install_user_indicators` performs exactly the I/O
/// described above, so that a root wires the whole thing in one call instead of copying three
/// statements. The rule the pair preserves is that the CALLER names the directory — which it takes
/// as a parameter and never resolves for itself.
///
/// ## Once, and observably
///
/// Returns `Err` with the already-installed count if called twice, rather than silently keeping the
/// first set or replacing it: a second call means two places believe they own this, and both
/// answers would be wrong somewhere. `RhaiStrategy::compile_with_indicators` still takes an explicit
/// set and OVERRIDES the installed one per name, which is what lets a test bind its own without
/// touching process state.
pub fn install_user_indicators(indicators: Vec<crate::RhaiIndicator>) -> Result<(), String> {
    let n = indicators.len();
    INSTALLED.set(indicators).map_err(|_| {
        format!(
            "user indicators are already installed ({} of them); install_user_indicators is a \
             once-per-process call made by a binary at startup, and this second call of {n} would \
             have silently done nothing",
            INSTALLED.get().map(Vec::len).unwrap_or(0)
        )
    })
}

/// The installed user-indicator NAMES — the file stems, which is each indicator's identity. Empty
/// when a binary installed none.
///
/// ⚠ A name here is not necessarily a CALLABLE spelling: a multi-output file whose line 0 is not its
/// namesake has no bare name, exactly as `bollinger` does not. A surface advertising what a script
/// may TYPE must join this with [`installed_user_bare_call`] and [`installed_user_line_accessors`],
/// the way `vike-cli indicators` joins `RHAI_INDICATORS` with [`line_accessors`] —
/// `crates/vike-cli/src/cmd/indicators.rs`'s `user_call_forms` is that join.
pub fn installed_user_indicators() -> Vec<&'static str> {
    INSTALLED
        .get()
        .map(|v| v.iter().map(vike_indicators::Indicator::name).collect())
        .unwrap_or_default()
}

/// Every CALLABLE spelling of one registry indicator's output lines, as `(line name, function
/// name)` pairs in output order — `[("upper", "bollinger_upper"), ("mid", "bollinger_mid"), …]`.
///
/// Empty for a single-output indicator (its bare name already IS line 0), and empty for a
/// `batch_only` one (no line of a future-reading indicator is bound — see
/// [`register_indicators`]).
///
/// Exported because the advertising surfaces must not have to REDERIVE this. `RHAI_INDICATORS`
/// answers "which bare names bind", and after per-line accessors landed that stopped being the
/// whole callable set: `bollinger` is absent from it while `bollinger_mid` is callable. A surface
/// listing only the bare names would now be telling a user that a band indicator is unreachable,
/// which is exactly the drift `RHAI_INDICATORS`' own doc exists to prevent — so the second half of
/// the answer is exported beside the first, from the same derivation `register_indicators` binds
/// from.
pub fn line_accessors(indicator: &str) -> Vec<(&'static str, String)> {
    let Some(meta) = vike_indicators::registry().iter().find(|m| m.name == indicator) else {
        return Vec::new();
    };
    if meta.outputs.len() < 2 || meta.batch_only || meta.params.len() > MAX_INDICATOR_ARITY {
        return Vec::new();
    }
    meta.outputs
        .iter()
        .map(|o| (o.name, line_fn_name(meta.name, o.name)))
        .filter(|(_, f)| {
            !HOST_FN_NAMES.contains(&f.as_str())
                && !vike_indicators::registry().iter().any(|m| m.name == *f)
                && rhai::Engine::new_raw().compile(format!("{f}()")).is_ok()
        })
        .collect()
}

/// Whether a script can reach `indicator` by ANY spelling — its bare name, or at least one per-line
/// accessor.
///
/// ⚠ **This, not `RHAI_INDICATORS.contains(..)`, is the question every advertising surface means to
/// ask.** Those were the same question until per-line accessors landed and split them: `bollinger`
/// is absent from `RHAI_INDICATORS` (its bare name is refused — line 0 is the upper band) while
/// `bollinger_mid(20)` is perfectly callable. A surface still asking the old question reports a
/// band indicator as unreachable and offers the user no way in, which is worse than the silence it
/// replaced.
///
/// [`unbound_reason`] remains the answer to the NARROWER question "why is the BARE name refused",
/// and it is still the right thing to print — its message names the accessor that works.
pub fn is_callable(indicator: &str) -> bool {
    RHAI_INDICATORS.contains(&indicator) || !line_accessors(indicator).is_empty()
}

/// One installed user indicator's `param(name, default)` knobs, in first-seen order — the same
/// `(name, default)` pairs a built-in advertises as `IndicatorMeta::params`, for a surface that
/// prints a CALL FORM. Empty for an unknown name and for a file that declares no knob.
///
/// Exported for the same reason [`line_accessors`] is: an advertising surface must not REDERIVE the
/// call shape. A user indicator gets one registered form per argument count from zero up to this
/// list's length (`crates/vike-script/src/engine.rs`'s `register_user_indicators`), so a listing
/// that printed a bare `my_ema` beside
/// the built-ins' `sma(period=20)` would be telling the author their own indicator takes no
/// arguments — which is the same silent wrongness `vike-cli indicators` was narrowed to prevent,
/// pointed at the one indicator whose parameters the author actually chose.
///
/// ⚠ Deliberately keyed on the NAME rather than returned wholesale beside
/// [`installed_user_indicators`]: the pair is then two lookups against one static, and a caller that
/// wants only the names never pays for the `String` clones the parameters cost.
pub fn installed_user_indicator_params(name: &str) -> Vec<(&'static str, f64)> {
    installed_by_name(name)
        .map(|i| i.params().iter().map(|(n, d)| (n.as_str(), *d)).collect())
        .unwrap_or_default()
}

/// One INSTALLED user indicator, by name — the lookup every `installed_user_*` accessor shares, so
/// the `OnceLock` is read ONE way and no caller is handed a `RhaiIndicator` of its own to stream.
fn installed_by_name(name: &str) -> Option<&'static crate::RhaiIndicator> {
    INSTALLED.get()?.iter().find(|i| vike_indicators::Indicator::name(*i) == name)
}

/// One INSTALLED user indicator's CALLABLE per-line spellings, `(line name, function name)` in
/// declaration order — the [`line_accessors`] twin for a file the user wrote, keyed on the name for
/// the same reason [`installed_user_indicator_params`] is.
///
/// Empty for a single-output file (its bare name already IS line 0, exactly as on the built-in
/// side) and for a name nothing installed.
///
/// ⚠ Exported because [`installed_user_indicators`] alone stopped being the callable set the moment
/// a user file could declare `fn outputs()`: a band file has NO bare name (see
/// [`installed_user_bare_call`]) and is reachable only through these. A surface that listed the
/// names alone would print a call that raises on every bar and omit every spelling that works —
/// the exact failure `vike-cli indicators` exists to prevent, pointed at the one indicator the
/// author wrote themselves.
pub fn installed_user_line_accessors(name: &str) -> Vec<(String, String)> {
    installed_by_name(name).map(user_line_accessors).unwrap_or_default()
}

/// Whether an INSTALLED user indicator's BARE name resolves — the user-side twin of
/// `RHAI_INDICATORS.contains(..)`, answered by the shared [`bare_name_binds`].
///
/// `false` with a non-empty [`installed_user_line_accessors`] is the band shape: reachable, but
/// only line by line. `false` is also the answer for a name nothing installed, which is the truth
/// about it — no spelling of it resolves.
pub fn installed_user_bare_call(name: &str) -> bool {
    installed_by_name(name).is_some_and(|i| user_bare_name_binds(name, i.outputs()))
}

/// A `(stem, line)` pair whose generated accessor IS a built-in indicator's own name — DERIVED from
/// the registry so it cannot rot when an indicator is renamed, and PROVEN rather than assumed (the
/// reconstruction is asserted by the callers that use it).
///
/// Every registry name containing a `_` is a candidate, because [`line_fn_name`] joins with exactly
/// that separator: `smi_ergodic` is what a file `smi.rhai` declaring a line `ergodic` would spell.
/// The one taken is the first whose STEM is itself a free user-indicator name, so a LOADER test
/// reaches `compile_indicator` instead of being refused for the file's own name first.
#[cfg(test)]
pub(crate) fn registry_name_a_user_line_could_spell() -> (&'static str, &'static str) {
    vike_indicators::registry()
        .iter()
        .filter_map(|m| m.name.split_once('_'))
        .find(|&(stem, line)| {
            // The accessor this pair GENERATES must be the registry name it was split out of —
            // sanitisation is the identity on a registry name's tail, and this checks that rather
            // than trusting it, so the witness cannot silently stop being one.
            vike_indicators::get(&line_fn_name(stem, line)).is_some()
                && user_indicator_conflict(stem).is_none()
        })
        .expect("a registry name with a `_`, whose stem is a free user-indicator name")
}

/// The first BUILT-IN per-line accessor this build registers, as `(indicator, line, accessor)` —
/// derived from [`line_accessors`] over the whole registry, so no test has to name one.
///
/// ⚠ Lines that sanitise to NOTHING are skipped. [`line_accessors`] does not filter them (they
/// still spell a callable `<name>_`), but [`user_line_conflict`] answers such a line under its FIRST
/// rule, and a caller asking this for a per-line-accessor witness would get the wrong refusal.
#[cfg(test)]
pub(crate) fn a_builtin_line_accessor() -> (&'static str, &'static str, String) {
    vike_indicators::registry()
        .iter()
        .find_map(|m| {
            line_accessors(m.name)
                .into_iter()
                .find(|(l, _)| !sanitize_line(l).is_empty())
                .map(|(l, f)| (m.name, l, f))
        })
        .expect("this build registers at least one per-line accessor")
}

/// Why a USER-written indicator (`user_data/indicators/<name>.rhai`) cannot be bound under that
/// name — `None` when the name is free.
///
/// The rules are [`exclusion`]'s, asked about a user file instead of a registry entry, plus one
/// that only exists on this side: a user name may not take a BUILT-IN indicator's name. All four
/// are refusals of SILENT SHADOWING, which is the only failure mode here that a script author
/// could not diagnose from their own file.
///
/// ⚠ The built-in rule deliberately refuses even the names `exclusion` leaves unbound (`var`,
/// `bollinger`, …). Those are not free slots: they are names this engine has REASONS to withhold,
/// and letting a user file occupy `bollinger` would make `bollinger(20)` resolve to something
/// unrelated to Bollinger bands on that one machine — a wrong answer that travels with the script
/// and looks like a built-in to everyone reading it.
pub fn user_indicator_conflict(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("an indicator file needs a name".into());
    }
    if rhai::Engine::new_raw().compile(format!("{name}()")).is_err() {
        return Some(format!(
            "`{name}` is not callable from Rhai — the name is a reserved word or contains a \
             character the grammar reads as a symbol, so a script calling it would fail at PARSE \
             time. Rename the file."
        ));
    }
    if HOST_FN_NAMES.contains(&name) {
        return Some(format!(
            "`{name}` is a built-in host function (a read, an order verb, or `param`), and binding \
             it would SHADOW that function for every script. Rename the file."
        ));
    }
    if vike_indicators::registry().iter().any(|m| m.name == name) {
        let extra = unbound_reason(name)
            .map(|why| format!(" (that built-in is itself unbound from Rhai — {why})"))
            .unwrap_or_default();
        return Some(format!(
            "`{name}` is a built-in indicator{extra}. A user file may not take a built-in's name: \
             the same call would mean different things on different machines. Rename the file."
        ));
    }
    if BUILTIN_LINE_FNS.contains(name) {
        return Some(format!(
            "`{name}` is a built-in indicator's per-line accessor (see `vike-cli indicators \
             --json`), and the user set is registered LAST, so this file would silently answer \
             every `{name}()` in every strategy. Rename the file."
        ));
    }
    None
}

/// Why a USER indicator's declared output LINE cannot become a per-line accessor — `None` when the
/// generated name ([`line_fn_name`]) is free.
///
/// The line half of [`user_indicator_conflict`], and deliberately the SAME four refusals
/// `register_indicators` applies to a built-in's generated accessor, plus the one that only exists
/// on this side (a built-in accessor's own name). Applied at COMPILE, so an author is told which
/// line to rename instead of losing an accessor silently — the built-in side can only `continue`
/// past a bad name, because the registry is not the reader's to fix.
///
/// ⚠ **What a generated accessor can and cannot collide with.** [`line_fn_name`] joins the two
/// halves with a literal `_`, so the name this asks about ALWAYS carries one. Only a name that
/// contains a `_` is therefore reachable at all — which is why the host-read/verb refusal below
/// cannot fire today (no [`HOST_FN_NAMES`] entry has one) while the built-in-indicator refusal can
/// (`smi_ergodic`, `williams_fractal`, … are registry names a `<stem>_<line>` accessor spells
/// exactly). Both rules stay, because that is a property of the separator and of two lists, either
/// of which can change; `every_user_line_rule_is_reachable_or_provably_not` asserts which of them
/// is which so the unreachable one cannot go quietly reachable.
///
/// ⚠ **One residual, DECLARED rather than silently tolerated: collisions BETWEEN two user files.**
/// This sees one file, so `a.rhai` declaring a line `b` (accessor `a_b`) and a file `a_b.rhai` are
/// invisible to each other, and the later registration wins. Nothing here can answer it: the
/// question needs the whole set, which only the loader (`load.rs`, which already reports
/// `DuplicateName` for two files claiming one stem) and `register_user_indicators` ever hold. It is
/// narrower than the built-in collision this DOES refuse — both files are the reader's own, in one
/// directory they can list — which is why it is documented and not built.
pub fn user_line_conflict(indicator: &str, line: &str) -> Option<String> {
    if sanitize_line(line).is_empty() {
        return Some(format!(
            "a line named `{line}` carries no character a Rhai identifier can hold, so it spells no \
             accessor at all. Give it a name with letters or digits in it."
        ));
    }
    let fname = line_fn_name(indicator, line);
    if rhai::Engine::new_raw().compile(format!("{fname}()")).is_err() {
        return Some(format!(
            "the accessor it generates, `{fname}`, is not callable from Rhai — it is a reserved \
             word, or the file's own name carries a character the grammar reads as a symbol, so a \
             script calling it would fail at PARSE time. Rename the line (or the file)."
        ));
    }
    if HOST_FN_NAMES.contains(&fname.as_str()) {
        return Some(format!(
            "the accessor it generates, `{fname}`, is a host read/verb, and binding it would \
             SHADOW that function for every script. Rename the line."
        ));
    }
    if vike_indicators::registry().iter().any(|m| m.name == fname) {
        return Some(format!(
            "the accessor it generates, `{fname}`, is a built-in indicator's own name. Rename the \
             line."
        ));
    }
    if BUILTIN_LINE_FNS.contains(&fname) {
        return Some(format!(
            "the accessor it generates, `{fname}`, is a built-in indicator's per-line accessor. \
             Rename the line."
        ));
    }
    None
}

/// [`bare_name_binds`], asked about a user file's declared line list — the ONE rule, not a second
/// spelling of it. `my_bands()` handing back `upper` is the same wrong answer `bollinger()` was
/// refused for, and every line stays reachable through [`user_line_accessors`] either way.
pub(crate) fn user_bare_name_binds(name: &str, lines: &[String]) -> bool {
    bare_name_binds(name, lines.len(), lines.first().map(String::as_str))
}

/// Every CALLABLE per-line spelling of one USER indicator's outputs, as `(line name, function name)`
/// pairs in declaration order — the [`line_accessors`] twin for a file the user wrote.
///
/// Empty for a single-output indicator (its bare name already IS line 0), exactly as on the built-in
/// side. There is no filtering arm here and that is not an oversight: every rule
/// [`line_accessors`] applies at registration time is applied to a user file at COMPILE, by
/// [`user_line_conflict`], so a `RhaiIndicator` that exists at all has usable line names.
pub fn user_line_accessors(ind: &crate::RhaiIndicator) -> Vec<(String, String)> {
    let name = vike_indicators::Indicator::name(ind);
    let lines = ind.outputs();
    if lines.len() < 2 {
        return Vec::new();
    }
    lines.iter().map(|l| (l.clone(), line_fn_name(name, l))).collect()
}

/// Registers user-written indicators (`user_data/indicators/`) onto `engine` as host functions,
/// alongside the built-in bridge.
///
/// Each is fed the current bar EXACTLY once per bar through the same `ctx.indicators` /
/// `fed_this_bar` cache the built-ins use, so a script referencing `my_thing()` three times in one
/// bar streams it once — the property `sma_bridge_second_reference_same_bar_is_cached` gates for
/// the built-in side and `user_indicator_is_fed_once_per_bar` gates here.
///
/// TWO families of name, exactly as [`register_indicators`] generates for a built-in and from the
/// SAME [`line_fn_name`]:
///
/// - the **bare name**, `my_thing()` — bound for a single-output file, and for a multi-output one
///   whose line 0 is the namesake line. See [`user_bare_name_binds`].
/// - a **per-line accessor** for every line of a multi-output file, `my_bands_mid()`
///   ([`user_line_accessors`]). This is what makes a user-written band indicator's middle band
///   reachable at all, and it is why `on_bar` may now return an array.
///
/// ⚠ **All the lines of one user indicator share ONE streaming instance**, because
/// [`register_user_form`] keys the cache on `(indicator name, arguments)` — the LINE is captured by
/// the closure and never enters the key. Three accessor reads in one bar therefore feed the file's
/// `on_bar` once and index three entries of the one vector it returned. A line in the key would run
/// the author's recurrence three times over the same bars: same numbers at triple the cost, three
/// `fed_this_bar` slots where the contract says one, and — since a user `on_bar` is arbitrary code —
/// three independent copies of state that only agree while it stays deterministic.
///
/// One form per argument count, from zero up to the file's own `param()` count (capped by
/// [`MAX_INDICATOR_ARITY`]) — the same ladder the built-ins get, for the same reason: an argument
/// past the declared surface must be a function-not-found rather than a value silently discarded.
/// Every line accessor offers the same ladder as the bare name, so `my_bands_mid(50)` takes what
/// `my_bands(50)` would.
///
/// A name [`user_indicator_conflict`] rejects is SKIPPED rather than bound — the caller is expected
/// to have surfaced the reason at load time (`vike-cli init` and the Studio loader both do), and
/// binding it anyway would be the shadowing the rule exists to prevent. Its LINES need no such arm:
/// [`user_line_conflict`] refused a bad one at compile, where the author is told which to rename.
pub(crate) fn register_user_indicators(
    engine: &mut rhai::Engine,
    ctx: &SharedCtx,
    inds: &[crate::RhaiIndicator],
) {
    for ind in inds {
        let name = vike_indicators::Indicator::name(ind).to_string();
        if user_indicator_conflict(&name).is_some() {
            continue;
        }
        let arity = ind.params().len().min(MAX_INDICATOR_ARITY);
        if user_bare_name_binds(&name, ind.outputs()) {
            for argc in 0..=arity {
                register_user_form(engine, ctx, ind, &name, &name, 0, argc);
            }
        }
        for (line, (_, fname)) in user_line_accessors(ind).into_iter().enumerate() {
            for argc in 0..=arity {
                register_user_form(engine, ctx, ind, &name, &fname, line, argc);
            }
        }
    }
}

/// Registers ONE argument-count form of ONE spelling (the bare name, or one line accessor) of a user
/// indicator. `name` is the indicator's own name and is the CACHE IDENTITY; `fname` is the spelling
/// being registered; `line` is which entry of `on_bar`'s return this spelling reads.
///
/// ⚠ **The cache key carries the arguments and NOT the line**, `"user:<name>:<args>"`. Two halves,
/// both load-bearing:
///
/// - `my_mean(20)` and `my_mean(50)` are two streaming instances rather than one — exactly the
///   identity rule [`indicator_value`] applies to the built-ins. A key on the name alone would make
///   the SECOND call site in a script silently read the first one's lookback.
/// - `my_bands_upper(20)` and `my_bands_mid(20)` are ONE instance read at two indices. `fname` is
///   deliberately not in the key: it is the spelling, not the identity, and putting it there would
///   stream one file's recurrence once per line.
///
/// The instance is built by re-running the file's top level with the arguments bound to its
/// `param()` declarations (`RhaiIndicator::compile_with`), so a knob genuinely changes the
/// recurrence rather than being recorded and ignored.
fn register_user_form(
    engine: &mut rhai::Engine,
    ctx: &SharedCtx,
    ind: &crate::RhaiIndicator,
    name: &str,
    fname: &str,
    line: usize,
    argc: usize,
) {
    // The compiled indicator is the PROTOTYPE. It is re-instantiated per distinct argument tuple
    // and cached in `ctx.user_indicators`, so two strategies built from one loaded set — and two
    // call sites with different knobs — never share streaming state.
    //
    // ⚠ It is cached as a CONCRETE `RhaiIndicator`, in its own map, rather than boxed into
    // `ctx.indicators` beside the built-ins. That map is `Box<dyn Indicator>`, and reading the fault
    // back out of a trait object would mean either a downcast (`Indicator` exposes no `as_any`) or
    // widening the shared trait with a method that means nothing to the ~140 built-ins. The fault is
    // the whole reason this bridge can be loud; a concrete map keeps it reachable without either.
    let proto = ind.clone();
    let c = ctx.clone();
    let n = name.to_string();
    // The line list the ACCESSORS were generated from — the prototype's, i.e. the file read at its
    // `param()` defaults. Checked against each re-instantiation below.
    let declared: Vec<String> = ind.outputs().to_vec();

    // The bar path, shared by every form: resolve-or-build the instance for THESE arguments, feed it
    // at most once this bar, raise a fault, return the value.
    let body = move |args: &[f64]| -> Result<f64, Box<rhai::EvalAltResult>> {
        let key = format!("{n}:{args:?}");
        let mut g = c.write().unwrap();
        if !g.user_indicators.contains_key(&key) {
            // A re-instantiation can FAIL — the file's top level runs again with the caller's
            // values, and a script that divides by a knob will throw when that knob is 0. Raising
            // here is the same choice `bad_arg` makes: an error the author can see beats a NaN they
            // cannot distinguish from warm-up.
            let built = proto
                .compile_with(args)
                .map_err(|e| -> Box<rhai::EvalAltResult> { e.to_string().into() })?;
            // ⚠ `fn outputs()` runs AFTER the top level, so it can read a `param()` knob and return
            // a different list for a different call site — and the accessors were already
            // registered from the prototype's list. `my_bands_lower(9)` would then read index 2 of
            // a two-line vector: NaN, forever, with no fault. Raise instead, naming both lists.
            if built.outputs() != declared.as_slice() {
                return Err(format!(
                    "{n}: `fn outputs()` must not depend on a `param()` knob — the accessors were \
                     registered from [{}], but with argument(s) {args:?} this file declares [{}]. \
                     Return a fixed list.",
                    declared.join(", "),
                    built.outputs().join(", ")
                )
                .into());
            }
            g.user_indicators.insert(key.clone(), built);
        }
        // One `fed_this_bar` namespace shared with the built-ins. The `user:` prefix is what keeps
        // that safe: a built-in key is `"<name>:<coerced params>"`, which this would otherwise
        // collide with for a same-named paramless pair — `user_indicator_conflict` refuses that
        // name anyway, but relying on it from here would make this correct only by a rule enforced
        // somewhere else.
        let fed_key = format!("user:{key}");
        if g.fed_this_bar.insert(fed_key) {
            let bar = g.cur_bar.clone();
            vike_indicators::Indicator::on_bar(g.user_indicators.get_mut(&key).unwrap(), &bar);
        }
        let slot = g.user_indicators.get(&key).unwrap();
        // ⚠ The fault is RAISED, not passed on as the NaN it already is. `on_bar` had no error
        // channel (see `indicator.rs`), but this bridge does — and a warm-up-shaped NaN that
        // actually means "your script threw" is the exact failure `bad_arg` exists to prevent, one
        // layer down.
        if let Some(msg) = slot.fault() {
            return Err(msg.to_string().into());
        }
        // `line`, not `.first()`: the whole point of the accessors. Out of range is unreachable
        // (`read_values` pins `value().len()` to `outputs().len()`, which the accessors were
        // generated from) and reads as the warm-up NaN rather than panicking if that ever changes.
        Ok(vike_indicators::Indicator::value(slot).get(line).copied().unwrap_or(f64::NAN))
    };

    // `bad_arg` names the function the AUTHOR called — `my_bands_mid`, not `my_bands`: somebody who
    // wrote `my_bands_mid("20")` is not helped by an error about the indicator behind it.
    let label: &'static str = Box::leak(fname.to_string().into_boxed_str());
    match argc {
        0 => {
            engine.register_fn(label, move || body(&[]));
        }
        1 => {
            engine.register_fn(label, move |a: rhai::Dynamic| {
                let x = arg(&a).ok_or_else(|| bad_arg(label, 1, &a))?;
                body(&[x])
            });
        }
        2 => {
            engine.register_fn(label, move |a: rhai::Dynamic, b: rhai::Dynamic| {
                let x = arg(&a).ok_or_else(|| bad_arg(label, 1, &a))?;
                let y = arg(&b).ok_or_else(|| bad_arg(label, 2, &b))?;
                body(&[x, y])
            });
        }
        _ => {
            engine.register_fn(
                label,
                move |a: rhai::Dynamic, b: rhai::Dynamic, d: rhai::Dynamic| {
                    let x = arg(&a).ok_or_else(|| bad_arg(label, 1, &a))?;
                    let y = arg(&b).ok_or_else(|| bad_arg(label, 2, &b))?;
                    let z = arg(&d).ok_or_else(|| bad_arg(label, 3, &d))?;
                    body(&[x, y, z])
                },
            );
        }
    }
}

/// Registers the indicator-bridge host functions onto `engine` — every `vike_indicators::registry()`
/// entry [`unbound_reason`] does not reject, each backed by a streaming instance cached in
/// `ctx.indicators` and fed the current bar exactly once per bar (see [`indicator_value`]).
///
/// TWO families of name, both derived from the registry:
///
/// - the **bare name**, `sma(20)` — bound for every single-output indicator, and for the
///   multi-output ones whose line 0 is the namesake line (`macd`, `adx`, …). See [`exclusion`]'s
///   rule 4 for why the others are refused a bare name.
/// - a **per-line accessor** for every line of every multi-output indicator,
///   [`line_fn_name`]`(indicator, line)` — `bollinger_mid(20)`, `macd_signal(12, 26, 9)`,
///   `stochastic_k(14)`. This is what makes a band indicator's middle band reachable at all.
///
/// ⚠ **All the lines of one indicator share ONE streaming instance**, because [`indicator_value`]
/// keys the cache on `(name, coerced params)` with NO line in the key. That is the whole point:
/// `bollinger_upper(20)` and `bollinger_mid(20)` in one bar feed the indicator ONCE and read two
/// entries of the same output vector. A line in the key would stream three independent copies of
/// bollinger side by side — same numbers, triple the work, and three `fed_this_bar` slots where the
/// contract says one.
///
/// One form per argument count, `0 ..= meta.params.len()` (capped by [`MAX_INDICATOR_ARITY`]):
///
/// - `name()` — every parameter at its registry default. This is the ONLY spelling for the
///   parameterless indicators (the candlestick patterns, `vwap`, `obv`, …), and the only way to
///   reach a FRACTIONAL default: `psar`'s `step` defaults to `0.02` with a `[0.001, 0.5]` range,
///   which no integer argument can express (`psar(0)` clamps to `0.001`, `psar(1)` to `0.5`).
/// - `name(p1)`, `name(p1, p2)`, `name(p1, p2, p3)` — up to that indicator's own parameter count.
///   Arguments beyond it are NOT registered, so `doji(5)` is a function-not-found rather than an
///   argument silently discarded, while parameters left off the end take their registry defaults
///   (`vike_indicators::coerce`'s documented contract, which is what makes the short forms safe).
///
/// Arguments are `rhai::Dynamic` so ints and floats both resolve — see [`arg`].
pub(crate) fn register_indicators(engine: &mut rhai::Engine, ctx: &SharedCtx) {
    for meta in vike_indicators::registry().iter() {
        // The BARE name, unless `exclusion` refused it. A refusal here is not a refusal of the
        // indicator: its per-line accessors below are registered regardless, which is how
        // `bollinger_mid` exists while `bollinger` does not.
        if !UNBOUND.contains_key(meta.name) {
            register_arity_forms(engine, ctx, meta, meta.name.to_string(), 0);
        }
        // The PER-LINE accessors. Single-output indicators get none — `sma_sma()` would be noise
        // beside `sma()`, and the bare name already IS line 0 for them.
        //
        // ⚠ Gated on the same `batch_only` rule as the bare name, deliberately: `ichimoku` and
        // `williams_fractal` are multi-output AND read future bars, so a per-line accessor would
        // hand back exactly the retroactively-revised value rule 3 exists to refuse. The rules
        // COMPOSE — lifting the multi-output refusal must not quietly lift that one too.
        // ⚠ The accessors obey the SAME rules the bare name does — `batch_only` (a future-reading
        // line is wrong however it is spelled) and the arity ceiling (`kst_signal(...)` could no
        // more express 9 parameters than `kst(...)` could). Lifting the multi-output refusal must
        // not become a side door around the other three.
        if meta.outputs.len() < 2 || meta.batch_only || meta.params.len() > MAX_INDICATOR_ARITY {
            continue;
        }
        for (idx, out) in meta.outputs.iter().enumerate() {
            let fname = line_fn_name(meta.name, out.name);
            // Same three refusals a bare name faces. None of them fires on today's registry
            // (`generated_line_names_are_bindable` proves it), so this is the future-proofing arm.
            if HOST_FN_NAMES.contains(&fname.as_str())
                || vike_indicators::registry().iter().any(|m| m.name == fname)
                || rhai::Engine::new_raw().compile(format!("{fname}()")).is_err()
            {
                continue;
            }
            register_arity_forms(engine, ctx, meta, fname, idx);
        }
    }
}

/// Registers `fname()` … `fname(p1, p2, p3)` for one indicator and one output line.
///
/// Factored out because the bare name and every per-line accessor must offer the SAME parameter
/// surface: `bollinger_mid(20, 2.0)` has to take the same arguments as `bollinger(20, 2.0)` would,
/// and two copies of this ladder would be two places for that to drift.
fn register_arity_forms(
    engine: &mut rhai::Engine,
    ctx: &SharedCtx,
    meta: &'static IndicatorMeta,
    fname: String,
    line: usize,
) {
    let arity = meta.params.len().min(MAX_INDICATOR_ARITY);
    // `bad_arg` names the function the AUTHOR called, not the registry entry behind it: someone who
    // wrote `bollinger_mid("20")` is not helped by an error about `bollinger`.
    let n0 = fname.clone();
    let c = ctx.clone();
    engine.register_fn(n0.as_str(), move || -> f64 { indicator_value(&c, meta, &[], line) });
    if arity >= 1 {
        let c = ctx.clone();
        let n: &'static str = Box::leak(fname.clone().into_boxed_str());
        engine.register_fn(n, move |a: rhai::Dynamic| -> Result<f64, Box<rhai::EvalAltResult>> {
            let x = arg(&a).ok_or_else(|| bad_arg(n, 1, &a))?;
            Ok(indicator_value(&c, meta, &[x], line))
        });
    }
    if arity >= 2 {
        let c = ctx.clone();
        let n: &'static str = Box::leak(fname.clone().into_boxed_str());
        engine.register_fn(
            n,
            move |a: rhai::Dynamic, b: rhai::Dynamic| -> Result<f64, Box<rhai::EvalAltResult>> {
                let x = arg(&a).ok_or_else(|| bad_arg(n, 1, &a))?;
                let y = arg(&b).ok_or_else(|| bad_arg(n, 2, &b))?;
                Ok(indicator_value(&c, meta, &[x, y], line))
            },
        );
    }
    if arity >= 3 {
        let c = ctx.clone();
        let n: &'static str = Box::leak(fname.into_boxed_str());
        engine.register_fn(
            n,
            move |a: rhai::Dynamic,
                  b: rhai::Dynamic,
                  d: rhai::Dynamic|
                  -> Result<f64, Box<rhai::EvalAltResult>> {
                let x = arg(&a).ok_or_else(|| bad_arg(n, 1, &a))?;
                let y = arg(&b).ok_or_else(|| bad_arg(n, 2, &b))?;
                let z = arg(&d).ok_or_else(|| bad_arg(n, 3, &d))?;
                Ok(indicator_value(&c, meta, &[x, y, z], line))
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{Intent, ScriptCtx};
    use vike_model::Bar;

    /// A deterministic, non-degenerate OHLCV series: every field moves, `high >= low`, and volume
    /// varies — so an indicator reading any of them produces a CHANGING value rather than a
    /// constant a broken bridge could still reproduce.
    fn series(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let t = i as f64;
                let c = 100.0 + (t * 0.7).sin() * 12.0 + t * 0.05;
                Bar {
                    ts: i as i64 * 60_000,
                    open: c - 0.4,
                    high: c + 1.3,
                    low: c - 1.7,
                    close: c,
                    volume: 1_000.0 + (t * 0.3).cos() * 400.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: None,
                }
            })
            .collect()
    }

    /// Bit-for-bit float comparison with both-NaN treated as equal — the workspace convention (see
    /// vike-indicators' crate doc: never widen a tolerance, compare `to_bits`).
    fn same(a: f64, b: f64) -> bool {
        (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
    }

    fn engine_with_indicators(ctx: &SharedCtx) -> rhai::Engine {
        let mut engine = rhai::Engine::new();
        register_indicators(&mut engine, ctx);
        engine
    }

    /// Drives `src` (one indicator reference) through the bridge over `bars`, returning its value
    /// at each bar. Compiles ONCE and re-evaluates the AST, so the per-bar cost is an eval rather
    /// than a parse — the whole-registry test below runs this for every bound indicator.
    fn bridge_series(src: &str, bars: &[Bar]) -> Vec<f64> {
        let ctx = ScriptCtx::new();
        let engine = engine_with_indicators(&ctx);
        let ast = engine.compile(src).unwrap_or_else(|e| panic!("{src} failed to compile: {e}"));
        bars.iter()
            .map(|bar| {
                {
                    let mut g = ctx.write().unwrap();
                    g.cur_bar = bar.clone();
                    g.fed_this_bar.clear();
                }
                engine.eval_ast::<f64>(&ast).unwrap_or_else(|e| panic!("{src} failed: {e}"))
            })
            .collect()
    }

    /// The oracle: a directly-constructed streaming instance fed each bar exactly once.
    fn oracle_series(meta: &IndicatorMeta, raw: &[f64], bars: &[Bar]) -> Vec<f64> {
        let mut ind = meta.build_with(&vike_indicators::coerce(meta.params, raw));
        bars.iter().map(|b| ind.on_bar(b).first().copied().unwrap_or(f64::NAN)).collect()
    }

    #[test]
    fn verbs_record_intents() {
        let ctx = ScriptCtx::new();
        let mut engine = rhai::Engine::new();
        register_verbs(&mut engine, &ctx);
        engine.run("buy(2.0); sell(1.0); limit(1, 3.0, 100.0); market(-1, 4.0);").unwrap();
        let got = &ctx.read().unwrap().intents;
        assert_eq!(
            got,
            &vec![
                Intent::Market { side: 1, qty: 2.0 },
                Intent::Market { side: -1, qty: 1.0 },
                Intent::Limit { side: 1, qty: 3.0, price: 100.0 },
                Intent::Market { side: -1, qty: 4.0 },
            ]
        );
    }

    #[test]
    fn reads_reflect_snapshot() {
        let ctx = ScriptCtx::new();
        {
            let mut g = ctx.write().unwrap();
            g.cur_bar.close = 101.0;
            g.position = 0.0;
        }
        let mut engine = rhai::Engine::new();
        register_reads(&mut engine, &ctx);
        register_verbs(&mut engine, &ctx);
        engine.run("if close() > 100.0 && position() == 0.0 { buy(1.0); }").unwrap();
        assert_eq!(ctx.read().unwrap().intents.len(), 1);
        ctx.write().unwrap().intents.clear();
        ctx.write().unwrap().cur_bar.close = 99.0;
        engine.run("if close() > 100.0 && position() == 0.0 { buy(1.0); }").unwrap();
        assert_eq!(ctx.read().unwrap().intents.len(), 0);
    }

    #[test]
    fn sma_bridge_matches_direct() {
        let closes = [10.0, 11.0, 12.0, 13.0, 14.0];
        // oracle: direct streaming SMA(3)
        let mut direct = vike_indicators::make_with("sma", &[3.0]).unwrap();
        // subject: the bridge
        let ctx = ScriptCtx::new();
        let engine = engine_with_indicators(&ctx);
        for (i, &c) in closes.iter().enumerate() {
            let bar = Bar {
                ts: i as i64,
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 0.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            };
            {
                let mut g = ctx.write().unwrap();
                g.cur_bar = bar.clone();
                g.fed_this_bar.clear();
            }
            let via_bridge: f64 = engine.eval("sma(3)").unwrap();
            let via_direct = direct.on_bar(&bar)[0];
            assert_eq!(via_bridge.to_bits(), via_direct.to_bits(), "bar {i}");
        }
    }

    /// Regression test for the fed-once-per-bar caching contract: a SECOND reference to
    /// `sma(3)` within the same bar (no `fed_this_bar.clear()` between the two calls) must
    /// return the cached `value()[0]` untouched, not feed `on_bar` again. Non-vacuous by
    /// construction: the trailing assertion proves that feeding the oracle indicator twice on
    /// one bar actually changes its output, so a double-feed regression in `indicator_value`
    /// would necessarily desync the bridge's two same-bar reads and fail this test.
    #[test]
    fn sma_bridge_second_reference_same_bar_is_cached() {
        let closes = [10.0, 11.0, 12.0, 13.0, 14.0];
        let make_bar = |i: usize, c: f64| Bar {
            ts: i as i64,
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        };

        // oracle: direct streaming SMA(3), fed exactly once per bar
        let mut direct = vike_indicators::make_with("sma", &[3.0]).unwrap();
        // subject: the bridge
        let ctx = ScriptCtx::new();
        let engine = engine_with_indicators(&ctx);

        let last = closes.len() - 1;
        let mut oracle_single_fed = f64::NAN;
        for (i, &c) in closes.iter().enumerate() {
            let bar = make_bar(i, c);
            {
                let mut g = ctx.write().unwrap();
                g.cur_bar = bar.clone();
                g.fed_this_bar.clear();
            }
            if i == last {
                // Two references to sma(3) on the SAME bar, no fed_this_bar.clear() between
                // them: the first feeds on_bar; the second must be the cached value, untouched.
                let first: f64 = engine.eval("sma(3)").unwrap();
                let second: f64 = engine.eval("sma(3)").unwrap();
                assert_eq!(
                    first.to_bits(),
                    second.to_bits(),
                    "second same-bar reference must return the cached value without re-advancing"
                );
                oracle_single_fed = direct.on_bar(&bar)[0];
                assert_eq!(
                    first.to_bits(),
                    oracle_single_fed.to_bits(),
                    "bridge value (fed once, read twice) must match an oracle fed once on this bar"
                );
            } else {
                let _: f64 = engine.eval("sma(3)").unwrap();
                direct.on_bar(&bar);
            }
        }

        // Non-vacuity: feeding the oracle the final bar a SECOND time (mirroring what a
        // double-feed regression in `indicator_value` would do) must diverge from the
        // single-fed value — otherwise this test could never fail even if the bridge started
        // double-feeding.
        let oracle_double_fed = direct.on_bar(&make_bar(last, closes[last]))[0];
        assert_ne!(
            oracle_single_fed.to_bits(),
            oracle_double_fed.to_bits(),
            "feeding twice must change the result, or this test cannot catch a double-feed regression"
        );
    }

    /// THE correctness gate for widening the bound set: for EVERY name in [`RHAI_INDICATORS`],
    /// the bridge's per-bar value must equal a directly-fed streaming oracle bit-for-bit over a
    /// whole series. That is simultaneously the "returns a real value" and the "advances exactly
    /// once per bar" claim — a skipped feed, a double feed, a shared cache slot or a mis-coerced
    /// parameter each desync the bridge from the oracle at the first affected bar.
    ///
    /// Non-vacuous in two directions: an all-NaN pass is impossible because most of the bound set
    /// must produce at least one finite value (asserted as a RELATION against
    /// `RHAI_INDICATORS.len()` rather than as a count, so it cannot rot), and the set itself is
    /// asserted to have grown well past the three names that used to be hand-listed.
    #[test]
    fn every_bound_indicator_streams_bit_identically_and_advances_once_per_bar() {
        // 150 bars is a deliberate compromise: nearly every indicator in the catalog streams by
        // history-recompute (`stream_tail`), so this loop is O(bars^2) per indicator across the
        // whole bound set, and the parity claim it checks does not get stronger with a longer
        // series — only the warm-up coverage does, which the `finite` relation below already
        // pins loosely and `previously_unbound_indicators_now_return_real_values` pins exactly.
        let bars = series(150);
        let mut finite = 0usize;
        for &name in RHAI_INDICATORS.iter() {
            let meta = vike_indicators::get(name).expect("advertised name must be in the registry");
            let got = bridge_series(&format!("{name}()"), &bars);
            let want = oracle_series(meta, &[], &bars);
            for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                assert!(same(*g, *w), "{name} diverged at bar {i}: bridge {g} vs oracle {w}");
            }
            if got.iter().any(|v| v.is_finite()) {
                finite += 1;
            }
        }
        assert!(
            finite * 2 > RHAI_INDICATORS.len(),
            "most of the bound set must produce a real value over {} bars, got {finite}/{}",
            bars.len(),
            RHAI_INDICATORS.len()
        );
        assert!(
            RHAI_INDICATORS.len() > 100,
            "the bound set is derived from the registry now, not the old three-name list"
        );
    }

    /// The same bit-parity claim, for the ARGUMENT forms — which the zero-arity gate above cannot
    /// reach.
    ///
    /// ⚠ `{name}()` and `{name}(p)` are SEPARATE rhai registrations, so proving one says nothing
    /// about the other: a wrong argument order, a dropped parameter, or a `Dynamic` conversion that
    /// silently coerced would all pass the zero-arity gate untouched. Every parameterised indicator
    /// is driven here at its OWN declared default, so the call is spelled the way a script would
    /// spell it and the oracle is fed the identical slice.
    #[test]
    fn every_argument_form_streams_bit_identically_too() {
        let bars = series(150);
        let mut checked = 0usize;
        for &name in RHAI_INDICATORS.iter() {
            let meta = vike_indicators::get(name).expect("advertised name must be in the registry");
            let arity = meta.params.len().min(MAX_INDICATOR_ARITY);
            if arity == 0 {
                continue;
            }
            let params: Vec<f64> = meta.params.iter().take(arity).map(|p| p.default).collect();
            let args = params.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>().join(", ");
            let got = bridge_series(&format!("{name}({args})"), &bars);
            let want = oracle_series(meta, &params, &bars);
            for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                assert!(
                    same(*g, *w),
                    "{name}({args}) diverged at bar {i}: bridge {g} vs oracle {w}"
                );
            }
            checked += 1;
        }
        assert!(
            checked > 40,
            "the parameterised set must be non-trivial; only {checked} indicators took an argument"
        );
    }

    /// A wrong-TYPE argument is an ERROR, not a NaN.
    ///
    /// ⚠ This is the regression the `Dynamic` migration introduced and `bad_arg` exists to undo.
    /// NaN is what an indicator returns while WARMING UP, and every shipped template opens
    /// `if h.is_nan() { return; }` — so a NaN here makes a typo look like a warm-up that never
    /// finishes, and the strategy silently never trades. An error self-disables the strategy after
    /// the consecutive-failure cap, which the author can actually see.
    #[test]
    fn a_wrong_typed_argument_raises_rather_than_returning_a_warmup_shaped_nan() {
        let ctx = ScriptCtx::new();
        let engine = build_engine(&ctx);
        let err = engine
            .eval::<f64>(r#"sma("20")"#)
            .expect_err("a string argument must not resolve to a value");
        let msg = err.to_string();
        assert!(msg.contains("must be a number"), "unhelpful message: {msg}");
        assert!(msg.contains("sma"), "the message must name the indicator: {msg}");
        // ...and the correct spelling still works, so the test cannot pass by breaking `sma`.
        assert!(engine.eval::<f64>("sma(20)").is_ok(), "a numeric argument must still resolve");
    }

    /// The user-visible claim, spelled out on indicators that were NOT callable before: each
    /// returns a finite value, that value CHANGES across bars (so it is really streaming, not a
    /// warm-up constant), and it matches a directly-fed oracle. `obv` is here deliberately — it is
    /// parameterless, so `obv()` is its only spelling and it exercises the 0-arity form on an
    /// indicator that has no parameters at all.
    #[test]
    fn previously_unbound_indicators_now_return_real_values() {
        let bars = series(120);
        for (src, name, raw) in [
            ("wma(10)", "wma", vec![10.0]),
            ("atr(14)", "atr", vec![14.0]),
            ("cci(20)", "cci", vec![20.0]),
            ("obv()", "obv", vec![]),
            ("tema(12)", "tema", vec![12.0]),
        ] {
            let meta = vike_indicators::get(name).unwrap();
            let got = bridge_series(src, &bars);
            let want = oracle_series(meta, &raw, &bars);
            for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                assert!(same(*g, *w), "{src} diverged at bar {i}: {g} vs {w}");
            }
            let tail: Vec<f64> = got.iter().rev().take(20).copied().collect();
            assert!(
                tail.iter().all(|v| v.is_finite()),
                "{src} must be warm and finite at the tail"
            );
            assert!(
                tail.windows(2).any(|w| w[0].to_bits() != w[1].to_bits()),
                "{src} must CHANGE across bars — a constant would pass a broken bridge too"
            );
        }
    }

    /// Every registry entry is in exactly one of two states, and both are checked against a real
    /// engine: advertised in [`RHAI_INDICATORS`] AND callable, or carrying an [`unbound_reason`]
    /// AND genuinely not callable. This is the anti-drift property [`RHAI_INDICATORS`]' doc
    /// promises, now over the whole registry rather than over a hand-written const.
    #[test]
    fn the_advertised_set_is_exactly_the_bound_set() {
        let ctx = ScriptCtx::new();
        let engine = build_engine(&ctx);
        for meta in vike_indicators::registry() {
            let advertised = RHAI_INDICATORS.contains(&meta.name);
            let reason = unbound_reason(meta.name);
            assert_eq!(
                advertised,
                reason.is_none(),
                "{} is advertised={advertised} but unbound_reason={reason:?}",
                meta.name
            );
            // A `Dynamic` result accepts whatever the call returns; only OK-ness is under test.
            let call = engine.eval::<rhai::Dynamic>(&format!("{}()", meta.name));
            assert_eq!(
                call.is_ok(),
                advertised,
                "{}: advertised={advertised} but calling it was {:?}",
                meta.name,
                call.err().map(|e| e.to_string())
            );
        }
    }

    /// The exclusions, named. Each of these is a real, constructible registry indicator that a
    /// script must NOT be able to call, for the reason [`exclusion`] documents — and every one is
    /// a silent-wrong-answer hazard, so the test asserts both halves: a reason exists, and the
    /// call genuinely fails.
    #[test]
    fn excluded_indicators_are_real_but_not_callable() {
        let ctx = ScriptCtx::new();
        let engine = build_engine(&ctx);
        // ⚠ `macd` and `adx` USED to be on this list and are deliberately off it now. They were
        // excluded for being multi-output, and that rule was over-broad: line 0 of each is its own
        // namesake line, so `macd()` was always the macd line and was refused for a hazard that
        // does not apply to it. One name per surviving rule, so this stays a rule-coverage check
        // rather than a snapshot of whatever happened to be refused:
        //   bollinger  — multi-output, line 0 is `upper` (NOT the namesake)
        //   stochastic — same, line 0 is `%K`
        //   ichimoku   — batch_only AND multi-output
        //   zigzag     — batch_only
        //   var        — a Rhai reserved word
        //   kst        — 9 parameters, more than the bridge can express
        for name in ["bollinger", "stochastic", "ichimoku", "zigzag", "var", "kst"] {
            assert!(
                vike_indicators::make_with(name, &[]).is_some(),
                "test premise: {name} is a real registry indicator"
            );
            assert!(unbound_reason(name).is_some(), "{name} must carry an exclusion reason");
            assert!(!RHAI_INDICATORS.contains(&name), "{name} must not be advertised as callable");
            assert!(
                engine.eval::<rhai::Dynamic>(&format!("{name}()")).is_err(),
                "{name} must not be callable from a script"
            );
        }
        // ...and the counterpart, without which the narrowing above goes unverified: a namesake
        // multi-output indicator IS callable under its bare name now.
        for name in ["macd", "adx"] {
            assert!(unbound_reason(name).is_none(), "{name}'s line 0 is its namesake — it binds");
            assert!(RHAI_INDICATORS.contains(&name), "{name} must be advertised as callable");
        }
    }

    /// Pins the premise behind the `var` exclusion, which is about RHAI'S GRAMMAR and nothing
    /// about the indicator: `var(..)` cannot parse, so no binding scheme can make it callable.
    /// Without this the exclusion looks like a preference somebody could "clean up".
    #[test]
    fn var_is_a_rhai_reserved_word_and_cannot_be_called_at_all() {
        let raw = rhai::Engine::new_raw();
        assert!(raw.compile("var()").is_err(), "premise: `var(..)` is a Rhai parse error");
        assert!(raw.compile("variance()").is_ok(), "premise: an ordinary name parses fine");
        assert!(vike_indicators::get("var").is_some(), "premise: `var` is a real indicator");
    }

    /// The arity forms follow each indicator's own parameter count: `0 ..= params.len()` exists,
    /// anything beyond it does not. The negative half is the point — a parameterless indicator
    /// must REJECT `doji(5)` rather than accept and discard the argument, which is the shape that
    /// invites an author to believe the number meant something.
    #[test]
    fn arity_forms_follow_the_parameter_count() {
        let ctx = ScriptCtx::new();
        let engine = build_engine(&ctx);
        let ok = |src: &str| engine.eval::<f64>(src).is_ok();
        // sma: exactly one parameter.
        assert!(ok("sma()"), "0-arg form (all defaults) must exist");
        assert!(ok("sma(20)"), "an integer argument must resolve");
        assert!(ok("sma(20.0)"), "a float argument must resolve too");
        assert!(!ok("sma(20, 3)"), "sma has ONE parameter: a 2-arg form must not exist");
        // doji: no parameters at all.
        assert!(ok("doji()"), "a parameterless indicator is callable with no arguments");
        assert!(!ok("doji(5)"), "a parameterless indicator must REJECT an argument, not ignore it");
        // alma: three parameters, and the natural call mixes integers and floats.
        assert!(ok("alma(9)"), "short forms take registry defaults for the rest");
        assert!(ok("alma(9, 0.85)"), "two of three parameters");
        assert!(ok("alma(9, 0.85, 6)"), "MIXED int/float arguments must resolve");
        assert!(!ok("alma(9, 0.85, 6, 1)"), "alma has THREE parameters, not four");
    }

    /// Arguments really drive the instance (they are not decoration), and the 0-arg form really
    /// means "registry defaults" — including a FRACTIONAL default no integer argument can express.
    /// `psar`'s `step` defaults to 0.02 within `[0.001, 0.5]`, so before the 0-arg form existed the
    /// indicator was unreachable at any sane setting.
    #[test]
    fn the_zero_arg_form_means_registry_defaults_including_fractional_ones() {
        let bars = series(60);
        // sma's default length is 20: sma() and sma(20) must be the same instance's values...
        let default_form = bridge_series("sma()", &bars);
        let explicit_20 = bridge_series("sma(20)", &bars);
        assert!(
            default_form.iter().zip(&explicit_20).all(|(a, b)| same(*a, *b)),
            "sma() must equal sma(20), its registry default"
        );
        // ...while a different argument must genuinely differ, or the argument is decoration.
        let explicit_3 = bridge_series("sma(3)", &bars);
        assert!(
            default_form.iter().zip(&explicit_3).any(|(a, b)| !same(*a, *b)),
            "sma(3) must differ from sma(): the argument has to reach the instance"
        );
        // psar: the fractional default, reachable only through the 0-arg form.
        let meta = vike_indicators::get("psar").unwrap();
        let got = bridge_series("psar()", &bars);
        let want = oracle_series(meta, &[], &bars);
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            assert!(same(*g, *w), "psar() diverged at bar {i}: {g} vs {w}");
        }
        let clamped = bridge_series("psar(1)", &bars);
        assert!(
            got.iter().zip(&clamped).any(|(a, b)| !same(*a, *b)),
            "psar(1) clamps step to its 0.5 maximum and must differ from the 0.02 default"
        );
    }

    /// A non-numeric argument must NOT resolve to the registry default — the same claim the test
    /// this replaced made, now checked against the behaviour that superseded it.
    ///
    /// ⚠ Its predecessor asserted the argument reads as NaN. That was true and it was the wrong
    /// design: omitting an argument ALREADY means "take the default", so a typo had to be
    /// distinguishable from both — and NaN is not, because NaN is what an indicator returns while
    /// WARMING UP. `bad_arg` raises instead; this test keeps the original guarantee (a typo never
    /// silently becomes a plausible number) while
    /// `a_wrong_typed_argument_raises_rather_than_returning_a_warmup_shaped_nan` pins the new
    /// louder shape.
    #[test]
    fn a_non_numeric_argument_never_resolves_to_the_registry_default() {
        let ctx = ScriptCtx::new();
        let engine = engine_with_indicators(&ctx);
        assert!(
            engine.eval::<f64>(r#"sma("20")"#).is_err(),
            "a string argument must raise, not quietly take sma's default period"
        );
    }

    /// Each of [`exclusion`]'s four rules, exercised directly — including the ones that exclude
    /// nothing in today's registry (a rule proven only by the data it happens to receive is a rule
    /// nobody can trust). The `false`/`true` flags spell out the fact under test in each case.
    #[test]
    fn every_exclusion_rule_is_reachable() {
        // 1. not callable from Rhai (a reserved word).
        assert!(exclusion("var", false, Some("var"), 1, 1, false).is_some());
        // 2. collides with a host function — vacuous against today's registry, live as a rule.
        assert!(exclusion("close", false, Some("close"), 1, 1, true).is_some());
        assert!(exclusion("param", false, Some("param"), 1, 1, true).is_some());
        // 3. batch_only.
        assert!(exclusion("zigzag", true, Some("zigzag"), 1, 1, true).is_some());
        // 4. multi-output whose line 0 is NOT the namesake line -> no BARE name.
        assert!(exclusion("bollinger", false, Some("upper"), 3, 2, true).is_some());
        // ...but multi-output whose line 0 IS the namesake binds its bare name. This is the half
        // of rule 4 that was previously refused for a hazard that does not apply to it: `macd()`
        // has always been the macd line.
        assert!(exclusion("macd", false, Some("macd"), 3, 3, true).is_none());
        // ...and the namesake test reads the SANITISED line name, so `%K` is compared as `k`.
        assert!(exclusion("stochastic", false, Some("%K"), 2, 3, true).is_some());
        // ...and the accepting case, which every bound indicator takes.
        assert!(exclusion("sma", false, Some("sma"), 1, 1, true).is_none());
        // Rule ORDER: an unparseable name reports the parse problem, not a later rule.
        assert_eq!(
            exclusion("var", true, Some("upper"), 3, 3, false),
            exclusion("var", false, Some("var"), 1, 1, false)
        );
    }

    /// [`HOST_FN_NAMES`] must have no stale entry: every name in it is genuinely registered by
    /// `build_engine`, so the collision rule in [`exclusion`] protects real functions rather than
    /// reserving names nothing uses. (The other direction — a newly registered host fn that was
    /// never added to the list — is not machine-checkable here; rhai exposes no registration
    /// query without its `metadata` feature. `HOST_FN_NAMES`' doc carries that caveat.)
    #[test]
    fn host_fn_names_are_all_callable() {
        let ctx = ScriptCtx::new();
        let engine = build_engine(&ctx);
        // Arguments per host fn, chosen to match its registered signature.
        let call = |name: &str| -> String {
            match name {
                "buy" | "sell" => format!("{name}(1.0)"),
                "market" => format!("{name}(1, 1.0)"),
                "limit" => format!("{name}(1, 1.0, 100.0)"),
                "param" => format!(r#"{name}("k", 1.0)"#),
                _ => format!("{name}()"),
            }
        };
        for &name in HOST_FN_NAMES {
            let src = call(name);
            assert!(
                engine.eval::<rhai::Dynamic>(&src).is_ok(),
                "{name} is listed in HOST_FN_NAMES but `{src}` did not resolve"
            );
        }
    }

    /// Guards the one hand-written number in the binding: [`register_indicators`] writes out forms
    /// for arities 1..=[`MAX_INDICATOR_ARITY`], and a registry indicator with more parameters than
    /// that would bind SILENTLY with its extra parameters pinned to their defaults. Fail here
    /// instead, naming the arm to add. This is not hypothetical: `kst` already carries far more
    /// parameters than the cap — it is simply excluded today for an unrelated reason (multi-output).
    #[test]
    fn bound_indicator_arity_never_exceeds_the_registered_forms() {
        for &name in RHAI_INDICATORS.iter() {
            let meta = vike_indicators::get(name).unwrap();
            assert!(
                meta.params.len() <= MAX_INDICATOR_ARITY,
                "{name} takes {} parameters but register_indicators only writes forms up to \
                 MAX_INDICATOR_ARITY={MAX_INDICATOR_ARITY} — add the next arm (and raise the \
                 constant) rather than letting its tail parameters silently take defaults",
                meta.params.len()
            );
        }
    }

    /// Two references to the SAME indicator with DIFFERENT parameters are independent instances,
    /// each fed once per bar — the cache key is `(name, coerced params)`, not the name. Without
    /// this a two-moving-average script (the single most common shape there is) would read one
    /// average twice.
    #[test]
    fn different_parameters_are_independent_instances() {
        let bars = series(80);
        let ctx = ScriptCtx::new();
        let engine = engine_with_indicators(&ctx);
        let fast_ast = engine.compile("sma(5)").unwrap();
        let slow_ast = engine.compile("sma(30)").unwrap();
        let mut fast_oracle = vike_indicators::make_with("sma", &[5.0]).unwrap();
        let mut slow_oracle = vike_indicators::make_with("sma", &[30.0]).unwrap();
        for (i, bar) in bars.iter().enumerate() {
            {
                let mut g = ctx.write().unwrap();
                g.cur_bar = bar.clone();
                g.fed_this_bar.clear();
            }
            let fast: f64 = engine.eval_ast(&fast_ast).unwrap();
            let slow: f64 = engine.eval_ast(&slow_ast).unwrap();
            assert!(same(fast, fast_oracle.on_bar(bar)[0]), "sma(5) diverged at bar {i}");
            assert!(same(slow, slow_oracle.on_bar(bar)[0]), "sma(30) diverged at bar {i}");
        }
    }

    #[test]
    fn param_uses_override_then_default_and_records_seen() {
        let ctx = ScriptCtx::new();
        ctx.write().unwrap().overrides.insert("fast".to_string(), 3.0);
        let engine = build_engine(&ctx);

        // present in overrides -> the override
        let v: f64 = engine.eval(r#"param("fast", 5.0)"#).unwrap();
        assert_eq!(v, 3.0);
        // absent -> the default, and recorded (first-seen) in params_seen
        let d: f64 = engine.eval(r#"param("slow", 20.0)"#).unwrap();
        assert_eq!(d, 20.0);
        let g = ctx.read().unwrap();
        assert_eq!(g.params_seen.get("fast"), Some(&5.0)); // default recorded even when overridden
        assert_eq!(g.params_seen.get("slow"), Some(&20.0));
    }
}

#[cfg(test)]
mod line_accessor_tests {
    use super::*;
    use crate::ctx::ScriptCtx;
    use vike_model::Bar;

    /// Every (indicator, line name, function name, line index) the bridge actually binds.
    ///
    /// Derived from [`line_accessors`] — the same function `register_indicators` and the vike-cli
    /// listing consume — rather than re-filtering the registry here. A local copy of the predicate
    /// would drift the moment a rule changed, and it DID: adding the arity ceiling silently left
    /// this helper claiming `kst_kst` was bound when nothing registered it.
    fn generated() -> Vec<(&'static str, &'static str, String, usize)> {
        vike_indicators::registry()
            .iter()
            .flat_map(|m| {
                line_accessors(m.name)
                    .into_iter()
                    .enumerate()
                    .map(move |(i, (line, fname))| (m.name, line, fname, i))
            })
            .collect()
    }

    /// The sanitiser is lossy in principle, so this proves it is not lossy in fact.
    ///
    /// `%K` and `%D` both sanitise by dropping a character; two different line names collapsing onto
    /// one accessor would give that accessor two meanings, silently. Green across today's registry —
    /// it exists to fail on the future indicator that breaks it, which is the only moment anyone
    /// could act on it.
    #[test]
    fn generated_line_names_are_unambiguous() {
        let mut seen: std::collections::HashMap<String, (&str, &str)> =
            std::collections::HashMap::new();
        for (ind, line, fname, _) in generated() {
            if let Some((pi, pl)) = seen.insert(fname.clone(), (ind, line)) {
                panic!(
                    "`{fname}` is generated by BOTH {pi}'s `{pl}` and {ind}'s `{line}` — one \
                     accessor cannot mean two lines. Rename a line in the registry, or make \
                     `sanitize_line` injective."
                );
            }
        }
    }

    /// Every generated name must be BINDABLE: a legal rhai identifier, not a host function, and not
    /// another indicator's name. `register_indicators` skips one that is not — this says loudly
    /// which, rather than letting a line quietly vanish from the callable set.
    #[test]
    fn generated_line_names_are_bindable() {
        let probe = rhai::Engine::new_raw();
        for (ind, line, fname, _) in generated() {
            assert!(
                probe.compile(format!("{fname}()")).is_ok(),
                "{ind}'s `{line}` generates `{fname}`, which rhai cannot parse as a call"
            );
            assert!(
                !HOST_FN_NAMES.contains(&fname.as_str()),
                "{ind}'s `{line}` generates `{fname}`, which would shadow a host function"
            );
            assert!(
                !vike_indicators::registry().iter().any(|m| m.name == fname),
                "{ind}'s `{line}` generates `{fname}`, which is another indicator's own name"
            );
        }
    }

    /// The `%K`/`%D` family, named. Not redundant with the sweep above: it pins that these specific,
    /// real line names produce these specific, callable spellings — so a change to `sanitize_line`
    /// that still passed the generic sweep but renamed `stochastic_k` would fail here, where a
    /// script author would notice.
    #[test]
    fn the_percent_named_lines_get_the_spelling_users_will_type() {
        assert_eq!(line_fn_name("stochastic", "%K"), "stochastic_k");
        assert_eq!(line_fn_name("stochastic", "%D"), "stochastic_d");
        assert_eq!(line_fn_name("williams", "%R"), "williams_r");
        // ...and an ordinary multi-word line name is untouched.
        assert_eq!(line_fn_name("adx", "plus_di"), "adx_plus_di");
        assert_eq!(line_fn_name("bollinger", "mid"), "bollinger_mid");
    }

    fn line_series(n: usize) -> Vec<Bar> {
        (0..n)
            .map(|i| {
                let f = i as f64;
                let c = 100.0 + (f * 0.7).sin() * 10.0 + f * 0.05;
                Bar {
                    ts: i as i64 * 60_000,
                    open: c - 0.3,
                    high: c + 1.1,
                    low: c - 1.2,
                    close: c,
                    volume: 1_000.0 + f * 3.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some("X".into()),
                }
            })
            .collect()
    }

    /// The core correctness gate: each accessor returns ITS OWN line.
    ///
    /// Folds every generated accessor through the real bridge and compares against a directly-fed
    /// streaming oracle's `on_bar()[line]`, bit-for-bit. An off-by-one in the line index — or the
    /// old `.first()` behaviour surviving anywhere — is precisely a silently-wrong number, and it
    /// would be invisible to a test that only checked the value was finite.
    #[test]
    fn every_line_accessor_returns_its_own_line_bit_for_bit() {
        let bars = line_series(120);
        for (ind, line, fname, idx) in generated() {
            let meta =
                vike_indicators::registry().iter().find(|m| m.name == ind).expect("registry entry");
            let ctx = ScriptCtx::new();
            let mut engine = rhai::Engine::new();
            register_indicators(&mut engine, &ctx);

            let mut oracle = meta.build_with(&vike_indicators::coerce(meta.params, &[]));
            let ast = engine.compile(format!("{fname}()")).expect("compiles");

            for (i, b) in bars.iter().enumerate() {
                {
                    let mut g = ctx.write().unwrap();
                    g.cur_bar = b.clone();
                    g.fed_this_bar.clear();
                }
                let got: f64 = engine.eval_ast(&ast).expect("evaluates");
                let want = oracle.on_bar(b).get(idx).copied().unwrap_or(f64::NAN);
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "{fname} (= {ind} line {idx} `{line}`) diverged at bar {i}: {got} != {want}"
                );
            }
        }
    }

    /// All lines of one indicator share ONE instance, fed ONCE per bar.
    ///
    /// Three `bollinger_*` reads in a bar must advance bollinger once. Non-vacuous by construction:
    /// the oracle is fed exactly once per bar too, so if the bridge fed per-accessor the three reads
    /// would come from bars 1, 2 and 3 of a triple-advanced instance and none would match.
    #[test]
    fn reading_three_lines_in_one_bar_feeds_the_indicator_once() {
        let bars = line_series(60);
        let meta = vike_indicators::registry().iter().find(|m| m.name == "bollinger").unwrap();
        let ctx = ScriptCtx::new();
        let mut engine = rhai::Engine::new();
        register_indicators(&mut engine, &ctx);
        let ast = engine
            .compile("[bollinger_upper(20), bollinger_mid(20), bollinger_lower(20)]")
            .expect("compiles");
        let mut oracle = meta.build_with(&vike_indicators::coerce(meta.params, &[20.0]));

        for (i, b) in bars.iter().enumerate() {
            {
                let mut g = ctx.write().unwrap();
                g.cur_bar = b.clone();
                g.fed_this_bar.clear();
            }
            let got: rhai::Array = engine.eval_ast(&ast).expect("evaluates");
            let want = oracle.on_bar(b);
            for (l, w) in want.iter().enumerate() {
                let g = got[l].as_float().unwrap();
                assert_eq!(g.to_bits(), w.to_bits(), "bar {i} line {l}: {g} != {w}");
            }
            // ...and one cache slot, not three.
            assert_eq!(
                ctx.read().unwrap().fed_this_bar.len(),
                1,
                "bar {i}: three lines must share ONE fed_this_bar slot"
            );
        }
    }

    /// The band indicators keep their BARE name refused — the hazard that motivated the original
    /// rule is untouched by lifting it for the namesake case.
    #[test]
    fn a_band_indicator_still_has_no_bare_name_but_now_has_its_lines() {
        for band in ["bollinger", "donchian", "keltner", "envelopes", "std_error_bands"] {
            let why = unbound_reason(band);
            assert!(why.is_some(), "{band}'s bare name must stay refused: line 0 is `upper`");
            assert!(
                why.unwrap().contains("_<line>"),
                "{band}'s refusal must point at the accessor that DOES work: {why:?}"
            );
        }
        let ctx = ScriptCtx::new();
        let mut engine = rhai::Engine::new();
        register_indicators(&mut engine, &ctx);
        ctx.write().unwrap().cur_bar = line_series(1)[0].clone();
        assert!(engine.eval::<f64>("bollinger_mid(20)").is_ok(), "the accessor must resolve");
        assert!(engine.eval::<f64>("bollinger(20)").is_err(), "the bare name must not resolve");
    }

    /// The namesake indicators gain their bare name, and it is line 0 — the same value the
    /// `<name>_<name>` accessor returns. If those disagreed, one spelling would be lying.
    #[test]
    fn a_namesake_indicator_binds_its_bare_name_to_the_same_value_as_its_own_line() {
        let bars = line_series(80);
        let mut checked = 0;
        for m in
            vike_indicators::registry().iter().filter(|m| m.outputs.len() >= 2 && !m.batch_only)
        {
            if line_fn_name(m.name, m.outputs[0].name) != format!("{}_{}", m.name, m.name) {
                continue; // not a namesake indicator
            }
            // ...and a namesake can still be refused for an UNRELATED rule — `kst` is one, at 9
            // parameters. The rules compose, so this test asserts the namesake rule only where it
            // is the deciding one.
            if line_accessors(m.name).is_empty() {
                continue;
            }
            assert!(unbound_reason(m.name).is_none(), "{} must now bind its bare name", m.name);
            let own = line_fn_name(m.name, m.outputs[0].name);

            let ctx = ScriptCtx::new();
            let mut engine = rhai::Engine::new();
            register_indicators(&mut engine, &ctx);
            let ast = engine.compile(format!("[{}(), {own}()]", m.name)).expect("compiles");
            for b in &bars {
                {
                    let mut g = ctx.write().unwrap();
                    g.cur_bar = b.clone();
                    g.fed_this_bar.clear();
                }
                let got: rhai::Array = engine.eval_ast(&ast).expect("evaluates");
                let (a, c) = (got[0].as_float().unwrap(), got[1].as_float().unwrap());
                assert_eq!(a.to_bits(), c.to_bits(), "{}() != {own}()", m.name);
            }
            checked += 1;
        }
        assert!(checked >= 4, "expected several namesake indicators, found {checked}");
    }

    /// ⚠ ONE namesake rule, asked from two sides. [`bare_name_binds`] is what [`exclusion`]'s rule
    /// 4 consults about a REGISTRY entry and what [`user_bare_name_binds`] consults about a user
    /// file, and it sanitises the INDICATOR name — which the registry side never needed, because
    /// every registry name is already a bare lowercase identifier while a file stem is whatever the
    /// filesystem allowed.
    ///
    /// This replaced a test that asserted the sanitisation premise ALONE. That one passed with the
    /// whole user-indicator change reverted — it touched nothing the change introduced — and its
    /// stated justification was false besides: it claimed the two spellings would "disagree about a
    /// BUILT-IN", which they could not, because the user-side spelling was never asked about one.
    /// Sharing the function is what makes the premise load-bearing, and this is what gates it.
    ///
    /// Non-vacuous in TWO directions, and the middle assertion is honestly a third thing: the
    /// identity assertion fails the day a registry entry takes a capital, a dash or a `%` (which is
    /// when sharing the rule would start CHANGING a built-in's verdict); and the counts fail if the
    /// predicate ever answers one way for everything.
    ///
    /// ⚠ The AGREEMENT assertion cannot fail today, and saying so is the point.
    /// `user_bare_name_binds(name, lines)` is defined as
    /// `bare_name_binds(name, lines.len(), lines.first())`, and this test feeds both from the same
    /// `m.outputs` — so it compares one call against itself. It is a DELEGATION-DRIFT guard: it
    /// bites only if somebody later re-implements the user-side spelling independently, which is
    /// exactly the moment "one rule, two callers" would stop being true. An earlier draft of this
    /// comment claimed it "fails if either spelling drifts from the other", which overstated what
    /// the code can do.
    #[test]
    fn one_namesake_rule_serves_the_registry_and_the_user_files() {
        let (mut binds, mut refused) = (0usize, 0usize);
        for m in vike_indicators::registry() {
            assert_eq!(
                sanitize_line(m.name),
                m.name,
                "`{}` does not survive sanitisation, so sharing the namesake rule with the user \
                 side would CHANGE whether its bare name binds",
                m.name
            );
            let shared =
                bare_name_binds(m.name, m.outputs.len(), m.outputs.first().map(|o| o.name));
            let lines: Vec<String> = m.outputs.iter().map(|o| o.name.to_string()).collect();
            assert_eq!(
                shared,
                user_bare_name_binds(m.name, &lines),
                "the two sides must be ONE rule, and they disagree about `{}`",
                m.name
            );
            if shared {
                binds += 1;
            } else {
                refused += 1;
            }
        }
        assert!(
            binds > 0 && refused > 0,
            "a predicate that answered one way for the whole registry would prove nothing: \
             {binds} bind, {refused} refused"
        );
    }

    /// `ichimoku` and `williams_fractal` are multi-output AND `batch_only`. Lifting the multi-output
    /// refusal must NOT lift the future-reading one — an `ichimoku_tenkan()` would hand back a value
    /// the batch path later revises, which is the whole reason rule 3 exists.
    #[test]
    fn a_future_reading_indicator_gets_no_line_accessors_either() {
        let ctx = ScriptCtx::new();
        let mut engine = rhai::Engine::new();
        register_indicators(&mut engine, &ctx);
        ctx.write().unwrap().cur_bar = line_series(1)[0].clone();
        let mut checked = 0;
        for m in vike_indicators::registry().iter().filter(|m| m.batch_only && m.outputs.len() >= 2)
        {
            assert!(line_accessors(m.name).is_empty(), "{} must expose no accessors", m.name);
            for o in m.outputs {
                let f = line_fn_name(m.name, o.name);
                assert!(
                    engine.eval::<f64>(&format!("{f}()")).is_err(),
                    "{f} must NOT resolve: {} reads future bars",
                    m.name
                );
                checked += 1;
            }
        }
        assert!(checked >= 2, "expected ichimoku + williams_fractal lines, checked {checked}");
    }
}

/// The USER side of multi-output: `fn outputs()` -> per-line accessors, through the same
/// [`line_fn_name`] and the same namesake rule the built-ins get.
///
/// These live beside the engine rather than in `tests/` because the property that matters most —
/// that a LINE never enters the streaming cache key — is only observable from inside `ScriptCtx`,
/// and a value-level test cannot tell "one instance read three times" from "three instances each
/// fed once" (they agree, bar for bar, on any deterministic recurrence).
#[cfg(test)]
mod user_output_tests {
    use super::*;
    use crate::ctx::ScriptCtx;
    use vike_model::Bar;

    /// Three lines off ONE counter, so every value is a direct measurement of how many times the
    /// file's `on_bar` ran: bar k reads `[k, 10k, 100k]` when fed once.
    const COUNTER_BANDS: &str = r#"
        fn outputs() { ["a", "b", "c"] }
        fn init() { #{ n: 0 } }
        fn on_bar(bar) { this.n += 1; [this.n, this.n * 10, this.n * 100] }
    "#;

    /// A band shape: line 0 is NOT the namesake, so the bare name must be refused.
    const BANDS: &str = r#"
        let width = param("width", 2.0);
        fn outputs() { ["upper", "mid", "lower"] }
        fn on_bar(bar) { [bar.close + width, bar.close, bar.close - width] }
    "#;

    fn bar(c: f64) -> Bar {
        Bar {
            ts: 0,
            open: c,
            high: c,
            low: c,
            close: c,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some("X".into()),
        }
    }

    /// An engine with ONLY the user set on it, plus the fresh ctx to inspect.
    fn user_engine(inds: &[crate::RhaiIndicator]) -> (SharedCtx, rhai::Engine) {
        let ctx = ScriptCtx::new();
        let mut engine = rhai::Engine::new();
        register_user_indicators(&mut engine, &ctx, inds);
        (ctx, engine)
    }

    /// ⚠ **The cache-key gate.** All the lines of one user indicator share ONE streaming instance,
    /// fed ONCE per bar — the property `reading_three_lines_in_one_bar_feeds_the_indicator_once`
    /// pins for the built-ins, and the reason `register_user_form` keys on the indicator name plus
    /// the arguments while the LINE stays captured in the closure.
    ///
    /// Non-vacuous against BOTH regressions, which a value-only test cannot separate:
    ///   - a line IN the key -> three instances, each fed once. The three VALUES would be
    ///     identical (`[1, 10, 100]`), so only `user_indicators.len()` catches it — asserted.
    ///   - a feed per accessor call -> one instance advanced three times, so the reads would be
    ///     `[1, 20, 300]` — caught by the values, and by `fed_this_bar.len()`.
    #[test]
    fn every_line_of_one_user_indicator_shares_one_instance_fed_once_per_bar() {
        let ind = crate::compile_indicator("cb", COUNTER_BANDS).unwrap();
        let (ctx, engine) = user_engine(std::slice::from_ref(&ind));
        let ast = engine.compile("[cb_a(), cb_b(), cb_c()]").expect("all three accessors resolve");

        for k in 1..=3usize {
            {
                let mut g = ctx.write().unwrap();
                g.cur_bar = bar(1.0);
                g.fed_this_bar.clear();
            }
            let got: rhai::Array = engine.eval_ast(&ast).expect("evaluates");
            let vals: Vec<f64> = got.iter().map(|d| d.as_float().unwrap()).collect();
            let k = k as f64;
            assert_eq!(
                vals,
                vec![k, k * 10.0, k * 100.0],
                "bar {k}: three reads of one instance fed once — not one instance fed three times"
            );
            let g = ctx.read().unwrap();
            assert_eq!(
                g.user_indicators.len(),
                1,
                "three lines must be ONE cached instance; a line in the key would make it 3"
            );
            assert_eq!(g.fed_this_bar.len(), 1, "...and ONE fed_this_bar slot");
        }
    }

    /// The namesake rule, applied to a user file: `my_bands()` would hand back `upper` to somebody
    /// who read it as the middle — the exact refusal `bollinger` carries — so the bare name does not
    /// bind, while every line does.
    #[test]
    fn a_user_band_indicator_has_no_bare_name_but_all_of_its_lines() {
        let ind = crate::compile_indicator("my_bands", BANDS).unwrap();
        assert!(!user_bare_name_binds("my_bands", ind.outputs()));
        assert_eq!(
            user_line_accessors(&ind),
            vec![
                ("upper".to_string(), "my_bands_upper".to_string()),
                ("mid".to_string(), "my_bands_mid".to_string()),
                ("lower".to_string(), "my_bands_lower".to_string()),
            ]
        );

        let (ctx, engine) = user_engine(&[ind]);
        ctx.write().unwrap().cur_bar = bar(100.0);
        assert!(
            engine.eval::<f64>("my_bands()").is_err(),
            "the bare name must NOT resolve — it would be the upper band"
        );
        assert_eq!(engine.eval::<f64>("my_bands_mid()").unwrap(), 100.0);
        // ...and each accessor is genuinely its OWN line, not three spellings of line 0.
        ctx.write().unwrap().fed_this_bar.clear();
        assert_eq!(engine.eval::<f64>("my_bands_upper()").unwrap(), 102.0);
        assert_eq!(engine.eval::<f64>("my_bands_lower()").unwrap(), 98.0);
        // ...and the knob reaches a LINE accessor, so its ladder is the bare name's, not a stub.
        ctx.write().unwrap().fed_this_bar.clear();
        assert_eq!(engine.eval::<f64>("my_bands_upper(5)").unwrap(), 105.0);
    }

    /// The other half of the namesake rule: line 0 named after the file, so the bare name binds and
    /// IS line 0 — `macd()` on the user side. Without this the rule would read as "multi-output
    /// means no bare name", which is the over-broad version `exclusion` already narrowed away from.
    #[test]
    fn a_namesake_user_indicator_binds_its_bare_name_to_line_zero() {
        let src = r#"fn outputs() { ["trend", "signal"] }
                     fn on_bar(bar) { [bar.close, bar.close * 2.0] }"#;
        let ind = crate::compile_indicator("trend", src).unwrap();
        assert!(user_bare_name_binds("trend", ind.outputs()));
        let (ctx, engine) = user_engine(&[ind]);
        ctx.write().unwrap().cur_bar = bar(7.0);
        assert_eq!(engine.eval::<f64>("trend()").unwrap(), 7.0, "the bare name is line 0");
        ctx.write().unwrap().fed_this_bar.clear();
        assert_eq!(engine.eval::<f64>("trend_signal()").unwrap(), 14.0);
    }

    /// ⚠ A user file's name is whatever the filesystem allowed — `Shouty.RHAI` loads today — while
    /// every registry name is a bare lowercase identifier. Comparing the sanitised LINE against the
    /// RAW name would refuse this file's bare name for its capital letter, which has nothing to do
    /// with the hazard the rule exists for.
    #[test]
    fn a_capitalised_user_name_still_matches_its_own_namesake_line() {
        let lines = vec!["Shouty".to_string(), "other".to_string()];
        assert!(user_bare_name_binds("Shouty", &lines));
        // ...and the rule still bites when line 0 is genuinely a different line.
        assert!(!user_bare_name_binds("Shouty", &["upper".to_string(), "Shouty".to_string()]));
    }

    /// ⚠ A pre-existing hole this feature would have widened: a per-line accessor is a FUNCTION
    /// name, not a registry entry, so `registry().iter().any(|m| m.name == n)` never saw it — and
    /// `build_engine` registers the user set AFTER the built-ins, where `register_fn` REPLACES. A
    /// file called `stochastic_k.rhai` therefore answered every `stochastic_k()` in every strategy.
    ///
    /// Non-vacuous: the witness is DERIVED from `line_accessors` rather than spelled here, and the
    /// last assertion pins that a name which merely LOOKS like one is still free — so this cannot
    /// pass by refusing everything with an underscore in it.
    #[test]
    fn a_user_file_may_not_take_a_builtin_line_accessors_name() {
        let (_, taken) =
            line_accessors("bollinger").into_iter().next().expect("bollinger has lines");
        let why = user_indicator_conflict(&taken)
            .unwrap_or_else(|| panic!("`{taken}` is a built-in accessor and must be refused"));
        assert!(why.contains("per-line accessor"), "{why}");
        assert!(
            vike_indicators::get(&taken).is_none(),
            "test premise: `{taken}` is NOT a registry entry, so the older rule could not see it"
        );
        assert!(
            user_indicator_conflict("bollinger_not_a_line").is_none(),
            "an ordinary underscore name must stay free"
        );
    }

    /// Every [`user_line_conflict`] rule, asked with a witness that actually REACHES it — and, for
    /// the one rule that cannot be reached, the fact that makes it unreachable, asserted rather
    /// than assumed. A rule provable only by the data it happens to receive is a rule nobody can
    /// trust, which is the same argument `every_exclusion_rule_is_reachable` makes.
    ///
    /// ⚠ **The trap this test was written into once.** [`line_fn_name`] joins the two halves with a
    /// literal `_`, so `("pos", "ition")` spells `pos_ition` — NOT the host read `position` — and
    /// `("sm", "a")` spells `sm_a`, not `sma`. Both hand-written witnesses collided with nothing at
    /// all, and the test failed. Only a name CONTAINING a `_` is reachable, which is why the two
    /// witnesses below are derived from the registry instead of spelled here.
    #[test]
    fn every_user_line_rule_is_reachable_or_provably_not() {
        // 1. The line sanitises to nothing, so it spells no accessor at all.
        assert!(user_line_conflict("x", "%%").is_some());

        // 2. The accessor does not PARSE. A line cannot cause this on its own — its half is
        //    sanitised down to `[a-z0-9_]` — but the indicator half is a file stem, unsanitised,
        //    and a stem is whatever the filesystem allowed. `a)b_c()` is two tokens and a stray
        //    paren, under any reading of the grammar.
        assert!(user_line_conflict("a)b", "c").is_some(), "`a)b_c()` does not parse");

        // 3. The accessor is a host read/verb — UNREACHABLE, and provably so rather than skipped:
        //    the generated name always carries a `_` and no host name does. The rule stays because
        //    that is a property of the separator and of this list, either of which can change, and
        //    this assertion is what would then send its author here to write the real witness.
        assert!(
            HOST_FN_NAMES.iter().all(|n| !n.contains('_')),
            "a host name with a `_` in it makes the host-read rule reachable — replace this \
             assertion with a witness that trips it"
        );

        // 4. The accessor is a built-in indicator's OWN name.
        let (stem, line) = registry_name_a_user_line_could_spell();
        let why = user_line_conflict(stem, line)
            .unwrap_or_else(|| panic!("`{stem}` + `{line}` spells a built-in's name: must refuse"));
        assert!(why.contains(&line_fn_name(stem, line)) && why.contains("own name"), "{why}");

        // 5. The accessor is a built-in's per-line accessor. A DIFFERENT rule from 4 — the name it
        //    would take is a function the bridge registers, not a registry entry — so the message
        //    is asserted, not merely the refusal.
        let (ind, l, taken) = a_builtin_line_accessor();
        let why = user_line_conflict(ind, l)
            .unwrap_or_else(|| panic!("`{taken}` is a built-in accessor: must refuse"));
        assert!(why.contains(&taken) && why.contains("per-line accessor"), "{why}");

        // ...and an ordinary line on an ordinary indicator is free, so none of the above passes by
        // refusing everything with an underscore in it.
        assert!(user_line_conflict("my_bands", "mid").is_none());
    }

    /// ⚠ `fn outputs()` runs after the top level, so it CAN read a `param()` knob — and the
    /// accessors were already registered from the prototype's list. Reading a line this call site's
    /// instance does not have would be NaN forever with no fault, which is precisely the
    /// warm-up-shaped silence this whole seam refuses. So it raises, naming both lists.
    ///
    /// Non-vacuous: the SAME accessor at the default argument works two lines below, so the test
    /// measures the knob-dependence and not a broken registration.
    #[test]
    fn outputs_that_depend_on_a_knob_raise_rather_than_reading_a_missing_line() {
        let src = r#"
            let n = param("n", 3.0);
            fn outputs() { if n > 2.0 { ["a", "b", "c"] } else { ["a", "b"] } }
            fn on_bar(bar) { if n > 2.0 { [1.0, 2.0, 3.0] } else { [1.0, 2.0] } }
        "#;
        let ind = crate::compile_indicator("wobbly", src).unwrap();
        assert_eq!(ind.outputs().len(), 3, "the prototype declares three at its default");
        let (ctx, engine) = user_engine(&[ind]);
        ctx.write().unwrap().cur_bar = bar(1.0);

        let err = engine
            .eval::<f64>("wobbly_c(1)")
            .expect_err("a knob that changes the line list must not silently drop line 2");
        let msg = err.to_string();
        assert!(msg.contains("must not depend on a `param()` knob"), "{msg}");
        // BOTH lists, in their own brackets. `contains("a, b")` alone would be satisfied by the
        // three-line list on its own — an assertion that cannot tell the two apart is not one.
        assert!(msg.contains("[a, b, c]"), "the registered list: {msg}");
        assert!(msg.contains("[a, b]"), "...and the one this call site declares: {msg}");

        ctx.write().unwrap().fed_this_bar.clear();
        assert_eq!(
            engine.eval::<f64>("wobbly_c()").unwrap(),
            3.0,
            "the default instance declares the list the accessors were built from, and works"
        );
    }
}
