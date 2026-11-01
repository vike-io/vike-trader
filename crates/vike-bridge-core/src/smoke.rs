//! Venue-NEUTRAL helpers the bridges' live smokes share (`test-support` only, the `scripted` shape):
//! the fill wait over a core snapshot and the user-data pump's ready wait, the resting-order cancel
//! guard, the TIF and post-only smokes' order, the lifecycle word of an event, the leverage smokes'
//! write-and-read-back round trip, the restore smokes' planner pass over a `ReconClient` (the venue
//! and symbol passed in), and the kline smokes' window check. Nothing here reads the environment or
//! the credential store: a smoke's credentials stay in its own test files, and a helper that names
//! one venue's types stays in that venue's tests.

use std::fmt::{Debug, Display};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_exec::recon::{ReconClient, RestorePlan, RestoreScope, RestoredOrderRef, plan_restore};
use vike_exec::{CoreSnapshot, OrderView};
use vike_marketdata::Bar;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderStatus, OrderStatusReport, OrderType, TimeInForce, now_ms};

use crate::leverage_read::{LeverageReader, confirm, same_leverage};
use crate::rest::VenueRest;

/// Polls `snapshot` every hundredth of `within` until `coid` is FILLED with a non-zero quantity and
/// returns its view. Panics when the order is REJECTED or DENIED, or is not filled within `within`;
/// both messages carry the snapshot's recent events.
pub fn wait_filled(
    snapshot: impl Fn() -> Arc<CoreSnapshot>,
    coid: &str,
    within: Duration,
) -> OrderView {
    let every = within / 100;
    let deadline = Instant::now() + within;
    loop {
        let snap = snapshot();
        if let Some(o) = snap.orders.iter().find(|o| o.client_order_id == coid) {
            if o.status == OrderStatus::Filled && o.filled_qty > 0.0 {
                return o.clone();
            }
            assert!(
                !matches!(o.status, OrderStatus::Rejected | OrderStatus::Denied),
                "{coid} {:?}: {:?}",
                o.status,
                snap.recent_events
            );
        }
        assert!(
            Instant::now() < deadline,
            "{coid} never FILLED; recent: {:?}",
            snapshot().recent_events
        );
        std::thread::sleep(every);
    }
}

/// Condition-based settle: blocks until the user-data pump reports it has subscribed (or a 10 s
/// deadline), so the order path never races a cold-start handshake that would drop a fill landing
/// before subscribe — the fixed `sleep(3s)` flake this replaced.
pub fn wait_pump_ready(ready: &AtomicBool) {
    assert!(
        vike_log::poll::wait_until(Duration::from_secs(10), || ready.load(Ordering::Acquire)),
        "user-data pump never became ready"
    );
}

/// A BUY of `qty` on `venue`'s `symbol` at `price` (`None` for a market order) under `tif`: the
/// order the TIF and post-only smokes send, stamped now.
pub fn buy(
    coid: &str,
    venue: &str,
    symbol: &str,
    qty: f64,
    order_type: OrderType,
    price: Option<f64>,
    tif: TimeInForce,
) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.to_string(),
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty,
        order_type,
        price,
        time_in_force: tif,
        ts: now_ms(),
        ..Default::default()
    }
}

/// The `(client_order_id, word)` of an order-lifecycle event, `None` for anything else.
pub fn lifecycle_word(e: &Event) -> Option<(&str, &'static str)> {
    Some(match e {
        Event::OrderSubmitted(x) => (&x.client_order_id, "Submitted"),
        Event::OrderAccepted(x) => (&x.client_order_id, "Accepted"),
        Event::OrderRejected(x) => (&x.client_order_id, "Rejected"),
        Event::OrderTriggered(x) => (&x.client_order_id, "Triggered"),
        Event::OrderFilled(x) => (&x.client_order_id, "Filled"),
        Event::OrderPartiallyFilled(x) => (&x.client_order_id, "PartiallyFilled"),
        Event::OrderCanceled(x) => (&x.client_order_id, "Canceled"),
        Event::OrderCancelRejected(x) => (&x.client_order_id, "CancelRejected"),
        Event::Fill(x) => (&x.client_order_id, "Fill"),
        _ => return None,
    })
}

