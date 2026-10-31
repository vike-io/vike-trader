//! **The two account modes this adapter orders under, set through the API instead of reported** (owner rule
//! 2026-10-26: a user trades ANY ticker of the venue with no hand step; when the account's own mode blocks a
//! valid ticker, the bridge fixes it through the API where the API allows, and says WHY in words where it
//! does not).
//!
//! | mode | what the adapter orders under | read | write |
//! |---|---|---|---|
//! | position mode (ACCOUNT-wide) | ONE-WAY: every order carries `positionSide BOTH` (`crate::family::order_map`'s `build_perp_order_params`, `crate::algo`'s `build_perp_algo_params`) | `GET /fapi/v1/positionSide/dual` -> `dualSidePosition` | `POST /fapi/v1/positionSide/dual` `dualSidePosition=false` |
//! | margin type (PER SYMBOL) | CROSS: the engine's margin math models cross (`crate::leverage_read`) | `GET /fapi/v2/positionRisk?symbol=` -> `marginType` | `POST /fapi/v1/marginType` `marginType=CROSSED` |
//!
//! ## The rule
//!
//! * **Read first, write only on a mismatch.** A healthy account costs one read per mode and no write (and no
//!   venue "no need to change" answer in the log). A mode that cannot be READ is a warning and today's blind
//!   start ([`ModeOutcome::Unread`]): a failed probe never blocks a start and never writes.
//! * **The VENUE judges flatness, atomically, at write time.** Binance refuses the position-mode switch while
//!   the account holds any position or resting order on ANY symbol, and the margin-type switch while THAT
//!   symbol holds one; this module sends the write and reads the verdict instead of pre-reading positions and
//!   orders (two extra reads and a race). So a mode is never switched under an open position or order: the
//!   venue refuses, and that refusal is a NAMED one ([`ModeOutcome::Refused`]) saying what is wrong and what
//!   blocks the fix. `-4059`/`-4046` ("no need to change") on the write mean somebody else switched it first:
//!   success. shortcut: if a venue ever let a switch through under a position, this would move it; the
//!   offline tests pin that no write follows a refusal, and the demo drill in
//!   `crates/bridges/binance/tests/binance_modes_smoke.rs` measured the venue's own refusals (`-4067`,
//!   `-4047`, `-4048`; `crates/bridges/binance/CLAUDE.md` has the answers and what was never run live).
//! * **Position mode is probed at every perp start** ([`heal_at_start`], from `crate::exec`'s
//!   `perp_start_writes`, before the leverage write) whether or not an account states a leverage. **Margin type
//!   is probed for the mounted symbol at that start and for every other perpetual before its FIRST order**
//!   ([`heal_symbol`], from `crate::exec`'s `foreign_perp_factory`, whose client is built once per symbol).
//! * **DEMO host only; a MAINNET account is never read and never written** (owner ruling 2026-10-26: no
//!   exchange write on a live account). The gate is the client's HOST, not a tier label: [`writes_modes`] is
//!   true only for `crate::perp::DEMO_FAPI_REST`, so any other base URL (the mainnet host, or one nobody
//!   named) is left alone, [`ModeOutcome::LeftAlone`], whatever tier the account row states. A live account
//!   therefore starts exactly as it did before this module, and a mode that blocks its orders is worded at the
//!   order ([`order_reject_reason`]) with the hand fix. No setting was added.
//!
//! ## What a refusal does to orders
//!
//! * **Hedge mode that could not be switched blocks every order**, so it is worded at the order: the venue's
//!   `-4061` reject text reaches the operator as [`order_reject_reason`]'s sentence (hedge mode, what blocks
//!   the switch, what to do) in every submit path (single, batch, stop). On a MAINNET host the sentence says
//!   the bridge changes no live account's mode and names the hand fix.
//! * **The first `-4061` retries the switch** ([`reject_reason`], from every submit path): the start's switch
//!   is refused while the account holds a position or a resting order, and nothing retried it between
//!   starts, so an account flattened afterwards (or flipped to hedge while the engine ran) stayed refused
//!   until `account.active` went off and on. Now the reject itself asks again, DEMO host only, at most once
//!   per [`REPROBE_WINDOW`] (the answer serves the rejects that follow it, so one rejected batch is one
//!   probe): the order that met the reject stays rejected, and its reason says whether the account is
//!   one-way now (send it again) or what still blocks the switch. The order is NOT resent here.
//! * **Isolated margin that could not be switched blocks nothing** (an isolated symbol accepts orders): it is
//!   a warning, and the order trades on isolated margin as it did before this module, which the engine's cross
//!   margin math does not model.
//!
//! ## Not done, on purpose
//!
//! **Multi-Assets mode is neither read nor written.** `GET /fapi/v1/multiAssetsMargin` exists (it answered
//! `false` on the demo account), but no order this adapter sends names it, and switching it has a
//! wallet-valuation consequence this module does not own. Whether an order under Multi-Assets mode trades
//! is unmeasured here; the margin write asks for CROSSED, the one margin type that mode keeps (Binance's
//! published `-4168` refuses isolated under it). A lane that states a leverage still refuses, in
//! `crate::leverage_read`, what it cannot read.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};

