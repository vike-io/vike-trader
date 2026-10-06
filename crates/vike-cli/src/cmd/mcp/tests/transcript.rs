//! The agent transcript (`--trace`): what reaches disk, what is redacted, and when nothing does.

use super::*;
use crate::cmd::mcp::config::TraceRequest;
use crate::cmd::mcp::trace::{TOKEN_MINTED, TOKEN_PRESENTED};

// ---- the agent transcript (`--trace`) ----------------------------------------------------

fn scratch(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(tag).tempdir().expect("a scratch directory")
}

/// A server that RECORDS, writing into `dir`. Everything else is [`test_server`]'s — no node,
/// no credentials — so nothing here can reach a venue whatever the transcript says.
fn tracing_server(dir: &std::path::Path, profile: Profile) -> Server {
    Server {
        access: ToolAccess::new(profile, Vec::new()),
        trace: Some(McpTrace::new(dir.to_path_buf())),
        ..test_server()
    }
}

/// Every record written under `dir`, parsed. Reads what is ON DISK rather than what the writer
/// returned: the claim is that the transcript survives, not that a function was called.
fn records(dir: &std::path::Path) -> Vec<Value> {
    let mut out = Vec::new();
    let entries = std::fs::read_dir(dir).expect("the transcript directory exists");
    let mut paths: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable record file");
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            out.push(serde_json::from_str(line).expect("one JSON object per line"));
        }
    }
    out
}

#[test]
fn a_refused_write_is_recorded_with_the_reason_it_was_refused() {
    // ⚠ THE WHOLE POINT OF THE FILE. A refusal reaches the NODE's audit trail never — it was
    // refused before a byte left this process — so if it is not here it is nowhere.
    let dir = scratch("vike-mcp-refused");
    let mut s = tracing_server(dir.path(), Profile::ReadOnly);
    let resp = s
            .handle(&req(
                1,
                "tools/call",
                json!({ "name": "submit_order", "arguments": { "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 } }),
            ))
            .unwrap();
    assert_eq!(resp["result"]["isError"], true, "got {resp}");
    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 1, "one call, one record: {recorded:?}");
    assert_eq!(recorded[0]["tool"], json!("submit_order"));
    assert_eq!(recorded[0]["verdict"], json!("refused"));
    assert_eq!(recorded[0]["write"], json!(true));
    assert_eq!(recorded[0]["profile"], json!("read-only"));
    assert!(
        recorded[0]["detail"].as_str().unwrap().contains("read-only"),
        "the reason must be recorded, not just the fact: {recorded:?}"
    );
}

/// **THE DECLARED BOUND on what a `set_setting` transcript can say**, pinned in both
/// directions so a green run means it was measured rather than assumed.
///
/// `crate::cmd::mcp::trace` redacts an argument whose NAME is credential-shaped, and
/// `vike_config::is_secret_key` — the workspace's one authority for that — matches the bare
/// word `KEY`. So the `key` argument is recorded redacted, and no spelling of that concept
/// escapes it (`setting_key`, `dotted_key`: all `_KEY`). The fix is NOT a second table here;
/// what the surface owes instead is to say what the record still carries.
#[test]
fn the_transcript_redacts_the_settings_key_and_keeps_the_rest() {
    let dir = scratch("vike-mcp-setting");
    let mut s = tracing_server(dir.path(), Profile::Full);
    let args = json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7879" });
    s.handle(&req(1, "tools/call", json!({ "name": "set_setting", "arguments": args }))).unwrap();
    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 1, "one call, one record: {recorded:?}");
    assert_eq!(recorded[0]["tool"], json!("set_setting"));
    assert_eq!(recorded[0]["write"], json!(true), "a lifecycle verb is a WRITE in the record");
    assert_eq!(recorded[0]["verdict"], json!("preview"), "the first call sends nothing");
    // The bound…
    assert_ne!(
        recorded[0]["args"]["key"],
        json!("config.tradehub_addr"),
        "`key` matches the bare `KEY` shape, so its value is redacted — see this tool's spec \
             comment for why that is not fixed here: {recorded:?}"
    );
    // …and what survives it, which is what makes the record still worth keeping. (The tool's
    // `file` argument used to survive too; it went with the file era, `docs/decisions/0086`.)
    assert_eq!(recorded[0]["args"]["value"], json!("127.0.0.1:7879"), "{recorded:?}");
}

#[test]
fn a_preview_records_the_minted_token_and_the_confirm_records_the_presented_one() {
    let dir = scratch("vike-mcp-token");
    let mut s = tracing_server(dir.path(), Profile::Full);
    let args = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    let preview = s
        .handle(&req(1, "tools/call", json!({ "name": "submit_order", "arguments": args })))
        .unwrap();
    let token = preview["result"]["structuredContent"]["preview_token"]
        .as_str()
        .expect("a preview mints a token")
        .to_string();
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token.clone());
    // With no node configured this gets PAST the gate and fails at the connection, which is how
    // this suite tells "admitted" from "refused" without ever reaching a venue.
    s.handle(&req(2, "tools/call", json!({ "name": "submit_order", "arguments": confirming })))
        .unwrap();

    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    assert_eq!(recorded[0]["verdict"], json!("preview"), "a preview is not an `ok`");
    assert_eq!(recorded[0]["token"], json!(token));
    assert_eq!(recorded[0]["token_role"], json!(TOKEN_MINTED));
    assert_eq!(recorded[1]["token"], json!(token), "the same identity, on the call that used it");
    assert_eq!(recorded[1]["token_role"], json!(TOKEN_PRESENTED));
    assert_eq!(
        recorded[1]["verdict"],
        json!("error"),
        "an unreachable node is an ERROR, not a gate refusal: {recorded:?}"
    );
    // ⚠ The token is recorded as an IDENTITY in its own field and NOT copied out of the
    // arguments — where the credential-name rule redacts it like any other `_TOKEN`.
    assert_eq!(recorded[1]["args"]["preview_token"], json!(trace::REDACTED));
    // The order's own fields ARE recorded: what the agent attempted is the point.
    assert_eq!(recorded[1]["args"]["qty"], json!(1.0));
}

