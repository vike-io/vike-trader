use super::*;

/// Whatever the store holds for a credential-shaped key, no byte of it reaches the row —
/// the invariant every disclosure surface (CLI table, `--json`, the tradehub wire) leans on.
#[test]
fn a_secret_settings_value_never_enters_the_file_row() {
    const LEAK: &str = "sk-do-not-print-me";
    let secret = ResolvedSetting {
        key: "config.bot_token".to_string(),
        section: "config",
        value: Some(LEAK.to_string()),
        default: None,
        origin: Origin::Db,
        origin_value: Some(LEAK.to_string()),
        adjusted: false,
    };
    let r = resolve_file_row(&secret);
    assert!(r.secret);
    assert_eq!(r.value, SET);
    assert!(!format!("{r:?}").contains(LEAK), "the secret leaked into {r:?}");
}

/// Every typed setting reaches the table, and with no settings directory every one of them is
/// honestly reported as a compiled-in default — the loudest possible answer to "did my write
/// take effect?".
#[test]
fn the_files_half_covers_every_typed_setting() {
    let d = crate::describe(None).unwrap();
    let rows = file_rows(&d, None, false);
    assert_eq!(rows.len(), crate::provenance::setting_keys().len());
    assert!(rows.iter().any(|r| r.key == "policy.max_notional_per_order"));
    assert!(rows.iter().any(|r| r.key == "preferences.log_file_level"));
    assert!(rows.iter().any(|r| r.key == "flags.reconcile"));
    assert!(rows.iter().all(|r| r.origin == "default"), "no store was consulted");
    assert!(file_rows(&d, None, true).is_empty(), "--changed-only hides every default");
}

#[test]
fn the_files_filter_matches_the_key_or_the_section() {
    let d = crate::describe(None).unwrap();
    let policy = file_rows(&d, Some("policy"), false);
    assert!(policy.iter().all(|r| r.key.starts_with("policy.") || r.key.contains("policy")));
    assert!(policy.iter().any(|r| r.key == "policy.max_notional_per_order"), "the section matches");
    // ...and a KEY match is a match whatever its section: `config.reconcile_policy` is the
    // reconcile POLICY, and an operator filtering on the word is looking for it too.
    assert!(policy.iter().any(|r| r.key == "config.reconcile_policy"), "the key matches");
    assert!(!file_rows(&d, Some("flags"), false).is_empty());
    assert!(file_rows(&d, Some("no-such-key"), false).is_empty());
}

fn default_row(key: &str, section: &'static str) -> ResolvedSetting {
    ResolvedSetting {
        key: key.to_string(),
        section,
        value: None,
        default: None,
        origin: Origin::Default,
        origin_value: None,
        adjusted: false,
    }
}

/// The `READ` cell's four words, from the four `read_status` shapes.
#[test]
fn the_read_cell_names_the_binary_a_library_nothing_or_nobody_knows() {
    // The real consumption tables drive the real rows; pin the rendering rule itself on
    // synthesized states so the four arms stay covered whatever the tables say.
    let mut r = resolve_file_row(&default_row("config.tradehub_addr", "config"));
    r.read_status = ReadStatus::Unread { verdict: "v", why: "w" };
    assert_eq!(r.read_cell(), "NO");
    r.read_status = ReadStatus::Read { binary: None };
    assert_eq!(r.read_cell(), "yes");
    r.read_status = ReadStatus::Read { binary: Some("tradehub") };
    assert_eq!(r.read_cell(), "tradehub");
    r.read_status = ReadStatus::Unknown;
    assert_eq!(r.read_cell(), "?", "'nobody has said' is not 'nothing reads it' and not 'yes'");
}

/// **`config show` says `READ: yes` for `policy.max_leverage` now, and for the right reason.** The
/// row is built from the shipped `POLICY_VERDICTS`, so the answer is the one the policy gate holds
/// to the tree: the ceiling narrows the run profile's leverage and nothing about it is unread. (It
/// read `NO` while it was accepted and enforced by nothing; the `Unread` rendering of a ceiling is
/// held on planted data in `consumed_tests.rs`.)
#[test]
fn every_policy_ceiling_row_reads_yes_with_nothing_unread() {
    for key in ["policy.max_leverage", "policy.max_notional_per_order"] {
        let r = resolve_file_row(&default_row(key, "policy"));
        assert!(r.consumed, "{key}");
        assert_eq!(r.read_cell(), "yes", "{key}");
        assert_eq!((r.why_unread, r.unread_verdict), (None, None), "{key}");
    }
}

/// **A key no table carries is NOT `yes`.** It used to inherit the "no row means consumed" default;
/// it reads `?`, is not consumed, and carries the "unknown to the consumption table" verdict.
#[test]
fn an_unknown_key_is_not_consumed_and_says_it_is_unknown() {
    let r = resolve_file_row(&default_row("config.no_such_key", "config"));
    assert_eq!(r.read_status, ReadStatus::Unknown);
    assert!(!r.consumed);
    assert_eq!(r.read_cell(), "?");
    assert_eq!(r.read_by, None);
    assert!(r.unread_verdict.is_some_and(|v| v.contains("UNKNOWN TO THE CONSUMPTION TABLE")));
    assert!(r.why_unread.is_some());
}

/// Every row `config show` and the wire can carry has an explicit verdict — none of them is the
/// `?` an unknown key earns. (The same property as `consumed::every_setting_key_has_an_explicit_read_status`,
/// asserted on the RENDERED rows, which is what an operator reads.)
#[test]
fn no_rendered_row_reads_unknown() {
    let d = crate::describe(None).unwrap();
    let unknown: Vec<String> = file_rows(&d, None, false)
        .into_iter()
        .filter(|r| r.read_status == ReadStatus::Unknown)
        .map(|r| r.key)
        .collect();
    assert!(unknown.is_empty(), "rendered rows with no consumption verdict: {unknown:?}");
}
