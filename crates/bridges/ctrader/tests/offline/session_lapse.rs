//! **A lapsed account session** (`crates/bridges/ctrader/CLAUDE.md`'s section of that name), against a scripted venue
//! that does what the CI box's did between 2026-11-04 and 2026-11-06: the access token lapses, the sockets stay up, and
//! every request of the session is refused `INVALID_REQUEST`, "Trading account is not authorized", until something
//! authorizes the account again. An order must work again after any token age, or be refused in words the operator can
//! act on while the node says so, and the actor must never stop over it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_ctrader::conn::grant::{ExecPathDown, SharedGrant};
use vike_ctrader::conn::{ConnConfig, connect_and_auth, connect_and_auth_exec, fetch_symbol_lists};
use vike_ctrader::data::CtraderData;
use vike_ctrader::oauth::OAuthError;
use vike_ctrader::positions::{ExitKind, TrackedPosition};
use vike_ctrader::proto::pt;
use vike_ctrader::test_support::{
    self, AcceptedToken, FakeCtrader, NoopSink, RECONCILE_PENDING_COID, TrendbarsFault, collect,
    market_buy, token, wait_open_filled,
};
use vike_data::DataClient;
use vike_exec::{EventSender, ExecutionClient, Ingest, event_channel};
use vike_model::events::Event;

/// Long enough for the actor's 10 s heartbeat tick to come round on a busy box.
const WAIT: Duration = Duration::from_secs(25);

type Lane = tokio::sync::mpsc::Receiver<Ingest>;

/// An exec client on a gated venue, on the access token `AT_0`, sharing `cell` (the operator's report reads it).
fn exec_on(
    server: &FakeCtrader,
    cell: &Arc<SharedGrant>,
    tune: impl FnOnce(&mut ConnConfig),
) -> (vike_ctrader::exec::CtraderExec, Lane) {
    let mut cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "AT_0");
    cfg.refresh_token = Some("RT_0".to_string());
    cfg.grant = Some(cell.clone());
    tune(&mut cfg);
    let (events, ingest): (EventSender, Lane) = event_channel(256);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events.clone()).expect("auth ok");
    let exec =
        test_support::exec_with_no_halt(handle, events, &std::env::temp_dir(), module_path!());
    (exec, ingest)
}

/// A refresher that hands out `AT_1`/`RT_1` and makes the venue honour it, counting its spends.
fn refreshing_to_at_1(
    gate: &AcceptedToken,
    spent: &Arc<AtomicUsize>,
) -> vike_ctrader::conn::Refresher {
    let (gate, spent) = (gate.clone(), spent.clone());
    Arc::new(move |_: &str| {
        spent.fetch_add(1, Ordering::SeqCst);
        gate.accept_only("AT_1");
        Ok(token("AT_1", "RT_1"))
    })
}

fn refused() -> Result<vike_ctrader::oauth::Token, OAuthError> {
    Err(OAuthError::Transport("scripted refusal".to_string()))
}

