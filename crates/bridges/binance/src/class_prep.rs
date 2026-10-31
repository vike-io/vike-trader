//! **What the bridge does for a symbol of a class beyond the plain perpetual, so it trades without a hand step**
//! (owner rule 2026-10-26: any ticker; decision 0230). DEMO host only, like every account setting this crate
//! writes (`crate::modes::writes_modes`): a LIVE account's leverage and agreements are never touched.
//!
//! Two facts MEASURED on the demo host on 2026-10-30 (`crates/bridges/binance/tests/binance_classes_smoke.rs`):
//!
//! * **A symbol the account never traded reads leverage `0`, and for some symbols that refuses every order.**
//!   `POST /fapi/v1/order` answered `-2027` ("Exceeded the maximum allowable position at current leverage.")
//!   on `TSLAUSDT` (a TradFi equity perpetual, bracket maximum 5x) and on a weekly future (maximum 10x), while a
//!   quarterly and a gold perpetual (higher maxima) took the same minimum order at the same `0`. After
//!   `POST /fapi/v1/leverage` (`leverage=2`) the identical orders were accepted. The venue's default for a new
//!   symbol is evidently above the symbol's own maximum, so the first order is refused until a leverage is set
//!   ([`ensure_symbol_leverage`]: the account's configured figure, capped at the symbol's bracket maximum, written
//!   once, before the first order, only when the symbol reads `0` and holds nothing).
//! * **A TradFi perpetual needs the account's TradFi-Perps agreement**: every order on one answered `-4411`
//!   ("Please sign TradFi-Perps agreement contract fapi.") until `POST /fapi/v1/stock/contract` was sent, then
//!   the same order was accepted. The venue answers that POST with a plain-text 2xx body, which the transport
//!   reports as an unreadable answer to a write ([`vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS`]); an
//!   unreadable 2xx is therefore read as "sent", and the NEXT order proves it. The agreement cannot be unsigned,
//!   so the bridge signs it only on a reject that names it ([`AgreementMemory::sentence`]), never ahead of one,
//!   at most once per [`REPROBE_WINDOW`], and the order that met the reject is not resent.
//!
//! ⚠ A signature is the account holder's consent. The bridge gives it on the DEMO host only because a demo
//! account trades no real money; decision 0230 records that choice and what would reopen it. On a MAINNET host
//! the reject says what the holder must do and nothing is sent.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use serde_json::Value;
use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, RestTransport};

use crate::leverage_read::{PATH_LEVERAGE_BRACKET, parse_bracket_max};
use crate::modes::{REPROBE_WINDOW, writes_modes};
use crate::perp::{BinancePerpRest, PATH_POSITIONS, PATH_SET_LEVERAGE};

/// `-4411`: the account has not signed the TradFi-Perps agreement (every order on a `TRADIFI_PERPETUAL` symbol).
pub const E_TRADFI_AGREEMENT: i64 = -4411;
/// `-2027`: "Exceeded the maximum allowable position at current leverage."
pub const E_POSITION_AT_LEVERAGE: i64 = -2027;
/// `POST /fapi/v1/stock/contract`: sign the TradFi-Perps agreement (weight 0, no parameter beyond the signature).
pub const PATH_TRADFI_AGREEMENT: &str = "/fapi/v1/stock/contract";

/// What the first-order leverage probe found and did for one symbol.
#[derive(Debug, Clone, PartialEq)]
pub enum LeverageHeal {
    /// The account already holds a leverage on the symbol (a figure of 1 or more): nothing was written.
    Held(f64),
    /// The symbol read `0` and held nothing: the figure was written and the venue accepted it.
    Written(i64),
    /// The symbol read `0` and the venue REFUSED the write: the sentence says what it answered.
    Refused(String),
    /// The row could not be read, or the symbol holds a position or reads no `0`: nothing was written.
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
    rest: &BinancePerpRest<S, T>,
) -> LeverageHeal {
    if !writes_modes(&rest.base_url) {
        return LeverageHeal::LeftAlone;
    }
    let symbol = rest.symbol.as_str();
    let rows = match rest.transport.signed_requery(
        &rest.base_url,
        PATH_POSITIONS,
        "GET",
        &[("symbol", symbol.to_string())],
        &rest.signer,
    ) {
        Ok(rows) => rows,
        Err(e) => return LeverageHeal::Unread(format!("GET {PATH_POSITIONS} failed: {e}")),
    };
    let Some(row) = rows.as_array().and_then(|rows| {
        rows.iter().find(|r| {
            r.get("symbol").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case(symbol))
        })
    }) else {
        return LeverageHeal::Unread(format!("positionRisk answered no row for {symbol}"));
    };
    let held = row.get("leverage").and_then(json_num).unwrap_or(0.0);
    if held >= 1.0 {
        return LeverageHeal::Held(held);
    }
    if row.get("positionAmt").and_then(json_num).is_none_or(|amt| amt != 0.0) {
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
        Err(e) => LeverageHeal::Refused(format!(
            "binance: {symbol} reads leverage 0 on this account (never traded) and the venue refused the \
             bridge's write of {figure}x ({e}); the venue's default can exceed the symbol's maximum, which makes \
             it refuse the first order (-2027): set a leverage for the symbol in the Binance app"
        )),
    }
}

