use super::*;
// The day helpers now live in the model (dedup A4) — the local aliases were removed.
use vike_model::{MS_PER_DAY, utc_day};

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64) -> Bar {
    Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// A resting buy limit at 90 that only fills if the bar trades down to it.
fn limit(coid: &str, tif: TimeInForce, gtd_expiry: Option<i64>, ts: i64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(90.0),
        time_in_force: tif,
        gtd_expiry,
        ts,
        ..Default::default()
    }
}

fn client() -> PaperExecutionClient {
    PaperExecutionClient::new("sim", "BTCUSDT", 0.0, 0.0, 0.0)
}

/// Drain every queued event (helpers below project out of the drained list, so a test can
/// inspect expiries AND cancels from one drain).
fn drain(c: &mut PaperExecutionClient) -> Vec<Event> {
    std::iter::from_fn(|| c.poll_events()).collect()
}

/// (coid, ts) of every `OrderExpired` — the model's purpose-built expiry variant, which is
/// what a venue emits and what the OMS FSM terminalizes as `OrderStatus::Expired`.
fn expired_in(events: &[Event]) -> Vec<(String, i64)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderExpired(x) => Some((x.client_order_id.clone(), x.ts)),
            _ => None,
        })
        .collect()
}

/// (coid, reason) of every `OrderCanceled` — expiry itself must NOT use this vocabulary.
fn canceled_in(events: &[Event]) -> Vec<(String, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(x) => Some((x.client_order_id.clone(), x.reason.to_string())),
            _ => None,
        })
        .collect()
}

fn expired(c: &mut PaperExecutionClient) -> Vec<(String, i64)> {
    expired_in(&drain(c))
}

/// GTD expires on the first bar whose OPEN ts is at or past its deadline — not on the bar
/// before it — and that bar's fill pass no longer sees the order.
#[test]
fn gtd_expires_on_the_first_bar_at_or_past_its_deadline() {
    let mut c = client();
    c.submit(&limit("g1", TimeInForce::Gtd, Some(2_000), 0));

    // a bar strictly BEFORE the deadline: still resting, nothing expired.
    c.on_bar(&bar(1_999, 100.0, 101.0, 95.0, 100.0));
    assert!(expired(&mut c).is_empty(), "not yet expired one ms before the deadline");
    assert_eq!(c.pending.len(), 1, "still resting");

    // the bar AT the deadline expires it, stamped with that ts.
    c.on_bar(&bar(2_000, 100.0, 101.0, 95.0, 100.0));
    assert_eq!(expired(&mut c), vec![("g1".to_string(), 2_000)]);
    assert!(c.pending.is_empty(), "expired order left the resting book");
}

/// Expiry fires exactly ONCE — later bars re-emit nothing.
#[test]
fn gtd_expires_only_once() {
    let mut c = client();
    c.submit(&limit("g2", TimeInForce::Gtd, Some(1_000), 0));
    c.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
    assert_eq!(expired(&mut c).len(), 1, "expires on the deadline bar");
    for ts in [2_000, 3_000, 4_000] {
        c.on_bar(&bar(ts, 100.0, 101.0, 95.0, 100.0));
    }
    assert!(expired(&mut c).is_empty(), "never expires a second time");
    assert!(c.expiry.is_empty(), "the deadline entry is dropped with the order");
}

/// The expiry sweep runs BEFORE the fill pass: an order whose deadline this bar carries it
/// past cannot fill on that bar, even though the bar's range would trigger it.
#[test]
fn an_expiring_order_does_not_fill_on_the_bar_that_expires_it() {
    let mut c = client();
    c.submit(&limit("g3", TimeInForce::Gtd, Some(2_000), 0));
    // low 85 trades THROUGH the buy limit at 90 — it would fill if it were still alive.
    c.on_bar(&bar(2_000, 100.0, 101.0, 85.0, 95.0));
    assert!(c.fills.lock().unwrap().is_empty(), "expired order does not fill");
    assert_eq!(expired(&mut c).len(), 1, "it is terminalized as expired instead");
}

