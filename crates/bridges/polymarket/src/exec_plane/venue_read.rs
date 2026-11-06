//! `venue_read` — an OPERATOR's read of a Polymarket account, asked of the venue now: the wallet's pUSD
//! balance with the allowance it has given each exchange contract, the CLOB's own list of the
//! account's open orders and the outcome-token POSITIONS the wallet holds
//! (`vike_model::VenueReadKind`; the gatewayhub's `TradeUp::VenueRead` asks it).
//!
//! ## Why this is not the reconcile client
//! The reconcile client ([`crate::exec_plane::recon_client`]) reads the same two endpoints, but it is
//! built only under `flags.poly_reconcile` (unset on the CI box: its passes fold what they find, and on a
//! live wallet that is its own decision), and its balance is one number with no allowance. A read the
//! OPERATOR asks for is a different act: it folds nothing, books nothing and changes nothing at the
//! venue, so it needs neither the flag nor a running pass, only the account's own key. It builds its own
//! L2 handshake (`GET /auth/derive-api-key`, which never creates a key: [`ensure_l2`]) and makes one
//! signed GET.
//!
//! ## Both reads are account-wide
//! `GET /balance-allowance?asset_type=COLLATERAL` names the wallet's collateral and the approval it has
//! given each exchange; `GET /data/orders` lists every active order of the account, whatever the market
//! (the reconcile client's open-orders read is this same call). An empty list is an answer. A list the
//! venue paged (a `next_cursor` that is not its end mark) is returned with `truncated` set, never as if
//! complete.
//!
//! ## The positions are a keyless read of the PUBLIC data-api
//! `GET https://data-api.polymarket.com/positions?user=<funder>` lists what a wallet holds and needs no
//! signature and no L2 key, so this kind makes NO derive call and signs nothing: the one request carries
//! the funder address (the account's `ADDRESS`, else `FUNDER`, else the key's own EOA) and nothing else.
//! It asks for `sizeThreshold=0` (the endpoint's default hides a holding under one share) and the most
//! rows one read carries. An empty wallet answers `[]`, which is an answer. A row names its market in
//! words and states whether the market has RESOLVED (`redeemable`): that is a resolution flag, not a
//! winner flag, and a losing leg reads `true` too (`crate::exec_plane::settlement::positions`'s
//! `Position::redeemable`), so the row also carries the data-api's own price and value of the holding.
//! This read folds nothing (the reconcile client reads the same endpoint under `flags.poly_reconcile`
//! and FOLDS what it finds), places nothing and redeems nothing.
//!
//! ## What is not read
//! The `/balance-allowance` body shape is the documented one and has not been read live in this repo
//! (`crate::exec_plane::preflight`'s module doc): two shapes parse, anything else is an error in words
//! here (the preflight turns it into "unknown" and lets the order through; an operator asking is owed
//! the failure). The limit price of a resting order is the row's `price`.

use std::time::Duration;

use serde_json::Value;
use vike_bridge_core::side::sign_default_buy;
use vike_model::{
    MAX_VENUE_OPEN_ORDERS, MAX_VENUE_POSITIONS, VenueAllowance, VenueBalance, VenueOpenOrder,
    VenuePosition, VenueRead, VenueReadKind,
};
use vike_secrets::account_credentials::AccountCredentials;

use crate::config::{CLOB_BASE, DATA_API_BASE, PolymarketCreds};
use crate::exec_plane::exec::get_signed_within;
use crate::exec_plane::l1::ensure_l2;
use crate::exec_plane::order::{SignatureType, exchange_contract};
use crate::exec_plane::preflight::{BalanceAllowance, exchange_name, loggable};
use crate::exec_plane::recon_client::{
    normalize_order_status, num, rows, signature_type_for_account, str_field,
};

/// How long ONE signed GET may take. An operator is waiting on it, so it is far shorter than the order
/// path's [`crate::egress::REST_TIMEOUT`]; the derive that precedes it keeps its own bound.
const READ_TIMEOUT: Duration = Duration::from_secs(12);

/// The rows asked of `/data/orders` in one read ([`MAX_VENUE_OPEN_ORDERS`]).
const ORDERS_LIMIT: usize = MAX_VENUE_OPEN_ORDERS;

/// The rows asked of the data-api's `/positions` in one read ([`MAX_VENUE_POSITIONS`], the endpoint's own
/// ceiling): a list of exactly this size may have been cut by the page and is said to be.
const POSITIONS_LIMIT: usize = MAX_VENUE_POSITIONS;

/// The CLOB's "no further page" cursor (base64 of `-1`), the value its list endpoints end on.
const END_CURSOR: &str = "LTE=";

/// USDC and every Polymarket CTF token use 6 decimals: the wire's base units over this are whole units.
const DECIMALS: f64 = 1_000_000.0;

/// Epoch seconds of 5138-11-16: any `created_at` below this is in SECONDS, not milliseconds.
const SECONDS_CEILING: i64 = 100_000_000_000;

