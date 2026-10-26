//! **The account modes this adapter orders in, put right through the API when the venue lets it**
//! (owner rule 2026-10-26: any ticker of a venue tradable with no hand step; where the account's own
//! mode blocks a valid ticker, the bridge fixes it, and where it cannot it says WHY in words).
//!
//! Every order this crate signs names `positionIdx 0` (`crate::perp::BybitPerpRest::build_order_params`),
//! the ONE-WAY position mode, and the engine books cross margin. A symbol the venue holds in HEDGE mode
//! answers every such order `10001 "position idx not match position mode"` (measured on the demo host,
//! 2026-10-26), whatever the ticker; before this module nothing here wrote a mode, it only reported one
//! (`crate::leverage_read`, and only for an account that states a leverage).
//!
//! [`Modes::ensure`] is the whole rule, run on the exec thread BEFORE the first order of a symbol (once at
//! the start for the mounted one, lazily for an admitted one: `crate::per_symbol::PerSymbolRest::with_mode_check`)
//! and cached, so a symbol costs two reads and, only when something is wrong, a write:
//!
//! | what the venue says | what happens |
//! |---|---|
//! | the symbol is ONE-WAY (`position/list`: one row, `positionIdx 0`) | nothing |
//! | HEDGE (rows `positionIdx 1` and `2`), the symbol FLAT and nothing resting on it | `POST /v5/position/switch-mode` `mode 0` for the symbol; `110025` ("not modified") is success |
//! | HEDGE, but a position or a resting order blocks the switch (or the venue refuses it) | the order is REFUSED, a terminal `OrderRejected` naming the symbol, the mode and what blocks the fix; nothing is written |
//! | the answer cannot be read | nothing is written and the order goes through to the venue as before (a read failure never takes a working start off the venue); asked again after [`RETRY_AFTER`] |
//! | `account/info` says `ISOLATED_MARGIN` (UTA) and the whole linear book (USDT and USDC) is flat with nothing resting | `POST /v5/account/set-margin-mode` `REGULAR_MARGIN` (cross: what the engine models) |
//! | `ISOLATED_MARGIN` with a position or an order anywhere, `PORTFOLIO_MARGIN`, a classic account, or one symbol held isolated | a WARN in words, nothing written, the order is NOT refused (the venue takes orders in those modes; only the engine's margin figures differ) |
//!
//! ## Why those limits
//!
//! * **A write needs a flat, empty target.** The venue refuses a mode switch under a position anyway
//!   (`110024`-class), but the refusal is cheaper to read than to provoke, and a margin-mode write is
//!   ACCOUNT-wide, so it is judged over the whole linear book. Spot margin, inverse and options are not
//!   read here: the venue's own refusal is the guard for those, and it is quoted when it fires.
//! * **`PORTFOLIO_MARGIN` is never written away.** Leaving it re-rates the collateral of the whole
//!   account and re-entering needs the venue's eligibility check; the engine's cross figures being off is a
//!   smaller cost than that, so it is reported, not changed. Classic (non-UTA) accounts have no
//!   account-level margin mode to set, and `switch-isolated` (the classic per-symbol knob) needs a
//!   leverage figure this module does not own, so a symbol held isolated is reported.
//! * **A read that fails changes nothing**, and the refusal and the "unreadable" verdicts are cached only for
//!   [`RETRY_AFTER`], so a burst of orders costs one probe and a flattened account is re-read soon.
//! * **DEMO only: a MAINNET account is never read and never written for a mode** (owner ruling 2026-10-26: no
//!   exchange write on a live account). [`Modes::ensure`] returns `Ok(())` at once, with no request, unless
//!   the rule is built for the demo host AND not flagged mainnet ([`Modes::writes_modes`]); the start builds
//!   both from the one tier answer (`crate::perp::endpoints`), and the host test is there so that no label
//!   can open a mainnet write. A live account therefore starts and orders exactly as it did before this
//!   module, and a hedge symbol's order the venue refuses (`10001 "position idx not match position mode"`)
//!   is worded at the reject ([`order_reject_reason`]): the bridge changes no live account's mode, and the
//!   hand fix is named. No setting or environment variable carries the switch (decisions 0086 and 0095:
//!   venue settings are database rows and `vike-venues`' `VENUE_FIELDS` is a shared home).
//!
//! ## What the demo host proved (2026-10-26, `crates/bridges/bybit/tests/bybit_modes_smoke.rs`)
//! `account/info` answers `marginMode` and `unifiedMarginStatus`; a flat ONE-WAY symbol answers ONE
//! `position/list` row (`positionIdx 0`, `size "0"`); `switch-mode` `mode 3` then answers two flat rows
//! (`positionIdx 1`, `2`); a repeated switch answers `110025 "Position mode is not modified"`; and
//! `set-margin-mode` answers `0 "Request accepted"` with `reasons: []` both ways (the host took
//! `ISOLATED_MARGIN` that day, though it had answered `10032 Demo trading are not supported` on
//! 2026-07-19, so the drill self-skips if it refuses again). An order that names `positionIdx 0` on a hedge
//! symbol is answered `10001 "position idx not match position mode"`. The whole rule ran live on the
//! account `bybit#DEMO`: a client's start put a hedge symbol in one-way, an admitted hedge symbol was put
//! in one-way at its first order, a symbol with a resting order was refused in words and left alone, and
//! an `ISOLATED_MARGIN` account was put back to `REGULAR_MARGIN` at a client's start. NOT proven: a
//! classic account, `PORTFOLIO_MARGIN` (reported only), a MAINNET account, and the exact text of the
//! venue's own refusal of a switch under a position (`110024`, the documented one).

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::BybitV5Signer;
use vike_model::MarginMode;

