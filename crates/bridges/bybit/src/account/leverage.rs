//! **bybit's READ of an account's leverage** (decision 0182): what the gatewayhub's lane asks the
//! venue before a start (can this account safely be asked to hold N?) and after it (does it now hold
//! N?). Three signed GETs through this crate's own signer and transport, and NO write: the one write is
//! `crate::execution::rest::VenueRestBybit::set_leverage`, fired once by the exec thread at the start and, for a
//! symbol admitted later, once by `crate::execution::write_symbol_leverage`
//! (`vike_bridge_core::leverage_read`'s module doc has the shape).
//!
//! | read | endpoint | what is taken from it |
//! |---|---|---|
//! | the account's mode | `GET /v5/account/info` | `marginMode` must be `REGULAR_MARGIN`; `ISOLATED_MARGIN` and `PORTFOLIO_MARGIN` are refused |
//! | what is held now | `GET /v5/position/list?category=linear&symbol=` | EXACTLY ONE row (the adapter orders `positionIdx 0`, one-way: hedge mode answers two rows, portfolio margin an empty `leverage`; a `tradeMode` of 1 is an isolated symbol and is refused), its `leverage`, and whether its `size` is non-zero. A FLAT one-way symbol answers one row with `size "0"` (measured on the demo host, 2026-10-26) |
//! | the symbol's maximum | `GET /v5/market/instruments-info?category=linear&symbol=` | `leverageFilter.maxLeverage`, the row the exec thread already clamps to; a non-`LinearPerpetual` row is refused |
//!
//! ⚠ **The parsers follow the venue's DOCUMENTED shapes.** Two have since been captured from the demo
//! host (2026-10-26, `tests/bybit_smokes/bybit_modes_smoke.rs`): `account/info` (`marginMode`, `unifiedMarginStatus`)
//! and a flat one-way symbol's `position/list` row (ONE row, `positionIdx 0`, `size "0"`, `leverage`
//! present); the rest (portfolio margin's empty `leverage`, an isolated `tradeMode`) are the documented
//! ones. The tests below pin those shapes, and every unreadable answer is a refusal, so a wrong guess ends
//! at 1x with the reason, never at a wrong number on the venue. This module still never writes a mode:
//! `crate::account::modes` does, at the first order of a symbol.
//!
//! The transport is ungated (`rest_transport_on` without a rate window): a start spends at most
//! three probe reads and seven poll reads, once, far under the venue's budget. Every read rides the
//! transport's SHORT-timeout agent (`signed_requery`, the one the resync and the confirm use), so a venue
//! that stalls costs a read five seconds, not thirty, and the start the lane is inside stays bounded.

use crate::signer::BybitV5Signer;
use serde_json::{Value, json};
use vike_bridge_core::json::json_number;
use vike_bridge_core::leverage::leverage_figure;
use vike_bridge_core::leverage_read::{HeldLeverage, LeverageReader};
use vike_bridge_core::signer::VenueClock;
use vike_model::{AccountTier, MarginMode};
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::credentials::api_credentials;

use crate::execution::rest::parse_bybit_perp_instruments;
use crate::execution::rest::{PATH_INSTRUMENTS, PATH_POSITION_LIST, VENUE};
use crate::reconcile::parse_trade_mode;
use crate::refusals::{BybitAccountRefusal, BybitLeverageRefusal};
use crate::transport::client::rest_transport_on;
use crate::transport::client::{FailedRequest, unwrap_envelope};
use vike_bridge_core::transport::json::{RestTransportJson, RestTransportJsonUreq};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, VenueUrls};

/// `GET /v5/account/info`.
pub const PATH_ACCOUNT_INFO: &str = "/v5/account/info";

