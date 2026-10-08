use super::*;

fn scratch(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(tag).tempdir().expect("a scratch directory")
}

fn call<'a>(tool: &'a str, args: &'a Value) -> Call<'a> {
    Call {
        tool,
        write: false,
        profile: "full",
        args,
        verdict: Verdict::Ok,
        detail: None,
        token: None,
        token_role: None,
    }
}

/// The record of a real appended call, parsed back — the shape every assertion below reads.
fn appended(trace: &McpTrace, ts_ms: i64, c: &Call<'_>) -> Value {
    let path = trace.append(ts_ms, c).expect("the append succeeds");
    let text = std::fs::read_to_string(path).expect("the file is readable");
    let last = text.lines().next_back().expect("at least one line").to_string();
    serde_json::from_str(&last).expect("every line is one JSON object")
}

#[test]
fn a_secret_shaped_key_never_has_its_value_recorded() {
    let dir = scratch("vike-mcptrace-key");
    let trace = McpTrace::new(dir.path().to_path_buf());
    let args = json!({
        "venue": "binance",
        "api_key": "PLANTED-KEY-VALUE",
        "nested": { "client_secret": "PLANTED-SECRET-VALUE" },
        "preview_token": "pv-9"
    });
    let record = appended(&trace, 1_767_225_600_000, &call("submit_order", &args));
    let line = record.to_string();
    assert!(!line.contains("PLANTED-KEY-VALUE"), "a planted key value reached the file: {line}");
    assert!(!line.contains("PLANTED-SECRET-VALUE"), "a planted nested secret reached: {line}");
    assert_eq!(record["args"]["api_key"], json!(REDACTED));
    assert_eq!(record["args"]["nested"]["client_secret"], json!(REDACTED));
    // …and the same rule catches `preview_token`, whose identity the record carries in its own
    // field rather than copying a string the agent chose to send under that name.
    assert_eq!(record["args"]["preview_token"], json!(REDACTED));
    // Non-vacuity: an ordinary argument IS recorded, so the assertions above are about the
    // redaction rather than about an empty payload.
    assert_eq!(record["args"]["venue"], json!("binance"));
}

#[test]
fn a_credential_named_inside_a_free_text_value_is_redacted_whole() {
    let dir = scratch("vike-mcptrace-value");
    let trace = McpTrace::new(dir.path().to_path_buf());
    let args = json!({ "reason": "rotating BINANCE_LIVE_API_SECRET=PLANTED-INLINE-VALUE" });
    let record = appended(&trace, 1_767_225_600_000, &call("cancel_order", &args));
    assert_eq!(record["args"]["reason"], json!(REDACTED));
    assert!(!record.to_string().contains("PLANTED-INLINE-VALUE"));
}

#[test]
fn ordinary_prose_that_merely_says_token_survives() {
    // The nuisance half of the value rule: `is_secret_key` matches the bare word TOKEN, so a
    // rule without the shouting-case precondition would erase this reason — and the transcript
    // would be worthless for the thing it is FOR.
    assert!(!names_a_secret("the preview token expired, taking a fresh one"));
    assert!(names_a_secret("export OKX_DEMO_API_PASSPHRASE=hunter2"));
    assert!(names_a_secret("VIKE_TRADEHUB_CONTROL_KEY was wrong"));
    assert!(!names_a_secret("BTC_USDT on binance"));
}

#[test]
fn a_long_argument_is_capped_and_says_how_much_it_dropped() {
    let script = "x".repeat(MAX_STRING_BYTES * 3);
    let capped = cap_string(&script);
    assert!(capped.len() < script.len());
    assert!(capped.contains("bytes)"), "the cap must state what it dropped: {capped}");
    assert_eq!(cap_string("short"), "short");
}

