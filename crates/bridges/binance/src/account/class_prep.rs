//! **What the bridge does for a symbol of a class beyond the plain perpetual, so it trades without a hand step the
//! bridge may take** (owner rule 2026-10-26: any ticker; decision 0230). DEMO host only, like every account setting
//! this crate writes (`crate::account::modes::writes_modes`): a LIVE account's leverage is never touched.
//!
//! Two facts MEASURED on the demo host on 2026-10-30 (`crates/bridges/binance/tests/binance_smokes/binance_classes_smoke.rs`):
//!
//! * **A symbol the account never traded reads leverage `0`, and for some symbols that refuses every order.**
//!   `POST /fapi/v1/order` answered `-2027` ("Exceeded the maximum allowable position at current leverage.")
//!   on `TSLAUSDT` (a TradFi equity perpetual, bracket maximum 5x) and on a weekly future (maximum 10x), while a
//!   quarterly and a gold perpetual (higher maxima) took the same minimum order at the same `0`. After
//!   `POST /fapi/v1/leverage` (`leverage=2`) the identical orders were accepted. The venue's default for a new
//!   symbol is evidently above the symbol's own maximum, so the first order is refused until a leverage is set
//!   ([`ensure_symbol_leverage`]: the account's configured figure, capped at the symbol's bracket maximum, written
//!   once, before the first order, only when the symbol reads `0` and holds nothing).
//! * **A TradFi perpetual needs the account's TradFi-Perps agreement**: every order on one answers `-4411`
//!   ("Please sign TradFi-Perps agreement contract fapi.") until the account holder accepts it.
//!
//! ⚠ **The bridge NEVER signs the agreement, on a demo account too** (the owner's ruling of 2026-11-15 on decision
//! 0230's item 5): it is the holder's consent, given once in the Binance app. A `-4411` reject carries
//! [`agreement_hand_step`] and nothing is sent.

use serde_json::Value;
use vike_bridge_core::json::json_number;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::RestTransport;

use crate::account::leverage::{PATH_LEVERAGE_BRACKET, parse_bracket_max};
use crate::account::modes::writes_modes;
use crate::execution::rest::perp::{PATH_POSITIONS, PATH_SET_LEVERAGE, VenueRestBinancePerp};
use crate::refusals::BinanceLeverageRefusal;

/// `-4411`: the account has not signed the TradFi-Perps agreement (every order on a `TRADIFI_PERPETUAL` symbol).
pub const E_TRADFI_AGREEMENT: i64 = -4411;
/// `-2027`: "Exceeded the maximum allowable position at current leverage."
pub const E_POSITION_AT_LEVERAGE: i64 = -2027;

/// What the first-order leverage probe found and did for one symbol.
#[derive(Debug, Clone, PartialEq)]
pub enum LeverageHeal {
    /// The account already holds a leverage on the symbol (a figure of 1 or more): nothing was written.
    Held(f64),
    /// The symbol read `0` and held nothing: the figure was written and the venue accepted it (or a write
    /// with no verdict reads back held: the figure the symbol reads).
    Written(i64),
    /// The symbol read `0` and the venue REFUSED the write: the refusal carries what it answered.
    Refused(BinanceLeverageRefusal),
    /// The row could not be read, or the symbol holds a position or reads no `0`: nothing was written. Or a
    /// write got no verdict and does not read back held: the sentence says so.
    Unread(String),
    /// Not the demo host: nothing was read or written.
    LeftAlone,
}