/// The account's margin mode, from the `result` of `GET /v5/account/info`. PURE.
///
/// # Errors
/// Anything but `REGULAR_MARGIN`: the engine models plain cross margin, and a portfolio-margin
/// account has no leverage number per symbol at all.
pub fn check_margin_mode(result: &Value) -> Result<(), BybitLeverageRefusal> {
    match result.get("marginMode").and_then(Value::as_str) {
        Some("REGULAR_MARGIN") => Ok(()),
        Some("ISOLATED_MARGIN") => Err(BybitLeverageRefusal::IsolatedAccount),
        Some("PORTFOLIO_MARGIN") => Err(BybitLeverageRefusal::PortfolioAccount),
        Some(other) => Err(BybitLeverageRefusal::MarginModeWord { word: other.to_string() }),
        None => Err(BybitLeverageRefusal::NoMarginMode),
    }
}

/// What the account holds on `symbol`, from the `result` of `GET /v5/position/list`. PURE.
///
/// # Errors
/// No row, several rows (hedge mode), a hedge `positionIdx`, an isolated `tradeMode`, or a `leverage`
/// that is empty (portfolio margin) or not a figure of at least 1: each means "not the one-way cross
/// account this reads".
pub fn parse_position(result: &Value, symbol: &str) -> Result<HeldLeverage, BybitLeverageRefusal> {
    let rows: Vec<&Value> = result
        .get("list")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|r| {
                    r.get("symbol")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case(symbol))
                })
                .collect()
        })
        .ok_or(BybitLeverageRefusal::NoPositionList)?;
    let row = match rows.as_slice() {
        [] => return Err(BybitLeverageRefusal::NoPositionRow { symbol: symbol.to_string() }),
        [one] => *one,
        many => {
            return Err(BybitLeverageRefusal::HedgeRows {
                rows: many.len(),
                symbol: symbol.to_string(),
            });
        }
    };
    if let Some(index) = row.get("positionIdx").and_then(json_number)
        && index != 0.0
    {
        return Err(BybitLeverageRefusal::HedgeIndex { index });
    }
    // A classic (non-UTA) REGULAR_MARGIN account can hold one symbol isolated: `account/info` cannot
    // say so, this row does. An ABSENT `tradeMode` is cross, as everywhere else in this crate.
    if parse_trade_mode(row) == MarginMode::Isolated {
        return Err(BybitLeverageRefusal::IsolatedSymbol);
    }
    let held = row
        .get("leverage")
        .and_then(leverage_figure)
        .ok_or(BybitLeverageRefusal::NoUsableLeverage)?;
    // An unreadable size must not read as "flat": the open-position guard fails CLOSED.
    let position_open = row
        .get("size")
        .and_then(json_number)
        .map(|size| size != 0.0)
        .ok_or(BybitLeverageRefusal::NoUsableSize)?;
    Ok(HeldLeverage { held, position_open })
}

/// The symbol's published maximum leverage, from the WHOLE `instruments-info` envelope. PURE.
///
/// # Errors
/// No row for `symbol`, or a row that is not a linear perpetual (the exec plane refuses those).
pub fn parse_max_leverage(
    envelope: &Value,
    symbol: &str,
) -> Result<Option<f64>, BybitLeverageRefusal> {
    let rows = parse_bybit_perp_instruments(envelope);
    let row = rows
        .get(&symbol.to_uppercase())
        .ok_or_else(|| BybitLeverageRefusal::NotListed { symbol: symbol.to_string() })?;
    if let Some(kind) = row.contract_type.as_deref()
        && kind != "LinearPerpetual"
    {
        return Err(BybitLeverageRefusal::NotLinearPerpetual {
            symbol: symbol.to_string(),
            kind: kind.to_string(),
        });
    }
    Ok(row.max_leverage)
}

/// The reader for one account's mounted symbol over this crate's signed transport.
pub struct LeverageReaderBybit<T: RestTransportJson> {
    signer: BybitV5Signer,
    transport: T,
    base_url: String,
    symbol: String,
}

impl<T: RestTransportJson> LeverageReaderBybit<T> {
    /// A reader over `transport`, signing with `signer`, against `base_url`, for `symbol`.
    pub fn new(
        signer: BybitV5Signer,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        LeverageReaderBybit { signer, transport, base_url: base_url.into(), symbol: symbol.into() }
    }

