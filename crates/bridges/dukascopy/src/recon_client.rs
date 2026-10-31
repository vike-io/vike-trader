//! Dukascopy `ReconClient` — the venue-facing report seam (`vike_exec::recon::ReconClient`) for the one venue with NO
//! REST API. Every other venue's client opens a dedicated signed transport and PULLS reports on demand; Dukascopy's only
//! trading API is the JForex Java sidecar, spoken over JSON-lines stdio, a channel owned end to end by
//! [`crate::exec`]'s reader thread. So this client PULLS through it: each fetch asks the sidecar the `recon` question
//! ([`crate::recon_query::ReconLink::ask`], `ReconSnapshot.java` is the Java half) and maps the ONE answer, what the
//! venue lists NOW from `engine.getOrders()`, to the shared report rows.
//!
//! ## What each report is built from
//!
//! * **Orders**: the orders the venue shows RESTING (`OPENED`, or `CREATED` and not yet numbered), with the
//!   client_order_id the label carries (an order opened in the platform carries none and is an `UnknownOrder` to the
//!   diff, as on every venue). FILLED orders are positions, not orders, and are not listed (every wired client reports
//!   OPEN orders only, and a long-lived position listed as an order would be an unknown order for ever once the node
//!   reaped its own). **The node's native protections are reported too**: a stop-loss or a take-profit is no order at the
//!   venue, it is a price on a FILLED order, and the venue stores no coid for it, so without this the node's resting stop
//!   (and a bracket's take-profit, a reduce-only limit) would be an orphan local order every pass. The client knows the
//!   protections the node sent ([`ReconSource::protections`]); one is reported, under its coid, while the venue's
//!   FILLED orders on the other side still carry its price.
//! * **Positions**: the net per instrument the sidecar computed over the FILLED orders ([`crate::proto::ReconPosition`]),
//!   plus a FLAT row for every symbol the node may hold ([`crate::recon_query::ReconLink::known_symbols`]), because
//!   `vike_exec::recon::diff` raises nothing for an absent row: the flat row is what shows a position the venue closed
//!   unheard (decision 0200's phantom) as a `PositionDrift`, where the push-line snapshot this replaced read "in sync".
//! * **Fills**: one fill per FILLED order (an open position) the venue shows, filled since `since`, under the trade id
//!   `<order id>:1`, the id `OrderMap.java`'s `entryTradeId` mints for the first fill of an order. A fill the node folded
//!   is skipped by the diff on that id. ⚠ **Closed orders are NOT reported, and that is not an omission.** The venue's
//!   history cannot name the client_order_id of whoever closed an order, which a closing fill's id carries
//!   (`closeTradeId`), and an entry fill's sequence is a process-local counter, so a fill rebuilt from history would be
//!   under an id the node never folded and would book a fill twice. The residual is stated in `crates/bridges/dukascopy/CLAUDE.md`.
//!
//! ## An unreadable answer is an ERROR
//!
//! Every fetch is `Err(words)` when the question could not be answered ([`crate::recon_query`]'s module doc lists the
//! ways), never `Ok(vec![])`: an empty report is a claim ("the venue holds nothing") that the restore planner and the
//! diff act on. The failure is also said once a minute at `error` here.
//!
//! ## Policy and the rollout rule (decision 0043)
//!
//! Handing the node this client gives the default `quarantine` something to hold; it folds nothing. ⚠ **Do not set
//! `hybrid` or `synthesize` for this venue**: its POSITION report is not live-proven complete (whether
//! `engine.getOrders()` lists every order at every moment is unmeasured), and `PositionDrift` rewrites local size and
//! books realized PnL. No balance leg: the venue's balance is the previous end-of-day figure (`IAccount.getBalance()`),
//! never a live one, so `fetch_balance` keeps the trait's "not reported".
//!
//! ## ⚠ A client of a restarted GATEWAYHUB does not know the protections the dead one's node sent
//!
//! The protections are the exec client's own memory ([`crate::exec`]'s `Shared::protections`). A gatewayhub restart is a
//! new client with none, so a stop or take-profit the node restored cannot be named and is absent from the report; a
//! restored take-profit (a plain `limit` kind) on the MOUNTED symbol is then judged by the restore planner's silence
//! rule. The venue stays outside `vike_gatewayhub_client::trade::RECON_READS_ADMITTED` for this reason (an admitted
//! symbol's restored orders stay held). `crates/bridges/dukascopy/CLAUDE.md` and the decision record carry it.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_exec::recon::ReconClient;
use vike_model::events::{LiquiditySide, PositionSide, TradeId};
use vike_model::{
    FillReport, MarginMode, OrderRequest, OrderStatus, OrderStatusReport, OrderType,
    PositionStatusReport,
};