/// Cancels `coid` through the PLACING client when dropped, so no failure path leaves it resting.
/// `host` names the account in the by-hand line (`"demo"`, `"testnet"`).
pub struct CancelOnDrop<'a, R: VenueRest> {
    pub rest: &'a R,
    pub coid: &'a str,
    pub host: &'static str,
}

impl<R: VenueRest> Drop for CancelOnDrop<'_, R> {
    fn drop(&mut self) {
        match self.rest.cancel_order(self.coid) {
            Ok(()) => tracing::info!(coid = self.coid, "cleanup: cancel sent"),
            Err(e) => tracing::error!(
                coid = self.coid, error = %e.msg,
                "CLEANUP FAILED: cancel the {} order by hand", self.host
            ),
        }
    }
}

// ---- the leverage smokes: write a figure, read it back, put the prior one back -------------------

/// The leverage smokes' WRITE-AND-READ-BACK round trip on one DEMO account's `symbol`: reads the
/// prior figure, writes a small different one through `write` (the bridge's own writer), polls
/// `reader` until the venue holds it (`confirm`, the lane's own poll), then writes the PRIOR figure
/// and reads that back. The restore is armed BEFORE the write and also runs from a drop guard, so a
/// panic in between still puts the figure back. The printed lines name the account as `lane` and the
/// figure as `figure`, each venue's own words (e.g. `"DEMO"` / `"leverage"`). `Ok((prior, target))`
/// on success; `Err` is the skip reason (an open position on `symbol`, which this never moves the
/// leverage under).
pub fn leverage_write_read_back<E: Display + Debug>(
    reader: &dyn LeverageReader,
    symbol: &str,
    lane: &str,
    figure: &str,
    write: impl Fn(f64) -> Result<(), E>,
) -> Result<(f64, f64), String> {
    reader.check_account().unwrap_or_else(|why| panic!("account refused: {why}"));
    let before = reader.held().unwrap_or_else(|why| panic!("held could not be read: {why}"));
    if before.position_open {
        return Err(format!("the demo account holds an open {symbol} position"));
    }
    let prior = before.held;
    let target = if same_leverage(prior, 3.0) { 4.0 } else { 3.0 };
    if let Some(max) = reader.venue_max().expect("the venue maximum") {
        assert!(target <= max, "{target}x is above the venue maximum {max}x");
    }
    println!("\n=== {symbol} ({lane}): prior {prior}x, writing {target}x, restoring {prior}x ===");

    let mut restore = Restore { write: &write, prior, symbol, figure, done: false };
    write(target).expect("set_leverage through the bridge's writer");
    let seen = confirm(reader, target, std::thread::sleep)
        .unwrap_or_else(|why| panic!("the venue never held the written figure: {why}"));
    println!("read back after the write: {seen}x");
    assert!(same_leverage(seen, target));

    restore.run().unwrap_or_else(|why| {
        panic!("RESTORE FAILED: set {symbol} back to {prior}x on the demo account by hand: {why}")
    });
    let restored = confirm(reader, prior, std::thread::sleep).unwrap_or_else(|why| {
        panic!("the prior figure was not read back after the restore: {why}")
    });
    println!("read back after the restore: {restored}x");
    assert!(same_leverage(restored, prior));
    Ok((prior, target))
}

/// Puts the prior leverage back: explicitly (`run`, whose failure the smoke reports) or, if the smoke
/// panics first, from `Drop`. Never panics itself (a panic in `drop` during unwinding aborts).
struct Restore<'a, E: Display> {
    write: &'a dyn Fn(f64) -> Result<(), E>,
    prior: f64,
    symbol: &'a str,
    figure: &'a str,
    done: bool,
}

impl<E: Display> Restore<'_, E> {
    fn run(&mut self) -> Result<(), String> {
        (self.write)(self.prior).map_err(|e| e.to_string())?;
        self.done = true;
        Ok(())
    }
}

impl<E: Display> Drop for Restore<'_, E> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        match self.run() {
            Ok(()) => eprintln!("restored {} to {}x from the drop guard", self.symbol, self.prior),
            Err(why) => eprintln!(
                "RESTORE FAILED: set the DEMO account's {} {} back to {}x by hand: {why}",
                self.symbol, self.figure, self.prior
            ),
        }
    }
}