use crate::leverage_read::PATH_ACCOUNT_INFO;
use crate::perp::{DEMO_REST, PATH_ORDER_REALTIME, PATH_POSITION_LIST};
use crate::recon_client::parse_trade_mode;
use crate::transport::BybitTransport;

/// `POST /v5/position/switch-mode`.
pub const PATH_SWITCH_MODE: &str = "/v5/position/switch-mode";
/// `POST /v5/account/set-margin-mode`.
pub const PATH_SET_MARGIN_MODE: &str = "/v5/account/set-margin-mode";

/// How long a refusal, or an answer that could not be read, is remembered before the symbol is probed
/// again. A SETTLED symbol is remembered for the life of the process.
pub const RETRY_AFTER: Duration = Duration::from_secs(30);

/// `switch-mode`'s `mode`: one-way.
const MODE_ONE_WAY: i64 = 0;
/// "Position mode is not modified": the symbol already is in the mode asked for (measured, demo).
const POSITION_MODE_NOT_MODIFIED: i64 = 110025;
/// The settle coins of the linear perpetuals; a margin-mode write is judged over both books.
const SETTLE_COINS: [&str; 2] = ["USDT", "USDC"];
/// What the engine books, and what `set-margin-mode` calls it.
const CROSS: &str = "REGULAR_MARGIN";

/// The position mode a symbol's `position/list` rows say it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PositionMode {
    /// One row, `positionIdx 0`.
    OneWay,
    /// Rows `positionIdx 1` and/or `2`.
    Hedge,
    /// Any other shape; carries what was seen.
    Unknown(String),
}

