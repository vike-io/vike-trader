//! VenueRestBinanceSpot — signed REST submit/cancel. Exact port of
//! `exec/binance/client.py` + the shared `exec/crypto_client.py` flow.
//!
//! The spot lane reads balance and open orders through `crate::reconcile`'s
//! `ReconcileClientBinance`; this client holds no reconcile read.
//!
//! The WS executionReport is the SOLE source of fills; the REST POST/DELETE response is
//! ACK-ONLY. `submit_order` returns [OrderSubmitted, OrderAccepted|OrderRejected] for the
//! session to pump into the core ingest (Python published the same pair to its bus).
//! qty/price are formatted to the symbol's step/tick as decimal strings to dodge
//! -1111 BAD_PRECISION.

use vike_bridge_core::confirm::ConfirmByOrderClientId;
use vike_bridge_core::rest::{ExecutionClientRest, VenueRest, amend_answer_time};
use vike_model::events::{Event, OrderCanceled, OrderModified, OrderModifyRejected, RejectCause};
use vike_model::{OrderRequest, OrderType, SymbolProperties, TimeInForce};

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::json_number;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

// The pure, byte-identical-between-binance/aster mappers live once in `crate::family::execution::order_map`
// (mappers rung): `json_id` (used internally below), and `parse_symbol_properties` re-exported so
// `vike_binance::execution::rest::spot::parse_symbol_properties` stays the same public path.
use crate::family::execution::order_map::REPLACE_CANCEL_ID_PREFIX;
use crate::family::execution::order_map::json_id;
pub use crate::family::execution::order_map::parse_symbol_properties;
use crate::family::execution::order_word::OrderWord;
use crate::family::execution::signed_rest::{FamilyRestPaths, FamilySignedRest};

// Homed at the crate root since the exec/feeds seam (this module is `exec`-gated, the venue id is
// not: `crate::history`'s keyless kline REST names it too); imported here like any other consumer —
// no `crate::execution::rest::spot::VENUE` re-export, per the no-pub-use-shims-on-a-move convention (root
// CLAUDE.md). The value is unchanged, so every signed order this module builds is byte-identical.
use crate::VENUE;
pub const PATH_ORDER: &str = "/api/v3/order";
/// Binance spot's atomic cancel-then-place endpoint, the only price-modify spot has (see
/// [`VenueRestBinanceSpot::modify_order`]).
pub const PATH_CANCEL_REPLACE: &str = "/api/v3/order/cancelReplace";
/// `-2021` ORDER_CANCEL_REPLACE_PARTIALLY_FAILED: under `STOP_ON_FAILURE` the cancel LANDED and the
/// replacement was refused (the venue evaluates filters after the cancel).
const CANCEL_REPLACE_PARTIAL: i64 = -2021;
/// `-2022` ORDER_CANCEL_REPLACE_FAILED: the cancel failed, so nothing was placed.
const CANCEL_REPLACE_FAILED: i64 = -2022;
pub const PATH_OPEN_ORDERS: &str = "/api/v3/openOrders";
pub const PATH_ACCOUNT: &str = "/api/v3/account";
pub const PATH_TICKER: &str = "/api/v3/ticker/price";
pub const PATH_TIME: &str = "/api/v3/time";
pub const PATH_EXCHANGE_INFO: &str = "/api/v3/exchangeInfo";

/// The account's live spot maker/taker commission rates, as **fractions** (e.g. `0.001` = 10 bps).
/// Binance returns these under `commissionRates{maker,taker}` in the SAME `/api/v3/account` body
/// the reconcile client's balance read fetches — they were previously dropped (only `balances`
/// was read). The fee model (steal-list #5) surfaces them so a live start can prefer the account's
/// actual rates over the static [`vike_venues::fee_schedule_for`] default.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BinanceCommissionRates {
    pub maker: f64,
    pub taker: f64,
}

