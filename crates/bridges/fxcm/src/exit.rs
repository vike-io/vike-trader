//! A STOP-LOSS and a TAKE-PROFIT on a hedging FXCM account: the pure binder that turns a reduce-only `stop` or `limit` into
//! trade-bound venue orders (`S` or `L`, `fcshim.cpp`'s `fc_place_exit`), ONE per trade it covers. No session, no FFI.
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
//!   be protected. [`plan_exits`] binds to the OLDEST free trade facing the order whose size EQUALS the order's, caps the
//!   order to the one free trade there is when it holds less than asked (a reduce-only order never closes more than the
//!   position holds), and REFUSES by name otherwise, never sending an exit the venue would refuse or a size nobody chose.
//! * **One order may cover SEVERAL trades** (N venue orders under one client order id): when no single free trade is the
//!   order's size, the OLDEST free trades are taken whole, one exit each, as long as their sizes add up to the order's
//!   EXACTLY. shortcut: a prefix of the oldest-first trades, not a subset sum — a size that falls inside a trade, or
//!   that only a non-adjacent subset reaches, is refused by name; the way to upgrade is an operator asking for the sum
//!   of the trades it wants covered. How the N orders read as one is [`crate::exit_group`].
//! * **A trade carries at most ONE stop and ONE take-profit** (`ORA-20114`): a trade that has the kind is not free for it.
//!   The two kinds of one trade are an OCO pair the venue keeps (when one fills it deletes the other), so a bracket's
//!   stop-loss and take-profit bind the SAME trade without any help from here.
//! * **An exit already resting at the venue that this process did not place is TAKEN OVER, never replaced** (the exits a
//!   restart leaves behind: restart cancels nothing, and the node knows none of them): a new exit of the same kind, at the
//!   same rate, for a trade that carries one is that exit, so the node's order id names it from then on (a cancel and an
//!   amend reach it) and nothing is sent. One at ANOTHER rate is refused by name and left alone: it may be a person's, and a
//!   foreign order is cancelled by hand only.
//! * The exits already on the trades are read from the VENUE's Orders table at every bind ([`parse_open_exits`]), never
//!   from memory: a restart, an exit placed by hand and a partial close (which moves an exit to the remainder's NEW trade
//!   id) cannot leave the binder looking at a stale picture.

use serde_json::Value;
use vike_model::{OrderType, TriggerBy};

use crate::close::{OpenTrade, facing_oldest_first, parse_open_trades};
use crate::event_mapper::venue_rate_differs;
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
    /// Base units it covers: the venue holds an exit to its trade's whole size.
    pub(crate) units: i64,
    /// The rate it rests at. `None` from a shim that predates the takeover (it sends no rate): such an exit is never
    /// taken over, because nothing proves it is the one the order asks for.
    pub(crate) rate: Option<f64>,
    /// The ids a cancel or an amend of it needs (empty from that same older shim).
    pub(crate) account_id: String,
    pub(crate) offer_id: String,
}

/// Parse the `fc_open_exits` array (five keys per row that the binder cannot do without, held equal to the shim's
/// `snprintf` literal by `tests/fxcm_shim_exit.rs`, and three more that only the takeover reads: `rate`, `account_id` and
/// `offer_id`, absent from an older shim). FAIL CLOSED, like [`parse_open_trades`]: one row the binder cannot read refuses
/// the read, because skipping it could report a trade as free and the venue would then refuse the second exit.
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
            let units = row
                .get("amount")
                .and_then(Value::as_i64)
                .filter(|a| *a > 0)
                .ok_or_else(|| format!("open exit row {i} has no positive integer `amount`"))?;
            let optional =
                |k: &str| row.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
            Ok(OpenExit {
                order_id: text("order_id")?,
                trade_id: text("trade_id")?,
                kind,
                units,
                rate: row.get("rate").and_then(Value::as_f64).filter(|r| r.is_finite() && *r > 0.0),
                account_id: optional("account_id"),
                offer_id: optional("offer_id"),
            })
        })
        .collect()
}