/// The rows of `position/list` (or `order/realtime`) `result` that name `symbol`.
fn symbol_rows<'a>(result: &'a Value, symbol: &str) -> Vec<&'a Value> {
    result
        .get("list")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter(|r| {
                    r.get("symbol")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case(symbol))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The symbol's position mode, from the `result` of `GET /v5/position/list?category=linear&symbol=`. PURE.
#[must_use]
pub fn parse_position_mode(result: &Value, symbol: &str) -> PositionMode {
    let rows = symbol_rows(result, symbol);
    let idx: Option<Vec<i64>> =
        rows.iter().map(|r| r.get("positionIdx").and_then(json_num).map(|x| x as i64)).collect();
    match idx.as_deref() {
        Some([0]) => PositionMode::OneWay,
        Some(idx) if !idx.is_empty() && idx.iter().all(|i| matches!(i, 1 | 2)) => {
            PositionMode::Hedge
        }
        _ => PositionMode::Unknown(format!(
            "the venue answered {} position row(s) for {symbol} that name no single position mode",
            rows.len()
        )),
    }
}

/// A row that holds a position. An unreadable `size` counts as OPEN: a write is never made on a guess.
fn row_is_open(row: &Value) -> bool {
    row.get("size").and_then(json_num).is_none_or(|size| size != 0.0)
}

/// Whether the symbol's own rows hold a position (hedge mode answers one row per leg).
#[must_use]
pub fn holds_position(result: &Value, symbol: &str) -> bool {
    symbol_rows(result, symbol).into_iter().any(row_is_open)
}

/// Whether a settle-coin-wide `position/list` page shows NO position: no open row and no next page.
fn book_is_flat(result: &Value) -> bool {
    let rows = result.get("list").and_then(Value::as_array);
    let more = result.get("nextPageCursor").and_then(Value::as_str).is_some_and(|c| !c.is_empty());
    rows.is_some_and(|rows| rows.iter().all(|r| !row_is_open(r))) && !more
}

/// How many rows the `result` carries (orders: how many rest).
fn rows_len(result: &Value) -> usize {
    result.get("list").and_then(Value::as_array).map_or(0, Vec::len)
}

/// What `GET /v5/account/info` says about the account's margin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMode {
    /// `marginMode` verbatim: `REGULAR_MARGIN`, `ISOLATED_MARGIN` or `PORTFOLIO_MARGIN`.
    pub margin_mode: String,
    /// `unifiedMarginStatus` 1: a classic account, which has no account-level margin mode to set.
    pub classic: bool,
}

/// The account's margin mode, from the `result` of `GET /v5/account/info`. PURE.
///
/// # Errors
/// The answer carries no `marginMode`.
pub fn parse_account_mode(result: &Value) -> Result<AccountMode, String> {
    let margin_mode = result
        .get("marginMode")
        .and_then(Value::as_str)
        .ok_or_else(|| "GET /v5/account/info carries no marginMode".to_string())?
        .to_string();
    let classic = result.get("unifiedMarginStatus").and_then(json_num) == Some(1.0);
    Ok(AccountMode { margin_mode, classic })
}

/// What one probe of a symbol left behind.
enum Verdict {
    /// Right (or made right, or reported as far as it can be): never asked again.
    Settled,
    /// Refused, or something could not be read: asked again after [`Modes::retry_after`].
    Retry { at: Instant, refusal: Option<String> },
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn linear(symbol: &str) -> [(&'static str, Value); 2] {
    [("category", json!("linear")), ("symbol", json!(symbol))]
}

/// The account-mode rule of the module doc, for one account on one host. See [`Modes::ensure`].
pub struct Modes<T: BybitTransport> {
    signer: BybitV5Signer,
    transport: T,
    base_url: String,
    mainnet: bool,
    retry_after: Duration,
    verdicts: Mutex<HashMap<String, Verdict>>,
}

impl<T: BybitTransport> Modes<T> {
    /// A rule for the account `signer` signs for, against `base_url`; `mainnet` (or any host but the demo one)
    /// switches the whole rule off ([`Modes::writes_modes`]).
    /// `transport` carries the writes on the order-rate window (the caller's choice); the reads ride the
    /// transport's short-timeout, rate-gate-exempt `signed_requery`.
    pub fn new(
        signer: BybitV5Signer,
        transport: T,
        base_url: impl Into<String>,
        mainnet: bool,
    ) -> Self {
        Modes {
            signer,
            transport,
            base_url: base_url.into(),
            mainnet,
            retry_after: RETRY_AFTER,
            verdicts: Mutex::new(HashMap::new()),
        }
    }

    /// [`RETRY_AFTER`] replaced (a test's clock).
    #[must_use]
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = retry_after;
        self
    }

    fn network(&self) -> &'static str {
        if self.mainnet { "MAINNET" } else { "demo" }
    }

