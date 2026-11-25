//! Closing a trade BY ITS ID on a hedging FXCM account: the pure routing, the planner, the orchestration over two
//! closures, and the tracker that turns a close's fills into one coherent order lifecycle. No session, no FFI.
//!
//! ⚠ **WHY THIS EXISTS.** On a HEDGING account an opposite market order does not close the trade it faces: it opens a
//! second, opposite trade, and the pair stays locked until somebody closes both. MEASURED on the demo, 2026-10-13/14:
//! a BUY and then a "net-flat" SELL left trades `127974465 B` and `127974466 S`. Every close the core can send
//! (`flatten`, `market-exit`, a reduce-only market order, a margin-call or budget liquidation) reached the shim as
//! a plain `TrueMarketOpen` and did exactly that, and the node could not see it: the core folds fills into one NET
//! position, so it read flat. The SDK closes a trade with `TrueMarketClose` + the trade's id
//! (`fcshim.cpp`'s `forex_connect_close_trade`), which is what [`close_by_trade`] drives.
//!
//! The rules (the owner's, 2026-10-26), each one a row of [`route`]:
//!
//! * **Only a REDUCE-ONLY market order closes by trade.** A plain opposite order still OPENS a hedge: it may be a
//!   deliberate one (a manual one is, the owner's rule). A fired plain stop or trailing stop reaches here as the core
//!   splits it (`vike-core`'s `submit_fired`, since 2026-11-08): the part its position covers FLAGGED, which closes the
//!   trade by id here, and only the part past it plain, which opens the opposite trade. Nothing here tells a stop from
//!   a hedge; the flag does.
//! * **A hedged resting reduce-only LIMIT or STOP is bound to a trade** ([`crate::execution::exit`]): its plain venue form is an
//!   entry order, which OPENS a trade when it is hit, so it goes out as the trade's own take-profit (`L`) or stop-loss
//!   (`S`) instead. A STOP that is not reduce-only, and an unpriced reduce-only limit, are refused by name.
//! * **A reduce-only LIMIT the market already REACHES closes by trade id NOW, never worse than its price**
//!   ([`CloseDesk::limit_now`], the SDK's Close Limit order through `fcshim.cpp`'s `forex_connect_close_trade_limit`; the
//!   owner's word of 2026-11-15). That is a fired stop-limit or trailing stop-limit: the core releases its limit
//!   reduce-only, and as the trade's `L` it would sit on the wrong side of the market (`ORA-20115`, measured for an exit). A
//!   limit the market does NOT reach (the price gapped through it) stays the trade's `L`, which rests at the venue and
//!   fills when the price comes back; never reached, the trade stays open, which is the stop-limit's own meaning. A Close
//!   Limit that MISSES (the price moved past the limit on its way) is a limit the market did not reach, so it rests as that
//!   `L` too, whether the venue refuses it or accepts and then deletes it ([`CloseTracker`]'s miss, [`take_missed`]).
//! * **A netting account is unchanged** ([`PositionMode::OneWay`]): the same orders go out as before.
//! * **A shim without the new exports gets a NAMED refusal** ([`crate::transport::sys::FxcmSession::close_gap`]), never the
//!   silent opening order.
//! * **A close whose reply was lost holds further reduce-only closes of that instrument for a minute**
//!   ([`UNKNOWN_CLOSE_HOLD`]): it may have executed with nothing to route its fill, and a re-sent PARTIAL order would
//!   close the next trade as well.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::oldest_first::take_oldest_first;
use vike_model::events::{Event, OrderCanceled, OrderPartiallyFilled};
use vike_model::{OrderRequest, OrderType, PositionMode};

use crate::transport::sys::{FxcmError, PlacedOrder};
use vike_bridge_core::exit::ExitKind;

/// Whether the account NETS, from `forex_connect_account_traits`' `maintenance_type`: the SDK's own samples treat the string `"0"`
/// as a netting account and anything else as not (`OpenPosition` against `OpenPositionNetting`). `"0"` is
/// [`PositionMode::OneWay`] (an opposite order nets against the trade it faces), any other value
/// [`PositionMode::Hedge`] (an opposite order opens a second trade), and `None` (no export, a failed read) or a missing
/// or empty value [`PositionMode::Unknown`]: NOT assumed either way.
pub(crate) fn position_mode(json: Option<&str>) -> PositionMode {
    let value = json
        .and_then(|j| serde_json::from_str::<Value>(j).ok())
        .and_then(|v| v.get("maintenance_type").and_then(Value::as_str).map(str::to_owned));
    match value.as_deref() {
        Some("0") => PositionMode::OneWay,
        Some(other) if !other.is_empty() => PositionMode::Hedge,
        _ => PositionMode::Unknown,
    }
}