/// The reason of the terminal reject of `coid`, waiting up to [`WAIT`].
fn reject_of(ingest: &mut Lane, coid: &str) -> String {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        for event in vike_ctrader::drain!(ingest) {
            if let Event::OrderRejected(r) = event
                && r.client_order_id == coid
            {
                return r.reason.to_string();
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{coid} was never refused: it vanished");
}

fn poll(what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !holds() {
        assert!(Instant::now() < deadline, "never happened: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// **THE incident, end to end.** The order that finds the lapse is refused in the venue's words, the socket opens a new
/// session by itself, and the next order works. On v0.1.99 the second order was refused the same way, and so was
/// every later one until the 2026-11-06 restart.
#[test]
fn an_order_finding_a_lapsed_session_is_refused_in_words_and_the_next_order_works() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) = exec_on(&server, &cell, |_| {});

    gate.lapse_sessions();
    exec.submit(&market_buy("c-found-it"));
    let why = reject_of(&mut ingest, "c-found-it");
    assert!(why.contains("Trading account is not authorized"), "{why}");

    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
    exec.submit(&market_buy("c-after"));
    wait_open_filled(&mut ingest, "c-after");
    poll("the node's word is cleared", || cell.exec_path_down().is_none());
}

/// **An expired token, as on the CI box: no refresh was ever due** (a process that never rotated has no expiry), so the new
/// session is refused too and the refresh token is spent, ONCE, by the reconnect.
#[test]
fn a_token_refused_at_the_new_session_is_renewed_there_and_the_socket_comes_back() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let spent = Arc::new(AtomicUsize::new(0));
    let refresher = refreshing_to_at_1(&gate, &spent);
    let (mut exec, mut ingest) = exec_on(&server, &cell, |cfg| cfg.refresher = Some(refresher));

    // The token lapses and takes the session with it, and nothing tells the socket.
    gate.accept_only("NOT_AT_0");
    gate.lapse_sessions();
    exec.submit(&market_buy("c-found-it"));
    assert!(reject_of(&mut ingest, "c-found-it").contains("not authorized"));

    exec.submit(&market_buy("c-after"));
    wait_open_filled(&mut ingest, "c-after");
    assert_eq!(spent.load(Ordering::SeqCst), 1, "one refresh token, once");
    assert_eq!(gate.presented().last().map(String::as_str), Some("AT_1"));
    assert_eq!(cell.snapshot().refresh_token, "RT_1", "the cell holds the pair for the siblings");
}

/// **A fresh token the venue refuses too costs one refresh a minute, never a loop.** Each spend rotates a token and ends
/// every sibling's session.
#[test]
fn a_new_session_refused_again_spends_no_second_refresh_token_within_the_minute() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let spent = Arc::new(AtomicUsize::new(0));
    let counted = spent.clone();
    let (mut exec, mut ingest) = exec_on(&server, &cell, |cfg| {
        cfg.refresher = Some(Arc::new(move |_: &str| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(token("AT_1", "RT_1"))
        }));
    });

    gate.accept_only("NOBODY");
    gate.lapse_sessions();
    exec.submit(&market_buy("c-found-it"));
    let _ = reject_of(&mut ingest, "c-found-it");
    poll("the refresh was spent", || spent.load(Ordering::SeqCst) == 1);
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 3, WAIT);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(spent.load(Ordering::SeqCst), 1, "a second token inside the minute");
}

/// **A refused refresh leaves the actor running** (it used to stop, so an order read `actor thread gone` and a data key
/// went silent for the life of the process): orders are refused by name with the cure, the node is told, a new order
/// never waits to be placed hours later, and a sibling's rotation heals the socket.
#[test]
fn a_refused_refresh_refuses_orders_by_name_says_so_and_heals_when_the_grant_does() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) =
        exec_on(&server, &cell, |cfg| cfg.refresher = Some(Arc::new(|_: &str| refused())));

    gate.accept_only("AT_1");
    gate.lapse_sessions();
    exec.submit(&market_buy("c-found-it"));
    let _ = reject_of(&mut ingest, "c-found-it");
    poll("the node is told", || {
        cell.exec_path_down().is_some_and(|down| down.why.contains("NOT AUTHORIZED"))
    });
    let down = cell.exec_path_down().expect("still down");
    assert!(down.why.contains("ctrader_authorize"), "the cure is named: {}", down.why);
    assert!(!down.why.contains("AT_") && !down.why.contains("RT_"), "no value: {}", down.why);
    assert!(down.effect.contains("every order"), "orders wait for nothing now: {}", down.effect);

    exec.submit(&market_buy("c-while-dead"));
    let why = reject_of(&mut ingest, "c-while-dead");
    assert!(why.contains("NOT AUTHORIZED") && why.contains("ctrader_authorize"), "{why}");
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1, "nothing was sent for it");

    // Someone rotates (a sibling socket; here the cell itself): the socket adopts it.
    cell.renew(0, || 0, |_| Ok::<_, ()>(token("AT_1", "RT_1"))).expect("the rotation");
    poll("the node's word is cleared", || cell.exec_path_down().is_none());
    exec.submit(&market_buy("c-healed"));
    wait_open_filled(&mut ingest, "c-healed");
}