/// Parse `commissionRates{maker,taker}` (fractions) out of a `/api/v3/account` response body.
/// `None` when the object is absent (some account/permission shapes omit it) or a rate is
/// unparseable — the caller then keeps the static default (fail-soft). Pure + fixture-tested.
pub fn parse_commission_rates(account: &serde_json::Value) -> Option<BinanceCommissionRates> {
    let cr = account.get("commissionRates")?;
    Some(BinanceCommissionRates {
        maker: cr.get("maker").and_then(json_number)?,
        taker: cr.get("taker").and_then(json_number)?,
    })
}

/// **WHICH ACCOUNT this key belongs to** — `uid` out of a `/api/v3/account` response body.
///
/// ⚠ **MEASURED, not read off the venue's documentation.** On 2026-09-20 a read-only probe
/// (`crates/bridges/binance/tests/binance_smokes/binance_identity_probe.rs`) asked the live demo endpoint and the
/// sanitizer's audit trail came back naming `/uid`. That matters because this tree has an incident
/// on file from writing a venue parser out of public docs — `vike_secrets::key_permissions`'
/// module doc carries it as *the `canWithdraw` trap* — and the binance adapter's `book_identity`
/// declaration (`crate::adapter`) had said only that *an authenticated call* could answer, naming no field, because nothing here
/// had ever looked.
///
/// # Why this costs NOTHING to call
///
/// It parses a body the live path already holds. `vike_tradehub_core::arming`'s `resolve_fee_schedule` makes
/// one blocking `/api/v3/account` read at every live start (through `fetch_fee_rates`, whose spot
/// arm is parameterless), and `vike_tradehub_core::startup`'s `authed_read_probes` reaches the same endpoint
/// at every start through `fetch_balance` — which keeps one USDT figure and then discards even that
/// (`.map(|_| ())`). So recording the account this key trades needs no extra request, no extra
/// weight and no new permission: it needs the body to stop being thrown away.
///
/// # What it answers with
///
/// The uid as a STRING, whatever JSON type the venue used. It is an identifier to compare and to
/// store, never a number to do arithmetic on, and `vike_secrets::AccountRow::venue_account_id` is a
/// `TEXT` column — so parsing it into an integer here would be a conversion with a lossy edge and no
/// consumer.
///
/// `None` when the field is absent or is neither a string nor a number. Fail-soft on purpose: a
/// missing uid must degrade to *not yet known* — the state every migrated row already carries —
/// rather than fail a start over a bookkeeping field.
#[must_use]
pub fn parse_account_identity(account: &serde_json::Value) -> Option<String> {
    match account.get("uid")? {
        serde_json::Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// A percent trail as spot's `trailingDelta`: whole BASIS POINTS (the trailing-stop FAQ's "BIPS", 1 =
/// 0.01%), `percent` x 100, or `None` when that is not a whole number of at least one (a rounded delta
/// would move the stop). Whether it fits the symbol's `TRAILING_DELTA` filter (its above and below
/// bounds, by side and type) is the venue's answer: this bridge does not cache that filter (the symbol
/// grid it keeps, `vike_model::SymbolProperties`, has no slot for it).
pub(crate) fn trailing_delta_bips(percent: f64) -> Option<u32> {
    let bips = percent * 100.0;
    let whole = bips.round();
    // `percent` is below 100 (`vike_model::order_terms_refusal`), so `whole` fits; the tolerance absorbs
    // the float image of a two-decimal percent (1.23 x 100 = 123.00000000000001)
    ((bips - whole).abs() < 1e-6 && (1.0..1e9).contains(&whole)).then_some(whole as u32)
}

/// The REST half of the live spot client (ACK-only); fills come from the user-data WS
/// (slice 2). Signer/transport are seams: offline tests stub the transport with canned
/// JSON; the demo smoke uses `UreqTransport`.
pub struct VenueRestBinanceSpot<S: Signer, T: RestTransport> {
    pub signer: S,
    pub transport: T,
    pub base_url: String,
    pub symbol: String,
    pub properties: SymbolProperties,
    pub base_asset: String,
    /// Binance Broker/Link attribution id (unified cross-venue attribution, task 6), resolved ONCE
    /// at start from `BINANCE_BROKER_CODE`/`BINANCE_BUILDER_CODE` via `attribution_code_from` (see
    /// `vike_secrets::credentials`). Applied at the venue edge to every order-client-id string
    /// this client sends Binance (`newClientOrderId` on submit, `origClientOrderId` on cancel/query)
    /// via [`crate::family::execution::order_map::binance_broker_order_client_id`] — `None` (unset, or a history/resync-
    /// only client that never identifies an order by order client id) reproduces the pre-task-6 wire body
    /// byte-for-byte.
    pub link_id: Option<String>,
}

impl<S: Signer, T: RestTransport> VenueRestBinanceSpot<S, T> {
    /// `build_order_params` hook — pure, golden-gated. Delegates to the shared, byte-identical
    /// [`crate::family::execution::order_map::build_spot_order_params`] (mappers rung), passing this client's
    /// `symbol`/`properties`; `newOrderRespType=ACK` and the response's fills[] is deliberately
    /// ignored (WS is the sole fill source).
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        let mut params = crate::family::execution::order_map::build_spot_order_params(
            request,
            &self.symbol,
            &self.properties,
            VENUE,
            self.link_id.as_deref(),
        );
        // POST-ONLY on spot is the order TYPE `LIMIT_MAKER`, and the venue refuses a `timeInForce`
        // beside it (`-1106`), so the family builder's `type=LIMIT` + `timeInForce=<row wire>` pair
        // is swapped here, in this venue's own client (the family builder is shared with aster,
        // whose row never maps post-only). The row's wire string names the type.
        if request.order_type == OrderType::Limit && request.time_in_force == TimeInForce::PostOnly
        {
            params.retain(|(k, _)| *k != "timeInForce");
            if let Some(ty) = params.iter_mut().find(|(k, _)| *k == "type") {
                ty.1 = "LIMIT_MAKER".to_string();
            }
        }
        // A spot STOP is the type `STOP_LOSS` (a market order once `stopPrice` trades) and a stop-limit
        // `STOP_LOSS_LIMIT` (`price`, `timeInForce`, `stopPrice`); a take-profit is `TAKE_PROFIT`
        // (`stopPrice`) or, when it states a limit, `TAKE_PROFIT_LIMIT` (the same three as the
        // stop-limit). The family builder's `STOP`/`STOP_LIMIT` words are no spot types and it sends no
        // `stopPrice`, so all four are built here, in this venue's own client (aster's spot is not ours to
        // change). `GTC` is the one lifetime sent: `crate::execution::rest::perp::deny_unbuilt_kind` refuses a limit kind
        // asking another before this.
        let trigger_wire = match (&request.order_type, request.price) {
            (OrderType::Stop, _) => Some(("STOP_LOSS", false)),
            (OrderType::StopLimit, _) => Some(("STOP_LOSS_LIMIT", true)),
            (OrderType::TakeProfit, None) => Some(("TAKE_PROFIT", false)),
            (OrderType::TakeProfit, Some(_)) => Some(("TAKE_PROFIT_LIMIT", true)),
            _ => None,
        };
        if let Some((word, limited)) = trigger_wire {
            if let Some(ty) = params.iter_mut().find(|(k, _)| *k == "type") {
                ty.1 = word.to_string();
            }
            let tick = self.properties.tick_size;
            if limited {
                params.push(("timeInForce", "GTC".to_string()));
                params.push(("price", format_to_step_f(request.price.unwrap_or(0.0), tick)));
            }
            params
                .push(("stopPrice", format_to_step_f(request.trigger_price.unwrap_or(0.0), tick)));
        }
        // A PERCENT trailing stop carries `trailingDelta` ([`trailing_delta_bips`]) and fires as a market
        // order on that retracement from the extreme it tracks. With no activation it is `STOP_LOSS` and
        // tracks from the next trade. With one it is `TAKE_PROFIT` with the level as `stopPrice`: the
        // trailing-stop FAQ's side table arms a SELL `TAKE_PROFIT` once the price reaches `stopPrice` from
        // below (a BUY from above), which is vike's activation
        // (`crates/vike-model/src/orders/fill_trigger.rs`'s `trailing_activation_reached`), while a trailing
        // `STOP_LOSS`'s `stopPrice` arms on the OTHER side (a SELL once the price falls to it).
        // `crate::execution::rest::perp::deny_unbuilt_kind` refuses a price trail and a percent that is no whole bips first.
        if request.order_type == OrderType::Trailing {
            let word = if request.trigger_price.is_some() { "TAKE_PROFIT" } else { "STOP_LOSS" };
            if let Some(ty) = params.iter_mut().find(|(k, _)| *k == "type") {
                ty.1 = word.to_string();
            }
            if let Some(level) = request.trigger_price {
                params.push(("stopPrice", format_to_step_f(level, self.properties.tick_size)));
            }
            if let Some(bips) = request.trail_percent.and_then(trailing_delta_bips) {
                params.push(("trailingDelta", bips.to_string()));
            }
        }
        // An iceberg shows `icebergQty` of the order, on the quantity grid; the lane gate
        // (`crate::execution::rest::perp::deny_unbuilt_kind`) admits one only on a GTC `LIMIT`, `STOP_LOSS_LIMIT` or
        // `TAKE_PROFIT_LIMIT`, the three the venue takes it on.
        if let Some(shown) = request.display_quantity {
            params.push(("icebergQty", format_to_step_f(shown, self.properties.step_size)));
        }
        params
    }

    /// The family's submit ([`crate::family::execution::signed_rest::submit_order`]): Submitted → this lane's
    /// three bridge floors ([`FamilySignedRest::floor`] below) → REST → Accepted|Rejected, a refusal
    /// carrying its post-only cause. The returned events go into the core ingest — never applied
    /// locally.
    pub fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        crate::family::execution::signed_rest::submit_order(self, request)
    }

    /// Fetch the account's live maker/taker commission rates from `/api/v3/account` (the SAME
    /// signed call the reconcile client's balance read makes; the `commissionRates` fields were
    /// previously dropped).
    /// `Ok(None)` = the object is absent/unparseable (fail-soft → static default).
    pub fn fetch_commission_rates(&self) -> Result<Option<BinanceCommissionRates>, VenueApiError> {
        let account =
            self.transport.signed(&self.base_url, PATH_ACCOUNT, "GET", &[], &self.signer)?;
        Ok(parse_commission_rates(&account))
    }

    /// Cancel by order-client-id. "Unknown order" (-2011 with those words,
    /// [`crate::transport::error_codes::cancel_says_order_unknown`]) is swallowed (already gone ≠
    /// failure — the R6 gate's "unknown ≠ rejection" rule); everything else propagates. The family's
    /// [`crate::family::execution::signed_rest::cancel_by_order_client_id`]: `origClientOrderId` re-applies the same broker
    /// prefix submit stamped (task 6) — see [`crate::family::execution::rest_reads::query_order_orderid`]'s doc
    /// for why this can't be sent bare.
    pub fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        crate::family::execution::signed_rest::cancel_by_order_client_id(self, order_client_id)
    }

    /// Spot modify = ATOMIC cancel-replace (`POST /api/v3/order/cancelReplace`), the only way Binance
    /// spot changes a resting order's PRICE (`order/amend/keepPriority` can only reduce quantity).
    /// There is no in-place amend here, so every guard below exists to keep the engine's view of
    /// the order true; none of it is optional.
    ///
    /// * **Same order client id.** The replacement is placed under the SAME order client id as the order it
    ///   replaces (the registry, cancel path and reconcile key on it); the venue order id changes
    ///   and rides the `OrderModified`. The cancel half carries a throwaway id with
    ///   [`REPLACE_CANCEL_ID_PREFIX`] so the stream mapper drops its CANCELED report, and
    ///   `map_history` ignores an older CANCELED row of an order client id that has a newer incarnation.
    /// * **`cancelRestrictions=ONLY_NEW`.** The replacement is a FRESH order of the full quantity,
    ///   so replacing an order that already executed would re-expose the executed lots. The venue
    ///   refuses the cancel of a partially filled order atomically (no fill race), which surfaces
    ///   as `OrderModifyRejected`; a partially filled spot order is cancelled and re-submitted by
    ///   the caller instead. This is also why `vike_venues::amend_semantics("binance")`'s perp
    ///   `InPlaceTotal` row stays sound for spot: a spot amend never sees `filled_qty > 0`.
    /// * **`STOP_ON_FAILURE`.** A failed cancel never places the replacement. Venue filters are
    ///   evaluated AFTER the cancel, so a refused replacement (`-2021`) may leave the order
    ///   cancelled: [`Self::settle_replace`] asks the venue what became of it rather than guess.
    /// * LIMIT orders only (the spot builder emits no stop price); anything else is an explicit
    ///   `OrderModifyRejected`, never a silent no-op.
    ///
    /// Always answers with at least one event.
    pub fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        let order_client_id = order.order_client_id.clone();
        let reject = |reason: String, refusal: vike_model::OrderRefusalId| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order_client_id.clone(),
                reason: reason.into(),
                timestamp: amend_answer_time(None),
                refusal_id: vike_model::events::refusal_id_of(refusal.word()),
            })]
        };
        if order.order_type != OrderType::Limit {
            return reject(
                "binance spot modify supports LIMIT orders only (cancel and re-submit instead)"
                    .to_string(),
                vike_model::OrderRefusalId::VenueCapability,
            );
        }
        let projected = vike_model::modified_order(order, new_qty, new_price, None);
        let price_ok = projected.price.is_some_and(|p| p.is_finite() && p > 0.0);
        if !(projected.qty.is_finite() && projected.qty > 0.0) || !price_ok {
            return reject(
                "binance spot modify needs a positive finite quantity and price".into(),
                vike_model::OrderRefusalId::NotSent,
            );
        }
        let orig = crate::family::execution::order_map::binance_broker_order_client_id(
            self.link_id.as_deref(),
            &order_client_id,
        );
        static REPLACE_SEQUENCE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let cancel_id = format!(
            "{REPLACE_CANCEL_ID_PREFIX}{}-{}",
            vike_model::now_ms(),
            REPLACE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let mut params = self.build_order_params(&projected);
        params.push(("cancelReplaceMode", "STOP_ON_FAILURE".to_string()));
        params.push(("cancelRestrictions", "ONLY_NEW".to_string()));
        params.push(("cancelOrigClientOrderId", orig.clone()));
        params.push(("cancelNewClientOrderId", cancel_id));
        match self.transport.signed(
            &self.base_url,
            PATH_CANCEL_REPLACE,
            "POST",
            &params,
            &self.signer,
        ) {
            Ok(response) => {
                let void = response
                    .get("newOrderResponse")
                    .and_then(|n| n.get("orderId"))
                    .or_else(|| response.get("orderId"))
                    .map(json_id)
                    .unwrap_or_default();
                vec![Event::OrderModified(OrderModified {
                    order_client_id: order_client_id.clone(),
                    venue_order_id: Some(void.into()),
                    new_qty,
                    new_price,
                    new_trigger_price: None,
                    // the venue's own stamp of the replacement; absent, the time the reply was received
                    timestamp: amend_answer_time(
                        response
                            .get("newOrderResponse")
                            .unwrap_or(&response)
                            .get("transactTime")
                            .and_then(|t| t.as_i64()),
                    ),
                })]
            }
            Err(exc) => {
                tracing::warn!(
                    target: "vike_binance::execution::rest::spot",
                    code = exc.code,
                    msg = %exc.msg,
                    "cancel-replace modify failed"
                );
                if exc.code == CANCEL_REPLACE_PARTIAL
                    || exc.code == CANCEL_REPLACE_FAILED
                    || crate::transport::error_codes::TAXONOMY.submit_outcome_unknown(exc.code)
                {
                    self.settle_replace(
                        &order_client_id,
                        &orig,
                        &projected,
                        new_qty,
                        new_price,
                        &exc,
                    )
                } else {
                    // Refused before (or instead of) the cancel: the order is untouched. Its id by what it was.
                    let refusal = exc.refusal();
                    reject(exc.msg, refusal)
                }
            }
        }
    }

    /// After a cancel-replace whose outcome is not a clean success (`-2021` partial failure,
    /// `-2022` failure, or a timeout / status-unknown code that may have gone through), ask the venue what the order client id is
    /// NOW and report that: the error code alone cannot say whether the cancel half landed (the
    /// transport keeps only `{code, msg}`, not the response's `data`).
    ///
    /// * live at the requested terms: `OrderModified` (the replace did go through);
    /// * live at other terms: `OrderModifyRejected` (untouched);
    /// * cancelled / expired / rejected / unknown: `OrderCanceled` (the cancel landed, the
    ///   replacement did not; the order is GONE and the engine must say so, because the stream's
    ///   report of that cancel is dropped);
    /// * filled or any other status: `OrderModifyRejected` (the fill events own the terminal);
    /// * query failed: `OrderModifyRejected` naming the doubt; the gap-sentinel resync that
    ///   `run_loop` arms after every modify replays the true state.
    fn settle_replace(
        &self,
        order_client_id: &str,
        orig: &str,
        projected: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        cause: &VenueApiError,
    ) -> Vec<Event> {
        // An order read back untouched (or past amending) keeps the cause's own outcome: the venue's refusal
        // (`-2021`/`-2022`), or OUTCOME UNKNOWN for a cause that left the write open, since one read cannot prove a
        // replace in flight will not land; a read that settles nothing is OUTCOME UNKNOWN (decision 0303).
        let reject = |reason: String, refusal: vike_model::OrderRefusalId| {
            vec![Event::OrderModifyRejected(OrderModifyRejected {
                order_client_id: order_client_id.to_string(),
                reason: reason.into(),
                timestamp: amend_answer_time(None),
                refusal_id: vike_model::events::refusal_id_of(refusal.word()),
            })]
        };
        let refused = cause
            .clone()
            .outcome_unknown_when(|code| {
                crate::transport::error_codes::TAXONOMY.submit_outcome_unknown(code)
            })
            .refusal();
        let unknown = vike_model::OrderRefusalId::OutcomeUnknown;
        // stamped as an amend's answer is: the order read back's `updateTime`, else when it was read
        let gone = |why: &str, stated: Option<i64>| {
            vec![Event::OrderCanceled(OrderCanceled {
                order_client_id: order_client_id.to_string(),
                reason: format!("cancel-replace: {why} ({})", cause.msg).into(),
                timestamp: amend_answer_time(stated),
                post_only_would_cross: false,
                refusal_id: None,
            })]
        };
        let params: Vec<(&str, String)> =
            vec![("symbol", self.symbol.clone()), ("origClientOrderId", orig.to_string())];
        let body = match self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER,
            "GET",
            &params,
            &self.signer,
        ) {
            Ok(b) => b,
            // the venue has no such order any more.
            Err(e) if crate::transport::error_codes::says_order_unknown(&e) => {
                return gone("the order is gone at the venue", None);
            }
            Err(e) => {
                return reject(
                    format!(
                        "{} (order state unknown, status query failed: {}; resync pending)",
                        cause.msg, e.msg
                    ),
                    unknown,
                );
            }
        };
        match OrderWord::parse(body.get("status").and_then(|v| v.as_str()).unwrap_or("")) {
            OrderWord::New | OrderWord::PartiallyFilled => {
                let tick = self.properties.tick_size.max(f64::MIN_POSITIVE);
                let step = self.properties.step_size.max(f64::MIN_POSITIVE);
                let price = body.get("price").and_then(json_number);
                let qty = body.get("origQty").and_then(json_number);
                let replaced =
                    price.zip(projected.price).is_some_and(|(a, b)| (a - b).abs() < tick)
                        && qty.is_some_and(|q| (q - projected.qty).abs() < step);
                if replaced {
                    vec![Event::OrderModified(OrderModified {
                        order_client_id: order_client_id.to_string(),
                        venue_order_id: body.get("orderId").map(|v| json_id(v).into()),
                        new_qty,
                        new_price,
                        new_trigger_price: None,
                        timestamp: amend_answer_time(
                            body.get("updateTime").and_then(|t| t.as_i64()),
                        ),
                    })]
                } else {
                    reject(cause.msg.clone(), refused)
                }
            }
            OrderWord::Canceled | OrderWord::Expired | OrderWord::Rejected => gone(
                "the cancel landed, the replacement did not",
                body.get("updateTime").and_then(|t| t.as_i64()),
            ),
            OrderWord::LiveOther | OrderWord::Filled | OrderWord::PendingCancel => {
                reject(cause.msg.clone(), refused)
            }
            // A status this bridge cannot read settles nothing.
            OrderWord::Unknown => reject(cause.msg.clone(), unknown),
        }
    }

    /// GET /api/v3/time → server-minus-local offset, for a smoke's `Signer::set_offset_ms`. The body is
    /// [`crate::family::execution::rest_reads::server_time_offset`]; a production client reads it through
    /// `crate::family::transport`.
    pub fn server_time_offset(&self, local_now_ms: i64) -> Result<i64, VenueApiError> {
        crate::family::execution::rest_reads::server_time_offset(
            &self.transport,
            &self.base_url,
            PATH_TIME,
            local_now_ms,
        )
    }
}

