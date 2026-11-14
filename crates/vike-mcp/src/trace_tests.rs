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
    let said = record["args"]["reason"].as_str().expect("text");
    assert!(said.starts_with(REDACTED) && said.contains("BINANCE"), "{said}");
    assert!(!record.to_string().contains("PLANTED-INLINE-VALUE"));
}

#[test]
fn ordinary_prose_that_merely_says_token_survives() {
    // The nuisance half of the value rule: `is_secret_key` matches the bare word TOKEN, so a
    // rule without the shouting-case precondition would erase this reason — and the transcript
    // would be worthless for the thing it is FOR.
    assert!(names_a_secret("the preview token expired, taking a fresh one").is_none());
    assert!(names_a_secret("export OKX_DEMO_API_PASSPHRASE=hunter2").is_some());
    assert!(names_a_secret("VIKE_TRADEHUB_CONTROL_KEY was wrong").is_some());
    assert!(names_a_secret("BTC_USDT on binance").is_none());
    // A capitalised bare word is not a name unless it is ASSIGNED…
    assert!(names_a_secret("the TOKEN expired, a PASSWORD reset is needed").is_none());
    // …and an assigned one is, in every spelling an agent might type.
    for assigned in
        ["PASSWORD=hunter2", "PASSWORD: hunter2", "{\"SECRET\": \"hunter2\"}", "FUNDER='0xabc'"]
    {
        assert!(names_a_secret(assigned).is_some(), "{assigned}");
    }
}

/// **A planted secret under EVERY credential name the venue tables know never reaches the file**
/// (the keyaudit row B19: the redaction was "by pattern", never proven against the names the product
/// really uses). One value is planted per name — as an object KEY in the arguments, in the lower-case
/// spelling of that key, and inside a free-text `NAME=value` — for every `secret` field of every venue
/// schema, every hub key, and every `secret` deployment name the store accepts. The marker is unique
/// per name, so a hit names the escaping field. Names are READ from the `vike_venues` tables, never
/// restated: a field added there is planted here with no edit.
#[test]
fn a_planted_secret_under_every_venue_credential_name_stays_out_of_the_file() {
    use vike_venues::credential_keys::{PLATFORM_KEYS, STORE_KEYS_OUTSIDE_THE_GRID};
    use vike_venues::credential_schema::VENUE_KEY_SCHEMAS;

    let mut names: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    names.extend(
        VENUE_KEY_SCHEMAS.iter().flat_map(|s| s.fields).filter(|f| f.secret).map(|f| f.field),
    );
    names.extend(PLATFORM_KEYS);
    names.extend(STORE_KEYS_OUTSIDE_THE_GRID.iter().filter(|row| row.2).map(|row| row.0));
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
    // The other half of the one flag: a deployment name it leaves open is recorded, as `config show` prints it.
    for (name, ..) in STORE_KEYS_OUTSIDE_THE_GRID.iter().filter(|row| !row.2) {
        let args = json!({ *name: "open-value" });
        let record = appended(&trace, 1_767_225_600_000, &call("submit_order", &args));
        assert_eq!(record["args"][*name], json!("open-value"), "{name}");
    }
}

/// **A `value` is as secret as the `key` beside it names.** `set_setting`'s two arguments are one
/// row, so the name that judges the second lives in the first, and the node's fence refusing such a
/// key comes after this record is written. Every spelling a client can send, at any depth.
#[test]
fn a_value_beside_a_secret_key_never_reaches_the_file() {
    let dir = scratch("vike-mcptrace-pair");
    let trace = McpTrace::new(dir.path().to_path_buf());
    for key in [
        "config.bot_token",
        "  config.BOT_TOKEN ",
        "api_key",
        "venue.api_key.live",
        "FUNDER",
        "policy.private",
    ] {
        for value in [json!("PLANTED-PAIR-VALUE"), json!(987_654_321)] {
            let args = json!({ "key": key, "value": value });
            let record = appended(&trace, 1_767_225_600_000, &call("set_setting", &args));
            let line = record.to_string();
            assert!(!line.contains("PLANTED-PAIR-VALUE"), "{key}: {line}");
            assert!(!line.contains("987654321"), "{key}: {line}");
            assert_eq!(record["args"]["value"], json!(REDACTED), "{key}: {line}");
        }
    }
    // Nested, and under a `key` that is no text: a refused call still leaves its record.
    let args = json!({
        "params": { "key": "config.bot_token", "value": "PLANTED-PAIR-VALUE" },
        "key": ["config.bot_token"],
        "value": "PLANTED-PAIR-VALUE",
    });
    let record = appended(&trace, 1_767_225_600_000, &call("set_setting", &args));
    assert!(!record.to_string().contains("PLANTED-PAIR-VALUE"), "{record}");
    // Non-vacuity: what the record is kept for survives, a value under a key that names nothing.
    let args = json!({ "key": "config.tradehub_addr", "value": "127.0.0.1:7879" });
    let record = appended(&trace, 1_767_225_600_000, &call("set_setting", &args));
    assert_eq!(record["args"]["value"], json!("127.0.0.1:7879"), "{record}");
    let args = json!({ "value": "no key beside it" });
    let record = appended(&trace, 1_767_225_600_000, &call("set_setting", &args));
    assert_eq!(record["args"]["value"], json!("no key beside it"), "{record}");
}