/// The words an operator reads for the account's mode.
pub(crate) fn position_mode_words(mode: PositionMode) -> &'static str {
    match mode {
        PositionMode::OneWay => "a netting account",
        PositionMode::Hedge => "a hedging account",
        PositionMode::Unknown => "an account whose mode this shim cannot read",
    }
}

/// What the exec thread does with a submit.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// The order goes out as it always did (`forex_connect_place_market`, `forex_connect_place_entry*`).
    Place,
    /// Close the opposing trades by id ([`close_by_trade`]).
    CloseByTrade,
    /// Bind a stop or a take-profit to one open trade ([`crate::execution::exit::bind_exit`]).
    BindExit(ExitKind),
    /// Nothing is sent; the order is rejected with these words.
    Refuse(String),
}

/// The shape of a submit, as far as routing is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    /// An immediately executing order.
    Market,
    /// A resting limit; `priced` is whether it names the price it rests at (the unpriced form rests at a fixed distance
    /// from the quote, which can only be an entry).
    Limit { priced: bool },
    /// A stop. On this venue it exists only as a reduce-only exit bound to a trade.
    Stop,
}

impl Shape {
    /// The routing shape of an order type, or `None` for a kind the adapter does not place at all (the preflight has
    /// refused those before this is asked).
    pub(crate) fn of(order_type: &OrderType, price: Option<f64>) -> Option<Self> {
        match order_type {
            OrderType::Market => Some(Self::Market),
            OrderType::Limit => Some(Self::Limit { priced: price.is_some() }),
            OrderType::Stop => Some(Self::Stop),
            _ => None,
        }
    }
}

/// The routing table. `close_gap` and `exit_gap` are [`crate::transport::sys::FxcmSession::close_gap`] and
/// [`crate::transport::sys::FxcmSession::exit_gap`]: `Some(words)` when the shim cannot close a trade by its id, or cannot bind an
/// exit to one. `hold` is [`CloseHolds::reason`]: `Some(words)` while a close of this instrument is of UNKNOWN outcome; it
/// refuses a MARKET close only (a stop or a take-profit closes nothing when it is placed, so a lost close reply does not
/// hold it). Only a REDUCE-ONLY order on an account that is not known to net is ever diverted, and a stop is only ever
/// that: on a hedging account a plain stop would be an entry order, which OPENS a trade when it is hit, and a netting
/// account has no per-trade orders to bind it to.
pub(crate) fn route(
    mode: PositionMode,
    shape: Shape,
    reduce_only: bool,
    close_gap: Option<&str>,
    hold: Option<&str>,
    exit_gap: Option<&str>,
) -> Route {
    match shape {
        Shape::Stop if mode == PositionMode::OneWay => Route::Refuse(
            "fxcm places a stop only as an order bound to a trade, and a netting account has no per-trade orders: a \
             stop is not available on this account"
                .to_string(),
        ),
        Shape::Stop if !reduce_only => Route::Refuse(format!(
            "fxcm refuses a stop that is not reduce-only on {}: it would rest as an entry order, which OPENS a new \
             trade when it is hit. Its stop is the protective order of a trade: send it reduce-only",
            position_mode_words(mode)
        )),
        Shape::Stop => {
            exit_gap.map_or(Route::BindExit(ExitKind::Stop), |gap| Route::Refuse(gap.to_string()))
        }
        _ if !reduce_only || mode == PositionMode::OneWay => Route::Place,
        Shape::Market => match (close_gap, hold) {
            (Some(gap), _) => Route::Refuse(gap.to_string()),
            (None, Some(hold)) => Route::Refuse(hold.to_string()),
            (None, None) => Route::CloseByTrade,
        },
        Shape::Limit { priced: false } => Route::Refuse(format!(
            "fxcm refuses a reduce-only LIMIT order with no price on {}: unpriced, it would rest as an entry order at a \
             fixed distance from the quote, which OPENS a new trade when it is hit. Name the price it rests at, or close \
             with a reduce-only market order",
            position_mode_words(mode)
        )),
        Shape::Limit { priced: true } => {
            exit_gap.map_or(Route::BindExit(ExitKind::Limit), |gap| Route::Refuse(gap.to_string()))
        }
    }
}

