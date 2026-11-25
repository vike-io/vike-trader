use super::*;
use vike_model::TradingState;
use vike_tradehub_client::observe_decode::{build_portfolio, map_venue_block, wire_to_core};
use vike_tradehub_client::test_support::observe_frame::full_wire_snapshot;
use vike_tradehub_client::wire::{CoreSnapshotWire, HubIdentityWire, VenueBlockWire};

/// A wire identity for the I3 observe-status tests — `name`, `live` and (since the daemon
/// started reporting which box it is) `advertise_addr` render. Empty here so every pre-existing
/// assertion below reads the DIAL address, which is what it did before the field existed;
/// `the_status_line_names_the_box_the_daemon_reported` is the one that fills it.
///
/// The tag a line carries is what the frame runs, [`tag`]'s word below; `live` stands for the row's
/// one tier here, and `vike_tradehub_client::node_words`' tests prove the daemon's switch is not it.
fn ident(name: &str, live: bool) -> HubIdentityWire {
    HubIdentityWire {
        name: name.into(),
        strategy: "spread_maker".into(),
        params: "{}".into(),
        live,
        build: "vike-tradehub 0.1.0 (abc1234)".into(),
        advertise_addr: String::new(),
        priced_venues: None,
    }
}

/// `id` with the one tier word its row runs at.
fn tag(id: &HubIdentityWire) -> (&HubIdentityWire, &str) {
    (id, if id.live { "LIVE" } else { "paper" })
}

/// B3 (`identity` is `None` from an older node, or before the first frame): the identity-less
/// observe status.
///
/// ⚠ It is no longer BYTE-IDENTICAL to the pre-I3 string, and this test used to assert that it
/// was. The `(connected)` tail is what makes a healthy observe link classify — see
/// [`observing_status`]'s doc — and byte-identity with a string that classified as `Unknown` was
/// never the property worth keeping. The role word `OBSERVING` is not this line's either since the
/// status bar became one item: it is the access half (`crate::backend::tradehub_control::access_line`).
#[test]
fn observing_status_without_identity_names_the_address_and_carries_the_token() {
    assert_eq!(observing_status(None, "127.0.0.1:9301", None), "127.0.0.1:9301 (connected)");
}

/// ⚠ **BEFORE THE FIRST FRAME the registry NAME is the only identity this side has**, and the
/// status line used to throw it away and print an address instead. Both the CI box listeners bind
/// loopback, so every thin client on every box read `OBSERVING 127.0.0.1:7879` — the same
/// string whichever production daemon it was attached to
/// (`crates/vike-app-core/src/backend/identity.rs` carries the measurement).
#[test]
fn observing_status_leads_with_the_backend_name_before_a_frame_carries_one() {
    assert_eq!(
        observing_status(Some("the CI box"), "127.0.0.1:17879", None),
        "the CI box — 127.0.0.1:17879 (connected)"
    );
}

/// …and the daemon's OWN self-report outranks the registry's label for it: it carries the
/// arming tag as well as a name, and it is the backend answering for itself.
#[test]
fn the_daemons_own_identity_outranks_the_registry_name() {
    let id = ident("the build runner", true);
    assert_eq!(
        observing_status(Some("the CI box"), "127.0.0.1:17879", Some(tag(&id))),
        "the build runner [LIVE] — 127.0.0.1:17879 (connected)"
    );
}

/// ⚠ **…and when the daemon reports WHICH BOX it is, that is the address the line names.**
/// `OBSERVING 127.0.0.1:7879` is the same string on every box for every daemon — the tunnel
/// mouth, not the far end — so a status bar printing it tells an operator nothing while
/// looking like it told them something. The choice is `identity::shown_address`'s, so
/// this line and the Connections foot strip cannot name two different boxes at once.
///
/// (RFC 5737 documentation address: a real box's address must never reach a tracked file.)
#[test]
fn the_status_line_names_the_box_the_daemon_reported() {
    let mut id = ident("the build runner", true);
    id.advertise_addr = "203.0.113.7:7879".into();
    assert_eq!(
        observing_status(Some("the CI box"), "127.0.0.1:17879", Some(tag(&id))),
        "the build runner [LIVE] — 203.0.113.7:7879 (connected)"
    );
    // …and an old node, which reports nothing, keeps the pre-existing line exactly.
    assert_eq!(
        observing_status(Some("the CI box"), "127.0.0.1:17879", Some(tag(&ident("the build runner", true)))),
        "the build runner [LIVE] — 127.0.0.1:17879 (connected)"
    );
}

