//! **The Data Manager's Polymarket proxy box writes `venue.polymarket.socks_proxy` and lands in the
//! change journal** — `vike_app_core::ui::tool_views::save_polymarket_proxy`.
//!
//! Since decision 0095 the proxy is a venue setting, not a credential: the box writes the declared
//! SECRET field through `vike_secrets::set_venue_setting_in_journalled`, so the record is a
//! `set_setting` whose value is `<secret>` — and no part of a bought proxy's `user:pass` may reach the
//! ledger. The store path stays a PARAMETER (the settings directory is its parent), which is also what
//! lets this file exist without touching a developer's real store.

use vike_app_core::ui::tool_views::save_polymarket_proxy;
use vike_connections::CredentialWrite;
use vike_model::change_journal::{CHANGES_SUBDIR, KIND_SET_SETTING, Proc, month_file_name};
use vike_secrets::venue_setting::{SettingTier, load_venue_settings};

/// 2026-08-21T00:00:00Z.
const T: i64 = 1_787_356_800_000;

/// The two credential-bearing halves of the proxy URL — chosen to share no six-character run with
/// anything the record legitimately holds.
const PROXY_USER: &str = "zqxjvw7413mfbphgn";
const PROXY_PASS: &str = "dgnhpbfm6528317wvjxqz";

const MIN_LEAK_WINDOW: usize = 6;

fn proxy_url() -> String {
    format!("socks5h://{PROXY_USER}:{PROXY_PASS}@198.51.100.7:1080")
}

fn assert_no_secret_window(raw: &str, secret: &str) {
    let chars: Vec<char> = secret.chars().collect();
    for start in 0..=chars.len() - MIN_LEAK_WINDOW {
        for end in (start + MIN_LEAK_WINDOW)..=chars.len() {
            let window: String = chars[start..end].iter().collect();
            assert!(
                !raw.contains(&window),
                "the journal carries {window:?} of the proxy credential: {raw}"
            );
        }
    }
}

/// A settings directory WITH a settings database — the row lives there.
fn planted() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    vike_secrets::plant_settings_rows(
        dir.path(),
        &vike_secrets::StoredSettings {
            arming: vec![vike_secrets::ArmingRow {
                venue: "polymarket".to_string(),
                label: None,
                mode: "paper".to_string(),
                max_exposure: None,
            }],
            ..Default::default()
        },
    )
    .expect("a fresh store plants");
    dir
}

/// THE gate: one save, the row written, one `gui` `set_setting` record — and no part of the URL.
#[test]
fn saving_the_proxy_writes_the_secret_row_and_journals_no_part_of_it() {
    let dir = planted();
    let proc = Proc::new("vike-test", 4711, "0.1.0");
    save_polymarket_proxy(
        &proxy_url(),
        CredentialWrite { settings_dir: dir.path(), journal: None, proc: &proc, now_ms: T },
    );

    let all = load_venue_settings(dir.path()).unwrap();
    assert_eq!(
        all["polymarket"].get(SettingTier::Any, "socks_proxy"),
        Some(proxy_url().as_str()),
        "the row landed"
    );

    let raw = std::fs::read_to_string(
        dir.path().join("state").join(CHANGES_SUBDIR).join(month_file_name(T)),
    )
    .expect("the change journal was written");
    assert_eq!(raw.lines().count(), 1, "one save is one record: {raw}");
    assert!(!raw.contains(&proxy_url()), "the ledger carries the proxy URL: {raw}");
    assert_no_secret_window(&raw, PROXY_USER);
    assert_no_secret_window(&raw, PROXY_PASS);
    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("one JSON object");
    assert_eq!(v["kind"], KIND_SET_SETTING);
    assert_eq!(v["actor"]["origin"], "gui", "the Data Manager box is the GUI channel: {raw}");
    assert!(raw.contains("venue.polymarket.socks_proxy"), "{raw}");
}

/// No settings database: nothing is written anywhere and none is created (a venue setting lives
/// only in the database, and creating one would switch the box's credential store).
#[test]
fn a_box_with_no_settings_database_saves_nothing_and_creates_none() {
    let dir = tempfile::tempdir().unwrap();
    let proc = Proc::new("vike-test", 4711, "0.1.0");
    save_polymarket_proxy(
        &proxy_url(),
        CredentialWrite { settings_dir: dir.path(), journal: None, proc: &proc, now_ms: T },
    );
    assert!(!vike_secrets::db_path_in(dir.path()).exists(), "a database was created");
}

/// The box's text field hands over whatever was typed or pasted. The field grammar validates the
/// TRIMMED value, so the row stores that value — a pasted proxy URL with a trailing space or
/// newline must not land in the store (and so in `config show` and the next write's report) with
/// the padding on it. The same rule `vike-cli config set venue.*` applies.
#[test]
fn a_padded_box_value_is_stored_trimmed() {
    let dir = planted();
    let proc = Proc::new("vike-test", 4711, "0.1.0");
    save_polymarket_proxy(
        &format!("  {}\n", proxy_url()),
        CredentialWrite { settings_dir: dir.path(), journal: None, proc: &proc, now_ms: T },
    );
    let all = load_venue_settings(dir.path()).unwrap();
    assert_eq!(
        all["polymarket"].get(SettingTier::Any, "socks_proxy"),
        Some(proxy_url().as_str()),
        "the stored row is the value the grammar checked"
    );
}
