//! Closing a trade BY ITS ID on a hedging FXCM account: the pure routing, the planner, the orchestration over two
//! closures, and the tracker that turns a close's fills into one coherent order lifecycle. No session, no FFI.
//!
//! ⚠ **WHY THIS EXISTS.** On a HEDGING account an opposite market order does not close the trade it faces: it opens a
//! second, opposite trade, and the pair stays locked until somebody closes both. MEASURED on the demo, 2026-10-13/14:
//! a BUY and then a "net-flat" SELL left trades `127974465 B` and `127974466 S`. Every close the core can send
//! (`flatten`, `market-exit`, a reduce-only market order, a margin-call or budget liquidation) reached the shim as
//! a plain `TrueMarketOpen` and did exactly that, and the node could not see it: the core folds fills into one NET
//! position, so it read flat. The SDK closes a trade with `TrueMarketClose` + the trade's id
//! (`fcshim.cpp`'s `fc_close_trade`), which is what [`close_by_trade`] drives.
//!
//! The rules (the owner's, 2026-10-26), each one a row of [`route`]:
//!
//! * **Only a REDUCE-ONLY market order closes by trade.** A plain opposite order still OPENS a hedge: it may be a
//!   deliberate one. ⚠ So a core-held stop (`submit_fired` sends a plain market order, not flagged) still opens a
//!   trade when it fires on a hedging account; that is the core's to flag, and nothing here can tell it from a hedge.
//! * **A hedged resting reduce-only LIMIT or STOP is bound to a trade** ([`crate::exit`]): its plain venue form is an
//!   entry order, which OPENS a trade when it is hit, so it goes out as the trade's own take-profit (`L`) or stop-loss
//!   (`S`) instead. A STOP that is not reduce-only, and an unpriced reduce-only limit, are refused by name.
//! * **A netting account is unchanged** ([`AccountMode::Netted`]): the same orders go out as before.
//! * **A shim without the new exports gets a NAMED refusal** ([`crate::sys::FxcmSession::close_gap`]), never the
//!   silent opening order.
//! * **A close whose reply was lost holds further reduce-only closes of that instrument for a minute**
//!   ([`UNKNOWN_CLOSE_HOLD`]): it may have executed with nothing to route its fill, and a re-sent PARTIAL order would
//!   close the next trade as well.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_model::OrderType;
use vike_model::events::{Event, OrderCanceled, OrderPartiallyFilled};

use crate::exit::ExitKind;
use crate::sys::{FxcmError, PlacedOrder};

/// Whether the account NETS, from `fc_account_traits`' `maintenance_type`: the SDK's own samples treat the string `"0"`
/// as a netting account and anything else as not (`OpenPosition` against `OpenPositionNetting`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccountMode {
    /// `maintenance_type` is `"0"`: an opposite order nets against the trade it faces.
    Netted,
    /// Any other value: an opposite order opens a second trade.
    Hedged,
    /// The shim has no such export, or the read failed, or the value is missing: NOT assumed either way.
    Unknown,
}

impl AccountMode {
    /// The mode `fc_account_traits`' JSON states; `None` (no export, a failed read) is [`Self::Unknown`].
    pub(crate) fn from_traits(json: Option<&str>) -> Self {
        let value = json
            .and_then(|j| serde_json::from_str::<Value>(j).ok())
            .and_then(|v| v.get("maintenance_type").and_then(Value::as_str).map(str::to_owned));
        match value.as_deref() {
            Some("0") => Self::Netted,
            Some(other) if !other.is_empty() => Self::Hedged,
            _ => Self::Unknown,
        }
    }

    /// The words an operator reads for it.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Netted => "a netting account",
            Self::Hedged => "a hedging account",
            Self::Unknown => "an account whose mode this shim cannot read",
        }
    }
}

/// What the exec thread does with a submit.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// The order goes out as it always did (`fc_place_market`, `fc_place_entry*`).
    Place,
    /// Close the opposing trades by id ([`close_by_trade`]).
    CloseByTrade,
    /// Bind a stop or a take-profit to one open trade ([`crate::exit::bind_exit`]).
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