/// One log line for [`ensure_symbol_leverage`]: a write is a WARN (it changed the venue account), a refusal a WARN.
pub fn report_leverage<S: Signer, T: RestTransport>(
    rest: &BinancePerpRest<S, T>,
    outcome: &LeverageHeal,
) {
    let (host, symbol) = (rest.base_url.as_str(), rest.symbol.as_str());
    match outcome {
        LeverageHeal::Written(figure) => {
            tracing::warn!(target: "vike_binance::class_prep", host, symbol, figure, "a symbol the account never traded had no leverage: the bridge WROTE one before its first order (the venue refuses the first order of some symbols at the default)")
        }
        LeverageHeal::Refused(why) => {
            tracing::warn!(target: "vike_binance::class_prep", host, symbol, "{why}")
        }
        LeverageHeal::Unread(why) => {
            tracing::warn!(target: "vike_binance::class_prep", host, symbol, "leverage not healed: {why}")
        }
        LeverageHeal::Held(_) | LeverageHeal::LeftAlone => {}
    }
}

/// **The last signature of the TradFi-Perps agreement a `-4411` reject made**: when, and the sentence it gave.
/// Single flight, like [`crate::modes::Reprobe`] (the lock is held across the POST, so a burst of rejects is one
/// signature). Process-wide for the same reason and with the same upgrade condition: the agreement is
/// per account and the demo roster has one binance account.
#[derive(Default)]
pub struct AgreementMemory(Mutex<Option<(Instant, String)>>);

/// The process's memory ([`agreement_sentence`]).
static AGREEMENT: AgreementMemory = AgreementMemory::new();

impl AgreementMemory {
    /// A memory that has signed nothing.
    #[must_use]
    pub const fn new() -> Self {
        AgreementMemory(Mutex::new(None))
    }

    /// The sentence a `-4411` reject (`msg`: the venue's text) carries as of `now`: the standing answer of a
    /// signature made less than [`REPROBE_WINDOW`] ago, else the result of signing ([`PATH_TRADFI_AGREEMENT`]).
    pub fn sentence<S: Signer, T: RestTransport>(
        &self,
        rest: &BinancePerpRest<S, T>,
        msg: &str,
        now: Instant,
    ) -> String {
        let mut last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, said)) = last.as_ref()
            && now.saturating_duration_since(*at) < REPROBE_WINDOW
        {
            return said.clone();
        }
        let sent =
            rest.transport.signed(&rest.base_url, PATH_TRADFI_AGREEMENT, "POST", &[], &rest.signer);
        let said = match sent {
            // An unreadable 2xx on a write is the venue's plain-text answer (MEASURED): the request landed.
            Ok(_) => sent_sentence(msg, "accepted it"),
            Err(e) if e.code == E_TIMEOUT_AMBIGUOUS => sent_sentence(msg, "answered in plain text"),
            Err(e) => format!(
                "binance: the account has not signed the TradFi-Perps agreement, so the venue refuses every \
                 order on a TradFi perpetual ({E_TRADFI_AGREEMENT}: {msg}). The bridge tried to sign it on this \
                 DEMO account and the venue refused ({e}): sign the agreement in the Binance app"
            ),
        };
        tracing::warn!(target: "vike_binance::class_prep", host = rest.base_url.as_str(), "{said}");
        *last = Some((now, said.clone()));
        said
    }
}

fn sent_sentence(msg: &str, how: &str) -> String {
    format!(
        "binance: the account had not signed the TradFi-Perps agreement, so the venue refused this order \
         ({E_TRADFI_AGREEMENT}: {msg}). The bridge signed it just now on this DEMO account (the venue {how}): \
         send the order again. If the venue still answers {E_TRADFI_AGREEMENT}, sign the agreement in the \
         Binance app"
    )
}

/// The sentence a `-4411` reject carries as of now: on the demo host the signature's result
/// ([`AgreementMemory::sentence`]); anywhere else the hand step, with nothing sent.
#[must_use]
pub fn agreement_sentence<S: Signer, T: RestTransport>(
    rest: &BinancePerpRest<S, T>,
    msg: &str,
) -> String {
    agreement_sentence_at(&AGREEMENT, rest, msg, Instant::now())
}

/// [`agreement_sentence`] over an explicit memory and clock (the tests').
pub fn agreement_sentence_at<S: Signer, T: RestTransport>(
    memory: &AgreementMemory,
    rest: &BinancePerpRest<S, T>,
    msg: &str,
    now: Instant,
) -> String {
    if writes_modes(&rest.base_url) {
        memory.sentence(rest, msg, now)
    } else {
        agreement_hand_step(msg)
    }
}

/// The reject sentence when the bridge may not sign (a live host): what the holder must do. PURE.
#[must_use]
pub fn agreement_hand_step(msg: &str) -> String {
    format!(
        "binance: the account has not signed the TradFi-Perps agreement, so the venue refuses every order on a \
         TradFi (equity, commodity or currency) perpetual ({E_TRADFI_AGREEMENT}: {msg}). The bridge signs no \
         agreement for a LIVE account: the account holder signs it in the Binance app, then the order can be sent"
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