use crate::proto::ReconOrder;
use crate::recon_query::ReconAnswer;

/// The venue key stamped on every report row.
pub const VENUE: &str = "dukascopy";

/// How close (relative) a protection's price must be to the price the venue holds on an order to be that protection:
/// the sidecar puts a trigger on the instrument's grid (a tenth of a pip, half to even), so the two differ by at most
/// that, far under half a pip (`5e-5` of the price is half a pip of a EUR pair and under a pip of a yen pair).
pub const PROTECTION_PRICE_TOLERANCE: f64 = 5e-5;

/// How often a failed query is said at `error` (every fetch of every pass fails the same way while it fails).
const SAY_EVERY: Duration = Duration::from_secs(60);

/// One venue-authoritative net position line: the signed net size in UNITS and the signed-amount-weighted average of the
/// remaining orders' open prices, exactly as [`crate::proto::ReconPosition`] carries them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VenuePosition {
    /// Signed net size in UNITS: `>0` long, `<0` short, `0` flat.
    pub size: f64,
    /// Signed-amount-weighted average open price; `0.0` when flat.
    pub avg_px: f64,
    /// When the venue was read (epoch ms).
    pub ts: i64,
}

/// Map one venue net position → a normalized [`PositionStatusReport`]. Dukascopy is a NET / one-way FX account (like
/// OANDA / binance-perp / ibkr): `position_side` is [`PositionSide::Both`] and the SIGN of `qty` carries direction. No
/// isolated wallet → the `margin_mode`/`isolated_margin` defaults (Cross / `None`). Pure.
pub fn to_position_report(symbol: &str, p: &VenuePosition) -> PositionStatusReport {
    PositionStatusReport {
        venue: VENUE.to_string(),
        symbol: symbol.to_string(),
        position_side: PositionSide::Both,
        qty: p.size,
        avg_px: p.avg_px,
        ts: p.ts,
        margin_mode: MarginMode::default(),
        isolated_margin: None,
        delta: None,
    }
}

/// What the reconcile client reads from the running exec client (`crate::exec`'s `ReconHandle`): the one question, the
/// protections the node sent, and the symbols that get a flat row. A trait so the client's mapping and its error paths
/// are tested without a child process.
pub trait ReconSource: Send + Sync {
    /// Ask the sidecar what the venue holds now ([`crate::recon_query::ReconLink::ask`]): every failure is an `Err`.
    fn ask(&self) -> Result<ReconAnswer, String>;
    /// The native protections the sidecar acknowledged and the node still holds resting, `(coid, request)`.
    fn protections(&self) -> Vec<(String, OrderRequest)>;
    /// The engine admitted `symbol`: it gets a flat row from now on.
    fn know(&self, symbol: &str);
    /// The engine gave `symbol` back.
    fn forget(&self, symbol: &str);
    /// The symbols that get a flat row.
    fn known_symbols(&self) -> Vec<String>;
}

/// The order kind of an SDK order command: a pending `BUYLIMIT`/`SELLLIMIT` is a limit, a `BUYSTOP`/`SELLSTOP` an entry
/// stop (never wired, but the platform can hold one), a bare `BUY`/`SELL` a market order still being placed; any other
/// word (`PLACE_BID`, `BUYLIMIT_BYBID`, ...) stays the venue's own spelling.
fn order_type_of(command: &str) -> OrderType {
    match command {
        "BUYLIMIT" | "SELLLIMIT" => OrderType::Limit,
        "BUYSTOP" | "SELLSTOP" => OrderType::Stop,
        "BUY" | "SELL" => OrderType::Market,
        other => OrderType::Unrecognized(other.to_ascii_lowercase()),
    }
}