    /// One signed GET. The `Err` is [`FailedRequest`]'s words (the request, the venue's code and message) and never a
    /// credential.
    fn get(&self, path: &str, params: &[(&str, Value)]) -> Result<Value, String> {
        self.transport
            .signed_requery(&self.base_url, path, "GET", params, &self.signer)
            .map_err(|e| FailedRequest::new("GET", path, params, e).to_string())
    }

    /// [`Self::get`] with the `{retCode, retMsg, result}` envelope unwrapped.
    fn result(&self, path: &str, params: &[(&str, Value)]) -> Result<Value, String> {
        self.transport
            .signed_requery(&self.base_url, path, "GET", params, &self.signer)
            .and_then(unwrap_envelope)
            .map_err(|e| FailedRequest::new("GET", path, params, e).to_string())
    }
}

impl<T: RestTransportJson> LeverageReader for LeverageReaderBybit<T> {
    fn check_account(&self) -> Result<(), String> {
        check_margin_mode(&self.result(PATH_ACCOUNT_INFO, &[])?).map_err(|r| r.to_string())
    }

    fn venue_max(&self) -> Result<Option<f64>, String> {
        let (book, wire) = self.book()?;
        let envelope =
            self.get(PATH_INSTRUMENTS, &[("category", json!(book)), ("symbol", json!(wire))])?;
        parse_max_leverage(&envelope, &wire).map_err(|r| r.to_string())
    }

    fn held(&self) -> Result<HeldLeverage, String> {
        let (book, wire) = self.book()?;
        let result =
            self.result(PATH_POSITION_LIST, &[("category", json!(book)), ("symbol", json!(wire))])?;
        parse_position(&result, &wire).map_err(|r| r.to_string())
    }
}

impl<T: RestTransportJson> LeverageReaderBybit<T> {
    /// The book this USER name's leverage lives in and the venue's name of it (decisions 0231 and 0290), its KEPT
    /// row's (`crate::instrument::class::venue_symbol`): `linear` for a perpetual or a dated linear future, `inverse`
    /// for a coin-margined contract. A name with no row is refused in words (no read in a guessed book). A SPOT pair
    /// holds no leverage: the read refuses in words, and the lane leaves the symbol at the engine's own rate.
    fn book(&self) -> Result<(&'static str, String), String> {
        match crate::instrument::class::venue_symbol(&self.symbol)? {
            (_, crate::history::Category::Spot) => {
                Err(BybitLeverageRefusal::SpotPairRead { symbol: self.symbol.clone() }.to_string())
            }
            (wire, book) => Ok((book.wire(), wire)),
        }
    }
}

/// The reader for `symbol` on the account whose credentials are `creds`, on the account's signed REST
/// host: `urls`' `trade` `rest` row at the type `tier` names (`live` is mainnet; a demo account never
/// falls back to a live row). The caller (the gatewayhub's lane) has already checked that the table
/// (`vike_venues::LEVERAGE_VENUES`) opens this tier.
///
/// # Errors
/// The account holds no bybit key pair, or its rows hold no such host (the refusal's words).
pub fn leverage_reader(
    creds: &AccountCredentials<'_>,
    urls: &VenueUrls,
    tier: AccountTier,
    symbol: &str,
) -> Result<LeverageReaderBybit<RestTransportJsonUreq>, String> {
    let keys =
        api_credentials(creds, VENUE).ok_or_else(|| BybitAccountRefusal::NoKeyPair.to_string())?;
    let rest_base = urls
        .url(UrlType::of_tier(tier), UrlChannel::Trade, UrlProtocol::Rest, "")
        .map_err(|refusal| refusal.to_string())?;
    // A one-shot client: it reads the venue's clock for itself (owner ruling B-4).
    let clock = VenueClock::default();
    Ok(LeverageReaderBybit::new(
        BybitV5Signer::new(&keys, clock.ms()),
        rest_transport_on(&clock),
        rest_base,
        symbol,
    ))
}

#[path = "leverage_tests.rs"]
#[cfg(test)]
mod leverage_tests;