/// The answer to `kind` for the account whose credential fields are `creds`, on the CLOB the process
/// dials ([`CLOB_BASE`], through the declared egress). `Err` is the operator's sentence.
pub(crate) fn venue_read(
    creds: &AccountCredentials<'_>,
    kind: VenueReadKind,
) -> Result<VenueRead, String> {
    let loaded = crate::config::load_polymarket_creds_from(creds)
        .ok_or_else(|| "the account holds no polymarket private key".to_string())?;
    let signer = vike_bridge_core::eip712::eth_address_from_private_key(&loaded.private_key)
        .map_err(|e| format!("the account's PRIVATE_KEY is not a usable key ({e})"))?;
    // ClobAuth is signed by, and names, the key's own EOA — not the funder `address` the loader put
    // there (a deposit wallet's), which the signed GETs below do not use.
    if kind == VenueReadKind::Positions {
        // Public data: no L2 key to derive and nothing to sign (module doc). The funder is the wallet
        // that holds the shares; a bare-key account holds them at its own EOA.
        return positions_with(DATA_API_BASE, &funder_of(loaded.address, signer));
    }
    let mut l2 = PolymarketCreds { address: signer, ..loaded };
    ensure_l2(&mut l2, CLOB_BASE, vike_model::time::clock::now_secs())
        .map_err(|e| format!("could not obtain the wallet's L2 API key: {e}"))?;
    read_with(CLOB_BASE, &l2, signature_type_for_account(creds), kind)
}

/// [`venue_read`] over a derived L2 trio on `base`: the seam the loopback tests drive.
pub(crate) fn read_with(
    base: &str,
    l2: &PolymarketCreds,
    signature_type: SignatureType,
    kind: VenueReadKind,
) -> Result<VenueRead, String> {
    match kind {
        VenueReadKind::Balance => {
            let query = format!("asset_type=COLLATERAL&signature_type={}", signature_type.code());
            let body = signed_get(base, "/balance-allowance", &query, l2)?;
            balance_from(&body).map(VenueRead::Balance)
        }
        VenueReadKind::OpenOrders => {
            let body = signed_get(base, "/data/orders", &format!("limit={ORDERS_LIMIT}"), l2)?;
            let (orders, truncated) = open_orders_from(&body)?;
            Ok(VenueRead::OpenOrders { orders, truncated })
        }
        // Never reached from `venue_read`, which answers it before an L2 key is derived; this seam
        // signs for the CLOB and the positions are not the CLOB's.
        VenueReadKind::Positions => {
            Err("polymarket's positions are read from the public data-api, not the signed CLOB"
                .to_string())
        }
    }
}

/// The wallet that holds the account's shares: the account's own `ADDRESS`/`FUNDER` when it names one (a
/// deposit wallet is not the key's EOA), else the key's EOA.
fn funder_of(address: String, signer: String) -> String {
    if address.is_empty() { signer } else { address }
}

