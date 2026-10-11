//! The research tools: strategy authoring, the indicator roster, and the backtest run tools.

// The EXECUTION gate on the shipped templates (see `every_shipped_template_reaches_the_broker`):
// the real `RhaiStrategy` mounted over vike-model's shared `MockBroker` (its `test-support`
// feature, already a dev-dep of this crate for `tests/init_cli.rs`) and driven over real bars.
use vike_model::strategy::MockBroker;
use vike_model::{Bar, Strategy};
use vike_script::RhaiStrategy;

use super::*;
use crate::cmd::mcp::backtest_tools::search_from_args;

/// A function name NOTHING binds — the specimen the non-vacuity proof below rewrites a template
/// to call. Typo-shaped on purpose: the failure it stands for is a misspelling, and the two
/// assertions at the call site prove it is unbound rather than trusting this comment.
const UNBOUND_WITNESS: &str = "sma_typo";

/// THE §15.1 ONE-ROSTER GATE'S AGENT LEG. A model can only ask for a method the SCHEMA names,
/// so a roster that does not reach `tools_spec` leaves every agent a grid user — which is the
/// exact consequence §12 of the backtest-CLI-surface design says this stage exists to end.
#[test]
fn the_run_sweep_schema_advertises_the_whole_method_roster() {
    let spec = tools_spec();
    let tool = spec
        .as_array()
        .expect("tools_spec is an array")
        .iter()
        .find(|t| t["name"] == "run_sweep")
        .expect("run_sweep is served");
    let enumerated =
        tool["inputSchema"]["properties"]["optimizer"]["enum"].as_array().expect("an enum");
    let names: Vec<&str> = enumerated.iter().map(|v| v.as_str().expect("a string")).collect();
    assert_eq!(
        names,
        vike_datahub_client::SEARCH_METHODS.to_vec(),
        "the tool schema and the protocol must name one roster"
    );
    for knob in ["trials", "seed", "euler_depth"] {
        assert!(
            tool["inputSchema"]["properties"][knob].is_object(),
            "the agent must be able to pass {knob}"
        );
    }
}

/// A numeric argument arrives from a model as a JSON NUMBER, and the wire carries TOKENS — so
/// the boundary renders it. A string is accepted too (a model that quoted it is not wrong), and
/// anything else is refused BY NAME rather than silently dropped, which is what
/// `Value::as_str` alone would have done.
#[test]
fn a_numeric_tool_argument_is_rendered_and_a_bad_one_is_named() {
    let search = search_from_args(&json!({ "trials": 128, "optimizer": "tpe" }))
        .expect("a number is accepted")
        .expect("a selector was built");
    assert_eq!(search.trials.as_deref(), Some("128"));
    assert_eq!(search.optimizer.as_deref(), Some("tpe"));

    let quoted = search_from_args(&json!({ "seed": "7" })).expect("a string is accepted too");
    assert_eq!(quoted.expect("a selector").seed.as_deref(), Some("7"));

    let err = search_from_args(&json!({ "trials": [1, 2] })).expect_err("a list is refused");
    assert!(err.contains("trials"), "the refusal names the argument: {err}");
}

/// No search argument at all builds NO selector, so an ordinary agent grid search ships the
/// frame it always shipped — and reaches a daemon that predates the capability unchanged.
#[test]
fn a_sweep_with_no_method_argument_builds_no_selector() {
    assert!(
        search_from_args(&json!({ "profile": "[data]\n" })).expect("ok").is_none(),
        "an ordinary grid search must not start negotiating a capability"
    );
}

#[test]
fn discover_params_tool_returns_the_declared_knobs() {
    let resp = call(
        "discover_params",
        json!({ "script": "let qty = param(\"qty\", 2.5);\nfn on_bar() {}" }),
    );
    assert_eq!(resp["result"]["isError"], false);
    let params = resp["result"]["structuredContent"]["params"].as_array().unwrap();
    assert_eq!(params[0]["name"], "qty");
    assert_eq!(params[0]["default"], 2.5);
}

