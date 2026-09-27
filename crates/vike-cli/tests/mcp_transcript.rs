//! The MCP transport, driven end to end over real byte streams.
//!
//! Every other MCP test calls `Server::handle` or `Server::call_tool` IN PROCESS. This one writes
//! newline-delimited JSON-RPC into `vike_cli::cmd::mcp::serve` and parses what comes back out, so
//! the FRAMING is covered too — the `for line in reader.lines()` loop, the one-response-per-request
//! rule, the parse-error envelope. `serve`'s own doc comment has said "split from `run` so tests
//! drive it" since it was written, and until this file existed no test drove it.
//!
//! # What it proves, and why here rather than inline
//!
//! The mandatory-preview gate is the property every other surface rests on — the tool
//! descriptions, the `prompts/get` sheets, the registry manifest. It is asserted over the WIRE, for
//! EVERY tool in `WRITE_TOOLS` rather than for `submit_order` alone, because the roster is the
//! thing that grows: a venue verb added to the write set and not to this loop would be an
//! unexercised gate, and nothing else would notice.
//!
//! ⚠ The server under test has no node address and no credentials
//! (`vike_cli::cmd::mcp::test_server`), so nothing here can reach a venue even if the gate failed
//! open. That is deliberate, and it is also what makes the assertions readable: a confirm the gate
//! REFUSES comes back as a preview, and a confirm the gate ADMITS comes back as a connection
//! error. Two different answers, so "it got past the gate" is observable rather than assumed.
//!
//! This is the deterministic half of the M6 evaluation work. The other half — does a real model,
//! reading the tool descriptions, preview before it confirms — needs a live model and produces a
//! nondeterministic verdict, so it can never be a merge gate and is not attempted here.
//!
//! # The scoped session
//!
//! The same transport, on a server launched with `--profile read-only`
//! (`vike_cli::cmd::mcp::test_server_under`). Both halves are asserted over the wire because they
//! are two different promises to the client and a unit test of either alone would not notice the
//! other break: `tools/list` must OMIT the write tools — the roster is the agent's entire picture
//! of this server — and `tools/call` must REFUSE one by name, with the profile in the message, and
//! without handing back a preview token that would confirm it.

use std::io::Cursor;

use serde_json::{Value, json};

/// Drive one session: feed these request lines, return the parsed response lines.
///
/// One `serve` call per transcript, so each one is its own PROCESS-equivalent — a fresh
/// `PendingPreviews` store, which is what lets the cross-session assertion below mean anything.
fn transcript(lines: &[Value]) -> Vec<Value> {
    drive(vike_cli::cmd::mcp::test_server(), lines)
}

/// The same, on a server SCOPED to one tool profile — the `--profile <name>` launch, over the real
/// transport rather than through the in-process router.
fn transcript_under(profile: &str, lines: &[Value]) -> Vec<Value> {
    drive(vike_cli::cmd::mcp::test_server_under(profile), lines)
}

fn drive(mut server: vike_cli::cmd::mcp::Server, lines: &[Value]) -> Vec<Value> {
    let input = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
    let mut out = Vec::new();
    vike_cli::cmd::mcp::serve(Cursor::new(input), &mut out, &mut server)
        .expect("serve drains the reader without an io error");
    String::from_utf8(out)
        .expect("the server writes UTF-8")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line is one JSON-RPC message"))
        .collect()
}

/// A `tools/call` request for one tool, as a wire line.
fn call(id: i64, tool: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    })
}

/// The `structuredContent` of a tool response, which is where a preview's fields live.
fn structured(response: &Value) -> &Value {
    assert_eq!(
        response["result"]["isError"], false,
        "expected a successful tool result: {response}"
    );
    &response["result"]["structuredContent"]
}

/// Arguments broad enough for every write tool at once.
///
/// The tools take disjoint argument sets (`submit_order` wants venue/symbol/side/qty,
/// `cancel_order` wants a coid, `set_trading_state` wants a state), and each one IGNORES what it
/// does not name. Passing the union means the loop below needs no per-tool table — and a per-tool
/// table is exactly the thing that would fall out of step with `WRITE_TOOLS`.
///
/// ⚠ A write tool whose REQUIRED arguments are missing here does not skip the loops below — it
/// fails them, because its call answers a build error instead of a preview. That is the right
/// behaviour and it is why this is a union rather than a per-tool table: the failure names the
/// tool, and the fix is one line. `file` names `config.toml` deliberately — `set_setting` on the
/// POLICY file needs the operator's typed confirm, and putting that here would make every
/// write-roster loop depend on a gate that has its own tests in `crates/vike-cli/src/cmd/mcp.rs`.
fn every_write_tools_arguments() -> Value {
    json!({
        "venue": "sim",
        "symbol": "BTCUSDT",
        "side": 1,
        "qty": 1.0,
        "client_order_id": "c-transcript",
        "new_qty": 2.0,
        "state": "halted",
        // …and the node-lifecycle verbs' own required set.
        "interval": "1m",
        "controller_id": "sim__BTCUSDT__1m",
        "name": "spread_maker",
        // …and `delete_series`' own required set. `kind`/`venue` are its SELECTOR (exact, never a
        // substring) and `produced_by` is REQUIRED on this surface for every call, sweep or single
        // series — see that tool's doc for why it is unconditional here and conditional on the CLI.
        "kind": "bar",
        "produced_by": "klines:",
        "file": "config.toml",
        "key": "config.tradehub_addr",
        "value": "127.0.0.1:7879"
    })
}