/// Expiry cancels resting orders WITHOUT touching fills or position: an order that fills
/// before its deadline is untouched by the sweep.
#[test]
fn a_fill_before_the_deadline_is_unaffected() {
    let mut c = client();
    c.submit(&limit("g4", TimeInForce::Gtd, Some(9_000), 0));
    c.on_bar(&bar(1_000, 100.0, 101.0, 85.0, 95.0)); // trades through 90 → fills
    assert_eq!(c.fills.lock().unwrap().len(), 1, "fills normally before the deadline");
    assert_eq!(c.position, 1.0);
    c.on_bar(&bar(9_000, 100.0, 101.0, 95.0, 100.0)); // past the deadline
    assert!(expired(&mut c).is_empty(), "a filled order never expires afterwards");
    assert_eq!(c.fills.lock().unwrap().len(), 1, "no extra fill");
    assert_eq!(c.position, 1.0, "expiry never moves position");
}

/// A `Gtd` with no `gtd_expiry` has no deadline and rests like GTC.
#[test]
fn gtd_without_a_deadline_never_expires() {
    let mut c = client();
    c.submit(&limit("g5", TimeInForce::Gtd, None, 0));
    c.on_bar(&bar(i64::MAX / 2, 100.0, 101.0, 95.0, 100.0));
    assert!(expired(&mut c).is_empty());
    assert_eq!(c.pending.len(), 1);
}

/// Day expires at the UTC-day boundary of its ANCHOR BAR: it survives every bar on that day
/// and dies on the first bar of the next one.
#[test]
fn day_expires_at_the_utc_day_boundary() {
    let mut c = client();
    // submitted mid-day on UTC day 0 (the request ts is NOT the anchor — the first bar is)
    c.submit(&limit("d1", TimeInForce::Day, None, 12 * 3_600_000));
    // last ms of UTC day 0 — this bar anchors the session, so it is still alive
    c.on_bar(&bar(MS_PER_DAY - 1, 100.0, 101.0, 95.0, 100.0));
    assert!(expired(&mut c).is_empty(), "alive through the end of its own UTC day");
    assert_eq!(c.pending.len(), 1);
    // first ms of the next UTC day: expired
    c.on_bar(&bar(MS_PER_DAY, 100.0, 101.0, 95.0, 100.0));
    assert_eq!(expired(&mut c), vec![("d1".to_string(), MS_PER_DAY)]);
    assert!(c.pending.is_empty());
}

/// GTC — the default on every existing order — never expires, and records no state at all,
/// which is what keeps the r7 gate and every existing caller byte-identical.
#[test]
fn gtc_never_expires_and_records_nothing() {
    let mut c = client();
    c.submit(&limit("k1", TimeInForce::Gtc, Some(1), 0)); // deadline ignored for GTC
    assert!(c.expiry.is_empty(), "GTC records no expiry state");
    for ts in [1, 1_000, 10 * MS_PER_DAY] {
        c.on_bar(&bar(ts, 100.0, 101.0, 95.0, 100.0));
    }
    assert!(expired(&mut c).is_empty(), "GTC never expires");
    assert_eq!(c.pending.len(), 1, "still resting after ten days");
}

/// A FULLY default-constructed request (the `..Default::default()` every in-tree producer
/// builds) is GTC, so it records nothing AND its event stream carries no expiry/cancel at all
/// — the byte-identity property the r7 gate depends on, asserted on the stream rather than on
/// the guard's own spelling.
#[test]
fn a_default_request_is_inert() {
    let mut c = client();
    let req = OrderRequest { client_order_id: "z1".into(), ..Default::default() };
    assert!(
        matches!(req.time_in_force, TimeInForce::Gtc),
        "the model default must stay Gtc — an expiring default would move the parity fixtures"
    );
    c.submit(&req);
    assert!(c.expiry.is_empty(), "a default request records no deadline");
    for ts in [0, MS_PER_DAY, 10 * MS_PER_DAY] {
        c.on_bar(&bar(ts, 100.0, 101.0, 95.0, 100.0));
    }
    let events = drain(&mut c);
    assert!(expired_in(&events).is_empty(), "no expiry on the default path");
    assert!(canceled_in(&events).is_empty(), "no cancel on the default path");
}

