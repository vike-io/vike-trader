//! **okx's ACCOUNT MODES, read at every start and healed when the account can be healed** (owner rule
//! 2026-10-26: a user trades ANY ticker with no hand step; when the account's own mode blocks a valid
//! ticker the bridge fixes it through the API when the API allows it, else says WHY in words).
//!
//! This adapter orders NET positions (`posSide: net`, `crate::perp::OkxPerpRest::build_order_params`),
//! and an account in `long_short_mode` refuses every such order. Before this module the bridge only
//! REPORTED the mode (`crate::leverage_read::check_account_config`, and only for an account that states
//! a leverage), so a long/short account failed each order with the venue's own parameter error.
//!
//! | what | read | written |
//! |---|---|---|
//! | position mode | `GET /api/v5/account/config` `posMode` | `POST /api/v5/account/set-position-mode` `net_mode`, ONLY when the account is flat |
//! | account level | the same row's `acctLv` | NOTHING: named in words ([`level_note`]) |
//!
//! # The rule ([`settle_modes`])
//!
//! 1. `net_mode` already: nothing is written (idempotent; the probe costs one signed GET per start).
//! 2. `long_short_mode`: the account must be FLAT first, read three ways: no position with a non-zero
//!    `pos` (every instrument type), no resting order (`orders-pending`), no resting stop
//!    (`orders-algo-pending`, `ordType=conditional`, the one algo kind this adapter places). A read that
//!    fails reads as NOT flat (no write, the reason says which read). Anything else the venue counts
//!    (an algo order of a kind we never place) is the venue's own refusal to word.
//! 3. Flat: ONE `set-position-mode`, then the account is READ BACK ("never assume it worked", decision
//!    0182's rule): [`PositionMode::Healed`] only when the venue now says `net_mode`. The read-back
//!    follows a refused or timed-out write too, because a lost answer may hide a switch that landed.
//! 4. Not flat, refused by the venue, or not read back: [`PositionMode::Blocked`] with what blocks, in
//!    words, and the account is left exactly as it was. The bridge never closes a position or cancels an
//!    order to make room for the switch.
//!
//! # Tiers
//!
//! **The rule runs on the SIMULATED (demo) network only; a live (real-money) account is never read and
//! never written for a mode** (owner ruling 2026-10-26: no exchange write on a live account).
//! okx has ONE host for both networks and the tier is the `x-simulated-trading: 1` header, so the gate is
//! the very transport that would send the request: [`crate::transport::OkxTransport::simulated`] is true
//! only for a transport that sends the header, defaults to FALSE (a transport that does not say is the
//! real network), and [`settle_modes`] answers [`PositionMode::LeftAlone`] with no request otherwise,
//! whatever tier label the account row states. A live account therefore starts exactly as it did before
//! this module (its start still posts the leverage, which is older and is not a mode), and a net order
//! that a long/short live account has refused is worded at the order ([`explain_refusal`]): the bridge
//! changes no live account's mode, and the hand fix is named. No setting was added.
//!
//! # What this does NOT do
//!
//! * It is a START probe. A mode flipped on the venue while the engine runs is seen at the next start
//!   (`account.active` off and on).
//! * It never touches the account LEVEL. A Simple-mode account (`acctLv` 1) is named in words and left as
//!   it is. `set-account-level` is reachable with a trade key (the demo account, at level 3, was answered
//!   success for a write to level 3, 2026-10-26, `okx_modes_probes_smoke`), but a switch UP from level 1
//!   cannot be drilled on a level-3 account, OKX's page (read from memory, not verified here) says the
//!   first switch is taken on its web or app, and the write changes how the account's margin works. It
//!   is the one open item of the owner's rule for this venue.
//! * The gatewayhub lane's stated-leverage probe (decision 0182) still refuses a long/short account
//!   BEFORE this runs, so a figure stated for such an account is applied from the start AFTER the heal.
//!
//! The answers' shapes: `account/config`'s `posMode` and `acctLv` are measured on the demo host (the
//! 2026-10-26 read of `tests/okx_leverage_smoke.rs`); `tests/okx_modes_smoke.rs` is the live drill of
//! the write, the refusal and the read-back, and its `MEASURED` lines are the record.

use serde_json::{Value, json};
use vike_bridge_core::json::json_num;
use vike_bridge_core::signer::OkxV5Signer;

use crate::account_config::PATH_ACCOUNT_CONFIG;
use crate::perp::{PATH_ORDERS_ALGO_PENDING, PATH_ORDERS_PENDING, PATH_POSITIONS};
use crate::transport::{OkxTransport, unwrap_okx};

/// `POST /api/v5/account/set-position-mode`.
pub const PATH_SET_POSITION_MODE: &str = "/api/v5/account/set-position-mode";

/// The one position mode this adapter orders in (`posSide: net`).
const NET: &str = "net_mode";
/// The other word the venue documents; a mode outside the two is not guessed at.
const LONG_SHORT: &str = "long_short_mode";

