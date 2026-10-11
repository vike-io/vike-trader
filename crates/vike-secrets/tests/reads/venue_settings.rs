//! `vike_secrets::venue_setting::load_venue_settings` over a real store, and the journalled writer.

use std::assert_matches;
use vike_secrets::venue_setting::{SettingTier, load_venue_settings};

fn planted() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    vike_secrets::plant_settings_rows(dir.path(), &vike_secrets::StoredSettings::default())
        .expect("a fresh store plants");
    dir
}

#[test]
fn no_database_is_an_empty_snapshot_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load_venue_settings(dir.path()).expect("absent is not an error").is_empty());
}

#[test]
fn rows_are_grouped_by_venue() {
    let dir = planted();
    vike_secrets::set_venue_setting_in(dir.path(), "polymarket", None, "PROXY_PORT", "11080")
        .unwrap();
    vike_secrets::set_venue_setting_in(dir.path(), "ibkr", Some("demo"), "HOST", "<host>")
        .unwrap();
    let all = load_venue_settings(dir.path()).unwrap();
    assert_eq!(all.keys().map(String::as_str).collect::<Vec<_>>(), vec!["ibkr", "polymarket"]);
    assert_eq!(all["polymarket"].get(SettingTier::Any, "proxy_port"), Some("11080"));
    assert_eq!(all["ibkr"].get(SettingTier::Demo, "host"), Some("<host>"));
    assert_eq!(all["ibkr"].venue(), "ibkr");
}

/// Every `venue_setting` write lands in the change journal, and a SECRET field's value does not.
#[test]
fn a_journalled_write_records_the_key_and_redacts_a_secret() {
    let dir = planted();
    let journal = || vike_secrets::AccountJournal {
        actor: vike_model::change_journal::Actor::cli("test"),
        proc: vike_model::change_journal::Proc::new("test", 1, "0.0.0"),
        now_ms: 1,
    };
    let (previous, err) = vike_secrets::set_venue_setting_in_journalled(
        dir.path(),
        "polymarket",
        None,
        "SOCKS_PROXY",
        "socks5h://u:hunter2@h:1",
        true,
        journal(),
    )
    .unwrap();
    assert_eq!(previous, None);
    assert!(err.is_none());
    vike_secrets::set_venue_setting_in_journalled(
        dir.path(),
        "polymarket",
        None,
        "PROXY_HOST",
        "<host>",
        false,
        journal(),
    )
    .unwrap();

    let mut text = String::new();
    for entry in std::fs::read_dir(dir.path().join("state").join("changes")).unwrap().flatten() {
        text.push_str(&std::fs::read_to_string(entry.path()).unwrap());
    }
    assert!(text.contains("venue.polymarket.socks_proxy"), "{text}");
    assert!(text.contains("venue.polymarket.proxy_host") && text.contains("<host>"), "{text}");
    assert!(!text.contains("hunter2"), "a secret value reached the journal: {text}");
}

/// The journalled writer never brings a database into existence.
#[test]
fn a_journalled_write_refuses_a_box_with_no_database_and_creates_none() {
    let dir = tempfile::tempdir().unwrap();
    let journal = vike_secrets::AccountJournal {
        actor: vike_model::change_journal::Actor::cli("test"),
        proc: vike_model::change_journal::Proc::new("test", 1, "0.0.0"),
        now_ms: 1,
    };
    let (e, journal_error) = *vike_secrets::set_venue_setting_in_journalled(
        dir.path(),
        "polymarket",
        None,
        "PROXY_HOST",
        "h",
        false,
        journal,
    )
    .expect_err("no database");
    assert_matches!(e.kind, vike_secrets::DbErrorKind::NoDatabase, "{e}");
    assert!(journal_error.is_none(), "nothing beside a nonexistent database has a journal to open");
    assert!(!vike_secrets::db_path_in(dir.path()).exists(), "a database was created");
}