    /// **Whether this rule may read or write anything: the DEMO host, not flagged mainnet, and nothing else**
    /// (owner ruling 2026-10-26: no exchange write on a live account). Fail-closed on both counts: the
    /// flag and the host are one decision made from the same tier answer, and either one saying "not
    /// demo" (the mainnet host, or a host nobody named) closes the rule whatever the account row states.
    #[must_use]
    pub fn writes_modes(&self) -> bool {
        !self.mainnet && self.base_url == DEMO_REST
    }

    /// **Make `symbol` tradable in this adapter's modes, or say why it cannot be.** Blocking REST: the exec
    /// thread only.
    ///
    /// # Errors
    /// The symbol is in hedge position mode and the switch to one-way is blocked or refused: the words
    /// name the symbol, the mode and what blocks the fix, and nothing was written. Every other outcome
    /// (right already, healed, a state only reported, a read that failed) is `Ok`.
    pub fn ensure(&self, symbol: &str) -> Result<(), String> {
        if !self.writes_modes() {
            return Ok(());
        }
        let key = symbol.to_uppercase();
        if let Some(verdict) = lock(&self.verdicts).get(&key) {
            match verdict {
                Verdict::Settled => return Ok(()),
                Verdict::Retry { at, refusal } if at.elapsed() < self.retry_after => {
                    return refusal.clone().map_or(Ok(()), Err);
                }
                Verdict::Retry { .. } => {}
            }
        }
        let (answer, complete) = self.settle(&key);
        let verdict = match (&answer, complete) {
            (Ok(()), true) => Verdict::Settled,
            _ => Verdict::Retry { at: Instant::now(), refusal: answer.clone().err() },
        };
        lock(&self.verdicts).insert(key, verdict);
        answer
    }

    /// One probe: `(the answer, whether every read it needed was answered)`.
    fn settle(&self, symbol: &str) -> (Result<(), String>, bool) {
        let positions =
            self.read(&format!("position/list for {symbol}"), PATH_POSITION_LIST, &linear(symbol));
        let mut complete = true;
        match &positions {
            Ok(result) => match parse_position_mode(result, symbol) {
                PositionMode::OneWay => {}
                PositionMode::Hedge => {
                    if let Err(refusal) = self.leave_hedge(symbol, result) {
                        return (Err(refusal), false);
                    }
                }
                PositionMode::Unknown(why) => {
                    tracing::warn!(target: "vike_bybit", %symbol, "position mode not checked: {why}");
                    complete = false;
                }
            },
            Err(why) => {
                tracing::warn!(target: "vike_bybit", %symbol, "position mode not checked: {why}");
                complete = false;
            }
        }
        complete &= self.settle_margin(symbol, positions.as_ref().ok());
        (Ok(()), complete)
    }

    /// The symbol is HEDGE: write one-way when nothing blocks it, else refuse in words.
    fn leave_hedge(&self, symbol: &str, positions: &Value) -> Result<(), String> {
        let blocker = if holds_position(positions, symbol) {
            Some("holds an open position".to_string())
        } else {
            match self.read(
                &format!("order/realtime for {symbol}"),
                PATH_ORDER_REALTIME,
                &linear(symbol),
            ) {
                Ok(orders) => match rows_len(&orders) {
                    0 => None,
                    n => Some(format!("has {n} resting order(s)")),
                },
                Err(why) => Some(format!("could not be shown to have no resting order ({why})")),
            }
        };
        let words = |because: &str| {
            format!(
                "bybit: {symbol} is in HEDGE position mode, and this adapter orders ONE-WAY (every order \
                 names positionIdx 0, which the venue answers 10001 \"position idx not match position \
                 mode\" on a hedge symbol). {because}. Nothing was changed on the venue."
            )
        };
        if let Some(blocker) = blocker {
            return Err(words(&format!(
                "The switch to one-way is made only while the symbol is flat with no resting order, and \
                 {symbol} {blocker}: close it (or let it close) and the next order switches the mode by \
                 itself"
            )));
        }
        let params = [
            ("category", json!("linear")),
            ("symbol", json!(symbol)),
            ("mode", json!(MODE_ONE_WAY)),
        ];
        match self.write(PATH_SWITCH_MODE, &params) {
            Ok(Written::Changed) => {
                tracing::warn!(target: "vike_bybit", %symbol, network = self.network(), "position mode: {symbol} was HEDGE and flat with nothing resting; switched to ONE-WAY (the mode this adapter orders in)");
                Ok(())
            }
            Ok(Written::AlreadyThere) => Ok(()),
            Err(why) => Err(words(&format!(
                "The venue refused the switch to one-way (POST {PATH_SWITCH_MODE}): {why}"
            ))),
        }
    }

