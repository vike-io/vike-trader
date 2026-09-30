//! The RiskGate must judge an order against ONE instrument — the ORDER's.
//!
//! An `ExecutionEngine` admits orders for its primary symbol OR anything in
//! `extra_symbols` (`accepts_symbol`), so a foreign-symbol order is reachable today. The
//! gate, however, used to read a MIXTURE of two instruments when building its
//! `RiskContext`: the ORDER symbol's `multiplier_of` and `im_for`, but the ENGINE
//! symbol's position (`gate_position_size`), reversal credit and priceless mark.
//!
//! That mixture is worse than either basis alone, and it is the code that admits real
//! orders. Its sharpest consequence is the one pinned below: `is_covered_reduce` reads
//! `ctx.position_size`, and a covered reduce BYPASSES the min-qty / min-notional floors
//! (deliberately — an anti-stranding rule, so a protective bracket leg can always close
//! a position). Fed the WRONG symbol's position, a brand-new short in instrument B is
//! laundered past those floors because the account happens to be long instrument A.
//!
//! Every single-symbol engine is unaffected: `request.symbol == self.symbol` there, so
//! the same `f64` is read either way.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate, RiskLimits,
};
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent};

const VENUE: &str = "binance";
const MOUNTED: &str = "BTCUSDT";
const EXTRA: &str = "ETHUSDT";

/// An engine mounting `BTCUSDT` that also accepts `ETHUSDT`, with a min-qty floor high
/// enough that only a COVERED REDUCE can get a small order past it.
fn engine() -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1_000_000.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits { min_qty: Some(10.0), ..RiskLimits::new() }),
        RecordingClient::default(),
        VENUE,
        MOUNTED,
    );
    e.extra_symbols = vec![EXTRA.to_string()];
    e
}

fn fill(
    coid: &str,
    trade_id: &'static str,
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
) -> FillEvent {
    FillEvent {
        trade_id: trade_id.into(),
        client_order_id: coid.to_string(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn order(coid: &str, symbol: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: VENUE.into(),
        symbol: symbol.into(),
        side,
        qty,
        order_type: "market".into(),
        reduce_only,
        ..Default::default()
    }
}

/// Establish a genuine +10 BTCUSDT position by submitting and filling a passing order.
fn long_10_btc(e: &mut ExecutionEngine<RecordingClient>) {
    let mut ob = Outbox::default();
    e.submit_order(&order("seed", MOUNTED, 1, 10.0, false), 1, &mut ob);
    e.on_event(&Event::Fill(fill("seed", "t-seed", MOUNTED, 1, 10.0, 60_000.0)), &mut ob);
    let p = e
        .account
        .positions
        .get(&(VENUE.into(), MOUNTED.into(), "BOTH".into()))
        .map(|p| p.size)
        .unwrap_or(0.0);
    assert!((p - 10.0).abs() < 1e-12, "seed position must be +10 {MOUNTED}, got {p}");
}

/// THE FIX. Long 10 BTCUSDT; a small `reduce_only` SELL of ETHUSDT is an OPENING short in
/// an instrument the account is FLAT in, so the min-qty floor must apply and deny it.
///
/// FAILS ON THE PRE-FIX CODE: `ctx.position_size` was the BTCUSDT +10, so
/// `is_covered_reduce(true, -1, +10.0, 0.5)` returned true, the floor was bypassed, and a
/// brand-new sub-floor short reached the venue.
#[test]
fn foreign_symbol_sell_is_not_laundered_by_the_mounted_symbols_long() {
    let mut e = engine();
    long_10_btc(&mut e);

    let before = e.client.submissions.len();
    let mut ob = Outbox::default();
    e.submit_order(&order("eth-1", EXTRA, -1, 0.5, true), 2, &mut ob);

    assert_eq!(
        e.client.submissions.len(),
        before,
        "a sub-floor OPENING short in {EXTRA} must be denied: the account is flat in {EXTRA}, \
         and the +10 {MOUNTED} position is a different instrument"
    );
}

/// The anti-stranding bypass must still work where it is legitimate: the SAME small
/// `reduce_only` sell, on the symbol actually held, is a covered reduce and passes the
/// floor. Without this, the fix above could have been "make everything deny".
#[test]
fn a_genuine_covered_reduce_on_the_held_symbol_still_bypasses_the_floor() {
    let mut e = engine();
    long_10_btc(&mut e);

    let before = e.client.submissions.len();
    let mut ob = Outbox::default();
    e.submit_order(&order("btc-red", MOUNTED, -1, 0.5, true), 2, &mut ob);

    assert_eq!(
        e.client.submissions.len(),
        before + 1,
        "a sub-floor reduce COVERED by the real {MOUNTED} position must still pass the floor"
    );
}

/// The converse direction: a foreign-symbol order must not be blocked by a position it has
/// nothing to do with either. Flat in BTCUSDT, an above-floor ETHUSDT open is admitted.
#[test]
fn foreign_symbol_open_above_the_floor_is_admitted() {
    let mut e = engine();
    let before = e.client.submissions.len();
    let mut ob = Outbox::default();
    e.submit_order(&order("eth-2", EXTRA, -1, 25.0, false), 2, &mut ob);
    assert_eq!(
        e.client.submissions.len(),
        before + 1,
        "an above-floor open in {EXTRA} is an ordinary order and must be admitted"
    );
}

/// A single-symbol engine is byte-identical: `request.symbol == self.symbol`, so the
/// changed reads return the same `f64` they did before. Pinned so a later refactor cannot
/// quietly alter the common path while chasing the multi-symbol one.
#[test]
fn single_symbol_engine_behaviour_is_unchanged() {
    let mut e = ExecutionEngine::new(
        Account::new(1_000_000.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits { min_qty: Some(10.0), ..RiskLimits::new() }),
        RecordingClient::default(),
        VENUE,
        MOUNTED,
    );
    // no extra_symbols: the only reachable symbol is the mounted one
    let mut ob = Outbox::default();
    e.submit_order(&order("s1", MOUNTED, 1, 10.0, false), 1, &mut ob);
    e.on_event(&Event::Fill(fill("s1", "t1", MOUNTED, 1, 10.0, 60_000.0)), &mut ob);

    // covered reduce bypasses the floor
    let before = e.client.submissions.len();
    e.submit_order(&order("s2", MOUNTED, -1, 0.5, true), 2, &mut ob);
    assert_eq!(e.client.submissions.len(), before + 1, "covered reduce still bypasses");

    // a sub-floor OPEN is still denied
    let before = e.client.submissions.len();
    e.submit_order(&order("s3", MOUNTED, 1, 0.5, false), 3, &mut ob);
    assert_eq!(e.client.submissions.len(), before, "sub-floor open still denied");
}
