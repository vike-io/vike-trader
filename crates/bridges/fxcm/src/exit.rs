//! A STOP-LOSS and a TAKE-PROFIT on a hedging FXCM account: the pure binder that turns a reduce-only `stop` or `limit` into
//! ONE trade-bound venue order (`S` or `L`, `fcshim.cpp`'s `fc_place_exit`). No session, no FFI.
//!
//! ⚠ **WHY A TRADE-BOUND ORDER.** On a HEDGING account a plain resting opposite order is an ENTRY order: when it is hit it
//! OPENS a second trade (MEASURED 2026-10-26: a `LE` SELL beside a long was hit and opened trade `186982950 S`), so
//! this venue declared no stop at all and the core refused every stop (`TRIGGER_UNSUPPORTED`). The venue's own protective
//! order is bound to ONE trade: it rests AT the venue (so it survives a node outage and a restart), closes that trade when
//! it is hit (no hedge: the ClosedTrades row names the exit's order id) and is removed by the venue when the trade closes
//! any other way, so no stale exit can open a trade later (the hole cTrader's bound exits needed an orphan rule for).
//!
//! What the venue enforces, each MEASURED on the demo (`docs/ops/fxcm-forexconnect.md`, "Trade-bound stop-loss and
//! take-profit"), and what the planner therefore does:
//!
//! * **An exit covers a WHOLE trade** (`ORA-20134`, "Quantity should be the same as in trade"): a part of a trade cannot
//!   be protected. [`plan_exit`] binds to the OLDEST free trade facing the order whose size EQUALS the order's, caps the
//!   order to the one free trade there is when it holds less than asked (a reduce-only order never closes more than the
//!   position holds), and REFUSES by name otherwise, never sending an exit the venue would refuse or a size nobody chose.
//! * **A trade carries at most ONE stop and ONE take-profit** (`ORA-20114`): a trade that has the kind is not free for it.
//!   The two kinds of one trade are an OCO pair the venue keeps (when one fills it deletes the other), so a bracket's
//!   stop-loss and take-profit bind the SAME trade without any help from here.
//! * The exits already on the trades are read from the VENUE's Orders table at every bind ([`parse_open_exits`]), never
//!   from memory: a restart, an exit placed by hand and a partial close (which moves an exit to the remainder's NEW trade
//!   id) cannot leave the binder looking at a stale picture.

use serde_json::Value;
use vike_model::{OrderType, TriggerBy};

use crate::close::{OpenTrade, facing_oldest_first, parse_open_trades};
use crate::sys::{FxcmError, PendingLookup, PlacedOrder};

/// Which of the two trade-bound orders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitKind {
    /// The stop-loss, venue order type `S`.
    Stop,
    /// The take-profit, venue order type `L`.
    Limit,
}

impl ExitKind {
    /// ForexConnect's `OrderType` code, as `fc_place_exit` takes it.
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Stop => "S",
            Self::Limit => "L",
        }
    }

    /// The words an operator reads for it.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Limit => "take-profit limit",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "S" => Some(Self::Stop),
            "L" => Some(Self::Limit),
            _ => None,
        }
    }
}

/// What an order's own shape asks of this venue: the kind, and the venue rate the exit rests at.
///
/// A `stop` rests at its `trigger_price` and a `limit` at its `price`. Everything the venue's `S` / `L` cannot express is
/// REFUSED here, in words, instead of being substituted: a stop that also names a limit `price` (a stop-LIMIT), a stop
/// with no trigger, a trigger SOURCE other than the venue's own (`Mark`, `Index`: FXCM triggers on its own quote, the bid
/// for a sell and the ask for a buy), a limit with no price (the unpriced form rests at a pip distance from the quote,
/// which is an entry), and any rate that cannot be one. The grid check is the shim's (only a session knows the digits).
pub(crate) fn exit_shape(
    order_type: &OrderType,
    price: Option<f64>,
    trigger_price: Option<f64>,
    trigger_by: Option<TriggerBy>,
) -> Result<(ExitKind, f64), String> {
    let rate_of = |what: &str, px: Option<f64>| -> Result<f64, String> {
        match px {
            Some(p) if p.is_finite() && p > 0.0 => Ok(p),
            Some(p) => {
                Err(format!("fxcm: the {what} {p} is not a positive, finite rate; order refused"))
            }
            None => Err(format!("fxcm: the {what} is missing; order refused")),
        }
    };
    match order_type {
        OrderType::Stop => {
            if price.is_some() {
                return Err("fxcm refuses a stop that also names a limit `price` (a stop-LIMIT): its stop is a market \
                     stop that closes its trade at the venue's price"
                    .to_string());
            }
            if matches!(trigger_by, Some(TriggerBy::Mark | TriggerBy::Index)) {
                return Err("fxcm triggers a stop on its own quote (the bid for a sell, the ask for a buy): a mark or \
                     index trigger cannot be chosen, and the venue's own is not substituted for it"
                    .to_string());
            }
            Ok((ExitKind::Stop, rate_of("trigger price", trigger_price)?))
        }
        OrderType::Limit => Ok((ExitKind::Limit, rate_of("limit price", price)?)),
        other => {
            Err(format!("fxcm binds only a `stop` and a `limit` to a trade; `{other}` is neither"))
        }
    }
}

