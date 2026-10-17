//! Tool scoping: what each `--profile` ring serves, and the roster the router reads.

use super::*;
use crate::cmd::mcp::config::served_tool_names;
use crate::cmd::mcp::resources::{RESOURCE_TOOLS, resources_spec};
use crate::cmd::mcp::scope::profile_names;

// ---- tool scoping (`--profile`, `--deny-tool`) -------------------------------------------

/// EXACTLY what `--profile read-only` serves, spelled out.
///
/// ⚠ A positive list rather than a count, and rather than a filter re-derived here: a count
/// passes when one tool is swapped for another, and a re-derived filter is the implementation
/// agreeing with itself. A NEW tool breaks this test, which is correct — it is the
/// "adding a venue reddens every table" property applied to the tool roster: whoever adds a
/// tool decides which rings serve it, in a review, rather than inheriting a ring by accident.
const READ_ONLY_TOOLS: [&str; 12] = [
    "validate_strategy",
    "discover_params",
    "list_templates",
    "list_indicators",
    "run_backtest",
    "run_sweep",
    "run_walk_forward",
    "list_strategies",
    "list_series",
    "node_snapshot",
    // The two per-call node reads. They belong here on the SAME argument every row above them
    // does: `read-only` is `!is_write_tool`, and neither of these can change a byte on the node
    // — `strategy_status` and `settings_show` authenticate under the OBSERVE scope, which the
    // node will not accept a command on at all.
    "strategy_status",
    "settings_show",
];

/// …and EXACTLY what `--profile offline` serves: the four that open no socket.
const OFFLINE_TOOLS: [&str; 4] =
    ["validate_strategy", "discover_params", "list_templates", "list_indicators"];

fn server_under(profile: Profile) -> Server {
    Server { access: ToolAccess::new(profile, Vec::new()), ..test_server() }
}

#[test]
fn the_default_profile_serves_the_roster_that_shipped_before_the_flag() {
    // Held equal to `tools_spec` itself rather than to a hand-written list of seventeen names:
    // "identical to today's roster" is the actual claim, and a third copy of that roster (after
    // `server.json`'s) is a third thing to keep in step.
    assert_eq!(
        ToolAccess::full().advertised(),
        tools_spec(),
        "an absent --profile must serve exactly what this server implements"
    );
    for name in served_tool_names() {
        assert!(ToolAccess::full().admits(&name), "{name} must be routed under the default");
    }
}

#[test]
fn the_read_only_profile_serves_exactly_the_reads() {
    let access = ToolAccess::new(Profile::ReadOnly, Vec::new());
    assert_eq!(advertised_names(&access), READ_ONLY_TOOLS);
    for tool in WRITE_TOOLS {
        assert!(!access.admits(tool), "{tool} is a write and must be withheld");
    }
}

#[test]
fn the_offline_profile_serves_exactly_the_tools_that_open_no_socket() {
    let access = ToolAccess::new(Profile::Offline, Vec::new());
    assert_eq!(advertised_names(&access), OFFLINE_TOOLS);
    // The ring is the tool's OWN declaration read back, so this is also a check that every
    // offline tool really does declare it.
    let spec = tools_spec();
    for tool in spec.as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let offline = tool["annotations"]["openWorldHint"] == json!(false);
        assert_eq!(offline, OFFLINE_TOOLS.contains(&name), "{name}: openWorldHint disagrees");
    }
}

#[test]
fn the_rings_nest() {
    let (full, read_only, offline) = (
        ToolAccess::full(),
        ToolAccess::new(Profile::ReadOnly, Vec::new()),
        ToolAccess::new(Profile::Offline, Vec::new()),
    );
    for name in served_tool_names() {
        if offline.admits(&name) {
            assert!(read_only.admits(&name), "{name}: offline ⊄ read-only");
        }
        if read_only.admits(&name) {
            assert!(full.admits(&name), "{name}: read-only ⊄ full");
        }
    }
    assert!(advertised_names(&offline).len() < advertised_names(&read_only).len());
    assert!(advertised_names(&read_only).len() < advertised_names(&full).len());
}

