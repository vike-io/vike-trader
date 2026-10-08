//! The `Directory` verb end-to-end (the Trade window's venue · account list, the owner's rulings of
//! 2026-09-30): a real PAPER node ([`vike_mount::build_paper_maker_core`]) + the real
//! [`vike_tradehub::server`] with a [`vike_tradehub::server::settings::SettingsShowSource`] over a REAL
//! settings directory, driven through the REAL client verb ([`vike_tradehub_client::directory`])
//! under [`Scope::Read`] — the OBSERVE key, which is the point: the owner ruled that it may read the
//! account list, and that it may never read a credential.
//!
//! What is proven:
//! - **Advertisement:** `Welcome.features` carries `"directory"`, so the client verb sends.
//! - **Names and active accounts:** every roster `venue` row with the venue's own spelling, and
//!   every ACTIVE `account` row — a retired account is not listed.
//! - **Only venues this node can mount:** a `venue` row the roster has dropped is not listed, every
//!   roster venue still is, and an active account on the dropped venue is still listed.
//! - **The projection:** each listed account's fields are the stored row's, field for field, with
//!   a non-default `venue_account_id` so a swapped field would show. **The field set itself is
//!   pinned on the raw frame**, and `armed` is not in it.
//! - **⚠ THE SECURITY CONTRACT:** with credentials in the store, no credential value and no
//!   credential KEY NAME reaches the wire, asserted over the raw bytes of the reply frame (below the
//!   typed decode, which would drop a field its struct does not name) and over the typed payload.
//! - **An account table the node cannot read is an ERROR, never an empty list:** a project with no
//!   database, a store older than the `account` table and a store whose `account` table is broken
//!   each answer an error that says so — an empty `accounts` would read as *this node has no
//!   accounts* while its snapshot may be running some. A read creates nothing.
//! - **No error names a filesystem path:** the four tests that go through `refused_with` assert it —
//!   the missing database, the older store, the unreadable file and the broken account table —
//!   because the reasons the store crate renders for itself name the database and the credential
//!   file absolutely. The two source-less tests do not go through it, and say so below: they assert
//!   the node's own sentence about the missing source or directory.
//! - **The honest-error shapes:** a node started without a settings source, one that resolved no
//!   settings directory, and a database that exists and will not read each answer an error —
//!   never an empty directory.
//! - **A venue without a title:** a NULL `venue.title` is sent as `None`; the node invents no
//!   spelling.
//! - **Pre-auth:** a peer that skips the handshake is refused, never answered the directory.
//!
//! Grouped into the `daemon` binary: plain tests, no `#[ignore]`, no crate-level `#![cfg]`, and no
//! process-global mutation — the settings directory is a `tempfile::TempDir`, so nothing touches
//! `std::env` or the CWD.

use std::net::{SocketAddr, TcpStream};
use std::path::Path;

use vike_tradehub::server;
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    FEATURE_DIRECTORY, NODE_PROTO_VERSION, Request, Response, Scope, read_frame, read_frame_raw,
    write_frame,
};

use crate::support::spawn_maker_node;

const TOKEN: &str = "DIRECTORY_TOKEN";
const OBSERVE_KEY: &[u8] = b"directory-observe-key";

/// The credential key NAMES planted in the store, each with a VALUE the reply must never carry.
/// ⚠ These are test sentinels, not keys: nothing authenticates with them.
const PLANTED: [(&str, &str); 2] = [
    ("BINANCE_LIVE_API_KEY", "planted-api-key-value-never-on-the-wire"),
    ("BINANCE_LIVE_API_SECRET", "planted-api-secret-value-never-on-the-wire"),
];

/// The pieces of the credential grid the planted names imply — the owner prefix and the two field
/// words — which the reply must not carry either, so a leak of a fragment is as red as a leak of a
/// whole name.
const FORBIDDEN_FRAGMENTS: [&str; 3] = ["BINANCE_LIVE_", "API_KEY", "API_SECRET"];