#[test]
fn list_indicators_returns_exactly_the_host_bound_set() {
    // The tool must advertise ONLY what the Rhai host binds (vike_script::RHAI_INDICATORS),
    // never the whole `vike_indicators::registry()` — an agent acts on this list, and any
    // unbound name it is handed produces a script that silently self-disables. Both sides read
    // the SAME derived list, so this cannot pass while the tool under-reports either.
    let resp = call("list_indicators", json!({}));
    assert_eq!(resp["result"]["isError"], false);
    let inds = resp["result"]["structuredContent"]["indicators"].as_array().unwrap();
    let mut names: Vec<&str> = inds.iter().map(|i| i["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    // The union, for the reason spelled out on `is_callable`: an indicator reachable only
    // through `bollinger_mid(20)` is still an indicator this tool must offer.
    let mut expected: Vec<&str> = vike_indicators::registry()
        .iter()
        .map(|m| m.name)
        .filter(|n| vike_script::is_callable(n))
        .collect();
    expected.sort_unstable();
    assert_eq!(names, expected, "list_indicators must be the Rhai host-bound set");
    assert_eq!(resp["result"]["structuredContent"]["count"], names.len());
    // ⚠ The DEFAULT response stays compact: name + category, and no per-indicator detail. The
    // roster is the whole catalog now, so a default that carried every parameter and output
    // line would spend an agent's context on indicators it never asked about.
    for i in inds {
        assert!(i["category"].as_str().is_some_and(|s| !s.is_empty()), "{i}");
        assert!(i.get("params").is_none(), "the default roster must stay compact: {i}");
    }
}

/// Detail is PULLED, per name or per family — the other half of the tiering above.
#[test]
fn list_indicators_narrows_by_name_and_by_category() {
    // Derived: whatever the host binds first, never a hard-coded name.
    let first = vike_script::RHAI_INDICATORS.first().expect("the host binds something");
    let one = call("list_indicators", json!({ "name": first }));
    assert_eq!(one["result"]["isError"], false);
    let rows = one["result"]["structuredContent"]["indicators"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], *first);
    // The two fields a compact roster CANNOT carry, and the reason detail exists at all: a
    // multi-parameter, multi-output indicator is indistinguishable from a simple one without
    // them.
    assert!(rows[0]["params"].is_array(), "a detail row must carry its parameters");
    assert!(rows[0]["outputs"].as_array().is_some_and(|o| !o.is_empty()));
    let category = rows[0]["category"].as_str().unwrap().to_string();

    let family = call("list_indicators", json!({ "category": category.to_lowercase() }));
    assert_eq!(family["result"]["isError"], false, "the category match is case-insensitive");
    let fam = family["result"]["structuredContent"]["indicators"].as_array().unwrap();
    assert!(fam.iter().any(|r| r["name"] == *first));
    assert!(fam.iter().all(|r| r["category"] == category.as_str()));

    // A name nobody can call is an ERROR, not an empty list: an empty answer reads as "that
    // family is empty in this build", which sends somebody hunting through a config.
    let miss = call("list_indicators", json!({ "name": "sma_typo" }));
    assert_eq!(miss["result"]["isError"], true);
    let bad = call("list_indicators", json!({ "category": "not-a-category" }));
    assert_eq!(bad["result"]["isError"], true);
    assert!(bad["result"]["content"][0]["text"].as_str().unwrap().contains(&category));
}

/// A registry indicator the host HOLDS BACK answers with the host's own REASON. An agent that
/// reached for one — they are real indicator names, and a model has seen them — otherwise gets
/// "unknown", re-checks its spelling, and tries again with the same name.
///
/// Derived on every axis: which name is held back, and what the reason says, both come from
/// `vike-script`. A build binding the entire registry has nothing to explain and skips — stated
/// rather than silent, because a vacuous pass should be readable in the test, not inferred.
#[test]
fn list_indicators_says_why_a_registry_name_is_not_callable() {
    // ⚠ NOT `!RHAI_INDICATORS.contains(..)` any more. That set is the BARE names, and since
    // per-line accessors landed a name can be absent from it and still perfectly callable —
    // `bollinger` is. A genuinely held-back indicator is one no spelling reaches, which is
    // exactly what `is_callable` answers.
    let held = vike_indicators::registry().iter().find(|m| !vike_script::is_callable(m.name));
    let Some(held) = held else {
        return; // nothing is held back in this build
    };
    let resp = call("list_indicators", json!({ "name": held.name }));
    assert_eq!(resp["result"]["isError"], true, "a name a script cannot call is not an answer");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let why = vike_script::unbound_reason(held.name)
        .expect("a held-back registry indicator must carry a reason");
    assert!(text.contains(why), "the tool must quote the host's own reason: {text}");
}

/// ⚠ The `tools/list` description ships in EVERY session, whether the tool is called or not, so
/// it must NOT enumerate the roster. It used to — `RHAI_INDICATORS.join(", ")` was spliced in,
/// which cost 13 characters while the host bound three names and would cost kilobytes of every
/// agent's context now that it binds the catalog.
#[test]
fn list_indicators_description_does_not_enumerate_the_set() {
    let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    let tool = tools.iter().find(|t| t["name"] == "list_indicators").unwrap();
    let desc = tool["description"].as_str().unwrap();
    assert!(desc.contains("HOST-BOUND"), "{desc}");
    assert!(
        !desc.contains(&vike_script::RHAI_INDICATORS.join(", ")),
        "the description must not carry the roster: {desc}"
    );
    // A FIXED ceiling, deliberately independent of the set: the point is that this text cannot
    // grow when an indicator is added.
    const MAX_DESCRIPTION: usize = 700;
    assert!(
        desc.len() <= MAX_DESCRIPTION,
        "the tools/list description is {} bytes — it ships in every session and must stay \
             bounded; describe the tool, do not list its answer",
        desc.len()
    );
    // ...and it must tell the agent how to GET the set, or bounding it just hid the answer.
    assert!(desc.contains("category") && desc.contains("name"), "{desc}");
}

#[test]
fn list_strategies_is_advertised_read_only() {
    let resp = server().handle(&req(3, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|t| t["name"] == "list_strategies")
        .expect("list_strategies must be advertised in tools/list");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
}

#[test]
fn list_strategies_without_a_reachable_datahub_is_a_clean_error() {
    // Point at a port nothing is listening on: the connect must fail into a clean tool error,
    // never a panic. (`127.0.0.1:1` refuses immediately.)
    let mut s = Server { datahub_addr: "127.0.0.1:1".to_string(), ..test_server() };
    let resp = s
        .handle(&req(1, "tools/call", json!({ "name": "list_strategies", "arguments": {} })))
        .unwrap();
    assert_eq!(resp["result"]["isError"], true, "an unreachable datahub must error, not panic");
}

// ---- the absorbed vike-mcp tool surface (Phase A) ----------------------------------------

#[test]
fn validate_strategy_answers_ok_and_compile_errors_offline() {
    // A good script is an `ok:true` ANSWER; a bad script is an `ok:false` ANSWER carrying the
    // compile error — NOT an isError tool failure (the agent reads the error and fixes the
    // script). Mirrors vike-mcp's validate_strategy semantics; the argument is `script` here
    // (vike-mcp said `code`), aligned with this file's discover_params.
    let good = call(
        "validate_strategy",
        json!({ "script": "let fast = param(\"fast\", 5.0);\nfn on_bar() {}" }),
    );
    assert_eq!(good["result"]["isError"], false);
    assert_eq!(good["result"]["structuredContent"]["ok"], true);

    let bad = call("validate_strategy", json!({ "script": "fn on_bar( {" }));
    assert_eq!(bad["result"]["isError"], false, "a compile error is an ANSWER, not a failure");
    let sc = &bad["result"]["structuredContent"];
    assert_eq!(sc["ok"], false);
    assert!(!sc["error"].as_str().unwrap().is_empty());
}

#[test]
fn validate_strategy_missing_script_arg_is_a_tool_error() {
    let resp = call("validate_strategy", json!({}));
    assert_eq!(resp["result"]["isError"], true);
    assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("script"));
}

#[test]
fn list_templates_returns_named_parameterized_sources_that_compile() {
    let resp = call("list_templates", json!({}));
    assert_eq!(resp["result"]["isError"], false);
    let ts = resp["result"]["structuredContent"]["templates"].as_array().unwrap();
    assert!(ts.iter().any(|t| t["name"] == "SMA cross"));
    // Every advertised template must actually compile against THIS build's Rhai host and
    // expose param() knobs (so it drops straight into run_sweep) — the same pin
    // vike-studio-core keeps on its own copy of these sources.
    //
    // ⚠ Compiling is NOT the property that matters most, and this test cannot see it:
    // `discover_params` runs the script's TOP LEVEL only, and every template's real work sits
    // inside `fn on_bar()`. `every_shipped_template_reaches_the_broker` below is the gate that
    // covers what this one structurally cannot.
    for t in ts {
        let (name, code) = (t["name"].as_str().unwrap(), t["code"].as_str().unwrap());
        let params = vike_script::discover_params(code)
            .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
        assert!(!params.is_empty(), "template {name} must expose param()s");
        assert!(code.contains("on_bar"), "template {name} must define on_bar");
    }
}

/// A deterministic strictly-RISING bar series. Monotone closes are enough to drive every
/// shipped template past its warm-up and into a decision: `sma(5)` rises above `sma(20)`,
/// `rsi(14)` climbs past the reversion band's `hi`, and `high()` clears the breakout channel.
fn rising_bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let c = 100.0 + i as f64;
            Bar {
                ts: i as i64 * 60_000,
                open: c,
                high: c,
                low: c,
                close: c,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: Some("BTCUSDT".into()),
            }
        })
        .collect()
}