#[test]
fn the_roster_and_the_router_are_one_derivation() {
    // ⚠ THE PROPERTY THE WHOLE FEATURE RESTS ON. An advertised roster and a routing gate
    // computed separately are two lists that can disagree — a tool advertised and then refused
    // wastes an agent's turn, and one WITHHELD but still routed is an ungated write. Checked
    // for every ring AND with a `--deny-tool` layered on, because the subtraction is the case
    // where a second mechanism would most plausibly have been introduced.
    for profile in [Profile::Full, Profile::ReadOnly, Profile::Offline] {
        let access = ToolAccess::new(profile, vec!["list_series".to_string()]);
        let advertised = advertised_names(&access);
        for name in served_tool_names() {
            assert_eq!(
                advertised.contains(&name),
                access.admits(&name),
                "{name} under {profile:?}: tools/list and tools/call disagree"
            );
        }
        assert!(!access.admits("list_series"), "a denied tool is withheld under every ring");
    }
}

#[test]
fn a_write_tool_is_refused_under_read_only_and_the_refusal_names_the_profile() {
    let mut s = server_under(Profile::ReadOnly);
    let err = s
        .call_tool(
            "submit_order",
            &json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }),
        )
        .unwrap_err();
    assert!(err.refused, "a scope refusal is a GATE saying no, and the transcript reads that");
    assert!(err.message.contains("read-only"), "the profile must be named: {err:?}");
    assert!(err.message.contains("--profile full"), "…and how to change it: {err:?}");
    assert!(err.message.contains("node_snapshot"), "…and what IS served: {err:?}");
    // Nothing was previewed either: the gate runs BEFORE the command is built, so a refused
    // write leaves no token behind to be confirmed later.
    assert!(s.pending.by_token.is_empty(), "a refused write must mint nothing");
}

#[test]
fn a_denied_tool_is_subtracted_from_whatever_the_profile_allows() {
    // ONE mechanism, not a second one: the deny list feeds the same pass the profile does.
    let access = ToolAccess::new(Profile::Full, vec!["market_exit".to_string()]);
    assert!(!access.admits("market_exit"));
    assert!(access.admits("submit_order"), "the rest of the ring is untouched");
    assert!(!advertised_names(&access).contains(&"market_exit".to_string()));
    let mut s = Server { access, ..test_server() };
    let err = s.call_tool("market_exit", &json!({})).unwrap_err();
    assert!(err.refused);
    assert!(err.message.contains("--deny-tool market_exit"), "got: {err:?}");
}

#[test]
fn an_unknown_profile_is_refused_and_never_widens_to_full() {
    // ⚠ THE FAILURE THIS PINS is the one the rival surface gets right and a `_ => Full` arm
    // gets catastrophically wrong: a typo in a launch config that silently serves the write
    // tools to an agent the operator believed was scoped down.
    let err = Profile::parse("readonly").unwrap_err();
    for name in profile_names() {
        assert!(err.contains(name), "the error must name every valid profile: {err}");
    }
    let refused = parse_config(["--profile", "readonly"].map(String::from).into_iter(), None, None)
        .expect_err("an unknown profile must not parse");
    assert!(refused.contains("read-only"), "got: {refused}");
    // …and the whole invocation is refused rather than started, so there is no server to widen.
    let ok = parse_config(["--profile", "read-only"].map(String::from).into_iter(), None, None)
        .expect("a real profile parses");
    assert_eq!(ok.access.profile_name(), "read-only");
}

#[test]
fn an_unknown_deny_tool_is_a_usage_error_naming_the_roster() {
    // A silently-ignored `--deny-tool` is an operator believing a tool is gone when it is not.
    let err =
        parse_config(["--deny-tool", "submit-order"].map(String::from).into_iter(), None, None)
            .expect_err("a name this server does not serve must not parse");
    assert!(err.contains("submit_order"), "the roster must be quoted back: {err}");
    let ok =
        parse_config(["--deny-tool", "submit_order"].map(String::from).into_iter(), None, None)
            .expect("a served name parses");
    assert!(!ok.access.admits("submit_order"));
}

#[test]
fn every_resource_is_a_way_in_to_a_named_tool() {
    // The mapping the resource scope gate reads, held equal to the advertised resources in BOTH
    // directions — a third resource with no row here would silently escape the profile.
    let spec = resources_spec();
    let uris: Vec<&str> =
        spec.as_array().unwrap().iter().map(|r| r["uri"].as_str().unwrap()).collect();
    for (uri, tool) in RESOURCE_TOOLS {
        assert!(uris.contains(&uri), "RESOURCE_TOOLS names {uri}, which is not advertised");
        assert!(
            served_tool_names().iter().any(|t| t == tool),
            "{uri} claims to be a way in to {tool}, which this server does not serve"
        );
    }
    for uri in &uris {
        assert!(
            RESOURCE_TOOLS.iter().any(|(u, _)| u == uri),
            "{uri} is advertised with no tool row — it would escape the profile gate"
        );
    }
}