/// An UNNAMED record adds nothing to the status line — no invented name, and no stand-in
/// headline in front of an address the line already shows. The strip is where the invitation
/// to name it belongs.
#[test]
fn an_unnamed_backend_adds_nothing_to_the_status_line() {
    assert_eq!(observing_status(None, "127.0.0.1:17879", None), "127.0.0.1:17879 (connected)");
    assert_eq!(
        observing_status(Some("   "), "127.0.0.1:17879", None),
        "127.0.0.1:17879 (connected)",
        "a whitespace-only name is an absent one"
    );
}

/// ⚠⚠ **A NAME IS OPERATOR TEXT, AND IT NOW LEADS THE LINE WHATEVER IT SAYS.** The line's state is
/// set beside it by [`spawn_bridge`], never read out of it, so a backend the operator called
/// `error-box` keeps its name on a HEALTHY link's line. (While the line was classified by keyword
/// such a name repainted the dot red, and [`led_by`] dropped it to save the dot.)
#[test]
fn a_name_leads_the_line_whatever_its_words() {
    for hostile in ["error-box", "disconnected-1", "failed-over", "fault-2", "press Connect"] {
        let line = observing_status(Some(hostile), "127.0.0.1:17879", None);
        assert_eq!(line, format!("{hostile} — 127.0.0.1:17879 (connected)"));
    }
    assert_eq!(
        led_by(Some("the CI box"), "connecting to 127.0.0.1:17879…"),
        "the CI box — connecting to 127.0.0.1:17879…"
    );
}

/// I3 (split-plane): once a frame carries identity, the observe status LEADS with the daemon
/// name and the loud uppercase `[LIVE]` tag — the read-only twin of the control line's rule.
#[test]
fn observing_status_with_live_identity_leads_with_name_and_uppercase_live() {
    let id = ident("the build runner", true);
    assert_eq!(
        observing_status(None, "127.0.0.1:9301", Some(tag(&id))),
        "the build runner [LIVE] — 127.0.0.1:9301 (connected)"
    );
}

/// A paper daemon renders the lowercase `[paper]` tag and the line carries no uppercase
/// "LIVE" anywhere, so a glance can never read a paper daemon as live.
#[test]
fn observing_status_with_paper_identity_has_no_uppercase_live() {
    let id = ident("sim-box", false);
    let line = observing_status(None, "127.0.0.1:9301", Some(tag(&id)));
    assert_eq!(line, "sim-box [paper] — 127.0.0.1:9301 (connected)");
    assert!(!line.contains("LIVE"), "a paper daemon must never render LIVE: {line}");
}