/// A source over `dir` with no environment and no hot-apply seam — this file tests the READ.
fn source_over(dir: &Path) -> server::settings::SettingsShowSource {
    server::settings::SettingsShowSource {
        settings_dir: Some(dir.to_path_buf()),
        env: Default::default(),
        hot: None,
    }
}

/// A settings directory whose database holds the roster, one labelled account and one retired one.
fn settings_dir_with_accounts() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp settings dir");
    vike_secrets::plant_settings_rows(dir.path(), &vike_secrets::StoredSettings::default())
        .expect("plant a store");
    vike_secrets::edit_account_in(
        dir.path(),
        vike_secrets::AccountEdit::Create { venue: "binance", tier: "demo", label: Some("HEDGE") },
    )
    .expect("a labelled account");
    let retired = vike_secrets::edit_account_in(
        dir.path(),
        vike_secrets::AccountEdit::Create { venue: "okx", tier: "demo", label: Some("OLD") },
    )
    .expect("a second account")
    .after
    .expect("its row")
    .id;
    vike_secrets::edit_account_in(
        dir.path(),
        vike_secrets::AccountEdit::SetActive { id: retired, active: false },
    )
    .expect("retire it");
    dir
}

/// A connection to `addr` authenticated under the OBSERVE key, the handshake done and nothing else
/// sent — driven at the frame level, so what follows can read the reply below the typed decode.
fn observe_stream(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(OBSERVE_KEY, &nonce, NODE_PROTO_VERSION, Scope::Read);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Read, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("auth reply") {
        Response::AuthOk { .. } => {}
        other => panic!("expected AuthOk, got {other:?}"),
    }
    stream
}

/// The `features` a node advertises in its `Welcome`, read at the frame level so the assertion is
/// on the advertisement itself, not on the client verb's behaviour above it.
fn welcome_features(addr: SocketAddr) -> Vec<String> {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { features, .. } => features,
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// The body of the node's `Directory` reply frame exactly as it put it on the wire. Read BELOW the
/// typed decode on purpose: a typed decode silently drops a field its struct does not name, so a
/// leak through such a field is the one an assertion over the typed payload could not see.
fn raw_directory_reply(addr: SocketAddr) -> String {
    let mut stream = observe_stream(addr);
    write_frame(&mut stream, &Request::Directory).expect("directory request");
    let body = read_frame_raw(&mut stream).expect("the reply frame");
    String::from_utf8(body).expect("a JSON reply is UTF-8")
}

#[test]
fn the_directory_crosses_the_wire_with_names_and_active_accounts_only() {
    let dir = settings_dir_with_accounts();
    let settings = server::settings::SettingsShowSource {
        settings_dir: Some(dir.path().to_path_buf()),
        env: Default::default(),
        hot: None,
    };
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(settings));

    let got = vike_tradehub_client::directory(addr, OBSERVE_KEY).expect("advertised ⇒ served");

    let ctrader = got.venues.iter().find(|v| v.name == "ctrader").expect("the roster is listed");
    assert_eq!(ctrader.title.as_deref(), Some("cTrader"));
    assert_eq!(got.venues.len(), vike_model::VENUES.len());

    let hedge = got
        .accounts
        .iter()
        .find(|a| a.label.as_deref() == Some("HEDGE"))
        .expect("the labelled account");
    assert_eq!((hedge.venue.as_str(), hedge.tier.as_str()), ("binance", "demo"));
    assert!(
        got.accounts.iter().all(|a| a.label.as_deref() != Some("OLD")),
        "a retired account is not listed"
    );
}

#[test]
fn the_welcome_advertises_the_directory() {
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, None);
    let features = welcome_features(addr);
    assert!(features.iter().any(|f| f == FEATURE_DIRECTORY));
}