/// **The session ended without a word and nobody orders**: the probe finds it, and a new session opens. The fills of an
/// ended session are not delivered, so an idle account must not wait for its next order. Nothing reaches the engine:
/// the probe's refusal names no order.
#[test]
fn a_session_that_ended_without_a_word_is_found_with_no_order_and_rejects_nothing() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (_exec, mut ingest) =
        exec_on(&server, &cell, |cfg| cfg.auth_probe_every = Duration::from_millis(100));

    gate.lapse_sessions();
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 0);
    let rejected = test_support::rejected(&mut ingest);
    assert!(rejected.is_empty(), "a phantom reject: {rejected:?}");
}

/// **The venue's own word for it**: `ProtoOAAccountDisconnectEvent`, "a new session must be authorized for the account",
/// used to be a debug line.
#[test]
fn an_account_disconnect_event_opens_a_new_session_and_asks_for_the_streams_again() {
    let server = FakeCtrader::start_trendbars_fault_first(TrendbarsFault::Event(
        pt::ACCOUNT_DISCONNECT_EVENT,
    ));
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("auth ok");
    let mut data = CtraderData::new(handle);

    data.subscribe_bars("EURUSD", "1m").expect("bars");

    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 2, WAIT);
    data.shutdown();
}

/// **The report this closes**: an actor that stopped on a refused refresh left its keys silent with `is_dead` false.
/// It now goes on: a new session, and its streams asked for again.
#[test]
fn a_data_actor_whose_refresh_is_refused_goes_on_and_asks_for_its_streams_again() {
    let server = FakeCtrader::start_trendbars_fault_first(TrendbarsFault::Event(
        pt::ACCOUNTS_TOKEN_INVALIDATED_EVENT,
    ));
    let mut cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    cfg.refresh_token = Some("RT_0".to_string());
    cfg.refresher = Some(Arc::new(|_: &str| refused()));
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("auth ok");
    let mut data = CtraderData::new(handle);

    data.subscribe_bars("EURUSD", "1m").expect("bars");

    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
    server.wait_until_count_at_least("SUBSCRIBE_SPOTS_REQ", 2, WAIT);
    data.shutdown();
}

/// **A socket that only FOLLOWS the grant was refused, so a socket that may SPEND opens a new session on it**: the
/// reconcile client and the listing were refused `CH_ACCESS_TOKEN_INVALID` for 49 hours on the CI box while the exec and data
/// sockets, unasked, spent nothing. The reconnect's own handshake is refused and renews the grant, once, and the follower's
/// next handshake works.
#[test]
fn a_handshake_refused_elsewhere_makes_the_running_socket_renew_the_grant() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let spent = Arc::new(AtomicUsize::new(0));
    let refresher = refreshing_to_at_1(&gate, &spent);
    let (mut exec, mut ingest) = exec_on(&server, &cell, |cfg| cfg.refresher = Some(refresher));

    gate.accept_only("NOT_AT_0");
    let mut follower = ConnConfig::for_test(server.addr(), "cid", "secret", "AT_0");
    follower.grant = Some(cell.clone());
    assert!(fetch_symbol_lists(follower.clone()).is_err(), "the follower is refused");
    assert_eq!(spent.load(Ordering::SeqCst), 0, "a follower never spends");

    // The heartbeat tick reads it.
    poll("the running socket renewed the grant", || spent.load(Ordering::SeqCst) == 1);
    poll("it authorized on the new pair", || {
        gate.presented().last().map(String::as_str) == Some("AT_1")
    });
    poll("the node's word is cleared", || cell.exec_path_down().is_none());
    fetch_symbol_lists(follower).expect("the follower follows");
    assert_eq!(spent.load(Ordering::SeqCst), 1);

    // That was a new session like any other: refused again inside the minute, it opens no other.
    let logins = server.count_seen("APPLICATION_AUTH_REQ");
    gate.lapse_sessions();
    exec.submit(&market_buy("c-again"));
    let _ = reject_of(&mut ingest, "c-again");
    poll("the node is told", || cell.exec_path_down().is_some());
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), logins, "a login per refused order");
}

