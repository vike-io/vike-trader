//! FXCM's whole PURE layer — everything about this venue that can be decided without the
//! ForexConnect SDK, and therefore everything a gate can check on a box that has none.
//!
//! Three jobs, all stateless, all FFI-free:
//!
//! 1. **The async order-event decode** ([`map_fxcm_event`]) — the delayed fills/terminals the
//!    synchronous placement FFI call can't carry. Maps one shim event envelope (JSON drained from
//!    the C++ shim's ForexConnect response-listener queue via `fc_poll_event`) into canonical
//!    events. Honors the dual-publish contract: a fill emits the bare `Event::Fill` (the Account
//!    folds position/PnL) then the `OrderFilled` wrap (the FSM). The shim reports each fill's own
//!    `trade_id`, which is the reconnect dedup key — so when the shim re-surfaces trades after a
//!    ForexConnect reconnect (audit A3), the core drops the overlap and applies only the gap fills.
//! 2. **The submit PREFLIGHT** ([`preflight_request`]) — the request shapes this adapter must
//!    REFUSE rather than silently substitute. See that function; the two refusals it carries were
//!    both live-money defects recorded in `crates/bridges/fxcm/CLAUDE.md`'s Local traps.
//! 3. **The placement mapper** ([`map_placement`]) — the venue half of the emitter split for a
//!    SYNCHRONOUS placement outcome. Lifted out of [`super::exec`]'s `run` so it is drivable
//!    without a session: `crates/vike-bridge-core/tests/bridge_conformance.rs` folds it through the
//!    real `ManagedOrder` FSM as the fxcm accept edge.
//!
//! ⚠ **The envelope grammar in (1) is a CONTRACT WITH C++.** `crates/bridges/fxcm/src/shim/
//! fcshim.cpp`'s `EventListener` builds those JSON strings with `snprintf` format literals, and no
//! compiler on any CI box has ever seen that file. `crates/bridges/fxcm/tests/
//! fxcm_shim_envelope_grammar.rs` is what keeps the two halves in step — it extracts the keys the
//! C++ actually emits and fails when this module stops reading one.

use std::collections::HashMap;

use vike_model::OrderType;
use vike_model::events::{
    Event, FillEvent, OrderCanceled, OrderFilled, OrderModified, OrderModifyRejected,
    OrderRejected, TradeId,
};

const VENUE: &str = "fxcm";

/// Map an FXCM instrument (`"EUR/USD"`) back to the canonical symbol (`"EURUSD"`).
pub fn from_fxcm_instrument(instrument: &str) -> String {
    instrument.replace('/', "").to_uppercase()
}