/// IOC/FOK are immediate-execution semantics, not a resting deadline — this sweep leaves them
/// alone (documented on `Expiry`), so they cannot silently start disappearing.
#[test]
fn ioc_and_fok_are_untouched_by_the_deadline_sweep() {
    for tif in [TimeInForce::Ioc, TimeInForce::Fok] {
        let mut c = client();
        c.submit(&limit("i1", tif, Some(1), 0));
        assert!(c.expiry.is_empty(), "{tif:?} records no deadline");
        c.on_bar(&bar(10 * MS_PER_DAY, 100.0, 101.0, 95.0, 100.0));
        assert!(expired(&mut c).is_empty(), "{tif:?} is not expired by this check");
    }
}

/// Cancel-then-expire cannot double-emit: a user cancel drops the deadline with the order.
#[test]
fn a_user_cancel_removes_the_deadline() {
    let mut c = client();
    c.submit(&limit("c1", TimeInForce::Gtd, Some(1_000), 0));
    c.cancel("c1");
    assert!(c.expiry.is_empty(), "cancel drops the deadline entry");
    c.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
    assert!(expired(&mut c).is_empty(), "an already-canceled order never expires");
}

/// Expiry uses the model's `OrderExpired` variant, NOT `OrderCanceled{reason:"expired"}` —
/// the distinction a strategy reacting to `OrderEventKind::Expired` depends on, and the reason
/// a paper trade log and a live trade log agree on status for the identical order.
#[test]
fn expiry_emits_order_expired_not_a_cancel() {
    let mut c = client();
    c.submit(&limit("v1", TimeInForce::Gtd, Some(1_000), 0));
    c.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
    let events = drain(&mut c);
    assert_eq!(expired_in(&events), vec![("v1".to_string(), 1_000)]);
    assert!(canceled_in(&events).is_empty(), "expiry must not use the cancel vocabulary");
}

/// RESOLUTION CAVEAT, pinned deliberately: `Bar.ts` is the bar's OPEN, so a deadline lying
/// strictly INSIDE a bar interval is only noticed at the NEXT open — the order survives, and
/// can fill, for up to one interval past its deadline. This is inherent to bar-resolution
/// simulation; the test exists so the overshoot cannot silently change.
#[test]
fn a_deadline_inside_a_bar_overshoots_to_the_next() {
    let mut c = client();
    // 1m bars; deadline 30s into the 12:00:00 bar.
    let open = 12 * 3_600_000;
    c.submit(&limit("o1", TimeInForce::Gtd, Some(open + 30_000), 0));
    // the bar that CONTAINS the deadline: its open is still before it, so the order lives —
    // and the bar's full range fills it, 30s "after" it should have been dead.
    c.on_bar(&bar(open, 100.0, 101.0, 85.0, 95.0));
    assert!(expired(&mut c).is_empty(), "an interior deadline is not seen at the bar open");
    assert_eq!(c.fills.lock().unwrap().len(), 1, "it fills off the containing bar's range");

    // and with no fill available, expiry lands on the next open instead.
    let mut c2 = client();
    c2.submit(&limit("o2", TimeInForce::Gtd, Some(open + 30_000), 0));
    c2.on_bar(&bar(open, 100.0, 101.0, 95.0, 100.0)); // never trades down to 90
    assert!(expired(&mut c2).is_empty(), "still resting through the containing bar");
    c2.on_bar(&bar(open + 60_000, 100.0, 101.0, 95.0, 100.0));
    assert_eq!(expired(&mut c2), vec![("o2".to_string(), open + 60_000)], "expires next open");
}

/// A `Day` order whose request ts was never stamped (`OrderRequest::ts` defaults to 0, and
/// nothing in the write path writes it) must NOT be born expired: the session anchors on the
/// first BAR, so it lives out that bar's UTC day.
#[test]
fn an_unstamped_day_order_is_not_born_expired() {
    let mut c = client();
    c.submit(&limit("u1", TimeInForce::Day, None, 0)); // ts = 0 ⇒ utc_day 0 (1970)
    let today = 20_650 * MS_PER_DAY; // a plausible modern bar clock
    c.on_bar(&bar(today, 100.0, 101.0, 95.0, 100.0));
    assert!(expired(&mut c).is_empty(), "an unstamped Day order survives its first bar");
    assert_eq!(c.pending.len(), 1);
    c.on_bar(&bar(today + MS_PER_DAY, 100.0, 101.0, 95.0, 100.0));
    assert_eq!(expired(&mut c).len(), 1, "and expires on the next UTC day as normal");
}

