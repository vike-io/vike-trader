//! The protocol surface: initialize, the instructions text, prompts, resources and RPC errors.

use super::*;
use crate::cmd::mcp::prompts::prompt_description;
use crate::cmd::mcp::protocol::tool_ok;

#[test]
fn initialize_echoes_protocol_and_names_the_server() {
    let resp =
        server().handle(&req(1, "initialize", json!({ "protocolVersion": "2025-06-18" }))).unwrap();
    assert_eq!(resp["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(resp["result"]["serverInfo"]["name"], "vike-cli");
}

#[test]
fn initialize_carries_instructions_that_name_the_surface_beyond_this_one() {
    let mut s = server();
    let init = s.handle(&req(1, "initialize", json!({}))).unwrap();
    let text = init["result"]["instructions"].as_str().expect(
        "initialize must carry an `instructions` string — it is the ONE channel that can say \
             what this surface is NOT, and a client shows it to the model",
    );
    // The four capabilities that live outside this server, each measured as MISSING by the
    // 2026-09-06 model run. Named here rather than derived because the LIST is the editorial
    // decision this text exists to make; that each one is REAL is what the next test derives.
    for needle in [
        "vike-backend datahub --record",
        "vike-cli data hist fetch",
        // ⚠ `backtest run --local`, not `backtest --local`: decision 11 of the
        // backtest-CLI-surface design made the sub-verb mandatory, so the older spelling is
        // now an exit-2. A needle that stopped at `vike-cli backtest` would have kept passing
        // over the broken invocation, which is why this one carries the sub-verb.
        "vike-cli backtest run --local",
        // ⚠ …and the datahub it reads through. The bullet said "backtest with no server" until
        // decision 0084's 2026-09-25 amendment, and a needle on `--local` alone kept passing
        // over that false promise — the half that makes the command WORK on a box with files
        // and no server is this one.
        "VIKE_DATAHUB_STORE=DIR vike-backend datahub",
    ] {
        assert!(text.contains(needle), "the instructions must name `{needle}`");
    }
    assert!(
        !text.contains("no server"),
        "the instructions promise a run with no server, and since decision 0084's 2026-09-25 \
             amendment every history read goes through a datahub — `--local` moves the engine, \
             not the history"
    );
    assert!(text.contains("settings/secrets.env"), "the instructions must name the ONE store");
}

#[test]
fn the_instructions_are_scoped_by_the_same_access_the_roster_is() {
    // The write clause and the `list_series` clause are ADVERTISEMENTS of tools. A profile that
    // withholds the tool must withhold the sentence, or the prose is advertising what
    // `tools/list` refuses — the exact drift [`ToolAccess`] exists to make impossible.
    let full = instructions(&ToolAccess::full());
    assert!(full.contains("PREVIEW-GATED"), "`full` serves the write tools and must say so");
    assert!(full.contains("list_series"), "`full` serves the datahub reads and may say so");

    let read_only = instructions(&ToolAccess::new(Profile::ReadOnly, Vec::new()));
    assert!(
        !read_only.contains("PREVIEW-GATED"),
        "`read-only` serves no write tool, so the preview gate is a door the agent cannot reach"
    );
    assert!(read_only.contains("list_series"), "`read-only` still serves the datahub reads");

    let offline = instructions(&ToolAccess::new(Profile::Offline, Vec::new()));
    assert!(!offline.contains("PREVIEW-GATED"), "`offline` serves no write tool");
    assert!(
        !offline.contains("list_series"),
        "`offline` withholds every network tool, `list_series` included — telling an agent to \
             confirm a fetch with it is advertising a withheld tool in prose"
    );

    // What NO profile may drop: the operator-side commands. They are things a HUMAN types, and
    // no scoping of THIS server changes what the operator can run on the box.
    for (name, text) in [("full", &full), ("read-only", &read_only), ("offline", &offline)] {
        assert!(
            text.contains("vike-backend datahub --record") && text.contains("settings/secrets.env"),
            "the `{name}` profile dropped an OPERATOR-side command; the profile scopes this \
                 server's tools, not the operator's terminal"
        );
    }
}

#[test]
fn the_instructions_name_only_real_commands() {
    // The rot the whole repository is organised against: prose that names a command the binary
    // does not have. It is worse than naming none — it sends an operator to a terminal to type
    // something that fails, on the word of the surface itself.
    //
    // Each `vike-cli` verb is checked against `crate::COMMANDS`, the dispatcher's own roster,
    // and each SUBCOMMAND and FLAG against the owning module's own `USAGE`. Neither is a copy.
    //
    // ⚠ Two bounds, declared rather than implied. Arguments are NOT checked (`run.toml`,
    // `binance:BTCUSDT:1h` and `180` are values an operator supplies, not names this workspace
    // owns), and a NON-`vike-cli` binary is not checked here at all — this crate cannot see
    // another crate's manifest, so `crates/vike-ops/tests/docs/mcp_instructions_gate.rs` holds
    // `vike-backend` to a real `[[bin]]` from outside.
    fn usage_for(verb: &str) -> Option<&'static str> {
        match verb {
            "data" => Some(crate::cmd::data::usage::USAGE),
            "secrets" => Some(crate::cmd::secrets::USAGE),
            "backtest" => Some(crate::cmd::backtest::USAGE),
            _ => None,
        }
    }

    let verbs: Vec<&str> = crate::COMMANDS.iter().map(|(name, _)| *name).collect();
    let mut checked = 0usize;
    for access in [ToolAccess::full(), ToolAccess::new(Profile::Offline, Vec::new())] {
        let text = instructions(&access);
        for command in backticked_commands(&text) {
            let mut words = command.split_whitespace();
            if words.next() != Some("vike-cli") {
                continue;
            }
            // A span that is JUST the binary name (`vike-cli`, naming the product rather than
            // invoking it) carries no verb to check. It is not unchecked: the BINARY half is
            // `crates/vike-ops/tests/docs/mcp_instructions_gate.rs`'s, from where a manifest is
            // visible.
            let Some(verb) = words.next() else { continue };
            assert!(
                verbs.contains(&verb),
                "the MCP instructions name `vike-cli {verb}`, which is not a registered \
                     subcommand. The dispatcher's roster is {verbs:?}."
            );
            let usage = usage_for(verb).unwrap_or_else(|| {
                panic!(
                    "the MCP instructions name `vike-cli {verb}` and this test has no `USAGE` \
                         to hold its subcommands and flags to — add the arm, do not delete the \
                         check"
                )
            });
            for word in words {
                // Values are the operator's, not ours (see the bound above).
                if !word.starts_with("--") && (word.contains(['.', ':']) || word.starts_with('$')) {
                    continue;
                }
                if word.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                assert!(
                    usage.contains(word),
                    "the MCP instructions name `{word}` under `vike-cli {verb}`, which that \
                         command's own USAGE does not offer"
                );
                checked += 1;
            }
        }
    }
    // Non-vacuity: a text whose backtick spans stopped parsing would pass every assertion above
    // by checking nothing at all.
    assert!(checked >= 4, "only {checked} subcommand/flag(s) were checked — the parse broke");
}

#[test]
fn the_server_serves_prompts_and_each_one_renders() {
    let mut s = server();
    let init = s.handle(&req(0, "initialize", json!({}))).unwrap();
    assert!(
        init["result"]["capabilities"]["prompts"].is_object(),
        "initialize must declare the prompts capability once prompts/list answers"
    );
    let listed = s.handle(&req(1, "prompts/list", json!({}))).unwrap();
    let names: Vec<&str> = listed["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["backtest_a_strategy", "arm_a_venue", "triage_a_stuck_order"]);
    for name in names {
        // With NO arguments — the shape a human browsing a client's prompt menu gets. Every
        // argument this server declares is optional, so this must render rather than error.
        let got = s.handle(&req(2, "prompts/get", json!({ "name": name }))).unwrap();
        let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        assert!(!text.is_empty(), "{name} rendered an empty prompt");
        assert_eq!(got["result"]["messages"][0]["role"], "user", "{name}");
        assert_eq!(
            got["result"]["description"],
            prompt_description(name),
            "{name}: prompts/get must echo the description prompts/list advertised"
        );
    }
    let bogus = s.handle(&req(3, "prompts/get", json!({ "name": "nope" }))).unwrap();
    assert_eq!(bogus["error"]["code"], -32602, "got {bogus}");
}

#[test]
fn every_write_touching_prompt_teaches_the_token_not_just_confirm() {
    // ⚠ THE FAILURE THIS PINS. The published pages describing this surface said `confirm: true`
    // was the whole gate — true before the token binding landed, false since. A prompt written
    // from one of those pages teaches an agent a flow that is REFUSED on its second call, with
    // a message about a token the prompt never mentioned. So every prompt that touches a write
    // tool must name the token, its single use and its binding.
    let mut s = server();
    for name in ["arm_a_venue", "triage_a_stuck_order"] {
        let got = s.handle(&req(1, "prompts/get", json!({ "name": name }))).unwrap();
        let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
        for needle in ["preview_token", "confirm: true", "at most ONCE", "BOUND to the command"] {
            assert!(text.contains(needle), "{name}'s prompt never says {needle:?}");
        }
        assert!(
            text.contains("verified_by_node"),
            "{name}'s prompt must tell the agent to read the node's verdict, not the estimate"
        );
        // ...and the ROSTER, name by name. `two_call_gate` renders it from the write tools this
        // session ADMITS — which under the default `full` profile is `WRITE_TOOLS` entire; this
        // is what keeps it rendered. Spelled out by hand it was a FOURTH copy of the roster
        // held equal to nothing — an eighth write tool joins the routing, the `destructiveHint`
        // annotations and the transcript harness by construction, and would be missing from
        // exactly the sheet an agent reads before calling it ONCE and believing it executed.
        for tool in WRITE_TOOLS {
            assert!(text.contains(tool), "{name}'s prompt never names the write tool {tool:?}");
        }
        // The window is derived too, for the same reason one notch smaller: retuning
        // `PREVIEW_WINDOW` must not leave the teaching sheet quoting the old number.
        let window = format!("expires {} seconds", PREVIEW_WINDOW.as_secs());
        assert!(text.contains(&window), "{name}'s prompt must say {window:?}");
    }
    // The backtest flow reaches no write tool at all, and must not pretend otherwise — an
    // agent told to confirm something during a backtest looks for a gate that is not there.
    let bt = s.handle(&req(2, "prompts/get", json!({ "name": "backtest_a_strategy" }))).unwrap();
    let text = bt["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(!text.contains("preview_token"), "the backtest flow places no orders");
}

#[test]
fn a_prompt_argument_reaches_the_rendered_text() {
    // The arguments are advertised, so they must do something — an advertised knob that
    // changes nothing is the same defect class as a settings key nothing reads.
    let mut s = server();
    let got = s
        .handle(&req(
            1,
            "prompts/get",
            json!({ "name": "triage_a_stuck_order", "arguments": { "client_order_id": "c-42" } }),
        ))
        .unwrap();
    let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("c-42"), "the coid argument must reach the text: {text}");
}

#[test]
fn the_server_advertises_resources_and_lists_them() {
    let mut s = server();
    let init = s.handle(&req(1, "initialize", json!({}))).unwrap();
    assert!(
        init["result"]["capabilities"]["resources"].is_object(),
        "initialize must declare the resources capability once resources/list answers — a \
             client that does not see it never calls the method"
    );
    let listed = s.handle(&req(2, "resources/list", json!({}))).unwrap();
    let uris: Vec<&str> = listed["result"]["resources"]
        .as_array()
        .expect("resources/list returns an array")
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert!(uris.contains(&"vike://node/snapshot"), "got {uris:?}");
    assert!(uris.contains(&"vike://backtest/last"), "got {uris:?}");
    // Every advertised resource must carry the two fields a client renders it by. A URI with
    // no name is a row a human picks blind.
    for r in listed["result"]["resources"].as_array().unwrap() {
        assert!(r["name"].as_str().is_some_and(|n| !n.is_empty()), "{r} has no name");
        assert_eq!(r["mimeType"], "application/json", "{r}");
    }
}

#[test]
fn an_unread_resource_is_an_error_rather_than_an_empty_document() {
    // ⚠ The distinction this pins. An agent handed `{}` for `vike://backtest/last` would
    // summarise it as a backtest that produced nothing — a different claim from "no backtest
    // has run". Same for an unknown URI: a silent empty read is a lie an agent cannot detect.
    let mut s = server();
    let miss =
        s.handle(&req(1, "resources/read", json!({ "uri": "vike://backtest/last" }))).unwrap();
    assert_eq!(miss["error"]["code"], -32002, "got {miss}");
    assert!(
        miss["error"]["message"].as_str().unwrap().contains("no backtest has run"),
        "the error must say WHICH absence it is: {miss}"
    );

    let bogus = s.handle(&req(2, "resources/read", json!({ "uri": "vike://nope" }))).unwrap();
    assert_eq!(bogus["error"]["code"], -32002, "got {bogus}");

    // The node snapshot with no `--node` is the same shape — an error naming the missing
    // configuration, never an empty snapshot an agent would read as a flat account.
    let no_node =
        s.handle(&req(3, "resources/read", json!({ "uri": "vike://node/snapshot" }))).unwrap();
    assert_eq!(no_node["error"]["code"], -32002, "got {no_node}");
    assert!(no_node["error"]["message"].as_str().unwrap().contains("--node"), "got {no_node}");
}

#[test]
fn the_last_backtest_resource_serves_what_the_tool_returned() {
    // The resource is a second WAY IN to the tool's answer, not a second source of it — so
    // what it serves must be BYTE-IDENTICAL to the tool's own rendering. There is no datahub
    // here, so drive the recording seam directly rather than pretending a run happened.
    let mut s = server();
    let report = json!({ "report": { "sharpe": 1.25, "trades": 42 } });
    s.last_backtest = Some(render(&report));
    let got =
        s.handle(&req(1, "resources/read", json!({ "uri": "vike://backtest/last" }))).unwrap();
    let contents = &got["result"]["contents"][0];
    assert_eq!(contents["uri"], "vike://backtest/last");
    assert_eq!(contents["mimeType"], "application/json");
    assert_eq!(
        contents["text"].as_str().unwrap(),
        tool_ok(report.clone())["content"][0]["text"].as_str().unwrap(),
        "the resource and the tool must render the same report identically"
    );
}

#[test]
fn a_panicking_resource_read_is_an_rpc_error_not_a_dead_session() {
    // The twin of the tool pin below, for the second way in. A resource has no tool-result
    // envelope, so the caught panic surfaces as a JSON-RPC error — and the session goes on:
    // the SAME server answers the next request.
    let mut s = server();
    let resp =
        s.handle(&req(1, "resources/read", json!({ "uri": "vike://__test_panic" }))).unwrap();
    assert_eq!(resp["error"]["code"], -32603, "{resp}");
    assert!(resp["error"]["message"].as_str().unwrap().contains("panicked"), "{resp}");
    let next = s.handle(&req(2, "ping", json!({}))).unwrap();
    assert!(next.get("result").is_some(), "the session must survive a caught panic: {next}");
}

#[test]
fn a_panicking_prompt_render_is_an_rpc_error_not_a_dead_session() {
    let mut s = server();
    let resp = s.handle(&req(1, "prompts/get", json!({ "name": "__test_panic" }))).unwrap();
    assert_eq!(resp["error"]["code"], -32603, "{resp}");
    assert!(resp["error"]["message"].as_str().unwrap().contains("panicked"), "{resp}");
    let next = s.handle(&req(2, "ping", json!({}))).unwrap();
    assert!(next.get("result").is_some(), "the session must survive a caught panic: {next}");
}

#[test]
fn a_panicking_tool_becomes_iserror_not_a_dead_session() {
    // Mirrors vike-mcp's rpc.rs `tools_call_panic_becomes_iserror` pin: the catch_unwind in
    // `handle` turns a panicking tool into an `isError` tool RESULT — never a JSON-RPC
    // protocol error, never a crashed stdio loop.
    let resp = call("__test_panic", json!({}));
    assert_eq!(resp["result"]["isError"], true);
    assert!(resp["result"]["content"][0]["text"].as_str().unwrap().contains("panicked"));
    assert!(resp.get("error").is_none(), "a caught tool panic is NOT a protocol error");
}

#[test]
fn unknown_method_is_a_jsonrpc_error() {
    let resp = server().handle(&req(9, "no/such", json!({}))).unwrap();
    assert_eq!(resp["error"]["code"], -32601);
}

#[test]
fn a_notification_gets_no_response() {
    assert!(
        server()
            .handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .is_none()
    );
}
