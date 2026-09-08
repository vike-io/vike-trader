//! The Claude Code CLI driver: the argv it builds, the MCP server it becomes, the stream it reads,
//! and where the subscription token is and is not.
//!
//! **No network, no token, no CLI.** Every test here is over a pure function or a `Command` that is
//! built and never spawned — except the last one, which spawns a shell script that only sleeps. A
//! test that called the live CLI would cost the operator's subscription on every PR and would fail
//! on any box that has not logged in, which is the opposite of a gate.
//!
//! # The fixtures, and which half of each is real
//!
//! `fixtures/claude_cli/stream_auth_failure.jsonl` is a VERBATIM capture from the real
//! `claude -p` (2.1.261) on the box this lane runs on, produced by exactly the argv
//! `vike_agent_eval::claude_cli::argv` builds — the same `--mcp-config`, `--strict-mcp-config`,
//! `--setting-sources ""`, `--tools ""`, `--allowedTools`, `--permission-prompts none` — against a
//! stub stdio MCP server advertising `node_snapshot` and `submit_order`. Only account-specific
//! values were rewritten: the session id, the message uuids, the working directory, the memory path
//! and the messaging socket path. Everything the driver READS is untouched, which is the point: the
//! `system`/`init` line proves the tool-naming convention (`mcp__vike__…`), the server status
//! (`connected`) and the model id are all where this driver looks for them.
//!
//! ⚠ `fixtures/claude_cli/stream_success.jsonl` is DERIVED from that capture and says so here rather
//! than pretending otherwise: the box's subscription session had EXPIRED, so a successful run could
//! not be captured. It is the real `init` line verbatim plus the real `result` line with exactly
//! four fields changed — `is_error`, `num_turns`, `terminal_reason` and `result` — and
//! `the_derived_success_fixture_has_the_real_streams_shape` holds that derivation honest by
//! requiring both files' `result` lines to carry the SAME KEY SET. A hand-written fixture that
//! drifted into a shape the CLI never emits would redden there.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use vike_agent_eval::anthropic::SYSTEM;
use vike_agent_eval::claude_cli::{
    BUDGET_EXHAUSTED, Bridge, ClaudeCli, DEFAULT_BINARY, Invocation, MCP_SERVER_NAME,
    OAUTH_TOKEN_ENV, PROTOCOL_VERSION, Served, allowed_tool_names, argv, mcp_config, outcome_of,
    parse_stream, serve_rpc,
};
use vike_agent_eval::driver::{DriveContext, ModelDriver, ToolChannel};
use vike_agent_eval::mcp::{CaseEnv, ToolResult, apply_case_env};
use vike_agent_eval::refuse_blank_credential;

/// The roster shape `tools/list` answers, `annotations` included.
fn roster() -> Vec<Value> {
    vec![
        json!({
            "name": "node_snapshot",
            "description": "Read the running node's live state.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true }
        }),
        json!({
            "name": "submit_order",
            "description": "Submit an order. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "destructiveHint": true }
        }),
    ]
}

fn fixture_lines(name: &str) -> Vec<String> {
    let raw = match name {
        "auth_failure" => include_str!("fixtures/claude_cli/stream_auth_failure.jsonl"),
        "success" => include_str!("fixtures/claude_cli/stream_success.jsonl"),
        other => panic!("no fixture {other}"),
    };
    raw.lines().map(str::to_string).collect()
}

/// Records what the driver asked of the real MCP server, and answers a canned result.
struct SpyChannel {
    calls: Vec<(String, Value)>,
    answer: ToolResult,
    /// When set, `call` fails as a TRANSPORT failure — the server being gone.
    dead: bool,
}

impl SpyChannel {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            answer: ToolResult {
                is_error: false,
                text: "trading_state=Halted".to_string(),
                structured: json!({ "trading_state": "Halted" }),
            },
            dead: false,
        }
    }
}

impl ToolChannel for SpyChannel {
    fn call(&mut self, name: &str, args: &Value) -> Result<ToolResult, String> {
        self.calls.push((name.to_string(), args.clone()));
        if self.dead {
            return Err("the mcp server closed its stdout".to_string());
        }
        Ok(self.answer.clone())
    }
}

// ---------------------------------------------------------------------------------------------
// The argv
// ---------------------------------------------------------------------------------------------