    /// The account's margin mode: heal `ISOLATED_MARGIN` when the whole linear book is flat, report every
    /// other state that differs from cross. Never refuses an order. `true` when `account/info` was read.
    fn settle_margin(&self, symbol: &str, positions: Option<&Value>) -> bool {
        let account = match self
            .read("account/info", PATH_ACCOUNT_INFO, &[])
            .and_then(|result| parse_account_mode(&result))
        {
            Ok(account) => account,
            Err(why) => {
                tracing::warn!(target: "vike_bybit", %symbol, "margin mode not checked: {why}");
                return false;
            }
        };
        match account.margin_mode.as_str() {
            CROSS => {
                // A classic account holds a symbol isolated per symbol, and only its own row says so.
                if positions.is_some_and(|p| {
                    symbol_rows(p, symbol)
                        .iter()
                        .any(|r| parse_trade_mode(r) == MarginMode::Isolated)
                }) {
                    tracing::warn!(target: "vike_bybit", %symbol, "margin mode: {symbol} is held in ISOLATED margin (tradeMode 1) on a classic account and the engine books cross margin; not changed (the classic per-symbol switch needs a leverage figure this adapter does not own): set it to cross in Bybit's position settings. Orders are not refused.");
                }
            }
            "ISOLATED_MARGIN" if !account.classic => self.leave_isolated(symbol),
            "PORTFOLIO_MARGIN" => {
                tracing::warn!(target: "vike_bybit", %symbol, "margin mode: the account is in PORTFOLIO_MARGIN and the engine books plain cross margin; not changed (leaving portfolio margin re-rates the collateral of the whole account and re-entering it needs the venue's eligibility check): switch the account to cross margin in Bybit's account settings if the engine's margin figures must match the venue's. Orders are not refused.");
            }
            other => {
                tracing::warn!(target: "vike_bybit", %symbol, classic = account.classic, "margin mode: the account's marginMode is {other:?}, which is not the cross margin the engine books; not changed (no account-level switch applies). Orders are not refused.");
            }
        }
        true
    }

    /// `ISOLATED_MARGIN` on a UTA account: write cross only while the whole linear book is flat and empty.
    fn leave_isolated(&self, symbol: &str) {
        let report = |because: &str| {
            tracing::warn!(target: "vike_bybit", %symbol, network = self.network(), "margin mode: the account is in ISOLATED_MARGIN and the engine books cross margin; NOT switched to {CROSS}: {because}. Orders are not refused (the venue takes them in isolated margin; the engine's margin figures differ from the venue's).");
        };
        if let Some(blocker) = self.linear_book_blocker() {
            return report(&format!(
                "the switch is made only while the account holds no linear position and no resting linear order, and {blocker}"
            ));
        }
        match self.write(PATH_SET_MARGIN_MODE, &[("setMarginMode", json!(CROSS))]) {
            Ok(_) => {
                tracing::warn!(target: "vike_bybit", %symbol, network = self.network(), "margin mode: the account was in ISOLATED_MARGIN with no linear position or order; switched to {CROSS} (the cross margin the engine books)");
            }
            Err(why) => {
                report(&format!("the venue refused it (POST {PATH_SET_MARGIN_MODE}): {why}"))
            }
        }
    }

