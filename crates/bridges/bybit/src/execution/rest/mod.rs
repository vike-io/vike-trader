//! VenueRestBybit — signed V5 LINEAR perp submit/cancel + set-leverage. Exact
//! port of `exec/bybit/perp_client.py` (+ the shared crypto_client flow + spot unwrap).
//!
//! Linear deltas: category=linear, positionIdx=0 (one-way; hedge 1/2 is 5f), reduceOnly
//! as a JSON BOOL, qtyStep filters, set-leverage swallowing 110043 ("leverage not
//! modified").
//!
//! Positions, open orders and balance are read by `crate::reconcile`'s `ReconcileClientBybit`,
//! not here.

pub mod per_symbol;

use indexmap::IndexMap;
use serde_json::{Value, json};

use vike_bridge_core::confirm::{ConfirmByOrderClientId, found_by_state, resolve_confirm};
use vike_bridge_core::rest::VenueRest;
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderRejected, OrderSubmitted};
use vike_model::{AccountTier, Contract, OrderRequest, OrderType, SymbolProperties, TriggerBy};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::catalog::{derivative_contract, derivative_properties};
use crate::execution::event_mapper::map_order;
use crate::history::Category;
use crate::signer::BybitV5Signer;
use crate::transport::client::unwrap_envelope;
use crate::transport::error_codes::{
    cancel_says_gone, modify_failed, server_had_trouble, submit_outcome_unknown,
};
use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::{json_number, json_text};
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};

/// A row's venue order id, or `None` when it names none (absent, `null` or empty).
fn order_id_of(row: &Value) -> Option<String> {
    row.get("orderId").filter(|id| !id.is_null()).map(json_text).filter(|id| !id.is_empty())
}

/// Canonical venue id — the bybit `venue_time_in_force` row key (tif step-2).
pub const VENUE: &str = "bybit";

pub const PATH_ORDER_CREATE: &str = "/v5/order/create";
/// Single-order status query (open + recently-closed) — used by the audit-T1 requery.
pub const PATH_ORDER_REALTIME: &str = "/v5/order/realtime";
/// Recent CLOSED orders + executions — used by the audit-A3 history replay.
pub const PATH_ORDER_HISTORY: &str = "/v5/order/history";
pub const PATH_EXECUTION_LIST: &str = "/v5/execution/list";
pub const PATH_ORDER_CANCEL: &str = "/v5/order/cancel";
pub const PATH_ORDER_AMEND: &str = "/v5/order/amend";
pub const PATH_ORDER_CREATE_BATCH: &str = "/v5/order/create-batch";
/// Bybit v5 linear batch cap: 20 orders per create-batch request.
pub const BATCH_MAX: usize = 20;
pub const PATH_POSITION_LIST: &str = "/v5/position/list";
pub const PATH_SET_LEVERAGE: &str = "/v5/position/set-leverage";
pub const PATH_ACCOUNT: &str = "/v5/account/wallet-balance";
pub const PATH_INSTRUMENTS: &str = "/v5/market/instruments-info";
pub const PATH_TICKERS: &str = "/v5/market/tickers";
/// PUBLIC (keyless) server clock. The startup preflight's clock leg reads it through
/// `crate::adapter`'s `VenueAdapterBybit::server_time_ms` (the venue-adapter contract's clock read,
/// docs/decisions/0096): bybit stamps `X-BAPI-TIMESTAMP` on every private request
/// and rejects one outside [`BybitV5Signer`]'s `recv_window` with `10002`
/// ([`crate::transport::error_codes`]), so this endpoint answers "would our signed orders be rejected now".
///
/// ⚠ The stamp is the v5 envelope's TOP-LEVEL `"time"` — a JSON NUMBER in epoch ms.
/// `result.timeNano` is a nanosecond STRING and `result.timeSecond` a seconds STRING, so a naive
/// `as_i64()` on either reads the wrong unit or nothing. Measured from the CI box on 2026-08-08, both
/// hosts: `{"retCode":0,…,"result":{"timeSecond":"1786218814","timeNano":"1786218814374677738"},
/// "retExtInfo":{},"time":1786218814374}`.
pub const PATH_TIME: &str = "/v5/market/time";

/// The private socket's path on the `user` `ws` base (`wss://stream.bybit.com` + this).
pub const PATH_PRIVATE_WEBSOCKET: &str = "/v5/private";

/// **The hosts one bybit account dials, resolved ONCE from its `venue_url` rows** ([`endpoints`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BybitUrls {
    /// The account is `live` (mainnet). The account's TYPE, never read off a host: the account-mode
    /// rule (`crate::account::modes::Modes::writes_modes`) and the reject wording decide by it.
    pub mainnet: bool,
    /// The signed REST base: `trade` `rest` at the account's type.
    pub rest: String,
    /// The private socket, [`PATH_PRIVATE_WEBSOCKET`] included: `user` `ws` at the account's type.
    pub ws: String,
    /// The keyless instrument listings `crate::instrument::listing` reads (a name's book): `live` `data`
    /// `rest` for EVERY tier, the mainnet lists a demo account has always read.
    pub listings: String,
}

/// **The hosts this venue's private exec/user-data path binds to**, looked up ONCE per start in the
/// account's rows (`vike_venues::venue_urls::VenueUrls::url`) at the type its tier names
/// (`UrlType::of_tier`: `live` is mainnet, every lower tier demo). Bybit has no
/// `x-simulated-trading`-style header — demo is a wholly separate host — so the type is the entire
/// switch, and a `demo` `trade`/`user` lookup never falls back to a live row.
///
/// ⚠ **The account's tier IS the network for this venue** (decision 0095, as amended) —
/// `crate::adapter`'s `VenueAdapterBybit::start` resolves this from `AdapterInputs::tier` ONCE per
/// start; the resolved [`BybitUrls`] is then threaded into every adapter site that needs it (the
/// grid pre-fetch, the exec spawn, the funding poller, the recon client) instead of each spawned
/// thread reading anything for itself, and the start's clock read takes the same answer.
///
/// # Errors
/// The refusal naming the missing row: the account does NOT RUN.
pub fn endpoints(urls: &VenueUrls, tier: AccountTier) -> Result<BybitUrls, UrlRefusal> {
    let ty = UrlType::of_tier(tier);
    Ok(BybitUrls {
        mainnet: ty == UrlType::Live,
        rest: urls.url(ty, UrlChannel::Trade, UrlProtocol::Rest, "")?.to_string(),
        ws: format!(
            "{}{PATH_PRIVATE_WEBSOCKET}",
            urls.url(ty, UrlChannel::User, UrlProtocol::Websocket, "")?
        ),
        listings: urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "")?.to_string(),
    })
}