/// **The directory lists only venues this node can mount.** The store keeps a `venue` row the roster
/// has dropped ON PURPOSE (`vike_secrets`'s `ensure_venue_rows` never deletes one, because a live
/// `account` row may still reference it), so listing every row would hand a picker a venue this
/// node cannot mount. The venue list is therefore filtered to the node's own roster
/// (`vike_model::VENUES`); every roster venue is still listed.
///
/// The ACCOUNT list is NOT filtered the same way, and that is asserted too: an active account on a
/// dropped venue is a fact the operator needs (the node holds an account it can no longer run), so it
/// is listed, and a caller falls back to the account's own venue key.
#[test]
fn a_venue_the_roster_has_dropped_is_not_listed_but_its_active_account_still_is() {
    // Not a spelling any roster id can have (roster ids are lowercase letters only), so no future
    // venue can ever collide with it.
    const DROPPED: &str = "dropped-venue";
    let dir = settings_dir_with_accounts();
    // The row an older release's roster would have left behind. A second connection plants it:
    // nothing in this crate's writers creates a venue the roster does not name. `ROLLBACK` first,
    // because the helper hands the connection back inside the write lock it took.
    vike_secrets::hold_write_lock(dir.path())
        .execute_batch(&format!(
            "ROLLBACK; INSERT INTO venue (name, title) VALUES ('{DROPPED}', 'Dropped Venue');"
        ))
        .expect("plant a venue row the roster does not name");
    vike_secrets::edit_account_in(
        dir.path(),
        vike_secrets::AccountEdit::Create { venue: DROPPED, tier: "demo", label: None },
    )
    .expect("an active account on the dropped venue");

    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));
    let got = vike_tradehub_client::directory(addr, OBSERVE_KEY).expect("advertised ⇒ served");

    let listed: std::collections::BTreeSet<&str> =
        got.venues.iter().map(|v| v.name.as_str()).collect();
    assert!(!listed.contains(DROPPED), "a venue the roster dropped was listed: {listed:?}");
    let roster: std::collections::BTreeSet<&str> = vike_model::VENUES.iter().copied().collect();
    assert_eq!(listed, roster, "every roster venue is listed, and nothing but the roster");
    assert!(
        got.accounts.iter().any(|a| a.venue == DROPPED),
        "an active account on a dropped venue is still listed: {:?}",
        got.accounts
    );
}

/// The node's projection of a stored row onto the wire loses nothing and swaps nothing. A fixture
/// with every field at its default would pass a mapping that crossed two fields, so this one gives
/// one account a book (and the two accounts differ in venue, label and tier), and then compares
/// EVERY listed account with the store reader's own row — the expectation is the store's, not a
/// number written here.
#[test]
fn a_listed_accounts_fields_are_the_stored_rows_own() {
    let dir = tempfile::tempdir().expect("temp settings dir");
    vike_secrets::plant_settings_rows(dir.path(), &vike_secrets::StoredSettings::default())
        .expect("plant a store");
    let hedge = vike_secrets::edit_account_in(
        dir.path(),
        vike_secrets::AccountEdit::Create { venue: "binance", tier: "demo", label: Some("HEDGE") },
    )
    .expect("a labelled account")
    .after
    .expect("its row")
    .id;
    vike_secrets::edit_account_in(
        dir.path(),
        vike_secrets::AccountEdit::Create { venue: "okx", tier: "paper", label: None },
    )
    .expect("an unlabelled paper account");
    vike_secrets::set_venue_account_id_in(
        dir.path(),
        hedge,
        Some("U1234567"),
        false,
        vike_secrets::BookSource::Operator,
    )
    .expect("the broker's own number for the labelled account");

    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));
    let got = vike_tradehub_client::directory(addr, OBSERVE_KEY).expect("advertised ⇒ served");

    // The non-default cell is really on the wire, for the account it belongs to.
    let listed = got.accounts.iter().find(|a| a.id == hedge).expect("the labelled account");
    assert_eq!(listed.venue_account_id.as_deref(), Some("U1234567"));

    // …and every listed account IS the stored row, field for field, in the store's own order.
    let stored = match vike_secrets::resolve_accounts_in(dir.path()).expect("read the accounts") {
        vike_secrets::Accounts::Known(rows) => rows,
        other => panic!("a database store answers its account table: {other:?}"),
    };
    let active: Vec<_> = stored.iter().filter(|a| a.active).collect();
    assert_eq!(got.accounts.len(), active.len(), "every active account, and only those");
    for (wire, row) in got.accounts.iter().zip(active) {
        assert_eq!(
            (
                wire.id,
                wire.venue.as_str(),
                wire.label.as_deref(),
                wire.tier.as_str(),
                wire.venue_account_id.as_deref()
            ),
            (
                row.id,
                row.venue.as_str(),
                row.label.as_deref(),
                row.tier.as_str(),
                row.venue_account_id.as_deref()
            ),
            "account {} crossed the wire as something other than its row",
            row.id
        );
    }
}

