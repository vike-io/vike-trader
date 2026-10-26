//! LIVE FXCM demo smoke of the INSTRUMENT SUBSCRIPTION (the owner's "trade ANY ticker of the venue", 2026-10-26): a
//! symbol FXCM holds as view-only or disabled is made tradable by the BRIDGE, on all three paths that need it.
//!
//! ```text
//! export FCSDK_DIR=<repo>/vendor/fcsdk/linux LD_LIBRARY_PATH=$FCSDK_DIR/lib VIKE_SETTINGS_DIR=<project>/settings
//! cargo test -p vike-fxcm --features fxcm --test fxcm_subscribe_smoke -- --ignored --nocapture
//! ```
//!
//! 1. **The exec path**: a far-from-market LIMIT (rests, cannot fill, cancelled) on a pair that is not tradable. The
//!    exec thread subscribes it before it places the order (`crate::subscription`, `ensure`). A SECOND order, at an
//!    explicit price, then goes through the login-time snapshot lookup (`findOffer`) on the same session and pair.
//! 2. **The facts path**: `reads::quoted_facts`, what the control door asks, on a pair that is not tradable.
//! 3. **The price lane**: a pair that is not tradable is wanted on the process desk and a tick arrives.
//! 4. **The permission path** ([`DENIED`], USD/HKD): a pair the demo account may not create orders on is refused BY NAME,
//!    nothing is sent, and a row that is not tradable is not subscribed for nothing.
//!
//! ⚠ **RE-RUNNABLE, and never vacuous**: each path takes a pair that is NOT tradable RIGHT NOW (the first `V` or `D`
//! regular forex row of [`CANDIDATES`] that no earlier path took), asserts it read `V`/`D` before and `T` after, and says
//! which it picked. FXCM keeps a subscription and the bridge never un-subscribes, so every run leaves its three pairs
//! tradable; a run that finds fewer than three not-tradable candidates FAILS (extend [`CANDIDATES`]) instead of passing
//! over a path it did not exercise. It prints every Offers row that changed between the run's start and its end.
//!
//! Leaves the account FLAT (no fill is possible). It opens TWO sessions of the demo login (exec and reconcile, as the
//! daemon does), so run it when the daemon's own FXCM account is stopped (`vike-cli secrets account deactivate --id <N>`;
//! reactivate it after), and it reads the demo account's login by ITS ROW whether or not the row is active.
//!
//! Triple-gated like the other `*_smoke.rs`: `#[ignore]`, self-skips with no fxcm demo account in the store, and
//! self-skips on a box whose shim did not load (`no ForexConnect shim` is NOT a pass: look for `LOGIN OK`).
//!
//! The OLD-SHIM run (a shim that predates `fc_set_subscription`; 2026-10-26 against the one installed on the CI box) is this
//! test run from a directory whose `../lib/libfcshim.so` is the old shim (the loader's first rung); it is expected to
//! FAIL its `T` assertions with NAMED refusals, which is what that run is for.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_fxcm::home_conversion::{INSTRUMENT_FOREX, Offer};
use vike_fxcm::{FxcmConfig, FxcmExecutionClient, load_fxcm_config_from, recon_client_for};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest};

/// The default account's registry label: the one the process price desk polls.
const ACCOUNT: &str = vike_fxcm::quotes::PRICED_ACCOUNT;

/// Regular forex pairs the smoke may take, in the order it prefers them: FXCM's own conversion legs (`V`) first, then
/// pairs it holds `D`. The first one that is not tradable when a path asks is that path's pair. ⚠ NOT USDHKD: the demo
/// account's order permissions for it read Disabled (the one forex row of 48), so no order on it can be accepted: the
/// first run of 2026-10-26 13:51 UTC picked it and panicked on FXCM's `ORA-20102: Access Violation`. It has a step of
/// its own, [`DENIED`].
const CANDIDATES: [&str; 15] = [
    "EURJPY", "EURAUD", "EURCAD", "CHFJPY", "GBPCHF", "AUDCAD", "CADJPY", "NZDJPY", "GBPCAD",
    "GBPNZD", "GBPAUD", "AUDNZD", "USDSEK", "EURSEK", "EURNOK",
];

