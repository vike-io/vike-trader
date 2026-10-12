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

/// The `READ` cell's three words, from the three `(consumed, read_by)` shapes.
#[test]
fn the_read_cell_names_the_binary_a_library_or_nothing() {
    let base = resolve_file_row(&ResolvedSetting {
        key: "config.tradehub_addr".to_string(),
        section: "config",
        value: None,
        default: None,
        origin: Origin::Default,
        origin_value: None,
        adjusted: false,
    });
    // The real consumption table drives the real rows; pin the rendering rule itself on
    // synthesized field states so the three arms stay covered whatever the table says.
    let mut r = base;
    r.consumed = false;
    r.read_by = None;
    assert_eq!(r.read_cell(), "NO");
    r.consumed = true;
    assert_eq!(r.read_cell(), "yes");
    r.read_by = Some("tradehub");
    assert_eq!(r.read_cell(), "tradehub");
}