/// ⚠ THE SECURITY CONTRACT. The owner's ruling lets the observe key read the account list and no
/// API key, so with credentials in the store the reply carries NO credential value, NO credential
/// key NAME, and nothing else from the `credential` table. (The reply DOES still say which venues
/// hold which tiers of account, and that is the disclosure the ruling licenses: `directory()`'s
/// doc argues it. These assertions pin only what the ruling withholds.)
///
/// Asserted twice: over the raw bytes of the reply frame, and over the typed payload re-serialized
/// — the redaction pin `settings_show.rs` makes, one level lower.
#[test]
fn the_directory_carries_no_credential_value_and_no_key_name() {
    let dir = settings_dir_with_accounts();
    let updates: Vec<(String, String)> =
        PLANTED.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect();
    vike_secrets::save_credentials_to_store(
        dir.path(),
        vike_secrets::Table::Credential,
        &updates,
        Some(&vike_bridge_core::credentials::classify_credential_name),
    )
    .expect("plant two credentials");

    // The fixture holds what this test says it holds. Without these two checks a green below could
    // only mean there was nothing in the store to leak.
    let held = vike_secrets::resolve_store_in(dir.path(), vike_secrets::Table::Credential)
        .expect("read the planted store")
        .secrets
        .into_map();
    for (name, value) in PLANTED {
        assert_eq!(held.get(name).map(String::as_str), Some(value), "{name} is in the store");
    }
    let owner = vike_secrets::resolve_account_keys_in(dir.path())
        .expect("read the key names")
        .expect("a database store has an account table")
        .into_iter()
        .find(|(_, keys)| PLANTED.iter().all(|(name, _)| keys.names.iter().any(|n| n == name)))
        .map(|(id, _)| id)
        .expect("the planted keys are filed against an account");

    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));
    let wire = raw_directory_reply(addr);
    let got = vike_tradehub_client::directory(addr, OBSERVE_KEY).expect("advertised ⇒ served");
    let typed = serde_json::to_string(&got).expect("serialize the payload");

    // The account that OWNS the credentials is in the reply — the very row a leak would ride on —
    // and the raw bytes are a Directory reply, not an error sentence that happens to be clean.
    assert!(got.accounts.iter().any(|a| a.id == owner), "the owning account is listed");
    assert!(
        matches!(serde_json::from_str::<Response>(&wire), Ok(Response::Directory(_))),
        "the raw frame is a Directory reply: {wire}"
    );

    // Every value, every whole key name, and the fragments of the credential grid they imply.
    let mut forbidden: Vec<&str> = FORBIDDEN_FRAGMENTS.to_vec();
    for (name, value) in PLANTED {
        forbidden.extend([name, value]);
    }
    for needle in forbidden {
        for (what, doc) in [("the raw reply frame", &wire), ("the typed payload", &typed)] {
            assert!(!doc.contains(needle), "`{needle}` crossed the wire in {what}: {doc}");
        }
    }
}

