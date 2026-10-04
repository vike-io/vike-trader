//! DeribitRest — JSON-RPC private/buy|sell|cancel over the order transport. Exact port
//! of `exec/deribit/client.py` (NOT a REST/HMAC client — Deribit is JSON-RPC/WS; it
//! implements the same `VenueRest` seam so `LiveRestClient` reuses the live order path).
//!
//! amount/price are COIN units (options); **post_only is forced FALSE** (Deribit
//! defaults it true — a crossing order would otherwise be rejected/repriced: the trap
//! the Python program hit live). Instruments: tick_size + min_trade_amount from
//! public/get_instruments.

use std::sync::Mutex;

use indexmap::IndexMap;
use serde_json::{Value, json};

use vike_bridge_core::rest::VenueRest;
use vike_exec::ReconcileSnapshot;
use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted};
use vike_model::{AssetClass, OrderRequest, SymbolProperties};

use crate::combo::{
    ComboMapError, build_combo_order, build_combo_order_params, build_create_combo_params,
    map_combo, parse_combo, parse_combo_grid,
};
use crate::reconcile::build_reconcile_snapshot;
use crate::rpc::parse_response;
use crate::transport::{DeribitOrderTransport, is_dead_socket_error};
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::json_num;
use vike_bridge_core::transport::VenueApiError;

/// cancel of an already-gone order — swallowed (unknown ≠ rejection):
/// 10004 order_not_found, 11044 not_open_order, 10010 already_closed, 11008 already_filled
const NOT_FOUND: [i64; 4] = [10004, 11044, 10010, 11008];

/// Canonical venue id — the deribit `venue_tif` row key (tif step-2).
const VENUE: &str = "deribit";

/// Option instrument row — twin of `parse_deribit_option_instruments` (step = min qty =
/// min_trade_amount; no per-instrument max/notional).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeribitInstrument {
    pub properties: SymbolProperties,
    pub contract_size: f64,
    pub base_asset: String,
}