/// Whether the venue's price `held` on an order is the protection's price `want`.
fn same_price(held: f64, want: f64) -> bool {
    held > 0.0
        && (held - want).abs() <= PROTECTION_PRICE_TOLERANCE * want.abs().max(f64::MIN_POSITIVE)
}

/// The order report: the orders the venue shows resting, then the node's protections the venue still holds. See the
/// module doc for why FILLED orders are not here and why the protections are. Sorted by symbol then id.
pub fn order_reports(
    answer: &ReconAnswer,
    protections: &[(String, OrderRequest)],
) -> Vec<OrderStatusReport> {
    let mut reports: Vec<OrderStatusReport> = Vec::new();
    for o in answer.orders.iter().filter(|o| matches!(o.state.as_str(), "OPENED" | "CREATED")) {
        reports.push(OrderStatusReport {
            venue: VENUE.to_string(),
            symbol: o.symbol.clone(),
            venue_order_id: o.order_id.as_str().into(),
            client_order_id: o.coid.clone(),
            side: o.side,
            order_type: order_type_of(&o.command),
            qty: o.requested,
            filled_qty: o.amount,
            avg_px: if o.amount > 0.0 { o.price } else { 0.0 },
            status: if o.amount > 0.0 {
                OrderStatus::PartiallyFilled
            } else {
                OrderStatus::Accepted
            },
            ts: if o.created > 0 { o.created } else { answer.ts },
        });
    }
    for (coid, request) in protections {
        // The price the node asked the venue to hold, and which price of an order that is.
        let (tag, want) = match request.order_type {
            OrderType::Stop => ("sl", request.trigger_price),
            OrderType::Limit => ("tp", request.price),
            _ => continue,
        };
        let held = |o: &ReconOrder| if tag == "sl" { o.stop_loss } else { o.take_profit };
        let Some(want) = want.filter(|p| p.is_finite() && *p > 0.0) else { continue };
        // The orders a protection covers face it: a SELL stop protects the LONG orders.
        let legs: Vec<&ReconOrder> = answer
            .orders
            .iter()
            .filter(|o| {
                o.state == "FILLED"
                    && o.symbol.eq_ignore_ascii_case(&request.symbol)
                    && o.side == -request.side
                    && same_price(held(o), want)
            })
            .collect();
        if legs.is_empty() {
            continue; // the venue no longer holds it: absent, which the diff and the restore planner read as gone
        }
        let ids: Vec<&str> = legs.iter().map(|o| o.order_id.as_str()).collect();
        reports.push(OrderStatusReport {
            venue: VENUE.to_string(),
            symbol: request.symbol.clone(),
            venue_order_id: format!("{tag}:{}", ids.join(",")).as_str().into(),
            client_order_id: Some(coid.clone()),
            side: request.side,
            order_type: request.order_type.clone(),
            qty: request.qty,
            filled_qty: 0.0,
            avg_px: 0.0,
            status: OrderStatus::Accepted,
            ts: answer.ts,
        });
    }
    reports.sort_by(|a, b| {
        (a.symbol.as_str(), a.venue_order_id.as_str())
            .cmp(&(b.symbol.as_str(), b.venue_order_id.as_str()))
    });
    reports
}

/// The engine's sign for the venue's commission: the venue states a charge NEGATIVE (`IOrder.getCommission()`), the
/// engine books a charge POSITIVE. A figure that is not a charge reports nothing.
fn charge(venue_commission: f64) -> f64 {
    if venue_commission.is_finite() && venue_commission < 0.0 { -venue_commission } else { 0.0 }
}

/// The fill report: one fill per FILLED order (an open position) filled at or after `since` (`0` = no bound), under the
/// trade id the sidecar minted for the order's first fill. See the module doc for why closed orders are not here.
pub fn fill_reports(answer: &ReconAnswer, since: i64) -> Vec<FillReport> {
    let mut reports: Vec<FillReport> = Vec::new();
    for o in answer
        .orders
        .iter()
        .filter(|o| o.state == "FILLED" && o.amount > 0.0 && !o.order_id.is_empty())
    {
        let ts = if o.filled > 0 { o.filled } else { o.created };
        if since > 0 && ts < since {
            continue;
        }
        let Ok(trade_id) = TradeId::new(format!("{}:1", o.order_id)) else { continue };
        reports.push(FillReport {
            venue: VENUE.to_string(),
            symbol: o.symbol.clone(),
            trade_id,
            venue_order_id: o.order_id.as_str().into(),
            client_order_id: o.coid.clone(),
            side: o.side,
            last_qty: o.amount,
            last_px: o.price,
            commission: charge(o.commission),
            // The fill states no asset, which the engine reads as the account's currency.
            commission_asset: String::new(),
            liquidity_side: LiquiditySide::Taker,
            ts,
        });
    }
    reports.sort_by(|a, b| a.trade_id.cmp(&b.trade_id));
    reports
}