/// Decode one shim event envelope for the order identified by `coid`. Envelope shapes:
/// `{"kind":"fill","trade_id":..,"instrument":"EUR/USD","side":"B"|"S","amount":<units>,"rate":<px>,"commission":<f>,"ts":<ms>}`,
/// `{"kind":"canceled","ts":..}`, `{"kind":"rejected","reason":..,"ts":..}`. Unknown kinds → empty.
pub fn map_fxcm_event(v: &serde_json::Value, coid: &str) -> Vec<Event> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str());
    let f = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    let ts = v.get("ts").and_then(serde_json::Value::as_i64).unwrap_or(0);
    match s("kind") {
        Some("fill") => {
            let amount = f("amount");
            if amount <= 0.0 {
                return Vec::new();
            }
            // DROPPED, not synthesized. This envelope is the shim's OWN contract (see the module
            // doc): `trade_id` is the field the reconnect dedup is built on — after a ForexConnect
            // reconnect the shim RE-SURFACES trades, and the core dropping the overlap is the only
            // thing that stops them re-booking. An envelope without one cannot participate in that,
            // and nothing else in it is a stable per-fill identity (amount/rate/ts all repeat on the
            // re-surface). A missing `trade_id` is a shim bug; folding it anyway is the double-book.
            // ⚠ Was `unwrap_or_default()` = `""`, which skipped the dedup this module doc relies on.
            let trade_id = match TradeId::new(s("trade_id").unwrap_or_default()) {
                Ok(t) => t,
                Err(_) => {
                    tracing::warn!(
                        venue = VENUE,
                        %coid,
                        "shim fill envelope carries no `trade_id` — dropping it; the reconnect \
                         re-surface would otherwise re-book this fill every time"
                    );
                    return Vec::new();
                }
            };
            let fill = FillEvent {
                trade_id,
                client_order_id: coid.to_string(),
                venue: VENUE.to_string().into(),
                symbol: from_fxcm_instrument(s("instrument").unwrap_or_default()).into(),
                side: if s("side") == Some("S") { -1 } else { 1 },
                last_qty: amount,
                last_px: f("rate"),
                commission: f("commission"),
                commission_asset: String::new().into(),
                liquidity_side: String::new().into(),
                ts,
                mark_price: None,
                position_side: vike_model::events::PositionSide::Both,
            };
            // Dual-publish: bare Fill (Account) then the OrderFilled wrap (FSM).
            vec![
                Event::Fill(fill.clone()),
                Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts }),
            ]
        }
        Some("canceled") => vec![Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.to_string(),
            reason: s("reason").unwrap_or_default().to_string().into(),
            ts,
        })],
        Some("rejected") => vec![Event::OrderRejected(OrderRejected {
            client_order_id: coid.to_string(),
            reason: s("reason").unwrap_or("rejected").to_string().into(),
            ts,
        })],
        _ => Vec::new(),
    }
}

/// Does this request want immediate execution rather than a resting order?
///
/// Exactly [`OrderType::Market`], the kind the core preflight admits through the `fxcm` caps row's
/// `"market"` — the two must not disagree about what a market order is (an `Unrecognized` word is
/// never market here, and [`preflight_request`] refuses it). `OrderIntent::Flatten` and
/// `MarketExit` reach here through this same arm: both lower to `order_type: "market"`.
#[must_use]
pub fn is_market(order_type: &OrderType) -> bool {
    *order_type == OrderType::Market
}

