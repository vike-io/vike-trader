//! **okx's READ of an account's leverage** (decision 0182): what the gatewayhub's lane asks the venue
//! before a start (can this account safely be asked to hold N?) and after it (does it now hold N?).
//! Signed GETs through this crate's own signer and transport, and NO write: the one write is
//! `crate::perp::OkxPerpRest::set_leverage` (`mgnMode: "cross"`), fired once by the exec thread
//! (`vike_bridge_core::leverage_read`'s module doc has the shape).
//!
//! | read | endpoint | what is taken from it |
//! |---|---|---|
//! | the account's mode | `GET /api/v5/account/config` | `acctLv` must be `2` (futures) or `3` (multi-currency margin): `1` (spot only) and `4` (portfolio margin) are refused; `posMode` must be `net_mode` (the adapter orders `posSide net`) |
//! | what is held now | `GET /api/v5/account/leverage-info?mgnMode=cross&instId=` | EXACTLY ONE row (net mode), its `lever` |
//! | whether a position is open | `GET /api/v5/account/positions?instType=SWAP&instId=` | any row with `pos` non-zero; a hedge (`posSide` not `net`) or an ISOLATED open position is refused |
//! | the instrument's maximum | `GET /api/v5/public/instruments?instType=SWAP&instId=` (public) | `lever`; an inverse (`ctType` not `linear`) is refused |
//!
//! ⚠ **The write is `cross` only, and an order may still go isolated** (`tdMode` rides each order;
//! `OkxPerpRest::set_leverage` posts `mgnMode: cross` and nothing sets an isolated leverage). This
//! reader cannot see that, so the first release is demo-only (decision 0182, hazard 4).
//!
//! ⚠ **The parsers follow the venue's DOCUMENTED shapes; only `account/config`'s `uid`, `mainUid`,
//! `perm` and `posMode` have been measured on demo** (`crate::account_config`). `acctLv`,
//! `leverage-info` and a flat symbol's `positions` answer are not proven on the demo host. Every
//! unreadable answer is a refusal, so a wrong guess ends at 1x with the reason. A live demo drill is
//! owed. The transport is ungated: a start spends at most four probe reads and seven poll pairs. The
//! signed reads ride the transport's SHORT-timeout agent (`signed_requery`, the one the resync and the
//! confirm use), so a venue that stalls costs a read five seconds, not thirty.

use serde_json::{Value, json};
use vike_bridge_core::json::json_num;
use vike_bridge_core::leverage_read::{HeldLeverage, LeverageReader};
use vike_bridge_core::signer::OkxV5Signer;
use vike_model::AccountTier;
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::credentials::api_credentials;

use crate::account_config::PATH_ACCOUNT_CONFIG;
use crate::perp::{
    PATH_INSTRUMENTS, PATH_POSITIONS, REST, VENUE, parse_okx_perp_instruments, simulated,
};
use crate::transport::{OkxTransport, UreqOkxTransport, unwrap_okx};

/// `GET /api/v5/account/leverage-info`.
pub const PATH_LEVERAGE_INFO: &str = "/api/v5/account/leverage-info";

/// The account's mode, from `data[0]` of `GET /api/v5/account/config`. PURE.
///
/// # Errors
/// Spot-only or portfolio-margin account levels, a hedge (`long_short_mode`) position mode, or either
/// field missing: the engine models plain cross margin with net positions.
pub fn check_account_config(row: &Value) -> Result<(), String> {
    let level = row.get("acctLv").and_then(json_num);
    if level == Some(1.0) {
        return Err("the account is spot-only (acctLv 1): it has no swap margin".into());
    }
    if level == Some(4.0) {
        return Err("the account is in portfolio margin (acctLv 4), which cannot set or read a \
                    cross swap leverage"
            .into());
    }
    match level {
        Some(lv) if lv == 2.0 || lv == 3.0 => {}
        Some(other) => {
            return Err(format!("the account level acctLv {other} is not one this reader knows"));
        }
        None => {
            return Err("account/config carries no acctLv, so the account level is unknown".into());
        }
    }
    match row.get("posMode").and_then(Value::as_str) {
        Some("net_mode") => Ok(()),
        Some(other) => Err(format!(
            "the account's position mode is {other:?}; this adapter orders net positions \
             (posSide net)"
        )),
        None => Err("account/config carries no posMode, so the position mode is unknown".into()),
    }
}

/// The cross leverage held on `symbol`, from the unwrapped `data` array of `leverage-info`. PURE.
///
/// # Errors
/// No row, several rows (a long/short account answers one per side), a row that is not net or not
/// cross, or a `lever` that is not a figure of at least 1.
pub fn parse_leverage_info(data: &Value, symbol: &str) -> Result<f64, String> {
    let rows: Vec<&Value> = data
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|r| {
                    r.get("instId")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case(symbol))
                })
                .collect()
        })
        .ok_or_else(|| "leverage-info carries no data array".to_string())?;
    let row = match rows.as_slice() {
        [] => return Err(format!("the venue answered no leverage row for {symbol}")),
        [one] => *one,
        many => {
            return Err(format!(
                "the venue answered {} leverage rows for {symbol}: a long/short account, and this \
                 adapter orders net positions",
                many.len()
            ));
        }
    };
    if row.get("posSide").and_then(Value::as_str).is_some_and(|side| side != "net") {
        return Err("the leverage row is for a long/short side, not net".to_string());
    }
    if row.get("mgnMode").and_then(Value::as_str).is_some_and(|mode| mode != "cross") {
        return Err("the leverage row is not for cross margin".to_string());
    }
    row.get("lever")
        .and_then(json_num)
        .filter(|x| x.is_finite() && *x >= 1.0)
        .ok_or_else(|| "the leverage row carries no usable lever".to_string())
}

