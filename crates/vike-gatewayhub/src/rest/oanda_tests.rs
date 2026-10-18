//! The OANDA practice-token READS, tested where they can be named: `oanda_history_credentials` and
//! `read_oanda_history_token` are `pub(crate)` (they return the plaintext practice token, so no
//! dependent crate may call them), which is why these tests live in the crate and not in
//! `tests/oanda_credential.rs` (that suite drives the lane through the real server and the client's
//! real `RemoteKlines`, and keeps the startup line, the memo and the scope). Moved from there with
//! their fixtures; the fixtures are therefore written twice, once per side of the crate boundary.
//!
//! Drift register row R7, the credential half: the read holds the one practice key and nothing else,
//! three states are three answers, the permission finding is logged by the announcing read only, an
//! unreadable store is not an absent key, and no value reaches a log line.
//!
//! Every fixture plants ACCOUNT ROWS and writes their FIELDS (decision 0156 item 6) - no credential
//! NAME is composed or spelled - and every VALUE is an obviously fake, distinctive string, so "no
//! value reached a log line" is an assertion that could actually fail.

use vike_model::AccountTier;

use super::*;

/// Every event's level, then every field — `message` first — as ` name=value`, one line per event:
/// the shape the assertions below read (`starts_with("ERROR")`, and "no line carries a value", which
/// needs EVERY field — an `error = %e` field is where a store error's text lands, and a value there
/// would be as much a leak as one in the message). Projected from `vike_log::capture::captured`, the
/// workspace's one scoped capture.
fn under_capture<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    let (out, events) = vike_log::capture::captured(f);
    let mut lines = Vec::with_capacity(events.len());
    for e in &events {
        let mut line = format!("{} message={}", e.level, e.message);
        for (k, v) in &e.fields {
            line.push_str(&format!(" {k}={v}"));
        }
        lines.push(line);
    }
    (out, lines)
}

/// The token's FIELD on the practice account.
const TOKEN_FIELD: &str = vike_venues::credential_keys::API_KEY;
/// The oanda account's other field — the neighbour on the SAME account the read must leave.
const ACCOUNT_ID_FIELD: &str = "ACCOUNT_ID";

const PRACTICE_VALUE: &str = "fake-practice-token-7q4z";

/// One planted account: its venue, tier and label, and the `(field, value)` pairs filed against it.
type Planted = (&'static str, AccountTier, Option<&'static str>, Vec<(String, String)>);

/// A store's worth of accounts around the practice account, with the practice account's token set to
/// `token` (`None` = the account holds no token). The NEIGHBOURS the one-field read must leave
/// behind: the practice account's own account id, the LIVE oanda account's pair, the PAPER oanda
/// account's key, a LABELLED demo oanda account's key, and another venue's key. Every value
/// distinctive.
fn store_rows(token: Option<&str>) -> Vec<Planted> {
    let pair = |field: &str, value: &str| (field.to_string(), value.to_string());
    let mut practice = vec![pair(ACCOUNT_ID_FIELD, "fake-account-id-2k9d")];
    if let Some(token) = token {
        practice.push(pair(TOKEN_FIELD, token));
    }
    vec![
        ("oanda", AccountTier::Demo, None, practice),
        (
            "oanda",
            AccountTier::Live,
            None,
            vec![
                pair(TOKEN_FIELD, "fake-live-token-8m3x"),
                pair(ACCOUNT_ID_FIELD, "fake-live-account-5j1c"),
            ],
        ),
        ("oanda", AccountTier::Paper, None, vec![pair(TOKEN_FIELD, "fake-sim-token-3w7n")]),
        (
            "oanda",
            AccountTier::Demo,
            Some("ALT"),
            vec![pair(TOKEN_FIELD, "fake-labelled-token-9p5r")],
        ),
        ("binance", AccountTier::Live, None, vec![pair(TOKEN_FIELD, "fake-binance-key-4t8y")]),
    ]
}

/// Every neighbour, the practice account holding NO token.
fn neighbours() -> Vec<Planted> {
    store_rows(None)
}

/// The practice token plus every neighbour.
fn full_store_rows() -> Vec<Planted> {
    store_rows(Some(PRACTICE_VALUE))
}