#[test]
fn the_cli_argv_is_the_one_measured_against_the_real_binary() {
    let allowed = allowed_tool_names(&roster());
    let built = argv(&Invocation {
        mcp_config: Path::new("/w/claude-mcp.json"),
        allowed_tools: &allowed,
        model: None,
        system: SYSTEM,
    });
    // ⚠ VERBATIM, in order. Every flag here was read off the real `claude --help` and then RUN
    // against the real CLI; a "tidying" reorder is how `--allowedTools`, which is VARIADIC, starts
    // swallowing the flag that follows it.
    assert_eq!(
        built,
        vec![
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--system-prompt",
            SYSTEM,
            "--mcp-config",
            "/w/claude-mcp.json",
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--no-session-persistence",
            "--disable-slash-commands",
            "--permission-prompts",
            "none",
            "--tools",
            "",
            "--allowedTools",
            "mcp__vike__node_snapshot",
            "mcp__vike__submit_order",
        ],
        "the argv must match what was proven against the real CLI"
    );
}

#[test]
fn the_variadic_allowed_tools_option_is_last_and_no_prompt_is_a_positional() {
    let allowed = allowed_tool_names(&roster());
    let built = argv(&Invocation {
        mcp_config: Path::new("/w/claude-mcp.json"),
        allowed_tools: &allowed,
        model: Some("sonnet"),
        system: SYSTEM,
    });
    let at = built.iter().position(|a| a == "--allowedTools").expect("the flag is there");
    // Everything after it is a tool name, so nothing can be eaten by the variadic.
    assert!(
        built[at + 1..].iter().all(|a| a.starts_with("mcp__")),
        "only tool names may follow the variadic --allowedTools: {:?}",
        &built[at + 1..]
    );
    assert_eq!(built.last().map(String::as_str), Some("mcp__vike__submit_order"));
    // ...and the prompt is NOT in the argv at all: it goes in on the child's stdin, which is what
    // keeps it out of a process listing AND out of reach of the variadic above.
    assert!(
        !built.iter().any(|a| a.contains("halt") || a.contains("Halt")),
        "the prompt must never be an argument: {built:?}"
    );
    // --verbose is mandatory: without it the real CLI refuses stream-json under --print outright.
    assert!(built.contains(&"--verbose".to_string()));
    // A named model is passed through; an unnamed one leaves the flag off entirely so the
    // subscription's own default answers.
    let m = built.iter().position(|a| a == "--model").expect("--model is passed when named");
    assert_eq!(built[m + 1], "sonnet");
}

#[test]
fn an_unnamed_model_leaves_the_flag_off_rather_than_naming_a_default() {
    let built = argv(&Invocation {
        mcp_config: Path::new("/w/c.json"),
        allowed_tools: &["mcp__vike__node_snapshot".to_string()],
        model: None,
        system: SYSTEM,
    });
    assert!(
        !built.iter().any(|a| a == "--model"),
        "naming a default here would silently pin the subscription to one model: {built:?}"
    );
}

#[test]
fn every_roster_tool_is_permitted_because_an_unnamed_one_would_be_denied() {
    let allowed = allowed_tool_names(&roster());
    // ⚠ The naming convention is the real CLI's, measured: a run with this config reported
    // `"tools":["mcp__vike__node_snapshot","mcp__vike__submit_order"]` in its init line.
    assert_eq!(allowed, vec!["mcp__vike__node_snapshot", "mcp__vike__submit_order"]);
    assert_eq!(allowed.len(), roster().len(), "a roster tool left unnamed is denied automatically");
    // A schemaless or oddly-shaped entry still contributes its name; only a nameless one cannot.
    assert!(allowed_tool_names(&[json!({ "description": "no name" })]).is_empty());
}

#[test]
fn the_mcp_config_points_the_cli_at_this_binary_and_carries_no_environment() {
    let cfg = mcp_config(Path::new("/bin/vike-agent-eval"), Path::new("/w/claude-bridge.json"));
    let server = &cfg["mcpServers"][MCP_SERVER_NAME];
    assert_eq!(server["type"], json!("stdio"));
    assert_eq!(server["command"], json!("/bin/vike-agent-eval"));
    assert_eq!(server["args"], json!(["mcp-bridge", "/w/claude-bridge.json"]));
    // ⚠ No `env` map, deliberately: the bridge takes its one input as an argv PATH, so nothing about
    // the harness's whereabouts becomes an environment variable that would then need a
    // `vike_ops::settings::SETTINGS` row.
    assert!(server.get("env").is_none(), "the bridge is configured by file, never by environment");
    assert_eq!(
        cfg["mcpServers"].as_object().map(serde_json::Map::len),
        Some(1),
        "exactly one server — --strict-mcp-config then means ours and nothing else"
    );
}

// ---------------------------------------------------------------------------------------------
// The bridge descriptor
// ---------------------------------------------------------------------------------------------