/// **A refusal met elsewhere re-reads the book, whichever word came first.** The token lapses and the session ends in
/// silence, a bound stop fills and this socket hears nothing; a minute later the reconcile client is refused. A re-auth on
/// the same socket left the book listing the closed position, so the orphan rule never fired and the stop's sibling went on
/// resting, and the crate measured that a stale bound stop can open a position. Here the book lists a long the venue no
/// longer does, with its exit resting: the new session reads the venue's list, and the exit is cancelled. Reddens on the
/// `pair_was_refused` trigger going back to the same-socket renewal.
#[test]
fn a_refusal_met_elsewhere_re_reads_the_book_and_cancels_the_exit_of_a_position_that_closed_unheard()
 {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let spent = Arc::new(AtomicUsize::new(0));
    let mut cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "AT_0");
    cfg.refresh_token = Some("RT_0".to_string());
    cfg.grant = Some(cell.clone());
    cfg.refresher = Some(refreshing_to_at_1(&gate, &spent));
    let (events, _ingest): (EventSender, Lane) = event_channel(256);
    let handle = connect_and_auth_exec(cfg, Arc::new(NoopSink), events).expect("auth ok");
    // The seed taught this order's id; the book says it is the stop of a long.
    let long = TrackedPosition { symbol_id: 1, side: 1, volume: 100, open_ts: 1 };
    {
        let mut book = handle.positions.lock().expect("book");
        book.upsert(7, long);
        book.bind_exit(RECONCILE_PENDING_COID, 7, ExitKind::Stop, 100);
    }
    assert_eq!(server.count_seen("RECONCILE_REQ"), 1, "the floor: the connect's seed alone");

    gate.accept_only("NOT_AT_0");
    gate.lapse_sessions();
    let mut follower = ConnConfig::for_test(server.addr(), "cid", "secret", "AT_0");
    follower.grant = Some(cell.clone());
    assert!(fetch_symbol_lists(follower).is_err(), "the follower is refused");

    // The doctor is told as the new session is sought, in the words of a socket that is reconnecting.
    gate.refuse_the_application(true);
    poll("the node is told", || cell.exec_path_down().is_some());
    let down = cell.exec_path_down().expect("still down");
    assert!(down.effect.contains("waits"), "{}", down.effect);
    gate.refuse_the_application(false);

    server.wait_until_count_at_least("RECONCILE_REQ", 2, WAIT);
    server.wait_until_count_at_least("CANCEL_ORDER_REQ", 1, WAIT);
    assert_eq!(
        spent.load(Ordering::SeqCst),
        1,
        "the reconnect's handshake renewed the grant, once"
    );
}

/// **A new session the venue honours ends the refusal**: an actor reads the note as "open a new session", a session on the
/// same pair moves no generation, and without this it would open another on every heartbeat tick. Reddens on
/// `note_token_honoured` going.
#[test]
fn a_handshake_the_venue_honours_ends_a_refusal_met_by_another() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let mut follower = ConnConfig::for_test(server.addr(), "cid", "secret", "AT_0");
    follower.grant = Some(cell.clone());

    gate.accept_only("NOT_AT_0");
    assert!(fetch_symbol_lists(follower.clone()).is_err());
    assert!(cell.token_refused_at(0), "the floor: the refusal is noted");

    gate.accept_only("AT_0");
    fetch_symbol_lists(follower).expect("the same pair is honoured now");
    assert!(!cell.token_refused_at(0));
}