#[test]
fn a_resource_is_withheld_exactly_when_the_tool_it_reaches_is() {
    // ⚠ A resource is a second WAY IN to a tool's implementation. Under `offline`, serving
    // `vike://node/snapshot` would hand back the answer of a tool the profile refused.
    let mut offline = server_under(Profile::Offline);
    let listed = offline.handle(&req(1, "resources/list", json!({}))).unwrap();
    assert_eq!(listed["result"]["resources"], json!([]), "got {listed}");
    let read = offline
        .handle(&req(2, "resources/read", json!({ "uri": "vike://node/snapshot" })))
        .unwrap();
    assert_eq!(read["error"]["code"], -32002, "got {read}");
    assert!(read["error"]["message"].as_str().unwrap().contains("offline"), "got {read}");

    // …and `read-only` withholds NEITHER, because neither resource is a write. That is the
    // non-vacuity half: without it the assertions above would pass on a gate that refused
    // every resource under every profile.
    let mut read_only = server_under(Profile::ReadOnly);
    let listed = read_only.handle(&req(3, "resources/list", json!({}))).unwrap();
    assert_eq!(listed["result"]["resources"].as_array().unwrap().len(), RESOURCE_TOOLS.len());
}

#[test]
fn a_scoped_prompt_says_the_writes_are_absent_instead_of_teaching_them() {
    let mut s = server_under(Profile::ReadOnly);
    let got = s.handle(&req(1, "prompts/get", json!({ "name": "triage_a_stuck_order" }))).unwrap();
    let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.starts_with("⚠ THIS SESSION SERVES NO ORDER-WRITE TOOL"), "got: {text}");
    assert!(text.contains("read-only"), "the banner names the profile: {text}");
    assert!(
        !text.contains("preview_token"),
        "a sheet must not teach a two-call gate for tools it has just said are absent: {text}"
    );
    // The READ half of the sheet survives — the prompt is still the right procedure, minus the
    // acting.
    assert!(text.contains("node_snapshot"), "got: {text}");
    // …and under the default profile the banner is absent entirely (byte-identical teaching).
    let mut full = server();
    let got =
        full.handle(&req(2, "prompts/get", json!({ "name": "triage_a_stuck_order" }))).unwrap();
    let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(!text.contains("SERVES NO ORDER-WRITE TOOL"), "got: {text}");
    assert!(text.contains("preview_token"));
}

#[test]
fn the_write_arm_and_the_write_roster_are_the_same_set() {
    // The `call_tool` match arm decides what is GATED; `WRITE_TOOLS` decides what is
    // ADVERTISED as destructive and what the transcript harness exercises. A tool in one and
    // not the other is either an ungated write or an unexercised gate — and until this test
    // existed the arm was held equal to NOTHING: the annotations and the roster were pinned to
    // each other, the routing was a seven-alternative pattern nobody compared to either.
    for name in WRITE_TOOLS {
        assert!(is_write_tool(name), "{name} is in WRITE_TOOLS but not routed as a write");
    }
    let spec = tools_spec();
    for tool in spec.as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let destructive = tool["annotations"]["destructiveHint"].as_bool() == Some(true);
        assert_eq!(
            destructive,
            is_write_tool(name),
            "{name}: destructiveHint and the write routing disagree"
        );
    }
}

#[test]
fn tools_list_has_read_and_write_tools_with_correct_hints() {
    let resp = server().handle(&req(2, "tools/list", json!({}))).unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"run_backtest") && names.contains(&"submit_order"));
    // The absorbed vike-mcp read surface + the closed write-verb drift are all advertised.
    for n in [
        "validate_strategy",
        "discover_params",
        "list_templates",
        "list_series",
        "run_sweep",
        "run_walk_forward",
        "modify",
        "mass_cancel",
    ] {
        assert!(names.contains(&n), "{n} must be advertised in tools/list");
    }
    for t in tools {
        let n = t["name"].as_str().unwrap();
        if WRITE_TOOLS.contains(&n) {
            assert_eq!(t["annotations"]["destructiveHint"], true, "{n} must be destructive");
            // The mandatory-preview gate is part of each write tool's advertised contract.
            let desc = t["description"].as_str().unwrap();
            assert!(desc.contains("confirm"), "{n} must document the preview gate: {desc}");
        } else {
            assert_eq!(t["annotations"]["readOnlyHint"], true, "{n} must be read-only");
        }
    }
}