/// A profile TOML and a Rhai script are text an agent wrote, and neither spells a name in capitals:
/// an ASSIGNED credential name counts in any case. A name that is only mentioned does not.
#[test]
fn an_assigned_credential_name_counts_in_any_case() {
    for assigned in [
        "api_key = \"hunter2\"",
        "[strategy.params]\nclient_secret: hunter2",
        "let private_key = \"0xabc\";",
        "password=hunter2",
        "funder='0xabc'",
        "api-key = hunter2",
    ] {
        assert!(names_a_secret(assigned).is_some(), "{assigned}");
    }
    for prose in [
        "rotate the api_key today",
        "the preview token expired, taking a fresh one",
        "symbol = \"BTCUSDT\"",
        "venue: binance",
        "12:30 at http://127.0.0.1:7879",
    ] {
        assert!(names_a_secret(prose).is_none(), "{prose}");
    }
}

/// **A `reason` is a rationale in prose, and erasing it loses the one thing the record is for** (decision 0039):
/// words that merely precede a colon stay. Every shape a credential really takes still goes.
#[test]
fn a_reason_keeps_its_prose_and_still_loses_a_credential() {
    let dir = scratch("vike-mcptrace-reason");
    let trace = McpTrace::new(dir.path().to_path_buf());
    let reason_of = |reason: &str| {
        let args = json!({ "reason": reason });
        appended(&trace, 1_767_225_600_000, &call("cancel_order", &args))["args"]["reason"].clone()
    };
    for prose in ["asked by the user: close BTC", "token: SOL", "the key: rotate it on Monday"] {
        assert_eq!(reason_of(prose), json!(prose));
    }
    for (planted, word) in [
        ("rotating BINANCE_LIVE_API_SECRET=PLANTED", "BINANCE"),
        ("PASSWORD=PLANTED", "PASSWORD"),
        ("PASSWORD: PLANTED", "PASSWORD"),
        ("password=PLANTED", "password"),
        ("password: PLANTED", "password"),
        ("api_key: PLANTED", "api_key"),
        ("{\"client_secret\": \"PLANTED\"}", "client_secret"),
        ("TOKEN: PLANTED", "TOKEN"),
    ] {
        let said = reason_of(planted);
        let said = said.as_str().expect("text");
        assert!(said.starts_with(REDACTED) && !said.contains("PLANTED"), "{planted}: {said}");
        assert!(said.contains(word), "the record says what triggered it: {said}");
    }
}

/// **The seed knobs of a profile are excused by their exact name, and only for a number**, so a profile that sets one
/// is still recorded; what a `seed` phrase holds, or any other credential beside it, still takes the text out. What
/// is redacted says which name did it.
#[test]
fn the_seed_knobs_are_excused_and_a_redaction_says_what_triggered_it() {
    let dir = scratch("vike-mcptrace-seed");
    let trace = McpTrace::new(dir.path().to_path_buf());
    let profile_of = |profile: &str| {
        let args = json!({ "profile": profile });
        appended(&trace, 1_767_225_600_000, &call("run_backtest", &args))["args"]["profile"].clone()
    };
    for knob in ["seed = 42", "seed_bar_interval_ms = 60000", "queue_seed_depth = 3"] {
        let profile = format!("[engine]\n{knob}\n");
        assert_eq!(profile_of(&profile), json!(profile));
    }
    let said = profile_of("seed = \"abandon abandon PLANTED\"\n");
    let said = said.as_str().expect("text");
    assert!(said.starts_with(REDACTED) && !said.contains("PLANTED"), "{said}");
    assert!(said.contains("seed"), "{said}");
    let said = profile_of("seed = 42\napi_key = \"PLANTED\"\n");
    let said = said.as_str().expect("text");
    assert!(said.starts_with(REDACTED) && !said.contains("PLANTED"), "{said}");
    assert!(said.contains("api_key"), "{said}");
    // `let key =` in a script is a credential name by shape: it goes, and the record says that is why.
    let args = json!({ "script": "let key = 1;" });
    let record = appended(&trace, 1_767_225_600_000, &call("run_backtest", &args));
    let said = record["args"]["script"].as_str().expect("text");
    assert!(said.starts_with(REDACTED) && said.contains("key"), "{said}");
}

#[test]
fn a_long_argument_is_capped_and_says_how_much_it_dropped() {
    let script = "x".repeat(MAX_STRING_BYTES * 3);
    let capped = cap_string(&script);
    assert!(capped.len() < script.len());
    assert!(capped.contains("bytes)"), "the cap must state what it dropped: {capped}");
    assert_eq!(cap_string("short"), "short");
    // A cap that lands inside a three-byte character keeps the longest whole-character prefix and
    // counts every byte it dropped.
    let euros = "€".repeat(MAX_STRING_BYTES);
    let kept = MAX_STRING_BYTES / 3;
    assert_eq!(
        cap_string(&euros),
        format!("{}…(+{} bytes)", "€".repeat(kept), euros.len() - kept * 3)
    );
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
    // The minted half of the pair is exercised end to end by `src/lib.rs`'s transcript tests,
    // over the real preview gate — the only place that knows a token was MINTED.
    assert_eq!(TOKEN_MINTED, "minted");
}