/// A dial that fails writes `Error`: the state is the producer's, set in the loop's `Err` arm, and
/// no word of the line (the OS's, or a hint that says "press Connect") can make it `Connecting`.
/// Driven through the real loop against an address nothing listens on, so the state is the one
/// [`spawn_bridge`] writes.
#[test]
fn a_failed_dial_writes_an_error() {
    use vike_model::feed_status::DataClientConnectionState;
    let status = std::sync::Arc::new(std::sync::Mutex::new(DataClientStatus::default()));
    let key = device();
    let mut handle = spawn_bridge(
        crate::test_support::dead_address(),
        None,
        move || Some(key.clone()),
        std::time::Duration::from_millis(50),
        patience(),
        status.clone(),
        |_snapshot| {},
        || {},
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = DataClientStatus::default();
    while std::time::Instant::now() < deadline {
        seen = status.lock().unwrap().clone();
        if seen.text.contains("failed") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    handle.stop();
    assert_eq!(seen.state, DataClientConnectionState::Error, "{seen:?}");
}

/// ⚠ **The observer's primary is the node's, not the frame's first block** (the restart drill: a
/// `[binance USDT, oanda EUR, bybit USDT]` node whose binance stopped and started again publishes
/// `[oanda, bybit, binance']`). `equity_total` is the dollar group's, so the margin sum, the panel's headline,
/// the strip and the listed groups must all be the DOLLAR group's too, with the EUR block listed beside them.
#[test]
fn the_observer_takes_the_primary_group_the_node_states_not_the_first_blocks() {
    let mut wire = full_wire_snapshot();
    wire.venues.swap(0, 1); // [bybit (400), binance (500)]: the EUR block leads
    wire.venues[0].ledger_currency = "EUR".into();
    wire.primary_currency = String::new(); // the node's primary is the dollar group

    let p = build_portfolio(&wire);
    assert_eq!(p.primary_currency(), "", "the stated key, not the first block's EUR");
    assert_eq!(p.margin_used_total.to_bits(), 500.0_f64.to_bits(), "the dollar block alone");
    assert_eq!(
        p.ledger_currencies().iter().map(|c| c.to_string()).collect::<Vec<_>>(),
        ["", "EUR"]
    );
    let rows = p.ledger_totals();
    assert_eq!(rows[0].margin_used.to_bits(), p.margin_used_total.to_bits());

    let snapshot = wire_to_core(&wire);
    let panel = vike_account_window::equity::AccountWindowEquityModel::from_snapshot(&snapshot);
    assert_eq!(panel.currency, "USD");
    assert_eq!(panel.primary, "");
    assert_eq!(panel.other_ledgers.len(), 1);
    assert_eq!(panel.other_ledgers[0].currency, "EUR");
    assert_eq!(panel.other_ledgers[0].margin_used.to_bits(), 400.0_f64.to_bits());
    let strip = vike_account_window::equity::hero::hero_totals(&panel.rows, panel.primary);
    assert_eq!(strip.realized.to_bits(), panel.rows[1].realized.to_bits(), "the dollar row alone");

    // A EUR primary is the EUR block, wherever it sits in the frame.
    wire.primary_currency = "EUR".into();
    let p = build_portfolio(&wire);
    assert_eq!(p.primary_currency(), "EUR");
    assert_eq!(p.margin_used_total.to_bits(), 400.0_f64.to_bits(), "the EUR block alone");
    let panel =
        vike_account_window::equity::AccountWindowEquityModel::from_snapshot(&wire_to_core(&wire));
    assert_eq!((panel.currency, panel.other_ledgers[0].currency), ("EUR", "USD"));
}

/// ⚠ **An old peer still renders.** A frame from a node that predates `ledger_currency` has no such key in any
/// block: it decodes (the key defaults empty), every block is the dollar group, and the panel and the
/// portfolio read as they did before the field existed, with no second headline.
#[test]
fn a_frame_without_ledger_currency_decodes_and_renders_as_one_group() {
    let mut json = serde_json::to_value(full_wire_snapshot()).expect("serializes");
    for block in json["venues"].as_array_mut().expect("venues") {
        assert!(
            block.as_object_mut().expect("block").remove("ledger_currency").is_none(),
            "none written"
        );
    }
    let wire: CoreSnapshotWire =
        serde_json::from_value(json).expect("an older node's frame decodes");
    assert!(wire.venues.iter().all(|v| v.ledger_currency.is_empty()));

    let snapshot = wire_to_core(&wire);
    assert!(!snapshot.portfolio.is_multi_currency());
    assert_eq!(
        snapshot.portfolio.margin_used_total.to_bits(),
        900.0_f64.to_bits(),
        "both blocks, as before"
    );
    let panel = vike_account_window::equity::AccountWindowEquityModel::from_snapshot(&snapshot);
    assert!(panel.other_ledgers.is_empty());
    assert_eq!(panel.currency, "USD");
    assert_eq!(panel.rows.len(), 2);
}

/// M-9 of the final review (slice B): an order whose wire account label this client cannot parse
/// (a label-grammar skew between node and client) is attributed to NO window. `OrderView::account`
/// cannot spell "unknown", so the bridge maps it to `None` like a default-account order; what keeps
/// it off the default window's markers and Cancel all is the tell it leaves on its venue: the block
/// of the engine it rests in carries the same unparseable label in its route key, so no order of
/// that venue is attributed at all (`vike_order_entry::dispatch::orders_name_their_account`). The order itself
/// stays in the snapshot, so the Account window still lists it.
#[test]
fn an_order_whose_label_this_client_cannot_read_is_attributed_to_no_window() {
    use vike_order_entry::dispatch::{orders_name_their_account, owns_order, window_orders};
    use vike_order_entry::trade_window::address::TradeWindowAddress;
    let mut w = full_wire_snapshot();
    w.venues.truncate(1);
    w.venues[0].tier = Some("live".into());
    w.venues[0].symbols = vec!["BTCUSDT".into()];
    let mut skewed = w.venues[0].clone();
    skewed.account = Some("sub-1".into());
    skewed.route_key = "binance#sub-1".into();
    skewed.tier = Some("demo".into());
    w.venues.push(skewed);
    w.orders[0].account = Some("sub-1".into());
    let snapshot = wire_to_core(&w);
    assert_eq!(
        snapshot.orders.len(),
        1,
        "the order stays in the snapshot: the Account window lists it"
    );
    let default = TradeWindowAddress::parse("binance", None, "BTCUSDT").expect("an address");
    assert!(
        !orders_name_their_account(&snapshot, "binance"),
        "the venue's orders cannot be attributed"
    );
    assert!(
        !owns_order(&snapshot, &default)(&snapshot.orders[0]),
        "never the default account's order"
    );
    assert_eq!(window_orders(&snapshot, &default).count(), 0, "no marker, no Cancel all");

    // CONTROL: the same node with a label this client reads attributes the order to its account.
    let mut readable = w.clone();
    readable.venues[1].account = Some("SUB".into());
    readable.venues[1].route_key = "binance#SUB".into();
    readable.orders[0].account = Some("SUB".into());
    let snapshot = wire_to_core(&readable);
    let sub = TradeWindowAddress::parse("binance", Some("SUB"), "BTCUSDT").expect("an address");
    assert!(orders_name_their_account(&snapshot, "binance"));
    assert_eq!(window_orders(&snapshot, &sub).count(), 1, "SUB's window marks its order");
    assert_eq!(window_orders(&snapshot, &default).count(), 0);
}

// ── connect_control: the refuse-paths of a gate that guards REAL order placement ─────────
//
// Every case below must return `None` WITHOUT dialling, so the address is a deliberately
// unroutable placeholder: if a refactor ever lets one of these reach `TradehubControlHandle::
// connect`, the test hangs/fails instead of silently arming a write path. The connecting
// path itself needs a live daemon and stays uncovered here.
const NEVER_DIALLED: &str = "127.0.0.1:0";

/// A device key for the scripted dials below, made once per test.
fn device() -> DeviceKey {
    DeviceKey::generate().expect("a device key").0
}

/// A `false` row (the default is ON since decision 0280), or an unarmed record: read-only observer,
/// whatever key is held — a key must never be sufficient on its own.
#[test]
fn control_disabled_never_connects_even_with_a_device_key() {
    assert!(connect_control(false, NEVER_DIALLED, Some(&device()), patience()).is_none());
    assert!(connect_control(false, NEVER_DIALLED, None, patience()).is_none());
}

/// Enabled but no device key ⇒ read-only, and nothing is sent (decision 0288 §3).
#[test]
fn control_enabled_without_a_device_key_stays_read_only() {
    assert!(connect_control(true, NEVER_DIALLED, None, patience()).is_none());
}

/// Enabled, a device key held, and NO daemon at the address: the connect fails and the observer
/// DEGRADES to read-only rather than erroring (`connect_control`'s `Err` arm).
#[test]
fn control_enabled_with_a_device_key_and_a_dead_daemon_stays_read_only() {
    let dead = crate::test_support::dead_address();
    assert!(connect_control(true, &dead, Some(&device()), patience()).is_none());
}

/// **With no device key the dot is RED and nothing is sent.** The bridge writes the no-key branch's
/// own line (`no_device_key_line`, led by the record's name as the loop leads it) as an `Error`: the
/// state is set there, so the cure's word "Connect" cannot make it an amber connect in progress. And
/// the bridge never dials: a listener that counts connections sees none.
#[test]
fn a_bridge_without_a_device_key_is_red_and_sends_nothing() {
    use vike_model::feed_status::DataClientConnectionState;
    for at in ["127.0.0.1:9301", "<host>:9301"] {
        let line = led_by(Some("the CI box"), &no_device_key_line(at));
        assert!(line.contains(&crate::backend::registry::no_device_key(at)), "{line}");
    }

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener.set_nonblocking(true).expect("nonblocking");
    let address = listener.local_addr().expect("local addr").to_string();
    let status = std::sync::Arc::new(std::sync::Mutex::new(DataClientStatus::default()));
    let mut handle = spawn_bridge(
        address.clone(),
        Some("the CI box".into()),
        || None,
        std::time::Duration::from_millis(50),
        patience(),
        status.clone(),
        |_snapshot| {},
        || {},
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && status.lock().unwrap().text.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    handle.stop();
    let seen = status.lock().unwrap().clone();
    assert_eq!(seen.state, DataClientConnectionState::Error, "{seen:?}");
    assert_eq!(seen.text, led_by(Some("the CI box"), &no_device_key_line(&address)), "{seen:?}");
    // ⚠ THE OLD AMBER: the words still say "Connect", and read by keyword they once painted this
    // hard error amber. The status strip's dot paints what the producer wrote: red.
    assert!(seen.text.contains("Connect"), "the premise: the words say Connect: {seen:?}");
    assert_eq!(crate::ui::status_bar::dot::feed_dot_color(&seen), vike_ui_theme::status::ERROR);
    assert!(listener.accept().is_err(), "a bridge with no device key dialled {address}");
}

// ── spawn_bridge: the stop handle (split-plane B6) ───────────────────────────────────────
//
// Both tests dial a DEAD loopback address (`crate::test_support::dead_address`),
// so every connect attempt is refused instantly and the thread spends its life in the dial
// backoff — exactly where an unsliced 2s sleep would park a `stop()` for the full window.

/// A bridge dialling `crate::test_support::dead_address`: nothing listens there, so
/// `TradehubCoreHandle::connect` fails FAST (refused) instead of hanging the test on a dial.
fn spawn_dead_bridge() -> BackendObserveBridgeHandle {
    let key = device();
    spawn_bridge(
        crate::test_support::dead_address(),
        Some("test-backend".into()),
        move || Some(key.clone()),
        // The row's default poll. A dead address never reaches the poll loop, so this value is
        // not what these tests are about — `nap_unless_stopped`'s own tests below are.
        std::time::Duration::from_millis(50),
        patience(),
        std::sync::Arc::default(),
        |_snapshot| {},
        || {},
    )
}

// ── nap_unless_stopped: the one sliced wait (split-plane B6, and the observe-poll row) ─────────

/// A stop raised BEFORE the wait skips it entirely: even a 30-second total returns at once, so a
/// bridge told to stop never sleeps a poll it no longer owes.
#[test]
fn a_nap_that_starts_stopped_returns_at_once() {
    let t0 = std::time::Instant::now();
    nap_unless_stopped(std::time::Duration::from_secs(30), &|| true);
    let elapsed = t0.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(5), "slept {elapsed:?} despite a stop");
}

/// **A stop raised mid-nap lands within a slice, however long the poll is.** This is the property
/// that lets `preferences.observe_poll_ms` run to a full second without making a backend switch —
/// which joins the bridge thread on the GUI's frame thread — wait out a whole poll: the wait is
/// naps of at most `STOP_SLICE` with the flag re-read between them.
#[test]
fn a_stop_raised_mid_nap_ends_a_long_poll_within_a_slice() {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let raiser = {
        let flag = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        })
    };
    let t0 = std::time::Instant::now();
    nap_unless_stopped(std::time::Duration::from_secs(30), &|| {
        flag.load(std::sync::atomic::Ordering::Relaxed)
    });
    let elapsed = t0.elapsed();
    raiser.join().expect("the raiser thread");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "a 30s nap took {elapsed:?} to notice a stop — it is not sliced"
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(50),
        "returned after {elapsed:?}, before the stop was raised"
    );
}

