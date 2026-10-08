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

use vike_model::events::{Event, FillEvent, OrderCanceled, OrderFilled, OrderRejected, TradeId};

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
                position_side: "BOTH".to_string().into(),
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
/// Case-insensitive to match the core preflight's own `eq_ignore_ascii_case` classification, so a
/// request this returns `true` for is exactly one the `fxcm` caps row admitted as `"market"` —
/// the two must not disagree about what a market order is. `OrderIntent::Flatten` and
/// `MarketExit` reach here through this same arm: both lower to `order_type: "market"`.
#[must_use]
pub fn is_market(order_type: &str) -> bool {
    order_type.eq_ignore_ascii_case("market")
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
/// Two refusals, both closing a silent substitution recorded in the crate's Local traps:
///
/// * **Sizing** — delegated to [`lots_for`], which needs `base_unit_size` because `qty` is BASE
///   UNITS and the shim places LOTS. The caller reads it from the live session
///   ([`crate::sys::FxcmSession::base_unit_size`]) and caches it per instrument; a session that
///   cannot report it refuses the submit rather than assuming a multiplier.
/// * **A priced limit.** `price` was READ BY NOTHING. Anything not classified market rests at a
///   fixed pip distance the shim computes from the live quote (`crates/bridges/fxcm/src/exec.rs`'s
///   `RESTING_PIPS`), so a request carrying `price: Some(1.09)` went on the wire at whatever rate
///   the quote implied. The
///   `fxcm` caps row has always said `"limit"` is an approximation, but an approximation the
///   operator is never told about is indistinguishable from a fill at a price they chose. A limit
///   request that names NO price is unchanged — it rests at `RESTING_PIPS`, which is exactly what
///   the caps row declares — so this refusal costs nothing that ever worked.
///
/// Pure: no session, no FFI, no clock. This is the function the conformance harness and
/// `crates/bridges/fxcm/tests/fxcm_shim_envelope_grammar.rs` both drive.
pub fn preflight_request(
    order_type: &str,
    qty: f64,
    price: Option<f64>,
    base_unit_size: f64,
) -> Result<i32, String> {
    if !is_market(order_type)
        && let Some(px) = price
    {
        return Err(format!(
            "fxcm cannot honor a requested limit price: `{order_type}` @ {px} would rest at the \
                 shim's fixed pip distance from the live quote instead, which is a different price \
                 than the one requested. Send the limit with no price to accept that resting \
                 behaviour, or a market order to execute now"
        ));
    }
    lots_for(qty, base_unit_size)
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
/// `trade_id`; [`crate::mount::FxcmVenueMount`]'s mount wires it, so the hole is open exactly when
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
