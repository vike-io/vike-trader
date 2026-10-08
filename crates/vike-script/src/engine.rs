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
///   `crates/vike-cli/src/cmd/mcp/offline_tools.rs`'s `tool_validate_strategy` reaches
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
/// terminal. That is the hazard `VIKE_LOG_FILE_LEVEL` exists for (`crates/vike-log/src/lib.rs`'s
/// `file_level_directive` carries the measurements, one of them a run that nearly filled a live
/// trading node's disk), and
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
///
/// ⚠ **It is `pub` because a SHELL now advertises it, and the sentence above is why that is safe.**
/// `crates/vike-cli/src/cmd/strategies.rs`'s `HOST_FNS` is one describing row per name here, and its
/// own completeness gate holds the two sets equal in both directions — so the human listing follows
/// the BINDING rather than a second list somebody keeps in step by hand. That matters on a RELEASE
/// install for the same reason `vike-cli indicators` exists: there is no source tree, so "read
/// `register_reads`" names a file the user does not have, and a roster typed into prose
/// over-advertises the moment a verb is added or withdrawn. A script calling a name the host does
/// not bind raises every bar and self-disables, which is the silent failure an over-advertising
/// roster causes.
pub const HOST_FN_NAMES: &[&str] = &[
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
/// (`crates/vike-script/src/templates.rs`'s `SMA_CROSS` calls `sma(fast.to_int())`).
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
/// the filesystem allowed (`Shouty.RHAI` loads today — `crates/vike-script/src/load_tests.rs`'s
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
/// The alternative is threading `&[RhaiIndicator]` through `run_backtest`, `run_paramscan`,
/// `run_paramscan_with`, `run_walkforward`, `run_paramscan_euler`, the Studio's `build_strategy` funnel and
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
/// which is the exact defect `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `CREDENTIAL_STORE_PIN`
/// ratchets down. So this takes ALREADY-COMPILED prototypes: the caller owns the directory
/// resolution and the file I/O (`crates/vike-script/src/load.rs`'s `load_user_indicators` +
/// `vike_model::paths::state_path::user_indicators_dir`), and this function performs none.
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

#[path = "engine_tests.rs"]
#[cfg(test)]
mod engine_tests;

#[path = "line_accessor_tests.rs"]
#[cfg(test)]
mod line_accessor_tests;

/// The USER side of multi-output: `fn outputs()` -> per-line accessors, through the same
/// [`line_fn_name`] and the same namesake rule the built-ins get.
///
/// These live beside the engine rather than in `tests/` because the property that matters most —
/// that a LINE never enters the streaming cache key — is only observable from inside `ScriptCtx`,
/// and a value-level test cannot tell "one instance read three times" from "three instances each
/// fed once" (they agree, bar for bar, on any deterministic recurrence).
#[path = "user_output_tests.rs"]
#[cfg(test)]
mod user_output_tests;