/// Every planted value, the practice token included — what no log line may carry.
fn every_value() -> Vec<String> {
    let mut values: Vec<String> = store_rows(None)
        .into_iter()
        .flat_map(|(_, _, _, fields)| fields.into_iter().map(|(_, v)| v))
        .collect();
    values.push(PRACTICE_VALUE.to_string());
    values
}

/// Make the settings DATABASE — the only credential store — hold exactly `accounts`, 0600 on unix so
/// the permission finding is only what a test asks for. The store is created the one way a store
/// comes into being (`vike-cli secrets init`'s library half); each account is created with its tier
/// FIRST and its fields are then written against it by owner and field — the one way an account's
/// credential reaches a store. A store being REPLACED (a rotation, a removal) is rebuilt in this
/// test's own temp directory: the writer adds and replaces fields and never removes one.
fn write_store(settings: &std::path::Path, accounts: &[Planted]) {
    let db_dir = settings.join("db");
    if db_dir.exists() {
        std::fs::remove_dir_all(&db_dir).expect("drop this test's own previous store");
    }
    vike_secrets::create_store(Some(settings.to_str().expect("a utf-8 temp path")))
        .expect("create the empty store");
    for &(venue, tier, label, ref fields) in accounts {
        let created = vike_secrets::edit_account_in_unjournalled(
            settings,
            vike_secrets::AccountEdit::Create { venue, tier, label },
        )
        .expect("create the account");
        let id = created.after.expect("a create returns its row").id;
        if !fields.is_empty() {
            vike_secrets::save_owned_credentials_unjournalled(
                settings,
                &vike_secrets::CredentialOwner::Account(id),
                fields,
                vike_secrets::UpsertIntent::Add,
            )
            .expect("write the account's fields");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            vike_secrets::db_path_in(settings),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("chmod");
    }
}

/// Bytes that are not a database where the database belongs: a store that EXISTS and will not open
/// (`chmod 000` proves nothing as root, which CI is).
fn plant_unreadable_store(settings: &std::path::Path) {
    std::fs::create_dir_all(settings.join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(settings), b"not a sqlite database at all")
        .expect("plant the garbage");
}

/// The handle the boot would hand this root for a settings directory `dir` (the full scope).
fn at(dir: &std::path::Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
}

/// The handle a boot that resolved NO settings directory hands this root: no location.
fn nowhere() -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(None, vike_secrets::CredentialScope::Full)
}

/// **The lane's credential read RETURNS the practice account's one field and NOTHING else — from the
/// one store.** The settings DATABASE answers through a bound query on one account's one field; this
/// plants the token among the practice account's own account id, its live, paper and labelled
/// siblings and another venue's key, and reads it back. A read that went back to the whole store, or
/// an address that drifted to another account or field, answers a neighbour's value.
#[test]
fn the_oanda_history_read_holds_the_one_practice_key_and_nothing_else() {
    let dir = tempfile::tempdir().expect("a temp dir");
    write_store(dir.path(), &full_store_rows());
    assert!(
        dir.path().join("db").join("vike.db").is_file(),
        "the fixture must really be a database store, or this half proves nothing"
    );
    let from_db = oanda_history_credentials(&at(dir.path()), false).expect("a readable database");
    assert_eq!(
        from_db.as_ref().map(|token| token.expose().as_str()),
        Some(PRACTICE_VALUE),
        "the DATABASE arm: exactly the practice account's token"
    );
    // …and with the practice account holding NO token, no neighbour answers in its place.
    write_store(dir.path(), &neighbours());
    let without = oanda_history_credentials(&at(dir.path()), false).expect("a readable database");
    assert!(without.is_none(), "a neighbour answered for the practice token");
}

