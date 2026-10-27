//! **bybit's READ of an account's leverage** (decision 0182): what the gatewayhub's lane asks the
//! venue before a start (can this account safely be asked to hold N?) and after it (does it now hold
//! N?). Three signed GETs through this crate's own signer and transport, and NO write: the one write is
//! `crate::perp::BybitPerpRest::set_leverage`, fired once by the exec thread at the start and, for a
//! symbol admitted later, once by `crate::exec::write_symbol_leverage`
//! (`vike_bridge_core::leverage_read`'s module doc has the shape).
//!
//! | read | endpoint | what is taken from it |
//! |---|---|---|
//! | the account's mode | `GET /v5/account/info` | `marginMode` must be `REGULAR_MARGIN`; `ISOLATED_MARGIN` and `PORTFOLIO_MARGIN` are refused |
//! | what is held now | `GET /v5/position/list?category=linear&symbol=` | EXACTLY ONE row (the adapter orders `positionIdx 0`, one-way: hedge mode answers two rows, portfolio margin an empty `leverage`; a `tradeMode` of 1 is an isolated symbol and is refused), its `leverage`, and whether its `size` is non-zero. A FLAT one-way symbol answers one row with `size "0"` (measured on the demo host, 2026-10-26) |
//! | the symbol's maximum | `GET /v5/market/instruments-info?category=linear&symbol=` | `leverageFilter.maxLeverage`, the row the exec thread already clamps to; a non-`LinearPerpetual` row is refused |
//!
//! ⚠ **The parsers follow the venue's DOCUMENTED shapes.** Two have since been captured from the demo
//! host (2026-10-26, `tests/bybit_modes_smoke.rs`): `account/info` (`marginMode`, `unifiedMarginStatus`)
//! and a flat one-way symbol's `position/list` row (ONE row, `positionIdx 0`, `size "0"`, `leverage`
//! present); the rest (portfolio margin's empty `leverage`, an isolated `tradeMode`) are the documented
//! ones. The tests below pin those shapes, and every unreadable answer is a refusal, so a wrong guess ends
//! at 1x with the reason, never at a wrong number on the venue. This module still never writes a mode:
//! `crate::modes` does, at the first order of a symbol.
//!
//! The transport is ungated (`UreqBybitTransport::new()` without a rate window): a start spends at most
//! three probe reads and seven poll reads, once, far under the venue's budget. Every read rides the
//! transport's SHORT-timeout agent (`signed_requery`, the one the resync and the confirm use), so a venue
//! that stalls costs a read five seconds, not thirty, and the start the lane is inside stays bounded.

use serde_json::{Value, json};
use vike_bridge_core::json::json_num;
use vike_bridge_core::leverage_read::{HeldLeverage, LeverageReader};
use vike_bridge_core::signer::BybitV5Signer;
use vike_model::{AccountTier, MarginMode};
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::credentials::api_credentials;

use crate::perp::parse_bybit_perp_instruments;
use crate::perp::{PATH_INSTRUMENTS, PATH_POSITION_LIST, VENUE, endpoints};
use crate::recon_client::parse_trade_mode;
use crate::transport::{BybitTransport, UreqBybitTransport, unwrap_envelope};

/// `GET /v5/account/info`.
pub const PATH_ACCOUNT_INFO: &str = "/v5/account/info";

/// The account's margin mode, from the `result` of `GET /v5/account/info`. PURE.
///
/// # Errors
/// Anything but `REGULAR_MARGIN`: the engine models plain cross margin, and a portfolio-margin
/// account has no leverage number per symbol at all.
pub fn check_margin_mode(result: &Value) -> Result<(), String> {
    match result.get("marginMode").and_then(Value::as_str) {
        Some("REGULAR_MARGIN") => Ok(()),
        Some("ISOLATED_MARGIN") => Err("the account is in isolated margin (marginMode \
             ISOLATED_MARGIN); the engine models cross margin"
            .to_string()),
        Some("PORTFOLIO_MARGIN") => Err("the account is in portfolio margin (marginMode \
             PORTFOLIO_MARGIN), which has no leverage number per symbol"
            .to_string()),
        Some(other) => {
            Err(format!("the account's marginMode is {other:?}, which is not plain cross margin"))
        }
        None => Err(
            "GET /v5/account/info carries no marginMode, so the account's margin mode is unknown"
                .to_string(),
        ),
    }
}