/// Parse a `public/get_instruments` payload into the OPTION rows' order grids, keyed by instrument
/// name.
///
/// ⚠ **The `kind == "option"` gate below is LOAD-BEARING — do not widen it to "classify the other
/// kinds too".** Two measurements say so:
///
/// * The frozen `fixtures/r6/deribit.json` `instruments_payload` carries a `BTC-PERPETUAL`
///   `kind: "future"` row, and the frozen `instruments` map it is compared against holds the two
///   OPTION rows alone — `crates/bridges/deribit/tests/offline/r6_deribit_parity.rs`'s
///   `instruments_parity` asserts that membership (the message is `"future + non-option rows
///   skipped"`). Admitting a future changes what this
///   function RETURNS, against oracle bytes that are the reference by
///   `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`.
/// * The gate is not even the only one: [`option_base_asset`] then requires the
///   `BASE-DDMMMYY-STRIKE-C|P` name shape, because `DeribitInstrument::base_asset` is DEFINED by
///   that grammar. A future has no such name and would be dropped a second time.
///
/// So this function classifies options and nothing else BY DESIGN, and that is not where Deribit's
/// futures/perps are missed. They are classified in `crates/bridges/deribit/src/exec.rs`'s
/// `parse_get_instrument` — the singular `public/get_instrument` grid, which is symbol-agnostic and
/// is what `crate::mount`'s `DeribitVenueMount` and the PIT properties recorder actually call.
/// This function has no caller under `src/` at all: its live callers are this crate's smoke tests
/// (each of which asks the venue for `kind=option` in the REQUEST) plus the r6 parity test.
pub fn parse_deribit_option_instruments(payload: &Value) -> IndexMap<String, DeribitInstrument> {
    let mut out = IndexMap::new();
    for entry in payload.get("result").and_then(|r| r.as_array()).unwrap_or(&vec![]) {
        if entry.get("kind").and_then(|k| k.as_str()) != Some("option") {
            continue;
        }
        let name = entry.get("instrument_name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        // Python gates on parse_instrument_name: BASE-DDMMMYY-STRIKE-C|P
        let Some(base) = option_base_asset(&name) else { continue };
        let f = |key: &str| -> f64 { entry.get(key).and_then(json_num).unwrap_or(0.0) };
        let step = f("min_trade_amount");
        out.insert(
            name,
            DeribitInstrument {
                properties: SymbolProperties {
                    tick_size: f("tick_size"),
                    step_size: step,
                    min_qty: step,
                    // Same value as the sibling `contract_size` field below, now ALSO carried on
                    // the grid so it travels with `SymbolProperties` wherever that goes (the PIT
                    // properties series, `Instrument`, the engine's multiplier grid). The struct
                    // field is retained: the r6 parity fixture pins it, and the option-chain path
                    // reads it directly.
                    contract_size: f("contract_size"),
                    // The class is the VENUE'S OWN `kind`, and this parser has ALREADY matched it
                    // exactly: the `continue` at the top of the loop admits only rows whose
                    // `kind` is the literal `"option"`, so every entry reaching here is one the
                    // venue itself called an option. Reading the same field a second time here
                    // could only ever disagree with the gate that let the row in.
                    // Deribit's other kinds (`future`, `future_combo`, `option_combo`, `spot`)
                    // never arrive here; `crates/bridges/deribit/src/catalog.rs`'s
                    // `asset_class_of` is the crate's ONE classifier over `kind` +
                    // `settlement_period`, and both of the venue's instrument payloads call it —
                    // the catalog list and `exec.rs`'s singular `parse_get_instrument` grid.
                    // ⚠ NOT derived from the `BASE-DDMMMYY-STRIKE-C|P` instrument name, although
                    // `option_base_asset` parses exactly that shape a few lines below — a name
                    // test is the implicit encoding
                    // `docs/decisions/0061-an-instrument-names-its-kind.md` exists to remove.
                    asset_class: Some(AssetClass::Option),
                    // Deribit DOES publish `tick_size_steps`, but parsing it is deliberately NOT
                    // in this lane: only the singular `public/get_instrument` read attaches the
                    // scheme (`crates/bridges/deribit/src/exec.rs`'s `parse_tick_scheme`), though
                    // the vike-data `kind=properties` codec does carry a `tick_scheme` column now
                    // (`crates/vike-data/src/datafusion_hist/codec.rs`'s `decode_tick_scheme`).
                    // Absent = the scalar `tick_size` IS the grid, exactly as before — and it
                    // stays absent through `..Default::default()`, which also covers
                    // max_qty/min_notional (no per-instrument caps) and `taker_hold_ms` (Deribit
                    // declares no venue hold). FRU rather than an exhaustive literal so a new
                    // `SymbolProperties` field costs this parser nothing.
                    ..Default::default()
                },
                contract_size: f("contract_size"),
                base_asset: base,
            },
        );
    }
    out
}

/// The read-side `parse_instrument_name` regex (`^([A-Z]+)-(\d{1,2}[A-Z]{3}\d{2})-(\d+)-([CP])$`)
/// reduced to the exec-side need: the base currency, or None for a non-option name.
fn option_base_asset(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('-').collect();
    if parts.len() != 4 {
        return None;
    }
    let (base, expiry, strike, cp) = (parts[0], parts[1], parts[2], parts[3]);
    let base_ok = !base.is_empty() && base.bytes().all(|b| b.is_ascii_uppercase());
    let expiry_ok = expiry.len() >= 5
        && expiry.len() <= 7
        && expiry[..expiry.len() - 5].bytes().all(|b| b.is_ascii_digit())
        && expiry[expiry.len() - 5..expiry.len() - 2].bytes().all(|b| b.is_ascii_uppercase())
        && expiry[expiry.len() - 2..].bytes().all(|b| b.is_ascii_digit());
    let strike_ok = !strike.is_empty() && strike.bytes().all(|b| b.is_ascii_digit());
    let cp_ok = cp == "C" || cp == "P";
    (base_ok && expiry_ok && strike_ok && cp_ok).then(|| base.to_string())
}

pub struct DeribitRest {
    /// the persistent authed order WS (Mutex: VenueRest takes &self; JSON-RPC calls
    /// mutate the socket)
    pub transport: Mutex<DeribitOrderTransport>,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub currency: String,
    /// coid -> venue order_id (cancel is by order_id)
    order_ids: Mutex<std::collections::HashMap<String, String>>,
}

impl DeribitRest {
    pub fn new(
        transport: DeribitOrderTransport,
        symbol: &str,
        properties: SymbolProperties,
        currency: &str,
    ) -> Self {
        DeribitRest {
            transport: Mutex::new(transport),
            symbol: symbol.to_string(),
            properties,
            currency: currency.to_string(),
            order_ids: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Pure, golden-gated. amount/price are NUMBERS (Python float(format_qty(...))).
    ///
    /// tif step-2 (FLIPPED): the limit path consumes deribit's `venue_tif` row — Gtc stays
    /// NotEmitted (no `time_in_force` param, the venue's server default `good_til_cancelled`
    /// rules; byte-identical to pre-flip), Ioc/Fok/Day map to Deribit's own TIF vocabulary
    /// (`immediate_or_cancel`/`fill_or_kill`/`good_til_day`). Gtd is Unsupported and gated at
    /// submit (`deny_unsupported_tif`) — if one reaches this builder anyway, no param is
    /// emitted (venue default), the gate being the loud guard. Market orders keep today's
    /// no-TIF shape (the table's resting-path scope).
    pub fn build_order_params(&self, request: &OrderRequest) -> Value {
        let is_limit = request.order_type.eq_ignore_ascii_case("limit");
        let amount =
            format_to_step_f(request.qty, self.properties.step_size).parse::<f64>().unwrap_or(0.0);
        let mut params = json!({
            "instrument_name": self.symbol,
            "amount": amount,
            "type": if is_limit { "limit" } else { "market" },
            "label": request.client_order_id,
            // Deribit defaults post_only=true; a crossing/marketable order MUST set false
            "post_only": false,
        });
        if is_limit {
            if let Some(px) = request.price {
                // Tiered grid (Deribit options carry a `tick_scheme`): snap the limit to the tick
                // IN FORCE at this price (half-to-even `round_price`) and format to that SAME tier
                // tick. A price above a tier boundary formatted on the scalar BASE grid is what the
                // venue rejects — the order-entry landmine this closes. With NO scheme (futures/
                // perps, and any option the venue reports a flat grid for) the `None` arm is
                // LITERALLY today's expression `format_to_step_f(px, tick_size)`, so the wire is
                // byte-identical (pinned in `tick_scheme_order_format_tests` below).
                let px_str = match &self.properties.tick_scheme {
                    Some(scheme) => format_to_step_f(scheme.round_price(px), scheme.tick_at(px)),
                    None => format_to_step_f(px, self.properties.tick_size),
                };
                params["price"] = json!(px_str.parse::<f64>().unwrap_or(0.0));
            }
            match vike_model::venues::venue_tif::venue_tif(VENUE, request.time_in_force) {
                vike_model::venues::venue_tif::TifOutcome::Mapped(w)
                | vike_model::venues::venue_tif::TifOutcome::Coerced { wire: w, .. } => {
                    params["time_in_force"] = json!(w);
                }
                vike_model::venues::venue_tif::TifOutcome::Ignored { .. }
                | vike_model::venues::venue_tif::TifOutcome::NotEmitted
                | vike_model::venues::venue_tif::TifOutcome::Unsupported => {}
            }
        }
        if request.reduce_only {
            params["reduce_only"] = json!(true);
        }
        params
    }

    /// Audit A3 resync: recent order history (`private/get_order_history_by_instrument`) for the
    /// post-reconnect replay — a BARE array. `include_unfilled` is required so cancelled/rejected
    /// orders that never traded (the non-fill terminals A3 recovers) are not omitted.
    pub fn get_order_history(&self, count: u32) -> Result<Value, VenueApiError> {
        self.private_result(
            "private/get_order_history_by_instrument",
            &json!({
                "instrument_name": self.symbol,
                "count": count,
                "include_old": true,
                "include_unfilled": true,
            }),
        )
    }

    /// Audit A3 resync: recent user trades (`private/get_user_trades_by_instrument`) for the replay.
    /// The result is `{trades, has_more}`; this returns the unwrapped `trades` array.
    pub fn get_user_trades(&self, count: u32) -> Result<Value, VenueApiError> {
        let result = self.private_result(
            "private/get_user_trades_by_instrument",
            &json!({ "instrument_name": self.symbol, "count": count, "sorting": "asc" }),
        )?;
        Ok(result.get("trades").cloned().unwrap_or(json!([])))
    }

    /// Audit T1: re-query one order by its `label` (= our idempotent client_order_id) over the WS.
    /// `Ok(Some(id))` = live at venue, `Ok(None)` = venue confirms absent (empty result), `Err` =
    /// the query itself failed. `private/get_order_state_by_label` returns an array of orders.
    ///
    /// ⚠ **`Ok(None)` licenses a terminal reject, so what this endpoint REPORTS is load-bearing:
    /// if it answered with OPEN orders only, an order that filled instantly would re-query empty
    /// and be rejected while the fill stood.** It does not. MEASURED on Deribit testnet
    /// 2026-08-25, both terminal shapes, account left flat:
    ///
    /// * a rested limit, then cancelled → `get_order_state_by_label` → `"order_state":"cancelled"`
    /// * a market order, then filled → `get_order_state_by_label` → `"order_state":"filled"`
    ///
    /// So an empty array genuinely means the venue never took the order, and the `Ok(None)` arm is
    /// sound. The ONLY thing that reopens this is Deribit changing the endpoint — re-measure, do
    /// not re-argue.
    fn query_order_by_label(&self, coid: &str) -> Result<Option<String>, VenueApiError> {
        let result = self.private_result(
            "private/get_order_state_by_label",
            &json!({ "currency": self.currency, "label": coid }),
        )?;
        Ok(result.as_array().and_then(|a| a.first()).and_then(|o| o.get("order_id")).map(
            |v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            },
        ))
    }

    /// `pub(crate)` (not private): `recon_client.rs`'s `DeribitReconClient` reuses this SAME
    /// signed JSON-RPC entry point — over its OWN `DeribitRest`, and therefore its own
    /// `Mutex<DeribitOrderTransport>`, so a reconcile read never takes the exec side's order
    /// transport (`DeribitReconClient::connect`'s doc carries that argument; this comment claimed
    /// the opposite — a shared socket — from before that deviation landed).
    ///
    /// ⚠ That separation is what lets `DeribitReconClient::call` RE-DIAL a dead socket on a
    /// transport failure. Nothing on the exec path may do the same blindly: a re-sent
    /// `private/buy` is a second order.
    pub(crate) fn private_result(
        &self,
        method: &str,
        params: &Value,
    ) -> Result<Value, VenueApiError> {
        let resp = self.transport.lock().unwrap().call(method, params)?;
        let (_rid, result, error) = parse_response(&resp);
        if let Some(err) = error.filter(|e| !e.is_null()) {
            return Err(VenueApiError {
                code: err.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
                msg: err.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string(),
            });
        }
        Ok(result.unwrap_or(json!([])))
    }

    /// Reconcile on arm: currency-scoped positions (filtered to the instrument) + the
    /// instrument's open orders. Raises on any getter error (a bad reconcile must abort
    /// the arm before any fill).
    pub fn connect(&self) -> Result<ReconcileSnapshot, VenueApiError> {
        let positions = self.private_result(
            "private/get_positions",
            &json!({"currency": self.currency, "kind": "option"}),
        )?;
        let orders = self.private_result(
            "private/get_open_orders_by_instrument",
            &json!({"instrument_name": self.symbol, "type": "all"}),
        )?;
        Ok(build_reconcile_snapshot(&positions, &orders, &self.symbol))
    }

    pub fn detach(&self) {
        self.transport.lock().unwrap().close();
    }

    /// Record this instrument's parsed properties grid into the PIT properties store (opt-in). Best-effort;
    /// the recorder no-ops when disabled and swallows store errors. Call after `new`.
    pub fn with_properties_recorder(
        self,
        rec: &std::sync::Arc<vike_data::PropertiesRecorder>,
    ) -> Self {
        rec.record("deribit", &self.symbol, self.properties, now_ns());
        self
    }
}

use vike_model::now_ns;

impl DeribitRest {
    /// Resolve a combo `OrderRequest` (non-empty `combo_legs`) to its venue-side order params.
    ///
    /// Two venue calls, per the combo-book contract (see [`crate::combo`]): register the leg
    /// structure with `private/create_combo` (which returns the CANONICAL combo — possibly the
    /// inverse of, or a ratio-reduced form of, what we asked for), then reconcile that against the
    /// spec to get the orientation/scale mapping. Any failure to reconcile is an `Err(reason)` the
    /// caller turns into a terminal `OrderRejected` — we never guess a direction or a net price.
    ///
    /// The combo's OWN tick/step grid is fetched from `public/get_instrument` on the combo id (the
    /// legs' grids do not apply to the combo book). A PARTIAL parse — either axis missing or zero
    /// — counts as a failed fetch ([`parse_combo_grid`] requires BOTH axes positive), never a
    /// silently-unquantized axis; on any failure we fall back to this client's configured leg
    /// grid (logged), which at worst leaves the value unquantized (`format_to_step`'s zero-step
    /// passthrough) rather than blocking the order.
    fn resolve_combo_order(&self, request: &OrderRequest) -> Result<(&'static str, Value), String> {
        let create = self
            .private_result("private/create_combo", &build_create_combo_params(&request.combo_legs))
            .map_err(|e| format!("create_combo failed: {} ({})", e.msg, e.code))?;
        let combo = parse_combo(&create).ok_or_else(|| ComboMapError::Unparseable.to_string())?;
        let combo = self.wait_combo_active(combo)?;
        let mapping = map_combo(&request.combo_legs, &combo.legs).map_err(|e| e.to_string())?;

        // The combo book's own grid; a failed OR PARTIAL fetch (either axis missing/zero) falls
        // back to this client's configured leg grid — loudly, never a half-used grid.
        let grid = self
            .private_result("public/get_instrument", &json!({ "instrument_name": combo.id }))
            .ok()
            .and_then(|r| parse_combo_grid(&r))
            .unwrap_or_else(|| {
                tracing::warn!(
                    target: "vike_deribit::combo",
                    combo_id = %combo.id,
                    "combo grid fetch failed or partial — falling back to the configured leg grid"
                );
                (self.properties.tick_size, self.properties.step_size)
            });

        // A limit combo carries the SIGNED net; `price: None` (or a non-limit type) is a combo
        // MARKET order. The sign is never inspected — a credit combo's negative net rides through.
        let net =
            request.order_type.eq_ignore_ascii_case("limit").then_some(request.price).flatten();
        let order = build_combo_order(&combo.id, mapping, request.side, request.qty, net);
        let method = if order.side > 0 { "private/buy" } else { "private/sell" };
        tracing::info!(
            target: "vike_deribit::combo",
            combo_id = %order.instrument_name,
            coid = %request.client_order_id,
            num = mapping.num,
            den = mapping.den,
            side = order.side,
            "resolved combo instrument"
        );
        Ok((method, build_combo_order_params(&order, &request.client_order_id, grid.0, grid.1)))
    }

    /// A BRAND-NEW combo book is born `"inactive"` and flips `"active"` moments later — observed
    /// live on testnet (`state_timestamp` ~574 ms after `creation_timestamp`; the gate-flip smoke
    /// hit exactly this race). Rejecting on the first read would terminally reject the FIRST
    /// order on every newly registered combo structure, so poll `public/get_combo_details`
    /// briefly (bounded: 5 × 300 ms on the per-ORDER submit path, which is already synchronous
    /// venue I/O) before giving up with the [`ComboMapError::Inactive`] reject. An already-active
    /// combo (the idempotent-create common case) returns immediately with zero extra calls.
    fn wait_combo_active(
        &self,
        combo: crate::combo::VenueCombo,
    ) -> Result<crate::combo::VenueCombo, String> {
        if combo.is_active() {
            return Ok(combo);
        }
        for _ in 0..5 {
            std::thread::sleep(std::time::Duration::from_millis(300));
            let Ok(details) =
                self.private_result("public/get_combo_details", &json!({"combo_id": combo.id}))
            else {
                continue;
            };
            if let Some(re) = parse_combo(&details).filter(crate::combo::VenueCombo::is_active) {
                tracing::info!(
                    target: "vike_deribit::combo",
                    combo_id = %re.id,
                    "fresh combo book activated"
                );
                return Ok(re);
            }
        }
        Err(ComboMapError::Inactive(combo.id).to_string())
    }

    /// The shared submit tail: fire `method`/`params` at the order socket and fold the JSON-RPC
    /// reply into exactly ONE terminal-or-accepted event. Single-leg and combo submits share this
    /// so the emitter split, the order-id bookkeeping and the audit-T1 ambiguous-timeout recovery
    /// are identical on both paths (an order never silently vanishes on either).
    fn dispatch_submit(&self, request: &OrderRequest, method: &str, params: &Value) -> Event {
        // ⚠ The `let` is LOAD-BEARING, not style. A `match self.transport.lock().unwrap().call(…)`
        // keeps the `MutexGuard` — a temporary in the scrutinee — alive until the END of the match,
        // so any arm that touches `self.transport` again re-locks a mutex this thread already
        // holds: `std::sync::Mutex` is not reentrant, and on Linux that is a hard self-deadlock of
        // the exec thread. The audit-T1 arm below has ALWAYS re-entered (`query_order_by_label` →
        // `private_result` → `self.transport.lock()`); it was reachable only on a WS response
        // timeout, which is why nothing had hit it. Widening the ambiguous door to every read-half
        // failure would have made that path ordinary. Binding to a `let` drops the guard at the
        // semicolon.
        let outcome = self.transport.lock().unwrap().call(method, params);
        match outcome {
            Ok(resp) => {
                let (_rid, result, error) = parse_response(&resp);
                if let Some(err) = error.filter(|e| !e.is_null()) {
                    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                    let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("");
                    tracing::warn!(target: "vike_deribit::client", code, msg, "submit rejected");
                    return Event::OrderRejected(OrderRejected {
                        client_order_id: request.client_order_id.clone(),
                        reason: msg.to_string().into(),
                        ts: request.ts,
                    });
                }
                let order_id = result
                    .as_ref()
                    .and_then(|r| r.get("order"))
                    .and_then(|o| o.get("order_id"))
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                if !order_id.is_empty() {
                    self.order_ids
                        .lock()
                        .unwrap()
                        .insert(request.client_order_id.clone(), order_id.clone());
                }
                Event::OrderAccepted(OrderAccepted {
                    client_order_id: request.client_order_id.clone(),
                    venue_order_id: Some(order_id.into()),
                    ts: request.ts,
                })
            }
            Err(exc) => {
                // ⚠⚠ ORDER SAFETY. The socket may be unusable, and this transport never re-dials
                // itself — so heal it HERE, before the audit-T1 re-query below needs a live socket
                // to ask over (a re-query on the dead socket just fails and we are back to
                // guessing). **A re-dial is NOT a re-send**: `connect()` closes, dials and
                // authenticates, and the order frame is never written a second time. Deribit does
                // not dedupe on `label`, so a resent `private/buy` is a SECOND REAL ORDER; nothing
                // on this path may resend, whatever the socket did.
                if is_dead_socket_error(&exc) {
                    self.redial_order_socket(&request.client_order_id, &exc);
                }
                if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS {
                    // audit T1: the request frame was already on the wire, so Deribit may have
                    // accepted it — re-query by label instead of emitting a false terminal reject
                    // (which would strand a phantom). Since 2026-08-25 this covers EVERY read-half
                    // failure, not only the response timeout.
                    self.resolve_ambiguous_submit(request)
                } else {
                    // Definite: the send never reached the wire, so no order exists to strand.
                    tracing::warn!(target: "vike_deribit::client", code = exc.code, msg = %exc.msg, "submit rejected");
                    Event::OrderRejected(OrderRejected {
                        client_order_id: request.client_order_id.clone(),
                        reason: exc.msg.into(),
                        ts: request.ts,
                    })
                }
            }
        }
    }

    /// Re-open the order socket after a transport failure. Best-effort and LOUD: a failed re-dial
    /// leaves the re-query to fail too, which [`vike_bridge_core::resolve_ambiguous_submit`] already
    /// turns into an optimistic (never terminal) accept.
    ///
    /// ⚠ It re-dials and NOTHING ELSE — see [`Self::dispatch_submit`] for why an order is never
    /// re-sent. Logged per event rather than latched behind a "log once" flag the way
    /// `recon_client.rs` does: this fires per ORDER, at an order boundary, which is exactly the
    /// granularity the workspace's logging rule asks to instrument.
    fn redial_order_socket(&self, coid: &str, cause: &VenueApiError) {
        tracing::warn!(
            target: "vike_deribit::client",
            coid,
            code = cause.code,
            msg = %cause.msg,
            "order-WS unusable — re-dialing so the status re-query has a live socket (the order is NEVER re-sent)"
        );
        if let Err(e) = self.transport.lock().unwrap().connect() {
            tracing::error!(
                target: "vike_deribit::client",
                coid,
                error = %e,
                "order-WS re-dial failed; the submit resolves on whatever evidence is available"
            );
        }
    }

    /// The audit-T1 tail: re-query the order by its `label` and map the answer through the SHARED
    /// [`vike_bridge_core::resolve_ambiguous_submit`] (consumed verbatim — order landed → adopt,
    /// venue confirms absent → the true reject, re-query inconclusive → optimistic accept, never a
    /// false terminal).
    ///
    /// The "venue confirms absent" arm rests on `get_order_state_by_label` reporting TERMINAL
    /// orders too, which was MEASURED on testnet rather than assumed — the evidence lives once, on
    /// [`Self::query_order_by_label`].
    ///
    /// The one thing done BESIDE that mapping: an adopted order's venue id is recorded in
    /// `order_ids`. Without it the order is UNCANCELLABLE — `cancel_order` resolves coid → venue
    /// order id through that map and returns `Ok(())` on a miss, so an order adopted here would
    /// silently ignore every cancel. That was survivable while this path only fired on a rare
    /// response timeout; it is not, now that every read-half failure lands here.
    fn resolve_ambiguous_submit(&self, request: &OrderRequest) -> Event {
        let coid = &request.client_order_id;
        let query = self.query_order_by_label(coid);
        if let Ok(Some(order_id)) = &query {
            self.order_ids.lock().unwrap().insert(coid.clone(), order_id.clone());
        }
        vike_bridge_core::resolve_ambiguous_submit(coid, request.ts, query)
    }
}

impl VenueRest for DeribitRest {
    /// `[OrderSubmitted, OrderAccepted|OrderRejected]` — the emitter split: Rust emits
    /// `OrderSubmitted` synchronously, the venue's reply decides the second event, and there is no
    /// path out of here that emits neither.
    ///
    /// A request with non-empty `combo_legs` routes through the combo book
    /// ([`Self::resolve_combo_order`]) instead of the single-leg `build_order_params`; a combo that
    /// cannot be registered or reconciled yields a terminal `OrderRejected` carrying the reason.
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // tif step-2 gate: a TIF deribit cannot express (Gtd — no good-till-DATE, only
        // good_til_day) is a LOUD terminal reject; the wire is never touched, never a silent
        // default (pinned in `tif_table_tests` below). Applies to combo limits too — the combo
        // params carry no TIF either.
        if let Some(reject) = vike_model::venues::venue_tif::deny_unsupported_tif(VENUE, request) {
            events.push(reject);
            return events;
        }
        let (method, params) = if request.combo_legs.is_empty() {
            let method = if request.side > 0 { "private/buy" } else { "private/sell" };
            (method, self.build_order_params(request))
        } else {
            match self.resolve_combo_order(request) {
                Ok(resolved) => resolved,
                Err(reason) => {
                    tracing::warn!(target: "vike_deribit::combo", coid = %request.client_order_id, %reason, "combo submit rejected");
                    events.push(Event::OrderRejected(OrderRejected {
                        client_order_id: request.client_order_id.clone(),
                        reason: reason.into(),
                        ts: request.ts,
                    }));
                    return events;
                }
            }
        };
        events.push(self.dispatch_submit(request, method, &params));
        events
    }

    /// Idempotent cancel. On a DEAD SOCKET the cancel is re-dialed and re-sent ONCE — and that is
    /// a deliberately different answer from the submit path's "never re-send", justified by
    /// idempotency rather than by the failure being any less ambiguous:
    ///
    /// * `private/cancel` names one immutable venue `order_id`, so a re-send can only ever act on
    ///   THAT order. It cannot create anything, and it cannot reach a second order the way a
    ///   resent `private/buy` would (Deribit does not dedupe on `label`).
    /// * If the first attempt already took effect, the retry comes back in [`NOT_FOUND`], which
    ///   this method has always swallowed as success. So "already cancelled" and "cancelled now"
    ///   are the same outcome by construction.
    ///
    /// The alternative — heal the socket but report the failure — leaves a live order un-cancelled
    /// on a venue the process can now reach again, which is the wrong direction for a call whose
    /// whole purpose is REMOVING risk. A cancel that genuinely fails still surfaces: `LiveRestClient`
    /// turns it into a NON-terminal `OrderCancelRejected` advisory, never a terminal.
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError> {
        let Some(order_id) = self.order_ids.lock().unwrap().get(client_order_id).cloned() else {
            return Ok(()); // nothing live to cancel
        };
        match self.cancel_once(&order_id, client_order_id) {
            Err(exc) if is_dead_socket_error(&exc) => {
                self.redial_order_socket(client_order_id, &exc);
                // Re-dial failed → this second attempt fails the same way and the ORIGINAL-shaped
                // error surfaces; no separate branch needed.
                self.cancel_once(&order_id, client_order_id)
            }
            other => other,
        }
    }
}

impl DeribitRest {
    /// One `private/cancel` round trip, with the venue's own already-gone codes swallowed as
    /// success ("unknown ≠ rejection"). Split out so [`VenueRest::cancel_order`] can run it twice
    /// across a re-dial without duplicating that swallow.
    fn cancel_once(&self, order_id: &str, client_order_id: &str) -> Result<(), VenueApiError> {
        match self.private_result("private/cancel", &json!({ "order_id": order_id })) {
            Ok(_) => Ok(()),
            Err(exc) if NOT_FOUND.contains(&exc.code) => {
                tracing::debug!(target: "vike_deribit::client", client_order_id, "cancel: order already gone");
                Ok(())
            }
            Err(exc) => Err(exc),
        }
    }
}

#[cfg(test)]
mod instrument_class_tests {
    //! `parse_deribit_option_instruments` names the class it parsed
    //! (`docs/decisions/0061-an-instrument-names-its-kind.md`), and names it from the venue's own
    //! `kind` gate rather than from the instrument name.
    use serde_json::json;
    use vike_model::AssetClass;

    use super::parse_deribit_option_instruments;

    #[test]
    fn an_option_row_names_itself_an_option() {
        let payload = json!({"result": [{
            "instrument_name": "BTC-1JAN27-100000-C",
            "kind": "option",
            "tick_size": 0.0005,
            "min_trade_amount": 0.1,
            "contract_size": 1.0,
        }]});
        let out = parse_deribit_option_instruments(&payload);
        let inst = &out["BTC-1JAN27-100000-C"];
        assert_eq!(inst.properties.asset_class, Some(AssetClass::Option));
    }

    /// The gate, from the class's side: a `kind` this parser does not admit produces NO row at
    /// all, so there is no path by which a non-option is tagged `Option`. Both rows carry an
    /// option-SHAPED name, which is the point — the name is not what decides.
    #[test]
    fn a_non_option_kind_yields_no_row_to_tag() {
        let payload = json!({"result": [
            {"instrument_name": "BTC-PERPETUAL", "kind": "future", "tick_size": 0.5,
             "min_trade_amount": 10.0, "contract_size": 10.0},
            {"instrument_name": "BTC-1JAN27-100000-C", "kind": "option_combo", "tick_size": 0.0005,
             "min_trade_amount": 0.1, "contract_size": 1.0},
        ]});
        assert!(parse_deribit_option_instruments(&payload).is_empty());
    }
}

#[path = "properties_recorder_tests.rs"]
#[cfg(test)]
mod properties_recorder_tests;

#[path = "tif_table_tests.rs"]
#[cfg(test)]
mod tif_table_tests;

#[path = "tick_scheme_order_format_tests.rs"]
#[cfg(test)]
mod tick_scheme_order_format_tests;
