//! The research tools: strategy authoring, the indicator roster, and the backtest run tools.

// The EXECUTION gate on the shipped templates (see `every_shipped_template_reaches_the_broker`):
// the real `RhaiStrategy` mounted over vike-model's shared `OrderBrokerMock` (its `test-support`
// feature) and driven over [`rising_bars`]: monotone closes take `sma(5)` above `sma(20)`, `rsi(14)`
// past the reversion band's `hi`, and `high()` through the breakout channel.
use vike_model::Strategy;
use vike_model::strategy::OrderBrokerMock;
use vike_rhai::{RhaiStrategy, TEMPLATES};

use super::*;
use crate::backtest_tools::{bounded_walkforward, run_args, search_from};
use crate::data_tools::{LIST_SERIES_LIMIT, bounded_series};
use crate::offline_tools::PROFILE_SCHEMA;
use crate::tool_args::{RunSweepArgs, RunWalkForwardArgs};

/// The bound, its note, the description that states it, and the reason for it: the worst row rendered
/// at the limit stays under the 50,000 characters a client shows inline.
#[test]
fn list_series_is_bounded_and_says_so() {
    let token_id = "7".repeat(77);
    let row = json!({
        "kind": "quote", "venue": "polymarket", "symbol": token_id, "interval": "1m",
        "first_ts": 1_792_579_500_000_i64, "last_ts": 1_793_117_700_000_i64, "rows": 1_000_000,
    });
    let at_the_limit = json!(bounded_series(vec![row.clone(); LIST_SERIES_LIMIT]));
    assert_eq!(
        at_the_limit.as_object().map(|o| o.len()),
        Some(1),
        "the plain shape: {at_the_limit}"
    );
    assert!(render(&at_the_limit).len() < 50_000, "{}", render(&at_the_limit).len());

    let past_it = json!(bounded_series(vec![row; LIST_SERIES_LIMIT + 1]));
    assert_eq!(past_it["series"].as_array().map(Vec::len), Some(LIST_SERIES_LIMIT));
    assert_eq!(past_it["truncated"], json!(true));
    assert_eq!(past_it["total"], json!(LIST_SERIES_LIMIT + 1));
    assert!(past_it["note"].as_str().is_some_and(|n| n.contains("vike-cli data history ls")));

    let spec = tools_spec();
    let tool = spec.as_array().and_then(|t| t.iter().find(|t| t["name"] == "list_series"));
    let description = tool.and_then(|t| t["description"].as_str()).unwrap_or_default();
    assert!(description.contains(&format!("At most {LIST_SERIES_LIMIT} rows")), "{description}");
}