/// What the account holds on `symbol`, from the `result` of `GET /v5/position/list`. PURE.
///
/// # Errors
/// No row, several rows (hedge mode), a hedge `positionIdx`, an isolated `tradeMode`, or a `leverage`
/// that is empty (portfolio margin) or not a figure of at least 1: each means "not the one-way cross
/// account this reads".
pub fn parse_position(result: &Value, symbol: &str) -> Result<HeldLeverage, String> {
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
        .ok_or_else(|| "GET /v5/position/list carries no `list`".to_string())?;
    let row = match rows.as_slice() {
        [] => return Err(format!("the venue answered no position row for {symbol}")),
        [one] => *one,
        many => {
            return Err(format!(
                "the venue answered {} position rows for {symbol}: the account is in hedge mode, \
                 and this adapter orders one-way (positionIdx 0)",
                many.len()
            ));
        }
    };
    if let Some(idx) = row.get("positionIdx").and_then(json_num)
        && idx != 0.0
    {
        return Err(format!("the position row is positionIdx {idx} (hedge mode), not one-way"));
    }
    // A classic (non-UTA) REGULAR_MARGIN account can hold one symbol isolated: `account/info` cannot
    // say so, this row does. An ABSENT `tradeMode` is cross, as everywhere else in this crate.
    if parse_trade_mode(row) == MarginMode::Isolated {
        return Err("the position row is tradeMode 1 (isolated margin on this symbol); the \
                    engine models cross margin"
            .to_string());
    }
    let held = row
        .get("leverage")
        .and_then(json_num)
        .filter(|x| x.is_finite() && *x >= 1.0)
        .ok_or_else(|| {
            "the position row carries no usable leverage (portfolio margin answers an empty one)"
                .to_string()
        })?;
    // An unreadable size must not read as "flat": the open-position guard fails CLOSED.
    let position_open = row
        .get("size")
        .and_then(json_num)
        .map(|size| size != 0.0)
        .ok_or_else(|| "the position row carries no usable size".to_string())?;
    Ok(HeldLeverage { held, position_open })
}

/// The symbol's published maximum leverage, from the WHOLE `instruments-info` envelope. PURE.
///
/// # Errors
/// No row for `symbol`, or a row that is not a linear perpetual (the exec plane refuses those).
pub fn parse_max_leverage(envelope: &Value, symbol: &str) -> Result<Option<f64>, String> {
    let rows = parse_bybit_perp_instruments(envelope);
    let row = rows
        .get(&symbol.to_uppercase())
        .ok_or_else(|| format!("instruments-info lists no linear contract {symbol}"))?;
    if let Some(kind) = row.contract_type.as_deref()
        && kind != "LinearPerpetual"
    {
        return Err(format!("{symbol} is a {kind}; this adapter trades linear perpetuals only"));
    }
    Ok(row.max_leverage)
}

/// The reader for one account's mounted symbol over this crate's signed transport.
pub struct BybitLeverageReader<T: BybitTransport> {
    signer: BybitV5Signer,
    transport: T,
    base_url: String,
    symbol: String,
}

impl<T: BybitTransport> BybitLeverageReader<T> {
    /// A reader over `transport`, signing with `signer`, against `base_url`, for `symbol`.
    pub fn new(
        signer: BybitV5Signer,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        BybitLeverageReader { signer, transport, base_url: base_url.into(), symbol: symbol.into() }
    }

    /// One signed GET. The `Err` is the venue's own words and never a credential.
    fn get(&self, what: &str, path: &str, params: &[(&str, Value)]) -> Result<Value, String> {
        self.transport
            .signed_requery(&self.base_url, path, "GET", params, &self.signer)
            .map_err(|e| format!("{what} could not be read: {e}"))
    }

    /// [`Self::get`] with the `{retCode, retMsg, result}` envelope unwrapped.
    fn result(&self, what: &str, path: &str, params: &[(&str, Value)]) -> Result<Value, String> {
        unwrap_envelope(self.get(what, path, params)?)
            .map_err(|e| format!("{what} could not be read: {e}"))
    }
}

impl<T: BybitTransport> LeverageReader for BybitLeverageReader<T> {
    fn check_account(&self) -> Result<(), String> {
        check_margin_mode(&self.result("account/info", PATH_ACCOUNT_INFO, &[])?)
    }

    fn venue_max(&self) -> Result<Option<f64>, String> {
        let envelope = self.get(
            "instruments-info",
            PATH_INSTRUMENTS,
            &[("category", json!("linear")), ("symbol", json!(self.symbol))],
        )?;
        parse_max_leverage(&envelope, &self.symbol)
    }

    fn held(&self) -> Result<HeldLeverage, String> {
        let result = self.result(
            "position/list",
            PATH_POSITION_LIST,
            &[("category", json!("linear")), ("symbol", json!(self.symbol))],
        )?;
        parse_position(&result, &self.symbol)
    }
}

/// The reader for `symbol` on the account whose credentials are `creds`, at the network `tier` selects
/// (`live` is mainnet). The caller (the gatewayhub's lane) has already checked that the table
/// (`vike_secrets::LEVERAGE_VENUES`) opens this tier.
///
/// # Errors
/// The account holds no bybit key pair.
pub fn leverage_reader(
    creds: &AccountCredentials<'_>,
    tier: AccountTier,
    symbol: &str,
) -> Result<BybitLeverageReader<UreqBybitTransport>, String> {
    let keys = api_credentials(creds, VENUE)
        .ok_or_else(|| "the account holds no bybit api key pair".to_string())?;
    let (rest_base, _) = endpoints(tier == AccountTier::Live);
    Ok(BybitLeverageReader::new(
        BybitV5Signer::new(&keys, vike_model::now_ms),
        UreqBybitTransport::new(),
        rest_base,
        symbol,
    ))
}

#[path = "leverage_read_tests.rs"]
#[cfg(test)]
mod leverage_read_tests;
