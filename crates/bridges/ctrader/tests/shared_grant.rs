//! **The sockets of one process share ONE rotating cTrader grant** — end to end against an
//! in-process fake venue that, once the grant is rotated, honours only the NEW access token.
//!
//! cTrader rotates the refresh token on every refresh and ends the sessions of the superseded
//! access token. Inside one `vike-tradehub` the exec actor, the data actor and the reconcile
//! client each used to carry their own copy of the pair (`crates/bridges/ctrader/src/conn/grant.rs`
//! tells the failure). These tests drive the sockets that REACH a venue — the connect, and the
//! reconcile client's revive — through `ConnConfig::grant`, with the rotation done on the cell the
//! way the exec actor does it (`SharedGrant::renew` with an injected refresher, so no OAuth call is
//! made). The token gate (`FakeCtrader::start_gated_on_access_token`) is the venue's side.
//!
//! ⚠ No config here carries a refresh token, so a regression that fell through to the real
//! `oauth::refresh` would stop at "NO refresh token is configured" instead of reaching a network.

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use vike_ctrader::CtraderReconClient;
use vike_ctrader::conn::grant::SharedGrant;
use vike_ctrader::conn::{ConnConfig, ConnError, connect_and_auth};
use vike_ctrader::oauth::Token;
use vike_ctrader::token_store::TokenPersist;
use vike_exec::recon::ReconClient;
use vike_model::AccountTier;

use common::{AcceptedToken, FakeCtrader, NoopSink};

// ── the rotation reaches the credential STORE, exactly once ──────────────────────────────────────
//
// These live here and not beside `SharedGrant` because they seed a settings database with the
// record-less writer bodies, and `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs`
// reads every `crates/*/src` file — inline test modules included — as a caller of a writer. `tests/`
// is out of that gate's scope on purpose (a test writing a temp store is not a surface a user's
// keys reach).

/// 2026-08-21T00:00:00Z — the injected instant every rotation below is stamped with.
const T: i64 = 1_787_356_800_000;

fn token(access: &str, refresh: &str) -> Token {
    Token {
        access_token: access.to_string(),
        refresh_token: refresh.to_string(),
        expires_in: 2_592_000,
    }
}

/// A settings DATABASE under `dir` holding one ctrader demo account with the pair
/// `(access, refresh)`; returns the account row's id. Created the way a fresh box gets one
/// (`vike-cli secrets init`'s library half) and seeded through the record-less writer bodies.
fn store_with_account(dir: &Path, access: &str, refresh: &str) -> i64 {
    vike_secrets::create_store(dir.to_str()).expect("create the empty store");
    let id = vike_secrets::edit_account_in_unjournalled(
        dir,
        vike_secrets::AccountEdit::Create {
            venue: "ctrader",
            tier: AccountTier::Demo,
            label: None,
        },
    )
    .expect("create the account row")
    .after
    .expect("a created row")
    .id;
    let rows = [
        ("ACCESS_TOKEN".to_string(), access.to_string()),
        ("REFRESH_TOKEN".to_string(), refresh.to_string()),
    ];
    vike_secrets::save_owned_credentials_unjournalled(
        dir,
        &vike_secrets::CredentialOwner::Account(id),
        &rows,
        vike_secrets::UpsertIntent::Add,
    )
    .expect("seed the account's grant");
    id
}

/// What the store holds for the account: `(access, refresh)`.
fn stored(dir: &Path, account: i64) -> (String, String) {
    let book = vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
    .credential_book()
    .expect("the store reads");
    let fields = book.account(account).expect("the account");
    (
        fields.get("ACCESS_TOKEN").expect("access").to_string(),
        fields.get("REFRESH_TOKEN").expect("refresh").to_string(),
    )
}

fn home(dir: &Path, account: i64) -> TokenPersist {
    TokenPersist {
        settings_dir: dir.to_path_buf(),
        account_row: Some(account),
        state_dir: dir.join("state"),
    }
}