/// How long reduce-only closes of an instrument are refused after a close of it ended with an UNKNOWN outcome.
///
/// **Why a hold exists.** A close whose reply was lost (`-11`/`-12`) may have executed: only the REQUEST id came back,
/// so the closing order has no route, its ClosedTrades fill is dropped and the core still shows the position. Nothing
/// retries it. A reduce-only order re-sent meanwhile would be planned against the venue's trades AFTER that close, so a
/// PARTIAL one would close the NEXT trade as well. The hold is the pause that keeps a second close from compounding the
/// first; the venue's trades are the truth, and reconcile reports the difference.
///
/// **Why 60 s.** The reply wait is 10 s (`ORDER_WAIT_MS`) and a market close that happens settles in under a second
/// (measured 0.2-0.4 s, a partial one 4 s), so a minute is longer than any close takes to show in the Trades table and
/// short enough that the 30 s margin-call and drawdown clocks still get a second try inside the next minute. Nothing
/// ends the hold early: a time bound is the one rule that needs no read of the venue.
///
/// **What it does not do.** It does not tell the core about the missing fill (reconcile does, held under `quarantine`),
/// and a flatten re-sent AFTER the hold converges: the planner reads the fresh trades, closes what is left or, with
/// nothing facing the order, refuses by name and opens nothing.
pub(crate) const UNKNOWN_CLOSE_HOLD: Duration = Duration::from_secs(60);

/// The instruments with a close of UNKNOWN outcome, and when it was recorded. At most one row per instrument.
#[derive(Debug, Default)]
pub(crate) struct CloseHolds {
    since: HashMap<String, Instant>,
}

impl CloseHolds {
    /// A close of `instrument` ended with an unknown outcome at `now`.
    pub(crate) fn record(&mut self, instrument: &str, now: Instant) {
        self.since.insert(instrument.to_string(), now);
    }

    /// The words that refuse a reduce-only close of `instrument` at `now`, or `None` when no hold stands (an expired
    /// one is dropped here).
    pub(crate) fn reason(&mut self, instrument: &str, now: Instant) -> Option<String> {
        let at = *self.since.get(instrument)?;
        let age = now.saturating_duration_since(at);
        let Some(left) = UNKNOWN_CLOSE_HOLD.checked_sub(age).filter(|d| !d.is_zero()) else {
            self.since.remove(instrument);
            return None;
        };
        Some(format!(
            "fxcm: a close of {instrument} was sent {} s ago and the venue gave no usable answer, so it MAY have happened \
             (only its request id came back, so its fill cannot be routed and the node may still show the position). A \
             further reduce-only close of {instrument} is refused for {} more s (the hold is {} s) rather than risk \
             closing a second trade; the venue's open trades are the truth (`trade reconcile ls` shows the difference), and a \
             close sent after the hold is planned against them",
            age.as_secs(),
            left.as_secs() + u64::from(left.subsec_nanos() > 0),
            UNKNOWN_CLOSE_HOLD.as_secs()
        ))
    }
}

/// Did this close leave an UNKNOWN outcome: the first leg's lost reply, or a later leg's? Either way a closing order
/// may exist that nothing will route.
pub(crate) fn left_unknown(result: &Result<Closed, (bool, String)>) -> bool {
    match result {
        Ok(done) => done.unknown,
        Err((unknown, _)) => *unknown,
    }
}

/// What the exec thread asks about a submit and tells about a close, in one place: the account's mode, the shim's gaps
/// and the holds ([`route`] is the table, [`CloseHolds`] the memory). The exec thread owns one; a test drives the same
/// object with a scripted clock, so the wiring of `close_gap`, of `exit_gap` and of the hold into the routing is held by
/// a test instead of by the live smoke alone.
#[derive(Debug)]
pub(crate) struct CloseDesk {
    mode: PositionMode,
    gap: Option<String>,
    exit_gap: Option<String>,
    holds: CloseHolds,
}

impl CloseDesk {
    pub(crate) fn new(mode: PositionMode, gap: Option<String>, exit_gap: Option<String>) -> Self {
        Self { mode, gap, exit_gap, holds: CloseHolds::default() }
    }

    /// Whether this account is known to net, so a reduce-only market order goes out as a plain opposite order.
    pub(crate) fn nets(&self) -> bool {
        self.mode == PositionMode::OneWay
    }

    /// [`route`] for a submit of `shape` on `instrument` at `now`, with this desk's gaps and holds.
    pub(crate) fn route(
        &mut self,
        instrument: &str,
        shape: Shape,
        reduce_only: bool,
        now: Instant,
    ) -> Route {
        let hold = self.holds.reason(instrument, now);
        route(
            self.mode,
            shape,
            reduce_only,
            self.gap.as_deref(),
            hold.as_deref(),
            self.exit_gap.as_deref(),
        )
    }

    /// A close of `instrument` came to `result`: start the hold when its outcome is unknown. `true` = a hold started.
    pub(crate) fn settle(
        &mut self,
        instrument: &str,
        result: &Result<Closed, (bool, String)>,
        now: Instant,
    ) -> bool {
        let unknown = left_unknown(result);
        if unknown {
            self.holds.record(instrument, now);
        }
        unknown
    }

