//! In-crate tests: the calendar helpers, the text cap, redaction, the launch argv, the task table.

use std::path::PathBuf;

use super::*;

/// The calendar arithmetic, against instants whose UTC rendering is known independently of this
/// code. Without these the helper would be pinned only against itself.
#[test]
fn utc_stamp_matches_known_instants() {
    assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc_stamp(86_400_000), "1970-01-02T00:00:00Z");
    // Unix time 1_000_000_000 and 1_600_000_000, two widely-published instants.
    assert_eq!(utc_stamp(1_000_000_000_000), "2001-09-09T01:46:40Z");
    assert_eq!(utc_stamp(1_600_000_000_000), "2020-09-13T12:26:40Z");
    // …and a leap day, the case an off-by-one in the era arithmetic lands on.
    assert_eq!(utc_stamp(1_583_020_800_000), "2020-03-01T00:00:00Z");
    assert_eq!(compact_stamp(1_600_000_000_000), "20200913T122640Z");
}

#[test]
fn a_capped_field_says_it_was_capped_and_stays_valid_utf8() {
    let short = "a short answer";
    assert_eq!(cap_text(short), short);
    // ⚠ A THREE-byte character, deliberately: the cap is not a multiple of 3, so the cut lands
    // mid-character and the boundary walk is what stops this from panicking. A two-byte
    // character would divide the cap exactly and prove nothing.
    let long: String = "€".repeat(MAX_TEXT_BYTES);
    assert!(!long.is_char_boundary(MAX_TEXT_BYTES), "this test needs a straddling cut");
    let capped = cap_text(&long);
    assert!(capped.contains("truncated"), "a capped field must say so: {capped}");
    assert!(
        capped.len() <= MAX_TEXT_BYTES + 64,
        "the cap must actually shorten the field: {} bytes",
        capped.len()
    );
    assert!(capped.starts_with('€'), "the surviving prefix must still be the text");
}

#[test]
fn redaction_reaches_every_string_of_the_document() {
    let mut doc = json!({
        "conclusion": "the key is sk-live-SECRET, apparently",
        "nested": { "list": ["sk-live-SECRET", 7] },
        "untouched": 12,
    });
    redact_in_place(&mut doc, &["sk-live-SECRET", "", "   "]);
    let text = doc.to_string();
    assert!(!text.contains("sk-live-SECRET"), "a secret survived redaction: {text}");
    assert_eq!(text.matches(REDACTED).count(), 2, "both occurrences must be replaced: {text}");
    assert_eq!(doc["untouched"], json!(12), "non-strings are untouched");
}

#[test]
fn the_default_launch_asks_for_the_read_only_ring() {
    let dir = PathBuf::from("record");
    let spec = RunSpec {
        vike_cli: None,
        task: "node-review",
        prompt: "p",
        node: Some("127.0.0.1:7900"),
        datahub: None,
        settings_dir: None,
        record_dir: &dir,
        allow_writes: false,
        max_steps: DEFAULT_MAX_STEPS,
        budget: Duration::from_secs(DEFAULT_BUDGET_SECS),
        scrub: &[],
    };
    let args = mcp_argv(&spec);
    let profile = args
        .iter()
        .position(|a| a == "--profile")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);
    assert_eq!(profile, Some(READ_ONLY_PROFILE), "the default launch must be read-only: {args:?}");
    assert!(args.iter().any(|a| a == "--trace-dir"), "the per-call record is always on: {args:?}");

    let wide = RunSpec { allow_writes: true, ..spec };
    let args = mcp_argv(&wide);
    let profile = args
        .iter()
        .position(|a| a == "--profile")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);
    assert_eq!(profile, Some(FULL_PROFILE), "--allow-writes is the ONLY widening: {args:?}");
}

/// **Every launch declares the session UNATTENDED — `--allow-writes` included.** The owner's
/// rulings on decision 0040 (2026-09-28 and 2026-09-29): a run nobody attends changes no setting
/// at all. The rule an ATTENDED agent follows — change a live setting only after the owner said
/// yes in chat (0086 point 6) — cannot be kept by a session that has no chat, so the server this
/// launches is told it is unattended and refuses every `set_setting` outright
/// (`crates/vike-cli/tests/mcp_unattended_cli.rs` pins that half against the shipped binary).
///
/// ⚠ It is asserted for BOTH rings on purpose: the read-only ring serves no `set_setting` at all,
/// so only the `--allow-writes` launch is where the flag does work — and a flag passed only when
/// it "mattered" is a flag one refactor of this argv drops.
#[test]
fn every_launch_declares_the_session_unattended() {
    let dir = PathBuf::from("record");
    for allow_writes in [false, true] {
        let spec = RunSpec {
            vike_cli: None,
            task: "node-review",
            prompt: "p",
            node: Some("127.0.0.1:7900"),
            datahub: None,
            settings_dir: None,
            record_dir: &dir,
            allow_writes,
            max_steps: DEFAULT_MAX_STEPS,
            budget: Duration::from_secs(DEFAULT_BUDGET_SECS),
            scrub: &[],
        };
        let args = mcp_argv(&spec);
        assert!(
            args.iter().any(|a| a == "--unattended"),
            "allow_writes={allow_writes}: every launch must say nobody is in the chat: {args:?}"
        );
    }
}

#[test]
fn every_task_is_selectable_and_unique() {
    assert!(!TASKS.is_empty(), "an empty task table would make `--task` unusable");
    for task in TASKS {
        assert_eq!(task_by_name(task.name).map(|t| t.name), Some(task.name));
        assert!(!task.prompt.trim().is_empty(), "{} has no prompt", task.name);
        assert!(!task.what.trim().is_empty(), "{} has no one-line description", task.name);
    }
    let mut names: Vec<&str> = TASKS.iter().map(|t| t.name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "two tasks share a --task name");
}