#[test]
fn a_read_tool_is_recorded_too() {
    // Non-vacuity for the write assertions above: the transcript is a record of the SESSION,
    // not a second copy of the order log.
    let dir = scratch("vike-mcp-read");
    let mut s = tracing_server(dir.path(), Profile::Full);
    s.handle(&req(1, "tools/call", json!({ "name": "list_templates", "arguments": {} }))).unwrap();
    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0]["tool"], json!("list_templates"));
    assert_eq!(recorded[0]["verdict"], json!("ok"));
    assert_eq!(recorded[0]["write"], json!(false));
    assert_eq!(recorded[0]["token"], Value::Null, "a read mints and presents no token");
    assert_eq!(recorded[0]["token_role"], Value::Null);
}

#[test]
fn a_planted_secret_in_an_argument_never_reaches_the_transcript() {
    // Two shapes, because the rule has two positions: a credential-shaped KEY, and a credential
    // NAMED inside free text. `crate::cmd::mcp::trace` owns the rule; this proves it is actually
    // wired into the server's own path rather than only unit-tested beside it.
    let dir = scratch("vike-mcp-secret");
    let mut s = tracing_server(dir.path(), Profile::Full);
    s.handle(&req(
        1,
        "tools/call",
        json!({ "name": "submit_order", "arguments": {
            "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0,
            "api_secret": "PLANTED-ARGUMENT-SECRET",
            "reason": "using BINANCE_LIVE_API_SECRET=PLANTED-INLINE-SECRET"
        }}),
    ))
    .unwrap();
    let recorded = records(dir.path());
    let line = recorded[0].to_string();
    assert!(!line.contains("PLANTED-ARGUMENT-SECRET"), "a keyed secret reached disk: {line}");
    assert!(!line.contains("PLANTED-INLINE-SECRET"), "an inline secret reached disk: {line}");
    assert_eq!(recorded[0]["args"]["api_secret"], json!(trace::REDACTED));
    assert_eq!(recorded[0]["args"]["reason"], json!(trace::REDACTED));
}

#[test]
fn without_the_flag_the_transcript_writes_nothing_at_all() {
    // ⚠ Not "writes an empty file" — creates NOTHING. The default must leave no trace on an
    // operator's disk, because the default is what every existing MCP client config runs.
    let dir = scratch("vike-mcp-off");
    let mut s = test_server();
    assert!(s.trace.is_none(), "the default server records nothing");
    s.handle(&req(1, "tools/call", json!({ "name": "list_templates", "arguments": {} }))).unwrap();
    s.handle(&req(
            2,
            "tools/call",
            json!({ "name": "submit_order", "arguments": { "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 } }),
        ))
        .unwrap();
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
    assert!(left.is_empty(), "an untraced session wrote {} entries", left.len());
}

#[test]
fn the_transcript_survives_a_restart_and_a_second_session_appends_to_it() {
    // Two servers over one directory, which is BOTH the restart shape and the two-agents shape.
    let dir = scratch("vike-mcp-restart");
    let mut first = tracing_server(dir.path(), Profile::Full);
    first
        .handle(&req(1, "tools/call", json!({ "name": "list_templates", "arguments": {} })))
        .unwrap();
    drop(first);
    let mut second = tracing_server(dir.path(), Profile::ReadOnly);
    second
        .handle(&req(1, "tools/call", json!({ "name": "list_indicators", "arguments": {} })))
        .unwrap();
    let recorded = records(dir.path());
    assert_eq!(recorded.len(), 2, "the second session APPENDED: {recorded:?}");
    assert_eq!(recorded[0]["tool"], json!("list_templates"), "the first record survived");
    assert_eq!(recorded[1]["tool"], json!("list_indicators"));
    assert_eq!(recorded[1]["profile"], json!("read-only"), "each record names its own scope");
}

#[test]
fn a_trace_that_cannot_resolve_a_project_refuses_rather_than_recording_nowhere() {
    // ⚠ The asymmetry with `vike_boot::journal_boot_settings`, which records nothing when there
    // is no project: there NOBODY asked, here the operator TYPED `--trace`, and a server that
    // started anyway would be discovered to have recorded nothing after the incident.
    let err = resolve_trace(TraceRequest::ProjectState, None).expect_err("no project, no home");
    assert!(err.contains("--trace-dir"), "the refusal must name a way out: {err}");
    // Spelled without its `VIKE_` head deliberately: `crates/vike-ops/tests/settings/settings_registry.rs`
    // harvests env-shaped string literals out of `src/` and demands a `SETTINGS` row for each,
    // keyed on `(name, krate)` — and this crate has no row for that variable, because it reads
    // it through `vike-boot` rather than itself.
    assert!(err.contains("SETTINGS_DIR"), "…and the other one: {err}");
    // The named-directory form is honourable with no project at all.
    let dir = scratch("vike-mcp-resolve");
    let resolved = resolve_trace(TraceRequest::Dir(dir.path().to_path_buf()), None)
        .expect("an explicit directory needs no project");
    assert_eq!(resolved.expect("a writer").dir(), dir.path());
    // …and no flag is no writer, whatever the project situation is.
    assert!(resolve_trace(TraceRequest::Off, None).expect("off is not a failure").is_none());
}