    /// The route of `req` on `instrument`, the ONE place the exec thread asks: [`Self::route`] for its shape, then
    /// [`Self::limit_now`] for a reduce-only LIMIT bound for the trade's `L`. `quote` (the one instrument's row,
    /// `forex_connect_quote`) and `limit_gap` are asked for such a limit only.
    pub(crate) fn route_order(
        &mut self,
        instrument: &str,
        req: &OrderRequest,
        quote: impl FnOnce() -> Option<(f64, f64)>,
        limit_gap: impl FnOnce() -> Option<String>,
        now: Instant,
    ) -> Route {
        let Some(shape) = Shape::of(&req.order_type, req.price) else {
            return Route::Refuse(format!("fxcm places no `{}` order", req.order_type));
        };
        match (self.route(instrument, shape, req.reduce_only, now), req.price) {
            (Route::BindExit(ExitKind::Limit), Some(limit)) => {
                let quote = quote();
                // No quote: only a FIRED close goes Close Limit first (the review of batch 1d, finding 4). An ordinary
                // take-profit has no reason to try a close now, and a lost reply would start the close hold, which then
                // refuses a protective stop's close.
                if quote.is_none() && !fired_by_the_engine(&req.order_client_id) {
                    return Route::BindExit(ExitKind::Limit);
                }
                self.limit_now(instrument, req.side >= 0, limit, quote, limit_gap().as_deref(), now)
                    .unwrap_or(Route::BindExit(ExitKind::Limit))
            }
            (routed, _) => routed,
        }
    }

    /// **A reduce-only priced LIMIT the market already reaches closes NOW, by trade id, never worse than its price.** Asked
    /// only of a [`Route::BindExit`] limit (a hedging or unreadable account). That is how a fired stop-limit or trailing
    /// stop-limit arrives (the core releases its limit reduce-only), and as the trade's own `L` it would sit on the wrong
    /// side of the market (`ORA-20115`). `quote` is the instrument's `(bid, ask)`; `limit_gap` is
    /// [`crate::transport::sys::FxcmSession::close_limit_gap`]. `Some(CloseByTrade)` when [`reaches`] (the caller closes
    /// under the order's price); `Some(Refuse)` when it reaches but the shim cannot close under a price, or a close of
    /// the instrument is of unknown outcome; `None` when the price is not reached, and the order stays the trade's `L`,
    /// which rests at the venue until the price comes back.
    ///
    /// ⚠ **No quote is not "not reached"** (the live demo of 2026-11-16: the quote read found no row, the fired stop-limit
    /// went out as the trade's `L` and the venue refused it ORA-20115, four times). With no quote the Close Limit goes
    /// FIRST: bounded by the limit it can never fill worse, and a miss rests as the `L` all the same (the exec thread's
    /// `rest_refused`, the drain's `rest_missed`). Only a shim that cannot close under a price, or a close hold, keeps the
    /// `L` then, as before. [`Self::route_order`] asks this with no quote only for a fired close ([`fired_by_the_engine`]).
    pub(crate) fn limit_now(
        &mut self,
        instrument: &str,
        buy: bool,
        limit: f64,
        quote: Option<(f64, f64)>,
        limit_gap: Option<&str>,
        now: Instant,
    ) -> Option<Route> {
        let hold = self.holds.reason(instrument, now);
        let Some((bid, ask)) = quote else {
            return (limit_gap.is_none() && hold.is_none()).then_some(Route::CloseByTrade);
        };
        if !reaches(buy, limit, bid, ask) {
            return None;
        }
        Some(match (limit_gap, hold) {
            (Some(gap), _) => Route::Refuse(gap.to_string()),
            (None, Some(hold)) => Route::Refuse(hold),
            (None, None) => Route::CloseByTrade,
        })
    }
}

/// The price every leg of a close by trade id is bounded by: a LIMIT's own (it reaches [`Route::CloseByTrade`] only through
/// [`CloseDesk::limit_now`]); none for a market close.
pub(crate) fn close_bound(req: &OrderRequest) -> Option<f64> {
    if req.order_type == OrderType::Market { None } else { req.price }
}

/// Whether the market reaches a limit now: a SELL's when the bid is at or above it, a BUY's when the ask is at or below
/// it. Exactly at the limit is reached: the close may fill there, never worse.
pub(crate) fn reaches(buy: bool, limit: f64, bid: f64, ask: f64) -> bool {
    if buy { ask <= limit } else { bid >= limit }
}