use crate::perp::{BinancePerpRest, DEMO_FAPI_REST, PATH_POSITIONS};

/// **Whether the bridge may touch an account mode on `base_url`'s network: the DEMO futures host and nothing
/// else.** Fail-closed on purpose: the mainnet host, a proxy or any host this crate does not name as demo is
/// a network where a write is not made, whatever tier the account row says (owner ruling 2026-10-26).
#[must_use]
pub fn writes_modes(base_url: &str) -> bool {
    base_url == DEMO_FAPI_REST
}

/// `GET`/`POST /fapi/v1/positionSide/dual`: the account's position mode.
pub const PATH_POSITION_MODE: &str = "/fapi/v1/positionSide/dual";
/// `POST /fapi/v1/marginType`: one symbol's margin type.
pub const PATH_MARGIN_TYPE: &str = "/fapi/v1/marginType";

/// `-4061`: an order's `positionSide` does not match the account's position mode (every one-way order on a
/// hedge-mode account).
const E_POSITION_SIDE_MISMATCH: i64 = -4061;
/// `-4059`: "No need to change position side." (the account is already in the asked mode).
const E_NO_NEED_POSITION_SIDE: i64 = -4059;
/// `-4046`: "No need to change margin type."
const E_NO_NEED_MARGIN_TYPE: i64 = -4046;
/// `-4067`: the position-mode switch is refused while the account has resting orders.
const E_POSITION_SIDE_OPEN_ORDERS: i64 = -4067;
/// `-4068`: the position-mode switch is refused while the account has a position.
const E_POSITION_SIDE_OPEN_POSITION: i64 = -4068;
/// `-4047`: the margin-type switch is refused while the symbol has resting orders.
const E_MARGIN_TYPE_OPEN_ORDERS: i64 = -4047;
/// `-4048`: the margin-type switch is refused while the symbol has a position.
const E_MARGIN_TYPE_OPEN_POSITION: i64 = -4048;
/// `-2014`/`-2015`: the key is malformed / refused (permissions, IP whitelist): it may not change settings.
const KEY_REFUSED: [i64; 2] = [-2014, -2015];

/// What one mode probe found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeOutcome {
    /// Already the mode this adapter orders under (or somebody switched it while we asked): nothing was
    /// written, or the venue said "no need".
    Right,
    /// The account was in the other mode, flat, and the venue accepted the switch.
    Switched,
    /// The account is in the other mode and the venue REFUSED the switch: the sentence says what is wrong and
    /// what blocks the fix, in the operator's words.
    Refused(String),
    /// The mode could not be read: nothing was written, the start goes on as it did before this module.
    Unread(String),
    /// Not the demo host ([`writes_modes`]): nothing was read and nothing was written; the account starts as
    /// it did before this module.
    LeftAlone,
}