/// One exit already resting on a trade, as `fc_open_exits` writes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenExit {
    pub(crate) order_id: String,
    /// The trade it is bound to (a PARTIAL close of the trade moves it to the remainder's new id, so this is read afresh).
    pub(crate) trade_id: String,
    pub(crate) kind: ExitKind,
}

/// Parse the `fc_open_exits` array (five keys per row, held equal to the shim's `snprintf` literal by
/// `tests/fxcm_shim_exit.rs`; the side and the amount are checked and not kept: the binder needs only which trade has
/// which kind). FAIL CLOSED, like [`parse_open_trades`]: one row the binder cannot read refuses the read, because
/// skipping it could report a trade as free and the venue would then refuse the second exit.
pub(crate) fn parse_open_exits(json: &str) -> Result<Vec<OpenExit>, String> {
    let rows: Vec<Value> = serde_json::from_str(json)
        .map_err(|e| format!("the open exits the shim returned are not a JSON array: {e}"))?;
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let text = |k: &str| {
                row.get(k)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("open exit row {i} has no text `{k}`"))
            };
            let code = text("type")?;
            let kind = ExitKind::from_code(&code)
                .ok_or_else(|| format!("open exit row {i} has the type `{code}`"))?;
            match text("buysell")?.as_str() {
                "B" | "S" => {}
                other => return Err(format!("open exit row {i} has the side `{other}`")),
            }
            row.get("amount")
                .and_then(Value::as_i64)
                .filter(|a| *a > 0)
                .ok_or_else(|| format!("open exit row {i} has no positive integer `amount`"))?;
            Ok(OpenExit { order_id: text("order_id")?, trade_id: text("trade_id")?, kind })
        })
        .collect()
}

/// Which trade an exit binds to and how much of it the exit covers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExitPlan {
    pub(crate) trade_id: String,
    /// Base units the exit is placed for: the trade's WHOLE size, which is the order's own unless it was capped.
    pub(crate) units: i64,
}

/// Bind a reduce-only `kind` exit of `units` on `instrument` to ONE trade, or say why none can be bound.
///
/// `buy` is the ORDER's side (a SELL stop closes LONG trades). The trades facing it are taken oldest first
/// ([`facing_oldest_first`], the order a close uses), a trade that already has an exit of this kind is not free, and:
/// the oldest free trade of EXACTLY `units` takes it; else, when exactly one free trade is left and it holds LESS than
/// `units`, the exit is capped to it (the order is a reduce-only one: it never closes more than the position holds, and the
/// caller logs the cap); else the order is refused by name, whether nothing faces it, everything is covered, or the free
/// trades are not of the size asked (the venue refuses a part of a trade, and an exit that spans several trades is not
/// one order here: place one per trade).
pub(crate) fn plan_exit(
    trades: &[OpenTrade],
    exits: &[OpenExit],
    instrument: &str,
    buy: bool,
    kind: ExitKind,
    units: i64,
) -> Result<ExitPlan, String> {
    let (order, held) = if buy { ("buy", "short") } else { ("sell", "long") };
    let word = kind.word();
    let facing = facing_oldest_first(trades, instrument, buy);
    if facing.is_empty() || units <= 0 {
        return Err(format!(
            "fxcm: no open {held} trade on {instrument} for a reduce-only {order} {word} of {units} to bind to; a \
             reduce-only order never opens one, and this venue's {word} is bound to a trade"
        ));
    }
    let covering =
        |t: &OpenTrade| exits.iter().find(|e| e.trade_id == t.trade_id && e.kind == kind);
    let free: Vec<&OpenTrade> = facing.iter().copied().filter(|t| covering(t).is_none()).collect();
    if free.is_empty() {
        let have: Vec<String> = facing
            .iter()
            .filter_map(|t| {
                covering(t).map(|e| format!("order {} on trade {}", e.order_id, e.trade_id))
            })
            .collect();
        return Err(format!(
            "fxcm: every open {held} trade on {instrument} already has a {word} bound to it ({}); the venue allows one \
             stop and one take-profit per trade: amend or cancel that one instead of placing another",
            have.join(", ")
        ));
    }
    if let Some(t) = free.iter().find(|t| t.units == units) {
        return Ok(ExitPlan { trade_id: t.trade_id.clone(), units });
    }
    if let [only] = free[..]
        && only.units < units
    {
        return Ok(ExitPlan { trade_id: only.trade_id.clone(), units: only.units });
    }
    let sizes: Vec<String> = free.iter().map(|t| format!("{} ({})", t.trade_id, t.units)).collect();
    Err(format!(
        "fxcm: a {word} binds to a WHOLE trade and none of the free {held} trades on {instrument} holds {units} units \
         (free: {}). The venue refuses an exit for a part of a trade (ORA-20134) and one order does not span trades: \
         place the {word} for a trade's own size, one per trade",
        sizes.join(", ")
    ))
}