/// One venue order of an exit: the trade it protects, what it covers and where it rests. An exit of one trade is one leg;
/// an exit that spans trades is several ([`crate::exit_group::ExitGroup`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExitLeg {
    /// The venue's order id for it: what a fill, a venue-side removal, a cancel and an amend name.
    pub(crate) order_id: String,
    /// The trade it is bound to. A partial close moves it to the remainder's new id ([`crate::exit_group::ExitGroup::follow_split`]).
    pub(crate) trade_id: String,
    /// Base units it covers: the trade's whole size.
    pub(crate) units: i64,
    /// The rate it rests at.
    pub(crate) rate: f64,
    pub(crate) account_id: String,
    pub(crate) offer_id: String,
    /// This exit was ALREADY at the venue and was taken over: nothing was sent for it, and a bind that fails afterwards
    /// leaves it where it was.
    pub(crate) taken_over: bool,
}

impl ExitLeg {
    fn placed(step: &LegPlan, p: PlacedOrder) -> Self {
        Self {
            order_id: p.order_id,
            trade_id: step.trade_id.clone(),
            units: step.units,
            rate: p.rate,
            account_id: p.account_id,
            offer_id: p.offer_id,
            taken_over: false,
        }
    }

    fn taken(step: &LegPlan, e: &OpenExit) -> Self {
        Self {
            order_id: e.order_id.clone(),
            trade_id: step.trade_id.clone(),
            units: step.units,
            // `takeable` held a rate: the planner never takes over an exit that reports none.
            rate: e.rate.unwrap_or_default(),
            account_id: e.account_id.clone(),
            offer_id: e.offer_id.clone(),
            taken_over: true,
        }
    }

    /// The ids a cancel and the rate echo need, in the shape the rest of the exec thread keeps for a resting order.
    pub(crate) fn as_placed(&self) -> PlacedOrder {
        PlacedOrder {
            order_id: self.order_id.clone(),
            offer_id: self.offer_id.clone(),
            account_id: self.account_id.clone(),
            rate: self.rate,
        }
    }
}

/// What a submitted exit asks for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ask<'a> {
    /// FXCM's slashed form (`EUR/USD`).
    pub(crate) instrument: &'a str,
    /// The ORDER's side: a SELL stop closes LONG trades.
    pub(crate) buy: bool,
    pub(crate) kind: ExitKind,
    /// Base units the order covers.
    pub(crate) units: i64,
    /// The rate it rests at.
    pub(crate) rate: f64,
}

/// One trade of a plan: the exit to place on it, or the exit already there to take over.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LegPlan {
    pub(crate) trade_id: String,
    /// Base units the exit covers: the trade's WHOLE size, which is the order's own unless it was capped.
    pub(crate) units: i64,
    /// An exit of this kind already rests on the trade at the asked rate and this process did not place it.
    pub(crate) takeover: Option<OpenExit>,
}

/// Whether `e`, resting on `t`, is the exit `ask` asks for and not this process's: the same rate (to the venue's grid, no
/// tolerance wider than float noise), the trade's whole size and the ids a later cancel needs.
fn takeable(e: &OpenExit, t: &OpenTrade, ask: &Ask<'_>, ours: &impl Fn(&str) -> bool) -> bool {
    !ours(&e.order_id)
        && e.units == t.units
        && !e.account_id.is_empty()
        && !e.offer_id.is_empty()
        && e.rate.is_some_and(|r| !venue_rate_differs(ask.rate, r, 0.0))
}