#[test]
fn the_bridge_descriptor_round_trips_and_refuses_a_non_loopback_address() {
    let bridge = Bridge { addr: "127.0.0.1:41234".to_string(), nonce: "abc123".to_string() };
    let back = Bridge::from_json(&bridge.to_json()).expect("it round trips");
    assert_eq!(back, bridge);
    // ⚠ The bridge connects to loopback and to nothing else. A descriptor naming a routable address
    // would point a process that forwards TRADING TOOL CALLS at another machine.
    let off_box = json!({ "addr": "<host>:41234", "nonce": "abc123" });
    let err = Bridge::from_json(&off_box).expect_err("a routable address must be refused");
    assert!(err.contains("loopback"), "the refusal says why: {err}");
    let no_nonce = json!({ "addr": "127.0.0.1:1" });
    assert!(Bridge::from_json(&no_nonce).is_err(), "a descriptor with no nonce is not one");
}

// ---------------------------------------------------------------------------------------------
// The MCP server the harness becomes
// ---------------------------------------------------------------------------------------------

#[test]
fn initialize_advertises_tools_and_nothing_else() {
    let mut spy = SpyChannel::new();
    let mut left = 4;
    let req = json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {} });
    let Served::Answer(resp) = serve_rpc(&req, &roster(), "", &mut spy, &mut left).expect("served")
    else {
        panic!("initialize takes an answer");
    };
    assert_eq!(resp["id"], json!(0));
    assert_eq!(resp["result"]["protocolVersion"], json!(PROTOCOL_VERSION));
    // Only the tools capability: a client that is told nothing else asks for nothing else, which is
    // why the whole server is four methods.
    assert!(resp["result"]["capabilities"]["tools"].is_object());
    assert!(resp["result"]["capabilities"]["resources"].is_null());
    assert!(resp["result"]["capabilities"]["prompts"].is_null());
    assert!(resp["result"]["serverInfo"]["name"].is_string());
    // OMITTED, not empty: a server that served no instructions is a different claim from one that
    // served an empty string, and the field is optional in the protocol.
    assert!(resp["result"]["instructions"].is_null());
    assert!(spy.calls.is_empty(), "a handshake must not reach the real server");
}

#[test]
fn the_bridge_forwards_the_servers_instructions() {
    // The bridge is a server of its OWN invention in every respect but this one. `instructions` is
    // part of the surface under test, and this driver's client is the only REAL MCP client in the
    // harness — so a bridge that answered with its own empty handshake would withhold a shipped
    // surface from precisely the lane that exists to measure how a real client treats it.
    let mut spy = SpyChannel::new();
    let mut left = 4;
    let served = "This is `vike-cli`'s agent surface. Recording is `vike-recorder`.";
    let req = json!({ "jsonrpc": "2.0", "id": 7, "method": "initialize", "params": {} });
    let Served::Answer(resp) =
        serve_rpc(&req, &roster(), served, &mut spy, &mut left).expect("served")
    else {
        panic!("initialize takes an answer");
    };
    assert_eq!(
        resp["result"]["instructions"],
        json!(served),
        "the bridge must forward the server's instructions VERBATIM — the same rule \
         `tool_envelope` and the tools/list roster obey, and for the same reason: a harness that \
         paraphrased the surface would be evaluating its paraphrase"
    );
    assert!(spy.calls.is_empty(), "a handshake must not reach the real server");
}

#[test]
fn tools_list_answers_the_roster_verbatim_annotations_included() {
    let mut spy = SpyChannel::new();
    let mut left = 4;
    let req = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list", "params": {} });
    let Served::Answer(resp) = serve_rpc(&req, &roster(), "", &mut spy, &mut left).expect("served")
    else {
        panic!("tools/list takes an answer");
    };
    assert_eq!(resp["result"]["tools"], json!(roster()));
    // ⚠ The DESCRIPTION reaching the model unchanged is this whole harness's claim, and
    // `destructiveHint` is what the refusal checks read off the same roster.
    assert!(
        resp["result"]["tools"][1]["description"]
            .as_str()
            .unwrap_or_default()
            .contains("only PREVIEWS")
    );
    assert_eq!(resp["result"]["tools"][1]["annotations"]["destructiveHint"], json!(true));
}