/// Same guarantee under submitter-vs-bar CLOCK SKEW (the classic seconds-vs-ms mixup): the
/// deadline never depends on the submitter's clock, only on the bar clock.
#[test]
fn a_day_order_is_immune_to_submitter_clock_skew() {
    let mut c = client();
    let today = 20_650 * MS_PER_DAY;
    c.submit(&limit("u2", TimeInForce::Day, None, today / 1_000)); // ts in SECONDS
    c.on_bar(&bar(today, 100.0, 101.0, 95.0, 100.0));
    assert!(expired(&mut c).is_empty(), "a skewed submit ts cannot kill the order early");
    c.on_bar(&bar(today + MS_PER_DAY, 100.0, 101.0, 95.0, 100.0));
    assert_eq!(expired(&mut c).len(), 1);
}

/// An expiring OTO PARENT cascade-cancels its held children: they can only ever arm on the
/// parent's FILL, so leaving them resting-but-inactive would strand them live for the rest of
/// the run.
#[test]
fn an_expiring_bracket_parent_cancels_its_held_children() {
    let mut c = client();
    let mut entry = limit("e1", TimeInForce::Gtd, Some(1_000), 0);
    entry.contingency_type = Some("OTO".into());
    c.submit(&entry);
    for child in ["sl", "tp"] {
        let mut leg = limit(child, TimeInForce::Gtc, None, 0);
        leg.side = -1;
        leg.parent_order_id = Some("e1".to_string());
        leg.linked_order_ids = vec!["sl".to_string(), "tp".to_string()];
        c.submit(&leg);
    }
    assert_eq!(c.pending.len(), 3, "entry + two held exits resting");

    c.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
    let events = drain(&mut c);
    assert_eq!(expired_in(&events), vec![("e1".to_string(), 1_000)], "the parent expires");
    let mut cancels = canceled_in(&events);
    cancels.sort();
    assert_eq!(
        cancels,
        vec![
            ("sl".to_string(), "parent-expired".to_string()),
            ("tp".to_string(), "parent-expired".to_string()),
        ],
        "both held children are cascade-canceled, distinctly reasoned"
    );
    assert!(c.pending.is_empty(), "no orphaned leg left resting");
    assert!(c.contingency.is_empty(), "and no orphaned linkage left behind");
}

/// Per-book expiry under `MultiPaperExecutionClient`: a symbol-less bar advances EVERY book
/// (so an order expires off the shared clock), while a symbol-tagged bar advances only its own
/// book — expiry is driven by the bar stream the book actually receives.
#[test]
fn multi_book_expiry_follows_each_books_own_bar_stream() {
    let mut multi = MultiPaperExecutionClient::new();
    multi.add_book(PaperExecutionClient::new("sim", "AAA", 0.0, 0.0, 0.0));
    multi.add_book(PaperExecutionClient::new("sim", "BBB", 0.0, 0.0, 0.0));
    let mut a = limit("a1", TimeInForce::Gtd, Some(1_000), 0);
    a.symbol = "AAA".into();
    multi.submit(&a);

    // a bar for the OTHER book only: book AAA never advances, so nothing expires.
    let mut b_bar = bar(1_000, 100.0, 101.0, 95.0, 100.0);
    b_bar.symbol = Some("BBB".to_string());
    multi.on_bar(&b_bar);
    assert_eq!(multi.book("AAA").expect("book AAA").pending.len(), 1, "AAA untouched");

    // a symbol-less bar fans out to every book, expiring the AAA order.
    multi.on_bar(&bar(1_000, 100.0, 101.0, 95.0, 100.0));
    assert!(multi.book("AAA").expect("book AAA").pending.is_empty(), "expired off the fanout");
    let expiries: Vec<String> = std::iter::from_fn(|| multi.poll_events())
        .filter_map(|e| match e {
            Event::OrderExpired(x) => Some(x.client_order_id),
            _ => None,
        })
        .collect();
    assert_eq!(expiries, vec!["a1".to_string()], "and surfaces through the router exactly once");
}

#[test]
fn utc_day_floors_across_the_epoch() {
    assert_eq!(utc_day(0), 0);
    assert_eq!(utc_day(MS_PER_DAY - 1), 0);
    assert_eq!(utc_day(MS_PER_DAY), 1);
    assert_eq!(utc_day(-1), -1, "pre-epoch floors down, so days stay monotone");
}