#[test]
fn the_framing_layer_answers_one_line_per_request() {
    // Three requests in one stream, one of them a NOTIFICATION (no `id`), plus a malformed line.
    // The notification must produce nothing, the malformed line must produce a parse error rather
    // than killing the session, and the request after it must still be answered — the property
    // that makes a long-lived stdio session survive one bad client message.
    let input = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }).to_string(),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
        "{ not json".to_string(),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }).to_string(),
    ]
    .join("\n");
    let mut out = Vec::new();
    let mut server = vike_cli::cmd::mcp::test_server();
    vike_cli::cmd::mcp::serve(Cursor::new(input), &mut out, &mut server).unwrap();
    let responses: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(responses.len(), 3, "one per request + the parse error, none for the notification");
    assert_eq!(responses[0]["id"], 1);
    assert_eq!(responses[1]["error"]["code"], -32700, "a malformed line is a parse error");
    assert_eq!(responses[2]["id"], 2, "the session survives a malformed line");
    assert!(responses[2]["result"]["tools"].is_array());
}

#[test]
fn every_write_tool_refuses_to_execute_on_confirm_alone() {
    // ⚠ THE REGRESSION THIS PINS, over the wire and for the whole roster. `confirm: true` used to
    // be the entire gate, so a FIRST-and-only call executed. It now fails CLOSED: without a
    // `preview_token` the call is a preview whatever `confirm` says, and the preview hands back the
    // token that would confirm it.
    for (i, tool) in vike_cli::cmd::mcp::WRITE_TOOLS.iter().enumerate() {
        let id = i as i64 + 1;
        let mut args = every_write_tools_arguments();
        args["confirm"] = json!(true);
        let responses = transcript(&[call(id, tool, args)]);
        assert_eq!(responses.len(), 1, "{tool}: one request, one response");
        assert_eq!(responses[0]["id"], id, "{tool}: the response carries the request's id");
        let body = structured(&responses[0]);
        assert_eq!(
            body["will_execute"],
            json!(false),
            "{tool}: confirm alone must return a preview, not execute"
        );
        assert!(
            body["preview_token"].is_string(),
            "{tool}: a preview must carry the token that confirms it"
        );
        // ...and it must SAY it is unverified, because with no node there is no verdict, and an
        // absent verdict presented as an approving one is the failure mode that matters here.
        assert_eq!(body["verified_by_node"], json!(false), "{tool}");
    }
}