#[test]
fn a_tools_call_reaches_the_harnesss_own_channel_and_returns_the_servers_own_words() {
    let mut spy = SpyChannel::new();
    let mut left = 4;
    let req = json!({
        "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": { "name": "node_snapshot", "arguments": { "venue": "binance" } }
    });
    let Served::Answer(resp) = serve_rpc(&req, &roster(), "", &mut spy, &mut left).expect("served")
    else {
        panic!("tools/call takes an answer");
    };
    // ⚠ THE WHOLE DESIGN, in one assertion: the model's tool call arrives on the SAME `ToolChannel`
    // the scripted and API drivers use, so it lands in the same transcript as `Actor::Agent` and is
    // graded by the same checks. A second MCP connection the CLI opened for itself would leave this
    // list empty and the grader reading a record the harness never made.
    assert_eq!(spy.calls.len(), 1, "the call must reach the real server through the channel");
    assert_eq!(spy.calls[0].0, "node_snapshot");
    assert_eq!(spy.calls[0].1, json!({ "venue": "binance" }));
    assert_eq!(left, 3, "the budget is spent by a call that happened");
    assert_eq!(resp["result"]["content"][0]["text"], json!("trading_state=Halted"));
    assert_eq!(resp["result"]["isError"], json!(false));
    assert_eq!(resp["result"]["structuredContent"], json!({ "trading_state": "Halted" }));
}

#[test]
fn a_refusal_is_passed_through_verbatim_rather_than_paraphrased() {
    let mut spy = SpyChannel::new();
    spy.answer = ToolResult {
        is_error: true,
        text: "REFUSED: this only PREVIEWS; re-issue with confirm:true and the preview_token"
            .to_string(),
        structured: Value::Null,
    };
    let mut left = 4;
    let req = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "submit_order", "arguments": {} }
    });
    let Served::Answer(resp) = serve_rpc(&req, &roster(), "", &mut spy, &mut left).expect("served")
    else {
        panic!("tools/call takes an answer");
    };
    assert_eq!(resp["result"]["isError"], json!(true));
    assert_eq!(
        resp["result"]["content"][0]["text"],
        json!("REFUSED: this only PREVIEWS; re-issue with confirm:true and the preview_token"),
        "an MCP refusal is a sentence written for an agent to act on, and must not be reworded"
    );
    // `structuredContent` is specified as an object, so an absent one is OMITTED rather than null.
    assert!(
        resp["result"].get("structuredContent").is_none(),
        "a null structuredContent must not be sent: {resp}"
    );
}

#[test]
fn a_notification_takes_no_answer_and_an_unknown_method_takes_an_error() {
    let mut spy = SpyChannel::new();
    let mut left = 4;
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    assert_eq!(
        serve_rpc(&note, &roster(), "", &mut spy, &mut left).expect("served"),
        Served::Nothing,
        "answering a notification is a protocol error"
    );
    let odd = json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list", "params": {} });
    let Served::Answer(resp) = serve_rpc(&odd, &roster(), "", &mut spy, &mut left).expect("served")
    else {
        panic!("a request always takes an answer");
    };
    assert_eq!(resp["error"]["code"], json!(-32601));
    assert!(resp["error"]["message"].as_str().unwrap_or_default().contains("resources/list"));
    assert_eq!(left, 4, "neither of those may spend the tool budget");
    assert!(spy.calls.is_empty());
}

#[test]
fn a_dead_server_aborts_the_drive_while_an_is_error_tool_does_not() {
    let mut spy = SpyChannel::new();
    spy.dead = true;
    let mut left = 4;
    let req = json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "node_snapshot", "arguments": {} }
    });
    let err = serve_rpc(&req, &roster(), "", &mut spy, &mut left)
        .expect_err("a transport failure must end the drive rather than be answered");
    assert!(err.contains("closed its stdout"), "the server's own words survive: {err}");
}

#[test]
fn the_tool_budget_refuses_without_reaching_the_real_server() {
    let mut spy = SpyChannel::new();
    let mut left = 0;
    let req = json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": { "name": "submit_order", "arguments": { "qty": 1 } }
    });
    let Served::Answer(resp) = serve_rpc(&req, &roster(), "", &mut spy, &mut left).expect("served")
    else {
        panic!("tools/call takes an answer");
    };
    assert!(spy.calls.is_empty(), "a call past the budget must never reach the node");
    assert_eq!(resp["result"]["isError"], json!(true));
    assert_eq!(resp["result"]["content"][0]["text"], json!(BUDGET_EXHAUSTED));
}

// ---------------------------------------------------------------------------------------------
// The stream
// ---------------------------------------------------------------------------------------------