/// Bind a reduce-only `ask.kind` exit of `ask.units` on `ask.instrument` to the open trades, or say why none can be bound.
///
/// The trades facing the order are taken oldest first ([`facing_oldest_first`], the order a close uses). A trade that
/// already has an exit of this kind is not free, unless that exit is the very one asked for and `ours` (is this venue
/// order id one this process placed?) says it is not this process's: it is then TAKEN OVER. Of the usable trades:
/// the oldest of EXACTLY `ask.units` takes it; else, when exactly one is left and it holds LESS than `ask.units`, the exit
/// is capped to it (the order is a reduce-only one: it never closes more than the position holds, and the caller logs
/// the cap); else the oldest trades whose sizes ADD UP to `ask.units` take one exit each; else the order is refused by name,
/// whether nothing faces it, everything is covered, or the sizes do not fit (the venue refuses a part of a trade).
pub(crate) fn plan_exits(
    trades: &[OpenTrade],
    exits: &[OpenExit],
    ask: &Ask<'_>,
    ours: &impl Fn(&str) -> bool,
) -> Result<Vec<LegPlan>, String> {
    let (order, held) = if ask.buy { ("buy", "short") } else { ("sell", "long") };
    let (word, instrument, units) = (ask.kind.word(), ask.instrument, ask.units);
    let facing = facing_oldest_first(trades, instrument, ask.buy);
    if facing.is_empty() || units <= 0 {
        return Err(format!(
            "fxcm: no open {held} trade on {instrument} for a reduce-only {order} {word} of {units} to bind to; a \
             reduce-only order never opens one, and this venue's {word} is bound to a trade"
        ));
    }
    let mut usable: Vec<(&OpenTrade, Option<&OpenExit>)> = Vec::new();
    let mut blocking: Vec<String> = Vec::new();
    for t in facing {
        match exits.iter().find(|e| e.trade_id == t.trade_id && e.kind == ask.kind) {
            None => usable.push((t, None)),
            Some(e) if takeable(e, t, ask, ours) => usable.push((t, Some(e))),
            Some(e) => blocking.push(match e.rate {
                Some(rate) => format!("order {} on trade {} at {rate}", e.order_id, e.trade_id),
                None => format!("order {} on trade {}", e.order_id, e.trade_id),
            }),
        }
    }
    if usable.is_empty() {
        return Err(format!(
            "fxcm: every open {held} trade on {instrument} already has a {word} bound to it ({}); the venue allows one \
             stop and one take-profit per trade: amend or cancel that one instead of placing another. (An exit this node \
             did not place, as after a restart, is taken over by a {word} of its own rate and trade size)",
            blocking.join(", ")
        ));
    }
    let step = |(t, e): &(&OpenTrade, Option<&OpenExit>), units: i64| LegPlan {
        trade_id: t.trade_id.clone(),
        units,
        takeover: e.cloned(),
    };
    if let Some(one) = usable.iter().find(|(t, _)| t.units == units) {
        return Ok(vec![step(one, units)]);
    }
    if let [only] = &usable[..]
        && only.0.units < units
    {
        return Ok(vec![step(only, only.0.units)]);
    }
    let mut covered = 0;
    let mut legs = Vec::new();
    for one in &usable {
        if covered >= units {
            break;
        }
        covered += one.0.units;
        legs.push(step(one, one.0.units));
    }
    if covered == units {
        return Ok(legs);
    }
    let sizes: Vec<String> =
        usable.iter().map(|(t, _)| format!("{} ({})", t.trade_id, t.units)).collect();
    Err(format!(
        "fxcm: a {word} binds to WHOLE trades and neither one of the free {held} trades on {instrument} nor the oldest of \
         them added up holds exactly {units} units (free, oldest first: {}). The venue refuses an exit for a part of a \
         trade (ORA-20134), so ask for the size of one trade, or of the oldest trades added up",
        sizes.join(", ")
    ))
}

/// What a bind placed or took over: one leg per trade the exit covers.
#[derive(Debug)]
pub(crate) struct Bound {
    pub(crate) legs: Vec<ExitLeg>,
}

impl Bound {
    /// Base units covered: less than asked only when the order was capped ([`plan_exits`]).
    pub(crate) fn units(&self) -> i64 {
        self.legs.iter().map(|l| l.units).sum()
    }
}

