//! **Binance COIN-M (`dapi`), the third book the exec plane signs in** (decision 0258's binance amendment): a
//! coin-margined perpetual or quarterly (`BTCUSD_PERP.P`, `BTCUSD_261225.P`) is MOUNTED, ordered in whole
//! contracts, and its position, wallet, fee and funding are read in its settle coin.
//!
//! # The book is the PRODUCT
//!
//! A COIN-M name (`vike_catalog::is_inverse_contract`, the rule the node's door reads the settle coin with) gets
//! the `dapi` host of its account's network (the `venue_url` row of product `coinm`, `crate::execution`'s
//! `BinanceUrls::futures_rest`) in place of `fapi`, and nothing else changes in the client: [`RestTransportDapiPaths`] carries
//! each `/fapi/...` request of the USDⓈ-M code to its `dapi` twin when, and only when, the client's margining is
//! `Inverse` (`crate::execution::rest::perp::VenueRestBinancePerp::margining`, [`margining_of`] its name, set with its host, never read off
//! the host string). A USDⓈ-M client is `Linear`, so every linear request is byte-identical; no binance name is
//! `Quanto`, and a client stating it is refused in words ([`NO_QUANTO_BOOK`]), never read as either book. The two
//! books' signed grammar is the same (the same HMAC query, the same order, cancel, modify, batch, leverage,
//! margin-type and position-mode words, the same `ORDER_TRADE_UPDATE` stream); what differs is carried here:
//!
//! | what | USDⓈ-M | COIN-M |
//! |---|---|---|
//! | quantity | base coin | whole CONTRACTS of `contractSize` US dollars (100 on BTC, 10 on the rest) |
//! | versions | `/fapi/v1` and `/fapi/v2` | `/dapi/v1` (and `/dapi/v2/leverageBracket`, the per-SYMBOL bracket read) |
//! | `positionRisk` | `symbol=` | `pair=` (the documented filter): asked for the pair, answered for the one symbol |
//! | conditional orders | the Algo Service (`crate::execution::rest::algo`) | none: dapi has no Algo Service, so its reads answer EMPTY without a call and a stop, stop-limit or take-profit is refused by name ([`deny_unbuilt`]) |
//! | time in force | GTC, IOC, FOK, GTX, GTD | GTC, IOC, FOK, GTX (the row's own `timeInForce`): GTD is refused by name |
//! | wallet | the USDT row of `/fapi/v2/balance` | the SETTLE COIN's row of `/dapi/v1/balance` ([`parse_coin_balance`]) |
//! | funding | the `/fapi/v1/income` history (`crate::execution::funding::income_poller`) | the same poller on the mount's `dapi` client, so `/dapi/v1/income`, each row in its settle coin; the stream's funding rows are dropped on both, so nothing is booked twice |
//!
//! # The money is the ROW's
//!
//! [`mount_of`] reads the mounted contract's own `dapi` exchangeInfo row (the network's list: a `demo` account
//! signs on `demo-dapi`, whose list is not mainnet's): `Inverse(contractSize)` and the row's `marginAsset` as the
//! coin. A COIN-M name whose row cannot be read, is not TRADING, or states no positive `contractSize` or no coin
//! does NOT RUN: its face is the row's alone (`vike_catalog::inverse_face_value` refuses to guess the common
//! 100, which is 10x wrong on every coin but BTC).

use serde_json::{Value, json};
use vike_bridge_core::json::json_number;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, UreqTransport, VenueApiError};
use vike_model::events::{Event, OrderRejected};
use vike_model::{
    ContractValue, Margining, NotionalKind, OrderRequest, SymbolProperties, TimeInForce,
};

use crate::family::perp_series_symbol;

/// `GET /dapi/v1/exchangeInfo` (public; it ignores a `symbol` filter, so the whole list is the unit of work).
pub const PATH_EXCHANGE_INFO: &str = "/dapi/v1/exchangeInfo";
/// `GET /dapi/v1/time` (public): the clock a COIN-M client signs on.
pub const PATH_TIME: &str = "/dapi/v1/time";
/// `POST`/`PUT /dapi/v1/listenKey`: the COIN-M user-data key.
pub const LISTENKEY_PATH: &str = "/dapi/v1/listenKey";