/// What [`settle_modes`] found and did about the account's position mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PositionMode {
    /// Already `net_mode`: nothing was written.
    Net,
    /// Was `long_short_mode` on a flat account; switched, and read back as `net_mode`.
    Healed,
    /// The mode could not be read, or is a word this adapter does not know: nothing was written.
    Unknown(String),
    /// `long_short_mode` and NOT switched: why, in words. The account is as it was.
    Blocked(String),
    /// Not the simulated network (`OkxTransport::simulated` is false): nothing was read and nothing was
    /// written; the account starts as it did before this module.
    LeftAlone,
}

/// The start probe's whole answer: the position mode, and a note about the account level when the
/// level is one the venue may refuse swap orders on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModesReport {
    pub position: PositionMode,
    pub level: Option<String>,
}

/// The account level in words when it is `1` (Simple mode, the level of a fresh account). `None` for any
/// other level: no claim is made about 2, 3 or 4. PURE, over `data[0]` of `account/config`.
#[must_use]
pub fn level_note(row: &Value) -> Option<String> {
    (row.get("acctLv").and_then(json_num) == Some(1.0)).then(|| {
        "the account is in Simple mode (acctLv 1), which OKX may refuse swap orders on. This \
         bridge does not change an account level: choose single-currency or multi-currency margin \
         mode in the OKX app or web (Settings, Account mode), then restart this account's engine"
            .to_string()
    })
}

/// The venue's words for a refused order, with the account's position mode named when the venue's words
/// point at it. OKX answers a net order on a long/short account `Parameter posSide error` (measured on
/// the demo host, `tests/okx_modes_smoke.rs`), which names a parameter and not the account setting
/// behind it, so the operator who reads only the order's refusal would be left to guess. Any other
/// message passes through UNCHANGED. The sentence says "usually" because the venue's words are all that
/// is known here; the start's own sentence ([`PositionMode::Blocked`], in the daemon's log) says what
/// blocked the switch.
///
/// `simulated` is the sending transport's own answer ([`OkxTransport::simulated`]): on the real network the
/// bridge changes no account mode, so the sentence names the hand fix instead of promising a switch.
#[must_use]
pub fn explain_refusal(msg: &str, simulated: bool) -> String {
    if !msg.contains("posSide") {
        return msg.to_string();
    }
    if !simulated {
        return format!(
            "{} (usually the account is in long/short position mode and this adapter orders net \
             positions; the bridge changes no LIVE account's mode: switch the account to Net mode \
             in OKX's trading settings, Position mode, which OKX allows only while the account \
             holds no position and no order, then send the order again)",
            msg.trim_end()
        );
    }
    format!(
        "{} (usually the account is in long/short position mode and this adapter orders net \
         positions; the bridge switches the account to net mode when its engine starts while the \
         account is flat, so close the positions, cancel the orders and switch this account off \
         and on)",
        msg.trim_end()
    )
}

/// A signed GET on the short-timeout, rate-gate-exempt path (a start-time read, never an order).
fn read<T: OkxTransport>(
    transport: &T,
    signer: &OkxV5Signer,
    base_url: &str,
    path: &str,
    params: &[(&str, Value)],
) -> Result<Value, String> {
    transport
        .signed_requery(base_url, path, "GET", params, signer)
        .and_then(unwrap_okx)
        .map_err(|e| format!("{path} could not be read: {e}"))
}

/// `data[0]` of `account/config`.
fn config<T: OkxTransport>(
    transport: &T,
    signer: &OkxV5Signer,
    base_url: &str,
) -> Result<Value, String> {
    let data = read(transport, signer, base_url, PATH_ACCOUNT_CONFIG, &[])?;
    data.get(0).cloned().ok_or_else(|| format!("{PATH_ACCOUNT_CONFIG} answered no row"))
}

/// The instruments with a non-zero position in the unwrapped `positions` answer (every instrument
/// type). PURE. An unreadable `pos` is NOT flat: the row is listed, because an unreadable size must
/// not read as no position.
#[must_use]
pub fn open_positions(data: &Value) -> Vec<String> {
    data.as_array()
        .into_iter()
        .flatten()
        .filter(|row| row.get("pos").and_then(json_num).is_none_or(|pos| pos != 0.0))
        .map(|row| row.get("instId").and_then(Value::as_str).unwrap_or("?").to_string())
        .collect()
}

/// What keeps the account from being flat, in words (empty = flat), or the read that failed. Three
/// reads, each over every instrument type: positions, resting orders, resting conditional stops.
fn blockers<T: OkxTransport>(
    transport: &T,
    signer: &OkxV5Signer,
    base_url: &str,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let positions = open_positions(&read(transport, signer, base_url, PATH_POSITIONS, &[])?);
    if !positions.is_empty() {
        out.push(format!("{} open position(s) ({})", positions.len(), positions.join(", ")));
    }
    let count = |data: &Value| data.as_array().map_or(0, Vec::len);
    let orders = count(&read(transport, signer, base_url, PATH_ORDERS_PENDING, &[])?);
    if orders > 0 {
        out.push(format!("{orders} resting order(s)"));
    }
    let stops = count(&read(
        transport,
        signer,
        base_url,
        PATH_ORDERS_ALGO_PENDING,
        &[("ordType", json!("conditional"))],
    )?);
    if stops > 0 {
        out.push(format!("{stops} resting stop(s)"));
    }
    Ok(out)
}