/// **Three states, three answers, and no value in any line logged on the way.** No settings
/// directory, no store and a store without the key are ABSENT; a store that exists and will not read
/// is UNREADABLE (bytes that are not a database where the database belongs) and says so in an
/// `error!`; a present key is the value. The permission finding is logged by the announcing (startup)
/// read and by no other.
#[test]
fn the_oanda_history_token_answers_three_states_and_no_line_carries_a_value() {
    let absent = tempfile::tempdir().expect("a temp dir");
    let without = tempfile::tempdir().expect("a temp dir");
    write_store(without.path(), &neighbours());
    let with = tempfile::tempdir().expect("a temp dir");
    write_store(with.path(), &full_store_rows());
    let unreadable = tempfile::tempdir().expect("a temp dir");
    plant_unreadable_store(unreadable.path());

    let (answers, lines) = under_capture(|| {
        [
            read_oanda_history_token(&nowhere(), true),
            read_oanda_history_token(&at(absent.path()), true),
            read_oanda_history_token(&at(without.path()), true),
            read_oanda_history_token(&at(with.path()), false),
            read_oanda_history_token(&at(unreadable.path()), false),
        ]
    });
    assert_eq!(
        answers,
        [
            Err(HistoryTokenError::NotConfigured),
            Err(HistoryTokenError::NotConfigured),
            Err(HistoryTokenError::NotConfigured),
            Ok(PRACTICE_VALUE.to_string()),
            Err(HistoryTokenError::StoreUnreadable),
        ]
    );
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
    assert!(errors[0].contains("NOT the same as the key being absent"), "{}", errors[0]);
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(&value)), "a value reached the log: {lines:?}");
    }
}

/// The finding a group- or world-readable store earns is logged by the ANNOUNCING read and by no
/// other — the connection memo's own read runs once per connection and must stay silent.
#[cfg(unix)]
#[test]
fn only_the_announcing_read_logs_the_stores_permission_finding() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("a temp dir");
    write_store(dir.path(), &full_store_rows());
    std::fs::set_permissions(
        vike_secrets::db_path_in(dir.path()),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("chmod");

    let (_, silent) = under_capture(|| read_oanda_history_token(&at(dir.path()), false));
    assert!(silent.is_empty(), "a per-connection read logged: {silent:?}");
    let (_, announced) = under_capture(|| read_oanda_history_token(&at(dir.path()), true));
    assert!(
        announced.iter().any(|l| l.starts_with("WARN")),
        "the startup read must report a store others can read: {announced:?}"
    );
    for value in every_value() {
        assert!(!announced.iter().any(|l| l.contains(&value)), "{announced:?}");
    }
}

/// **The presence word — every state its own answer, and no value in any line logged on the way**
/// (the design's §2.4; 0097's verdict 6). No settings directory, no store, a store without the key
/// and a store holding it BLANK are `Absent` — the answers the lane's own token read gives — a stored
/// key is `Present`, and a store that exists and will not read is `Unreadable`, logged once and never
/// folded into `Absent`.
#[test]
fn the_presence_probe_answers_every_state_and_no_line_carries_a_value() {
    let absent = tempfile::tempdir().expect("a temp dir");
    let without = tempfile::tempdir().expect("a temp dir");
    write_store(without.path(), &neighbours());
    let blank = tempfile::tempdir().expect("a temp dir");
    write_store(blank.path(), &store_rows(Some("   ")));
    let with = tempfile::tempdir().expect("a temp dir");
    write_store(with.path(), &full_store_rows());
    let unreadable = tempfile::tempdir().expect("a temp dir");
    plant_unreadable_store(unreadable.path());

    let (answers, lines) = under_capture(|| {
        [
            oanda_history_presence(&nowhere()),
            oanda_history_presence(&at(absent.path())),
            oanda_history_presence(&at(without.path())),
            oanda_history_presence(&at(blank.path())),
            oanda_history_presence(&at(with.path())),
            oanda_history_presence(&at(unreadable.path())),
        ]
    });
    assert_eq!(
        answers,
        [
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Absent,
            CredentialPresence::Present,
            CredentialPresence::Unreadable,
        ]
    );
    // The word agrees with the lane's own read on every one of those stores.
    for dir in [absent.path(), without.path(), blank.path(), with.path()] {
        let read = read_oanda_history_token(&at(dir), false);
        assert_eq!(
            read.is_ok(),
            oanda_history_presence(&at(dir)) == CredentialPresence::Present,
            "the presence word and the lane's read disagree about {}",
            dir.display()
        );
    }
    let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
    assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
    assert!(errors[0].contains("NOT the same as the key being absent"), "{}", errors[0]);
    for value in every_value() {
        assert!(!lines.iter().any(|l| l.contains(&value)), "a value reached the log: {lines:?}");
    }
}