/// Whether `order_client_id` names an order the ENGINE made on its own (decision 0270's source letter `E`, outside any
/// group): an operator's fired core-held stop's close (stop-limit, trailing stop-limit, limit-if-touched), the order a
/// fired stop-limit reaches this crate as. An ordinary take-profit is the operator's (`G`, `C`, `T`, `M`), a strategy's
/// (`S`) or a group's leg (it carries a role), and an id before v2 says nothing: none of them is.
///
/// ⚠ A STRATEGY's fired stop-limit carries the strategy's own `S` (decision 0270) and reads like its take-profit: with
/// no quote it is the trade's `L`, as before. Telling them apart needs a mark on the fired order, which is the core's.
pub(crate) fn fired_by_the_engine(order_client_id: &str) -> bool {
    use vike_model::orders::client_id::{
        OrderClientIdReading, OrderClientIdSource, parse_order_client_id,
    };
    matches!(
        parse_order_client_id(order_client_id),
        Some(OrderClientIdReading::V2(p)) if p.src == OrderClientIdSource::Engine && p.role.is_none()
    )
}

/// A two-sided quote a limit can be judged by: both sides positive and finite, else none.
pub(crate) fn usable_quote(bid: f64, ask: f64) -> Option<(f64, f64)> {
    (bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > 0.0).then_some((bid, ask))
}

/// The open trades facing an order of side `buy` on `instrument` (a BUY faces SHORT trades, a SELL faces LONG ones),
/// OLDEST first: the open time, then the trade id, numerically when both are numbers. The one order a close and an exit
/// bind in.
pub(crate) fn facing_oldest_first<'a>(
    trades: &'a [OpenTrade],
    instrument: &str,
    buy: bool,
) -> Vec<&'a OpenTrade> {
    // Every facing trade, whole (a trade's units are positive: `parse_open_trades` refuses any other row).
    take_facing(trades, instrument, buy, i64::MAX).into_iter().map(|(t, _)| t).collect()
}

/// Whether the trade's instrument (the VENUE's spelling the shim writes: `EUR/USD`, `USOil`, `Copper`) is `instrument` (the
/// order's kept row's wire name, the same spelling: decision 0290), exactly (audit flag FXCM-1, [`take_facing`]).
fn on_instrument(trade: &OpenTrade, instrument: &str) -> bool {
    trade.instrument == instrument
}

/// The shared oldest-first take (`vike_bridge_core::oldest_first`) over FXCM's facts: a trade faces the order on its
/// instrument from the other side, the OLE open time orders them, then the trade id, numerically when both are numbers.
///
/// "Its instrument" is ONE ROW (audit flag FXCM-1): `instrument` is the order's kept row's wire name
/// (`crate::instrument::names::wire_name`: `USOil` for `USOIL.CFD`, the row's case kept) and a trade's is the venue's
/// own name of its offer (the shim's `instrumentForOfferId`, the Offers row the kept row was listed from), so one row
/// is one spelling on both sides and the match is exact. It was the engine's upper-cased respelling (`USOIL`), which
/// found no facing trade of a mixed-case CFD.
fn take_facing<'a>(
    trades: &'a [OpenTrade],
    instrument: &str,
    buy: bool,
    units: i64,
) -> Vec<(&'a OpenTrade, i64)> {
    take_oldest_first(
        trades,
        |t| on_instrument(t, instrument) && t.buy != buy,
        |a, b| {
            a.open_time.total_cmp(&b.open_time).then_with(|| {
                match (a.trade_id.parse::<u64>(), b.trade_id.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    _ => a.trade_id.cmp(&b.trade_id),
                }
            })
        },
        |t| t.units,
        units,
    )
}

/// One open trade, as `forex_connect_open_trades` writes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenTrade {
    pub(crate) trade_id: String,
    /// The venue's own name of the trade's instrument (`EUR/USD`, `USOil`): its kept row's wire name.
    pub(crate) instrument: String,
    /// The trade's own side: a BUY trade is long.
    pub(crate) buy: bool,
    /// Base units.
    pub(crate) units: i64,
    /// The trade's open time as an OLE date (days): the FIFO key.
    pub(crate) open_time: f64,
}