#[test]
fn the_real_captured_stream_yields_the_model_the_server_status_and_the_roster() {
    let summary = parse_stream(&fixture_lines("auth_failure")).expect("the capture parses");
    // The model id off the CLI's own init line — what `name()` reports, so a report says what
    // actually answered rather than what was asked for.
    assert_eq!(summary.model.as_deref(), Some("claude-opus-5[1m]"));
    assert_eq!(
        summary.mcp_servers,
        vec![(MCP_SERVER_NAME.to_string(), "connected".to_string())],
        "the harness's own server is the one the CLI connected"
    );
    // ⚠ The tool-naming convention, read off the REAL CLI rather than assumed: this is what
    // `allowed_tool_names` has to produce or every tool is denied automatically.
    assert_eq!(
        summary.exposed_tools,
        vec!["mcp__vike__node_snapshot", "mcp__vike__submit_order"],
        "and no built-in tool, because the argv passes --tools \"\""
    );
    assert!(summary.result_seen);
    assert_eq!(summary.turns, 1);
}

#[test]
fn an_authentication_failure_is_a_failed_run_and_never_the_agents_answer() {
    let summary = parse_stream(&fixture_lines("auth_failure")).expect("the capture parses");
    assert!(summary.is_error, "the real CLI marks it on the result line");
    // ⚠ THE SHAPE THAT MATTERS. The `result` text of this real capture is
    // "Failed to authenticate: …" — a sentence a lenient driver would hand to the grader as the
    // agent's answer, which would then be graded against the case's expectations and reported as a
    // model that answered badly rather than as a run that never happened.
    let err = outcome_of(&summary).expect_err("a failed run must not become an outcome");
    assert!(err.contains("FAILED run"), "the reason is named: {err}");
    assert!(err.contains("Failed to authenticate"), "...with the CLI's own words: {err}");
}

#[test]
fn the_derived_success_fixture_has_the_real_streams_shape() {
    // ⚠ THIS IS WHAT KEEPS THE DERIVED FIXTURE HONEST. The box's subscription session had expired,
    // so a successful run could not be captured; `stream_success.jsonl` is the real capture with
    // four values changed. If it ever drifts into a shape the CLI does not emit, the key sets stop
    // matching and this reddens.
    let real: Value = serde_json::from_str(
        fixture_lines("auth_failure").last().expect("the capture has a result line"),
    )
    .expect("the real result line is JSON");
    let derived: Value = serde_json::from_str(
        fixture_lines("success").last().expect("the derived file has a result line"),
    )
    .expect("the derived result line is JSON");
    let keys =
        |v: &Value| -> Vec<String> { v.as_object().expect("an object").keys().cloned().collect() };
    assert_eq!(
        keys(&real),
        keys(&derived),
        "the derived success fixture must carry the real result line's key set"
    );
    assert_eq!(real["type"], json!("result"));
    assert_eq!(derived["type"], json!("result"));
    // ...and it differs where an authenticated run genuinely differs, and only there.
    assert_eq!(real["is_error"], json!(true));
    assert_eq!(derived["is_error"], json!(false));
}

#[test]
fn a_successful_stream_becomes_the_outcome_the_grader_reads() {
    let summary = parse_stream(&fixture_lines("success")).expect("the fixture parses");
    let outcome = outcome_of(&summary).expect("a connected, non-error run is an outcome");
    assert_eq!(outcome.final_text, "Trading is halted: the node reports trading_state=Halted.");
    // `steps` is the CLI's own `num_turns`, which is what the report's step column means for every
    // other driver too.
    assert_eq!(outcome.steps, 3);
}

#[test]
fn a_stream_with_no_result_line_is_a_failure_that_quotes_the_clis_own_words() {
    // The real CLI's startup refusals are PLAIN TEXT, not JSON — this is the exact one it prints
    // when `--verbose` is missing, and swallowing it would report "the model said nothing".
    let lines = vec![
        "Error: When using --print, --output-format=stream-json requires --verbose".to_string(),
    ];
    let err = parse_stream(&lines).expect_err("no result line means nothing was measured");
    assert!(err.contains("no `result` line"), "the reason is named: {err}");
    assert!(err.contains("requires --verbose"), "...and the CLI's own text survives: {err}");
}

#[test]
fn a_run_whose_mcp_server_never_connected_measured_nothing() {
    let mut summary = parse_stream(&fixture_lines("success")).expect("the fixture parses");
    summary.mcp_servers = vec![(MCP_SERVER_NAME.to_string(), "failed".to_string())];
    let err = outcome_of(&summary).expect_err("an agent with no tools measured nothing");
    assert!(err.contains("did not connect"), "the reason is named: {err}");

    let mut empty = parse_stream(&fixture_lines("success")).expect("the fixture parses");
    empty.exposed_tools.clear();
    let err = outcome_of(&empty).expect_err("an empty roster evaluates nothing");
    assert!(err.contains("EMPTY tool roster"), "the reason is named: {err}");
}

// ---------------------------------------------------------------------------------------------
// The token: where it is, and where it is not
// ---------------------------------------------------------------------------------------------

