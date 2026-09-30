//! The Messages API request and response shapes, against committed fixture JSON.
//!
//! No network, no key, no model. What is pinned is the TRANSLATION on either side of the loop — the
//! MCP roster becoming the API's `tools` array, and a model turn becoming the tool calls the harness
//! then executes. Both are places where a silent shape change turns every case into a false
//! negative that looks like a model getting worse: a roster the API rejects, or a `tool_use` block
//! read as an empty turn, would show up as "the agent called no tools" rather than as a harness bug.
//!
//! The fixtures are the response shapes as documented, trimmed to the fields this harness reads and
//! kept beside the test — they are what the loop is written against.

use serde_json::{Value, json};

use vike_agent_eval::anthropic::{
    API_VERSION, DEFAULT_MODEL, ENDPOINT, SYSTEM, parse_turn, request_body, system_for,
    tool_result_block, tools_for_api,
};

fn fixture(name: &str) -> Value {
    let raw = match name {
        "tool_use_turn" => include_str!("fixtures/tool_use_turn.json"),
        "end_turn" => include_str!("fixtures/end_turn.json"),
        "error" => include_str!("fixtures/error.json"),
        other => panic!("no fixture {other}"),
    };
    serde_json::from_str(raw).expect("the fixture is JSON")
}

/// The roster shape `tools/list` answers, including the two fields the API has no slot for.
fn mcp_roster() -> Vec<Value> {
    vec![
        json!({
            "name": "node_snapshot",
            "description": "Read the running node's live state.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        }),
        json!({
            "name": "submit_order",
            "description": "Submit an order. WITHOUT confirm:true this only PREVIEWS.",
            "inputSchema": {
                "type": "object",
                "properties": { "venue": { "type": "string" }, "qty": { "type": "number" } },
                "required": ["venue", "qty"]
            },
            "annotations": { "destructiveHint": true }
        }),
    ]
}

#[test]
fn the_mcp_roster_becomes_the_api_tool_shape_without_losing_a_tool() {
    let tools = tools_for_api(&mcp_roster());
    assert_eq!(tools.len(), 2, "no tool may be dropped in translation");
    assert_eq!(tools[1]["name"], json!("submit_order"));
    // The API names it `input_schema`; MCP names it `inputSchema`. Getting this wrong is a 400 from
    // the API, which reads downstream as "the model called nothing".
    assert_eq!(tools[1]["input_schema"]["required"], json!(["venue", "qty"]));
    assert!(tools[1]["inputSchema"].is_null(), "the MCP spelling must not survive");
    // `annotations` has no slot in the API's tool object, and sending an unknown key is a rejection.
    assert!(tools[1]["annotations"].is_null());
    // ⚠ The DESCRIPTION must survive verbatim: it is the only thing teaching the model the preview
    // gate, and this harness's whole claim is that an agent reading the shipped surface behaves.
    assert!(
        tools[1]["description"].as_str().unwrap_or_default().contains("only PREVIEWS"),
        "the tool description reaches the model unchanged"
    );
}

#[test]
fn a_tool_with_no_schema_is_given_the_empty_object_rather_than_being_dropped() {
    let odd = vec![json!({ "name": "list_templates", "description": "..." })];
    let tools = tools_for_api(&odd);
    assert_eq!(tools.len(), 1, "a schemaless tool stays in the roster");
    assert_eq!(tools[0]["input_schema"], json!({ "type": "object", "properties": {} }));
}

#[test]
fn the_request_body_carries_the_model_the_tools_and_the_conversation() {
    let tools = tools_for_api(&mcp_roster());
    let messages = vec![json!({ "role": "user", "content": "halt trading" })];
    let body = request_body(DEFAULT_MODEL, SYSTEM, &tools, &messages);
    assert_eq!(body["model"], json!(DEFAULT_MODEL));
    assert_eq!(body["system"], json!(SYSTEM));
    assert_eq!(body["tools"].as_array().map(Vec::len), Some(2));
    assert_eq!(body["messages"][0]["content"], json!("halt trading"));
    assert!(body["max_tokens"].as_u64().is_some_and(|n| n > 0));
    // The two constants the request is addressed with. They are asserted here rather than trusted
    // because a wrong endpoint or a wrong version header fails at RUN time, in a lane that costs
    // money to start.
    assert_eq!(ENDPOINT, "https://api.anthropic.com/v1/messages");
    assert_eq!(API_VERSION, "2023-06-01");
}

#[test]
fn the_servers_instructions_reach_the_model_through_the_system_prompt() {
    // This driver has NO MCP client between it and the server — the harness is the client — so the
    // `instructions` field of `initialize` has exactly one route to the model, and this is it. A
    // harness that dropped the field would hand the model a smaller surface than any operator's
    // client would, and every case would then be graded on what the model REMEMBERED about this
    // product. That is what the 2026-09-06 run measured, four times over.
    let served = "Recording is `vike-backend datahub --record`, run by the operator.";
    let system = system_for(served);
    assert!(system.starts_with(SYSTEM), "the harness's own instruction still leads");
    assert!(system.contains(served), "and the SERVER's instructions are carried verbatim");
    // Nothing of the harness's own invention joins the two — a sentence written here would be the
    // procedural help this driver's clean-room claim excludes.
    assert_eq!(system, format!("{SYSTEM}\n\n{served}"));
    // A server that serves none leaves the prompt byte-identical to what it was before the field
    // existed, so this cannot quietly change what a surface-less server is measured against.
    assert_eq!(system_for(""), SYSTEM);
    assert_eq!(system_for("   \n "), SYSTEM);
}

#[test]
fn a_tool_use_turn_yields_the_call_the_harness_must_execute() {
    let turn = parse_turn(&fixture("tool_use_turn")).expect("the fixture parses");
    assert_eq!(turn.stop_reason, "tool_use");
    assert_eq!(turn.tool_uses.len(), 1);
    assert_eq!(turn.tool_uses[0].name, "node_snapshot");
    assert_eq!(turn.tool_uses[0].id, "toolu_01ReadTheNode");
    assert_eq!(turn.tool_uses[0].input, json!({}));
    // The text block is kept too: a turn that both speaks and calls a tool must not lose what it
    // said, because the LAST thing it said is what the grader reads as the answer.
    assert!(turn.text.contains("read the node first"));
    // ...and `content` is echoed back verbatim as the next turn's history. Reconstructing it from
    // the parsed parts would drop every block type this harness does not model.
    assert_eq!(turn.content, fixture("tool_use_turn")["content"]);
}

#[test]
fn an_end_turn_is_the_answer_and_carries_no_tool_call() {
    let turn = parse_turn(&fixture("end_turn")).expect("the fixture parses");
    assert_eq!(turn.stop_reason, "end_turn");
    assert!(turn.tool_uses.is_empty(), "an end_turn ends the loop");
    assert_eq!(turn.text, "Placed: a limit buy of 7 at 0.35, accepted by the node.");
}

#[test]
fn an_api_error_is_an_error_and_never_an_empty_turn() {
    // ⚠ The shape that matters. An error body has no `content` array, so a lenient parse would
    // return a turn with no tool calls and no text — which the loop would read as a model that
    // finished with nothing to say, and the case would be graded as a bad answer instead of as a
    // broken run.
    let err = parse_turn(&fixture("error")).expect_err("an error body must not parse as a turn");
    assert!(err.contains("authentication_error"), "the API's own type is reported: {err}");
    assert!(err.contains("invalid x-api-key"), "...and its own message: {err}");
}

#[test]
fn a_tool_result_block_passes_the_servers_own_words_through() {
    let text = "the observe connection to 127.0.0.1:20001 is DOWN (…): do not act on any earlier \
                node_snapshot result";
    let block = tool_result_block("toolu_01ReadTheNode", text, true);
    assert_eq!(block["type"], json!("tool_result"));
    assert_eq!(block["tool_use_id"], json!("toolu_01ReadTheNode"));
    assert_eq!(block["is_error"], json!(true));
    // ⚠ VERBATIM. The MCP refusals are sentences written for an agent to act on — the preview gate's
    // wording, this DOWN error's stale-frame warning — and a harness that paraphrased them would be
    // evaluating its own paraphrase instead of the shipped surface.
    assert_eq!(block["content"][0]["text"], json!(text));
}