/// Parse the `forex_connect_open_trades` array (the five keys of [`OpenTrade`], held equal to the shim's `snprintf` literal by
/// `tests/fxcm_shim/fxcm_shim_close.rs`). FAIL CLOSED: one row the planner cannot read refuses the read, because skipping it
/// could close a younger trade while the oldest stays, and a close must not guess.
pub(crate) fn parse_open_trades(json: &str) -> Result<Vec<OpenTrade>, String> {
    let rows: Vec<Value> = serde_json::from_str(json)
        .map_err(|e| format!("the open trades the shim returned are not a JSON array: {e}"))?;
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let text = |k: &str| {
                row.get(k)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("open trade row {i} has no text `{k}`"))
            };
            let buy = match text("buysell")?.as_str() {
                "B" => true,
                "S" => false,
                other => return Err(format!("open trade row {i} has the side `{other}`")),
            };
            let units =
                row.get("amount").and_then(Value::as_i64).filter(|a| *a > 0).ok_or_else(|| {
                    format!("open trade row {i} has no positive integer `amount`")
                })?;
            let open_time = row
                .get("open_time")
                .and_then(Value::as_f64)
                .ok_or_else(|| format!("open trade row {i} has no number `open_time`"))?;
            Ok(OpenTrade {
                trade_id: text("trade_id")?,
                instrument: text("instrument")?,
                buy,
                units,
                open_time,
            })
        })
        .collect()
}

/// The net base units `trades` hold in `instrument` (a wire name, matched as `take_facing` matches it), long positive.
pub(crate) fn net_units(trades: &[OpenTrade], instrument: &str) -> f64 {
    trades
        .iter()
        .filter(|t| on_instrument(t, instrument))
        .map(|t| if t.buy { t.units as f64 } else { -(t.units as f64) })
        .sum()
}

/// Which trades, and how much of each, a reduce-only market order of `units` on `instrument` closes.
///
/// Modelled on cTrader's `plan_reduce` and reading the same thing, the ACTUAL open trades, not the account's mode.
/// `buy` is the ORDER's side: a BUY closes SHORT trades, a SELL closes LONG ones. The trades facing the order are taken
/// oldest first (the open time, then the trade id, numerically when both are numbers), each for `min(remaining, its
/// units)`, and the total is capped at what they hold: a reduce-only order never opens, so the excess is dropped.
///
/// # Errors
/// No trade faces the order (a flat book, or a trade of the wrong side only): the words say so, and that a reduce-only
/// order never opens. The core's local book can be wrong about the venue; this refusal is where that shows.
pub(crate) fn plan_close(
    trades: &[OpenTrade],
    instrument: &str,
    buy: bool,
    units: i64,
) -> Result<Vec<(String, i64)>, String> {
    // Empty when no trade faces the order or `units` is not positive.
    let legs = take_facing(trades, instrument, buy, units);
    if legs.is_empty() {
        let (order, held) = if buy { ("buy", "short") } else { ("sell", "long") };
        return Err(format!(
            "fxcm: no open {held} trade on {instrument} for a reduce-only {order} of {units} to close; a reduce-only \
             order never opens one"
        ));
    }
    Ok(legs.into_iter().map(|(t, take)| (t.trade_id.clone(), take)).collect())
}

/// What a close sent.
#[derive(Debug)]
pub(crate) struct Closed {
    /// The first leg's closing order: the order the core's `OrderAccepted` names.
    pub(crate) first: PlacedOrder,
    /// The closing order ids of every further leg that was sent.
    pub(crate) more_ids: Vec<String>,
    /// The base units of every leg that was SENT: what the fills must add up to.
    pub(crate) units: i64,
    /// Every leg that was sent, in order: its closing order id and its base units ([`CloseTracker::with_legs`]).
    pub(crate) legs: Vec<(String, i64)>,
    /// Why the legs after the last sent one were not sent, if any were not.
    pub(crate) stopped: Option<String>,
    /// A LATER leg was sent and its outcome is UNKNOWN (no usable answer): it may have happened, and nothing routes its
    /// fill ([`CloseHolds`]). The first leg's unknown outcome is the `Err` of [`close_by_trade`], not this.
    pub(crate) unknown: bool,
}