/// The venue-native LOT COUNT for a base-unit `qty` on an instrument whose base unit size is
/// `base_unit_size`, or the refusal reason.
///
/// # ⚠ `qty` IS BASE UNITS. It used to be a lot count, and that was a live-money defect.
///
/// `fcshim.cpp`'s `fc_place` sets `Amount = getBaseUnitSize(instrument, account) * lots`, so the
/// number crossing the FFI boundary has always been LOTS while the number coming back on the fill
/// (`last_qty`) is base units — and `qty` is base units at every other venue in this workspace and
/// on every `OrderRequest` that reaches this one. A caller sending the units it holds everywhere
/// else therefore placed that many LOTS: `qty: 10000` on EUR/USD was ten thousand lots, i.e. ten
/// million units (a base unit of 1000), a thousand times the intended size, accepted without
/// comment.
///
/// The record said this could not be fixed from Rust, because the multiplier is a per-instrument,
/// per-ACCOUNT figure only a live ForexConnect session knows and no gate in this repo could compile
/// an FFI export, let alone run one. **Both halves of that changed**: the CI box has the SDK staged and
/// `.github/workflows/release.yml` builds the artifact there, so `fc_base_unit_size` is a shim
/// export a machine can now build and a live smoke can exercise. This function is its consumer, and
/// [`crate::sys::FxcmSession::base_unit_size`] reads the figure from the SAME two lookups the
/// placement multiplies by, so the round trip is an identity by construction rather than by
/// agreement between two numbers.
///
/// # Why an inexact size is REFUSED rather than rounded
///
/// FXCM cannot place a fraction of a lot: `Amount` is `baseUnit * lots` with `lots` an `int`. So a
/// `qty` that is not a whole multiple of the base unit has NO faithful representation, and every
/// available substitute is somebody else's number — which is the exact defect this venue has now
/// produced twice. The refusal names the base unit and the two placeable sizes on either side, so
/// the caller re-sends a size they chose.
///
/// The four shapes the pre-#1470 expression `(qty.round() as i32).max(1)` silently substituted are
/// still refused, and are now refused for the same reason as everything else — they are not exact
/// multiples:
///
/// * **A sub-lot request.** `0.2` was floored UP to ONE LOT — five times the requested size.
/// * **A fractional request.** `1.6` became `2`.
/// * **A non-finite / non-positive request.** `NaN as i32` is `0`, then `.max(1)` → one lot.
/// * **An out-of-range request.** `as i32` SATURATES in Rust, so `1e30` became `i32::MAX` lots.
///
/// Every refusal becomes a terminal `OrderRejected` at the call site ([`map_placement`]), so the
/// no-silent-vanish half of the venue-adapter contract holds: the order does not go on the wire and
/// it does not disappear either.
pub fn lots_for(qty: f64, base_unit_size: f64) -> Result<i32, String> {
    // The multiplier itself is checked FIRST and hard: it comes over the FFI boundary, a zero would
    // make every division degenerate and a negative one would flip the side. `fc_base_unit_size`
    // already refuses a non-positive figure; this is the Rust-side half of the same refusal, so the
    // pure function is safe to call with anything and the property is testable without a session.
    if !base_unit_size.is_finite() || base_unit_size <= 0.0 {
        return Err(format!(
            "fxcm cannot size an order: the venue reported a base unit size of {base_unit_size} \
             for this instrument, which is not a positive finite number. Nothing can be converted \
             from it, and guessing a multiplier is how a size nobody chose gets placed"
        ));
    }
    if !qty.is_finite() || qty <= 0.0 {
        return Err(format!("fxcm: qty {qty} is not a positive, finite size in base units"));
    }
    // `%` on f64 is `fmod`, which is EXACT for finite operands — no rounding can make a
    // non-multiple look like one, and none can make a multiple look inexact. Deliberately not
    // `(qty / base).fract()`, whose rounding decides the answer at the boundary.
    if qty % base_unit_size != 0.0 {
        let lots_below = (qty / base_unit_size).floor();
        return Err(format!(
            "fxcm places whole LOTS of {base_unit_size} base units: qty {qty} is not a multiple of \
             that, and this venue has no way to place a fraction of a lot (the shim sets \
             `Amount = base_unit * lots` with an integer lot count). The nearest placeable sizes \
             are {} and {}. Rounding to one of them is what this adapter used to do silently, \
             which is why it refuses instead",
            lots_below * base_unit_size,
            (lots_below + 1.0) * base_unit_size
        ));
    }
    let lots = qty / base_unit_size;
    if lots > f64::from(i32::MAX) {
        return Err(format!(
            "fxcm: qty {qty} is {lots} lots of {base_unit_size} base units, which exceeds the \
             shim's i32 `lots` parameter"
        ));
    }
    Ok(lots as i32)
}