#[test]
fn a_token_fires_once_and_only_for_its_own_command() {
    let args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    let responses = transcript(&[call(1, "submit_order", args.clone())]);
    let token = structured(&responses[0])["preview_token"].as_str().unwrap().to_string();

    // ⚠ A token minted in one SESSION is meaningless in another: the store lives in the `Server`,
    // which lives for the life of the process, so a fresh `serve` has an EMPTY one. An agent that
    // cached a token across a restart is refused rather than quietly executing against a book that
    // has moved since the preview was priced.
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token);
    let fresh = transcript(&[call(1, "submit_order", confirming.clone())]);
    assert_eq!(fresh[0]["result"]["isError"], true, "got {}", fresh[0]);
    let text = fresh[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("unknown or already used"),
        "a token from a dead session must not confirm anything, got: {text}"
    );

    // Within ONE session: the preview, then the same token against a DIFFERENT command. The
    // binding is what makes previewing `qty: 0.5` and confirming `qty: 50` impossible.
    let mut other = confirming.clone();
    other["qty"] = json!(50.0);
    let bound = transcript(&[
        call(1, "submit_order", args.clone()),
        // The token this second call quotes is `pv-1` — the first token this fresh store mints,
        // which is what the line above just issued. Quoting the response would need two `serve`
        // calls against one server, and the point here is the BINDING, not the plumbing.
        call(2, "submit_order", {
            let mut a = other.clone();
            a["preview_token"] = json!("pv-1");
            a
        }),
    ]);
    assert_eq!(bound.len(), 2);
    assert_eq!(bound[1]["result"]["isError"], true, "got {}", bound[1]);
    let text = bound[1]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("does not match this command"),
        "a token is bound to what it previewed, got: {text}"
    );

    // ...and the SAME token against the SAME command gets PAST the gate — which with no node
    // configured surfaces as a connection error. That is the positive half: without it, every
    // assertion above would still pass if the gate refused everything unconditionally.
    let admitted = transcript(&[
        call(1, "submit_order", args.clone()),
        call(2, "submit_order", {
            let mut a = confirming.clone();
            a["preview_token"] = json!("pv-1");
            a
        }),
    ]);
    assert_eq!(admitted[1]["result"]["isError"], true, "got {}", admitted[1]);
    let text = admitted[1]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("no vike-tradehub node configured"),
        "a matching token must reach the node step — that is what proves the gate ADMITS as well \
         as refuses, got: {text}"
    );

    // The same token a second time is gone: `take` removes before the caller executes, so even a
    // duplicated call cannot fire twice.
    let twice = transcript(&[
        call(1, "submit_order", args.clone()),
        call(2, "submit_order", {
            let mut a = confirming.clone();
            a["preview_token"] = json!("pv-1");
            a
        }),
        call(3, "submit_order", {
            let mut a = confirming.clone();
            a["preview_token"] = json!("pv-1");
            a
        }),
    ]);
    let text = twice[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("unknown or already used"), "a token fires at most once, got: {text}");
}

#[test]
fn a_read_only_session_omits_every_write_tool_from_tools_list() {
    // ⚠ Over the WIRE, and asserted as a set rather than a count: an agent's whole picture of this
    // server is the `tools/list` it reads once at startup, so "the roster it was handed" is the
    // property, not "the router would have refused".
    let listed = transcript_under(
        "read-only",
        &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} })],
    );
    let names: Vec<&str> = listed[0]["result"]["tools"]
        .as_array()
        .expect("tools/list is an array")
        .iter()
        .map(|t| t["name"].as_str().expect("every tool is named"))
        .collect();
    for tool in vike_cli::cmd::mcp::WRITE_TOOLS {
        assert!(!names.contains(&tool), "{tool} must not be advertised under read-only: {names:?}");
    }
    // …and the reads are all still there, which is what makes the assertion above about the WRITE
    // roster rather than about an empty answer.
    for tool in ["validate_strategy", "run_backtest", "node_snapshot"] {
        assert!(names.contains(&tool), "{tool} must still be served under read-only: {names:?}");
    }

    // The default is untouched: every write tool is advertised exactly as before the flag existed.
    let full =
        transcript(&[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} })]);
    let full_names: Vec<&str> = full[0]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for tool in vike_cli::cmd::mcp::WRITE_TOOLS {
        assert!(full_names.contains(&tool), "{tool} must be advertised by default: {full_names:?}");
    }
}

#[test]
fn a_read_only_session_refuses_every_write_call_and_names_the_profile() {
    // The router half, for the WHOLE roster rather than for `submit_order` alone — the same reason
    // the confirm-alone loop above walks it: a venue verb added to the write set and not covered
    // here would be an unexercised gate.
    for (i, tool) in vike_cli::cmd::mcp::WRITE_TOOLS.iter().enumerate() {
        let id = i as i64 + 1;
        let mut args = every_write_tools_arguments();
        args["confirm"] = json!(true);
        let responses = transcript_under("read-only", &[call(id, tool, args)]);
        assert_eq!(responses[0]["result"]["isError"], true, "{tool}: got {}", responses[0]);
        let text = responses[0]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("read-only"), "{tool}: the refusal must name the profile: {text}");
        assert!(text.contains("--profile full"), "{tool}: …and how to change it: {text}");
        // ⚠ NOT a preview. A refused write must not hand back a token that would confirm it —
        // that would be the gate advertising its own bypass.
        assert!(
            responses[0]["result"]["structuredContent"].is_null(),
            "{tool}: a refusal is not a preview: {}",
            responses[0]
        );
    }
}

#[test]
fn a_read_tool_needs_no_token_at_all() {
    // The gate must not have leaked onto the read side: `list_templates` is offline and takes no
    // confirmation, and an agent asked to confirm a read would look for a token that is never
    // issued. This is the non-vacuity check on the loop above — it proves the assertions there are
    // about the WRITE roster, not about every tool this server has.
    let responses = transcript(&[call(1, "list_templates", json!({}))]);
    let body = structured(&responses[0]);
    assert!(body["templates"].is_array(), "got {body}");
    assert!(body["preview_token"].is_null(), "a read must issue no token: {body}");
}