/// With nothing raising a stop, the nap sleeps AT LEAST the whole total — in slices, the last one
/// shortened to fit — so a poll of 120ms (not a multiple of the slice) is not rounded down to two.
#[test]
fn an_unstopped_nap_sleeps_the_whole_total_even_when_it_is_not_a_slice_multiple() {
    let total = std::time::Duration::from_millis(120);
    let t0 = std::time::Instant::now();
    nap_unless_stopped(total, &|| false);
    let elapsed = t0.elapsed();
    assert!(elapsed >= total, "slept {elapsed:?}, less than the {total:?} it owed");
}

/// The default poll is exactly one slice, so at the default the nap is the single 50ms sleep the
/// connected loop always took, and the slice is the same constant the dial backoff is walked in.
#[test]
fn the_default_poll_is_one_slice() {
    assert_eq!(
        std::time::Duration::from_millis(u64::from(
            vike_config::preferences::DEFAULT_OBSERVE_POLL_MS
        )),
        STOP_SLICE
    );
}

/// `stop()` returns well under the 2s dial backoff: the backoff sleep is SLICED with the
/// flag re-checked between slices, so a mid-backoff stop parks for ~one slice, never the
/// whole window. The 1s bound is 20x the slice and half the backoff — a regression to one
/// unsliced sleep fails it deterministically.
#[test]
fn stop_returns_well_under_the_dial_backoff() {
    let mut handle = spawn_dead_bridge();
    // Let the thread through its first (refused) dial and into the backoff sleep.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let t0 = std::time::Instant::now();
    handle.stop();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < std::time::Duration::from_millis(1000),
        "stop() took {elapsed:?} — the backoff sleep is not sliced"
    );
    handle.stop(); // idempotent: the join handle is already taken; returns immediately
}