/// A refused refresh leaves the store exactly as it was, and so does the cell.
#[test]
fn a_refused_refresh_writes_nothing_to_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let account = store_with_account(dir.path(), "AT0", "RT0");
    let cell = SharedGrant::new("AT0", "RT0", Some(&home(dir.path(), account)));

    let refused = cell.renew(0, || T, |_| Err::<Token, _>("refused"));
    assert_eq!(refused.unwrap_err(), "refused");

    assert_eq!(cell.snapshot().refresh_token, "RT0");
    assert_eq!(stored(dir.path(), account), ("AT0".into(), "RT0".into()));
}

/// A rotation lands in the store BEFORE `renew` returns, and the sibling that adopts it writes
/// nothing of its own.
#[test]
fn the_rotation_is_saved_once_by_whoever_rotated() {
    let dir = tempfile::tempdir().unwrap();
    let account = store_with_account(dir.path(), "AT0", "RT0");
    let cell = SharedGrant::new("AT0", "RT0", Some(&home(dir.path(), account)));

    cell.renew(0, || T, |_| Ok::<_, ()>(token("AT1", "RT1"))).unwrap();
    assert_eq!(stored(dir.path(), account), ("AT1".into(), "RT1".into()));

    cell.renew(0, || T + 1, |_| -> Result<Token, ()> { panic!("adopt only") }).unwrap();
    assert_eq!(stored(dir.path(), account), ("AT1".into(), "RT1".into()));
}

/// Eight sockets invalidated at once — the lapse of one access token — spend ONE refresh token
/// between them, all end on the same new pair, and the store holds it. (Before the cell, two of
/// three sockets spent the same token and the loser stopped.)
#[test]
fn simultaneous_invalidations_spend_one_refresh_token_and_store_its_result() {
    let dir = tempfile::tempdir().unwrap();
    let account = store_with_account(dir.path(), "AT0", "RT0");
    let cell = SharedGrant::new("AT0", "RT0", Some(&home(dir.path(), account)));
    let spent = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(Barrier::new(8));

    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (cell, spent, start) = (cell.clone(), spent.clone(), start.clone());
            std::thread::spawn(move || {
                start.wait();
                cell.renew(
                    0,
                    || T,
                    |_| {
                        spent.fetch_add(1, Ordering::SeqCst);
                        // Long enough that every sibling reaches the gate while this one holds it.
                        std::thread::sleep(Duration::from_millis(60));
                        Ok::<_, ()>(token("AT1", "RT1"))
                    },
                )
                .unwrap()
            })
        })
        .collect();
    let outcomes: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();

    assert_eq!(spent.load(Ordering::SeqCst), 1, "the refresh token was spent more than once");
    assert!(outcomes.iter().all(|(_, g)| g.refresh_token == "RT1" && g.generation == 1));
    assert_eq!(stored(dir.path(), account), ("AT1".into(), "RT1".into()));
}

/// The rotation home is shared: a cell built without one takes the first one a joiner brings, so
/// the socket that brings the daemon's home arms the saves for its siblings. (Unique token strings:
/// the registry is process-wide.)
#[test]
fn a_cell_without_a_rotation_home_takes_the_first_one_a_joiner_brings() {
    let dir = tempfile::tempdir().unwrap();
    let account = store_with_account(dir.path(), "AT-home-0", "RT-home-0");
    let persist = home(dir.path(), account);

    let (bare, _) = SharedGrant::join("AT-home-0", "RT-home-0", None);
    let (armed, _) = SharedGrant::join("AT-home-0", "RT-home-0", Some(&persist));
    assert!(Arc::ptr_eq(&bare, &armed));

    bare.renew(0, || T, |_| Ok::<_, ()>(token("AT-home-1", "RT-home-1"))).unwrap();
    assert_eq!(stored(dir.path(), account), ("AT-home-1".into(), "RT-home-1".into()));
}

// ── the sockets that reach a venue ───────────────────────────────────────────────────────────────

/// A test config for the gated fake that shares `cell` and is at generation `seen`.
fn sharing(server: &FakeCtrader, cell: &Arc<SharedGrant>, access: &str, seen: u64) -> ConnConfig {
    let mut cfg = ConnConfig::for_test(server.addr(), "cid", "secret", access);
    cfg.grant = Some(cell.clone());
    cfg.grant_seen = seen;
    cfg
}