/// `dualSidePosition` from the body of `GET /fapi/v1/positionSide/dual` (a JSON bool, or the strings
/// `"true"`/`"false"`). PURE. `None` for any other shape: an unreadable answer is never read as a mode.
#[must_use]
pub fn dual_side_position(body: &Value) -> Option<bool> {
    match body.get("dualSidePosition")? {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

/// Whether `symbol` is on ISOLATED margin, from the body of `GET /fapi/v2/positionRisk?symbol=`. PURE.
/// Hedge mode answers two rows for the symbol and both carry the same `marginType`.
///
/// # Errors
/// No row for the symbol, or a row whose `marginType` is neither `cross` nor `isolated`.
pub fn margin_is_isolated(body: &Value, symbol: &str) -> Result<bool, String> {
    let row = body
        .as_array()
        .and_then(|rows| {
            rows.iter().find(|r| {
                r.get("symbol")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case(symbol))
            })
        })
        .ok_or_else(|| format!("positionRisk answered no row for {symbol}"))?;
    match row.get("marginType").and_then(Value::as_str) {
        Some(m) if m.eq_ignore_ascii_case("isolated") => Ok(true),
        Some(m) if m.eq_ignore_ascii_case("cross") => Ok(false),
        other => Err(format!("positionRisk carries no usable marginType for {symbol} ({other:?})")),
    }
}

/// **Make the account's position mode ONE-WAY.** Reads `GET /fapi/v1/positionSide/dual`; a one-way account is
/// [`ModeOutcome::Right`] with no write; a hedge account gets ONE `POST` and the venue's verdict:
/// accepted -> [`ModeOutcome::Switched`], `-4059` -> `Right`, anything else -> [`ModeOutcome::Refused`] with
/// [`hedge_refusal`]'s sentence. An unreadable mode is [`ModeOutcome::Unread`] and nothing is written.
#[must_use]
pub fn ensure_one_way<S: Signer, T: RestTransport>(rest: &BinancePerpRest<S, T>) -> ModeOutcome {
    if !writes_modes(&rest.base_url) {
        return ModeOutcome::LeftAlone;
    }
    let dual = match rest.transport.signed_requery(
        &rest.base_url,
        PATH_POSITION_MODE,
        "GET",
        &[],
        &rest.signer,
    ) {
        Ok(body) => match dual_side_position(&body) {
            Some(dual) => dual,
            None => return ModeOutcome::Unread(format!("{PATH_POSITION_MODE} answered {body}")),
        },
        Err(e) => return ModeOutcome::Unread(format!("GET {PATH_POSITION_MODE} failed: {e}")),
    };
    if !dual {
        return ModeOutcome::Right;
    }
    let params = [("dualSidePosition", "false".to_string())];
    match rest.transport.signed(&rest.base_url, PATH_POSITION_MODE, "POST", &params, &rest.signer) {
        Ok(_) => ModeOutcome::Switched,
        Err(e) if e.code == E_NO_NEED_POSITION_SIDE => ModeOutcome::Right,
        Err(e) => ModeOutcome::Refused(hedge_refusal(&e)),
    }
}

/// **Make `rest.symbol`'s margin type CROSS.** Reads `GET /fapi/v2/positionRisk?symbol=`; a cross symbol is
/// [`ModeOutcome::Right`] with no write; an isolated one gets ONE `POST /fapi/v1/marginType` with
/// `marginType=CROSSED` and the venue's verdict (accepted -> `Switched`, `-4046` -> `Right`, else
/// [`ModeOutcome::Refused`] with [`isolated_refusal`]'s sentence). An unreadable row is
/// [`ModeOutcome::Unread`] and nothing is written.
#[must_use]
pub fn ensure_cross_margin<S: Signer, T: RestTransport>(
    rest: &BinancePerpRest<S, T>,
) -> ModeOutcome {
    if !writes_modes(&rest.base_url) {
        return ModeOutcome::LeftAlone;
    }
    let read = rest.transport.signed_requery(
        &rest.base_url,
        PATH_POSITIONS,
        "GET",
        &[("symbol", rest.symbol.clone())],
        &rest.signer,
    );
    let isolated = match read {
        Ok(body) => match margin_is_isolated(&body, &rest.symbol) {
            Ok(isolated) => isolated,
            Err(why) => return ModeOutcome::Unread(why),
        },
        Err(e) => return ModeOutcome::Unread(format!("GET {PATH_POSITIONS} failed: {e}")),
    };
    if !isolated {
        return ModeOutcome::Right;
    }
    let params = [("symbol", rest.symbol.clone()), ("marginType", "CROSSED".to_string())];
    match rest.transport.signed(&rest.base_url, PATH_MARGIN_TYPE, "POST", &params, &rest.signer) {
        Ok(_) => ModeOutcome::Switched,
        Err(e) if e.code == E_NO_NEED_MARGIN_TYPE => ModeOutcome::Right,
        Err(e) => ModeOutcome::Refused(isolated_refusal(&rest.symbol, &e)),
    }
}

/// **The perp start's mode probe**: the account's position mode, then the mounted symbol's margin type, each
/// logged. Returns both outcomes (the tests read them). Never blocks and never fails a start.
pub fn heal_at_start<S: Signer, T: RestTransport>(
    rest: &BinancePerpRest<S, T>,
) -> (ModeOutcome, ModeOutcome) {
    let position = ensure_one_way(rest);
    report("position mode", "hedge -> one-way", rest, &position);
    (position, heal_symbol(rest))
}

/// **The margin probe for one symbol**, logged: the mounted symbol at the start, any other perpetual before
/// its first order. Never blocks an order.
pub fn heal_symbol<S: Signer, T: RestTransport>(rest: &BinancePerpRest<S, T>) -> ModeOutcome {
    let outcome = ensure_cross_margin(rest);
    report("margin type", "isolated -> cross", rest, &outcome);
    outcome
}

/// One log line per probe: a switch is a WARN (it changed the venue account, and the host says which network),
/// a refused hedge switch an ERROR (every order will be refused), a refused isolated switch a WARN.
fn report<S: Signer, T: RestTransport>(
    what: &str,
    change: &str,
    rest: &BinancePerpRest<S, T>,
    outcome: &ModeOutcome,
) {
    let (host, symbol) = (rest.base_url.as_str(), rest.symbol.as_str());
    match outcome {
        ModeOutcome::Right => {
            tracing::debug!(target: "vike_binance::modes", host, symbol, what, "already the mode this adapter orders under")
        }
        ModeOutcome::Switched => {
            tracing::warn!(target: "vike_binance::modes", host, symbol, what, change, "SWITCHED by the bridge (the venue saw the account flat), so every symbol trades without a hand step")
        }
        ModeOutcome::Refused(why) if what == "position mode" => {
            tracing::error!(target: "vike_binance::modes", host, symbol, what, "{why}")
        }
        ModeOutcome::Refused(why) => {
            tracing::warn!(target: "vike_binance::modes", host, symbol, what, "{why}")
        }
        ModeOutcome::Unread(why) => {
            tracing::warn!(target: "vike_binance::modes", host, symbol, what, "mode not read, nothing written (the start goes on as before): {why}")
        }
        ModeOutcome::LeftAlone => {
            tracing::debug!(target: "vike_binance::modes", host, symbol, what, "not the demo host: the bridge reads and writes no account mode here")
        }
    }
}

/// What blocks a switch, from the venue's own refusal: `orders` and `position` are the two codes the venue
/// answers for that switch, and the venue's code and text always ride along.
fn blocker(e: &VenueApiError, orders: i64, position: i64) -> String {
    let own = format!("the venue answered {}: {}", e.code, e.msg);
    match e.code {
        c if c == orders => format!("it holds RESTING ORDERS ({own})"),
        c if c == position => format!("it holds an OPEN POSITION ({own})"),
        c if KEY_REFUSED.contains(&c) => format!(
            "this key may not change account settings ({own}): enable Futures trading on the key \
             and whitelist this box's IP"
        ),
        _ => format!("the venue refused the switch ({own})"),
    }
}

/// The sentence for a hedge-mode account the switch to one-way was REFUSED on: what is wrong, what blocks
/// the fix, what to do. PURE.
#[must_use]
pub fn hedge_refusal(e: &VenueApiError) -> String {
    format!(
        "binance: this account is in HEDGE position mode and this adapter orders ONE-WAY \
         (positionSide BOTH), so the venue refuses every order (-4061). The bridge tried to switch it \
         to one-way and could not: {}. Binance switches the mode only while the account holds \
         no position and no resting order on ANY symbol: close the positions and cancel the orders, then \
         send an order again (the bridge asks the venue again at a refused order, at most every 30 \
         seconds) or set the account's `active` off and on (or choose One-way Mode in the Binance \
         Futures preferences)",
        blocker(e, E_POSITION_SIDE_OPEN_ORDERS, E_POSITION_SIDE_OPEN_POSITION)
    )
}

/// The sentence for an isolated symbol the switch to cross was REFUSED on. PURE.
#[must_use]
pub fn isolated_refusal(symbol: &str, e: &VenueApiError) -> String {
    format!(
        "binance: {symbol} is on ISOLATED margin and the engine's margin math models CROSS. The bridge \
         tried to switch it to cross and could not: {}. Orders are NOT refused and trade on isolated \
         margin meanwhile; Binance switches a symbol's margin type only while it holds no position and no \
         resting order: close or cancel them and restart the account, or choose Cross in the Binance \
         Futures preferences",
        blocker(e, E_MARGIN_TYPE_OPEN_ORDERS, E_MARGIN_TYPE_OPEN_POSITION)
    )
}

/// **The reason an order carries when the venue rejects it**: the venue's own text, except `-4061` (a one-way
/// order on a hedge-mode account), which is worded as [`ModeOutcome::Refused`]'s hedge sentence explains why
/// it persists. One function for the single, batch and stop submit paths. PURE.
#[must_use]
pub fn order_reject_reason(code: i64, msg: &str, base_url: &str) -> String {
    // The two refusals a class beyond the plain perpetual meets (`crate::class_prep`'s module doc).
    if code == crate::class_prep::E_TRADFI_AGREEMENT {
        return crate::class_prep::agreement_hand_step(msg);
    }
    if code == crate::class_prep::E_POSITION_AT_LEVERAGE {
        return crate::class_prep::position_at_leverage_reason(msg, writes_modes(base_url));
    }
    if code != E_POSITION_SIDE_MISMATCH {
        return msg.to_string();
    }
    if !writes_modes(base_url) {
        return format!(
            "binance: the account is in HEDGE position mode and this adapter orders ONE-WAY (positionSide \
             BOTH), so the venue refuses the order (-4061: {msg}). The bridge changes no LIVE account's \
             mode: choose One-way Mode in the Binance Futures preferences (Binance allows it only while the \
             account holds no position and no resting order on any symbol), then send the order again"
        );
    }
    format!(
        "binance: the account is in HEDGE position mode and this adapter orders ONE-WAY (positionSide \
         BOTH), so the venue refuses the order (-4061: {msg}). The bridge switches the account to one-way \
         by itself at start when it holds no position and no resting order on any symbol; this one still \
         is in hedge mode, so something blocked the switch (see the start's log): close the positions and \
         cancel the orders, then set the account's `active` off and on (or choose One-way Mode in the \
         Binance Futures preferences)"
    )
}

/// How long ONE re-probe of the position mode answers every `-4061` that follows it: a rejected batch (five
/// elements) or a strategy retrying a refused order is one probe, not one per reject, and a stuck hedge account
/// costs one read and at most one refused write per window.
pub const REPROBE_WINDOW: Duration = Duration::from_secs(30);

/// **The last re-probe of the position mode that a `-4061` reject made**: when it ran and the sentence it
/// gave (`None` = it had nothing to add: the mode could not be read, so the order carries the standard
/// sentence). The lock is held across the probe on purpose (single flight: a second reject waits for the
/// first one's answer instead of probing too).
///
/// shortcut: ONE per process ([`reject_reason`]'s static), not one per account: the position mode is
/// account-wide and the demo roster has one binance account, so a second hedge account would wait out
/// the first one's window before its own first reject probes. Upgrade when a second binance account that
/// can be in hedge mode exists.
#[derive(Default)]
pub struct Reprobe(Mutex<Option<(Instant, Option<String>)>>);

/// The process's re-probe memory ([`reject_reason`]).
static REPROBE: Reprobe = Reprobe::new();

impl Reprobe {
    /// A memory that has probed nothing.
    #[must_use]
    pub const fn new() -> Self {
        Reprobe(Mutex::new(None))
    }

    /// The sentence a `-4061` reject (`msg`: the venue's text) carries as of `now`: the standing answer of a
    /// probe made less than [`REPROBE_WINDOW`] ago, else the result of probing the position mode again
    /// ([`ensure_one_way`]: one read, and a write only when the account reads hedge). `None` = say the
    /// standard sentence ([`order_reject_reason`]).
    fn sentence<S: Signer, T: RestTransport>(
        &self,
        rest: &BinancePerpRest<S, T>,
        msg: &str,
        now: Instant,
    ) -> Option<String> {
        let mut last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, said)) = last.as_ref()
            && now.saturating_duration_since(*at) < REPROBE_WINDOW
        {
            return said.clone();
        }
        let outcome = ensure_one_way(rest);
        report("position mode", "hedge -> one-way", rest, &outcome);
        let said = match outcome {
            ModeOutcome::Switched => Some(format!(
                "binance: the venue refused this order because the account was in HEDGE position mode \
                 and this adapter orders ONE-WAY (positionSide BOTH) (-4061: {msg}). The bridge has \
                 switched the account to one-way just now (it held no position and no resting order): \
                 send the order again"
            )),
            ModeOutcome::Right => Some(format!(
                "binance: the venue refused this order because the account was in HEDGE position mode \
                 and this adapter orders ONE-WAY (positionSide BOTH) (-4061: {msg}). The account reads \
                 ONE-WAY now (it was switched a moment ago): send the order again"
            )),
            ModeOutcome::Refused(why) => Some(format!("{why} (this order: -4061: {msg})")),
            ModeOutcome::Unread(_) | ModeOutcome::LeftAlone => None,
        };
        *last = Some((now, said.clone()));
        said
    }
}