/// Every order of `coids` ended (refused, or filled after a new session), waiting up to [`WAIT`]: their reasons.
fn ended(ingest: &mut Lane, coids: &[&str]) -> Vec<String> {
    let mut ended: Vec<(String, String)> = Vec::new();
    let deadline = Instant::now() + WAIT;
    while ended.len() < coids.len() {
        assert!(Instant::now() < deadline, "only {ended:?} of {coids:?} ended: the rest vanished");
        for event in vike_ctrader::drain!(ingest) {
            let (coid, why) = match event {
                Event::OrderRejected(r) => (r.client_order_id, r.reason.to_string()),
                Event::OrderFilled(f) => (f.client_order_id, "filled".to_string()),
                _ => continue,
            };
            if coids.contains(&coid.as_str()) {
                ended.push((coid, why));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    ended.into_iter().map(|(_, why)| why).collect()
}

/// **Every order written on a lapsed session is answered, not only the first** (a bracket's stop and take-profit go out
/// together): the first refusal starts the reconnect, and closing the socket threw the second refusal away unread, so that
/// order stayed `Submitted` until the engine's ladder ended it, on a protective leg too. Reddens on the read before the close going.
#[test]
fn two_orders_on_a_lapsed_session_each_get_their_answer() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) = exec_on(&server, &cell, |_| {});

    gate.lapse_sessions();
    // The actor sits in a read of up to a second, so both are drained and written in one pass.
    exec.submit(&market_buy("c-stop"));
    exec.submit(&market_buy("c-take-profit"));
    let why = ended(&mut ingest, &["c-stop", "c-take-profit"]);
    assert_eq!(why.len(), 2, "{why:?}");
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
}

/// **An order held past the bound is refused by name and never placed, whatever keeps the session from opening.** Here it is
/// `CH_CLIENT_AUTH_FAILURE`, a refusal no refresh token cures, so the grant is not dead and nothing used to refuse the
/// order: the engine's ladder closed it, and the replay then placed a live order the node believes dead. Reddens on the
/// bound going, and on a refused order still being written when the session returns.
#[test]
fn an_order_held_past_the_bound_is_refused_and_never_placed_when_the_session_returns() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) =
        exec_on(&server, &cell, |cfg| cfg.hold_orders_for = Duration::from_millis(300));

    gate.refuse_the_application(true);
    gate.lapse_sessions();
    exec.submit(&market_buy("c-found-it"));
    let _ = reject_of(&mut ingest, "c-found-it");

    exec.submit(&market_buy("c-held"));
    let why = reject_of(&mut ingest, "c-held");
    assert!(why.contains("no session") && why.contains("not sent"), "{why}");
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1, "the one that found the lapse");

    gate.refuse_the_application(false);
    poll("the session is back", || cell.exec_path_down().is_none());
    server.wait_until_count_at_least("RECONCILE_REQ", 2, WAIT);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1, "a refused order was placed after all");
}

/// **A session that opens late does not carry an old order out with it.** The handshake that succeeds can itself block for
/// many response timeouts, and an order that arrived during it is as old as the whole attempt for all the actor can tell.
/// Reddens on the look at the success going, and on a command's age counting from the look that found it.
#[test]
fn an_order_that_arrived_during_a_slow_handshake_is_judged_before_it_goes_out() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) =
        exec_on(&server, &cell, |cfg| cfg.hold_orders_for = Duration::from_millis(300));

    gate.answer_the_application_after(Duration::from_secs(2));
    gate.lapse_sessions();
    exec.submit(&market_buy("c-found-it"));
    let _ = reject_of(&mut ingest, "c-found-it");
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);

    exec.submit(&market_buy("c-during"));
    let why = reject_of(&mut ingest, "c-during");
    assert!(why.contains("not sent"), "{why}");
    server.wait_until_count_at_least("RECONCILE_REQ", 2, WAIT);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1, "it went out on the new session");
}

