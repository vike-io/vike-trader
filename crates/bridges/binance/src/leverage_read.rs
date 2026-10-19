//! **binance's READ of an account's leverage** (decision 0182): what the gatewayhub's lane asks the
//! venue before a start (can this account safely be asked to hold N?) and after it (does it now hold
//! N?). Signed GETs through this crate's own signer and transport, and NO write: the one write is
//! `crate::perp::BinancePerpRest::set_leverage` (`POST /fapi/v1/leverage`, the figure cast `as i64`),
//! fired once by the exec thread at the start and, for a perpetual admitted later, once by
//! `crate::exec::write_symbol_leverage` (`vike_bridge_core::leverage_read`'s module doc has the shape).
//! USDⓈ-M futures only: a spot symbol has no leverage and is refused.
//!
//! | read | endpoint | what is taken from it |
//! |---|---|---|
//! | the account's mode AND what is held | `GET /fapi/v2/positionRisk?symbol=` (the read `recon_client` already makes) | EXACTLY ONE row with `positionSide BOTH` (hedge mode answers LONG and SHORT), `marginType` `cross`, its `leverage`, and whether `positionAmt` is non-zero |
//! | the symbol's maximum (OPTIONAL) | `GET /fapi/v1/leverageBracket?symbol=` (signed) | the highest `initialLeverage` of the brackets. ⚠ Failure-tolerant: an unreadable bracket is "no maximum known", never a refusal |
//!
//! There is no separate account-mode endpoint here, so [`LeverageReader::check_account`] and
//! [`LeverageReader::held`] read the same row (two GETs of weight 5 per probe). A portfolio-margin
//! account answers `/fapi` with an error, which is a refusal ("could not be read"). The bracket is not
//! required for correctness: a figure the venue rejects never becomes held, so the poll ends in an
//! error naming both numbers; the bracket only makes that refusal early and readable.
//!
//! ⚠ **The parsers follow the venue's DOCUMENTED shapes**; `positionRisk` is read by `recon_client`
//! (`tests/binance_reconcile_smoke.rs`), but `leverageBracket` has been proven only as the
//! unauthenticated `-2014` answer (`crate::exec`'s `leverage_for`). A live demo drill is owed. The
//! transport is ungated: a start spends at most three probe reads and seven poll reads, each on the
//! transport's SHORT-timeout agent (`signed_requery`, the one the resync and the confirm use), so a
//! venue that stalls costs a read five seconds, not thirty.

use serde_json::Value;
use vike_bridge_core::json::json_num;
use vike_bridge_core::leverage_read::{HeldLeverage, LeverageReader};
use vike_bridge_core::signer::{BinanceHmacSigner, Signer};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::credentials::api_credentials;

use crate::VENUE;
use crate::family::exec_loop::split_symbol;
use crate::perp::{DEMO_FAPI_REST, MAINNET_FAPI_REST, PATH_POSITIONS};

/// `GET /fapi/v1/leverageBracket`.
pub const PATH_LEVERAGE_BRACKET: &str = "/fapi/v1/leverageBracket";

/// What the account holds on `symbol`, and whether its mode is the one the engine models, from the
/// body of `GET /fapi/v2/positionRisk`. PURE.
///
/// # Errors
/// No row, several rows or a non-`BOTH` row (hedge mode), a non-cross `marginType` (or none), or a
/// `leverage` that is not a figure of at least 1.
pub fn parse_position_risk(body: &Value, symbol: &str) -> Result<HeldLeverage, String> {
    let rows: Vec<&Value> = body
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| {
                    r.get("symbol")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case(symbol))
                })
                .collect()
        })
        .ok_or_else(|| "positionRisk answered no array".to_string())?;
    let row = match rows.as_slice() {
        [] => return Err(format!("the venue answered no positionRisk row for {symbol}")),
        [one] => *one,
        many => {
            return Err(format!(
                "the venue answered {} positionRisk rows for {symbol}: the account is in hedge \
                 mode, and this adapter orders one-way (positionSide BOTH)",
                many.len()
            ));
        }
    };
    if row.get("positionSide").and_then(Value::as_str).is_some_and(|side| side != "BOTH") {
        return Err("the positionRisk row is a hedge-mode side (LONG/SHORT), not BOTH".to_string());
    }
    match row.get("marginType").and_then(Value::as_str) {
        Some(mode) if mode.eq_ignore_ascii_case("cross") => {}
        Some(mode) => {
            return Err(format!(
                "the symbol is on {mode} margin; the engine models cross margin (set it to cross \
                 on the venue first)"
            ));
        }
        None => {
            return Err(
                "positionRisk carries no marginType, so the margin mode is unknown".to_string()
            );
        }
    }
    let held = row
        .get("leverage")
        .and_then(json_num)
        .filter(|x| x.is_finite() && *x >= 1.0)
        .ok_or_else(|| "the positionRisk row carries no usable leverage".to_string())?;
    // An unreadable amount must not read as "flat": the open-position guard fails CLOSED.
    let position_open = row
        .get("positionAmt")
        .and_then(json_num)
        .map(|amt| amt != 0.0)
        .ok_or_else(|| "the positionRisk row carries no usable positionAmt".to_string())?;
    Ok(HeldLeverage { held, position_open })
}

