//! `venue_read` — an OPERATOR's read of a Polymarket account, asked of the venue now: the wallet's pUSD
//! balance with the allowance it has given each exchange contract, the CLOB's own list of the
//! account's open orders and the outcome-token POSITIONS the wallet holds
//! (`vike_model::VenueReadKind`; the gatewayhub's `TradeUp::VenueRead` asks it).
//!
//! ## Why this is not the reconcile client
//! The reconcile client ([`crate::reconcile`]) reads the same two endpoints, but it is
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
//! `GET {data-api}/v2/positions?user=<funder>` lists what a wallet holds and needs
//! no signature and no L2 key, so this kind makes NO derive call and signs nothing: the one request
//! carries the funder address (the account's `ADDRESS`, else `FUNDER`, else the key's own EOA) and the
//! widest list the route offers (`crate::settlement::positions`' module doc is the field map
//! and says why each part of the query is there). It asks for the most rows one read carries and says
//! `truncated` when the venue's `has_more` says another page follows: this is ONE read, where the reconcile
//! client walks every page. An empty wallet answers an empty list, which is an answer. A row names its
//! market in words and states whether the market has RESOLVED (`redeemable`): that is a resolution flag,
//! not a winner flag, and a losing leg reads `true` too (`Position::redeemable`), so the row also carries
//! the data-api's own price and value of the holding. This read folds nothing (the reconcile client reads
//! the same endpoint under `flags.poly_reconcile` and FOLDS what it finds), places nothing and redeems
//! nothing.
//!
//! ## What is not read
//! The `/balance-allowance` body shape is the documented one and has not been read live in this repo
//! (`crate::account::preflight`'s module doc): two shapes parse, anything else is an error in words
//! here (the preflight turns it into "unknown" and lets the order through; an operator asking is owed
//! the failure). The limit price of a resting order is the row's `price`.

use std::time::Duration;

use serde_json::Value;
use vike_bridge_core::json::get_str_borrowed;
use vike_bridge_core::side::sign_default_buy;
use vike_model::{
    MAX_VENUE_OPEN_ORDERS, MAX_VENUE_POSITIONS, VenueRead, VenueReadAllowance, VenueReadBalance,
    VenueReadKind, VenueReadOpenOrder, VenueReadPosition,
};
use vike_secrets::account_credentials::AccountCredentials;
use vike_venues::venue_urls::VenueUrls;

use crate::account::preflight::{BalanceAllowance, exchange_name, loggable};
use crate::config::PolymarketConfig;
use crate::execution::order::{SignatureType, exchange_contract};
use crate::execution::rest::get_signed_within;
use crate::reconcile::{normalize_order_status, number, rows, signature_type_for_account};
use crate::settlement::positions::{fetch_positions_page, parse_positions};
use crate::transport::sign::l1::ensure_l2;

/// How long ONE signed GET may take. An operator is waiting on it, so it is far shorter than the order
/// path's [`crate::transport::REST_TIMEOUT`]; the derive that precedes it keeps its own bound.
const READ_TIMEOUT: Duration = Duration::from_secs(12);

/// The rows asked of `/data/orders` in one read ([`MAX_VENUE_OPEN_ORDERS`]).
const ORDERS_LIMIT: usize = MAX_VENUE_OPEN_ORDERS;

/// The rows asked of the data-api's `/v2/positions` in one read ([`MAX_VENUE_POSITIONS`]).
const POSITIONS_LIMIT: usize = MAX_VENUE_POSITIONS;

/// The CLOB's "no further page" cursor (base64 of `-1`), the value its list endpoints end on.
const END_CURSOR: &str = "LTE=";

/// USDC and every Polymarket CTF token use 6 decimals: the wire's base units over this are whole units.
const DECIMALS: f64 = 1_000_000.0;

/// Epoch seconds of 5138-11-16: any `created_at` below this is in SECONDS, not milliseconds.
const SECONDS_CEILING: i64 = 100_000_000_000;

/// The answer to `kind` for the account whose credential fields are `creds`, on the CLOB and the
/// data-api rows among `urls` (`crate::config`'s `clob_base` and `data_api_base`, through the
/// declared egress). `Err` is the operator's sentence; a missing row is one.
pub(crate) fn venue_read(
    creds: &AccountCredentials<'_>,
    kind: VenueReadKind,
    urls: &VenueUrls,
) -> Result<VenueRead, String> {
    let loaded = crate::config::load_polymarket_credentials_from(creds)
        .ok_or_else(|| "the account holds no polymarket private key".to_string())?;
    let signer =
        vike_bridge_core::eip712::eth_address_from_private_key(loaded.private_key.expose())
            .map_err(|e| format!("the account's PRIVATE_KEY is not a usable key ({e})"))?;
    // ClobAuth is signed by, and names, the key's own EOA — not the funder `address` the loader put
    // there (a deposit wallet's), which the signed GETs below do not use.
    if kind == VenueReadKind::Positions {
        // Public data: no L2 key to derive and nothing to sign (module doc). The funder is the wallet
        // that holds the shares; a bare-key account holds them at its own EOA.
        let data_api = crate::config::data_api_base(urls).map_err(|e| e.to_string())?;
        return positions_with(data_api, &funder_of(loaded.address, signer));
    }
    let clob = crate::config::clob_base(urls).map_err(|e| e.to_string())?;
    let mut l2 = PolymarketConfig { address: signer, ..loaded };
    ensure_l2(&mut l2, clob, vike_model::time::clock::now_secs())
        .map_err(|e| format!("could not obtain the wallet's L2 API key: {e}"))?;
    read_with(clob, &l2, signature_type_for_account(creds), kind)
}