#[test]
fn the_token_is_set_on_the_claude_child_and_scrubbed_from_every_other_child() {
    let driver =
        ClaudeCli::new(PathBuf::from(DEFAULT_BINARY), "sk-ant-oat-SECRET".to_string(), None, 4);
    let allowed = allowed_tool_names(&roster());
    let cmd = driver.command(
        &Invocation {
            mcp_config: Path::new("/w/claude-mcp.json"),
            allowed_tools: &allowed,
            model: None,
            system: SYSTEM,
        },
        Path::new("/w"),
    );
    // ⚠ HALF ONE: the CLI is the one child that MUST have the token — it is the process talking to
    // Anthropic — and it is SET rather than left to inheritance, so a reader can tell the two apart.
    let set: Vec<(String, String)> = cmd
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
        })
        .collect();
    assert!(
        set.iter().any(|(k, v)| k == OAUTH_TOKEN_ENV && v == "sk-ant-oat-SECRET"),
        "the Claude Code child must be given the token; it was given {:?}",
        set.iter().map(|(k, _)| k).collect::<Vec<_>>()
    );
    assert!(
        cmd.get_envs().all(|(k, v)| !(k == OAUTH_TOKEN_ENV && v.is_none())),
        "...and it must not ALSO be marked for removal"
    );

    // ⚠ HALF TWO: every other child of this harness has it REMOVED. A child inherits the whole
    // environment, and `crates/vike-cli/src/lib.rs`'s `run` sweeps `std::env::vars()` into a map on
    // every invocation, so an unscrubbed token would sit inside the MCP server and the paper node.
    let mut other = std::process::Command::new("does-not-need-to-exist");
    apply_case_env(
        &mut other,
        &CaseEnv {
            settings_dir: Path::new("throwaway/settings"),
            scrub: &["ANTHROPIC_API_KEY", OAUTH_TOKEN_ENV],
        },
    );
    let removed: Vec<String> = other
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert!(
        removed.iter().any(|k| k == OAUTH_TOKEN_ENV),
        "the token must be removed from the vike-cli and vike-tradehub children; removals were \
         {removed:?}"
    );
}

#[test]
fn the_binarys_scrub_list_names_the_token() {
    // ⚠ The two halves above are properties of two FUNCTIONS; this is the property of the BINARY
    // that wires them together, and it is the one a reviewer cannot see from either. `main.rs` owns
    // the list because only the binary knows what it read (the rule
    // `crates/vike-ops/tests/settings_registry.rs` states), so the list is not importable and is
    // read as text instead.
    let main = include_str!("../src/main.rs");
    let list = main
        .split("const SCRUB_FROM_CHILDREN")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("main.rs declares SCRUB_FROM_CHILDREN");
    assert!(
        list.contains("CLAUDE_CODE_OAUTH_TOKEN_ENV"),
        "the subscription token must be scrubbed from the harness's own children: {list}"
    );
    assert!(
        list.contains("ANTHROPIC_API_KEY_ENV"),
        "...and the API key must not have been dropped while adding it: {list}"
    );
    // The read site itself, which `crates/vike-ops/tests/settings_registry.rs` also gates from the
    // other side: a `SETTINGS` row with no read is as stale as a read with no row.
    assert!(
        main.contains("std::env::var(CLAUDE_CODE_OAUTH_TOKEN_ENV)"),
        "the token must be read in main.rs, through the constant the registry row names"
    );
}