/// What a bind placed.
#[derive(Debug)]
pub(crate) struct Bound {
    /// The exit order: what the core's `OrderAccepted` names, and what a cancel and an amend need.
    pub(crate) placed: PlacedOrder,
    /// The trade it is bound to.
    pub(crate) trade_id: String,
    /// Base units it covers: less than asked only when the order was capped ([`plan_exit`]).
    pub(crate) units: i64,
}

/// How a bind failed, in the shape the exec thread's placement failure has: `(sent with an UNKNOWN outcome, the words,
/// what a later lookup of the lost placement needs)`.
pub(crate) type BindFailure = (bool, String, Option<PendingLookup>);

/// Read the trades and the exits already bound, [`plan_exit`], and place the one exit. `read_trades` and `read_exits` are
/// the shim's fresh bounded reads, `place(trade_id, units)` one `fc_place_exit`.
///
/// Nothing is sent when either read fails or no trade can be bound: the order is refused in words (`false`: a definite
/// refusal). A placement that was SENT and whose reply was lost is `true`, with the terms a later `confirm` looks it up by
/// (an exit RESTS, so unlike a market close it can be found again).
pub(crate) fn bind_exit(
    instrument: &str,
    buy: bool,
    kind: ExitKind,
    units: i64,
    read_trades: impl FnOnce() -> Result<String, FxcmError>,
    read_exits: impl FnOnce() -> Result<String, FxcmError>,
    place: impl FnOnce(&str, i32) -> Result<PlacedOrder, FxcmError>,
) -> Result<Bound, BindFailure> {
    let refused = |why: String| -> BindFailure { (false, why, None) };
    let trades = read_trades()
        .map_err(|e| {
            format!("fxcm could not read the account's open trades, so nothing was sent: {e}")
        })
        .and_then(|json| parse_open_trades(&json).map_err(|e| format!("fxcm: {e}")))
        .map_err(refused)?;
    let exits = read_exits()
        .map_err(|e| {
            format!("fxcm could not read the account's resting exits, so nothing was sent: {e}")
        })
        .and_then(|json| parse_open_exits(&json).map_err(|e| format!("fxcm: {e}")))
        .map_err(refused)?;
    let plan = plan_exit(&trades, &exits, instrument, buy, kind, units).map_err(refused)?;
    let Ok(amount) = i32::try_from(plan.units) else {
        return Err(refused(format!(
            "fxcm: the amount {} of trade {} is not a figure the shim takes",
            plan.units, plan.trade_id
        )));
    };
    match place(&plan.trade_id, amount) {
        Ok(placed) => Ok(Bound { placed, trade_id: plan.trade_id, units: plan.units }),
        Err(e) => {
            let (unknown, text) = (e.outcome_unknown(), e.to_string());
            Err((unknown, text, e.into_lookup()))
        }
    }
}

#[path = "exit_tests.rs"]
#[cfg(test)]
mod exit_tests;