/// What the exec actor does when its session ends: spend the cell's refresh token (here, a
/// refresher that hands out `(access, refresh)`) — and the venue, from that moment, honours only
/// the new access token.
fn rotate(cell: &SharedGrant, gate: &AcceptedToken, access: &str, refresh: &str) {
    cell.renew(
        0,
        || 0,
        |_| {
            Ok::<_, ()>(Token {
                access_token: access.to_string(),
                refresh_token: refresh.to_string(),
                expires_in: 2_592_000,
            })
        },
    )
    .expect("the injected refresher succeeds");
    gate.accept_only(access);
}

/// THE startup defect: the data feed's config is made from the account snapshot taken BEFORE the
/// exec connect healed a lapsed grant, so it presented the access token the venue had just ended
/// and then spent the refresh token exec had just spent. Built from that stale snapshot but
/// sharing the cell, it connects with the pair the sibling rotated.
#[test]
fn a_socket_built_from_a_stale_snapshot_connects_with_the_pair_a_sibling_rotated() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    rotate(&cell, &gate, "AT_1", "RT_1");

    // The data feed: snapshot AT_0, generation 0 — one rotation behind the cell.
    let data = sharing(&server, &cell, "AT_0", 0);
    let handle = connect_and_auth(data, Arc::new(NoopSink))
        .expect("the stale snapshot must adopt the rotated pair BEFORE its first handshake");
    assert_eq!(handle.ctid, 99);
    assert_eq!(
        gate.presented(),
        ["AT_1"],
        "the venue must never see the superseded access token from a socket that shares the cell"
    );
}

/// The control: the same stale snapshot with NO cell — the shape every config had before — is
/// refused by the venue as an auth failure. This is what makes the test above prove something.
#[test]
fn without_a_shared_grant_the_stale_pair_is_refused() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    gate.accept_only("AT_1");
    let alone = ConnConfig::for_test(server.addr(), "cid", "secret", "AT_0");
    let Err(err) = connect_and_auth(alone, Arc::new(NoopSink)) else {
        panic!("a superseded access token must be refused")
    };
    assert!(err.is_auth_failure(), "the refusal is an AUTH failure, got {err}");
    assert!(matches!(err, ConnError::Venue { .. }));
    assert_eq!(gate.presented(), ["AT_0"]);
}

/// The reconcile client kept a frozen copy of the pair: after a sibling's rotation every revive
/// presented the ended access token and every later pass failed until the daemon restarted. Now a
/// revive follows the shared grant, and the client never spends the refresh token itself.
#[test]
fn the_reconcile_client_follows_a_sibling_rotation_across_a_revive() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let recon_cfg = sharing(&server, &cell, "AT_0", 0);
    // `Duration::ZERO`: every fetch re-handshakes, so the revive path runs on each call.
    let client = CtraderReconClient::connect_with_idle_revive(&recon_cfg, "EURUSD", Duration::ZERO)
        .expect("connects while the grant is current");
    client.fetch_order_status_reports(0).expect("a pass over the current grant");

    // The exec actor rotates; the venue now honours only AT_1.
    rotate(&cell, &gate, "AT_1", "RT_1");

    client
        .fetch_order_status_reports(0)
        .expect("the revive adopts the rotated pair instead of presenting the ended token");
    let presented = gate.presented();
    assert_eq!(
        presented.last().map(String::as_str),
        Some("AT_1"),
        "the revive after the rotation must present the new access token; presented: {presented:?}"
    );
    assert!(
        !presented.iter().skip_while(|t| *t != "AT_1").any(|t| t == "AT_0"),
        "once rotated, the old access token is never presented again: {presented:?}"
    );
}

/// A reconcile client BUILT after the rotation from the stale snapshot (the order `live_mount_for_
/// account` now uses: exec first, so a grant healed at start is already in the cell) comes up on
/// the new pair instead of staying reconcile-less for the whole session.
#[test]
fn a_reconcile_client_built_after_the_heal_connects_on_the_healed_pair() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    rotate(&cell, &gate, "AT_1", "RT_1");

    let stale = sharing(&server, &cell, "AT_0", 0);
    let client = CtraderReconClient::connect_with_idle_revive(&stale, "EURUSD", Duration::ZERO)
        .expect("built from the stale snapshot, it must come up on the healed pair");
    client.fetch_position_status_reports().expect("and answer a pass");
    assert_eq!(gate.presented().first().map(String::as_str), Some("AT_1"));
}