/// Dropping the handle stops the thread too (`Drop` = `stop()`), so a caller that simply
/// lets it fall out of scope — the B1 runtime backend switch — leaks no dialling thread.
#[test]
fn drop_stops_the_bridge_thread() {
    let handle = spawn_dead_bridge();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let stop_flag = handle.stop.clone(); // same-module test: reach the private flag
    let t0 = std::time::Instant::now();
    drop(handle);
    let elapsed = t0.elapsed();
    assert!(stop_flag.load(std::sync::atomic::Ordering::Relaxed), "drop must raise the stop flag");
    assert!(
        elapsed < std::time::Duration::from_millis(1000),
        "drop took {elapsed:?} — Drop must stop-and-join promptly"
    );
}

// ── the operator's link patience reaches the dial ────────────────────────────────────────
//
// `preferences.observe_stall_timeout_ms` and `preferences.control_reply_timeout_ms` are read by the
// BINARY and handed down as a `LinkPatience`; these tests hold that the two windows reach the
// right socket, against a scripted node that goes silent with its socket held open (no FIN, no
// RST — the silent death the windows exist for). At the default 45 s / 30 s every one of them
// would run past its own deadline, so each is its own kill proof.

use vike_tradehub_client::liveness::LinkPatience;

fn patience() -> LinkPatience {
    LinkPatience::default()
}