/// The release seed's [`endpoints`] at `tier`: the rows a fresh box holds, for a test or a smoke.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn seed_endpoints(tier: AccountTier) -> BybitUrls {
    endpoints(&VenueUrls::seed(VENUE), tier).expect("the seed holds every bybit row")
}

#[path = "endpoint_switch_tests.rs"]
#[cfg(test)]
mod endpoint_switch_tests;

const LEVERAGE_NOT_MODIFIED: i64 = 110043; // already at target — benign

/// One row of [`parse_bybit_perp_instruments`]: the grid and the typed contract
/// (`crates/bridges/bybit/src/catalog.rs`'s `derivative_properties`/`derivative_contract`, the same
/// two calls the picker's rows are built from), plus base_asset from baseCoin.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BybitInstrument {
    pub properties: SymbolProperties,
    /// **What this row IS**, read off its own fields — margining from `contractType`, the settle
    /// asset from `settleCoin`, a dated future's expiry from `deliveryTime`. `None` when the row
    /// names no contract this crate can type (a spot row, an unknown `contractType`, no
    /// `settleCoin`). Decided by `crates/bridges/bybit/src/catalog.rs`'s `derivative_contract`,
    /// which the catalog's picker rows call too, so the two planes cannot disagree about it.
    pub contract: Option<Contract>,
    pub base_asset: String,
    /// The venue's published account-leverage CEILING for this symbol —
    /// `leverageFilter.maxLeverage`, a decimal STRING (`"100.00"` for BTCUSDT, verified live
    /// 2026-08-05). `None` when the row carries no `leverageFilter` or an unparseable value.
    ///
    /// Deliberately NOT a [`SymbolProperties`] field, following `vike_hyperliquid`'s explicit
    /// precedent ("`max_leverage` is NOT a `SymbolProperties` field — it stays on the
    /// `InstrumentRef`"): `SymbolProperties` is persisted in the `kind=properties` PIT series, so
    /// a field there costs a Parquet codec-column decision, and nothing downstream ROUNDS with a
    /// leverage. Its ONE consumer is `vike_bridge_core::leverage::clamp_to_venue_cap`, at the
    /// exec thread's startup `set_leverage`.
    ///
    /// The sibling `leverageFilter.minLeverage` (`"1"`) and `leverageStep` (`"0.01"`) are
    /// deliberately NOT parsed: nothing clamps upward or snaps a leverage to a step today, and an
    /// unused parsed field reads as a supported capability. Add them with their consumer.
    pub max_leverage: Option<f64>,
    /// **The venue's own `contractType` for this row, verbatim** — `"LinearPerpetual"`,
    /// `"InversePerpetual"`, `"LinearFutures"`, `"InverseFutures"`. `None` when the row carries
    /// none (a spot row, or a shape the venue changed).
    ///
    /// ⚠ **It names the BOOK, and the request's category does not.** MEASURED 2026-09-16:
    /// `instruments-info?category=linear&symbol=XRPUSD` ANSWERS, with
    /// `contractType: "InversePerpetual"` and `settleCoin: "XRP"` — so the exec grid fetch's
    /// `category: "linear"` literal does not fail loudly on an inverse symbol, it succeeds and
    /// returns a coin-settled grid (`qtyStep=1`, `minOrderQty=1` — USD contracts). Its consumers are
    /// `crates/bridges/bybit/src/execution/mod.rs`'s `non_linear_perpetual_refusal` (a word no book is signed
    /// for), `mount_money` (a coin-margined mount: the inverse book and contract value) and
    /// `crate::instrument::class::category_of_contract_type` (the book of an admitted symbol).
    pub contract_type: Option<String>,
}

/// Every row of one `instruments-info` `{result:{list}}` page, keyed by its upper-cased symbol —
/// the exec plane's grid map. Unlike the catalog it gates on NOTHING (no `status`, no
/// `contractType`): the exec thread asks for one symbol and must see whatever the venue answered,
/// which is what lets [`crate::execution::mount_money`] read an `InversePerpetual` row the
/// `category: "linear"` request was answered with.
///
/// The grid and the [`BybitInstrument::contract`] are `crates/bridges/bybit/src/catalog.rs`'s
/// `derivative_properties`/`derivative_contract` — the class of the grid is the contract's
/// (`Contract::class`), so the stored class, the typed contract and the picker row are one
/// decision. Its doc records why the margining is the ROW's `contractType`, never the request's
/// `category`.
pub fn parse_bybit_perp_instruments(payload: &Value) -> IndexMap<String, BybitInstrument> {
    let mut out = IndexMap::new();
    let rows = payload.get("result").and_then(|r| r.get("list")).and_then(|l| l.as_array());
    for entry in rows.unwrap_or(&vec![]) {
        let symbol = entry.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_uppercase();
        if symbol.is_empty() {
            continue;
        }
        let contract = derivative_contract(entry);
        out.insert(
            symbol,
            BybitInstrument {
                properties: derivative_properties(entry, contract.as_ref()),
                contract,
                base_asset: entry
                    .get("baseCoin")
                    .and_then(|b| b.as_str())
                    .unwrap_or("")
                    .to_string(),
                // The account-leverage ceiling. `Option`, NOT the `number` closure's absent-is-0.0
                // convention: `0.0` here would be indistinguishable from "the venue says you may
                // not use leverage at all", and the ONE consumer
                // (`vike_bridge_core::leverage::clamp_to_venue_cap`) must be able to tell
                // "unknown, change nothing" from a real ceiling.
                max_leverage: entry
                    .get("leverageFilter")
                    .and_then(|d| d.get("maxLeverage"))
                    .and_then(json_number),
                // Carried, never interpreted here. Its one consumer is the exec thread's refusal.
                contract_type: entry
                    .get("contractType")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            },
        );
    }
    out
}