/// Binance spot's parts and facts for the family's signed REST bodies: `/api/v3` (`myTrades` for the
/// fills), the broker link id on every order client id it sends or asks about (task 6: the venue
/// stores the PREFIXED id, so a re-query or a cancel by the bare one would read "unknown"), this lane's
/// three bridge floors, the post-only cause of a refusal, and its own log target.
impl<S: Signer, T: RestTransport> FamilySignedRest for VenueRestBinanceSpot<S, T> {
    const VENUE: &'static str = VENUE;
    const PATHS: FamilyRestPaths = FamilyRestPaths {
        order: PATH_ORDER,
        all_orders: "/api/v3/allOrders",
        trades: "/api/v3/myTrades",
        leverage: "",
        batch: "",
    };

    fn transport(&self) -> &dyn RestTransport {
        &self.transport
    }
    fn signer(&self) -> &dyn Signer {
        &self.signer
    }
    fn base_url(&self) -> &str {
        &self.base_url
    }
    fn symbol(&self) -> &str {
        &self.symbol
    }
    fn link_id(&self) -> Option<&str> {
        self.link_id.as_deref()
    }
    fn order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)> {
        self.build_order_params(request)
    }
    /// tif step-2 gate: a TIF binance cannot express (GTD/Day) is a LOUD terminal reject — the wire
    /// is never touched, never a silent GTC (pinned in `tif_gate_tests`). Same discipline for a
    /// trigger source spot cannot express (Mark/Index — spot's only price series is last trades):
    /// `deny_unsupported_trigger_by`. And a kind this bridge does not build:
    /// `crate::execution::rest::perp::deny_unbuilt_kind`, one gate for both lanes.
    fn floor(&self, request: &OrderRequest) -> Option<Event> {
        vike_venues::venue_time_in_force::deny_unsupported_time_in_force(VENUE, request)
            .or_else(|| vike_bridge_core::trigger::deny_unsupported_trigger_by(VENUE, request))
            .or_else(|| crate::execution::rest::perp::deny_unbuilt_kind(request, true))
    }
    fn refusal(
        &self,
        request: &OrderRequest,
        code: i64,
        msg: &str,
    ) -> (Option<RejectCause>, String) {
        (crate::transport::error_codes::post_only_cause(request, code, msg), msg.to_string())
    }
    fn log_refused(&self, order_client_id: &str, exc: &VenueApiError) {
        tracing::warn!(target: "vike_binance::execution::rest::spot", order_client_id, code = exc.code, msg = %exc.msg, "submit rejected");
    }
}