/// **Inside the bound the order waits and goes out**, and the node is told exactly that while it does: an operator told
/// "refused" would place it again by hand, and both would go out. Reddens on the bound refusing at once, and on the words.
#[test]
fn an_order_inside_the_bound_waits_for_the_session_and_the_node_says_so() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) = exec_on(&server, &cell, |_| {});

    gate.refuse_the_application(true);
    gate.lapse_sessions();
    exec.submit(&market_buy("c-found-it"));
    let _ = reject_of(&mut ingest, "c-found-it");
    poll("the node is told", || cell.exec_path_down().is_some());
    let down = cell.exec_path_down().expect("still down");
    assert!(down.effect.contains("waits") && down.effect.contains("goes out"), "{}", down.effect);
    assert!(!down.effect.contains("every order"), "{}", down.effect);

    exec.submit(&market_buy("c-held"));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(server.count_seen("NEW_ORDER_REQ"), 1, "it waits for the session");

    gate.refuse_the_application(false);
    wait_open_filled(&mut ingest, "c-held");
    poll("the node's word is cleared", || cell.exec_path_down().is_none());
}

/// **A cancel is not lost by the bound**: it waits for the session however long that takes, because a refusal would leave
/// the order resting with nobody asking again. Reddens on a cancel being refused past the bound like an order.
#[test]
fn a_cancel_outlives_the_bound_and_reaches_the_venue_when_the_session_returns() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) =
        exec_on(&server, &cell, |cfg| cfg.hold_orders_for = Duration::from_millis(200));
    assert!(exec.venue_order_id(RECONCILE_PENDING_COID).is_some(), "the seed taught the order");

    gate.refuse_the_application(true);
    gate.lapse_sessions();
    exec.submit(&market_buy("c-found-it"));
    let _ = reject_of(&mut ingest, "c-found-it");
    exec.cancel(RECONCILE_PENDING_COID);

    // Past the bound, with the session still refused: backoff doubles from 200 ms, so a look falls inside this.
    std::thread::sleep(Duration::from_secs(2));
    let got = vike_ctrader::drain!(ingest);
    assert!(!got.iter().any(|e| matches!(e, Event::OrderCancelRejected(_))), "{got:?}");
    assert_eq!(server.count_seen("CANCEL_ORDER_REQ"), 0, "the floor: no session to carry it");

    gate.refuse_the_application(false);
    server.wait_until_count_at_least("CANCEL_ORDER_REQ", 1, WAIT);
}

/// **A cTrader outage is not left listed on a healthy engine**: an actor that ends inside its reconnect (a `Shutdown`,
/// `account.active` off and on, a tradehub-only restart) leaves its word on the grant's cell, which lives for the process.
/// The next adapter's first handshake succeeds, and nothing else would clear it until a re-auth or a reconnect, so `trade
/// doctor` said WARN on a working engine until the gatewayhub restarted. Reddens on the `healed` at the top of `actor_loop`.
#[test]
fn a_new_adapter_clears_the_word_an_ended_actor_of_the_account_left() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) =
        exec_on(&server, &cell, |cfg| cfg.refresher = Some(Arc::new(|_: &str| refused())));

    gate.accept_only("AT_1");
    gate.lapse_sessions();
    exec.submit(&market_buy("c-found-it"));
    let _ = reject_of(&mut ingest, "c-found-it");
    poll("the node is told", || cell.exec_path_down().is_some());
    drop(exec);
    drop(ingest);
    assert!(cell.exec_path_down().is_some(), "the floor: the ended actor left its word");

    gate.accept_only("AT_0");
    let (_exec, _ingest) = exec_on(&server, &cell, |_| {});
    poll("the word is cleared", || cell.exec_path_down().is_none());
}