/// The scripted node's observe/control split, and how long it holds the silent socket open: far
/// past every assertion, so a test cannot pass because the node finally hung up.
const SILENT_HOLD: std::time::Duration = std::time::Duration::from_secs(20);

/// A one-connection node on loopback speaking the REAL handshake and advertising the observe
/// heartbeat (the window is armed only against such a node). `observe`: after `AuthOk` it reads
/// the `Subscribe`, pushes ONE snapshot (naming the daemon as `identity`) and goes silent; otherwise
/// it reads ONE command and goes silent. The socket is HELD in both cases.
fn silent_node(observe: bool, identity: Option<HubIdentityWire>) -> String {
    use vike_hub_protocol::frame::{read_frame, write_frame};
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    use vike_tradehub_client::proto::{
        FEATURE_OBSERVE_HEARTBEAT, NODE_PROTO_VERSION, Request, Response,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let address = listener.local_addr().expect("local addr").to_string();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let nonce = [29u8; 32];
        match read_frame::<_, HandshakeFrame>(&mut stream) {
            Ok(HandshakeFrame::Hello { .. }) => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        let features = vec![
            FEATURE_OBSERVE_HEARTBEAT.to_string(),
            vike_hub_protocol::access::FEATURE_PERSON_AUTH.to_string(),
        ];
        write_frame(
            &mut stream,
            &HandshakeReply::Welcome {
                proto_version: NODE_PROTO_VERSION,
                features,
                nonce: Some(nonce),
            },
        )
        .expect("welcome");
        // A device's answer, admitted unchecked: what is under test is the window, not the list.
        let Ok(HandshakeFrame::PersonAuth { scope, .. }) =
            read_frame::<_, HandshakeFrame>(&mut stream)
        else {
            return;
        };
        write_frame(&mut stream, &HandshakeReply::AuthOk { scope, admitted: None })
            .expect("authok");
        if observe {
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Subscribe { .. }) => {}
                other => panic!("expected Subscribe, got {other:?}"),
            }
            let mut snapshot = vike_tradehub_client::wire::CoreSnapshotWire::empty();
            snapshot.sequence = 3;
            snapshot.identity = identity;
            write_frame(&mut stream, &Response::SnapshotFrame(Box::new(snapshot)))
                .expect("one frame");
        } else {
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Command { .. }) => {}
                other => panic!("expected the one Command, got {other:?}"),
            }
        }
        std::thread::sleep(SILENT_HOLD);
    });
    address
}