impl<S: Signer + Send, T: RestTransport + Send> ConfirmByOrderClientId
    for VenueRestBinanceSpot<S, T>
{
    const LOOKUP_WINDOW: Option<std::time::Duration> = None; // none stated by the venue

    fn confirm_when(&self, order_client_id: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        crate::family::execution::signed_rest::confirm_when(
            self,
            order_client_id,
            ts,
            absent_rejects,
        )
    }
}

impl<S: Signer + Send, T: RestTransport + Send> VenueRest for VenueRestBinanceSpot<S, T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        VenueRestBinanceSpot::submit_order(self, request)
    }
    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        VenueRestBinanceSpot::cancel_order(self, order_client_id)
    }
    /// ACTIVELY confirm one order by its client id (the by-order-client-id status GET): found -> `OrderAccepted`
    /// (+ the terminal when the venue already closed it), absent -> the "venue confirms absent"
    /// reject, a FAILED query -> NOTHING (the watchdog ladder and reconcile decide). This trait door
    /// assumes the submit was fully dispatched; the exec thread's confirm closure goes through
    /// [`ConfirmByOrderClientId::confirm_when`] with the real answer.
    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        self.confirm_when(order_client_id, ts, true)
    }
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        VenueRestBinanceSpot::modify_order(self, order, new_qty, new_price)
    }
}