/// Close `units` of the trades facing a reduce-only order, leg by leg: read the trades, [`plan_close`], send one
/// closing order per leg. `read_trades` is the shim's fresh read, `send_leg(trade_id, units)` one `forex_connect_close_trade`, and
/// `may_start_leg` says whether there is still time for another one (each leg can take the shim's whole bound, and a
/// close must not outrun the order's ack timeout).
///
/// The first leg's failure is the order's: `Err((outcome_unknown, words))`, the shape the exec thread's placement
/// failure has. A LATER leg's failure ends the sending and rides on [`Closed::stopped`]; the legs already sent stand,
/// and an UNKNOWN outcome on any leg stops it too (a second order on top of a close that may have happened is how a
/// position is closed twice).
pub(crate) fn close_by_trade(
    instrument: &str,
    buy: bool,
    units: i64,
    read_trades: impl FnOnce() -> Result<String, FxcmError>,
    mut send_leg: impl FnMut(&str, i32) -> Result<PlacedOrder, FxcmError>,
    mut may_start_leg: impl FnMut() -> bool,
) -> Result<Closed, (bool, String)> {
    let read = read_trades().map_err(|e| {
        (false, format!("fxcm could not read the account's open trades, so nothing was sent: {e}"))
    })?;
    let trades = parse_open_trades(&read).map_err(|e| (false, format!("fxcm: {e}")))?;
    let legs = plan_close(&trades, instrument, buy, units).map_err(|e| (false, e))?;
    let mut sent: Vec<(PlacedOrder, i64)> = Vec::new();
    let mut stopped = None;
    let mut unknown = false;
    for (i, (trade_id, leg_units)) in legs.iter().enumerate() {
        if i > 0 && !may_start_leg() {
            stopped = Some(format!(
                "the time bound of one order ran out before trade {trade_id} and {} more were closed",
                legs.len() - i - 1
            ));
            break;
        }
        let Ok(amount) = i32::try_from(*leg_units) else {
            let why = format!(
                "the amount {leg_units} of trade {trade_id} is not a figure the shim takes"
            );
            if i == 0 {
                return Err((false, why));
            }
            stopped = Some(why);
            break;
        };
        match send_leg(trade_id, amount) {
            Ok(placed) => sent.push((placed, *leg_units)),
            Err(e) if i == 0 => return Err((e.outcome_unknown(), e.to_string())),
            Err(e) => {
                unknown = e.outcome_unknown();
                stopped = Some(if unknown {
                    format!(
                        "trade {trade_id}: the close was sent and its outcome is UNKNOWN ({e}); it may have happened"
                    )
                } else {
                    format!("trade {trade_id} was not closed: {e}")
                });
                break;
            }
        }
    }
    let total = sent.iter().map(|(_, u)| *u).sum();
    let legs = sent.iter().map(|(p, u)| (p.order_id.clone(), *u)).collect();
    let mut ids = sent.into_iter().map(|(p, _)| p);
    let first = ids.next().ok_or_else(|| (false, "fxcm: no closing order was sent".to_string()))?;
    Ok(Closed {
        first,
        more_ids: ids.map(|p| p.order_id).collect(),
        units: total,
        legs,
        stopped,
        unknown,
    })
}

/// A close in progress: how many base units its fills still owe, and the words for the legs that were not sent.
///
/// One core order is several venue orders here (one per trade), and the order FSM takes `OrderFilled` only once: a
/// second one from `Filled` is an invalid transition. So every fill but the last is relabelled `OrderPartiallyFilled`.
/// If legs were NOT sent, the last fill is relabelled too and an `OrderCanceled` naming them follows it, which is the
/// legal terminal of a partly filled order (an `OrderRejected` from `PartiallyFilled` is not).
///
/// ⚠ **A leg the venue removes WITHOUT filling it is a MISS, never an `OrderRejected`** (the review of 2026-11-16, finding 1):
/// the core already holds the order `Accepted`, and the order FSM refuses a rejection from there, so the order stayed
/// `Accepted` for ever and the trade was left with no word. A Close Limit (`CL`) misses whenever the price moves past its
/// limit between the quote and its arrival; a market close could too. The miss counts the leg's units as not owed and
/// ends the order `OrderCanceled` with the words (legal from `Accepted` and from `PartiallyFilled`), after the other legs.
/// A Close Limit ALL of whose legs missed with nothing filled is not cancelled here: its limit rests as the trade's own
/// `L` instead, as a limit the market does not reach does ([`Self::resting_at`], [`take_missed`]; the exec thread binds it).
#[derive(Debug, PartialEq)]
pub(crate) struct CloseTracker {
    left: i64,
    then_cancel: Option<String>,
    /// The units each sent leg still owes, by its closing order id ([`Self::with_legs`]).
    legs: HashMap<String, i64>,
    /// The units sent, and the units filled so far.
    sent: i64,
    filled: i64,
    /// Where a missed Close Limit rests instead ([`Self::resting_at`]); `None` for a market close.
    rest: Option<LimitRest>,
    /// Every leg of a Close Limit missed and nothing filled: the words, waiting for the exec thread ([`take_missed`]).
    missed: Option<(String, i64)>,
}

/// What a Close Limit's limit rests as when the close misses: the trade's own `L` at `rate`, for an order of side `buy`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LimitRest {
    pub(crate) instrument: String,
    pub(crate) buy: bool,
    pub(crate) rate: f64,
}

/// A Close Limit the venue removed unfilled after accepting it: why, where its limit rests instead, the units, and the
/// time of the venue's removal.
#[derive(Debug, PartialEq)]
pub(crate) struct Missed {
    pub(crate) why: String,
    pub(crate) rest: LimitRest,
    pub(crate) units: i64,
    pub(crate) at: i64,
}