/// ⚠ THE FIELD SET, pinned on the raw frame. The Directory reply is a disclosure to the observe key,
/// so its SHAPE is the contract: the reply is `venues` and `accounts`, a venue row is `name` and
/// `title`, and an account row is `id`, `venue`, `label`, `tier` and `venue_account_id`. Nothing
/// else. In particular **`armed` is deliberately NOT sent**: it is outside the owner's ruling (venue,
/// label, mode, no API keys), nothing reads it, and a field that has shipped cannot be taken back.
///
/// Read below the typed decode, which drops any key its struct does not name — and a reappearing
/// key is precisely the thing this test is about — and compared as an exact SET, so a field that
/// comes back under any name fails here, `armed` first. The leak test above holds the other half:
/// no key name and no value.
#[test]
fn the_directory_reply_carries_exactly_these_fields_and_never_armed() {
    use std::collections::BTreeSet;

    let dir = settings_dir_with_accounts();
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));
    let wire = raw_directory_reply(addr);
    let frame: serde_json::Value = serde_json::from_str(&wire).expect("the reply is JSON");
    let reply = frame
        .get("Directory")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("the raw frame is a Directory reply: {wire}"));

    let keys = |object: &serde_json::Map<String, serde_json::Value>| -> BTreeSet<String> {
        object.keys().cloned().collect()
    };
    let set =
        |names: &[&str]| -> BTreeSet<String> { names.iter().map(|n| (*n).to_string()).collect() };

    assert_eq!(keys(reply), set(&["venues", "accounts"]), "the reply's own keys: {wire}");
    let venues = reply["venues"].as_array().expect("venues is a list");
    let accounts = reply["accounts"].as_array().expect("accounts is a list");
    // The fixture really has rows to check: a loop over an empty list proves nothing.
    assert!(!venues.is_empty() && !accounts.is_empty(), "nothing to check in: {wire}");
    for venue in venues {
        assert_eq!(
            keys(venue.as_object().expect("a venue row is an object")),
            set(&["name", "title"]),
            "a venue row names exactly these fields: {venue}"
        );
    }
    for account in accounts {
        assert_eq!(
            keys(account.as_object().expect("an account row is an object")),
            set(&["id", "venue", "label", "tier", "venue_account_id"]),
            "an account row names exactly these fields, and `armed` is deliberately not one of \
             them: {account}"
        );
    }
    assert!(!wire.contains("armed"), "`armed` crossed the wire: {wire}");
}

/// What the client verb says when the node answers `Directory` with an error: `InvalidData`
/// carrying the node's own sentence, which is returned for the caller to check for its cause.
///
/// ⚠ Four error tests go through this — `a_directory_read_never_creates_the_database`,
/// `a_store_older_than_the_account_table_answers_an_error_naming_that`,
/// `a_database_that_will_not_read_answers_an_error_saying_so` and
/// `an_account_table_that_will_not_read_answers_an_error_saying_so` — so each asserts the same two
/// things about the sentence: it names no path (the settings directory's own, which is unique per
/// test, and no `/` at all), because what the store crate renders for itself — a `DbError`, a
/// `NoAccountTable` — names the database or the credential file absolutely, and the observe key is
/// owed none of that.
///
/// Two error tests do NOT go through it: `a_node_without_a_source_answers_an_honest_error` reads the
/// reply at the frame level (the node's own words about the missing source), and
/// `a_node_that_resolved_no_settings_directory_answers_an_honest_error` asserts the cause and the
/// error kind only.
fn refused_with(addr: SocketAddr, settings_dir: &Path) -> String {
    let err =
        vike_tradehub_client::directory(addr, OBSERVE_KEY).expect_err("an error, not a directory");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "a server-side error: {err}");
    let msg = err.to_string();
    assert!(
        !msg.contains(&settings_dir.display().to_string()) && !msg.contains('/'),
        "an error text names a filesystem path: {msg}"
    );
    msg
}

/// A node on a project with NO database — a box that has not migrated, or whose credential store
/// is still a file — has no account table to list from, so it answers an ERROR saying so and never
/// an empty directory (an empty `accounts` would read as *this node has no accounts* while its
/// snapshot may be running some). And the read creates nothing. The second half is the one that
/// matters for a verb the observe key may call: a store's existence is the whole of which
/// credential store answers (`vike_secrets::Backend`), so a read that could create the database
/// would turn "this box has no database" into "this box has an empty one" and retire every
/// credential in the file beside it.
#[test]
fn a_directory_read_never_creates_the_database() {
    let dir = tempfile::tempdir().expect("temp settings dir");
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));

    let msg = refused_with(addr, dir.path());

    assert!(
        msg.contains("directory unavailable") && msg.contains("no settings database"),
        "names the cause: {msg}"
    );
    let left_behind: Vec<_> = std::fs::read_dir(dir.path())
        .expect("list the settings directory")
        .map(|e| e.expect("an entry").file_name())
        .collect();
    assert!(left_behind.is_empty(), "a read created {left_behind:?}");
}