    /// What stops a margin-mode write: a linear position or a resting linear order of either settle coin,
    /// or a read that cannot show there is none. `None` = flat and empty.
    fn linear_book_blocker(&self) -> Option<String> {
        for coin in SETTLE_COINS {
            let by_coin = [("category", json!("linear")), ("settleCoin", json!(coin))];
            match self.read(&format!("{coin} position/list"), PATH_POSITION_LIST, &by_coin) {
                Ok(result) if book_is_flat(&result) => {}
                Ok(_) => return Some(format!("it holds a {coin}-settled linear position")),
                Err(why) => return Some(why),
            }
            match self.read(&format!("{coin} order/realtime"), PATH_ORDER_REALTIME, &by_coin) {
                Ok(result) if rows_len(&result) == 0 => {}
                Ok(result) => {
                    return Some(format!(
                        "{} {coin}-settled order(s) rest on it",
                        rows_len(&result)
                    ));
                }
                Err(why) => return Some(why),
            }
        }
        None
    }

    /// One signed GET on the short-timeout agent: the unwrapped `result`, or the words of why not.
    fn read(&self, what: &str, path: &str, params: &[(&str, Value)]) -> Result<Value, String> {
        let envelope = self
            .transport
            .signed_requery(&self.base_url, path, "GET", params, &self.signer)
            .map_err(|e| format!("{what} could not be read: {e}"))?;
        crate::transport::unwrap_envelope(envelope)
            .map_err(|e| format!("{what} could not be read: {e}"))
    }

    /// One signed POST. The venue's answer is its `retCode` and, for `set-margin-mode`, the `reasons` it
    /// gives a refusal, so the raw envelope is read here and not unwrapped.
    fn write(&self, path: &str, params: &[(&str, Value)]) -> Result<Written, String> {
        let (code, msg, body) =
            match self.transport.signed(&self.base_url, path, "POST", params, &self.signer) {
                Ok(envelope) => (
                    envelope.get("retCode").and_then(Value::as_i64).unwrap_or(0),
                    envelope.get("retMsg").and_then(Value::as_str).unwrap_or("").to_string(),
                    envelope,
                ),
                Err(e) => (e.code, e.msg, Value::Null),
            };
        let reasons: Vec<String> = body
            .get("result")
            .and_then(|r| r.get("reasons"))
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|r| {
                        r.get("reasonMsg").or_else(|| r.get("reasonCode")).map(|m| match m {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        match code {
            0 if reasons.is_empty() => Ok(Written::Changed),
            POSITION_MODE_NOT_MODIFIED => Ok(Written::AlreadyThere),
            _ if reasons.is_empty() => Err(format!("{code} {msg}")),
            _ => Err(format!("{code} {msg} ({})", reasons.join("; "))),
        }
    }
}

/// What a write left.
enum Written {
    Changed,
    /// `110025`: the venue already held what was asked.
    AlreadyThere,
}

/// The venue's text for an order whose `positionIdx` does not match the symbol's position mode (`10001` is
/// bybit's generic parameter error, so the text is what identifies it).
const POSITION_IDX_MISMATCH: &str = "position idx not match position mode";

/// **The reason an order carries when the venue rejects it.** The venue's own text, except on a host that is
/// not the demo one (a live account, whose mode the bridge never changes, owner ruling 2026-10-26): a
/// `10001 "position idx not match position mode"` there is worded as the hedge-mode mismatch it is, with the
/// hand fix. One function for the single and the batch submit paths. PURE.
#[must_use]
pub fn order_reject_reason(code: i64, msg: &str, base_url: &str, symbol: &str) -> String {
    if code != 10001
        || base_url == DEMO_REST
        || !msg.to_ascii_lowercase().contains(POSITION_IDX_MISMATCH)
    {
        return msg.to_string();
    }
    format!(
        "bybit: {symbol} is in HEDGE position mode and this adapter orders ONE-WAY (every order names \
         positionIdx 0), so the venue refuses the order (10001: {msg}). The bridge changes no LIVE \
         account's mode: switch {symbol} to One-Way position mode in Bybit's contract trading settings \
         (Bybit allows it only while the symbol holds no position and no resting order), then send the \
         order again"
    )
}

#[path = "modes_tests.rs"]
#[cfg(test)]
mod modes_tests;