/// The position report: the venue's net per instrument, and a FLAT row for every `known` symbol the venue lists none on
/// (the row `vike_exec::recon::diff` needs to see a position the venue closed). A symbol the venue lists that the client
/// was never told of is reported all the same (a position the node does not hold is a `PositionOnlyExternal`). Sorted by
/// symbol.
pub fn position_reports(answer: &ReconAnswer, known: &[String]) -> Vec<PositionStatusReport> {
    let mut rows: std::collections::BTreeMap<String, PositionStatusReport> =
        std::collections::BTreeMap::new();
    for p in &answer.positions {
        let symbol = p.symbol.to_ascii_uppercase();
        let venue_position = VenuePosition { size: p.size, avg_px: p.avg_px, ts: answer.ts };
        rows.insert(symbol.clone(), to_position_report(&symbol, &venue_position));
    }
    for symbol in known {
        rows.entry(symbol.clone()).or_insert_with(|| {
            let mut flat = PositionStatusReport::flat(VENUE, symbol);
            flat.ts = answer.ts;
            flat
        });
    }
    rows.into_values().collect()
}

/// The Dukascopy reconcile client: three pulls over one sidecar question. Built from a running
/// [`crate::exec::DukascopyExecutionClient`] via its `recon_client()` (there is no endpoint to connect to: the venue's
/// truth arrives only through the sidecar's stdio that client owns).
pub struct DukascopyReconClient {
    source: Arc<dyn ReconSource>,
    /// When a failed query was last said at `error` (see [`SAY_EVERY`]).
    said: Mutex<Option<Instant>>,
}

impl DukascopyReconClient {
    /// A client over `source` (the exec client's handle, or a test double).
    pub fn new(source: Arc<dyn ReconSource>) -> Self {
        Self { source, said: Mutex::new(None) }
    }

    /// One question, and its failure said at `error` at most once a minute (the failure itself is returned every time).
    fn read(&self) -> Result<ReconAnswer, String> {
        let answer = self.source.ask();
        if let Err(error) = &answer {
            let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
            if said.is_none_or(|at| at.elapsed() >= SAY_EVERY) {
                *said = Some(Instant::now());
                tracing::error!(
                    target: "vike_dukascopy::recon",
                    %error,
                    "the dukascopy reconcile query FAILED: the venue's orders and positions were NOT read, so this pass \
                     compares nothing (an unreadable answer is an error, never 'no orders')"
                );
            }
        }
        answer
    }
}