/// **A REFUSED write's OWN journal record can also fail to append, and that must not be silently
/// dropped either.** Regression guard for the shape `result.map(|previous| (previous,
/// journal_error))` used to have: it is a no-op on `Err`, so a refusal whose own journal record
/// also failed to write reported only the refusal, with no way for the caller to learn the ledger
/// did not record it either. Mirrors `crates/vike-cli/src/cmd/settings_write.rs`'s
/// `set_setting_journalled_within`, which already surfaces both halves for the settings-row case.
///
/// `tier: Some("any")` is a REAL refusal `set_venue_setting_in` states on its own (the stored word
/// no production caller can spell — `crate::schema::stored_venue_setting_tier`'s refusal), reached
/// here directly rather than through `vike-cli`'s own pre-validation, exactly as this function's
/// own doc says a caller besides the CLI (the desktop) can. The journal directory is a FILE, not a
/// directory, so the append attempt for THAT refusal fails too.
#[test]
fn a_refused_writes_own_journal_failure_is_also_surfaced() {
    let dir = planted();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    std::fs::write(dir.path().join("state").join("changes"), b"not a directory").unwrap();
    let journal = vike_secrets::AccountJournal {
        actor: vike_model::change_journal::Actor::cli("test"),
        proc: vike_model::change_journal::Proc::new("test", 1, "0.0.0"),
        now_ms: 1,
    };
    let (e, journal_error) = *vike_secrets::set_venue_setting_in_journalled(
        dir.path(),
        "polymarket",
        Some("any"),
        "PROXY_HOST",
        "h",
        false,
        journal,
    )
    .expect_err("a stored tier of \"any\" is refused by the underlying write");
    assert!(!matches!(e.kind, vike_secrets::DbErrorKind::NoDatabase), "{e}");
    assert!(
        journal_error.is_some(),
        "the refusal's OWN journal record must be reported as failed too, not silently dropped"
    );
}

/// **A REFUSED venue write is itself journalled — as a refusal, naming the key, with a secret
/// field's value redacted.** The `Outcome::Refused` arm of the journalled writer had no test: the
/// only refusal case above never READS the journal (its journal is a file on purpose), and the Ok
/// case never refuses. `tier: Some("any")` is the refusal `set_venue_setting_in` states on its own;
/// the field is a declared SECRET one, so the attempted value must not reach the ledger even
/// though nothing was stored.
#[test]
fn a_refused_write_is_journalled_as_refused_with_a_secret_value_redacted() {
    const CANARY: &str = "socks5h://u:hunter2-canary@h:1";
    let dir = planted();
    let journal = vike_secrets::AccountJournal {
        actor: vike_model::change_journal::Actor::cli("test"),
        proc: vike_model::change_journal::Proc::new("test", 1, "0.0.0"),
        now_ms: 1,
    };
    let (_refusal, journal_error) = *vike_secrets::set_venue_setting_in_journalled(
        dir.path(),
        "polymarket",
        Some("any"),
        "SOCKS_PROXY",
        CANARY,
        true,
        journal,
    )
    .expect_err("a stored tier of \"any\" is refused by the underlying write");
    assert!(journal_error.is_none(), "the journal is writable, so the refusal is recorded");

    let mut text = String::new();
    for entry in std::fs::read_dir(dir.path().join("state").join("changes")).unwrap().flatten() {
        text.push_str(&std::fs::read_to_string(entry.path()).unwrap());
    }
    assert_eq!(text.lines().count(), 1, "one refused attempt is one record: {text}");
    assert!(text.contains("refused"), "the record says the write was refused: {text}");
    assert!(text.contains("socks_proxy"), "…and names the key it was refused for: {text}");
    assert!(!text.contains("hunter2-canary"), "a secret value reached the journal: {text}");
    assert!(
        vike_secrets::venue_setting::load_venue_settings(dir.path())
            .unwrap()
            .get("polymarket")
            .is_none_or(|s| s.rows().next().is_none()),
        "a refused write stored nothing"
    );
}
