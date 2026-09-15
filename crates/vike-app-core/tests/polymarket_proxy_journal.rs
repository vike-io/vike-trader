//! **The Data Manager's Polymarket proxy box lands in the change journal** —
//! `vike_app_core::tool_views::save_polymarket_proxy`, the second of the three credential-store
//! writers.
//!
//! It is a credential write like any other: a bought SOCKS5 egress carries `user:pass@` in its URL,
//! it goes through the workspace's one sanctioned upsert into `secrets.env`, and until now the only
//! trace of a change was a `tracing::info!` line that `vike_model::change_journal`'s module doc
//! measures as deleted within days.
//!
//! Two things here are specific to this site and are gated as such:
//!
//!   * **the tier is [`vike_model::change_journal::TIER_UNTIERED`]**, because `POLY_SOCKS_PROXY`
//!     governs every Polymarket tier at once — `"SIM"`/`"DEMO"`/`"LIVE"` would each assert something
//!     false about the other two;
//!   * **the store path is a PARAMETER**. This function used to call
//!     `vike_secrets::workspace_dotenv_path`, the `$VIKE_SETTINGS_DIR`-BLIND resolver, so on a
//!     deployment that names its settings directory the box wrote into whatever project the working
//!     directory sat above. That is also why this file can exist at all: a test could not have
//!     called it without rewriting the developer's real credential store.

use std::path::Path;

use vike_app_core::tool_views::save_polymarket_proxy;
use vike_connections::CredentialWrite;
use vike_model::change_journal::{
    CHANGES_SUBDIR, ChangeJournal, KIND_CREDENTIAL_WRITE, Proc, TIER_UNTIERED, month_file_name,
};

/// 2026-08-21T00:00:00Z.
const T: i64 = 1_787_356_800_000;

/// The two credential-bearing halves of the proxy URL.
///
/// ⚠ Chosen to share no six-character run with anything the record legitimately holds —
/// `POLY_SOCKS_PROXY`, `polymarket`, `untiered`, `secrets.env`, `credential_write`, the digits of
/// [`T`]. Otherwise the window check below would be measuring coincidence rather than leakage.
const PROXY_USER: &str = "zqxjvw7413mfbphgn";
const PROXY_PASS: &str = "dgnhpbfm6528317wvjxqz";

const MIN_LEAK_WINDOW: usize = 6;

fn proxy_url() -> String {
    format!("socks5h://{PROXY_USER}:{PROXY_PASS}@198.51.100.7:1080")
}

fn assert_no_secret_window(raw: &str, secret: &str) {
    let chars: Vec<char> = secret.chars().collect();
    assert!(chars.len() >= MIN_LEAK_WINDOW, "the fixture is shorter than the window");
    for start in 0..=chars.len() - MIN_LEAK_WINDOW {
        for end in (start + MIN_LEAK_WINDOW)..=chars.len() {
            let window: String = chars[start..end].iter().collect();
            assert!(
                !raw.contains(&window),
                "the change journal carries a {}-character window of the proxy credential \
                 ({window:?}). `Change::credential_write` takes no value parameter, so this means a \
                 value reached a cell that was supposed to hold a NAME.\nledger: {raw}",
                end - start
            );
        }
    }
}

/// THE gate: one save, one `gui` record, the untiered tier — and no part of the proxy's `user:pass`.
#[test]
fn saving_the_proxy_records_one_untiered_gui_credential_write() {
    let dir = tempfile::tempdir().unwrap();
    let settings = dir.path().join("settings");
    let state = settings.join("state");
    std::fs::create_dir_all(&settings).unwrap();
    let store = settings.join("secrets.env");
    std::fs::write(&store, "# hand-edited, keep me\nBINANCE_LIVE_API_KEY=untouched\n").unwrap();

    let journal = ChangeJournal::in_state_dir(&state, Proc::new("vike-test", 4711, "0.1.0"));
    save_polymarket_proxy(
        &proxy_url(),
        CredentialWrite { store: &store, journal: Some(&journal), now_ms: T },
    );

    // Anti-vacuity FIRST: the proxy really landed, and nothing else moved. Without this the whole
    // test would stay green for a `save_polymarket_proxy` that had stopped writing at all.
    let saved = std::fs::read_to_string(&store).unwrap();
    assert!(saved.contains(&format!("POLY_SOCKS_PROXY={}", proxy_url())), "{saved}");
    assert!(saved.contains("# hand-edited, keep me"), "the upsert preserves every line: {saved}");
    assert!(saved.contains("BINANCE_LIVE_API_KEY=untouched"), "{saved}");

    let raw = std::fs::read_to_string(state.join(CHANGES_SUBDIR).join(month_file_name(T)))
        .expect("the change journal was written");
    assert_eq!(raw.lines().count(), 1, "one save is one record: {raw}");

    // The whole URL, and then every window of each credential half of it — asserted FIRST, before
    // any field is inspected, so this is the assertion that reports when a value reaches the ledger
    // rather than a field-equality check that happens to notice.
    assert!(!raw.contains(&proxy_url()), "the ledger carries the proxy URL: {raw}");
    assert_no_secret_window(&raw, PROXY_USER);
    assert_no_secret_window(&raw, PROXY_PASS);

    let v: serde_json::Value = serde_json::from_str(raw.trim_end()).expect("one JSON object");
    assert_eq!(v["kind"], KIND_CREDENTIAL_WRITE);
    assert_eq!(v["outcome"], "applied");
    assert_eq!(v["actor"]["origin"], "gui", "the Data Manager box is the GUI channel: {raw}");

    assert_eq!(v["target"]["store"], "secrets.env");
    assert_eq!(v["target"]["venue"], "polymarket");
    assert_eq!(
        v["target"]["tier"], TIER_UNTIERED,
        "one proxy governs every Polymarket tier — naming one of them would be a false claim about \
         the other two: {raw}"
    );
    assert_eq!(v["target"]["count"], 1);
    let keys: Vec<&str> =
        v["target"]["keys"].as_array().unwrap().iter().map(|k| k.as_str().unwrap()).collect();
    assert_eq!(keys, vec!["POLY_SOCKS_PROXY"]);
}

/// A journal-less surface writes NOTHING and invents no directory — the proxy still saves.
///
/// The honest degradation for a root whose boot walk found no project, and the same shape
/// `vike_model::change_journal`'s `a_journal_less_surface_writes_nothing` pins. Asserted against a
/// state directory that is never created, so "recorded nothing" is distinguishable from "recorded
/// somewhere else": an invented path would leave a directory behind.
#[test]
fn a_journal_less_proxy_save_still_writes_the_store_and_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("secrets.env");

    save_polymarket_proxy(
        &proxy_url(),
        CredentialWrite { store: &store, journal: None, now_ms: T },
    );

    let saved = std::fs::read_to_string(&store).expect("the store was still written");
    assert!(saved.contains("POLY_SOCKS_PROXY="), "{saved}");

    let siblings: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(siblings, vec!["secrets.env".to_string()], "a journal-less save invented a path");
    assert!(!Path::new(&dir.path().join(CHANGES_SUBDIR)).exists());
    assert!(!Path::new(&dir.path().join("state")).exists());
}