/// Back-compat name for the slice-2 smoke wiring.
pub type LiveBinanceSpotClient<S, T> = ExecutionClientRest<VenueRestBinanceSpot<S, T>>;

#[cfg(test)]
mod identity_tests {
    use super::*;

    /// **THE MEASURED SHAPE.** The live demo endpoint answered `uid` as a JSON NUMBER on
    /// 2026-09-20, alongside the `commissionRates` object this same body is already read for.
    #[test]
    fn a_numeric_uid_is_read_as_the_account_identity() {
        let body = serde_json::json!({
            "accountType": "SPOT",
            "uid": 1_234_567_890_u64,
            "commissionRates": {"maker": "0.00100000", "taker": "0.00100000"},
        });
        assert_eq!(parse_account_identity(&body).as_deref(), Some("1234567890"));
        // …and the field this body has always been read for is untouched by the new one.
        assert!(parse_commission_rates(&body).is_some(), "the fee read still works");
    }

    /// ⚠ **A STRING uid is read too, and this is not defensive coding.** The probe saw a number on
    /// the SPOT demo endpoint; okx answers its own uid as a string, and a venue is free to widen a
    /// numeric id the day it outgrows 2^53. Accepting both costs one match arm and removes a class
    /// of silent `None` that would read as *this account has no uid*.
    #[test]
    fn a_string_uid_is_read_too() {
        let body = serde_json::json!({ "uid": "  592123  " });
        assert_eq!(
            parse_account_identity(&body).as_deref(),
            Some("592123"),
            "and it is trimmed — an operator-visible id with edge whitespace is one id"
        );
    }

    /// **AN ABSENT OR UNUSABLE uid DEGRADES, it does not fail.** *Not yet known* is the state every
    /// migrated `account` row already carries, and `venue_account_id`'s own doc forbids reading it
    /// as *this account has no book*. A start must not be lost over a bookkeeping field.
    #[test]
    fn an_absent_or_unusable_uid_is_not_yet_known() {
        for body in [
            serde_json::json!({ "accountType": "SPOT" }),
            serde_json::json!({ "uid": serde_json::Value::Null }),
            serde_json::json!({ "uid": "" }),
            serde_json::json!({ "uid": "   " }),
            serde_json::json!({ "uid": {"nested": 1} }),
            serde_json::json!({ "uid": [1, 2] }),
            serde_json::json!({ "uid": true }),
        ] {
            assert_eq!(parse_account_identity(&body), None, "{body}");
        }
    }
}