#[path = "leverage_cap_tests.rs"]
#[cfg(test)]
mod leverage_cap_tests;

#[path = "asset_class_tests.rs"]
#[cfg(test)]
mod asset_class_tests;

pub struct VenueRestBybit<T: RestTransportJson> {
    pub signer: BybitV5Signer,
    pub transport: T,
    pub base_url: String,
    /// The account is `live` ([`BybitUrls::mainnet`], the account's type): a refusal is worded for an
    /// account whose modes the bridge never changes (`crate::account::modes::order_reject_reason`).
    pub mainnet: bool,
    /// The VENUE's name of the instrument (`DOGEUSDT`, never the engine's `DOGEUSDT.S`): what goes on the wire.
    pub symbol: String,
    pub properties: SymbolProperties,
    pub leverage: f64,
    /// **The V5 book every per-symbol call of this client names** (decision 0231): `linear` for the mounted
    /// symbol and a dated linear future, `inverse` for a coin-margined contract, `spot` for a pair. A client is
    /// one book, built from the venue's own `contractType` word (or, with no grid fetched, from the kept row:
    /// `crate::history::Category::of_kept`), so no call can sign in a book its symbol is not in.
    pub category: Category,
}

impl<T: RestTransportJson> VenueRestBybit<T> {
    /// The `(category, venue symbol)` an ENGINE symbol is read in when this client is asked about it: this
    /// client's own book for its own symbol, else the book its KEPT row says (the replay of an admitted symbol runs
    /// on the mounted client), else this client's. `DOGEUSDT.S` is the spot pair `DOGEUSDT`.
    fn book_of(&self, symbol: &str) -> (&'static str, String) {
        let (wire, _) = crate::instrument::class::split_spot(symbol);
        if wire.eq_ignore_ascii_case(&self.symbol) {
            return (self.category.wire(), self.symbol.clone());
        }
        let book = Category::of_kept(symbol).unwrap_or(self.category);
        (book.wire(), wire.to_string())
    }

    fn call(
        &self,
        path: &str,
        method: &str,
        params: &[(&str, Value)],
    ) -> Result<Value, VenueApiError> {
        let response = self.transport.signed(&self.base_url, path, method, params, &self.signer)?;
        unwrap_envelope(response)
    }

    /// Audit A3 resync: recent CLOSED orders (`GET /v5/order/history`) for the post-reconnect
    /// history replay, on the short-timeout requery transport. Returns the raw `result.list` array.
    pub fn get_order_history(&self, limit: u32) -> Result<Value, VenueApiError> {
        self.get_order_history_for(&self.symbol, limit)
    }