impl ReconClient for DukascopyReconClient {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        let answer = self.read()?;
        Ok(order_reports(&answer, &self.source.protections()))
    }

    fn fetch_fill_reports(&self, since: i64) -> Result<Vec<FillReport>, String> {
        Ok(fill_reports(&self.read()?, since))
    }

    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        let answer = self.read()?;
        Ok(position_reports(&answer, &self.source.known_symbols()))
    }

    /// The engine admitted `symbol`: it gets a flat row. The read itself spans the whole account either way.
    fn admit_symbol(&self, symbol: &str) {
        self.source.know(symbol);
    }

    fn forget_symbol(&self, symbol: &str) {
        self.source.forget(symbol);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::proto::ReconPosition;

    fn row(
        id: &str,
        coid: Option<&str>,
        symbol: &str,
        side: i32,
        state: &str,
        command: &str,
        amount: f64,
        requested: f64,
        price: f64,
    ) -> ReconOrder {
        ReconOrder {
            order_id: id.into(),
            coid: coid.map(str::to_string),
            symbol: symbol.into(),
            side,
            state: state.into(),
            command: command.into(),
            amount,
            requested,
            price,
            stop_loss: 0.0,
            take_profit: 0.0,
            commission: -0.03,
            created: 1_000,
            filled: if state == "FILLED" { 2_000 } else { 0 },
        }
    }

    fn answer(orders: Vec<ReconOrder>, positions: Vec<ReconPosition>) -> ReconAnswer {
        ReconAnswer { orders, positions, foreign: 0, ts: 9_000 }
    }

    fn stop(coid: &str, symbol: &str, side: i32, trigger: f64) -> (String, OrderRequest) {
        (
            coid.to_string(),
            OrderRequest {
                client_order_id: coid.into(),
                venue: VENUE.into(),
                symbol: symbol.into(),
                side,
                qty: 1000.0,
                order_type: OrderType::Stop,
                trigger_price: Some(trigger),
                reduce_only: true,
                ..Default::default()
            },
        )
    }

    fn take_profit(coid: &str, symbol: &str, side: i32, price: f64) -> (String, OrderRequest) {
        (
            coid.to_string(),
            OrderRequest {
                client_order_id: coid.into(),
                venue: VENUE.into(),
                symbol: symbol.into(),
                side,
                qty: 1000.0,
                order_type: OrderType::Limit,
                price: Some(price),
                reduce_only: true,
                ..Default::default()
            },
        )
    }

    // ---- the order report ------------------------------------------------------------------

    /// A resting order of ours names its coid and is `Accepted`; one opened in the platform carries no coid; a FILLED order
    /// is a position and is no order at all.
    #[test]
    fn resting_orders_are_reported_with_their_coids_and_filled_orders_are_not_orders() {
        let a = answer(
            vec![
                row("11", Some("c1"), "EURUSD", 1, "OPENED", "BUYLIMIT", 0.0, 2000.0, 1.05),
                row("12", None, "EURUSD", -1, "OPENED", "SELLLIMIT", 0.0, 1000.0, 1.2),
                row("13", Some("c3"), "USDJPY", 1, "CREATED", "BUYLIMIT", 0.0, 1000.0, 150.0),
                row("14", Some("c4"), "USDJPY", 1, "FILLED", "BUY", 1000.0, 1000.0, 156.9),
            ],
            vec![],
        );
        let reports = order_reports(&a, &[]);
        assert_eq!(reports.len(), 3, "the FILLED order is a position, not an order: {reports:?}");
        let c1 = reports.iter().find(|r| r.venue_order_id.as_str() == "11").expect("c1");
        assert_eq!(c1.client_order_id.as_deref(), Some("c1"));
        assert_eq!(c1.venue, "dukascopy");
        assert_eq!(c1.order_type, OrderType::Limit);
        assert_eq!((c1.side, c1.qty, c1.filled_qty, c1.avg_px), (1, 2000.0, 0.0, 0.0));
        assert_eq!(c1.status, OrderStatus::Accepted);
        assert_eq!(c1.ts, 1_000);
        let platform =
            reports.iter().find(|r| r.venue_order_id.as_str() == "12").expect("platform");
        assert_eq!(platform.client_order_id, None, "an order opened in the platform names no coid");
        assert!(reports.iter().all(|r| r.client_order_id.as_deref() != Some("c4")));
        // sorted by symbol, then id
        let order: Vec<&str> = reports.iter().map(|r| r.venue_order_id.as_str()).collect();
        assert_eq!(order, ["11", "12", "13"]);
    }

    /// A resting order that is partly filled says so.
    #[test]
    fn a_partly_filled_resting_order_is_partially_filled() {
        let a = answer(
            vec![row("11", Some("c1"), "EURUSD", 1, "OPENED", "BUYLIMIT", 1000.0, 3000.0, 1.05)],
            vec![],
        );
        let r = &order_reports(&a, &[])[0];
        assert_eq!(r.status, OrderStatus::PartiallyFilled);
        assert_eq!((r.qty, r.filled_qty, r.avg_px), (3000.0, 1000.0, 1.05));
    }

    #[test]
    fn the_sdk_commands_map_to_the_order_kinds() {
        assert_eq!(order_type_of("BUYLIMIT"), OrderType::Limit);
        assert_eq!(order_type_of("SELLLIMIT"), OrderType::Limit);
        assert_eq!(order_type_of("BUYSTOP"), OrderType::Stop);
        assert_eq!(order_type_of("SELL"), OrderType::Market);
        assert_eq!(order_type_of("PLACE_BID"), OrderType::Unrecognized("place_bid".into()));
    }

    /// **A native stop-loss is a price on a FILLED order, and the node's resting stop is reported while the venue still
    /// holds that price on an order facing it** (a SELL stop covers LONG orders), under the stop's own coid.
    #[test]
    fn a_resting_stop_loss_is_reported_while_the_venue_holds_its_price_on_an_order_it_covers() {
        let mut long = row("21", Some("m1"), "USDJPY", 1, "FILLED", "BUY", 1000.0, 1000.0, 156.9);
        long.stop_loss = 156.5;
        let a = answer(vec![long], vec![]);
        let reports = order_reports(&a, &[stop("s1", "USDJPY", -1, 156.5)]);
        assert_eq!(reports.len(), 1, "{reports:?}");
        let s = &reports[0];
        assert_eq!(s.client_order_id.as_deref(), Some("s1"));
        assert_eq!(s.order_type, OrderType::Stop);
        assert_eq!((s.side, s.qty, s.status), (-1, 1000.0, OrderStatus::Accepted));
        assert_eq!(s.venue_order_id.as_str(), "sl:21");

        // the grid snap (a tenth of a pip) is inside the tolerance
        assert_eq!(order_reports(&a, &[stop("s1", "USDJPY", -1, 156.5004)]).len(), 1);
        // the venue holds ANOTHER price: the node's stop is absent (gone at the venue), not reported
        assert!(order_reports(&a, &[stop("s1", "USDJPY", -1, 156.0)]).is_empty());
        // a stop on the SAME side as the order covers nothing
        assert!(order_reports(&a, &[stop("s1", "USDJPY", 1, 156.5)]).is_empty());
        // another symbol's order carries it: not this stop's
        assert!(order_reports(&a, &[stop("s1", "EURUSD", -1, 156.5)]).is_empty());
        // the venue carries no stop-loss any more
        let mut bare = row("21", Some("m1"), "USDJPY", 1, "FILLED", "BUY", 1000.0, 1000.0, 156.9);
        bare.stop_loss = 0.0;
        assert!(
            order_reports(&answer(vec![bare], vec![]), &[stop("s1", "USDJPY", -1, 156.5)])
                .is_empty()
        );
    }

    /// The take-profit is the stop-loss's twin: a reduce-only `limit` is the `take_profit` price of the order.
    #[test]
    fn a_resting_take_profit_is_reported_from_the_take_profit_price_not_the_stop_loss() {
        let mut long = row("21", Some("m1"), "EURUSD", 1, "FILLED", "BUY", 1000.0, 1000.0, 1.1);
        long.take_profit = 1.12;
        long.stop_loss = 1.09;
        let a = answer(vec![long], vec![]);
        let reports = order_reports(&a, &[take_profit("t1", "EURUSD", -1, 1.12)]);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].order_type, OrderType::Limit);
        assert_eq!(reports[0].venue_order_id.as_str(), "tp:21");
        // the stop-loss price is not a take-profit
        assert!(order_reports(&a, &[take_profit("t1", "EURUSD", -1, 1.09)]).is_empty());
        // one order may carry both: both are reported
        let both = order_reports(
            &a,
            &[take_profit("t1", "EURUSD", -1, 1.12), stop("s1", "EURUSD", -1, 1.09)],
        );
        assert_eq!(both.len(), 2);
    }

    /// One stop over several orders: every covered order is named in the venue id.
    #[test]
    fn one_stop_over_several_orders_names_them_all() {
        let mut a1 = row("21", Some("m1"), "USDJPY", 1, "FILLED", "BUY", 1000.0, 1000.0, 156.9);
        let mut a2 = row("22", Some("m2"), "USDJPY", 1, "FILLED", "BUY", 1000.0, 1000.0, 157.0);
        a1.stop_loss = 156.5;
        a2.stop_loss = 156.5;
        let r = order_reports(&answer(vec![a1, a2], vec![]), &[stop("s1", "USDJPY", -1, 156.5)]);
        assert_eq!(r[0].venue_order_id.as_str(), "sl:21,22");
    }

    // ---- the position report ---------------------------------------------------------------

    /// The venue's rows are reported, sorted, a symbol the venue lists no order on and the client knows is FLAT (the row that
    /// shows a position closed unheard), and a symbol the client was never told of is reported all the same.
    #[test]
    fn positions_are_the_venues_rows_plus_a_flat_row_for_every_known_symbol() {
        let a = answer(
            vec![],
            vec![
                ReconPosition { symbol: "USDJPY".into(), size: 2000.0, avg_px: 155.25 },
                ReconPosition { symbol: "GBPUSD".into(), size: -1000.0, avg_px: 1.3 },
            ],
        );
        let known = vec!["EURUSD".to_string(), "USDJPY".to_string()];
        let rows = position_reports(&a, &known);
        let symbols: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
        assert_eq!(symbols, ["EURUSD", "GBPUSD", "USDJPY"]);
        assert_eq!(rows[0].qty, 0.0, "EURUSD: the venue lists no order on it, so it is flat");
        assert_eq!(rows[0].avg_px, 0.0);
        assert_eq!(rows[0].ts, 9_000);
        assert_eq!(rows[1].qty, -1000.0, "a position the client was never told of is reported");
        assert_eq!(
            rows[2].qty, 2000.0,
            "a known symbol the venue lists keeps the venue's row, not a flat one"
        );
        assert_eq!(rows[2].avg_px, 155.25);
        assert!(
            rows.iter().all(|r| r.venue == "dukascopy" && r.position_side == PositionSide::Both)
        );
    }

    /// A flat venue with no known symbol is an empty report, and that is an ANSWER (the sidecar said so).
    #[test]
    fn a_flat_venue_the_client_knows_no_symbol_on_is_an_empty_report() {
        assert!(position_reports(&answer(vec![], vec![]), &[]).is_empty());
    }

    // ---- the fill report -------------------------------------------------------------------

    /// One fill per FILLED order, under the id of its first fill; the commission in the engine's sign; the lookback bound.
    #[test]
    fn a_filled_order_is_one_fill_under_its_first_trade_id_and_the_lookback_bounds_it() {
        let mut older = row("31", Some("m0"), "EURUSD", 1, "FILLED", "BUY", 1000.0, 1000.0, 1.1);
        older.filled = 500;
        let a = answer(
            vec![
                row("32", Some("m1"), "USDJPY", -1, "FILLED", "SELL", 2000.0, 2000.0, 156.9),
                older,
                row("33", Some("r1"), "EURUSD", 1, "OPENED", "BUYLIMIT", 0.0, 1000.0, 1.0),
                row("34", None, "EURUSD", 1, "FILLED", "BUY", 1000.0, 1000.0, 1.2),
            ],
            vec![],
        );
        let all = fill_reports(&a, 0);
        assert_eq!(all.len(), 3, "a resting order is no fill: {all:?}");
        let f = all.iter().find(|f| f.venue_order_id.as_str() == "32").expect("32");
        assert_eq!(f.trade_id.as_str(), "32:1", "OrderMap.entryTradeId(orderId, 1)");
        assert_eq!(f.client_order_id.as_deref(), Some("m1"));
        assert_eq!((f.side, f.last_qty, f.last_px), (-1, 2000.0, 156.9));
        assert_eq!(f.commission, 0.03, "the venue's charge is negative; the engine's is positive");
        assert_eq!(f.liquidity_side, LiquiditySide::Taker);
        assert_eq!(f.ts, 2_000);
        let platform = all.iter().find(|f| f.venue_order_id.as_str() == "34").expect("34");
        assert!(platform.client_order_id.is_none());
        // since: the order filled at 500 is older than the window
        let recent = fill_reports(&a, 1_000);
        assert_eq!(recent.len(), 2);
        assert!(recent.iter().all(|f| f.venue_order_id.as_str() != "31"));
    }

    /// An order with no id yet has no first-fill id to name: it is not reported as a fill.
    #[test]
    fn a_filled_order_with_no_id_is_not_a_fill() {
        let a = answer(
            vec![row("", Some("m1"), "EURUSD", 1, "FILLED", "BUY", 1000.0, 1000.0, 1.1)],
            vec![],
        );
        assert!(fill_reports(&a, 0).is_empty());
    }

    // ---- the client ------------------------------------------------------------------------

    struct Source {
        reply: Result<ReconAnswer, String>,
        protections: Vec<(String, OrderRequest)>,
        asked: AtomicUsize,
        told: Mutex<Vec<String>>,
        forgotten: Mutex<Vec<String>>,
        known: Vec<String>,
    }

    impl Source {
        fn new(reply: Result<ReconAnswer, String>) -> Arc<Self> {
            Arc::new(Self {
                reply,
                protections: Vec::new(),
                asked: AtomicUsize::new(0),
                told: Mutex::new(Vec::new()),
                forgotten: Mutex::new(Vec::new()),
                known: vec!["EURUSD".to_string()],
            })
        }
    }

    impl ReconSource for Source {
        fn ask(&self) -> Result<ReconAnswer, String> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.reply.clone()
        }
        fn protections(&self) -> Vec<(String, OrderRequest)> {
            self.protections.clone()
        }
        fn know(&self, symbol: &str) {
            self.told.lock().unwrap().push(symbol.to_string());
        }
        fn forget(&self, symbol: &str) {
            self.forgotten.lock().unwrap().push(symbol.to_string());
        }
        fn known_symbols(&self) -> Vec<String> {
            self.known.clone()
        }
    }

    /// **An unreadable answer is an ERROR on all three reads, never an empty report**: an empty one is a claim the diff and
    /// the restore planner would act on.
    #[test]
    fn a_failed_query_is_an_error_on_every_read_and_never_an_empty_report() {
        let source = Source::new(Err("dukascopy: the JForex sidecar did not answer".into()));
        let client = DukascopyReconClient::new(source.clone());
        assert_eq!(
            client.fetch_order_status_reports(0),
            Err("dukascopy: the JForex sidecar did not answer".to_string())
        );
        assert!(client.fetch_fill_reports(0).is_err());
        assert!(client.fetch_position_status_reports().is_err());
        assert!(
            client.fetch_mass_status(0).is_err(),
            "the composed snapshot fails with its first read"
        );
        assert!(source.asked.load(Ordering::SeqCst) >= 4);
    }

    #[test]
    fn the_client_maps_one_answer_to_the_three_reports_and_claims_nothing_it_does_not_read() {
        let mut long = row("21", Some("m1"), "USDJPY", 1, "FILLED", "BUY", 1000.0, 1000.0, 156.9);
        long.stop_loss = 156.5;
        let a = answer(
            vec![row("11", Some("c1"), "EURUSD", 1, "OPENED", "BUYLIMIT", 0.0, 1000.0, 1.05), long],
            vec![ReconPosition { symbol: "USDJPY".into(), size: 1000.0, avg_px: 156.9 }],
        );
        let mut source = Source::new(Ok(a));
        Arc::get_mut(&mut source).unwrap().protections = vec![stop("s1", "USDJPY", -1, 156.5)];
        let client = DukascopyReconClient::new(source);
        let orders = client.fetch_order_status_reports(0).unwrap();
        assert_eq!(orders.len(), 2, "the resting limit and the node's stop-loss");
        assert_eq!(client.fetch_fill_reports(0).unwrap().len(), 1);
        let positions = client.fetch_position_status_reports().unwrap();
        assert_eq!(
            positions.iter().map(|p| (p.symbol.as_str(), p.qty)).collect::<Vec<_>>(),
            [("EURUSD", 0.0), ("USDJPY", 1000.0)]
        );
        let mass = client.fetch_mass_status(0).unwrap();
        assert_eq!((mass.orders.len(), mass.fills.len(), mass.positions.len()), (2, 1, 2));
        // what this client does not do
        assert_eq!(
            client.fetch_balance(),
            Ok(None),
            "the venue's balance is the previous end-of-day figure: not reported"
        );
        assert!(!client.lists_trigger_orders(), "no claim a conformance row has not proven");
        assert_eq!(
            client.admitted_symbols(),
            None,
            "the read spans the account but claims no admitted list"
        );
    }

    #[test]
    fn the_engines_admissions_reach_the_flat_row_list() {
        let source = Source::new(Ok(answer(vec![], vec![])));
        let client = DukascopyReconClient::new(source.clone());
        client.admit_symbol("USDJPY");
        client.forget_symbol("USDJPY");
        assert_eq!(*source.told.lock().unwrap(), ["USDJPY"]);
        assert_eq!(*source.forgotten.lock().unwrap(), ["USDJPY"]);
    }
}
