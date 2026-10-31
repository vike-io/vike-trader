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
    // A capitalised bare word is not a name unless it is ASSIGNED…
    assert!(!names_a_secret("the TOKEN expired, a PASSWORD reset is needed"));
    // …and an assigned one is, in every spelling an agent might type.
    for assigned in
        ["PASSWORD=hunter2", "PASSWORD: hunter2", "{\"SECRET\": \"hunter2\"}", "FUNDER='0xabc'"]
    {
        assert!(names_a_secret(assigned), "{assigned}");
    }
}

/// **A planted secret under EVERY credential name the venue tables know never reaches the file**
/// (the keyaudit row B19: the redaction was "by pattern", never proven against the names the product
/// really uses). One value is planted per name — as an object KEY in the arguments, in the lower-case
/// spelling of that key, and inside a free-text `NAME=value` — for every `secret` field of every venue
/// schema, every hub key, and every deployment name the store accepts. The marker is unique per name,
/// so a hit names the escaping field. Names are READ from the `vike_venues` tables, never restated:
/// a field added there is planted here with no edit.
#[test]
fn a_planted_secret_under_every_venue_credential_name_stays_out_of_the_file() {
    use vike_venues::credential_keys::{PLATFORM_KEYS, STORE_KEYS_OUTSIDE_THE_GRID};
    use vike_venues::credential_schema::VENUE_KEY_SCHEMAS;

    let mut names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    names.extend(
        VENUE_KEY_SCHEMAS.iter().flat_map(|s| s.fields).filter(|f| f.secret).map(|f| f.field),
    );
    names.extend(PLATFORM_KEYS);
    names.extend(STORE_KEYS_OUTSIDE_THE_GRID.iter().map(|(name, _)| *name));
    // Non-vacuity: the three sources really contributed (a renamed table would otherwise shrink this
    // test to nothing and still pass).
    for expected in ["API_KEY", "PRIVATE_KEY", "FUNDER", "CLIENT_SECRET", "ACCESS_TOKEN"] {
        assert!(
            names.contains(expected),
            "the venue schemas no longer carry {expected}: {names:?}"
        );
    }
    assert!(names.len() >= 30, "the credential names read from the venue tables: {names:?}");

    let dir = scratch("vike-mcptrace-everyname");
    let trace = McpTrace::new(dir.path().to_path_buf());
    for (i, name) in names.iter().enumerate() {
        let as_key = format!("PLANTED-KEY-{i:03}-VALUE");
        let as_lower_key = format!("PLANTED-LOWER-{i:03}-VALUE");
        let as_text = format!("PLANTED-TEXT-{i:03}-VALUE");
        let args = json!({
            *name: as_key,
            name.to_ascii_lowercase(): as_lower_key,
            "nested": { "tier": "demo", *name: format!("{as_key}-nested") },
            "reason": format!("rotating {name}={as_text}"),
            "venue": "binance"
        });
        let record = appended(&trace, 1_767_225_600_000, &call("submit_order", &args));
        let line = record.to_string();
        for marker in [&as_key, &as_lower_key, &as_text] {
            assert!(!line.contains(marker.as_str()), "{name}: `{marker}` reached the file: {line}");
        }
        // Non-vacuity per name: the record is the call's, with its ordinary argument intact.
        assert_eq!(record["args"]["venue"], json!("binance"), "{name}: {line}");
    }
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