/// The refusal sentence: the account is long/short, the bridge orders net, and `why` is what stopped the
/// switch. Names the way out.
fn blocked(why: &str) -> PositionMode {
    PositionMode::Blocked(format!(
        "the account is in long/short position mode and this adapter orders net positions, so OKX \
         refuses its orders; the bridge switches the account to net mode only while it is flat, and \
         could not: {why}. Close the positions and cancel the orders, then switch this account's \
         engine off and on (account.active) and the bridge switches the mode"
    ))
}

/// **The start probe** (module doc): read the account's modes, and switch a flat long/short account to
/// net. Never panics, never fails a start: every outcome is a [`ModesReport`] the caller announces.
pub fn settle_modes<T: OkxTransport>(
    transport: &T,
    signer: &OkxV5Signer,
    base_url: &str,
) -> ModesReport {
    // The gate (owner ruling 2026-10-26: no exchange write on a live account): only a transport that sends
    // the simulated-trading header is probed; the real network is left alone with no request.
    if !transport.simulated() {
        return ModesReport { position: PositionMode::LeftAlone, level: None };
    }
    let row = match config(transport, signer, base_url) {
        Ok(row) => row,
        Err(why) => return ModesReport { position: PositionMode::Unknown(why), level: None },
    };
    let level = level_note(&row);
    let position = match row.get("posMode").and_then(Value::as_str) {
        Some(NET) => PositionMode::Net,
        Some(LONG_SHORT) => switch_to_net(transport, signer, base_url),
        Some(other) => PositionMode::Unknown(format!(
            "account/config answers posMode {other:?}, a word this adapter does not know"
        )),
        None => PositionMode::Unknown("account/config carries no posMode".to_string()),
    };
    ModesReport { position, level }
}

/// The write half of [`settle_modes`]: flat check, one `set-position-mode`, read back.
fn switch_to_net<T: OkxTransport>(
    transport: &T,
    signer: &OkxV5Signer,
    base_url: &str,
) -> PositionMode {
    match blockers(transport, signer, base_url) {
        Err(why) => return blocked(&format!("{why}, so the account cannot be shown flat")),
        Ok(found) if !found.is_empty() => {
            return blocked(&format!("it is not flat: {}", found.join("; ")));
        }
        Ok(_) => {}
    }
    let written = transport
        .signed(base_url, PATH_SET_POSITION_MODE, "POST", &[("posMode", json!(NET))], signer)
        .and_then(unwrap_okx);
    // Read back whatever the write answered: a timed-out POST may have landed, and only the venue's
    // own word for the mode says so.
    match (written, config(transport, signer, base_url)) {
        (_, Ok(row)) if row.get("posMode").and_then(Value::as_str) == Some(NET) => {
            PositionMode::Healed
        }
        (Err(e), _) => {
            blocked(&format!("OKX refused set-position-mode (code {}: {})", e.code, e.msg))
        }
        (Ok(_), Ok(row)) => blocked(&format!(
            "set-position-mode answered success but the account still reads posMode {:?}",
            row.get("posMode").and_then(Value::as_str).unwrap_or("?")
        )),
        (Ok(_), Err(why)) => blocked(&format!(
            "set-position-mode was sent but the account could not be read back: {why}"
        )),
    }
}

/// Say what [`settle_modes`] found, at the level it deserves: a switch is a WARN (an account setting
/// moved), a refusal an ERROR (every order of this account will be refused by the venue), an unreadable
/// mode a WARN. The account id is never logged and no credential is in any sentence.
pub fn announce(report: &ModesReport, symbol: &str) {
    match &report.position {
        PositionMode::Net => {
            tracing::debug!(target: "vike_okx", %symbol, "account position mode is net_mode");
        }
        PositionMode::Healed => {
            tracing::warn!(target: "vike_okx", %symbol, "the account was in long_short_mode and flat: switched it to net_mode (set-position-mode), read back as net_mode");
        }
        PositionMode::Unknown(why) => {
            tracing::warn!(target: "vike_okx", %symbol, %why, "the account's position mode could not be settled (continuing: the venue judges each order)");
        }
        PositionMode::Blocked(why) => {
            tracing::error!(target: "vike_okx", %symbol, %why, "OKX will refuse this account's orders");
        }
        PositionMode::LeftAlone => {
            tracing::debug!(target: "vike_okx", %symbol, "not the simulated network: the bridge reads and writes no account mode here");
        }
    }
    if let Some(note) = &report.level {
        tracing::error!(target: "vike_okx", %symbol, %note, "account level");
    }
}

#[cfg(test)]
#[path = "account_modes_tests.rs"]
mod account_modes_tests;