    /// [`Self::get_order_history`] for ANOTHER symbol of the account (an admitted one: the resync and the
    /// gap-sentinel replay each symbol the engine trades, `crate::execution::bybit_history_events_of`). The
    /// request is the mounted symbol's, byte for byte, with that symbol.
    pub fn get_order_history_for(&self, symbol: &str, limit: u32) -> Result<Value, VenueApiError> {
        let (category, wire) = self.book_of(symbol);
        let params: Vec<(&str, Value)> =
            vec![("category", json!(category)), ("symbol", json!(wire)), ("limit", json!(limit))];
        let response = self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER_HISTORY,
            "GET",
            &params,
            &self.signer,
        )?;
        Ok(unwrap_envelope(response)?.get("list").cloned().unwrap_or(json!([])))
    }

    /// Audit A3 resync: recent executions (`GET /v5/execution/list`) for the history replay.
    pub fn get_execution_history(&self, limit: u32) -> Result<Value, VenueApiError> {
        self.get_execution_history_for(&self.symbol, limit)
    }

    /// [`Self::get_execution_history`] for ANOTHER symbol of the account (see
    /// [`Self::get_order_history_for`]).
    pub fn get_execution_history_for(
        &self,
        symbol: &str,
        limit: u32,
    ) -> Result<Value, VenueApiError> {
        let (category, wire) = self.book_of(symbol);
        let params: Vec<(&str, Value)> =
            vec![("category", json!(category)), ("symbol", json!(wire)), ("limit", json!(limit))];
        let response = self.transport.signed_requery(
            &self.base_url,
            PATH_EXECUTION_LIST,
            "GET",
            &params,
            &self.signer,
        )?;
        Ok(unwrap_envelope(response)?.get("list").cloned().unwrap_or(json!([])))
    }

    /// Audit T1: re-query one order by orderLinkId (= our idempotent order_client_id) on the
    /// short-timeout requery transport, to resolve an ambiguous submit into a NON-phantom outcome.
    /// TWO-STAGE, because `/v5/order/realtime` only retains OPEN + recently-closed orders: a submit
    /// that FILLED-AND-CLOSED just outside that retention window returns EMPTY there, so concluding
    /// "absent" from realtime alone would phantom-reject a REAL live position. We therefore FALL BACK
    /// to `/v5/order/history` (the CLOSED/filled endpoint) on an empty realtime, and only report
    /// absent when BOTH are empty. `Ok(Some((id, row)))` = venue HAS the order (live, or filled/closed):
    /// its venue id and its row, which the active confirm reads by state and the submit-timeout path by
    /// the id alone (a fill, if any, follows on the user-data / A3 lane); `Ok(None)` =
    /// both endpoints confirm absent → the true terminal reject (the active confirm: only for a dispatched order client id);
    /// `Err` = the query itself failed → silence on both paths, never a false terminal; the stuck-order
    /// ladder decides. The history fallback rides the same
    /// gate-exempt short-timeout requery transport and only ever fires on the rare ambiguous submit.
    fn query_order_orderlinkid(
        &self,
        order_client_id: &str,
    ) -> Result<Option<(String, Value)>, VenueApiError> {
        match self.query_order_status(PATH_ORDER_REALTIME, order_client_id)? {
            Some(found) => Ok(Some(found)),
            // realtime is an OPEN-orders view — empty ≠ absent for a filled-and-closed order that
            // dropped off its retention window. Confirm against history before concluding absent.
            None => self.query_order_status(PATH_ORDER_HISTORY, order_client_id),
        }
    }

    /// Signed by-orderLinkId single-order lookup against a status endpoint (`/v5/order/realtime` OR
    /// `/v5/order/history`), on the short-timeout requery transport. `Ok(Some((id, row)))` = the first
    /// matching row and its venue `orderId`; `Ok(None)` = a real, EMPTY `result.list` (Bybit returns no
    /// not-found error code for a by-id miss); `Err` = the re-query itself failed, or the answer has
    /// no `list` or names no order, which says nothing about it. Both endpoints
    /// share the `{category, symbol, orderLinkId}` request and the `result.list` reply shape.
    fn query_order_status(
        &self,
        path: &str,
        order_client_id: &str,
    ) -> Result<Option<(String, Value)>, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("category", json!(self.category.wire())),
            ("symbol", json!(self.symbol)),
            ("orderLinkId", json!(order_client_id)),
        ];
        let response =
            self.transport.signed_requery(&self.base_url, path, "GET", &params, &self.signer)?;
        let result = unwrap_envelope(response)?;
        let says_nothing = |what: &str| VenueApiError {
            code: E_TIMEOUT_AMBIGUOUS,
            msg: format!("GET {path} answered 2xx with no {what}"),
        };
        let list =
            result.get("list").and_then(|l| l.as_array()).ok_or_else(|| says_nothing("list"))?;
        let Some(row) = list.first() else { return Ok(None) };
        let id = order_id_of(row).ok_or_else(|| says_nothing("orderId"))?;
        Ok(Some((id, row.clone())))
    }

    /// Resolve an ambiguous (timed-out) submit into a NON-phantom event (audit T1). `code` is what left
    /// it unknown: after a code that says the server had trouble ([`server_had_trouble`]) a query that
    /// FAILS too says nothing, because a restarting venue most likely took nothing and an id-less accept
    /// is a phantom no sweep ever finds; the order stays pre-ack for the watchdog's confirm and backstop.
    fn resolve_ambiguous_submit(&self, request: &OrderRequest, code: i64) -> Vec<Event> {
        let query = self.query_order_orderlinkid(&request.order_client_id);
        if server_had_trouble(code)
            && let Err(e) = &query
        {
            tracing::warn!(
                target: "vike_bybit::execution::rest", order_client_id = %request.order_client_id, code,
                query_code = e.code, "the venue had trouble and the re-query failed too; saying nothing"
            );
            return Vec::new();
        }
        let id = query.map(|found| found.map(|(id, _row)| id));
        vike_bridge_core::resolve_ambiguous_submit(&request.order_client_id, request.timestamp, id)
            .into_iter()
            .collect()
    }

    /// POST set-leverage (one-way: buy == sell). Swallows 110043 (already at target).
    pub fn set_leverage(&self) -> Result<(), VenueApiError> {
        // A spot pair has no leverage to set (decision 0231): nothing is sent.
        if self.category == Category::Spot {
            return Ok(());
        }
        // Python str(self._leverage) — a float formats as "2.0"
        let leverage = vike_bridge_core::format::py_f64_str(self.leverage);
        let params: Vec<(&str, Value)> = vec![
            ("category", json!(self.category.wire())),
            ("symbol", json!(self.symbol)),
            ("buyLeverage", json!(leverage)),
            ("sellLeverage", json!(leverage)),
        ];
        match self.call(PATH_SET_LEVERAGE, "POST", &params) {
            Ok(_) => Ok(()),
            Err(exc) if exc.code == LEVERAGE_NOT_MODIFIED => Ok(()),
            Err(exc) => Err(exc),
        }
    }

    /// **What a SPOT pair's conditional order cannot say is refused whole, with the reason** (decision 0231).
    /// The spot book's conditional is `orderFilter=StopOrder` with a `triggerPrice`, and it takes neither
    /// `triggerDirection` nor `triggerBy` ("valid for `linear` & `inverse`",
    /// https://bybit-exchange.github.io/docs/v5/order/create-order), so [`Self::build_order_params`] builds the
    /// spot stop-limit and the two take-profits on it, and these stay refused:
    ///
    /// - a `stop` (a conditional `Market` closing at a loss): the spot conditional was built for the stop-limit and
    ///   the take-profits (wave 2 of the order kinds), and the stop's opening is a one-line change here once a demo
    ///   pass has placed one;
    /// - a conditional asked to trigger on the mark or the index price: the spot wire names no source, so the
    ///   venue's own (the last price, as the derivatives' `LastPrice` default) would be a silent substitute.
    ///
    /// The terminal `OrderRejected` is the one `deny_unsupported_time_in_force` shapes use.
    fn spot_stop_refusal(&self, request: &OrderRequest) -> Option<Event> {
        if self.category != Category::Spot {
            return None;
        }
        let reason = match (&request.order_type, request.trigger_by) {
            (OrderType::Stop, _) => format!(
                "bybit: a {} order on the spot pair {} is not built (the spot book's conditional order \
                 is another wire than the derivatives'); nothing was sent. Rest a limit order, or trade the \
                 perpetual for a protective stop",
                request.order_type, self.symbol
            ),
            (kind, Some(by @ (TriggerBy::Mark | TriggerBy::Index))) if kind.is_trigger() => {
                format!(
                    "bybit: a {kind} order on the spot pair {} triggers on the last price only (the spot \
                     conditional order names no trigger source); a {by:?} price trigger is not built, nothing \
                     was sent",
                    self.symbol
                )
            }
            _ => return None,
        };
        Some(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: request.order_client_id.clone(),
            reason: reason.into(),
            timestamp: request.timestamp,
        }))
    }

    /// **A kind [`Self::build_order_params`] does not build is refused whole, by name**: a `market_to_limit` or a
    /// word outside the known kinds would otherwise go out as a plain `Market` (the builder's `orderType` has two
    /// words), filling at once an order its sender meant to wait or bound. The capability row refuses both before
    /// an order reaches a bridge; this is the bridge's own floor under it, and a kind added to the model later is
    /// refused here until it is built.
    fn unbuilt_kind_refusal(request: &OrderRequest) -> Option<Event> {
        let built = matches!(
            request.order_type,
            OrderType::Market
                | OrderType::Limit
                | OrderType::Stop
                | OrderType::StopLimit
                | OrderType::TakeProfit
        );
        (!built).then(|| {
            Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: format!(
                    "bybit: a {} order is not built by this bridge; nothing was sent",
                    request.order_type
                )
                .into(),
                timestamp: request.timestamp,
            })
        })
    }

    /// **A conditional order whose lifetime it cannot carry is refused whole, by name**, never sent with no
    /// `timeInForce`, which silently changes the order. `deny_unsupported_time_in_force` judges a plain limit's only; the
    /// core's preflight refuses these first, and this is the bridge's own floor under it.
    ///
    /// - A stop-limit or a limit-if-touched: the limit's `timeInForce` is bybit's `venue_time_in_force` row, and a lifetime
    ///   the row says `Unsupported` (`Gtd`, `Day`, `AtOpen`, `AtClose`) would rest as the venue's GTC.
    /// - A stop or a market-if-touched (a conditional `Market`, which carries no lifetime): an AUCTION lifetime
    ///   (`AtOpen`, `AtClose`), since bybit has none and the order would fire as a plain conditional. A stop's
    ///   `Gtd`/`Day` keep their old path (sent with no lifetime, as before the auction lifetimes existed).
    fn conditional_time_in_force_refusal(request: &OrderRequest) -> Option<Event> {
        let tif = request.time_in_force;
        let why = match (&request.order_type, request.price.is_some()) {
            (OrderType::StopLimit, _) | (OrderType::TakeProfit, true)
                if vike_venues::venue_time_in_force::venue_time_in_force(VENUE, tif)
                    == vike_venues::venue_time_in_force::TimeInForceOutcome::Unsupported =>
            {
                "its conditional limit carries no such timeInForce, and it would rest GTC"
            }
            (OrderType::Stop, _) | (OrderType::TakeProfit, false) if tif.is_auction() => {
                "bybit has no auction, and it would fire as a plain conditional"
            }
            _ => return None,
        };
        Some(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: request.order_client_id.clone(),
            reason: format!(
                "bybit: a {} order with the lifetime {tif:?} is not built ({why}); nothing was sent",
                request.order_type
            )
            .into(),
            timestamp: request.timestamp,
        }))
    }

    /// Pure, golden-gated: Buy/Sell + Limit/Market casing, orderLinkId as the client id,
    /// positionIdx 0 (int), reduceOnly BOOL. A `stop_limit` is a conditional `Limit`: the limit's
    /// `timeInForce` and `price`, plus the stop's trigger words. A `take_profit` is the same conditional
    /// firing the OTHER way (a BUY on a fall): a `Limit` at its `price` when it has one (limit-if-touched),
    /// else a `Market` (market-if-touched). On a spot pair the trigger words are `triggerPrice` and
    /// `orderFilter StopOrder` alone ([`Self::spot_stop_refusal`] says why).
    pub fn build_order_params(&self, request: &OrderRequest) -> Vec<(&'static str, Value)> {
        let is_limit = match request.order_type {
            OrderType::Limit | OrderType::StopLimit => true,
            OrderType::TakeProfit => request.price.is_some(),
            _ => false,
        };
        let spot = self.category == Category::Spot;
        let mut params: Vec<(&'static str, Value)> = vec![
            ("category", json!(self.category.wire())),
            ("symbol", json!(self.symbol)),
            ("side", json!(if request.side > 0 { "Buy" } else { "Sell" })),
            ("orderType", json!(if is_limit { "Limit" } else { "Market" })),
            ("qty", json!(format_to_step_f(request.qty, self.properties.step_size))),
            ("orderLinkId", json!(request.order_client_id)),
        ];
        if spot {
            // SPOT (decision 0231, MEASURED on the demo host 2026-10-30): a spot order has no position index and
            // no reduce-only word; and a MARKET order's `qty` is read as QUOTE currency on a buy unless
            // `marketUnit` says base (13 USDT of DOGE bought for `qty 13`, the echo says `marketUnit
            // quoteCoin`), so a market order always states the base coin: the engine's quantity is the base's.
            // (A reduce-only request needs no flag: a spot sale cannot exceed the coin held, the venue answers
            // 170131 otherwise.)
            if !is_limit {
                params.push(("marketUnit", json!("baseCoin")));
            }
        } else {
            params.push(("positionIdx", json!(0)));
            params.push(("reduceOnly", json!(request.reduce_only)));
        }
        if is_limit {
            // tif step-2 (FLIPPED): the Limit path consumes bybit's `venue_time_in_force` row —
            // GTC/IOC/FOK map 1:1 (a default/GTC request emits the same timeInForce=GTC bytes
            // as the pre-flip hardcode, pinned in `r6_bybit_parity.rs`). An Unsupported TIF
            // (GTD/Day, gated at submit by `deny_unsupported_time_in_force`) that reaches this builder
            // anyway emits NO timeInForce — the venue's server default rules, and the submit
            // gate is the loud guard.
            match vike_venues::venue_time_in_force::venue_time_in_force(
                VENUE,
                request.time_in_force,
            ) {
                vike_venues::venue_time_in_force::TimeInForceOutcome::Mapped(w)
                | vike_venues::venue_time_in_force::TimeInForceOutcome::Coerced {
                    wire: w, ..
                } => {
                    params.push(("timeInForce", json!(w)));
                }
                vike_venues::venue_time_in_force::TimeInForceOutcome::Ignored { wire } => {
                    params.push(("timeInForce", json!(wire)));
                }
                vike_venues::venue_time_in_force::TimeInForceOutcome::NotEmitted
                | vike_venues::venue_time_in_force::TimeInForceOutcome::Unsupported => {}
            }
            params.push((
                "price",
                json!(format_to_step_f(request.price.unwrap_or(0.0), self.properties.tick_size)),
            ));
        }
        // `order_type:"stop"` (bracket SL) → a native conditional (trigger) order on the SAME create
        // endpoint: a Market that fires at triggerPrice; a `stop_limit` is the same conditional firing
        // as the Limit above. Direction from the side — a BUY stop (closing a short) fires as price
        // RISES (1); a SELL stop (closing a long) as it FALLS (2); a `take_profit` the opposite way (a
        // BUY on a fall, a SELL on a rise).
        // Additive: the golden limit/market cases never carry a trigger, so their bytes are unchanged.
        if !matches!(
            request.order_type,
            OrderType::Stop | OrderType::StopLimit | OrderType::TakeProfit
        ) {
            return params;
        }
        params.push((
            "triggerPrice",
            json!(format_to_step_f(
                request.trigger_price.unwrap_or(0.0),
                self.properties.tick_size
            )),
        ));
        if spot {
            // The spot conditional (https://bybit-exchange.github.io/docs/v5/order/create-order: "`StopOrder`:
            // Spot conditional order, the assets will not be occupied until the price of the underlying asset
            // reaches the trigger price"): no direction and no source word, the venue reads both itself.
            // ⚠ Unmeasured: the page states no spot direction rule. Read off the derivative one ("triggerPrice >
            // market price" to fire on a rise), the venue fires a spot conditional toward the side of the market
            // its trigger sat on when placed, so a trigger already crossed at placement (a BUY take-profit above
            // the market) waits for a RISE, where the derivative wire states the fall its kind means.
            params.push(("orderFilter", json!("StopOrder")));
        } else {
            let rises = (request.side > 0) != (request.order_type == OrderType::TakeProfit);
            params.push(("triggerDirection", json!(if rises { 1 } else { 2 })));
            // Trigger-source law: a requested `trigger_by` consumes bybit's
            // `vike_bridge_core::trigger::venue_trigger_by` row — V5 `triggerBy` expresses all
            // three sources, so nothing is ever denied here. `None` keeps the historical
            // "LastPrice" hardcode byte-identically (the venue's own default is also last, but
            // the explicit param is the pinned pre-field wire shape — never dropped).
            let trigger_by = request
                .trigger_by
                .and_then(|s| vike_bridge_core::trigger::venue_trigger_by(VENUE, s).wire())
                .unwrap_or("LastPrice");
            params.push(("triggerBy", json!(trigger_by)));
        }
        params
    }

    /// Native amend (POST /v5/order/amend): re-price and/or re-size a resting order in place.
    /// `qty`/`price` are step/tick-formatted like submit; the client id is `orderLinkId`. Returns
    /// `[OrderModified]` on the venue's ack; a non-terminal `OrderModifyRejected` (venue reason) on failure (order keeps its terms). RUST-NATIVE,
    /// no Python twin.
    pub fn modify_order(
        &self,
        order_client_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        self.amend(order_client_id, new_qty, new_price, None)
    }

    /// [`Self::modify_order`] that may also move a conditional order's TRIGGER: `new_trigger_price` is the
    /// amend's `triggerPrice`, tick-formatted, which the venue's page gives perps, futures AND spot ("For spot,
    /// it is the TP/SL and Conditional order trigger price", https://bybit-exchange.github.io/docs/v5/order/amend-order).
    /// The `OrderModified` echoes the three as given: `vike_model::modified_prices` folds them on the order.
    /// With `None` it is `modify_order` byte for byte.
    pub fn amend(
        &self,
        order_client_id: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
    ) -> Vec<Event> {
        let mut params: Vec<(&str, Value)> = vec![
            ("category", json!(self.category.wire())),
            ("symbol", json!(self.symbol)),
            ("orderLinkId", json!(order_client_id)),
        ];
        if let Some(q) = new_qty {
            params.push(("qty", json!(format_to_step_f(q, self.properties.step_size))));
        }
        if let Some(p) = new_price {
            params.push(("price", json!(format_to_step_f(p, self.properties.tick_size))));
        }
        if let Some(t) = new_trigger_price {
            params.push(("triggerPrice", json!(format_to_step_f(t, self.properties.tick_size))));
        }
        // `Self::call`, with the envelope's `time` (epoch ms, the venue's stamp of its answer, a refusal's
        // too) read first.
        let reply =
            self.transport.signed(&self.base_url, PATH_ORDER_AMEND, "POST", &params, &self.signer);
        let stated = reply.as_ref().ok().and_then(|r| r.get("time")).and_then(Value::as_i64);
        match reply.and_then(unwrap_envelope) {
            Ok(result) => vec![Event::OrderModified(OrderModified {
                order_client_id: order_client_id.to_string(),
                venue_order_id: result
                    .get("orderId")
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .map(Into::into),
                new_qty,
                new_price,
                new_trigger_price,
                timestamp: vike_bridge_core::rest::amend_answer_time(stated),
            })],
            // A refused amend keeps the order's terms, but the refusal must not vanish: a
            // NON-terminal advisory carrying the venue's reason (same shape as binance/aster perp).
            Err(exc) => vec![modify_failed(order_client_id, &exc, stated)],
        }
    }

    /// Native batch-submit (POST /v5/order/create-batch). The body is `{category, request:[…]}` —
    /// an object wrapping the order array, so it rides the flat param map (no raw-body signer
    /// needed). Emits OrderSubmitted per order, then per-order Accepted|Rejected: the top-level
    /// `retCode` gates the whole batch; individual outcomes come from `retExtInfo.list[i].code`
    /// (0 = ok, orderId from `result.list[i]`), matched by request order. Chunks at BATCH_MAX.
    /// RUST-NATIVE, no Python twin.
    pub fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        let mut events: Vec<Event> = Vec::with_capacity(requests.len() * 2);
        for r in requests {
            events.push(Event::OrderSubmitted(OrderSubmitted {
                order_client_id: r.order_client_id.clone(),
                timestamp: r.timestamp,
            }));
        }
        // tif step-2 gate, per order: a TIF bybit cannot express (GTD/Day) is a LOUD terminal
        // reject and its order never enters a wire chunk; the supported rest batch as before.
        let mut sendable: Vec<&OrderRequest> = Vec::with_capacity(requests.len());
        for r in requests {
            match vike_venues::venue_time_in_force::deny_unsupported_time_in_force(VENUE, r)
                .or_else(|| self.spot_stop_refusal(r))
                .or_else(|| Self::unbuilt_kind_refusal(r))
                .or_else(|| Self::conditional_time_in_force_refusal(r))
            {
                Some(reject) => events.push(reject),
                None => sendable.push(r),
            }
        }
        for chunk in sendable.chunks(BATCH_MAX) {
            let order_objs: Vec<Value> = chunk
                .iter()
                .map(|r| {
                    Value::Object(
                        self.build_order_params(r)
                            .into_iter()
                            // `category` is the top-level batch field, not a per-order one
                            .filter(|(k, _)| *k != "category")
                            .map(|(k, v)| (k.to_string(), v))
                            .collect(),
                    )
                })
                .collect();
            let params: Vec<(&str, Value)> = vec![
                ("category", json!(self.category.wire())),
                ("request", Value::Array(order_objs)),
            ];
            // raw envelope (not `call`): per-order status lives in retExtInfo, a sibling of result
            match self.transport.signed(
                &self.base_url,
                PATH_ORDER_CREATE_BATCH,
                "POST",
                &params,
                &self.signer,
            ) {
                Ok(envelope) => {
                    // a body with no `retCode` says nothing about the chunk: it may rest, so ask
                    let Some(ret_code) = envelope.get("retCode").and_then(|c| c.as_i64()) else {
                        for r in chunk {
                            events.extend(self.resolve_ambiguous_submit(r, E_TIMEOUT_AMBIGUOUS));
                        }
                        continue;
                    };
                    if ret_code != 0 {
                        // HTTP 200 + a status-unknown retCode: the batch may have landed — re-query each.
                        if submit_outcome_unknown(ret_code) {
                            for r in chunk {
                                events.extend(self.resolve_ambiguous_submit(r, ret_code));
                            }
                            continue;
                        }
                        let msg = envelope
                            .get("retMsg")
                            .and_then(|m| m.as_str())
                            .unwrap_or("batch reject")
                            .to_string();
                        for r in chunk {
                            events.push(Event::OrderRejected(OrderRejected {
                                cause: None,
                                order_client_id: r.order_client_id.clone(),
                                reason: crate::account::modes::order_reject_reason(
                                    ret_code,
                                    &msg,
                                    self.mainnet,
                                    &self.symbol,
                                )
                                .into(),
                                timestamp: r.timestamp,
                            }));
                        }
                        continue;
                    }
                    let result_list = envelope
                        .get("result")
                        .and_then(|r| r.get("list"))
                        .and_then(|l| l.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let ext_list = envelope
                        .get("retExtInfo")
                        .and_then(|r| r.get("list"))
                        .and_then(|l| l.as_array())
                        .cloned()
                        .unwrap_or_default();
                    for (i, r) in chunk.iter().enumerate() {
                        let ext_code =
                            ext_list.get(i).and_then(|e| e.get("code")).and_then(|c| c.as_i64());
                        let id = result_list
                            .get(i)
                            .and_then(|o| o.get("orderId"))
                            .and_then(|o| o.as_str())
                            .filter(|id| !id.is_empty());
                        if ext_code.unwrap_or(0) == 0 {
                            match id {
                                Some(id) => events.push(Event::OrderAccepted(OrderAccepted {
                                    order_client_id: r.order_client_id.clone(),
                                    venue_order_id: Some(id.into()),
                                    timestamp: r.timestamp,
                                })),
                                // a 2xx that names no order: it may rest, so ask
                                None => events
                                    .extend(self.resolve_ambiguous_submit(r, E_TIMEOUT_AMBIGUOUS)),
                            }
                        } else if submit_outcome_unknown(ext_code.unwrap_or(0)) {
                            events.extend(self.resolve_ambiguous_submit(r, ext_code.unwrap_or(0)));
                        } else {
                            events.push(Event::OrderRejected(OrderRejected {
                                cause: None,
                                order_client_id: r.order_client_id.clone(),
                                reason: crate::account::modes::order_reject_reason(
                                    ext_code.unwrap_or(0),
                                    ext_list
                                        .get(i)
                                        .and_then(|e| e.get("msg"))
                                        .and_then(|m| m.as_str())
                                        .unwrap_or("batch order reject"),
                                    self.mainnet,
                                    &self.symbol,
                                )
                                .into(),
                                timestamp: r.timestamp,
                            }));
                        }
                    }
                }
                // audit T1: an ambiguous whole-batch timeout may have been accepted — re-query each.
                Err(exc) if submit_outcome_unknown(exc.code) => {
                    for r in chunk {
                        events.extend(self.resolve_ambiguous_submit(r, exc.code));
                    }
                }
                Err(exc) => {
                    for r in chunk {
                        events.push(Event::OrderRejected(OrderRejected {
                            cause: None,
                            order_client_id: r.order_client_id.clone(),
                            reason: crate::account::modes::order_reject_reason(
                                exc.code,
                                &exc.msg,
                                self.mainnet,
                                &self.symbol,
                            )
                            .into(),
                            timestamp: r.timestamp,
                        }));
                    }
                }
            }
        }
        events
    }

    /// Signed GET returning the unwrapped `result` — the seam the funding poller and
    /// smoke probes reuse.
    pub fn call_public(
        &self,
        path: &str,
        params: &[(&str, Value)],
    ) -> Result<Value, VenueApiError> {
        self.call(path, "GET", params)
    }

    /// Last price from /v5/market/tickers (signed transport works for public GETs too).
    pub fn last_price(&self) -> Result<f64, VenueApiError> {
        let result = self.call(
            PATH_TICKERS,
            "GET",
            &[("category", json!(self.category.wire())), ("symbol", json!(self.symbol))],
        )?;
        let lst = result.get("list").and_then(|l| l.as_array());
        Ok(lst
            .and_then(|l| l.first())
            .and_then(|t| t.get("lastPrice"))
            .and_then(json_number)
            .unwrap_or(0.0))
    }
}