/// **The reason an order carries when the venue rejects it, with the first `-4061` asking the venue about the
/// position mode again**: [`order_reject_reason`]'s sentence for every other code, for a MAINNET host (the
/// bridge reads and writes no live account's mode) and for a reject whose re-probe could not read the mode;
/// otherwise the re-probe's own sentence ([`Reprobe::sentence`]). One function for the single, batch and stop
/// submit paths. The order that met the reject is not resent.
pub fn reject_reason<S: Signer, T: RestTransport>(
    rest: &BinancePerpRest<S, T>,
    code: i64,
    msg: &str,
) -> String {
    reject_reason_at(&REPROBE, rest, code, msg, Instant::now())
}

/// [`reject_reason`] over an explicit memory and clock (the tests').
fn reject_reason_at<S: Signer, T: RestTransport>(
    reprobe: &Reprobe,
    rest: &BinancePerpRest<S, T>,
    code: i64,
    msg: &str,
    now: Instant,
) -> String {
    if code == E_POSITION_SIDE_MISMATCH
        && writes_modes(&rest.base_url)
        && let Some(said) = reprobe.sentence(rest, msg, now)
    {
        return said;
    }
    // `-4411` on the demo host signs the agreement (once per window); elsewhere the hand step.
    if code == crate::class_prep::E_TRADFI_AGREEMENT {
        return crate::class_prep::agreement_sentence(rest, msg);
    }
    order_reject_reason(code, msg, &rest.base_url)
}

#[path = "modes_tests.rs"]
#[cfg(test)]
mod modes_tests;