/// Whether `symbol` names a COIN-M contract: the catalog's name rule (`BTCUSD_PERP`, `BTCUSD_261225`, with or
/// without the lane's `.P`), which the node's door reads the settle coin with too. A caller asks it on the perp
/// lane only (a bare spot symbol never reaches a futures client).
#[must_use]
pub fn is_coin_m(symbol: &str) -> bool {
    vike_catalog::is_inverse_contract(crate::VENUE, symbol)
}

/// Whether the ENGINE symbol `symbol` mounts a COIN-M contract: the perp lane (`.P`) and a COIN-M name.
#[must_use]
pub fn names_coin_m(symbol: &str) -> bool {
    crate::family::execution::dispatch::split_symbol(symbol).1 && is_coin_m(symbol)
}

/// **The margining of the futures book `symbol` names** (an exchange symbol or a series label): `Inverse` (COIN-M,
/// `dapi`) for a COIN-M name ([`is_coin_m`]), else `Linear` (USDⓈ-M, `fapi`). The ONE place a binance name becomes
/// a margining; no binance name is `Quanto`.
#[must_use]
pub fn margining_of(symbol: &str) -> Margining {
    if is_coin_m(symbol) { Margining::Inverse } else { Margining::Linear }
}

/// The clock path of the futures host `symbol` signs on (`crate::execution`'s `BinanceUrls::futures_rest`, by the same
/// name rule): dapi's for a COIN-M name, else `crate::execution::rest::perp::PATH_TIME`.
#[must_use]
pub fn time_path_for(symbol: &str) -> &'static str {
    if is_coin_m(symbol) { PATH_TIME } else { crate::execution::rest::perp::PATH_TIME }
}

/// The words a `Quanto` futures client is refused in, every order ([`deny_unbuilt`]) and every request
/// ([`RestTransportDapiPaths`]), nothing sent: binance lists a USDⓈ-M (`Linear`) and a COIN-M (`Inverse`) book and no third.
pub const NO_QUANTO_BOOK: &str = "binance has no quanto futures book (its books are USDⓈ-M, linear, and COIN-M, \
                                  inverse): nothing was sent";

/// The code of [`NO_QUANTO_BOOK`]'s refusal on the wire path: local, like [`E_NO_ALGO_SERVICE`].
pub const E_NO_QUANTO_BOOK: i64 = -9_002;

/// What one request of the USDⓈ-M code becomes on a COIN-M host.
enum Routed {
    /// The request as it is (not a futures path).
    Pass,
    /// Sent to `path` with `params`; `only` keeps the answer's rows of that one symbol.
    To { path: String, params: Vec<(String, String)>, only: Option<String> },
    /// Answered here, with no call: what dapi's missing Algo Service amounts to.
    Answer(Result<Value, VenueApiError>),
}

/// The COIN-M twin of a USDⓈ-M request (the module doc's table).
fn route(path: &str, method: &str, params: &[(&str, String)]) -> Routed {
    let owned = |ps: &[(&str, String)]| -> Vec<(String, String)> {
        ps.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect()
    };
    match path {
        // No Algo Service on dapi: nothing rests there and nothing ever did, so its lists are empty and a
        // lookup or a cancel of one of its orders is the venue's "no such order".
        crate::execution::rest::algo::PATH_OPEN_ALGO_ORDERS
        | crate::execution::rest::algo::PATH_ALL_ALGO_ORDERS => {
            return Routed::Answer(Ok(json!([])));
        }
        crate::execution::rest::algo::PATH_ALGO_ORDER => {
            return Routed::Answer(Err(match method {
                "POST" => VenueApiError {
                    code: E_NO_ALGO_SERVICE,
                    msg: "binance COIN-M (dapi) has no Algo Service: a conditional order is not built on \
                          this book, nothing was sent"
                        .to_string(),
                },
                "DELETE" => {
                    VenueApiError { code: -2011, msg: "Unknown order sent.".to_string() }
                }
                _ => VenueApiError { code: -2013, msg: "Order does not exist.".to_string() },
            }));
        }
        // dapi's v1 bracket read takes a PAIR; v2 takes the symbol and answers `[{symbol, brackets}]`.
        crate::account::leverage::PATH_LEVERAGE_BRACKET => {
            return Routed::To {
                path: "/dapi/v2/leverageBracket".to_string(),
                params: owned(params),
                only: None,
            };
        }
        // dapi's positionRisk documents `pair=` (and `marginAsset=`), not `symbol=`: ask for the pair and keep
        // the asked symbol's rows, which is what fapi answers for `symbol=`.
        crate::execution::rest::perp::PATH_POSITIONS => {
            let only = params.iter().find(|(k, _)| *k == "symbol").map(|(_, v)| v.clone());
            let mut sent: Vec<(String, String)> =
                owned(params).into_iter().filter(|(k, _)| k != "symbol").collect();
            if let Some(symbol) = &only {
                sent.push((
                    "pair".to_string(),
                    symbol.split('_').next().unwrap_or(symbol).to_string(),
                ));
            }
            return Routed::To { path: "/dapi/v1/positionRisk".to_string(), params: sent, only };
        }
        _ => {}
    }
    match ["/fapi/v1/", "/fapi/v2/", "/fapi/v3/"].iter().find_map(|v| path.strip_prefix(v)) {
        Some(tail) => {
            Routed::To { path: format!("/dapi/v1/{tail}"), params: owned(params), only: None }
        }
        None => Routed::Pass,
    }
}