/// **A healthy session clears its own account's word and never a sibling's**: two trading accounts can hold one grant, so
/// one cell, and the new adapter of one must not hide the other's real outage from the doctor. Reddens on the cell keeping
/// one word for the grant, and on the start clearing nothing.
#[test]
fn a_new_adapter_does_not_clear_a_siblings_outage_on_a_shared_cell() {
    let (server, _gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let word = |why: &str| ExecPathDown {
        why: why.to_string(),
        effect: "scripted",
        failed_attempts: 0,
        retry_in_ms: 0,
    };
    // The fake's account is 99, the lower key, so its word is the one read until it goes.
    cell.set_exec_path(99, Some(word("this account's")));
    cell.set_exec_path(12_345, Some(word("the sibling's")));

    let (_exec, _ingest) = exec_on(&server, &cell, |_| {});
    poll("this account's word went, and the sibling's stays", || {
        cell.exec_path_down().is_some_and(|down| down.why == "the sibling's")
    });
}

/// **A refusal in an auth word still ends the order it answers.** These frames were diverted to the refresh alone, so
/// the order vanished: no reject, nothing for the engine until the stuck-order watchdog.
#[test]
fn a_refusal_in_an_auth_word_still_ends_the_order_it_answers() {
    for (code, words) in [
        ("ACCOUNT_NOT_AUTHORIZED", "account is not authorized"),
        ("CH_ACCESS_TOKEN_INVALID", "Access token expired"),
    ] {
        let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
        let cell = SharedGrant::new("AT_0", "RT_0", None);
        let spent = Arc::new(AtomicUsize::new(0));
        let refresher = refreshing_to_at_1(&gate, &spent);
        let (mut exec, mut ingest) = exec_on(&server, &cell, |cfg| cfg.refresher = Some(refresher));

        gate.lapse_sessions_saying(code, words);
        exec.submit(&market_buy("c-1"));
        let why = reject_of(&mut ingest, "c-1");
        assert!(why.contains(code) && why.contains(words), "{code}: {why}");
    }
}

/// **A cancel refused on a lapsed session leaves the order working.** The answer is a bare `ERROR_RES` echoing the coid,
/// which used to read as the TERMINAL reject of an order that still rests at the venue.
#[test]
fn a_cancel_refused_on_a_lapsed_session_is_not_the_end_of_the_order() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) = exec_on(&server, &cell, |_| {});
    assert!(exec.venue_order_id(RECONCILE_PENDING_COID).is_some(), "the seed taught the order");

    gate.lapse_sessions();
    exec.cancel(RECONCILE_PENDING_COID);

    let got = collect(&mut ingest, 1);
    assert!(
        got.iter().any(|e| matches!(e, Event::OrderCancelRejected(r)
            if r.client_order_id == RECONCILE_PENDING_COID && r.reason.contains("not authorized"))),
        "{got:?}"
    );
    assert!(!got.iter().any(|e| matches!(e, Event::OrderRejected(_))), "terminal: {got:?}");
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
}

/// **A new session refused in the same words is not cured by another** (a refusal's words can be read for a reason that
/// is not the session, and a handshake is a whole symbol list): one a minute, and the node goes on being told.
#[test]
fn a_new_session_refused_in_the_same_words_opens_no_other_within_the_minute() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let (mut exec, mut ingest) = exec_on(&server, &cell, |_| {});

    gate.lapse_sessions();
    exec.submit(&market_buy("c-first"));
    let _ = reject_of(&mut ingest, "c-first");
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
    poll("the new session is up", || cell.exec_path_down().is_none());

    gate.lapse_sessions();
    exec.submit(&market_buy("c-second"));
    assert!(reject_of(&mut ingest, "c-second").contains("not authorized"));
    poll("the node is told again", || cell.exec_path_down().is_some());
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 2, "a login per refused order");
}

/// **The refusal of a re-auth is not answered by spending again on the spot.** A token the venue refuses as soon as it
/// is issued would otherwise rotate at the speed of a round trip, and every rotation ends a sibling's session.
#[test]
fn a_refused_re_auth_spends_no_refresh_token_in_a_loop() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let spent = Arc::new(AtomicUsize::new(0));
    let counted = spent.clone();
    let (_exec, mut ingest) = exec_on(&server, &cell, |cfg| {
        // The token is inside the refresh margin, so the heartbeat tick renews on this socket.
        cfg.token_expires_at_ms = Some(0);
        cfg.refresher = Some(Arc::new(move |_: &str| {
            let n = counted.fetch_add(1, Ordering::SeqCst);
            Ok(token(&format!("AT_{n}x"), &format!("RT_{n}x")))
        }));
    });

    // The venue refuses the new pair too.
    gate.accept_only("NOBODY");
    poll("the socket spent", || spent.load(Ordering::SeqCst) >= 1);
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
    std::thread::sleep(Duration::from_secs(3));
    let n = spent.load(Ordering::SeqCst);
    assert!(n <= 2, "one on the socket and one in the reconnect, then a minute's wait: {n}");
    let rejected = test_support::rejected(&mut ingest);
    assert!(rejected.is_empty(), "the re-auth's refusal is nobody's order: {rejected:?}");
}