/// The active confirm over the by-orderLinkId re-query ([`VenueRestBybit::query_order_orderlinkid`]:
/// realtime, then history), in the shared form of `vike_bridge_core::confirm`: found is read by its
/// state through the stream's own [`map_order`] (a fill rides the `execution` topic, so `Filled` is the
/// accept alone), absent on BOTH endpoints -> the "venue confirms absent" reject only for an order client id
/// the exec thread finished dispatching, a FAILED query -> nothing.
impl<T: RestTransportJson + Send> ConfirmByOrderClientId for VenueRestBybit<T> {
    // None: bybit documents no age limit on `/v5/order/history` that was measured here.
    const LOOKUP_WINDOW: Option<std::time::Duration> = None;

    fn confirm_when(&self, order_client_id: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        let query = self.query_order_orderlinkid(order_client_id);
        resolve_confirm(order_client_id, ts, query, absent_rejects, |(id, mut row)| {
            // asked by this order client id, so the answer is about it; the mapper names the order off the row
            row["orderLinkId"] = json!(order_client_id);
            found_by_state(order_client_id, ts, Some(id), map_order(&row, VENUE, &self.symbol))
        })
    }
}

impl<T: RestTransportJson + Send> VenueRest for VenueRestBybit<T> {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        })];
        // tif step-2 gate: a TIF bybit cannot express (GTD/Day) is a LOUD terminal reject —
        // the wire is never touched, never a silent GTC (pinned in `tests/offline/time_in_force_gate.rs`).
        if let Some(reject) =
            vike_venues::venue_time_in_force::deny_unsupported_time_in_force(VENUE, request)
        {
            events.push(reject);
            return events;
        }
        if let Some(reject) = self
            .spot_stop_refusal(request)
            .or_else(|| Self::unbuilt_kind_refusal(request))
            .or_else(|| Self::conditional_time_in_force_refusal(request))
        {
            events.push(reject);
            return events;
        }
        let params = self.build_order_params(request);
        match self.call(PATH_ORDER_CREATE, "POST", &params) {
            Ok(result) => match order_id_of(&result) {
                Some(id) => events.push(Event::OrderAccepted(OrderAccepted {
                    order_client_id: request.order_client_id.clone(),
                    venue_order_id: Some(id.into()),
                    timestamp: request.timestamp,
                })),
                // a 2xx that names no order: it may rest, so ask, as for a timeout
                None => events.extend(self.resolve_ambiguous_submit(request, E_TIMEOUT_AMBIGUOUS)),
            },
            // audit T1: an ambiguous timeout may have been accepted — re-query, don't false-reject.
            Err(exc) if submit_outcome_unknown(exc.code) => {
                events.extend(self.resolve_ambiguous_submit(request, exc.code));
            }
            Err(exc) => {
                tracing::warn!(target: "vike_bybit::execution::rest", code = exc.code, msg = %exc.msg, "submit rejected");
                events.push(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: crate::account::modes::order_reject_reason(
                        exc.code,
                        &exc.msg,
                        self.mainnet,
                        &self.symbol,
                    )
                    .into(),
                    timestamp: request.timestamp,
                }))
            }
        }
        events
    }

    fn cancel_order(
        &self,
        order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("category", json!(self.category.wire())),
            ("symbol", json!(self.symbol)),
            ("orderLinkId", json!(order_client_id)),
        ];
        vike_bridge_core::rest::OrderCancelAck::from_venue_answer(
            VENUE,
            order_client_id,
            self.call(PATH_ORDER_CANCEL, "POST", &params),
            |exc| cancel_says_gone(exc.code),
        )
    }

    /// ACTIVELY confirm one order's status, in the shared form of `vike_bridge_core::confirm`: this
    /// trait door assumes the submit was dispatched ([`ConfirmByOrderClientId::confirm_when`] with `true`); the
    /// exec client's confirm closure (`crate::execution`) goes through `confirm_when` with the ledger's
    /// real answer. Runs on the ExecutionClientActor's own confirm worker.
    fn confirm_order(&self, order_client_id: &str, ts: i64) -> Vec<Event> {
        self.confirm_when(order_client_id, ts, true)
    }

    /// Native Bybit amend — overrides the no-op default with `/v5/order/amend`. Bybit identifies the
    /// order by `orderLinkId`, so only the order client id is needed from the resting `order`. A STOP's `new_price`
    /// is its trigger (`vike_model::modified_price_is_trigger`), so it goes out as [`VenueRestBybit::amend`]'s
    /// `triggerPrice` and the `OrderModified` echoes it as the new trigger: the stop moves in place (demo
    /// host, 2026-11-08, `bybit_trigger_amend_smoke`: one `OrderModified`, the row `Untriggered` at the new
    /// `triggerPrice`, `orderType Market` and `triggerDirection` kept). Every other kind's is its `price`.
    fn modify_order(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
    ) -> Vec<Event> {
        if vike_model::modified_price_is_trigger(&order.order_type) {
            return self.amend(&order.order_client_id, new_qty, None, new_price);
        }
        VenueRestBybit::modify_order(self, &order.order_client_id, new_qty, new_price)
    }

    /// Native Bybit trigger amend: a stop-limit's or a take-profit's `triggerPrice` (and its `qty` and
    /// limit `price` when asked) in the one `/v5/order/amend` call ([`VenueRestBybit::amend`]).
    fn modify_trigger(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Vec<Event> {
        self.amend(&order.order_client_id, new_qty, new_price, Some(new_trigger_price))
    }

    /// Native Bybit batch-submit — overrides the fan-out default with `/v5/order/create-batch`.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        VenueRestBybit::submit_batch(self, requests)
    }
}