#[test]
fn an_oversized_record_keeps_the_call_and_drops_the_arguments() {
    let dir = scratch("vike-mcptrace-huge");
    let trace = McpTrace::new(dir.path().to_path_buf());
    // Many keys, each under the string cap — so the DEGRADE path is reached through the RECORD
    // cap rather than through any single value's cap.
    let mut map = serde_json::Map::new();
    for i in 0..MAX_OBJECT_KEYS {
        map.insert(format!("k{i}"), json!("y".repeat(MAX_STRING_BYTES)));
    }
    let args = Value::Object(map);
    let record = appended(&trace, 1_767_225_600_000, &call("run_backtest", &args));
    assert_eq!(record["tool"], json!("run_backtest"), "the CALL survives");
    assert!(
        record["args"][ELIDED].is_string(),
        "the arguments are dropped with a marker, not silently: {record}"
    );
}

#[test]
fn the_record_survives_a_restart_because_it_appends() {
    let dir = scratch("vike-mcptrace-restart");
    let ts = 1_767_225_600_000;
    // Two SEPARATE writers over one directory — the process-restart shape, and also the
    // two-concurrent-sessions shape.
    let first = McpTrace::new(dir.path().to_path_buf());
    let path = first.append(ts, &call("list_templates", &json!({}))).unwrap();
    drop(first);
    let second = McpTrace::new(dir.path().to_path_buf());
    second.append(ts, &call("node_snapshot", &json!({}))).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "the second writer APPENDED rather than rewriting: {text}");
    assert!(lines[0].contains("list_templates"), "the first record is still there: {text}");
    assert!(lines[1].contains("node_snapshot"));
}

#[test]
fn the_month_file_name_is_the_instant_it_belongs_to() {
    assert_eq!(month_file_name(1_767_225_600_000), "mcp-2026-01.jsonl");
    assert!(is_month_file_name("mcp-2026-01.jsonl"));
    assert!(!is_month_file_name(TRACE_LOCK_FILE), "the lock sentinel is never prunable");
    assert!(!is_month_file_name("mcp-old.jsonl"));
}

#[test]
fn prune_keeps_the_newest_months_and_leaves_strangers_alone() {
    let dir = scratch("vike-mcptrace-prune");
    let trace = McpTrace::new(dir.path().to_path_buf());
    for month in 1..=4 {
        std::fs::write(dir.path().join(format!("mcp-2026-{month:02}.jsonl")), "{}\n").unwrap();
    }
    let stranger = dir.path().join("mcp-export.jsonl");
    std::fs::write(&stranger, "mine").unwrap();
    trace.prune(2);
    assert!(!dir.path().join("mcp-2026-01.jsonl").exists(), "the oldest goes first");
    assert!(!dir.path().join("mcp-2026-02.jsonl").exists());
    assert!(dir.path().join("mcp-2026-03.jsonl").exists());
    assert!(dir.path().join("mcp-2026-04.jsonl").exists());
    assert!(stranger.exists(), "housekeeping never deletes what it does not recognise");
}

#[test]
fn a_verdict_and_a_token_role_reach_the_record() {
    let dir = scratch("vike-mcptrace-verdict");
    let trace = McpTrace::new(dir.path().to_path_buf());
    let args = json!({ "venue": "sim" });
    let c = Call {
        tool: "submit_order",
        write: true,
        profile: "read-only",
        args: &args,
        verdict: Verdict::Refused,
        detail: Some("not available under the `read-only` profile".to_string()),
        token: Some("pv-1".to_string()),
        token_role: Some(TOKEN_PRESENTED),
    };
    let record = appended(&trace, 1_767_225_600_000, &c);
    assert_eq!(record["verdict"], json!("refused"));
    assert_eq!(record["write"], json!(true));
    assert_eq!(record["profile"], json!("read-only"));
    assert_eq!(record["token"], json!("pv-1"));
    assert_eq!(record["token_role"], json!(TOKEN_PRESENTED));
    assert_eq!(record["kind"], json!(RECORD_KIND));
    assert!(record["detail"].as_str().unwrap().contains("read-only"));
    assert!(record["proc"]["pid"].is_number(), "the writing process is named: {record}");
    // The minted half of the pair is exercised end to end by `cmd/mcp.rs`'s transcript tests,
    // over the real preview gate — the only place that knows a token was MINTED.
    assert_eq!(TOKEN_MINTED, "minted");
}