/// **Give a symbol the account never traded a leverage, before its first order.** Reads
/// `GET /fapi/v2/positionRisk?symbol=` (one row: the leverage and the position); a symbol that reads a leverage of
/// 1 or more is [`LeverageHeal::Held`] and costs no write. A symbol that reads `0` and holds NOTHING (a position
/// on a never-traded symbol is impossible, so a held amount is a surprise and is left alone) gets ONE
/// `POST /fapi/v1/leverage` of `min(rest.leverage, the symbol's bracket maximum)`, the maximum read from
/// `GET /fapi/v1/leverageBracket` (optional: an unreadable bracket writes the configured figure and lets the
/// venue judge it). Never blocks an order.
#[must_use]
pub fn ensure_symbol_leverage<S: Signer, T: RestTransport>(
    rest: &VenueRestBinancePerp<S, T>,
) -> LeverageHeal {
    if !writes_modes(rest.ty) {
        return LeverageHeal::LeftAlone;
    }
    let symbol = rest.symbol.as_str();
    let row = match risk_row(rest) {
        Ok(row) => row,
        Err(why) => return LeverageHeal::Unread(why),
    };
    let held = row.get("leverage").and_then(json_number).unwrap_or(0.0);
    if held >= 1.0 {
        return LeverageHeal::Held(held);
    }
    if row.get("positionAmt").and_then(json_number).is_none_or(|amt| amt != 0.0) {
        return LeverageHeal::Unread(format!(
            "{symbol} reads a leverage of 0 but its position is not readable as flat: nothing written"
        ));
    }
    let configured = rest.leverage.floor();
    let cap = rest
        .transport
        .signed_requery(
            &rest.base_url,
            PATH_LEVERAGE_BRACKET,
            "GET",
            &[("symbol", symbol.to_string())],
            &rest.signer,
        )
        .ok()
        .and_then(|body| parse_bracket_max(&body, symbol));
    let figure = cap.map_or(configured, |cap| configured.min(cap.floor())) as i64;
    if figure < 1 {
        return LeverageHeal::Unread(format!("no whole leverage of 1 or more fits {symbol}"));
    }
    let params = [("symbol", symbol.to_string()), ("leverage", figure.to_string())];
    match rest.transport.signed(&rest.base_url, PATH_SET_LEVERAGE, "POST", &params, &rest.signer) {
        Ok(_) => LeverageHeal::Written(figure),
        // The write got no verdict, so it may have landed: the leverage the symbol reads now is the answer.
        Err(e) if crate::transport::error_codes::TAXONOMY.submit_outcome_unknown(e.code) => {
            match risk_row(rest).map(|row| row.get("leverage").and_then(json_number).unwrap_or(0.0))
            {
                Ok(read) if read >= 1.0 => LeverageHeal::Written(read as i64),
                _ => LeverageHeal::Unread(format!(
                    "the bridge's write of {figure}x got no verdict ({e}) and reading {symbol} back did not show \
                     a leverage held; the venue answers its first order -2027 while it still reads 0"
                )),
            }
        }
        Err(error) => LeverageHeal::Refused(BinanceLeverageRefusal::FirstWriteRefused {
            symbol: symbol.to_string(),
            figure,
            error,
        }),
    }
}

/// The symbol's one row of `GET /fapi/v2/positionRisk?symbol=` (its leverage and position), or why it could not
/// be read.
fn risk_row<S: Signer, T: RestTransport>(
    rest: &VenueRestBinancePerp<S, T>,
) -> Result<Value, String> {
    let symbol = rest.symbol.as_str();
    let rows = rest
        .transport
        .signed_requery(
            &rest.base_url,
            PATH_POSITIONS,
            "GET",
            &[("symbol", symbol.to_string())],
            &rest.signer,
        )
        .map_err(|e| format!("GET {PATH_POSITIONS} failed: {e}"))?;
    rows.as_array()
        .and_then(|rows| {
            rows.iter().find(|r| {
                r.get("symbol")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case(symbol))
            })
        })
        .cloned()
        .ok_or_else(|| format!("positionRisk answered no row for {symbol}"))
}

/// One log line for [`ensure_symbol_leverage`]: a write is a WARN (it changed the venue account), a refusal a WARN.
pub fn report_leverage<S: Signer, T: RestTransport>(
    rest: &VenueRestBinancePerp<S, T>,
    outcome: &LeverageHeal,
) {
    let (host, symbol) = (rest.base_url.as_str(), rest.symbol.as_str());
    match outcome {
        LeverageHeal::Written(figure) => {
            tracing::warn!(target: "vike_binance::account::class_prep", host, symbol, figure, "a symbol the account never traded had no leverage: the bridge WROTE one before its first order (the venue refuses the first order of some symbols at the default)")
        }
        LeverageHeal::Refused(why) => {
            tracing::warn!(target: "vike_binance::account::class_prep", host, symbol, "{why}")
        }
        LeverageHeal::Unread(why) => {
            tracing::warn!(target: "vike_binance::account::class_prep", host, symbol, "leverage not healed: {why}")
        }
        LeverageHeal::Held(_) | LeverageHeal::LeftAlone => {}
    }
}

/// The reason a `-4411` reject carries on either network: what the holder does, with the venue's code and text. PURE.
#[must_use]
pub fn agreement_hand_step(symbol: &str, msg: &str) -> String {
    format!(
        "binance: Binance requires the TradFi-Perps agreement for {symbol} ({E_TRADFI_AGREEMENT}: {msg}): accept \
         it once in the Binance app (Futures → TradFi), then send again. The bridge never signs it, on a demo \
         account either"
    )
}

/// The reason `-2027` carries: the venue's text and what it usually means on this venue. PURE.
#[must_use]
pub fn position_at_leverage_reason(msg: &str, demo_host: bool) -> String {
    let fix = if demo_host {
        "a DEMO account's symbol is given a leverage by the bridge before its first order, so this order is \
         bigger than the symbol's current leverage allows: send it smaller, or lower the leverage"
    } else {
        "set a leverage for the symbol in the Binance app (the bridge writes no LIVE account's leverage for a \
         symbol it never traded), or, if the symbol has traded, send the order smaller or lower the leverage"
    };
    format!(
        "binance: the venue refused the order ({E_POSITION_AT_LEVERAGE}: {msg}). On an account that never traded \
         the symbol this is the venue's default leverage being above the symbol's own maximum (a TradFi equity \
         perpetual is capped near 5x): {fix}"
    )
}

#[path = "class_prep_tests.rs"]
#[cfg(test)]
mod class_prep_tests;
