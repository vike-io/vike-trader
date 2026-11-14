//! The agent transcript `--trace` writes: which files, which records, what is redacted. Read off disk
//! after the process has gone, because the claim is about what survives.

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::harness::{Doc, FakeDaemon, Golden, Seen, Session, World, request};

/// The transcript directory as it stands: `{ files, records }`, the month in the file name shown as
/// `<yyyy-mm>` because it is the one part of the name that varies.
fn on_disk(dir: &Path) -> Value {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("the transcript directory {}: {e}", dir.display()))
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    json!({
        "files": names.iter().map(|n| shown_name(n)).collect::<Vec<_>>(),
        "records": records(dir, &names),
    })
}

fn shown_name(name: &str) -> String {
    match name.strip_prefix("mcp-").and_then(|rest| rest.strip_suffix(".jsonl")) {
        Some(month) if month.len() == 7 => "mcp-<yyyy-mm>.jsonl".to_string(),
        _ => name.to_string(),
    }
}

fn records(dir: &Path, names: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    for name in names.iter().filter(|n| n.ends_with(".jsonl")) {
        let text = std::fs::read_to_string(dir.join(name)).expect("a readable transcript file");
        for line in text.lines() {
            out.push(serde_json::from_str(line).expect("one JSON object per line"));
        }
    }
    out
}

fn with_transcript(doc: Doc, dir: &str) -> Golden {
    let on_disk = on_disk(&doc.world().path(dir));
    doc.with("on_disk", on_disk).golden()
}

/// `--trace-dir`: one record per `tools/call` across a read, a preview, a refusal and a confirm, the
/// redaction in both positions, and the cap.
pub(super) fn trace_dir() -> Vec<Golden> {
    let world = World::empty();
    let dir = world.arg("agent");
    let mut s = Session::start("trace.dir", world, &["--trace-dir", &dir]);
    s.tool("validate_strategy", json!({ "script": "fn on_bar() { }" }));
    s.tool("validate_strategy", json!({ "script": format!("// {}", "x".repeat(600)) }));
    let order = json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 });
    let preview = s.tool("submit_order", order.clone());
    let token = preview["result"]["structuredContent"]["preview_token"].clone();
    let mut junk = order.clone();
    junk["confirm"] = json!(true);
    junk["preview_token"] = json!("pv-junk");
    s.tool("submit_order", junk);
    let mut right = order.clone();
    right["confirm"] = json!(true);
    right["preview_token"] = token;
    s.tool("submit_order", right);
    s.tool("set_setting", json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7879" }));
    let mut secrets = order;
    secrets["api_secret"] = json!("PLANTED-ARGUMENT-SECRET");
    secrets["reason"] = json!("using BINANCE_LIVE_API_SECRET=PLANTED-INLINE-SECRET");
    s.tool("submit_order", secrets);
    s.tool("no_such_tool", json!({}));
    let doc = s.finish().what("`--trace-dir`: the records on disk, redacted and capped");
    vec![with_transcript(doc, "agent")]
}

/// `--trace`: the project's own state directory, under `read-only`, so a refusal is recorded.
pub(super) fn trace_project_state() -> Vec<Golden> {
    let mut s =
        Session::start("trace.project", World::empty(), &["--trace", "--profile", "read-only"]);
    s.tool("submit_order", json!({ "venue": "sim", "symbol": "BTCUSDT", "side": 1, "qty": 1.0 }));
    s.tool("list_templates", json!({}));
    let doc =
        s.finish().what("`--trace`: `<project>/settings/state/agent`, a refusal among the records");
    vec![with_transcript(doc, "settings/state/agent")]
}

/// A cancelled run gets no answer, so the transcript is the only place its end is written. The worker
/// reports AFTER its socket closed and nothing on the wire orders the two, so the record itself is
/// waited for: a client that closes stdin right behind the cancel may end the process first.
pub(super) fn trace_of_a_cancelled_call() -> Vec<Golden> {
    let daemon = FakeDaemon::keyless();
    let world = World::empty().scrub(&daemon.addr, "<daemon-addr>");
    let dir = world.path("agent");
    let dir_arg = dir.display().to_string();
    let mut s = Session::start(
        "trace.cancelled",
        world,
        &["--trace-dir", &dir_arg, "--backtest-addr", &daemon.addr],
    );
    s.send(request(
        7,
        "tools/call",
        json!({ "name": "run_sweep", "arguments": { "profile": "hold-traced" } }),
    ));
    assert_eq!(daemon.next_seen(), Seen::Request("hold-traced".to_string()));
    s.send(json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 7 }
    }));
    assert_eq!(daemon.next_seen(), Seen::PeerClosed("hold-traced".to_string()));
    let recorded = || {
        let names: Vec<String> = std::fs::read_dir(&dir)
            .map(|entries| {
                entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()
            })
            .unwrap_or_default();
        !records(&dir, &names).is_empty()
    };
    assert!(
        vike_log::poll::wait_until(Duration::from_secs(30), recorded),
        "the cancelled call was never recorded"
    );
    s.note("the record is on disk");
    s.expect_silence();
    let doc = s.finish().what("a cancelled run's only record");
    vec![with_transcript(doc, "agent")]
}
