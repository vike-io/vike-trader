//! The MCP transport, driven end to end over real byte streams.
//!
//! Every other MCP test calls `Server::handle` or `Server::call_tool` IN PROCESS. This one writes
//! newline-delimited JSON-RPC into `vike_mcp::serve` and parses what comes back out, so
//! the FRAMING is covered too — the one-response-per-request rule, the parse-error envelope, and
//! the SDK under them. `serve`'s own doc comment has said "split from `run` so tests drive it" since
//! it was written, and until this file existed no test drove it.
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
//! (`vike_mcp::test_server`), so nothing here can reach a venue even if the gate failed
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
//! (`vike_mcp::test_server_under`). Both halves are asserted over the wire because they
//! are two different promises to the client and a unit test of either alone would not notice the
//! other break: `tools/list` must OMIT the write tools — the roster is the agent's entire picture
//! of this server — and `tools/call` must REFUSE one by name, with the profile in the message, and
//! without handing back a preview token that would confirm it.

use std::io::Cursor;

use serde_json::{Value, json};
// The union of every write tool's arguments (its doc says why a union and never a per-tool
// table); this transcript passes its own order client id, `c-transcript`.
use crate::write_args::every_write_tools_arguments;

/// Drive one session: feed these request lines, return the parsed response lines.
///
/// One `serve` call per transcript, so each one is its own PROCESS-equivalent — a fresh
/// `PendingPreviews` store, which is what lets the cross-session assertion below mean anything.
fn transcript(lines: &[Value]) -> Vec<Value> {
    drive(vike_mcp::test_server(), lines)
}

/// The same, on a server SCOPED to one tool profile — the `--profile <name>` launch, over the real
/// transport rather than through the in-process router.
fn transcript_under(profile: &str, lines: &[Value]) -> Vec<Value> {
    drive(vike_mcp::test_server_under(profile), lines)
}

fn drive(mut server: vike_mcp::Server, lines: &[Value]) -> Vec<Value> {
    let input = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
    let mut out = Vec::new();
    vike_mcp::serve(Cursor::new(input), &mut out, &mut server)
        .expect("serve drains the reader without an io error");
    String::from_utf8(out)
        .expect("the server writes UTF-8")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("each line is one JSON-RPC message"))
        .collect()
}

/// What a Windows client's pipe really carries, measured from `vike-mcp.exe`: CRLF between the lines
/// and, from Windows PowerShell 5.1 under `$OutputEncoding = [Text.Encoding]::UTF8`, a byte order mark
/// ahead of the first one, or two when its console is UTF-8 as well. The mark cost that client its
/// `initialize` (`-32700`, the session then carrying on as if nothing had been asked). What goes back
/// is LF only, whatever came in.
#[test]
fn a_windows_pipes_crlf_and_byte_order_mark_are_read_past() {
    let ping = |id: i64| json!({ "jsonrpc": "2.0", "id": id, "method": "ping" }).to_string();
    for marks in ["\u{feff}", "\u{feff}\u{feff}"] {
        let input = format!("{marks}{}\r\n{}\r\n", ping(1), ping(2));
        let mut out = Vec::new();
        vike_mcp::serve(Cursor::new(input), &mut out, &mut vike_mcp::test_server())
            .expect("serve drains the reader without an io error");
        let out = String::from_utf8(out).expect("the server writes UTF-8");
        assert!(!out.contains('\r'), "{out:?}");
        let answers: Vec<Value> =
            out.lines().map(|l| serde_json::from_str(l).expect("JSON")).collect();
        assert_eq!(
            answers,
            [
                json!({ "jsonrpc": "2.0", "id": 1, "result": {} }),
                json!({ "jsonrpc": "2.0", "id": 2, "result": {} }),
            ]
        );
    }
}