/// The symbol's highest allowed leverage (the first bracket's `initialLeverage`), from the body of
/// `GET /fapi/v1/leverageBracket`, which is an array of `{symbol, brackets}` or one such object.
/// `None` for any shape this does not understand: the bracket is optional, so it never refuses.
#[must_use]
pub fn parse_bracket_max(body: &Value, symbol: &str) -> Option<f64> {
    let entry = match body {
        Value::Array(rows) => rows.iter().find(|r| {
            r.get("symbol").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case(symbol))
        })?,
        Value::Object(_) => body,
        _ => return None,
    };
    entry
        .get("brackets")?
        .as_array()?
        .iter()
        .filter_map(|b| b.get("initialLeverage").and_then(json_num))
        .filter(|x| x.is_finite() && *x >= 1.0)
        .reduce(f64::max)
}

/// The reader for one account's mounted USDⓈ-M symbol over this crate's signed transport.
pub struct BinanceLeverageReader<S: Signer, T: RestTransport> {
    signer: S,
    transport: T,
    base_url: String,
    /// The EXCHANGE symbol (`BTCUSDT`, the `.P` suffix removed), the one `set_leverage` posts.
    symbol: String,
}

impl<S: Signer, T: RestTransport> BinanceLeverageReader<S, T> {
    /// A reader over `transport`, signing with `signer`, against the fapi `base_url`, for `symbol`.
    pub fn new(
        signer: S,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        BinanceLeverageReader {
            signer,
            transport,
            base_url: base_url.into(),
            symbol: symbol.into(),
        }
    }

    /// One signed GET of `path?symbol=`. The `Err` is the venue's own words and never a credential.
    fn get(&self, what: &str, path: &str) -> Result<Value, String> {
        self.transport
            .signed_requery(
                &self.base_url,
                path,
                "GET",
                &[("symbol", self.symbol.clone())],
                &self.signer,
            )
            .map_err(|e| format!("{what} could not be read: {e}"))
    }
}

impl<S: Signer, T: RestTransport> LeverageReader for BinanceLeverageReader<S, T> {
    fn check_account(&self) -> Result<(), String> {
        parse_position_risk(&self.get("positionRisk", PATH_POSITIONS)?, &self.symbol).map(|_| ())
    }

    fn venue_max(&self) -> Result<Option<f64>, String> {
        // Optional, so any failure is "no maximum known": the poll is the judge of a rejected figure.
        Ok(self
            .get("leverageBracket", PATH_LEVERAGE_BRACKET)
            .ok()
            .and_then(|body| parse_bracket_max(&body, &self.symbol)))
    }

    fn held(&self) -> Result<HeldLeverage, String> {
        parse_position_risk(&self.get("positionRisk", PATH_POSITIONS)?, &self.symbol)
    }
}

/// The reader for `symbol` (the engine's spelling, `BTCUSDT.P`) on the account whose credentials are
/// `creds`, on the fapi host `tier` selects (`live` is mainnet). The caller (the gatewayhub's lane)
/// has already checked that the table (`vike_secrets::LEVERAGE_VENUES`) opens this tier.
///
/// # Errors
/// The account holds no binance key pair, or `symbol` is a spot symbol (no `.P`).
pub fn leverage_reader(
    creds: &AccountCredentials<'_>,
    tier: AccountTier,
    symbol: &str,
) -> Result<BinanceLeverageReader<BinanceHmacSigner, UreqTransport>, String> {
    let (api_symbol, is_perp) = split_symbol(symbol);
    if !is_perp {
        return Err(format!(
            "{symbol} is a binance SPOT symbol: leverage is a USDⓈ-M futures setting (a perpetual \
             is spelled with the .P suffix)"
        ));
    }
    let keys = api_credentials(creds, VENUE)
        .ok_or_else(|| "the account holds no binance api key pair".to_string())?;
    let base = if tier == AccountTier::Live { MAINNET_FAPI_REST } else { DEMO_FAPI_REST };
    Ok(BinanceLeverageReader::new(
        BinanceHmacSigner::new(&keys, vike_model::now_ms),
        UreqTransport::new("binance"),
        base,
        api_symbol,
    ))
}

#[path = "leverage_read_tests.rs"]
#[cfg(test)]
mod leverage_read_tests;