/// The bridge dials with the observe window it was GIVEN: a stream that goes silent is ended at
/// `patience.observe_stall` (here 300 ms) and the status line says the link dropped. ⚠ KILL PROOF:
/// dial with the constant instead of `patience.observe_stall` and the status never changes inside
/// the 5 s wait (the default is 45 s).
///
/// The same run pins the STATES the real loop writes on the way: the PAPER daemon's identity frame
/// is `Connected` (while lines were read by keyword a `[paper]` line read grey and only a `[LIVE]`
/// one green, so the dot tracked arming), and the unplanned drop is an `Error`, never the
/// `Disconnected` a stop on purpose writes.
#[test]
fn the_bridge_ends_a_silent_stream_at_the_observe_window_it_was_given() {
    use vike_model::feed_status::DataClientConnectionState;
    let address = silent_node(true, Some(ident("sim-box", false)));
    let status = std::sync::Arc::new(std::sync::Mutex::new(DataClientStatus::default()));
    let key = device();
    let mut handle = spawn_bridge(
        address.clone(),
        None,
        move || Some(key.clone()),
        std::time::Duration::from_millis(50),
        LinkPatience::from_millis(300, 30_000),
        status.clone(),
        |_snapshot| {},
        || {},
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut seen = DataClientStatus::default();
    let mut identified = None;
    while std::time::Instant::now() < deadline {
        seen = status.lock().unwrap().clone();
        if seen.text.starts_with("sim-box [") {
            identified = Some(seen.clone());
        }
        if seen.text.contains("disconnected from") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    handle.stop();
    assert!(
        seen.text.contains(&format!("disconnected from {address}")),
        "the silent stream must be ended at the 300 ms window the bridge was given; status: {seen:?}"
    );
    assert_eq!(seen.state, DataClientConnectionState::Error, "an unplanned drop: {seen:?}");
    let identified = identified.expect("the paper daemon's identity frame reached the status line");
    assert_eq!(identified.state, DataClientConnectionState::Connected, "{identified:?}");
}

/// The control channel waits for a reply the window it was GIVEN: a command the node read and
/// never answered resolves as outcome-UNKNOWN at `patience.control_reply` (here 300 ms), not at
/// the 30 s default. ⚠ KILL PROOF: dial with the constant and the ticket resolves to nothing inside
/// the 5 s wait.
#[test]
fn the_control_channel_gives_a_command_up_at_the_reply_window_it_was_given() {
    use vike_tradehub_client::wire::CommandWire;
    let address = silent_node(false, None);
    let handle =
        connect_control(true, &address, Some(&device()), LinkPatience::from_millis(45_000, 300))
            .expect("the scripted node completes the real handshake");
    let ticket = handle
        .try_command(CommandWire::SetTradingState(TradingState::Active))
        .expect("the queue is open");
    assert_eq!(
        handle.await_outcome(ticket, std::time::Duration::from_secs(5)),
        Some(vike_tradehub_client::CommandOutcome::Disconnected),
        "the node read the command and went silent: UNKNOWN at the 300 ms window"
    );
}

/// `vike-config` restates the two built-in windows and their ceilings (it is layer 20 and cannot
/// name this layer's constants), so THIS test is the pin: the defaults ARE the liveness constants,
/// the floor keeps the observe window at two heartbeats or more, the control ceiling is the one the
/// one-shot replies' own assertion puts on a reply wait, and the whole accepted range keeps every
/// relation the liveness assertions hold. A floor changed on either side reddens here, by name.
#[test]
fn the_link_patience_bounds_restate_the_liveness_constants() {
    use std::time::Duration;
    use vike_config::preferences::{
        DEFAULT_CONTROL_REPLY_TIMEOUT_MS, DEFAULT_OBSERVE_STALL_TIMEOUT_MS,
        MAX_CONTROL_REPLY_TIMEOUT_MS, MAX_OBSERVE_STALL_TIMEOUT_MS,
    };
    use vike_tradehub_client::liveness::{
        CONTROL_REPLY_TIMEOUT, OBSERVE_HEARTBEAT, OBSERVE_READ_TIMEOUT,
    };
    let ms = |v: u32| Duration::from_millis(u64::from(v));
    assert_eq!(ms(DEFAULT_OBSERVE_STALL_TIMEOUT_MS), OBSERVE_READ_TIMEOUT);
    assert_eq!(ms(DEFAULT_CONTROL_REPLY_TIMEOUT_MS), CONTROL_REPLY_TIMEOUT);
    assert_eq!(
        LinkPatience::from_millis(
            DEFAULT_OBSERVE_STALL_TIMEOUT_MS,
            DEFAULT_CONTROL_REPLY_TIMEOUT_MS
        ),
        LinkPatience::default(),
        "a box with neither row dials with today's windows"
    );
    // The control ceiling is the one-shot replies' own upper bound (ten minutes).
    assert_eq!(ms(MAX_CONTROL_REPLY_TIMEOUT_MS), Duration::from_secs(600));
    // The observe ceiling is five minutes: past it a dead link outlives a person's attention.
    assert_eq!(ms(MAX_OBSERVE_STALL_TIMEOUT_MS), Duration::from_secs(300));
    for observe in [DEFAULT_OBSERVE_STALL_TIMEOUT_MS, MAX_OBSERVE_STALL_TIMEOUT_MS] {
        assert!(ms(observe) >= 2 * OBSERVE_HEARTBEAT, "at least two missed heartbeats");
    }
}

/// **Every production dial in this crate passes the operator's patience.** `connect` is the
/// default-window door for a caller with no setting to read; a library site that dials through it
/// would silently ignore both rows. Scans this crate's non-test sources, comments excluded.
#[test]
fn no_production_dial_in_this_crate_bypasses_the_link_patience() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read a source directory").flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !name.ends_with("_tests") && name != "tests" {
                    walk(&path, out);
                }
            } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(
        files.len() > 20,
        "the walk found {} files — it is looking in the wrong place",
        files.len()
    );
    // Assembled, because this file's own text is a test module the walk skips and a literal would
    // still read as data in a reviewer's search.
    let needles = [
        concat!("TradehubCoreHandle", "::connect("),
        concat!("TradehubControlHandle", "::connect("),
    ];
    for file in files {
        let text = std::fs::read_to_string(&file).expect("read a source file");
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for needle in needles {
                assert!(
                    !line.contains(needle),
                    "{}:{} dials through `connect`, which ignores the operator's link patience; \
                     pass `LinkPatience` through connect_with_read_timeout / \
                     connect_with_reply_timeout",
                    file.display(),
                    n + 1
                );
            }
        }
    }
}