/// ⚠ **The gate behind "an agent can copy a template and it will actually trade."**
///
/// `list_templates` is the surface an AGENT copies from, and until this test it was the one
/// template surface with no behavioral gate at all: the compile pin above is the whole of what
/// this crate checked. (`crates/vike-studio-core/tests/templates_execute.rs` is the twin over
/// that crate's own copy of these sources; it runs them through the real Run pipeline instead.)
///
/// Parsing is the wrong bar. Rhai resolves a REGISTERED function when its line RUNS, not at
/// compile time, and `discover_params` runs only the top level — so every call a template makes
/// (all of them inside `fn on_bar()`) is unchecked by a compile gate. Three mistakes land in
/// that blind spot identically: an unbound NAME (a typo, or any name outside
/// `vike_script::RHAI_INDICATORS` — the set the host actually registers), a wrong
/// ARITY, and a wrong ARGUMENT TYPE (`rhai`'s `resolve_fn` hashes each argument's `TypeId` and
/// performs no INT->FLOAT coercion for a registered function, so `market(1, 1)` or `sma(5.0)`
/// misses just as hard as a typo). Each raises `ErrorFunctionNotFound` on every bar;
/// `RhaiStrategy`'s hook runner swallows it (fail-safe: zero orders that bar) and self-disables
/// after 10 consecutive errors. The strategy looks mounted and silently never trades. Only
/// EXECUTION distinguishes that from a strategy that simply saw no signal.
///
/// Non-vacuity is demonstrated rather than argued — see
/// `an_unbound_call_compiles_and_then_reaches_the_broker_with_nothing` below, which drives the
/// same series and shows this assertion genuinely fails for such a script.
#[test]
fn every_shipped_template_reaches_the_broker() {
    for (name, src) in TEMPLATES {
        let mut strat = RhaiStrategy::<MockBroker>::compile(src)
            .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
        let mut broker = MockBroker::default();
        for bar in rising_bars(80) {
            broker.px = bar.close;
            strat.on_bar(&mut broker, &bar);
        }
        assert!(
            !broker.markets.is_empty(),
            "template {name} placed no order over 80 rising bars — a template an agent COPIES \
                 must actually trade. Check that every function it calls is host-bound \
                 (`vike_script::RHAI_INDICATORS`, plus `crates/vike-script/src/engine/host.rs`'s \
                 `register_reads`/`register_verbs`/`build_engine`) and that each call's arity and \
                 ARGUMENT TYPES match the registration exactly — rhai coerces neither."
        );
        for (symbol, side, qty) in &broker.markets {
            assert_eq!(symbol, "BTCUSDT", "template {name} must route to the bar's own symbol");
            assert!(*side == 1 || *side == -1, "template {name} sent side {side}, not ±1");
            assert!(*qty > 0.0, "template {name} sent a non-positive qty {qty}");
        }
    }
}