/// Everything about a submit request this adapter must decide BEFORE the shim sees it: the lot
/// count it will place, or the reason it will place nothing.
///
/// Four refusals, each closing a silent substitution (the first is recorded in the crate's Local
/// traps):
///
/// * **Sizing** — delegated to [`lots_for`], which needs `base_unit_size` because `qty` is BASE
///   UNITS and the shim places LOTS. The caller reads it from the live session
///   ([`crate::sys::FxcmSession::base_unit_size`]) and caches it per instrument; a session that
///   cannot report it refuses the submit rather than assuming a multiplier.
/// * **A kind that is not market, limit or stop** — including [`OrderType::Unrecognized`], and
///   `stop_limit`, `take_profit`: each used to fall into the resting-limit arm. The core preflight
///   refuses them first; this is the adapter's own guard. (`stop` joined the two on 2026-10-26, as a
///   TRADE-BOUND exit only: [`crate::exit::exit_shape`] and [`crate::close::route`] refuse it where it
///   would be an entry order.)
/// * **A limit price that cannot be a rate** (non-finite, zero, negative). A PRICED limit is no longer
///   refused: the price travels to the shim as the entry's RATE (`fc_place_entry_at`, ABI 2; the
///   shim refuses a rate off the instrument's price grid). It was refused from #1470 until then
///   because the only entry point took a pip distance from the quote (`RESTING_PIPS`), so a request
///   carrying `price: Some(1.09)` would have rested at whatever rate the quote implied. A limit with
///   NO price is unchanged: it still rests at `RESTING_PIPS`, as the caps row declares.
///
/// Pure: no session, no FFI, no clock. This is the function the conformance harness and
/// `crates/bridges/fxcm/tests/fxcm_shim_envelope_grammar.rs` both drive.
pub fn preflight_request(
    order_type: &OrderType,
    qty: f64,
    price: Option<f64>,
    base_unit_size: f64,
) -> Result<i32, String> {
    if let OrderType::Unrecognized(word) = order_type {
        return Err(format!(
            "fxcm cannot place an unrecognised order type `{word}` — it would rest as a plain \
             limit entry, which is not what that kind means; order refused"
        ));
    }
    // The kinds the caps row declares are the only ones placeable; every other recognised kind
    // (`stop_limit`, `take_profit`) used to fall into the resting-limit arm and rest at a price
    // that is not a trigger at all. A `stop` is placeable only as a trade-bound exit
    // ([`crate::exit`]): whether it IS one (reduce-only, a trigger, a hedging account) is the
    // router's and the binder's to decide, which they need more than this function's arguments for.
    if !is_market(order_type) && *order_type != OrderType::Limit && *order_type != OrderType::Stop {
        return Err(format!(
            "fxcm places only `market`, `limit` and (as a trade's protective order) `stop` \
             orders; `{order_type}` is not one of them and would otherwise rest as a plain limit \
             entry, which is not what that kind means"
        ));
    }
    // A limit's price goes to the shim as the RATE of the entry (`fc_place_entry_at`), so a price
    // that cannot be a rate is refused here, before the wire. (Whether it is ON the instrument's
    // price grid is the shim's check: only a live session knows the digits.)
    if *order_type == OrderType::Limit
        && let Some(px) = price
        && (!px.is_finite() || px <= 0.0)
    {
        return Err(format!(
            "fxcm: limit price {px} is not a positive, finite rate; order refused"
        ));
    }
    lots_for(qty, base_unit_size)
}

/// The shim's codes for a request that WAS SENT and whose outcome it cannot tell (`fcshim.cpp`'s
/// `FC_SENT_NO_ANSWER` / `FC_SENT_NO_ORDER_ID`): no answer inside the wait, or an answer with neither
/// an error nor an order id. Distinct from `-7`/`-8`, which are also returned before anything is sent.
pub const SENT_NO_ANSWER: i32 = -11;
/// See [`SENT_NO_ANSWER`].
pub const SENT_NO_ORDER_ID: i32 = -12;

/// A native return code that means "sent, outcome UNKNOWN" - the order may rest, or the amend may
/// have applied. ⚠ Never a refusal: a terminal `OrderRejected` for an order that rests leaves a live
/// order the book does not know. Pure.
pub fn outcome_is_unknown(code: i32) -> bool {
    code == SENT_NO_ANSWER || code == SENT_NO_ORDER_ID
}

/// Does a read-back after a lost amend reply still need a second look? `false` only when it shows the
/// REQUESTED rate (the amend applied); a stale rate, an unreadable order and a shim with no read-back
/// are all inconclusive, because the broker may apply a late edit after the read. Pure.
pub fn amend_needs_reread(requested: f64, current: &Result<Option<(f64, f64)>, String>) -> bool {
    !matches!(current, Ok(Some((rate, tick))) if !venue_rate_differs(requested, *rate, *tick))
}