/// The routing table. `close_gap` and `exit_gap` are [`crate::sys::FxcmSession::close_gap`] and
/// [`crate::sys::FxcmSession::exit_gap`]: `Some(words)` when the shim cannot close a trade by its id, or cannot bind an
/// exit to one. `hold` is [`CloseHolds::reason`]: `Some(words)` while a close of this instrument is of UNKNOWN outcome; it
/// refuses a MARKET close only (a stop or a take-profit closes nothing when it is placed, so a lost close reply does not
/// hold it). Only a REDUCE-ONLY order on an account that is not known to net is ever diverted, and a stop is only ever
/// that: on a hedging account a plain stop would be an entry order, which OPENS a trade when it is hit, and a netting
/// account has no per-trade orders to bind it to.
pub(crate) fn route(
    mode: AccountMode,
    shape: Shape,
    reduce_only: bool,
    close_gap: Option<&str>,
    hold: Option<&str>,
    exit_gap: Option<&str>,
) -> Route {
    match shape {
        Shape::Stop if mode == AccountMode::Netted => Route::Refuse(
            "fxcm places a stop only as an order bound to a trade, and a netting account has no per-trade orders: a \
             stop is not available on this account"
                .to_string(),
        ),
        Shape::Stop if !reduce_only => Route::Refuse(format!(
            "fxcm refuses a stop that is not reduce-only on {}: it would rest as an entry order, which OPENS a new \
             trade when it is hit. Its stop is the protective order of a trade: send it reduce-only",
            mode.describe()
        )),
        Shape::Stop => {
            exit_gap.map_or(Route::BindExit(ExitKind::Stop), |gap| Route::Refuse(gap.to_string()))
        }
        _ if !reduce_only || mode == AccountMode::Netted => Route::Place,
        Shape::Market => match (close_gap, hold) {
            (Some(gap), _) => Route::Refuse(gap.to_string()),
            (None, Some(hold)) => Route::Refuse(hold.to_string()),
            (None, None) => Route::CloseByTrade,
        },
        Shape::Limit { priced: false } => Route::Refuse(format!(
            "fxcm refuses a reduce-only LIMIT order with no price on {}: unpriced, it would rest as an entry order at a \
             fixed distance from the quote, which OPENS a new trade when it is hit. Name the price it rests at, or close \
             with a reduce-only market order",
            mode.describe()
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
             closing a second trade; the venue's open trades are the truth (`trade recon ls` shows the difference), and a \
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
    mode: AccountMode,
    gap: Option<String>,
    exit_gap: Option<String>,
    holds: CloseHolds,
}

impl CloseDesk {
    pub(crate) fn new(mode: AccountMode, gap: Option<String>, exit_gap: Option<String>) -> Self {
        Self { mode, gap, exit_gap, holds: CloseHolds::default() }
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
}

/// The open trades facing an order of side `buy` on `instrument` (a BUY faces SHORT trades, a SELL faces LONG ones),
/// OLDEST first: the open time, then the trade id, numerically when both are numbers. The one order a close and an exit
/// bind in.
pub(crate) fn facing_oldest_first<'a>(
    trades: &'a [OpenTrade],
    instrument: &str,
    buy: bool,
) -> Vec<&'a OpenTrade> {
    let mut facing: Vec<&OpenTrade> =
        trades.iter().filter(|t| t.instrument == instrument && t.buy != buy).collect();
    facing.sort_by(|a, b| {
        a.open_time.total_cmp(&b.open_time).then_with(|| {
            match (a.trade_id.parse::<u64>(), b.trade_id.parse::<u64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.trade_id.cmp(&b.trade_id),
            }
        })
    });
    facing
}

/// One open trade, as `fc_open_trades` writes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenTrade {
    pub(crate) trade_id: String,
    /// FXCM's slashed form (`EUR/USD`).
    pub(crate) instrument: String,
    /// The trade's own side: a BUY trade is long.
    pub(crate) buy: bool,
    /// Base units.
    pub(crate) units: i64,
    /// The trade's open time as an OLE date (days): the FIFO key.
    pub(crate) open_time: f64,
}

/// Parse the `fc_open_trades` array (the five keys of [`OpenTrade`], held equal to the shim's `snprintf` literal by
/// `tests/fxcm_shim_close.rs`). FAIL CLOSED: one row the planner cannot read refuses the read, because skipping it
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
    let facing = facing_oldest_first(trades, instrument, buy);
    if facing.is_empty() || units <= 0 {
        let (order, held) = if buy { ("buy", "short") } else { ("sell", "long") };
        return Err(format!(
            "fxcm: no open {held} trade on {instrument} for a reduce-only {order} of {units} to close; a reduce-only \
             order never opens one"
        ));
    }
    let mut remaining = units;
    let mut legs = Vec::new();
    for t in facing {
        if remaining <= 0 {
            break;
        }
        let take = remaining.min(t.units);
        legs.push((t.trade_id.clone(), take));
        remaining -= take;
    }
    Ok(legs)
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
    /// Why the legs after the last sent one were not sent, if any were not.
    pub(crate) stopped: Option<String>,
    /// A LATER leg was sent and its outcome is UNKNOWN (no usable answer): it may have happened, and nothing routes its
    /// fill ([`CloseHolds`]). The first leg's unknown outcome is the `Err` of [`close_by_trade`], not this.
    pub(crate) unknown: bool,
}

/// Close `units` of the trades facing a reduce-only order, leg by leg: read the trades, [`plan_close`], send one
/// closing order per leg. `read_trades` is the shim's fresh read, `send_leg(trade_id, units)` one `fc_close_trade`, and
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
    let mut ids = sent.into_iter().map(|(p, _)| p);
    let first = ids.next().ok_or_else(|| (false, "fxcm: no closing order was sent".to_string()))?;
    Ok(Closed {
        first,
        more_ids: ids.map(|p| p.order_id).collect(),
        units: total,
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
#[derive(Debug, PartialEq)]
pub(crate) struct CloseTrack {
    left: i64,
    then_cancel: Option<String>,
}

impl CloseTrack {
    pub(crate) fn new(sent_units: i64, stopped: Option<String>) -> Self {
        Self { left: sent_units, then_cancel: stopped }
    }

    /// Relabel the events one routed envelope decoded to. `true` = the close is complete (the caller forgets it).
    pub(crate) fn relabel(&mut self, evs: Vec<Event>) -> (Vec<Event>, bool) {
        let mut done = false;
        let mut out = Vec::with_capacity(evs.len() + 1);
        for ev in evs {
            let Event::OrderFilled(w) = ev else {
                out.push(ev);
                continue;
            };
            self.left -= w.fill.last_qty.round() as i64;
            let cancel = if self.left > 0 { None } else { self.then_cancel.take() };
            done = self.left <= 0;
            if self.left > 0 || cancel.is_some() {
                let (coid, ts) = (w.client_order_id.clone(), w.ts);
                out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    client_order_id: w.client_order_id,
                    fill: w.fill,
                    ts: w.ts,
                }));
                if let Some(why) = cancel {
                    out.push(Event::OrderCanceled(OrderCanceled {
                        client_order_id: coid,
                        reason: why.into(),
                        ts,
                    }));
                }
            } else {
                out.push(Event::OrderFilled(w));
            }
        }
        (out, done)
    }
}

#[path = "close_tests.rs"]
#[cfg(test)]
mod close_tests;