/// How a bind failed, in the shape the exec thread's placement failure has: `(sent with an UNKNOWN outcome, the words,
/// what a later lookup of the lost placement needs)`.
pub(crate) type BindFailure = (bool, String, Option<PendingLookup>);

/// What the exec thread asks of the venue for ONE exit: the shim's reads and writes, bound to the session, the order's
/// instrument, side and kind. The real one is `exec.rs`'s `SessionVenue`; a test scripts it.
pub(crate) trait ExitVenue {
    /// `fc_open_trades`: a fresh bounded read of the account's open trades ([`parse_open_trades`]).
    fn open_trades(&mut self) -> Result<String, FxcmError>;
    /// `fc_open_exits`: a fresh bounded read of the exits resting on them ([`parse_open_exits`]).
    fn open_exits(&mut self) -> Result<String, FxcmError>;
    /// `fc_place_exit`: ONE exit on `trade_id`, `units` base units (the trade's whole size), at `rate`.
    fn place(&mut self, trade_id: &str, units: i32, rate: f64) -> Result<PlacedOrder, FxcmError>;
    /// `fc_delete_order`: remove a resting exit.
    fn delete(&mut self, leg: &ExitLeg) -> Result<(), FxcmError>;
    /// `fc_edit_order`: move a resting exit's rate in place, settling a lost reply by reading it back. `Err` = the words.
    fn edit(&mut self, leg: &ExitLeg, rate: f64) -> Result<(), String>;
    /// Whether there is still time inside this command to start another venue order (each can take the shim's whole
    /// bound, and one command must not outrun the order's ack timeout).
    fn may_start(&mut self) -> bool;
}

/// Remove the legs a bind PLACED (never one it took over: that exit was at the venue before this order) and say what
/// came of it, as the tail of a refusal: empty when `legs` is empty.
pub(crate) fn remove_placed(venue: &mut impl ExitVenue, legs: &[ExitLeg]) -> String {
    let mine: Vec<&ExitLeg> = legs.iter().filter(|l| !l.taken_over).collect();
    let kept = legs.len() - mine.len();
    let stuck: Vec<String> = mine
        .iter()
        .filter_map(|l| {
            venue
                .delete(l)
                .err()
                .map(|e| format!("order {} on trade {} ({e})", l.order_id, l.trade_id))
        })
        .collect();
    let mut text = String::new();
    if !mine.is_empty() && stuck.is_empty() {
        text.push_str(&format!("; the {} exit(s) it had placed were removed again", mine.len()));
    } else if !stuck.is_empty() {
        text.push_str(&format!(
            "; these could NOT be removed and still rest at the venue, so cancel them by hand: {}",
            stuck.join(", ")
        ));
    }
    if kept > 0 {
        text.push_str(&format!(
            "; {kept} exit(s) taken over from the venue were left where they were"
        ));
    }
    text
}

/// A bind that failed after it had placed `legs`: the order is refused whole, the words telling what became of them.
fn abandon(venue: &mut impl ExitVenue, legs: &[ExitLeg], why: String) -> BindFailure {
    (false, format!("{why}{}", remove_placed(venue, legs)), None)
}

/// The account's open trades and the exits resting on them, each a FRESH bounded read of the venue. A read that fails,
/// or that the binder cannot read, is the words for why nothing was sent.
pub(crate) fn read_book(
    venue: &mut impl ExitVenue,
) -> Result<(Vec<OpenTrade>, Vec<OpenExit>), String> {
    let trades = venue
        .open_trades()
        .map_err(|e| {
            format!("fxcm could not read the account's open trades, so nothing was sent: {e}")
        })
        .and_then(|json| parse_open_trades(&json).map_err(|e| format!("fxcm: {e}")))?;
    let exits = venue
        .open_exits()
        .map_err(|e| {
            format!("fxcm could not read the account's resting exits, so nothing was sent: {e}")
        })
        .and_then(|json| parse_open_exits(&json).map_err(|e| format!("fxcm: {e}")))?;
    Ok((trades, exits))
}