/// Settle an amend whose reply never arrived by reading the order back: `requested` is the rate asked
/// for, `current` the venue's `(rate, tick)` from a fresh Orders read. `Ok(rate)` -> the amend DID
/// apply (the event carries the venue's rate); `Err(text)` -> `OrderModifyRejected` naming what is
/// known. A rate off the request, an unreadable order and a shim with no read-back are all refusals
/// that say so, so the stale local price is never silent. Pure.
///
/// ⚠ **A read-back that shows the OLD rate does not prove the amend failed**: the broker may apply the
/// edit after the read. The text says exactly that (NOT confirmed, reconcile decides), and the caller
/// reads once more after a short pause ([`amend_needs_reread`]) before it asks this.
pub fn resolve_unknown_amend(
    requested: f64,
    why: &str,
    current: Result<Option<(f64, f64)>, String>,
) -> Result<f64, String> {
    match current {
        Ok(Some((rate, tick))) if !venue_rate_differs(requested, rate, tick) => Ok(rate),
        Ok(Some((rate, _))) => Err(format!(
            "fxcm amend to {requested} got no confirmed answer ({why}); NOT confirmed: the venue still shows {rate} but the edit may still apply, reconcile decides"
        )),
        Ok(None) => Err(format!(
            "fxcm amend to {requested} got no confirmed answer ({why}) and this shim cannot read the order back: its rate is UNKNOWN until reconcile reports it"
        )),
        Err(e) => Err(format!(
            "fxcm amend to {requested} got no confirmed answer ({why}) and the order could not be read back ({e}): its rate is UNKNOWN until reconcile reports it"
        )),
    }
}

/// Whether the rate the VENUE holds for an order differs from the one asked for by MORE than half a
/// tick. Both sides sit on the instrument's grid (the shim refuses an off-grid request), so a real
/// difference is at least one whole tick and half a tick separates it from float noise. A tick of
/// `0.0` (the shim could not read the grid) falls back to a `1e-9` floor. Pure; a non-finite venue
/// rate differs.
pub fn venue_rate_differs(requested: f64, venue: f64, tick: f64) -> bool {
    let gap = (venue - requested).abs();
    gap.is_nan() || gap > (tick / 2.0).max(1e-9)
}

/// What an amend of a resting order asks of the shim: the new RATE, or the reason it asks nothing.
///
/// ForexConnect's `EditOrder` is wired for the RATE only (`fcshim.cpp`'s `fc_edit_order`), so a
/// request that also names a DIFFERENT size is refused rather than half-applied — the order would
/// keep its old size while the caller believed it had the new one. A `new_qty` equal to the order's
/// own is no change and passes. Pure; the refusal becomes a NON-terminal `OrderModifyRejected`
/// ([`map_amend`]), so the resting order keeps its terms.
pub fn plan_amend(
    order_qty: f64,
    new_qty: Option<f64>,
    new_price: Option<f64>,
) -> Result<f64, String> {
    if let Some(q) = new_qty
        && (q - order_qty).abs() > f64::EPSILON * order_qty.abs().max(1.0)
    {
        return Err(format!(
            "fxcm amends the price of a resting order only: a new size ({q}, was {order_qty}) is \
             not supported. Cancel it and place a new order at the new size"
        ));
    }
    match new_price {
        Some(px) if px.is_finite() && px > 0.0 => Ok(px),
        Some(px) => Err(format!("fxcm: amend price {px} is not a positive, finite rate")),
        None => {
            Err("fxcm amends the price of a resting order only, and this amend names none".into())
        }
    }
}

/// The venue half of an amend: `Ok(())` → `OrderModified` (the new rate, under the SAME venue order
/// id — `EditOrder` edits in place); `Err(reason)` → the NON-terminal `OrderModifyRejected`, so a
/// refused amend is never silent and never ends the order.
pub fn map_amend(
    coid: &str,
    venue_order_id: Option<&str>,
    ts: i64,
    new_price: f64,
    outcome: Result<(), &str>,
) -> Event {
    match outcome {
        Ok(()) => Event::OrderModified(OrderModified {
            client_order_id: coid.into(),
            venue_order_id: venue_order_id.map(Into::into),
            new_qty: None,
            new_price: Some(new_price),
            ts,
        }),
        Err(reason) => {
            let reason = if reason.trim().is_empty() { "fxcm amend failed" } else { reason };
            Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: coid.into(),
                reason: reason.to_string().into(),
                ts,
            })
        }
    }
}