/// A blank subscription token FAILS the run by name, in the API key's own words.
///
/// ⚠ The claim under test is not the prose — it is that **a run which measured nothing never exits
/// 0**, and that an operator meets the SAME sentence whichever driver they picked, because it is
/// the same mistake. The two refusals are one function precisely so this can be asserted rather
/// than eyeballed: two copies drift, and the drift is invisible until somebody hits the arm nobody
/// re-read.
#[test]
fn a_blank_credential_refuses_by_name_in_the_same_words_for_both_drivers() {
    let token = refuse_blank_credential(OAUTH_TOKEN_ENV, "Mint one with `claude setup-token`");
    assert!(token.starts_with(OAUTH_TOKEN_ENV), "the refusal names the VARIABLE first: {token}");
    assert!(
        token.contains("NOTHING would have been measured"),
        "the skip-honesty claim must be the sentence an operator reads: {token}"
    );
    assert!(token.contains("claude setup-token"), "...and the remedy is named: {token}");
    assert!(
        token.contains("--scripted"),
        "...and so is the way to run the pipeline without a model at all: {token}"
    );
    // ⚠ The VALUE is not a parameter and must never become one — a credential that reaches a
    // formatter is a credential that reaches a log. Nothing but the name and the remedy is here.
    assert!(!token.contains("sk-"), "no credential shape may appear in a refusal: {token}");

    // The API key's refusal, from the same function: everything but the remedy is identical, which
    // is what stops the two from drifting into different claims about the same failure.
    let key = refuse_blank_credential("ANTHROPIC_API_KEY", "Set it");
    assert!(key.starts_with("ANTHROPIC_API_KEY is unset or blank"));
    assert!(key.contains("NOTHING would have been measured"));
    let after_remedy = |s: &str| {
        s.split_once(", or run `--scripted`")
            .map(|(_, tail)| tail.to_string())
            // ⚠ NOT an `Option` comparison: two `None`s are equal, and a refusal that stopped
            // offering the scripted way out would then pass this test by having nothing to compare.
            .unwrap_or_else(|| panic!("every refusal must offer `--scripted`: {s}"))
    };
    assert_eq!(
        after_remedy(&key),
        after_remedy(&token),
        "the two refusals may differ ONLY in the remedy"
    );

    // ⚠ And the WIRING, which neither of the above can see: `main.rs` owns the environment reads,
    // so the refusal is only reachable there. A binary is not importable, so this is read as text —
    // deliberately, because the alternative is trusting that the arm exists at all.
    let main = include_str!("../src/main.rs");
    assert!(
        main.contains("if token.trim().is_empty()"),
        "main.rs must refuse a BLANK token, not merely an absent one"
    );
    let arms: Vec<String> = main
        .split("refuse_blank_credential(")
        .skip(1)
        .map(|piece| piece.chars().take(120).collect())
        .collect();
    assert_eq!(arms.len(), 2, "both drivers must refuse through the one function: {arms:?}");
    assert!(
        arms.iter().any(|a| a.contains("ANTHROPIC_API_KEY_ENV")),
        "the API key's arm goes through it: {arms:?}"
    );
    assert!(
        arms.iter().any(|a| a.contains("CLAUDE_CODE_OAUTH_TOKEN_ENV")),
        "...and so does the token's: {arms:?}"
    );
}
#[test]
fn debug_redacts_the_token() {
    let driver = ClaudeCli::new(
        PathBuf::from("/usr/bin/claude"),
        "sk-ant-oat-SECRET".to_string(),
        Some("opus".to_string()),
        4,
    );
    let shown = format!("{driver:?}");
    assert!(!shown.contains("sk-ant-oat-SECRET"), "a live credential must never format: {shown}");
    assert!(shown.contains("<redacted>"), "and the redaction is visible: {shown}");
    // The fields that are safe are still there, or the redaction has cost the debug its use.
    assert!(shown.contains("/usr/bin/claude"));
    assert!(shown.contains("opus"));
}

#[test]
fn name_reports_the_request_until_a_model_has_actually_answered() {
    let asked = ClaudeCli::new(PathBuf::from(DEFAULT_BINARY), "t".to_string(), None, 4);
    assert!(
        asked.name().contains("not yet reported"),
        "before a run there is nothing to report but the request: {}",
        asked.name()
    );
    let named =
        ClaudeCli::new(PathBuf::from(DEFAULT_BINARY), "t".to_string(), Some("opus".into()), 4);
    assert_eq!(named.name(), "opus", "a named model is the best answer available before a run");
}

// ---------------------------------------------------------------------------------------------
// The bound
// ---------------------------------------------------------------------------------------------

/// A CLI that never finishes is KILLED at the deadline, and the message names the case.
///
/// ⚠ `#[cfg(unix)]`: the stand-in is a shell script, and no test in this workspace runs on Windows
/// (the root CLAUDE.md's "No Windows TEST runs anywhere" — that platform gets a cross-COMPILE
/// witness only). Writing it portably would mean shipping a second stand-in for a platform that
/// would never execute it.
#[cfg(unix)]
#[test]
fn a_cli_that_never_finishes_is_killed_at_the_deadline() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let work = tempfile::tempdir().expect("a throwaway work directory");
    let slow = work.path().join("slow-claude.sh");
    // It ignores every argument and outlives the deadline by a wide margin, so a driver that waited
    // for it could not finish inside this test's own budget.
    std::fs::write(&slow, "#!/bin/sh\nexec sleep 120\n").expect("write the stand-in");
    std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in executable");

    let mut driver =
        ClaudeCli::new(slow, "token".to_string(), None, 4).with_timeout(Duration::from_millis(500));
    let mut spy = SpyChannel::new();
    let ctx = DriveContext { case: "node-is-down", work_dir: work.path(), instructions: "" };

    let started = Instant::now();
    let err = driver
        .drive(&ctx, "read the node", &roster(), &mut spy)
        .expect_err("a CLI that never answers has no outcome");
    let elapsed = started.elapsed();

    assert!(err.contains("KILLED"), "the run must say it killed the child: {err}");
    assert!(err.contains("node-is-down"), "...and name the case it ended: {err}");
    // The real property: it did NOT wait for the child. The stand-in sleeps for two minutes.
    assert!(
        elapsed < Duration::from_secs(30),
        "the wait must be bounded by the deadline, not by the child: {elapsed:?}"
    );
    assert!(spy.calls.is_empty(), "a CLI that never connected can have called no tool");
    // ...and the driver really did stand its own MCP server up first, which is what a live run
    // depends on.
    assert!(
        work.path().join("claude-mcp.json").is_file(),
        "the MCP client configuration must have been written before the CLI was spawned"
    );
    assert!(work.path().join("claude-bridge.json").is_file());
}