/// The code of the local refusal of a conditional order on a COIN-M host: not a venue code (so no table reads
/// it as "outcome unknown") and not one the mode re-probe reads.
pub const E_NO_ALGO_SERVICE: i64 = -9_001;

/// Keep only `only`'s rows of an array answer (a pair read answers every contract of the pair).
fn keep_rows(answer: Value, only: Option<&str>) -> Value {
    match (answer, only) {
        (Value::Array(rows), Some(symbol)) => Value::Array(
            rows.into_iter()
                .filter(|r| {
                    r.get("symbol")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case(symbol))
                })
                .collect(),
        ),
        (answer, _) => answer,
    }
}

/// **A transport that carries the USDⓈ-M code's requests to their COIN-M twins for a COIN-M client** (the
/// module doc): `.1` is the client's margining (`crate::execution::rest::perp::VenueRestBinancePerp::margining`, set with its host).
/// For a `Linear` (USDⓈ-M) client it is the inner transport, request for request; a `Quanto` one sends nothing
/// ([`NO_QUANTO_BOOK`]).
pub struct RestTransportDapiPaths<T>(pub T, pub Margining);

impl<T: RestTransport> RestTransportDapiPaths<T> {
    fn send(
        &self,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        call: impl Fn(&T, &str, &[(&str, String)]) -> Result<Value, VenueApiError>,
    ) -> Result<Value, VenueApiError> {
        match self.1 {
            Margining::Linear => call(&self.0, path, params),
            Margining::Inverse => match route(path, method, params) {
                Routed::Pass => call(&self.0, path, params),
                Routed::Answer(answer) => answer,
                Routed::To { path, params, only } => {
                    let params: Vec<(&str, String)> =
                        params.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                    call(&self.0, &path, &params).map(|answer| keep_rows(answer, only.as_deref()))
                }
            },
            Margining::Quanto => {
                Err(VenueApiError { code: E_NO_QUANTO_BOOK, msg: NO_QUANTO_BOOK.to_string() })
            }
        }
    }
}

impl<T: RestTransport> RestTransport for RestTransportDapiPaths<T> {
    fn signed(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        self.send(path, method, params, |t, p, ps| t.signed(base_url, p, method, ps, signer))
    }

    fn public(
        &self,
        base_url: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Value, VenueApiError> {
        self.send(path, "GET", params, |t, p, ps| t.public(base_url, p, ps))
    }

    fn signed_requery(
        &self,
        base_url: &str,
        path: &str,
        method: &str,
        params: &[(&str, String)],
        signer: &dyn Signer,
    ) -> Result<Value, VenueApiError> {
        self.send(path, method, params, |t, p, ps| {
            t.signed_requery(base_url, p, method, ps, signer)
        })
    }
}

/// The terminal refusal of what a COIN-M client (`margining` `Inverse`, its `crate::execution::rest::perp::VenueRestBinancePerp::margining`)
/// does not build, pushed after the synchronous `OrderSubmitted` with the wire untouched, or `None` (a USDⓈ-M
/// client, or an order the book takes): a stop, stop-limit or take-profit (dapi has no Algo Service, and its own
/// conditional order types are unmeasured here), and a GTD lifetime (the row's `timeInForce` lists GTC, IOC, FOK
/// and GTX). A `Quanto` client builds nothing ([`NO_QUANTO_BOOK`]).
#[must_use]
pub fn deny_unbuilt(margining: Margining, request: &OrderRequest) -> Option<Event> {
    let reason = match margining {
        Margining::Linear => return None,
        Margining::Quanto => NO_QUANTO_BOOK.to_string(),
        Margining::Inverse if crate::execution::rest::algo::is_algo_kind(&request.order_type) => format!(
            "binance COIN-M builds no `{}` order: dapi has no Algo Service and its own conditional types are \
             unmeasured; order refused",
            request.order_type
        ),
        Margining::Inverse if request.time_in_force == TimeInForce::Gtd => {
            "binance COIN-M has no good-till-date: its instrument rows list GTC, IOC, FOK and GTX; order refused"
                .to_string()
        }
        Margining::Inverse => return None,
    };
    Some(Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: request.order_client_id.clone(),
        reason: reason.into(),
        timestamp: request.timestamp,
    }))
}