/// The pair the demo account may not trade (`canCreateEntryOrder` and `canCreateMarketOpenOrder` read Disabled while the
/// row reads `T` or `V`/`D` like any other): an order on it must be refused BY NAME, before anything is sent.
const DENIED: &str = "USDHKD";

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// The demo fxcm account's login: the active default row when there is one, else the one fxcm demo row there is
/// (this smoke runs while the daemon's account is deactivated).
fn config() -> Option<FxcmConfig> {
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let settings = vike_paths::state_path::project_settings_dir_from(
        settings_dir.as_deref(),
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("fxcm"))
    .unwrap_or_default();
    let active = dir.credentials_for("fxcm", &AccountLabel::Default, AccountTier::Demo);
    if let Some(cfg) = load_fxcm_config_from(AccountTier::Demo, &active, &settings) {
        return Some(cfg);
    }
    let id = {
        let rows = dir.rows()?.ok()?.known()?;
        let mut demo = rows
            .iter()
            .filter(|r| r.venue == "fxcm" && r.tier == AccountTier::Demo && r.label.is_none());
        let first = demo.next()?.id;
        demo.next().is_none().then_some(first)?
    };
    let book = dir.credentials()?.ok()?;
    load_fxcm_config_from(AccountTier::Demo, &book.view("fxcm", Some(id)), &settings)
}

fn order(coid: &str, symbol: &str, price: Option<f64>) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "fxcm".into(),
        symbol: symbol.into(),
        side: 1,
        // ONE lot of a forex pair on the demo is its base unit size, 1000 (measured on EUR/USD, EUR/GBP, USD/JPY).
        qty: 1000.0,
        order_type: vike_model::OrderType::from_stored("limit"),
        price, // None rests RESTING_PIPS away from the quote; Some rests at that rate: neither can fill
        trigger_price: None,
        trigger_by: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts: now_ms(),
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
    }
}

fn next_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, secs: u64) -> Option<Event> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    match rt.block_on(async { tokio::time::timeout(Duration::from_secs(secs), rx.recv()).await }) {
        Ok(Some(Ingest::Event(ev))) => Some(ev),
        Ok(Some(other)) => panic!("expected Ingest::Event, got {other:?}"),
        Ok(None) => panic!("ingest channel closed"),
        Err(_) => None,
    }
}

fn name_of(pair: &str) -> String {
    format!("{}/{}", &pair[..3], &pair[3..])
}

fn row_of(reads: &vike_fxcm::reads::AccountReads, pair: &str) -> Offer {
    let name = name_of(pair);
    reads
        .offers()
        .expect("an Offers table")
        .into_iter()
        .find(|o| o.instrument == name)
        .unwrap_or_else(|| panic!("{name} is not in the Offers table"))
}

/// The first candidate that is a regular forex row, not tradable right now, and not taken by an earlier path.
fn pick(reads: &vike_fxcm::reads::AccountReads, taken: &[&str]) -> &'static str {
    let table = reads.offers().expect("an Offers table");
    CANDIDATES
        .iter()
        .copied()
        .find(|c| {
            !taken.contains(c)
                && table.iter().any(|o| {
                    o.instrument == name_of(c) && o.instrument_type == INSTRUMENT_FOREX && o.subscription != "T"
                })
        })
        .unwrap_or_else(|| {
            panic!(
                "no candidate is left that is not tradable (taken: {taken:?}): every earlier run subscribed them. Extend \
                 CANDIDATES with regular forex pairs the table lists as `V` or `D`; the smoke does not pass over a path it \
                 did not exercise"
            )
        })
}