/// Read the trades and the exits already bound, [`plan_exits`], and place (or take over) one exit per trade it covers
/// ([`place_plan`]). `ours` says whether a venue order id is one this process placed.
///
/// Nothing is sent when either read fails or no trade can be bound: the order is refused in words (`false`: a definite
/// refusal). A placement that was SENT and whose reply was lost is `true`, with the terms a later `confirm` looks it up by
/// (an exit RESTS, so unlike a market close it can be found again) when it was the order's ONLY exit.
pub(crate) fn bind_exits(
    ask: &Ask<'_>,
    ours: &impl Fn(&str) -> bool,
    venue: &mut impl ExitVenue,
) -> Result<Bound, BindFailure> {
    let refused = |why: String| -> BindFailure { (false, why, None) };
    let (trades, exits) = read_book(venue).map_err(refused)?;
    let plan = plan_exits(&trades, &exits, ask, ours).map_err(refused)?;
    place_plan(ask, &plan, venue).map(|legs| Bound { legs })
}

/// Place (or take over) the exit of every step of `plan`, in order.
///
/// ⚠ **An exit that spans trades is placed whole or not at all.** The core believes the order covers the size it asked for,
/// so a stop that covers two trades of three is worse than a refusal: when a later leg fails, or the time bound of one
/// command runs out, the legs placed here are removed again ([`remove_placed`]) and the order is refused, the words
/// naming anything that could not be removed. A lost reply on a leg after the first is a refusal for the same reason (its
/// exit may rest; the words say so). A lost reply on the FIRST of several legs is left to the watchdog with no lookup: a
/// `confirm` that found it would accept an order whose other legs were never placed.
pub(crate) fn place_plan(
    ask: &Ask<'_>,
    plan: &[LegPlan],
    venue: &mut impl ExitVenue,
) -> Result<Vec<ExitLeg>, BindFailure> {
    let word = ask.kind.word();
    let mut legs: Vec<ExitLeg> = Vec::new();
    for (i, step) in plan.iter().enumerate() {
        if let Some(e) = &step.takeover {
            legs.push(ExitLeg::taken(step, e));
            continue;
        }
        let Ok(amount) = i32::try_from(step.units) else {
            let why = format!(
                "fxcm: the amount {} of trade {} is not a figure the shim takes",
                step.units, step.trade_id
            );
            return Err(abandon(venue, &legs, why));
        };
        if i > 0 && !venue.may_start() {
            let why = format!(
                "fxcm: the time bound of one order ran out before the {word} on trade {} ({} of {} trades were done), so \
                 the order is refused whole",
                step.trade_id,
                legs.len(),
                plan.len()
            );
            return Err(abandon(venue, &legs, why));
        }
        match venue.place(&step.trade_id, amount, ask.rate) {
            Ok(p) => legs.push(ExitLeg::placed(step, p)),
            // Nothing was placed before it: the failure is the order's own, as it was for an exit of one trade.
            Err(e) if legs.is_empty() => {
                let (unknown, text) = (e.outcome_unknown(), e.to_string());
                // A lookup that found this one would accept an order whose other legs were never placed.
                let lookup = if plan.len() > 1 { None } else { e.into_lookup() };
                return Err((unknown, text, lookup));
            }
            Err(e) => {
                let note = if e.outcome_unknown() {
                    format!(
                        "the {word} sent for trade {} got no usable answer and MAY rest at the venue",
                        step.trade_id
                    )
                } else {
                    format!("the {word} for trade {} was refused: {e}", step.trade_id)
                };
                let why = format!(
                    "fxcm: the {word} covers {} trades and {note}, so the order is refused whole",
                    plan.len()
                );
                return Err(abandon(venue, &legs, why));
            }
        }
    }
    Ok(legs)
}

#[path = "exit_tests.rs"]
#[cfg(test)]
mod exit_tests;