/// **The "trade any symbol" capability, as the desktop reads it off the wire** (the node x client
/// matrix, client half): a block is built exactly as the node writes it, serialized, decoded and
/// mapped, then asked whether the desktop may send a symbol its engine was not started on.
///
/// * NEW node: a demo or live block publishes `admits_symbols` (every venue's door admits any symbol,
///   `vike_catalog::SYMBOL_RULES`), so the window and the dispatcher take the picked symbol.
/// * OLD node (v0.1.63): the key is not in the frame at all, so every venue, binance included, reads
///   one symbol only, and the window says the node does not offer it, okx and deribit too. Such a node
///   would Ack a foreign symbol and sign it on the mounted one.
#[test]
fn the_desktop_sends_a_foreign_symbol_only_to_a_node_that_says_it_admits_symbols() {
    use vike_order_entry::dispatch::tradable;
    use vike_order_entry::trade_window::address::TradeWindowAddress;
    use vike_order_entry::trade_window::symbol_follow::{TradeWindowOneSymbolReason, one_symbol};

    // The block as the NODE sends it for a demo account mounted on `mounted`.
    let decoded = |venue: &str, mounted: &str, admits: bool| {
        let mut block = full_wire_snapshot().venues.remove(0);
        block.venue = venue.into();
        block.route_key = venue.into();
        block.symbols = vec![mounted.into()];
        block.tier = Some("demo".into());
        block.admits_symbols = admits;
        let json = serde_json::to_string(&block).expect("serializes");
        assert_eq!(
            json.contains("admits_symbols"),
            admits,
            "key written only when offered: {json}"
        );
        let wire: VenueBlockWire = serde_json::from_str(&json).expect("a node's block decodes");
        let mut snapshot = vike_execution::CoreSnapshot::empty(venue, mounted);
        snapshot.portfolio.venues = vec![map_venue_block(&wire)];
        snapshot
    };
    let foreign = |venue: &str, symbol: &str| TradeWindowAddress {
        venue: venue.into(),
        account: None,
        symbol: symbol.into(),
    };

    for (venue, mounted, other) in [
        ("bybit", "BTCUSDT", "ETHUSDT"),
        ("binance", "BTCUSDT", "ETHUSDT"),
        ("aster", "BTCUSDT.P", "ETHUSDT.P"),
    ] {
        let new_node = decoded(venue, mounted, true);
        assert_eq!(one_symbol(&new_node.portfolio.venues[0]), None, "{venue}: new node");
        assert!(tradable(&new_node, &foreign(venue, other)), "{venue}: new node, foreign symbol");
        assert!(tradable(&new_node, &foreign(venue, mounted)), "{venue}: the mounted one too");

        let old_node = decoded(venue, mounted, false);
        assert_eq!(
            one_symbol(&old_node.portfolio.venues[0]),
            Some(TradeWindowOneSymbolReason::NodeDoesNotOffer),
            "{venue}: old node"
        );
        assert!(!tradable(&old_node, &foreign(venue, other)), "{venue}: old node, foreign symbol");
        assert!(tradable(&old_node, &foreign(venue, mounted)), "{venue}: the mounted one still");
    }
    for (venue, mounted) in [("okx", "BTC-USDT-SWAP"), ("deribit", "BTC-PERPETUAL")] {
        let node = decoded(venue, mounted, false);
        assert_eq!(
            one_symbol(&node.portfolio.venues[0]),
            Some(TradeWindowOneSymbolReason::NodeDoesNotOffer),
            "{venue}"
        );
        assert!(!tradable(&node, &foreign(venue, "ETH-USDT-SWAP")), "{venue}: foreign symbol");
    }
}