/// **What a COIN-M mount is**, from its own `dapi` row.
#[derive(Debug, Clone, PartialEq)]
pub struct CoinMMount {
    /// The order grid (tick, whole-contract step and minimum, the market-order cap). `contract_size` is left
    /// unset: the face is [`Self::value`]'s, never a multiplier (decision 0170).
    pub properties: SymbolProperties,
    /// `Inverse(contractSize)`: one contract is `contractSize` US dollars.
    pub value: ContractValue,
    /// The settle coin (the row's `marginAsset`): the coin the engine keeps its books in (decision 0258).
    pub coin: String,
}

/// [`CoinMMount`] of the engine symbol `symbol` from a `dapi` exchangeInfo body. PURE.
///
/// # Errors
/// The row is absent or not TRADING, states no positive `contractSize`, is not coin-margined, or names no coin.
pub fn mount_of(symbol: &str, info: &Value) -> Result<CoinMMount, String> {
    let wire = crate::family::execution::dispatch::split_symbol(&symbol.to_ascii_uppercase()).0;
    let label = perp_series_symbol(&wire);
    let row = crate::catalog::parse_dapi(info)
        .into_iter()
        .find(|r| r.raw_symbol == label)
        .ok_or_else(|| format!("the network's COIN-M list has no TRADING row for {wire}"))?;
    let face = row.properties.contract_size;
    if !(face.is_finite() && face > 0.0) {
        return Err(format!("{wire}'s COIN-M row states no positive contractSize ({face})"));
    }
    let value = ContractValue::of(&row.contract, face)
        .filter(|v| v.kind == NotionalKind::Inverse)
        .ok_or_else(|| {
            format!("{wire}'s row is not a coin-margined contract ({:?})", row.contract)
        })?;
    let coin = row
        .settle_asset()
        .filter(|c| !c.is_empty())
        .ok_or_else(|| format!("{wire}'s COIN-M row names no settle coin"))?
        .to_string();
    Ok(CoinMMount {
        properties: SymbolProperties { contract_size: 0.0, ..row.properties },
        value,
        coin,
    })
}

/// [`mount_of`] over the `dapi` list at `dapi_rest`, the account's network's COIN-M origin
/// (`crate::execution`'s `BinanceUrls::dapi_rest`; one public GET of about 60 KB, weight 1).
///
/// # Errors
/// The list could not be read, or [`mount_of`]'s.
pub fn fetch_mount(dapi_rest: &str, symbol: &str) -> Result<CoinMMount, String> {
    let transport =
        UreqTransport::new("binance").with_rate_gate(crate::transport::ratelimit::perp_rest_gate());
    let info = transport
        .public(dapi_rest, PATH_EXCHANGE_INFO, &[])
        .map_err(|e| format!("{dapi_rest}{PATH_EXCHANGE_INFO} could not be read: {e}"))?;
    mount_of(symbol, &info)
}

/// The `balance` of `coin`'s row of a `/dapi/v1/balance` body: the coin's wallet cash, its realized PnL, fees
/// and funding already in it (decision 0258 item 4 adopts it). `None` when the answer lists no row for `coin`.
///
/// # Errors
/// A body that is not a JSON array.
pub fn parse_coin_balance(body: &str, coin: &str) -> Result<Option<f64>, String> {
    let rows: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = rows.as_array().ok_or("the balance answer is not a JSON array")?;
    Ok(rows
        .iter()
        .find(|r| {
            r.get("asset").and_then(Value::as_str).is_some_and(|a| a.eq_ignore_ascii_case(coin))
        })
        .and_then(|r| r.get("balance").and_then(json_number)))
}

#[path = "coin_m_tests.rs"]
#[cfg(test)]
mod coin_m_tests;