/// The grid price `percent` percent below the pair's bid: a BUY limit there rests and cannot fill.
fn far_below_bid(row: &Offer, percent: f64) -> f64 {
    let scale = 10f64.powi(row.digits);
    (row.bid * (1.0 - percent / 100.0) * scale).round() / scale
}

fn status_changes(before: &[Offer], after: &[Offer]) -> Vec<String> {
    before
        .iter()
        .filter_map(|b| {
            let now = after.iter().find(|a| a.instrument == b.instrument)?;
            (now.subscription != b.subscription)
                .then(|| format!("{}: {} -> {}", b.instrument, b.subscription, now.subscription))
        })
        .collect()
}

#[test]
#[ignore = "live demo; opens two sessions of the demo login; leaves the account flat and three more pairs tradable"]
fn fxcm_subscribes_a_symbol_it_is_asked_to_trade_admit_or_price() {
    vike_log::test_init();
    let Some(cfg) = config() else {
        eprintln!("skip: no fxcm demo account credentials");
        return;
    };
    if !vike_fxcm::sdk_available() {
        eprintln!("skip: no ForexConnect shim on this box (NOT a pass)");
        return;
    }
    let _recon = recon_client_for(&cfg, "EURUSD", ACCOUNT)
        .expect("the reconcile login failed (re-run with --nocapture: the session thread logs FXCM's words)");
    println!("LOGIN OK (reconcile session)");
    let reads = vike_fxcm::reads::account_reads(ACCOUNT).expect("registered");
    let table_before = reads.offers().expect("an Offers table");
    let count = |t: &[Offer], s: &str| t.iter().filter(|o| o.subscription == s).count();
    println!(
        "TABLE BEFORE: rows={} T={} V={} D={}",
        table_before.len(),
        count(&table_before, "T"),
        count(&table_before, "V"),
        count(&table_before, "D")
    );
    let exec_pair = pick(&reads, &[]);
    let facts_pair = pick(&reads, &[exec_pair]);
    let price_pair = pick(&reads, &[exec_pair, facts_pair]);
    println!(
        "PICKED (each is `V` or `D` right now): exec={exec_pair} facts={facts_pair} price={price_pair}"
    );

    // 1. THE EXEC PATH: a resting limit on a pair that is not tradable. Nothing subscribed it for the order.
    let before = row_of(&reads, exec_pair);
    assert_ne!(before.subscription, "T", "{exec_pair} must start not tradable");
    let (events, mut rx) = event_channel(64);
    let mut client = FxcmExecutionClient::spawn(cfg.clone(), events);
    let coid = format!("fxsub{}", now_ms());
    let started = Instant::now();
    client.submit(&order(&coid, exec_pair, None));
    match next_event(&mut rx, 60) {
        Some(Event::OrderSubmitted(e)) => assert_eq!(e.client_order_id, coid),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    let market_open = before.trading_status == "O";
    match next_event(&mut rx, 60) {
        Some(Event::OrderAccepted(e)) => {
            println!(
                "EXEC PATH: first limit on {exec_pair} (was `{}`) RESTING venue_order_id={:?} after {} ms",
                before.subscription,
                e.venue_order_id,
                started.elapsed().as_millis()
            );
            client.cancel(&coid);
            match next_event(&mut rx, 60) {
                Some(Event::OrderCanceled(c)) => assert_eq!(c.client_order_id, coid),
                other => panic!("expected OrderCanceled, got {other:?}"),
            }
        }
        Some(Event::OrderRejected(e)) if !market_open => {
            println!(
                "EXEC PATH: the market for {exec_pair} is closed: acceptance NOT asserted ({})",
                e.reason
            );
        }
        other => panic!(
            "the first order on {exec_pair} (was `{}`) must be accepted once the bridge has subscribed it; got {other:?}",
            before.subscription
        ),
    }
    let after = row_of(&reads, exec_pair);
    assert_eq!(after.subscription, "T", "the exec path must leave {exec_pair} tradable");
    println!(
        "EXEC PATH: {exec_pair} subscription {} -> {}",
        before.subscription, after.subscription
    );

    // The SECOND order, at an explicit price, on the same session and pair: it is placed through the login-time snapshot
    // lookup (`findOffer`), not a fresh quote. It is accepted whether the snapshot's STATUS followed the subscription (it
    // does: MEASURED on a second session) or the lookup's one fresh re-read caught it; the time says which is likelier.
    if market_open {
        let coid2 = format!("fxsubx{}", now_ms());
        let px = far_below_bid(&after, 2.0);
        let started = Instant::now();
        client.submit(&order(&coid2, exec_pair, Some(px)));
        match next_event(&mut rx, 60) {
            Some(Event::OrderSubmitted(e)) => assert_eq!(e.client_order_id, coid2),
            other => panic!("expected OrderSubmitted, got {other:?}"),
        }
        match next_event(&mut rx, 60) {
            Some(Event::OrderAccepted(e)) => {
                println!(
                    "EXEC PATH: second limit on {exec_pair} AT {px} (explicit price: the snapshot lookup) RESTING venue_order_id={:?} after {} ms",
                    e.venue_order_id,
                    started.elapsed().as_millis()
                );
                client.cancel(&coid2);
                match next_event(&mut rx, 60) {
                    Some(Event::OrderCanceled(c)) => assert_eq!(c.client_order_id, coid2),
                    other => panic!("expected OrderCanceled, got {other:?}"),
                }
            }
            other => panic!(
                "the second order on the now tradable {exec_pair} must be accepted; got {other:?}"
            ),
        }
    }

    // A CFD (an index, a commodity, a share) that is NOT tradable is never subscribed: the order that names it reaches the
    // exec thread without any facts admission, the shim refuses it by name (`-14`), and the row is left exactly as it was.
    let cfd = table_before
        .iter()
        .find(|o| {
            o.instrument_type != INSTRUMENT_FOREX
                && o.subscription != "T"
                && !o.instrument.contains('/')
                && o.instrument == o.instrument.to_ascii_uppercase()
        })
        .cloned();
    if let Some(cfd) = cfd {
        let coid3 = format!("fxcfd{}", now_ms());
        client.submit(&order(&coid3, &cfd.instrument, None));
        match next_event(&mut rx, 60) {
            Some(Event::OrderSubmitted(e)) => assert_eq!(e.client_order_id, coid3),
            other => panic!("expected OrderSubmitted, got {other:?}"),
        }
        match next_event(&mut rx, 60) {
            Some(Event::OrderRejected(e)) => {
                println!(
                    "CFD PATH: {} (`{}`, type {}) REFUSED: {}",
                    cfd.instrument, cfd.subscription, cfd.instrument_type, e.reason
                );
                assert!(
                    e.reason.contains("is not a regular forex instrument"),
                    "a CFD must be refused by name, not subscribed: {}",
                    e.reason
                );
            }
            other => panic!("a CFD that is not tradable must be refused; got {other:?}"),
        }
        let still = reads.offers().expect("an Offers table");
        let row = still.iter().find(|o| o.instrument == cfd.instrument).expect("listed");
        assert_eq!(row.subscription, cfd.subscription, "the bridge must not write for a CFD");
    } else {
        println!("CFD PATH: SKIPPED, the table lists no non-forex row that is not tradable");
    }

    // THE PERMISSION PATH: a pair the ACCOUNT may not trade (the demo's USD/HKD) is refused BY NAME before anything is sent,
    // never as FXCM's bare `ORA-20102: Access Violation`, and a row that is not tradable is not subscribed for nothing. An
    // account FXCM HAS enabled for it simply places and cancels the order: the permission is the venue's to give.
    if let Some(before) = reads
        .offers()
        .expect("an Offers table")
        .into_iter()
        .find(|o| o.instrument == name_of(DENIED))
    {
        let coid4 = format!("fxden{}", now_ms());
        client.submit(&order(&coid4, DENIED, None));
        match next_event(&mut rx, 60) {
            Some(Event::OrderSubmitted(e)) => assert_eq!(e.client_order_id, coid4),
            other => panic!("expected OrderSubmitted, got {other:?}"),
        }
        match next_event(&mut rx, 60) {
            Some(Event::OrderRejected(e)) => {
                println!(
                    "PERMISSION PATH: {DENIED} (`{}`) REFUSED: {}",
                    before.subscription, e.reason
                );
                assert!(
                    e.reason.contains("the account may not"),
                    "the refusal must say the ACCOUNT may not trade it: {}",
                    e.reason
                );
                assert_eq!(
                    row_of(&reads, DENIED).subscription,
                    before.subscription,
                    "a pair no order can use must not be subscribed for nothing"
                );
            }
            Some(Event::OrderAccepted(_)) => {
                println!("PERMISSION PATH: {DENIED} was ACCEPTED: this account may trade it");
                client.cancel(&coid4);
                match next_event(&mut rx, 60) {
                    Some(Event::OrderCanceled(c)) => assert_eq!(c.client_order_id, coid4),
                    other => panic!("expected OrderCanceled, got {other:?}"),
                }
            }
            other => {
                panic!("the order on {DENIED} must be refused by name or accepted; got {other:?}")
            }
        }
    } else {
        println!("PERMISSION PATH: SKIPPED, {DENIED} is not in the Offers table");
    }
    client.detach();

    // 2. THE FACTS PATH: what the control door asks.
    let before = row_of(&reads, facts_pair);
    assert_ne!(before.subscription, "T", "{facts_pair} must start not tradable");
    match vike_fxcm::reads::quoted_facts(ACCOUNT, facts_pair) {
        Ok(f) => println!(
            "FACTS PATH: {facts_pair} (was `{}`): tick {} lot {} quote {:?}",
            before.subscription,
            f.properties.tick_size,
            f.properties.step_size,
            f.quote.as_ref().map(|q| (&q.quote_ccy, q.account_per_quote.get()))
        ),
        Err(why) => panic!(
            "the facts of {facts_pair} (was `{}`) must come back: {why}",
            before.subscription
        ),
    }
    let after = row_of(&reads, facts_pair);
    assert_eq!(after.subscription, "T", "the facts path must leave {facts_pair} tradable");
    println!(
        "FACTS PATH: {facts_pair} subscription {} -> {}",
        before.subscription, after.subscription
    );

    // 3. THE PRICE LANE: a pair that is not tradable is wanted and a tick arrives.
    let before = row_of(&reads, price_pair);
    assert_ne!(before.subscription, "T", "{price_pair} must start not tradable");
    let ticks: Arc<Mutex<Vec<vike_fxcm::quotes::PriceTick>>> = Arc::default();
    let sink = Arc::clone(&ticks);
    let desk = vike_fxcm::quotes::process_desk();
    let listening = desk.listen(Arc::new(move |t| sink.lock().unwrap().push(t.clone())));
    listening.want(price_pair).expect("the price lane accepts the pair");
    let until = Instant::now() + Duration::from_secs(40);
    while ticks.lock().unwrap().is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(500));
    }
    let first = ticks.lock().unwrap().first().cloned();
    let after = row_of(&reads, price_pair);
    println!(
        "PRICE PATH: {price_pair} (was `{}`): first tick {first:?}; subscription now {}",
        before.subscription, after.subscription
    );
    drop(listening);
    assert_eq!(after.subscription, "T", "the price lane must leave {price_pair} tradable");
    if market_open {
        assert!(first.is_some(), "no tick for {price_pair} within 40 s");
    }

    let table_after = reads.offers().expect("an Offers table");
    println!("TABLE CHANGES: {:?}", status_changes(&table_before, &table_after));
    println!(
        "TABLE AFTER: rows={} T={} V={} D={}",
        table_after.len(),
        count(&table_after, "T"),
        count(&table_after, "V"),
        count(&table_after, "D")
    );
    println!("SUBSCRIBE SMOKE DONE");
}