/// The venue half of the emitter split for a SYNCHRONOUS placement outcome.
///
/// `Ok(venue_order_id)` → `[OrderAccepted]`; `Err(reason)` → `[OrderRejected]`, the terminal a dead
/// or refused venue path MUST synthesize so the intent never silently vanishes. An empty reason is
/// filled in rather than passed through — the contract is that a synthesized reject carries one,
/// and `crates/vike-bridge-core/tests/bridge_conformance.rs` checks exactly that.
///
/// This lived inline in [`super::exec`]'s `run` as two `Event` literals built off a
/// `Result<PlacedOrder, FxcmError>`. Lifting it here is what lets the conformance harness drive the
/// REAL accept edge instead of mirroring it, the way deribit's harness row has to.
pub fn map_placement(coid: &str, ts: i64, placement: Result<&str, &str>) -> Vec<Event> {
    match placement {
        Ok(venue_order_id) => vec![Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: coid.into(),
            venue_order_id: Some(venue_order_id.into()),
            ts,
        })],
        Err(reason) => {
            let reason = if reason.trim().is_empty() { "fxcm placement failed" } else { reason };
            vec![Event::OrderRejected(OrderRejected {
                client_order_id: coid.into(),
                reason: reason.to_string().into(),
                ts,
            })]
        }
    }
}

/// Route ONE drained shim envelope to the client order id that placed it, then decode it.
///
/// ⚠ **An envelope naming a venue order this process never placed publishes NOTHING, and that is a
/// DECLARED HOLE, not a decision.** `order_coid` is built at accept time and lives only in this
/// process's memory, so the fills a ForexConnect reconnect re-surfaces route fine WITHIN a session
/// and every one of them is unroutable ACROSS a restart — the re-surfaced trades name orders the
/// new process never saw. What would close it is the reconcile client
/// ([`mod@super::recon_client`]), whose `fetch_fill_reports` reads the same Trades table by
/// `trade_id`; [`crate::adapter::FxcmVenueAdapter`]'s start wires it, so the hole is open exactly when
/// reconciliation is off.
///
/// It used to be a bare `continue`. A dropped money event is now `warn!`-visible, the same
/// standard [`map_fxcm_event`]'s own `trade_id` refusal is held to.
pub fn map_drained_event(
    v: &serde_json::Value,
    order_coid: &HashMap<String, String>,
) -> Vec<Event> {
    let oid = v.get("order_id").and_then(|x| x.as_str()).unwrap_or_default();
    match order_coid.get(oid) {
        Some(coid) => map_fxcm_event(v, coid),
        None => {
            tracing::warn!(
                venue = VENUE,
                venue_order_id = %oid,
                kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("?"),
                "dropping a shim event for an order this process did not place — after a restart \
                 every re-surfaced trade lands here; reconcile is what recovers them"
            );
            Vec::new()
        }
    }
}

/// Can another envelope for this venue order still arrive? `false` once the shim has reported the
/// order CANCELED or REJECTED — neither can be followed by anything, so its routing row is dead.
///
/// A FILL deliberately does NOT close routing: an order can trade in several pieces, and the
/// reconnect re-surface replays every one of them (the core dedups by `trade_id`). Dropping the
/// route on the first fill would turn the second one into the unroutable case above.
#[must_use]
pub fn closes_routing(evs: &[Event]) -> bool {
    evs.iter().any(|e| matches!(e, Event::OrderCanceled(_) | Event::OrderRejected(_)))
}

/// Is this order past the point where it could be CANCELED? `true` for every terminal, fill
/// included — `delete_order` needs the resting order's ids and a terminal order has no resting
/// order, so its entry in the cancel map is dead weight from here on.
#[must_use]
pub fn ends_cancelability(evs: &[Event]) -> bool {
    evs.iter().any(|e| {
        matches!(e, Event::OrderCanceled(_) | Event::OrderRejected(_) | Event::OrderFilled(_))
    })
}

#[path = "event_mapper_tests.rs"]
#[cfg(test)]
mod event_mapper_tests;