/// The proof that the gate above is not vacuous, and a live specimen of the failure it exists
/// to catch.
///
/// The SMA-cross template with its `sma(` calls rewritten to [`UNBOUND_WITNESS`] still COMPILES
/// and still reports its `param()` knobs — both compile-only gates stay green on it — while
/// reaching the broker exactly never.
///
/// ⚠ The witness used to be `wma`, a REAL registry indicator the host did not bind. It stopped
/// being a witness when the host widened to the registry, so it is now a name outside the
/// registry ENTIRELY — asserted, not assumed, on both axes below. Do not restore `wma`: a
/// bound name here makes this test pass for the wrong reason and quietly turns
/// `every_shipped_template_reaches_the_broker` into a claim about nothing.
#[test]
fn an_unbound_call_compiles_and_then_reaches_the_broker_with_nothing() {
    let sma_cross = TEMPLATES[0].1; // the one starter this rewrite has a call to break
    let unbound = sma_cross.replace("sma(", &format!("{UNBOUND_WITNESS}("));
    assert!(
        unbound.contains(&format!("{UNBOUND_WITNESS}(")),
        "test premise: the rewrite must have applied"
    );
    assert!(
        vike_script::discover_params(&unbound).is_ok(),
        "test premise: a call to an unbound function still COMPILES — that is the whole hazard"
    );
    assert!(
        !vike_script::RHAI_INDICATORS.contains(&UNBOUND_WITNESS),
        "test premise: the witness must stay outside the host-bound set"
    );
    assert!(
        vike_indicators::get(UNBOUND_WITNESS).is_none(),
        "test premise: the witness must not be a registry indicator either"
    );

    let mut strat = RhaiStrategy::<MockBroker>::compile(&unbound).expect("still compiles");
    let mut broker = MockBroker::default();
    for bar in rising_bars(80) {
        broker.px = bar.close;
        strat.on_bar(&mut broker, &bar);
    }
    assert!(
        broker.markets.is_empty(),
        "a script calling an unbound host function must reach the broker with NOTHING — \
             otherwise `every_shipped_template_reaches_the_broker` could not detect one"
    );
}