/// A deterministic strictly-RISING bar series (close `100 + i`, one minute apart).
fn rising_bars(n: usize) -> Vec<vike_marketdata::Bar> {
    (0..n)
        .map(|i| {
            let c = 100.0 + i as f64;
            vike_marketdata::Bar {
                timestamp: i as i64 * 60_000,
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
        vike_model::run_request::search_methods::SEARCH_METHODS.to_vec(),
        "the tool schema and the protocol must name one roster"
    );
    for knob in ["trials", "seed", "euler_depth"] {
        assert!(
            tool["inputSchema"]["properties"][knob].is_object(),
            "the agent must be able to pass {knob}"
        );
    }
}

/// `run_sweep`'s search selection, read as the tool reads the call.
fn search_from_args(
    args: Value,
) -> Result<Option<vike_backtesthub_client::proto::SearchSelectionWire>, String> {
    run_args::<RunSweepArgs>("run_sweep", &args).map(|args| search_from(&args))
}

/// A numeric argument arrives from a model as a JSON NUMBER, and the wire carries TOKENS — so
/// the boundary renders it. A string is accepted too (a model that quoted it is not wrong), and
/// anything else is refused BY NAME rather than silently dropped, which is what
/// `Value::as_str` alone would have done.
#[test]
fn a_numeric_tool_argument_is_rendered_and_a_bad_one_is_named() {
    let search = search_from_args(json!({ "trials": 128, "optimizer": "tpe" }))
        .expect("a number is accepted")
        .expect("a selector was built");
    assert_eq!(search.trials.as_deref(), Some("128"));
    assert_eq!(search.optimizer.as_deref(), Some("tpe"));

    let quoted = search_from_args(json!({ "seed": "7" })).expect("a string is accepted too");
    assert_eq!(quoted.expect("a selector").seed.as_deref(), Some("7"));

    let err = search_from_args(json!({ "trials": [1, 2] })).expect_err("a list is refused");
    assert_eq!(err, "run_sweep: `trials` must be an integer (or its decimal string), got [1,2]");
}

/// No search argument at all builds NO selector, so an ordinary agent grid search ships the
/// frame it always shipped — and reaches a daemon that predates the capability unchanged.
#[test]
fn a_sweep_with_no_method_argument_builds_no_selector() {
    assert!(
        search_from_args(json!({ "profile": "[data]\n" })).expect("ok").is_none(),
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
    // The tool must advertise ONLY what the Rhai host binds (vike_rhai::RHAI_INDICATORS),
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
        .filter(|n| vike_rhai::is_callable(n))
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

/// `profile_template` is the published `profile.json` projected, offline: every table with its key names, each table's
/// keys in full in the schema's order (a key a roster bounds carries its words), and an unknown table refused by id,
/// naming every table. Held to the committed file itself, so a schema change moves the answer with no edit here.
#[test]
fn profile_template_answers_the_published_profile_schema() {
    let schema: Value = serde_json::from_str(PROFILE_SCHEMA).expect("profile.json is JSON");
    let answer =
        |args: Value| call("profile_template", args)["result"]["structuredContent"].clone();
    let published = schema["tables"].as_array().expect("the schema's tables");
    let listed = answer(json!({}));
    let names: Vec<&Value> =
        listed["tables"].as_array().expect("the table list").iter().map(|t| &t["table"]).collect();
    assert_eq!(names, published.iter().map(|t| &t["table"]).collect::<Vec<_>>());
    for table in published {
        let name = table["table"].as_str().expect("a table is named");
        let keys: Vec<&Value> = schema["keys"]
            .as_array()
            .expect("the schema's keys")
            .iter()
            .filter(|key| key["table"] == name)
            .collect();
        let rows = answer(json!({ "table": name }));
        let rows = rows["keys"].as_array().expect("the table's keys");
        let row_keys: Vec<&Value> = rows.iter().map(|row| &row["key"]).collect();
        assert_eq!(row_keys, keys.iter().map(|key| &key["key"]).collect::<Vec<_>>(), "{name:?}");
        for (row, key) in rows.iter().zip(&keys) {
            assert_eq!((&row["type"], &row["required"]), (&key["type"], &key["required"]), "{row}");
            let roster = schema["rosters"]
                .as_array()
                .expect("the schema's rosters")
                .iter()
                .find(|roster| roster["id"] == key["roster_id"]);
            assert_eq!(roster.map(|roster| &roster["members"]), row.get("values"), "{row}");
        }
    }
    let refused = call("profile_template", json!({ "table": "no_such_table" }));
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    let text = refused["result"]["content"][0]["text"].as_str().expect("a text");
    assert!(
        text.starts_with(
            "error [mcp.profile_template.unknown_table]: the backtest profile has no table \
             `no_such_table`; its tables are: \"\", \"data\", "
        ),
        "{text}"
    );
}

/// Detail is PULLED, per name or per family — the other half of the tiering above.
#[test]
fn list_indicators_narrows_by_name_and_by_category() {
    // Derived: whatever the host binds first, never a hard-coded name.
    let first = vike_rhai::RHAI_INDICATORS.first().expect("the host binds something");
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
/// `vike-rhai`. A build binding the entire registry has nothing to explain and skips — stated
/// rather than silent, because a vacuous pass should be readable in the test, not inferred.
#[test]
fn list_indicators_says_why_a_registry_name_is_not_callable() {
    // ⚠ NOT `!RHAI_INDICATORS.contains(..)` any more. That set is the BARE names, and since
    // per-line accessors landed a name can be absent from it and still perfectly callable —
    // `bollinger` is. A genuinely held-back indicator is one no spelling reaches, which is
    // exactly what `is_callable` answers.
    let held = vike_indicators::registry().iter().find(|m| !vike_rhai::is_callable(m.name));
    let Some(held) = held else {
        return; // nothing is held back in this build
    };
    let resp = call("list_indicators", json!({ "name": held.name }));
    assert_eq!(resp["result"]["isError"], true, "a name a script cannot call is not an answer");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let why = vike_rhai::unbound_reason(held.name)
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
        !desc.contains(&vike_rhai::RHAI_INDICATORS.join(", ")),
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
        let params = vike_rhai::discover_params(code)
            .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
        assert!(!params.is_empty(), "template {name} must expose param()s");
        assert!(code.contains("on_bar"), "template {name} must define on_bar");
    }
}

/// ⚠ **The gate behind "an agent can copy a template and it will actually trade."**
///
/// `list_templates` is the surface an AGENT copies from, and until this test it was the one
/// template surface with no behavioral gate at all: the compile pin above is the whole of what
/// this crate checked. (`crates/vike-backtesthub/tests/templates_execute.rs` is the twin over
/// that crate's own copy of these sources; it runs them through the real Run pipeline instead.)
///
/// Parsing is the wrong bar. Rhai resolves a REGISTERED function when its line RUNS, not at
/// compile time, and `discover_params` runs only the top level — so every call a template makes
/// (all of them inside a hook) is unchecked by a compile gate. Three mistakes land in that
/// blind spot identically: an unbound NAME (a typo, or any name outside
/// `vike_rhai::RHAI_INDICATORS` — the set the host actually registers), a wrong
/// ARITY, and a wrong ARGUMENT TYPE (`rhai`'s `resolve_fn` hashes each argument's `TypeId` and
/// performs no INT->FLOAT coercion for a registered function, so `.size(1)` on an order, whose
/// builder takes a float, misses just as hard as a typo). Each raises `ErrorFunctionNotFound` on
/// every bar; `RhaiStrategy`'s hook runner swallows it (fail-safe: zero orders that bar) and
/// self-disables after 10 consecutive errors. The strategy looks mounted and silently never
/// trades. Only EXECUTION distinguishes that from a strategy that simply saw no signal.
///
/// Non-vacuity is demonstrated rather than argued — see
/// `an_unbound_call_compiles_and_then_reaches_the_broker_with_nothing` below, which drives the
/// same series and shows this assertion genuinely fails for such a script.
#[test]
fn every_shipped_template_reaches_the_broker() {
    for (name, src) in TEMPLATES {
        let mut strat = RhaiStrategy::<OrderBrokerMock>::compile(src)
            .unwrap_or_else(|e| panic!("template {name} must compile: {e}"));
        // a starter sizes by a share of the equity, so the inert broker's 0.0 would size nothing
        let mut broker = OrderBrokerMock { equity: 10_000.0, ..OrderBrokerMock::default() };
        for bar in rising_bars(80) {
            broker.price = bar.close;
            strat.on_bar(&mut broker, &bar);
        }
        // every starter places through the `strategy` object's order seam, a script's one door
        let seam = broker.placed.iter().map(|p| (p.2, p.3));
        let seam = seam.chain(broker.groups.iter().flatten().map(|(_, leg)| (leg.side, leg.qty)));
        for (side, qty) in seam.clone() {
            assert!(side == 1 || side == -1, "template {name} sent side {side}, not ±1");
            assert!(qty > 0.0, "template {name} sent a non-positive qty {qty}");
        }
        assert!(
            seam.count() > 0,
            "template {name} placed no order over 80 rising bars — a template an agent COPIES \
                 must actually trade. Check that every function it calls is host-bound \
                 (`vike_rhai::RHAI_INDICATORS`, plus `crates/vike-rhai/src/engine/host.rs`'s \
                 `register_reads` and `crates/vike-rhai/src/tactics.rs`'s `register`) and that \
                 each call's arity and ARGUMENT TYPES match the registration exactly — rhai \
                 coerces neither."
        );
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
        vike_rhai::discover_params(&unbound).is_ok(),
        "test premise: a call to an unbound function still COMPILES — that is the whole hazard"
    );
    assert!(
        !vike_rhai::RHAI_INDICATORS.contains(&UNBOUND_WITNESS),
        "test premise: the witness must stay outside the host-bound set"
    );
    assert!(
        vike_indicators::get(UNBOUND_WITNESS).is_none(),
        "test premise: the witness must not be a registry indicator either"
    );

    let mut strat = RhaiStrategy::<OrderBrokerMock>::compile(&unbound).expect("still compiles");
    // the gate's own broker, so only the unbound call differs
    let mut broker = OrderBrokerMock { equity: 10_000.0, ..OrderBrokerMock::default() };
    for bar in rising_bars(80) {
        broker.price = bar.close;
        strat.on_bar(&mut broker, &bar);
    }
    assert!(
        broker.placed.is_empty() && broker.groups.is_empty(),
        "a script calling an unbound host function must reach the broker with NOTHING — \
             otherwise `every_shipped_template_reaches_the_broker` could not detect one"
    );
}

/// A minimal, VALID sweep profile (shape errors must never be what a connect test trips on).
const PARAMSCAN_PROFILE: &str = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\nfrom = \"0\"\nto = \"100000\"\n[strategy]\nname = \"buy_hold\"\n[paramscan]\nfast = [5, 10]\n";

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
}

/// **A `script` is refused before any run starts**, by each run tool, in one text: no inline source
/// rides a profile any more (`vike_client::refusals::BacktestCallRefusal`; it used to be injected as
/// `[strategy.params].src`, which the run dropped unread). The test server dials a dead port, so any
/// answer but the refusal would have been the dial's; a `null` script is no script, and dials.
#[test]
fn a_script_is_refused_before_any_run_starts() {
    let refusal = "error [backtest.script_unsupported]: the `script` argument is no longer supported: \
                   put the strategy in a file under strategies/ and name it with `[strategy] file`";
    let text = |resp: &Value| resp["result"]["content"][0]["text"].as_str().unwrap().to_string();
    let profile = "[strategy]\nfile = \"a.rhai\"\n";
    for tool in ["run_backtest", "run_sweep", "run_walk_forward"] {
        let resp = call(tool, json!({ "profile": profile, "script": "fn on_bar() {}" }));
        assert_eq!(resp["result"]["isError"], true, "{tool}");
        assert_eq!(text(&resp), refusal, "{tool}");
        let resp = call(tool, json!({ "profile": profile, "script": null }));
        let dial = "error [cli.backtest.addr.cannot_connect]: cannot connect";
        assert!(text(&resp).starts_with(dial), "{tool}: {}", text(&resp));
    }
}

/// `run_backtest` on a GRID profile is a clean tool error naming `run_sweep`, against the REAL
/// backtesthub — never a single backtest that silently ignored the grid (the daemon's
/// `run_backtest` refusal, `crates/vike-backtesthub/src/backtesthub_server/verbs.rs`'s `run_backtest`). A
/// refused run leaves `vike://backtest/last` empty: only a completed report is remembered.
#[test]
fn run_backtest_on_a_grid_profile_is_refused_and_names_run_sweep() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: std::sync::Arc<dyn vike_data::HistoryStore + Send + Sync> =
        std::sync::Arc::new(vike_data::HistoryStoreMemory::new());
    std::thread::spawn(move || {
        let _ = vike_backtesthub::test_support::serve_with_stand_ins(
            listener,
            store,
            vike_strategy_lab::stand_in,
        );
    });
    // A profile the daemon VALIDATES (so the refusal, not a shape error, is what answers): the
    // axis `size` is one `buy_hold` declares.
    let grid = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\n\
                interval = \"1d\"\nfrom = \"0\"\nto = \"100000\"\n[engine]\ncash = 1000.0\n\
                [strategy]\nfile = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 1.0\n\
                symbol = \"BTCUSDT\"\n[paramscan]\nsize = [1.0, 2.0]\n";
    let mut s = Server { backtest_addr: addr.to_string(), ..test_server() };
    let resp = s
        .handle(&req(
            1,
            "tools/call",
            json!({ "name": "run_backtest", "arguments": { "profile": grid } }),
        ))
        .unwrap();
    assert_eq!(resp["result"]["isError"], true, "a grid profile must not run as one point");
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("[paramscan]") && text.contains("run_sweep"), "{text}");
    assert!(s.last_backtest.is_none(), "a refused run is not remembered as the last report");
}

/// `run_sweep` answers the SUMMARIZED search from a backtesthub that has it (decision 0078's amendment of 2026-11-19),
/// against the REAL daemon: one row a grid point under the whole report's own keys (`overrides`, `report.<metric id>`,
/// `rank_by`), each row's report a flat metric table with no trade and no block, and `saved_run`, the run whose
/// `report.json` keeps every point whole. The fallback to the whole report against an older daemon is
/// `crate::tests::cancellation`'s fake, which advertises no summary and is answered the whole `ParamscanReport`.
#[test]
fn run_sweep_answers_each_points_metric_table_and_the_run_that_keeps_it_whole() {
    let runs = tempfile::Builder::new().prefix("vike-mcp-sweep").tempdir().expect("a runs root");
    let store: std::sync::Arc<dyn vike_data::HistoryStore + Send + Sync> =
        std::sync::Arc::new(vike_data::HistoryStoreMemory::new());
    let addr = vike_backtesthub::test_support::spawn_server_over_saving_to(
        store,
        vike_strategy_lab::stand_in,
        runs.path().to_path_buf(),
    );
    let grid = "[data]\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nkind = \"bar\"\n\
                interval = \"1d\"\nfrom = \"0\"\nto = \"100000\"\n[engine]\ncash = 1000.0\n\
                [strategy]\nfile = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 1.0\n\
                symbol = \"BTCUSDT\"\n[paramscan]\nsize = [1.0, 2.0, 3.0]\n";
    let mut s = Server { backtest_addr: addr.to_string(), ..test_server() };
    let resp = s
        .handle(&req(
            1,
            "tools/call",
            json!({ "name": "run_sweep", "arguments": { "profile": grid } }),
        ))
        .unwrap();
    assert_eq!(resp["result"]["isError"], false, "{resp}");
    let sweep = &resp["result"]["structuredContent"]["sweep"];
    assert_eq!((&sweep["total"], &sweep["rank_by"]), (&json!(3), &json!("sharpe")), "{sweep}");
    let rows = sweep["rows"].as_array().expect("the rows");
    assert_eq!(rows.len(), 3, "every point, paged in by the client: {sweep}");
    for row in rows {
        assert_eq!(row["overrides"][0][0], "size", "{row}");
        let table = row["report"].as_object().expect("a metric table");
        assert!(table.contains_key("sharpe") && table.contains_key("n_trades"), "{row}");
        assert!(table.values().all(|v| !v.is_array() && !v.is_object()), "flat: {row}");
    }
    let saved_run = sweep["saved_run"].as_str().expect("the run the daemon saved");
    let report = runs.path().join(saved_run).join("report.json");
    let whole: Value = serde_json::from_str(&std::fs::read_to_string(report).unwrap())
        .expect("its report.json is the whole report");
    assert_eq!(whole["rows"].as_array().map(Vec::len), Some(3));
    assert!(whole["rows"][0]["report"]["per_symbol_pnl"].is_array(), "each point whole: {whole}");
}

/// `run_walk_forward` answers its report without the stitched curve unless `include_curve` asks for it (decision 0078's
/// amendment of 2026-11-24): every other key stays, and a value that is no boolean is refused by name.
#[test]
fn run_walk_forward_leaves_the_stitched_curve_out_unless_asked() {
    let report = json!({
        "steps": [],
        "overall": { "sharpe": 1.0 },
        "equity_curve": [1000.0, 1050.0],
        "profitable_step_share": 1.0,
    });
    let mut without = report.clone();
    without.as_object_mut().expect("an object").remove("equity_curve");
    assert_eq!(bounded_walkforward(report.clone(), false), json!({ "walkforward": without }));
    assert_eq!(bounded_walkforward(report.clone(), true), json!({ "walkforward": report }));
    let args = json!({ "profile": "x", "include_curve": "yes" });
    let refused = run_args::<RunWalkForwardArgs>("run_walk_forward", &args).err();
    assert_eq!(refused.as_deref(), Some("`include_curve` must be true or false, got \"yes\""));
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