/// The other half of `Accounts::Unanswerable`: a database that opens and is older than the
/// `account` table (schema 1, the shape both live boxes were in before the migration) has nothing
/// to list accounts from, and says that instead of listing none.
#[test]
fn a_store_older_than_the_account_table_answers_an_error_naming_that() {
    let dir = tempfile::tempdir().expect("temp settings dir");
    vike_secrets::plant_schema_1(&vike_secrets::db_path_in(dir.path()), &[], &[])
        .expect("plant a schema-1 store");
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));

    let msg = refused_with(addr, dir.path());

    assert!(
        msg.contains("directory unavailable") && msg.contains("predates the account table"),
        "names the cause: {msg}"
    );
}

/// A database that EXISTS and will not read — here a file at the store's path that is not SQLite
/// at all — is an error that says so. Both read-error arms of `directory()` answer the same
/// sentence, but this input reaches only the VENUE read's: the venue table is read first, and a
/// file that is not a database fails to open before any account is asked for. The ACCOUNT read's
/// own arm is reached by `an_account_table_that_will_not_read_answers_an_error_saying_so` below: a
/// readable schema whose `account` table is broken, planted through the connection
/// `vike_secrets::hold_write_lock` hands back — it runs any SQL, which is how this crate plants a
/// broken table without declaring a `rusqlite` dependency of its own.
#[test]
fn a_database_that_will_not_read_answers_an_error_saying_so() {
    let dir = tempfile::tempdir().expect("temp settings dir");
    let db = vike_secrets::db_path_in(dir.path());
    std::fs::create_dir_all(db.parent().expect("the database's directory"))
        .expect("make the database's directory");
    std::fs::write(&db, "this is not a SQLite database\n".repeat(64))
        .expect("plant a file that is not a database");
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));

    let msg = refused_with(addr, dir.path());

    assert!(
        msg.contains("directory unavailable") && msg.contains("could not be read"),
        "names the cause: {msg}"
    );
}

/// The ACCOUNT read's own error arm: a store the VENUE read can open and the ACCOUNT read cannot. A
/// current-schema store whose `account` table is gone answers `read_venues_in` normally and then
/// fails the account `SELECT` at prepare, so `directory()` reaches the second of its two
/// `directory_unreadable` calls — the one the unreadable-file test above cannot reach.
///
/// Planted through `vike_secrets::hold_write_lock`'s connection, which runs any SQL. It hands the
/// connection back INSIDE the write lock it took, so the batch opens with `ROLLBACK` (the lock holds
/// nothing worth keeping) and the `DROP` then commits on its own.
///
/// The answer is the same fixed sentence as every other unreadable store, and it names no path.
#[test]
fn an_account_table_that_will_not_read_answers_an_error_saying_so() {
    let dir = tempfile::tempdir().expect("temp settings dir");
    vike_secrets::plant_settings_rows(dir.path(), &vike_secrets::StoredSettings::default())
        .expect("plant a store");
    vike_secrets::hold_write_lock(dir.path())
        .execute_batch("ROLLBACK; DROP TABLE account;")
        .expect("break the account table");
    // The fixture holds what this test says it holds: the VENUE read still answers, so what fails
    // below is the account read and nothing before it.
    assert!(
        !vike_secrets::read_venues_in(dir.path()).expect("the venue read opens").is_empty(),
        "the roster is still readable, or this proves nothing about the account arm"
    );
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));

    let msg = refused_with(addr, dir.path());

    assert!(
        msg.contains("directory unavailable") && msg.contains("could not be read"),
        "names the cause: {msg}"
    );
}