/// Take every Close Limit that missed whole out of `closes` ([`CloseTracker`]'s miss), for the exec thread to rest.
pub(crate) fn take_missed(closes: &mut HashMap<String, CloseTracker>) -> Vec<(String, Missed)> {
    let ids: Vec<String> =
        closes.iter().filter(|(_, t)| t.missed.is_some()).map(|(id, _)| id.clone()).collect();
    ids.into_iter()
        .filter_map(|id| {
            let track = closes.remove(&id)?;
            let (why, at) = track.missed?;
            Some((id, Missed { why, rest: track.rest?, units: track.sent, at }))
        })
        .collect()
}

impl CloseTracker {
    pub(crate) fn new(sent_units: i64, stopped: Option<String>) -> Self {
        Self {
            left: sent_units,
            then_cancel: stopped,
            legs: HashMap::new(),
            sent: sent_units,
            filled: 0,
            rest: None,
            missed: None,
        }
    }

    /// The legs that were sent ([`Closed::legs`]), so a leg the venue removes unfilled is read as a miss.
    pub(crate) fn with_legs(mut self, legs: Vec<(String, i64)>) -> Self {
        self.legs = legs.into_iter().collect();
        self
    }

    /// A Close Limit: where its limit rests when every leg misses.
    pub(crate) fn resting_at(mut self, rest: LimitRest) -> Self {
        self.rest = Some(rest);
        self
    }

    /// Relabel the events one routed envelope of the venue order `oid` decoded to. `true` = the close is complete (the
    /// caller forgets it).
    pub(crate) fn relabel(&mut self, oid: &str, evs: Vec<Event>) -> (Vec<Event>, bool) {
        let mut done = false;
        let mut out = Vec::with_capacity(evs.len() + 1);
        for event in evs {
            let Event::OrderFilled(w) = event else {
                match self.miss(oid, &event) {
                    Some((ended, cancel)) => {
                        done |= ended;
                        out.extend(cancel);
                    }
                    None => out.push(event),
                }
                continue;
            };
            let qty = w.fill.last_qty.round() as i64;
            if let Some(owed) = self.legs.get_mut(oid) {
                *owed -= qty;
                if *owed <= 0 {
                    self.legs.remove(oid);
                }
            }
            self.filled += qty;
            self.left -= qty;
            let cancel = if self.left > 0 { None } else { self.then_cancel.take() };
            done = self.left <= 0;
            if self.left > 0 || cancel.is_some() {
                let (order_client_id, ts) = (w.order_client_id.clone(), w.timestamp);
                out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    order_client_id: w.order_client_id,
                    fill: w.fill,
                    timestamp: w.timestamp,
                }));
                if let Some(why) = cancel {
                    out.push(Event::OrderCanceled(OrderCanceled {
                        order_client_id,
                        reason: why.into(),
                        timestamp: ts,
                        post_only_would_cross: false,
                        refusal_id: None,
                    }));
                }
            } else {
                out.push(Event::OrderFilled(w));
            }
        }
        (out, done)
    }

    /// `event` is the venue removing the sent leg `oid` (rejected or cancelled) with what it still owed unfilled: a MISS.
    /// `None` = not a removal of a leg this close sent (the event passes as it is). Else `(ended, events)`: the order
    /// goes on while other legs still owe; at its end it is cancelled in words, or, a Close Limit that filled nothing,
    /// left for the exec thread to rest ([`take_missed`]).
    fn miss(&mut self, oid: &str, event: &Event) -> Option<(bool, Option<Event>)> {
        // The decode ends a removed order `OrderCanceled` whether the venue said `C` or `R`, the venue's words in its
        // reason for an `R` (`crate::execution::event_mapper::map_fxcm_event`).
        let Event::OrderCanceled(c) = event else { return None };
        let (order_client_id, at) = (&c.order_client_id, c.timestamp);
        let owed = self.legs.remove(oid)?;
        let said = if c.reason.is_empty() { String::new() } else { format!(" ({})", c.reason) };
        let why = format!(
            "fxcm: the venue removed the close of {owed} units (order {oid}) without filling it{said}: nothing of it \
             closed"
        );
        self.left -= owed;
        let why = match self.then_cancel.take() {
            Some(earlier) => format!("{earlier}; {why}"),
            None => why,
        };
        if self.left > 0 {
            self.then_cancel = Some(why);
            return Some((false, None));
        }
        if self.filled == 0 && self.rest.is_some() {
            self.missed = Some((why, at));
            return Some((false, None));
        }
        let cancel = Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.clone(),
            reason: why.into(),
            timestamp: at,
            post_only_would_cross: false,
            refusal_id: None,
        });
        Some((true, Some(cancel)))
    }
}

#[path = "close_tests.rs"]
#[cfg(test)]
mod close_tests;