/// A minimal, VALID sweep profile (shape errors must never be what a connect test trips on).
const PARAMSCAN_PROFILE: &str = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\nfrom = \"0\"\nto = \"100000\"\n[strategy]\nname = \"buy_hold\"\n[sweep]\nfast = [5, 10]\n";

/// The tool's OWN argument checks still fire before any connect. Profile-SHAPE errors (no
/// `[sweep]`/`[walkforward]` table, bad range, unknown strategy) deliberately moved SERVER-side
/// when these tools started shipping the TOML verbatim — one profile parser in the workspace,
/// one error source; the server's `run_paramscan_profile` arm answers them (pinned by
/// `vike-datahub`'s `run_sweep_walkforward_profile_roundtrip` test).
#[test]
fn run_tools_reject_a_missing_profile_arg_before_any_connect() {
    for tool in ["run_sweep", "run_walk_forward"] {
        let resp = call(tool, json!({}));
        assert_eq!(resp["result"]["isError"], true, "{tool}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("profile"), "{tool}: {text}");
    }
    // An unparsable `script` injection is likewise a local error (it rewrites the TOML here).
    let resp = call("run_sweep", json!({ "profile": "this is [not valid", "script": "x" }));
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn remote_run_tools_without_a_reachable_datahub_are_clean_errors() {
    // Point at a port nothing is listening on (`127.0.0.1:1` refuses immediately): each remote
    // tool's connect failure must be a clean tool error, never a panic — same pin as
    // `list_strategies_without_a_reachable_datahub_is_a_clean_error`.
    let mut s = Server { datahub_addr: "127.0.0.1:1".to_string(), ..test_server() };
    let wf_profile = format!("{PARAMSCAN_PROFILE}[walkforward]\nn_splits = 4\n");
    for (tool, args) in [
        ("run_sweep", json!({ "profile": PARAMSCAN_PROFILE })),
        ("run_walk_forward", json!({ "profile": wf_profile })),
        ("list_series", json!({})),
    ] {
        let resp =
            s.handle(&req(1, "tools/call", json!({ "name": tool, "arguments": args }))).unwrap();
        assert_eq!(resp["result"]["isError"], true, "{tool} must error, not panic");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("cannot connect"), "{tool}: {text}");
    }
}