/// [`venue_read`] over a derived L2 trio on `base`: the seam the loopback tests drive.
pub(crate) fn read_with(
    base: &str,
    l2: &PolymarketConfig,
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

/// One page of the data-api's `GET /v2/positions` for `funder` on `data_api`: the seam the loopback tests
/// drive.
pub(crate) fn positions_with(data_api: &str, funder: &str) -> Result<VenueRead, String> {
    let body = fetch_positions_page(data_api, funder, POSITIONS_LIMIT, None, READ_TIMEOUT)?;
    let (positions, truncated) = positions_from(&body)?;
    Ok(VenueRead::Positions { positions, truncated })
}

/// One page of `GET /v2/positions` ([`parse_positions`] is the one parser) → the held positions and
/// whether the list is known to be incomplete: the venue's own cursor says so (`has_more` is exact, so a
/// full page proves nothing either way). A zero-size row is kept as the venue lists it.
pub(crate) fn positions_from(body: &Value) -> Result<(Vec<VenueReadPosition>, bool), String> {
    let page = parse_positions(body)?;
    let truncated = page.next_cursor.is_some() || page.positions.len() > POSITIONS_LIMIT;
    let positions = page
        .positions
        .into_iter()
        .take(POSITIONS_LIMIT)
        .map(|p| VenueReadPosition {
            symbol: p.asset,
            title: p.title,
            outcome: p.outcome,
            qty: p.size,
            average_price: p.average_price,
            price: p.current_price,
            value: p.current_value,
            redeemable: p.redeemable,
        })
        .collect();
    Ok((positions, truncated))
}

/// One L2-signed GET under [`READ_TIMEOUT`], its failure reduced to what is safe to repeat: the status
/// head, never the body (a proxy's error page can echo the request's `POLY_*` headers).
fn signed_get(base: &str, path: &str, query: &str, l2: &PolymarketConfig) -> Result<Value, String> {
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

fn allowance(spender: &str, base_units: u128) -> VenueReadAllowance {
    VenueReadAllowance {
        spender: spender.to_string(),
        name: contract_name(spender),
        amount: whole(base_units),
        unlimited: is_unlimited(base_units),
    }
}

/// `GET /balance-allowance` → the collateral balance and one approval per contract the venue lists. The
/// older shape (one `allowance` figure) is kept as a single row with no spender: the venue did not say
/// which contract it is for, and a row that named one would be a guess.
pub(crate) fn balance_from(body: &Value) -> Result<VenueReadBalance, String> {
    let snap = BalanceAllowance::parse(body)
        .ok_or_else(|| "polymarket's /balance-allowance answer is not an object".to_string())?;
    let balance = snap.balance.ok_or_else(|| {
        "polymarket's /balance-allowance answer carries no balance figure".to_string()
    })?;
    let mut allowances: Vec<VenueReadAllowance> =
        snap.per_spender.iter().map(|(spender, raw)| allowance(spender, *raw)).collect();
    if allowances.is_empty()
        && let Some(single) = snap.single
    {
        allowances.push(allowance("", single));
    }
    Ok(VenueReadBalance {
        asset: "pUSD".to_string(),
        balance: whole(balance),
        allowances,
        other_assets: Vec::new(),
    })
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
/// registry of the CLOB ids it placed, so no row names an order client id.
pub(crate) fn open_orders_from(body: &Value) -> Result<(Vec<VenueReadOpenOrder>, bool), String> {
    let list =
        rows(body).ok_or_else(|| "polymarket's /data/orders answer is not a list".to_string())?;
    let mut truncated = cursor_is_more(body) || list.len() > ORDERS_LIMIT;
    let mut orders = Vec::with_capacity(list.len().min(ORDERS_LIMIT));
    for row in list.iter().take(ORDERS_LIMIT) {
        let (original, matched) = (number(row, "original_size"), number(row, "size_matched"));
        let price = number(row, "price");
        orders.push(VenueReadOpenOrder {
            venue_order_id: get_str_borrowed(row, "id").to_string(),
            order_client_id: None,
            symbol: get_str_borrowed(row, "asset_id").to_string(),
            side: sign_default_buy(get_str_borrowed(row, "side")),
            price: (price > 0.0).then_some(price),
            // The row's `order_type` is the time in force (`GTC`, `GTD`), not a kind: no kind, no trigger.
            trigger_price: None,
            order_type: String::new(),
            qty: original,
            filled_qty: matched,
            status: normalize_order_status(get_str_borrowed(row, "status"), matched)
                .as_str()
                .to_string(),
            timestamp_ms: epoch_ms(number(row, "created_at")),
        });
    }
    // A list that is exactly the size asked for may have been cut by the venue's own page size.
    truncated |= orders.len() == ORDERS_LIMIT;
    Ok((orders, truncated))
}

#[path = "venue_read_tests.rs"]
#[cfg(test)]
mod venue_read_tests;