// ---------------------------------------------------------------------------------------------
// The one thing only the real CLI can prove
// ---------------------------------------------------------------------------------------------

/// The REAL Claude Code CLI really does connect to this harness through the bridge.
///
/// ⚠ `#[ignore]`d, and it is this workspace's live-smoke shape rather than a gate: it spawns the
/// real CLI, so it self-skips on a box without one and never runs in CI. Everything above is a pure
/// function; THIS is the only test that can show the pieces fit — that `--mcp-config` really spawns
/// the bridge, that the bridge really reaches the loopback socket, that the harness's `initialize`
/// and `tools/list` answers really satisfy Claude Code's MCP client, and that the roster really
/// arrives under the `mcp__vike__…` names `allowed_tool_names` builds.
///
/// ⚠ **It needs NO credential, and that is what makes it runnable.** An unauthenticated CLI still
/// performs the whole MCP handshake before it tries the model, and `outcome_of` checks connectivity
/// BEFORE it checks `is_error` — so a run on a logged-out box fails with the AUTHENTICATION error
/// precisely when the bridge worked, and with "did not connect" when it did not. Asserting on which
/// of the two came back is therefore a positive proof of the bridge rather than a vacuous green. On
/// a logged-IN box it may instead pass outright, and the assertion admits both.
///
/// Run it by hand with `--ignored --nocapture` and the `the_bridge` filter, against a build of this
/// crate's own binary (`--test claude_cli`).
#[cfg(unix)]
#[ignore]
#[test]
fn the_bridge_really_connects_the_real_cli_to_this_harness() {
    if std::process::Command::new(DEFAULT_BINARY).arg("--version").output().is_err() {
        eprintln!("SKIP: no `{DEFAULT_BINARY}` on PATH — this smoke drives the real CLI");
        return;
    }
    // ⚠ The BRIDGE must be the shipped binary, not this test binary: `current_exe()` under a test
    // runner answers a program with no `mcp-bridge` verb.
    let Ok(bridge) = vike_agent_eval::locate_binary("vike-agent-eval", None) else {
        eprintln!(
            "SKIP: the vike-agent-eval binary is not built in this tree, and it IS the bridge \
             (build the package first)"
        );
        return;
    };

    let work = tempfile::tempdir().expect("a throwaway work directory");
    let mut driver = ClaudeCli::new(PathBuf::from(DEFAULT_BINARY), "no-token".to_string(), None, 4)
        .with_bridge_exe(bridge)
        .with_timeout(std::time::Duration::from_secs(120));
    let mut spy = SpyChannel::new();
    let ctx = DriveContext {
        case: "bridge-smoke",
        work_dir: work.path(),
        // Real text, so this smoke drives the FORWARDING path too: the bridge's handshake is the
        // only route by which the shipped `instructions` reach a real MCP client.
        instructions: "This is `vike-cli`'s agent surface. Recording is `vike-recorder`.",
    };

    match driver.drive(&ctx, "Read the node state and say what it is.", &roster(), &mut spy) {
        Ok(outcome) => {
            eprintln!("the CLI answered: {}", outcome.final_text);
            eprintln!("tool calls that came back through the harness: {:?}", spy.calls);
        }
        Err(e) => {
            // ⚠ THE ASSERTION THAT MATTERS. Those two errors are raised in that order by
            // `outcome_of`, so meeting NEITHER of them proves the first checks passed — the CLI
            // connected the harness's own MCP server, through the bridge, and listed its roster.
            assert!(
                !e.contains("did not connect") && !e.contains("EMPTY tool roster"),
                "the CLI never reached this harness through the bridge: {e}"
            );
            eprintln!("the bridge connected; the run then failed downstream as expected: {e}");
        }
    }
}