/// One entry, both generations of the protocol, each session in the shape its generation's client
/// really sends. What the swap to the SDK is for.
#[test]
fn both_protocol_generations_are_served_by_this_entry() {
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "generations", "version": "0" },
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    let with_meta = |id: i64, method: &str, mut params: Value| {
        params["_meta"] = meta.clone();
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    };
    let tools_list = |id: i64| json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list" });
    let ping = |id: i64| json!({ "jsonrpc": "2.0", "id": id, "method": "ping" });
    let open = |id: i64, version: &str| {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {
                "protocolVersion": version, "capabilities": {},
                "clientInfo": { "name": "generations", "version": "0" }
            }
        })
    };

    // Up to 2025-11-25: a handshake, then requests that carry nothing.
    let older = transcript(&[
        open(1, "2025-11-25"),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        tools_list(2),
        call(3, "list_templates", json!({})),
        ping(4),
    ]);
    assert_eq!(older.len(), 4, "{older:?}");
    assert_eq!(older[0]["result"]["protocolVersion"], "2025-11-25", "{}", older[0]);
    assert!(older[1]["result"].get("resultType").is_none(), "the older shape: {}", older[1]);
    assert!(structured(&older[2])["templates"].is_array());
    assert_eq!(older[3]["result"], json!({}));

    // 2026-07-28: no handshake, and every request says who is asking.
    let newer = transcript(&[
        with_meta(1, "server/discover", json!({})),
        with_meta(2, "tools/list", json!({})),
        with_meta(3, "tools/call", json!({ "name": "list_templates", "arguments": {} })),
        with_meta(4, "ping", json!({})),
    ]);
    assert_eq!(newer.len(), 4, "{newer:?}");
    let versions = newer[0]["result"]["supportedVersions"].as_array().expect("discover answers");
    assert!(versions.contains(&json!("2026-07-28")), "{versions:?}");
    assert_eq!(newer[1]["result"]["resultType"], "complete", "{}", newer[1]);
    assert_eq!(newer[1]["result"]["tools"], older[1]["result"]["tools"], "one roster for both");
    assert_eq!(newer[2]["result"]["resultType"], "complete", "{}", newer[2]);
    assert!(structured(&newer[2])["templates"].is_array());
    assert_eq!(newer[3]["error"]["code"], -32601, "ping is gone in this generation");

    // A discover is a probe: a client that reads it and then opens with `initialize` is served too.
    let probed = transcript(&[
        with_meta(1, "server/discover", json!({})),
        open(2, "2025-11-25"),
        tools_list(3),
    ]);
    assert_eq!(probed.len(), 3, "{probed:?}");
    assert_eq!(probed[1]["result"]["protocolVersion"], "2025-11-25", "{}", probed[1]);
    assert!(probed[2]["result"]["tools"].is_array(), "{}", probed[2]);

    // ⚠ `initialize` naming a revision with no handshake: the older one is what it AGREES to, and
    // the session is then that older one, `ping` and the older result shape included. The SDK's
    // default handler records what was asked for instead, so this is the override's pin.
    for opening in [
        open(1, "2026-07-28"),
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2026-07-28" }
        }),
    ] {
        let downgraded = transcript(&[opening, tools_list(2), ping(3)]);
        assert_eq!(downgraded.len(), 3, "{downgraded:?}");
        assert_eq!(downgraded[0]["result"]["protocolVersion"], "2025-11-25", "{}", downgraded[0]);
        assert!(downgraded[1]["result"].get("resultType").is_none(), "{}", downgraded[1]);
        assert_eq!(downgraded[2]["result"], json!({}), "{}", downgraded[2]);
    }
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
    let mut server = vike_mcp::test_server();
    vike_mcp::serve(Cursor::new(input), &mut out, &mut server).unwrap();
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
    for (i, tool) in vike_mcp::WRITE_TOOLS.iter().enumerate() {
        let id = i as i64 + 1;
        let mut args = every_write_tools_arguments("c-transcript");
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
        text.contains("no vike-tradehub configured"),
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
    for tool in vike_mcp::WRITE_TOOLS {
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
    for tool in vike_mcp::WRITE_TOOLS {
        assert!(full_names.contains(&tool), "{tool} must be advertised by default: {full_names:?}");
    }
}

#[test]
fn a_read_only_session_refuses_every_write_call_and_names_the_profile() {
    // The router half, for the WHOLE roster rather than for `submit_order` alone — the same reason
    // the confirm-alone loop above walks it: a venue verb added to the write set and not covered
    // here would be an unexercised gate.
    for (i, tool) in vike_mcp::WRITE_TOOLS.iter().enumerate() {
        let id = i as i64 + 1;
        let mut args = every_write_tools_arguments("c-transcript");
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