/// Whether the account holds an open position on `symbol`, from the unwrapped `data` array of
/// `positions`. PURE.
///
/// # Errors
/// An open position that is not a net, cross one: it is on a margin model this engine does not
/// describe, and a figure must not be moved under it.
pub fn parse_open_position(data: &Value, symbol: &str) -> Result<bool, String> {
    let rows = data.as_array().ok_or_else(|| "positions carries no data array".to_string())?;
    let mut open = false;
    for row in rows.iter().filter(|r| {
        r.get("instId").and_then(Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case(symbol))
    }) {
        // An unreadable size must not read as "flat": the open-position guard fails CLOSED.
        let Some(pos) = row.get("pos").and_then(json_num) else {
            return Err(format!("the {symbol} position row carries no usable pos"));
        };
        if pos == 0.0 {
            continue;
        }
        if row.get("posSide").and_then(Value::as_str).is_some_and(|side| side != "net") {
            return Err(format!("an open {symbol} position is a long/short side, not net"));
        }
        if row.get("mgnMode").and_then(Value::as_str).is_some_and(|mode| mode != "cross") {
            return Err(format!(
                "an open {symbol} position is isolated; the engine models cross margin"
            ));
        }
        open = true;
    }
    Ok(open)
}

/// The instrument's published maximum leverage, from the unwrapped `data` of `public/instruments`. PURE.
///
/// # Errors
/// No SWAP row for `symbol`, or one that is not a linear contract (the exec plane refuses inverse).
pub fn parse_max_leverage(data: &Value, symbol: &str) -> Result<Option<f64>, String> {
    let rows = parse_okx_perp_instruments(&json!({ "data": data }));
    let row = rows
        .get(&symbol.to_uppercase())
        .ok_or_else(|| format!("public/instruments lists no SWAP {symbol}"))?;
    if let Some(kind) = row.contract_type.as_deref()
        && kind != "linear"
    {
        return Err(format!(
            "{symbol} is a {kind} contract; this adapter trades linear swaps only"
        ));
    }
    Ok(row.max_leverage)
}

/// The reader for one account's mounted instrument over this crate's signed transport.
pub struct OkxLeverageReader<T: OkxTransport> {
    signer: OkxV5Signer,
    transport: T,
    base_url: String,
    symbol: String,
}

impl<T: OkxTransport> OkxLeverageReader<T> {
    /// A reader over `transport`, signing with `signer`, against `base_url`, for the instId `symbol`.
    pub fn new(
        signer: OkxV5Signer,
        transport: T,
        base_url: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        OkxLeverageReader { signer, transport, base_url: base_url.into(), symbol: symbol.into() }
    }

    /// One signed GET with the `{code, msg, data}` envelope unwrapped to `data`. The `Err` is the
    /// venue's own words and never a credential.
    fn data(&self, what: &str, path: &str, params: &[(&str, Value)]) -> Result<Value, String> {
        let body = self
            .transport
            .signed_requery(&self.base_url, path, "GET", params, &self.signer)
            .map_err(|e| format!("{what} could not be read: {e}"))?;
        unwrap_okx(body).map_err(|e| format!("{what} could not be read: {e}"))
    }
}

impl<T: OkxTransport> LeverageReader for OkxLeverageReader<T> {
    fn check_account(&self) -> Result<(), String> {
        let data = self.data("account/config", PATH_ACCOUNT_CONFIG, &[])?;
        let row = data.get(0).ok_or_else(|| "account/config answered no row".to_string())?;
        check_account_config(row)
    }

    fn venue_max(&self) -> Result<Option<f64>, String> {
        let body = self
            .transport
            .public(
                &self.base_url,
                PATH_INSTRUMENTS,
                &[("instType", "SWAP".to_string()), ("instId", self.symbol.clone())],
            )
            .map_err(|e| format!("public/instruments could not be read: {e}"))?;
        let data =
            unwrap_okx(body).map_err(|e| format!("public/instruments could not be read: {e}"))?;
        parse_max_leverage(&data, &self.symbol)
    }

    fn held(&self) -> Result<HeldLeverage, String> {
        let held = parse_leverage_info(
            &self.data(
                "leverage-info",
                PATH_LEVERAGE_INFO,
                &[("mgnMode", json!("cross")), ("instId", json!(self.symbol))],
            )?,
            &self.symbol,
        )?;
        let position_open = parse_open_position(
            &self.data(
                "positions",
                PATH_POSITIONS,
                &[("instType", json!("SWAP")), ("instId", json!(self.symbol))],
            )?,
            &self.symbol,
        )?;
        Ok(HeldLeverage { held, position_open })
    }
}

/// The reader for the instId `symbol` on the account whose credentials are `creds`, on the network
/// `tier` selects (`live` is mainnet: the `x-simulated-trading` header is dropped). The caller (the
/// gatewayhub's lane) has already checked that the table (`vike_secrets::LEVERAGE_VENUES`) opens this
/// tier.
///
/// # Errors
/// The account holds no complete okx key set (key, secret and passphrase).
pub fn leverage_reader(
    creds: &AccountCredentials<'_>,
    tier: AccountTier,
    symbol: &str,
) -> Result<OkxLeverageReader<UreqOkxTransport>, String> {
    let keys = api_credentials(creds, VENUE).ok_or_else(|| {
        "the account holds no complete okx key set (key, secret, passphrase)".to_string()
    })?;
    Ok(OkxLeverageReader::new(
        OkxV5Signer::new(&keys, vike_model::now_ms),
        UreqOkxTransport::new(simulated(tier == AccountTier::Live)),
        REST,
        symbol,
    ))
}

#[path = "leverage_read_tests.rs"]
#[cfg(test)]
mod leverage_read_tests;