// ---- the restore smokes: one planner pass over what the venue reports now ------------------------

/// The ref a restart hands the planner for a resting LIMIT `coid` on `venue`'s `symbol`.
pub fn restored_limit(coid: &str, venue: &str, symbol: &str) -> RestoredOrderRef {
    RestoredOrderRef {
        coid: coid.to_string(),
        venue: Some(venue.to_string()),
        symbol: Some(symbol.to_string()),
        account: None,
        order_type: Some(OrderType::Limit),
        ..Default::default()
    }
}

/// One planner pass over what the venue reports now on `venue`'s `symbol` (the scope's ONE symbol,
/// spelled as the caller's engine spells it), plus the order rows it was planned from.
pub fn plan_pass(
    recon: &dyn ReconClient,
    venue: &str,
    symbol: &str,
    restored: &[RestoredOrderRef],
) -> (RestorePlan, Vec<OrderStatusReport>) {
    let scope = RestoreScope {
        venue: venue.to_string(),
        account: None,
        symbols: vec![symbol.to_string()],
        // the REAL client's claim, as the pass carries it (`ReconcileReports::lists_trigger_orders`)
        lists_trigger_orders: recon.lists_trigger_orders(),
    };
    let orders = recon.fetch_order_status_reports(0).expect("fetch_order_status_reports");
    let fills = recon.fetch_fill_reports(now_ms() - 3_600_000).expect("fetch_fill_reports");
    (plan_restore(&scope, restored, &orders, &fills), orders)
}

/// The plan ADOPTS `coid`.
pub fn adopts(plan: &RestorePlan, coid: &str) -> bool {
    plan.adopt.iter().any(|r| r.client_order_id.as_deref() == Some(coid))
}

/// Absent from the venue: `gone` (a terminal report) or `unproven` (silence, which is no proof by
/// itself since the restart-safe-orders work: `plan_restore` makes it gone only after the quiet
/// passes the core counts, and a smoke makes a single pass).
pub fn is_gone(plan: &RestorePlan, coid: &str) -> bool {
    plan.gone.iter().chain(plan.unproven.iter()).any(|r| r.coid == coid)
}

/// Polls every 500 ms until the planner no longer keeps `coid` alive (`gone`, never `adopt`) and
/// returns the order rows of the pass that said so; panics with the last plan after 15 s.
pub fn wait_until_gone(
    recon: &dyn ReconClient,
    venue: &str,
    symbol: &str,
    restored: &[RestoredOrderRef],
    coid: &str,
) -> Vec<OrderStatusReport> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (plan, orders) = plan_pass(recon, venue, symbol, restored);
        if is_gone(&plan, coid) && !adopts(&plan, coid) {
            return orders;
        }
        assert!(
            Instant::now() < deadline,
            "{coid} still not gone at the venue 15 s after the engine cancel: {plan:?}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

// ---- the kline smokes ---------------------------------------------------------------------------

/// Shared assertions on any returned window: ascending + de-duplicated, clipped to the request, and
/// carrying sane OHLCV (`volume` is BASE units, so it stays small: a turnover/cost-for-volume mix-up
/// shows up here as a wildly large number).
pub fn assert_window(bars: &[Bar], start_ms: i64, end_ms: i64) {
    assert!(bars.windows(2).all(|w| w[0].ts < w[1].ts), "ascending by ts and de-duplicated");
    for b in bars {
        assert!(b.ts >= start_ms && b.ts <= end_ms, "bar {} outside the request window", b.ts);
        assert!(b.open > 0.0 && b.close > 0.0, "prices are positive");
        assert!(b.high >= b.low, "high/low are not swapped");
        assert!(b.high >= b.open && b.high >= b.close, "high bounds the bar");
        assert!(b.low <= b.open && b.low <= b.close, "low bounds the bar");
        assert!(b.volume >= 0.0 && b.volume.is_finite(), "volume is base units");
        assert!(b.symbol.is_none(), "the series key carries the symbol, not the bar");
    }
}

#[cfg(test)]
#[path = "smoke_tests.rs"]
mod smoke_tests;
