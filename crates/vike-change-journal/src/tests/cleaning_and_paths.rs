use super::*;

/// Control characters never reach the file, whatever the caller passed. Belt and braces: the
/// JSON escape would already keep the framing intact, and `grep`, a terminal and a naive
/// splitter all see the raw bytes.
#[test]
fn no_control_character_survives_into_the_line() {
    let r = root();
    let j = journal(r.path());
    let nasty = "flat\n{\"kind\":\"forged\",\"seq\":0}\r\nmore\u{0}\u{7f}\u{85}";
    let c = Change::set_setting(Outcome::Applied, Actor::Gui, "f.toml", "f.k", None, nasty)
        .with_reason(Some(nasty));
    let path = j.append(T, &c).expect("append");

    let raw = std::fs::read_to_string(&path).expect("read");
    assert_eq!(raw.lines().count(), 1, "one record is still one line: {raw:?}");
    assert!(!raw.trim_end().contains('\n'), "no embedded newline");
    assert!(!raw.contains('\u{0}') && !raw.contains('\r'), "no NUL, no CR");
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("still parses");
    assert!(v["target"]["new"].as_str().unwrap().starts_with("flat{"));
}

/// Every cell is BYTE-capped on a char boundary, so a multi-byte sequence is never split into
/// invalid UTF-8 — the trap a byte-wise truncation walks into.
#[test]
fn cells_are_byte_capped_without_splitting_a_multibyte_char() {
    let snow = "☃";
    assert_eq!(snow.len(), 3, "the fixture must actually be multi-byte");
    let long = snow.repeat(MAX_FIELD_BYTES);
    let out = clean_field(&long);
    assert!(out.len() <= MAX_FIELD_BYTES);
    assert!(out.chars().all(|c| c == '☃'), "no partial sequence: {out:?}");
    assert_eq!(out.len() % 3, 0, "whole chars only");
    // Under the cap, nothing is touched.
    assert_eq!(clean_field("policy.max_notional_per_order"), "policy.max_notional_per_order");
}

/// The directory is created on first append, because `settings/state/changes` is not a marker
/// and a fresh install has none.
#[test]
fn the_journal_directory_is_created_on_first_append() {
    let r = root();
    let dir = r.path().join("state").join(CHANGES_SUBDIR);
    assert!(!dir.exists(), "precondition");
    let j = ChangeJournal::new(dir.clone(), Proc::new("t", 1, "0"));
    let c = Change::set_setting(Outcome::Applied, Actor::Boot, "f.toml", "f.k", None, "1");
    j.append(T, &c).expect("append creates the directory");
    assert!(dir.is_dir());
}

/// `in_state_dir` puts the journal exactly where the module doc says, under an ALREADY-RESOLVED
/// state directory rather than a fresh walk.
#[test]
fn in_state_dir_joins_the_changes_subdirectory() {
    let state = Path::new("/srv/vike-<unit>/settings/state");
    let j = ChangeJournal::in_state_dir(state, Proc::new("t", 1, "0"));
    assert_eq!(j.dir(), state.join(CHANGES_SUBDIR));
    assert_eq!(CHANGES_SUBDIR, "changes");
}
