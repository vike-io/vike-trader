//! `venue_read` — an OPERATOR's read of a Bybit account, asked of the venue now (`vike_model::VenueReadKind`; the
//! gatewayhub's `TradeUp::VenueRead` asks it through the adapter's `VenueAdapter::venue_read`): the BALANCE, signed
//! GETs of `/v5/account/info` (the account's version) and `/v5/account/wallet-balance` with `accountType=UNIFIED`,
//! plus `accountType=CONTRACT` on a classic or UTA 1.0 account; and the OPEN ORDERS of every book (below). Nothing
//! changes at the venue and nothing is folded.
//!
//! ## A figure per coin of the unified wallet
//! The venue's page (`bybit-exchange.github.io/docs/v5/account/wallet-balance`, read 2026-11-09) takes `UNIFIED`
//! and, with no `coin`, lists every coin with an asset or a liability. On a UTA 2.0 account a coin-margined (inverse)
//! contract is margined and settled in its coin INSIDE that wallet (`bybit-exchange.github.io/docs/v5/acct-mode`:
//! UTA 2.0 integrates inverse contracts, and `accountType=CONTRACT` is not supported there), so the `BTC` row is the
//! wallet a `BTCUSD` engine keeps its books in, the very figure its reconcile client adopts
//! (`crate::reconcile`'s `ReconcileClientBybit`, decision 0258). Each coin's `walletBalance` is read (its cash, with
//! its realized PnL, fees and funding in it; not `equity`, which adds unrealized PnL): the first coin with money in
//! [`VenueReadBalance::asset`], every other one with money in [`VenueReadBalance::other_assets`], as deribit's read does. An
//! account with no money at all answers the first row the venue lists at zero, or no asset at zero when it lists none.
//!
//! ## The separate inverse wallet of a UTA 1.0 account
//! `account/info`'s `unifiedMarginStatus` says the version (`crate::reconcile`'s `inverse_wallet_of`). On a classic
//! or UTA 1.0 account (`1`, `3`, `4`) inverse contracts live in a separate `CONTRACT` wallet, so its coins with money in
//! them join [`VenueReadBalance::other_assets`], each named `<COIN> (CONTRACT wallet)`; the inverse engine's reconcile
//! client reads its coin there too. An account that answers `CONTRACT` with "only support UNIFIED" (the demo host,
//! measured 2026-11-09, on an account stating `3`: `crate::reconcile`'s `only_unified`) has no such wallet and
//! lists the unified one alone. A version this bridge does not know, or an `account/info` that cannot be read, FAILS
//! the read: which wallets the account has is not guessed.
//!
//! ## The OPEN ORDERS of every book
//! The venue lists open orders per category, and a `linear` list per symbol, base coin or settle coin, so one call is
//! not the account: the read walks `GET /v5/order/realtime` once per book (`linear` with `settleCoin` `USDT`, then
//! `USDC`, then `inverse`, `spot` and `option` on the category alone), every page, with no `orderFilter`, which lists
//! a resting conditional (a stop, a take-profit) beside the active orders (measured on the demo host for the reconcile
//! read, 2026-10-24). The walk and the row parser are the reconcile client's (`crate::reconcile`'s
//! `ReconcileClientBybit::account_open_orders`), so the two reads cannot disagree about a row: our order client id is the
//! `orderLinkId`, a spot pair is named with its `.S` as the engine names it, and a size is the venue's (a coin on a
//! linear or spot row, dollars on an inverse one). Any walk failing fails the read. Not listed: a spot TP/SL or OCO
//! that the venue keeps outside the default filter on a CLASSIC account, and a list past 40 pages (the walk refuses
//! rather than cut). Not measured: the settle-coin and category-only request shapes (the reconcile walk asks by
//! symbol).
//!
//! ## What is not read
//! Positions stay `not offered`: the venue lists them per category and per settle coin or symbol.
//!
//! ## The host and the transport
//! The host is the account's `trade` `rest` `venue_url` row at its type (`crate::execution::rest::endpoints`, the one lookup the
//! start, the leverage write and the mode heal take; decision 0265): a demo account never reads a live row, and a
//! missing row refuses the read with the row named. Ungated and on the SHORT-timeout agent (`signed_requery`), as
//! `crate::account::leverage`: one read an operator asks for, which spends nothing of the order-rate window and stalls a
//! gatewayhub thread five seconds a request at most (three requests on a classic or UTA 1.0 account).
//!
//! Measured: the shape of a coin row is the demo capture
//! `crates/bridges/bybit/tests/fixtures/classes/wallet_balance_btc_coin_scoped.json`; the demo smoke read the whole
//! answer on the CI box's `INV` row (2026-11-09: status `3`, `CONTRACT` refused "only support UNIFIED", the unified wallet's
//! USDC, BTC, USDT and DOGE), not committed as a fixture. A `CONTRACT` wallet that ANSWERS is not measured (the
//! documented shape is the unified one's).

use crate::signer::BybitV5Signer;
use serde_json::{Value, json};
use vike_bridge_core::json::json_number;
use vike_bridge_core::signer::VenueClock;
use vike_bridge_core::transport::VenueApiError;
use vike_model::{VenueRead, VenueReadAssetBalance, VenueReadBalance, VenueReadKind};
use vike_secrets::credentials::Credentials;