/// **A re-auth the venue answers on the same socket ends the lapse too**: the session it found gone is authorized again
/// with no new socket, and the node must stop saying otherwise.
#[test]
fn a_re_auth_answered_on_the_same_socket_clears_the_nodes_word() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let spent = Arc::new(AtomicUsize::new(0));
    let refresher = refreshing_to_at_1(&gate, &spent);
    let (mut exec, mut ingest) = exec_on(&server, &cell, |cfg| {
        // The token is inside the refresh margin, so the heartbeat tick renews on this socket.
        cfg.token_expires_at_ms = Some(0);
        cfg.refresher = Some(refresher);
    });

    // The first lapse opens a new session; the second, inside the minute, opens none and is only stated.
    gate.lapse_sessions();
    exec.submit(&market_buy("c-first"));
    let _ = reject_of(&mut ingest, "c-first");
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
    poll("the new session is up", || cell.exec_path_down().is_none());
    gate.lapse_sessions();
    exec.submit(&market_buy("c-second"));
    let _ = reject_of(&mut ingest, "c-second");
    poll("the node is told", || cell.exec_path_down().is_some());
    let down = cell.exec_path_down().expect("still down");
    assert!(down.effect.contains("on this socket"), "the orders still go out: {}", down.effect);

    // The heartbeat tick renews and authorizes on this socket.
    poll("the node's word is cleared", || cell.exec_path_down().is_none());
    assert_eq!(spent.load(Ordering::SeqCst), 1);
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 2, "the new session's: no other socket");
}

/// **The node's word is the exec socket's.** A data socket's session says nothing about whether the account can trade,
/// and the two share one cell.
#[test]
fn a_data_socket_does_not_speak_for_the_exec_path() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let mut cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "AT_0");
    cfg.refresh_token = Some("RT_0".to_string());
    cfg.grant = Some(cell.clone());
    cfg.auth_probe_every = Duration::from_millis(100);
    cfg.refresher = Some(Arc::new(|_: &str| refused()));
    let handle = connect_and_auth(cfg, Arc::new(NoopSink)).expect("auth ok");
    let mut data = CtraderData::new(handle);

    gate.accept_only("AT_1");
    gate.lapse_sessions();
    server.wait_until_count_at_least("APPLICATION_AUTH_REQ", 2, WAIT);
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(cell.exec_path_down(), None);
    data.shutdown();
}

/// The control: a healthy session spends nothing and opens nothing, however often it is asked.
#[test]
fn a_healthy_session_is_only_asked() {
    let (server, gate) = FakeCtrader::start_gated_on_access_token("AT_0");
    let cell = SharedGrant::new("AT_0", "RT_0", None);
    let touched = Arc::new(AtomicBool::new(false));
    let flag = touched.clone();
    let (_exec, _ingest) = exec_on(&server, &cell, |cfg| {
        cfg.auth_probe_every = Duration::from_millis(50);
        cfg.refresher = Some(Arc::new(move |_: &str| {
            flag.store(true, Ordering::SeqCst);
            refused()
        }));
    });
    server.wait_until_count_at_least("TRADER_REQ", 6, WAIT);
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 1);
    assert_eq!(gate.presented().len(), 1);
    assert!(!touched.load(Ordering::SeqCst), "a refresh spent on a healthy session");
    assert_eq!(cell.exec_path_down(), None);
}