/// A venue whose title is NULL crosses the wire as `title: None`: the node invents no spelling for
/// it. The store holds a NULL on a box no writer has carried since the column was added, and for a
/// title somebody cleared; the node sends what the store holds. The CLIENT falls back to the venue's
/// key then (`WireDirectoryVenue::title`'s doc) — that fallback is the Trade window glue's, not this
/// verb's, so this pins only the node's half: no fallback and no invented name. The other venues
/// keep their spellings, so one NULL blanks nothing.
#[test]
fn a_venue_with_no_title_crosses_the_wire_as_none() {
    let dir = settings_dir_with_accounts();
    vike_secrets::hold_write_lock(dir.path())
        .execute_batch("ROLLBACK; UPDATE venue SET title = NULL WHERE name = 'binance';")
        .expect("clear one venue's title");
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));

    let got = vike_tradehub_client::directory(addr, OBSERVE_KEY).expect("advertised ⇒ served");

    let binance =
        got.venues.iter().find(|v| v.name == "binance").expect("the venue is still listed");
    assert_eq!(binance.title, None, "a NULL title is sent as `None`, never a fallback");
    let ctrader = got.venues.iter().find(|v| v.name == "ctrader").expect("another venue");
    assert_eq!(ctrader.title.as_deref(), Some("cTrader"), "one NULL blanks nothing else");
}

/// A peer that skips the handshake is refused, never answered the directory. `Request::Directory`
/// as the very first frame, and as the frame that should have been `Auth`, each earn `AuthDenied`:
/// the arm is post-auth, so the account list is not readable by anybody who has not presented a
/// key. The store holds accounts, so a reply that WAS a directory would not be an empty one.
#[test]
fn a_peer_that_skips_the_handshake_is_refused_not_answered() {
    let dir = settings_dir_with_accounts();
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(source_over(dir.path())));

    let mut first = TcpStream::connect(addr).expect("connect");
    write_frame(&mut first, &Request::Directory).expect("an unauthenticated directory request");
    match read_frame::<_, Response>(&mut first).expect("the refusal") {
        Response::AuthDenied { .. } => {}
        other => panic!("a first frame that is not Hello must be refused, got {other:?}"),
    }

    let mut second = TcpStream::connect(addr).expect("connect");
    write_frame(&mut second, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let Response::Welcome { .. } = read_frame::<_, Response>(&mut second).expect("welcome") else {
        panic!("expected Welcome");
    };
    write_frame(&mut second, &Request::Directory).expect("a directory request in place of Auth");
    match read_frame::<_, Response>(&mut second).expect("the refusal") {
        Response::AuthDenied { .. } => {}
        other => panic!("a second frame that is not Auth must be refused, got {other:?}"),
    }
}

/// A server constructed WITHOUT a settings source answers an honest error under Observe — never a
/// fabricated empty directory (the `StrategyStatus` identity-less shape, and `settings_show.rs`'s
/// twin of this test). Driven at the frame level because the client verb folds `Response::Error`
/// into an `io::Error`, and this pin is about the server's own words.
#[test]
fn a_node_without_a_source_answers_an_honest_error() {
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, None);
    let mut stream = observe_stream(addr);
    write_frame(&mut stream, &Request::Directory).expect("directory request");
    match read_frame::<_, Response>(&mut stream).expect("directory reply") {
        Response::Error(msg) => {
            assert!(msg.contains("settings source"), "the error names the missing source: {msg}");
        }
        other => panic!("a source-less node must answer Error, got {other:?}"),
    }
}

/// The other absence: a source whose boot walk found no project (`settings_dir: None`). The error
/// is the node's own sentence, surfaced by the client verb as `InvalidData`.
#[test]
fn a_node_that_resolved_no_settings_directory_answers_an_honest_error() {
    let settings = server::settings::SettingsShowSource {
        settings_dir: None,
        env: Default::default(),
        hot: None,
    };
    let (_mount, addr) = spawn_maker_node(TOKEN, OBSERVE_KEY, None, Some(settings));

    let err =
        vike_tradehub_client::directory(addr, OBSERVE_KEY).expect_err("an error, not a table");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("no settings directory"), "names the cause: {err}");
}