use crate::account::leverage::PATH_ACCOUNT_INFO;
use crate::execution::rest::{PATH_ACCOUNT, VENUE};
use crate::reconcile::{CONTRACT, ReconcileClientBybit, UNIFIED, inverse_wallet_of, only_unified};
use crate::transport::client::rest_transport_on;
use crate::transport::client::unwrap_envelope;
use vike_bridge_core::transport::json::RestTransportJson;

/// The answer to `kind` for the account whose pair is `creds`, on `rest_base`: the account's `trade` `rest`
/// `venue_url` row at its type (`crate::execution::rest::endpoints`, the lookup the start takes), or the refusal naming the
/// missing row, which is said only when the balance is asked. `Err` is the operator's sentence.
pub(crate) fn venue_read(
    creds: &Credentials,
    rest_base: Result<String, String>,
    kind: VenueReadKind,
) -> Result<VenueRead, String> {
    // A one-shot read: it reads the venue's clock for itself (owner ruling B-4).
    let clock = VenueClock::default();
    let signer = BybitV5Signer::new(creds, clock.ms());
    match kind {
        VenueReadKind::Balance => {
            read_balance(&rest_transport_on(&clock), &signer, &rest_base?).map(VenueRead::Balance)
        }
        VenueReadKind::OpenOrders => read_open_orders(&ReconcileClientBybit::new(
            signer,
            rest_transport_on(&clock),
            rest_base?,
            String::new(),
        )),
        VenueReadKind::Positions => Err(vike_model::not_offered_words(VENUE, kind)),
    }
}

/// The open orders over any transport (the seam the offline tests drive): every book's list
/// (`crate::reconcile`'s `ReconcileClientBybit::account_open_orders`), cut at [`vike_model::MAX_VENUE_OPEN_ORDERS`]
/// with the cut said (a walk that cannot finish fails instead).
pub fn read_open_orders<T: RestTransportJson>(
    client: &ReconcileClientBybit<T>,
) -> Result<VenueRead, String> {
    let (orders, truncated) = vike_model::open_orders_read(client.account_open_orders()?);
    Ok(VenueRead::OpenOrders { orders, truncated })
}

/// The read itself, over any transport: the seam the offline tests and the demo smoke
/// (`tests/bybit_smokes/bybit_venue_read_smoke.rs`) drive.
pub fn read_balance<T: RestTransportJson>(
    transport: &T,
    signer: &BybitV5Signer,
    rest_base: &str,
) -> Result<VenueReadBalance, String> {
    let get = |path: &str, params: &[(&str, Value)]| {
        transport.signed_requery(rest_base, path, "GET", params, signer).and_then(unwrap_envelope)
    };
    let said = |path: &str, e: VenueApiError| {
        format!("bybit {path} could not be read: [{}] {}", e.code, e.msg)
    };
    let info = get(PATH_ACCOUNT_INFO, &[]).map_err(|e| said(PATH_ACCOUNT_INFO, e))?;
    let separate = inverse_wallet_of(&info)? == CONTRACT;
    let body =
        get(PATH_ACCOUNT, &[("accountType", json!(UNIFIED))]).map_err(|e| said(PATH_ACCOUNT, e))?;
    let mut balance = balance_from(&body)?;
    if separate {
        match get(PATH_ACCOUNT, &[("accountType", json!(CONTRACT))]) {
            Ok(body) => balance.other_assets.extend(
                wallets(&body)?.into_iter().filter(|w| w.balance != 0.0).map(|w| {
                    VenueReadAssetBalance { asset: format!("{} {CONTRACT_LABEL}", w.asset), ..w }
                }),
            ),
            Err(e) if only_unified(&e) => {}
            Err(e) => return Err(said(PATH_ACCOUNT, e)),
        }
    }
    Ok(balance)
}

/// What follows a coin of the separate inverse wallet in the answer, so it never reads as the unified one's.
const CONTRACT_LABEL: &str = "(CONTRACT wallet)";

/// `result` of a wallet read → the `walletBalance` of every coin row, in the venue's order.
fn wallets(body: &Value) -> Result<Vec<VenueReadAssetBalance>, String> {
    let accounts = body
        .get("list")
        .and_then(Value::as_array)
        .ok_or_else(|| "bybit's wallet-balance answer carries no `list`".to_string())?;
    let mut wallets = Vec::new();
    for row in accounts.iter().filter_map(|a| a.get("coin").and_then(Value::as_array)).flatten() {
        let asset = row.get("coin").and_then(Value::as_str).unwrap_or("").to_string();
        let balance = row
            .get("walletBalance")
            .and_then(json_number)
            .ok_or_else(|| format!("bybit states no walletBalance for its `{asset}` row"))?;
        wallets.push(VenueReadAssetBalance { asset, balance });
    }
    Ok(wallets)
}

/// `result` of the unified wallet read → the `walletBalance` of each coin with money in it (module doc).
fn balance_from(body: &Value) -> Result<VenueReadBalance, String> {
    let mut wallets = wallets(body)?;
    if wallets.is_empty() {
        return Ok(VenueReadBalance {
            asset: String::new(),
            balance: 0.0,
            allowances: Vec::new(),
            other_assets: Vec::new(),
        });
    }
    let first = wallets.iter().position(|w| w.balance != 0.0).unwrap_or(0);
    let shown = wallets.remove(first);
    wallets.retain(|w| w.balance != 0.0);
    Ok(VenueReadBalance {
        asset: shown.asset,
        balance: shown.balance,
        allowances: Vec::new(),
        other_assets: wallets,
    })
}

#[path = "venue_read_tests.rs"]
#[cfg(test)]
mod venue_read_tests;