/// `GET /positions` of the data-api for `funder` on `data_api`: the seam the loopback tests drive. The
/// address goes into a query string, so only a plain `0x` address is accepted.
pub(crate) fn positions_with(data_api: &str, funder: &str) -> Result<VenueRead, String> {
    let is_address = funder.len() == 42
        && funder.starts_with("0x")
        && funder[2..].bytes().all(|b| b.is_ascii_hexdigit());
    if !is_address {
        return Err(
            "the account's funder is not a 0x address, so its positions cannot be asked for".into(),
        );
    }
    let query = format!("user={funder}&sizeThreshold=0&limit={POSITIONS_LIMIT}");
    let url = format!("{data_api}/positions?{query}");
    let mut resp = crate::egress::agent_with_timeout(READ_TIMEOUT)
        .get(&url)
        .call()
        .map_err(|e| format!("polymarket /positions could not be read: network: {e}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("polymarket /positions could not be read: network: {e}"))?;
    // The status head only, never the body (the rule of `signed_get`).
    if !(200..300).contains(&status) {
        return Err(format!("polymarket /positions could not be read: HTTP {status}"));
    }
    let body: Value = serde_json::from_str(&text)
        .map_err(|_| format!("polymarket's /positions answer (HTTP {status}) is not JSON"))?;
    let (positions, truncated) = positions_from(&body)?;
    Ok(VenueRead::Positions { positions, truncated })
}

/// A number the row states, as a JSON number or a decimal string; `None` when it states none, so an
/// absent price is not read as a price of zero.
fn stated(row: &Value, key: &str) -> Option<f64> {
    let v = row.get(key)?;
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// `GET /positions` (a bare array, or `{data: [..]}`) → the held positions and whether the list may be
/// incomplete. A row with no `asset` names nothing this read can hold and is skipped, as the reconcile
/// client skips it; a zero-size row is kept as the venue lists it.
pub(crate) fn positions_from(body: &Value) -> Result<(Vec<VenuePosition>, bool), String> {
    let list =
        rows(body).ok_or_else(|| "polymarket's /positions answer is not a list".to_string())?;
    let truncated = list.len() >= POSITIONS_LIMIT;
    let positions = list
        .iter()
        .take(POSITIONS_LIMIT)
        .filter(|row| !str_field(row, "asset").is_empty())
        .map(|row| VenuePosition {
            symbol: str_field(row, "asset"),
            title: str_field(row, "title"),
            outcome: str_field(row, "outcome"),
            qty: num(row, "size"),
            avg_price: stated(row, "avgPrice"),
            price: stated(row, "curPrice"),
            value: stated(row, "currentValue"),
            redeemable: row.get("redeemable").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect();
    Ok((positions, truncated))
}

/// One L2-signed GET under [`READ_TIMEOUT`], its failure reduced to what is safe to repeat: the status
/// head, never the body (a proxy's error page can echo the request's `POLY_*` headers).
fn signed_get(base: &str, path: &str, query: &str, l2: &PolymarketCreds) -> Result<Value, String> {
    get_signed_within(base, path, query, l2, READ_TIMEOUT)
        .map_err(|e| format!("polymarket {path} could not be read: {}", loggable(&e)))
}

/// 6-decimal base units → whole units.
fn whole(base_units: u128) -> f64 {
    base_units as f64 / DECIMALS
}

/// A saturated reading (`2^256-1` does not fit a `u128`, so [`BalanceAllowance`] parses it as
/// `u128::MAX`) or anything within a factor of two of it: the approval is the chain's maximum.
fn is_unlimited(base_units: u128) -> bool {
    base_units > u128::MAX / 2
}

/// The contract in words when it is one of the two exchanges an order settles through.
fn contract_name(spender: &str) -> String {
    [false, true]
        .into_iter()
        .find(|neg_risk| exchange_contract(*neg_risk).eq_ignore_ascii_case(spender))
        .map(|neg_risk| exchange_name(neg_risk).to_string())
        .unwrap_or_default()
}

fn allowance(spender: &str, base_units: u128) -> VenueAllowance {
    VenueAllowance {
        spender: spender.to_string(),
        name: contract_name(spender),
        amount: whole(base_units),
        unlimited: is_unlimited(base_units),
    }
}

/// `GET /balance-allowance` → the collateral balance and one approval per contract the venue lists. The
/// older shape (one `allowance` figure) is kept as a single row with no spender: the venue did not say
/// which contract it is for, and a row that named one would be a guess.
pub(crate) fn balance_from(body: &Value) -> Result<VenueBalance, String> {
    let snap = BalanceAllowance::parse(body)
        .ok_or_else(|| "polymarket's /balance-allowance answer is not an object".to_string())?;
    let balance = snap.balance.ok_or_else(|| {
        "polymarket's /balance-allowance answer carries no balance figure".to_string()
    })?;
    let mut allowances: Vec<VenueAllowance> =
        snap.per_spender.iter().map(|(spender, raw)| allowance(spender, *raw)).collect();
    if allowances.is_empty()
        && let Some(single) = snap.single
    {
        allowances.push(allowance("", single));
    }
    Ok(VenueBalance { asset: "pUSD".to_string(), balance: whole(balance), allowances })
}

/// Whether the list says another page follows: a `next_cursor` that is neither empty nor the end mark.
fn cursor_is_more(body: &Value) -> bool {
    body.get("next_cursor")
        .and_then(Value::as_str)
        .is_some_and(|c| !c.is_empty() && c != END_CURSOR)
}

/// A venue timestamp (`created_at`) as epoch ms: the CLOB states seconds, and a value that is already
/// milliseconds is left alone.
fn epoch_ms(raw: f64) -> i64 {
    let raw = raw as i64;
    if raw <= 0 {
        0
    } else if raw < SECONDS_CEILING {
        raw.saturating_mul(1000)
    } else {
        raw
    }
}

/// `GET /data/orders` (a bare array, or `{data: [..], next_cursor}`) → the open orders with the limit
/// price each rests at, and whether the list is known to be incomplete. A fresh process holds no
/// registry of the CLOB ids it placed, so no row names a client order id.
pub(crate) fn open_orders_from(body: &Value) -> Result<(Vec<VenueOpenOrder>, bool), String> {
    let list =
        rows(body).ok_or_else(|| "polymarket's /data/orders answer is not a list".to_string())?;
    let mut truncated = cursor_is_more(body) || list.len() > ORDERS_LIMIT;
    let mut orders = Vec::with_capacity(list.len().min(ORDERS_LIMIT));
    for row in list.iter().take(ORDERS_LIMIT) {
        let (original, matched) = (num(row, "original_size"), num(row, "size_matched"));
        let price = num(row, "price");
        orders.push(VenueOpenOrder {
            venue_order_id: str_field(row, "id"),
            client_order_id: None,
            symbol: str_field(row, "asset_id"),
            side: sign_default_buy(&str_field(row, "side")),
            price: (price > 0.0).then_some(price),
            qty: original,
            filled_qty: matched,
            status: normalize_order_status(&str_field(row, "status"), matched, original)
                .as_str()
                .to_string(),
            ts_ms: epoch_ms(num(row, "created_at")),
        });
    }
    // A list that is exactly the size asked for may have been cut by the venue's own page size.
    truncated |= orders.len() == ORDERS_LIMIT;
    Ok((orders, truncated))
}

#[path = "venue_read_tests.rs"]
#[cfg(test)]
mod venue_read_tests;
