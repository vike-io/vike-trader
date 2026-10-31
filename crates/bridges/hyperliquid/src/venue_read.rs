//! `venue_read` — an OPERATOR's read of a Hyperliquid account, asked of the venue now: its balance and
//! its open orders (`vike_model::VenueReadKind`; the gatewayhub's `TradeUp::VenueRead` asks it).
//!
//! Both are keyless `POST /info` reads against the account's MASTER address, on the network the
//! account's tier selects (`live` is mainnet, every lower tier testnet): the same two calls the
//! reconcile client makes (`frontendOpenOrders`, and the account value of `clearinghouseState` or the
//! spot USDC row of a unified account), so the answer is account-wide, not the mounted coin's. The key
//! signs nothing here. Nothing is folded and nothing changes at the venue.
//!
//! The master address is the account row's `ACCOUNT_ADDRESS` when it names one, else the venue's own
//! `userRole` answer for the key's address ([`crate::adapter::resolve_master`]); a venue that did not
//! confirm it is an error in words, never a read of the agent wallet's (empty) book. The reads ride the
//! per-IP weight window the newest adapter start opened ([`crate::adapter::adapter_window_transport`]),
//! as the leverage confirmation's do ([`crate::leverage_cap`]).
//!
//! The balance is the PERP product's: a margin account's `marginSummary.accountValue`, or for a
//! unified-account / portfolio-margin account the USDC that collateralises its perps. It is not a
//! spot-only holding list.

use serde_json::{Value, json};
use vike_bridge_core::json::json_num;
use vike_bridge_core::venue_adapter::AdapterInputs;
use vike_exec::recon::ReconClient;
use vike_model::{
    MAX_VENUE_OPEN_ORDERS, VenueBalance, VenueOpenOrder, VenueRead, VenueReadKind,
    not_offered_words,
};

use crate::base::config::{self, Product};
use crate::exec::recon_client::{HyperliquidReconClient, parse_orders};

/// The answer to `kind` for the account in `inputs`. `Err` is the operator's sentence.
pub(crate) fn venue_read(
    inputs: &AdapterInputs<'_>,
    kind: VenueReadKind,
) -> Result<VenueRead, String> {
    // Not read here (the node's positions are the engine's own, and the venue's `clearinghouseState` would
    // be a second source for the same book): said before the key is loaded or the master looked up, so
    // a refused kind costs the venue nothing.
    if kind == VenueReadKind::Positions {
        return Err(not_offered_words("hyperliquid", kind));
    }
    let hl = config::load(inputs.tier, &inputs.credentials)
        .ok_or_else(|| "the account holds no hyperliquid key".to_string())?;
    let transport = crate::adapter::adapter_window_transport(hl.network);
    let user = match hl.account_address {
        Some(address) => address,
        None => {
            // The key IS the account, or an agent wallet for another: only the venue knows, and an
            // unconfirmed answer would read a different (empty) book.
            let signer = crate::signing::Signer::from_private_key(&hl.private_key, hl.network)
                .map_err(|e| format!("the account's key is unusable ({e})"))?;
            let master =
                crate::adapter::resolve_master(&transport, signer.address(), inputs.account);
            if !master.confirmed {
                return Err("the venue did not confirm which account this key trades for".into());
            }
            master.address
        }
    };
    match kind {
        VenueReadKind::Balance => {
            let figure = HyperliquidReconClient::new(transport, user, Product::Perp)
                .fetch_balance()?
                .ok_or_else(|| "hyperliquid states no balance for this account".to_string())?;
            Ok(VenueRead::Balance(VenueBalance {
                asset: "USDC".to_string(),
                balance: figure,
                allowances: Vec::new(),
            }))
        }
        VenueReadKind::OpenOrders => {
            let body = transport
                .info(&json!({ "type": "frontendOpenOrders", "user": user }))
                .map_err(|e| format!("frontendOpenOrders could not be read: {e}"))?;
            let (orders, truncated) = open_orders_from(&body)?;
            Ok(VenueRead::OpenOrders { orders, truncated })
        }
        VenueReadKind::Positions => Err(not_offered_words("hyperliquid", kind)),
    }
}

/// `frontendOpenOrders` (a top-level array) → the open orders with the limit price each rests at (the
/// row's `limitPx`; a stop or market row states none and reads `None`), cut at
/// [`MAX_VENUE_OPEN_ORDERS`] with the cut said. The rows go through the reconcile parser, one report per
/// row in order, so the two reads cannot disagree about a side, a size or a status.
pub(crate) fn open_orders_from(body: &Value) -> Result<(Vec<VenueOpenOrder>, bool), String> {
    let rows = body
        .as_array()
        .ok_or_else(|| "hyperliquid's frontendOpenOrders answer is not a list".to_string())?;
    let reports = parse_orders(&body.to_string())?;
    let truncated = reports.len() > MAX_VENUE_OPEN_ORDERS;
    let orders = reports
        .into_iter()
        .zip(rows)
        .take(MAX_VENUE_OPEN_ORDERS)
        .map(|(report, row)| VenueOpenOrder {
            venue_order_id: report.venue_order_id.to_string(),
            client_order_id: report.client_order_id,
            symbol: report.symbol,
            side: report.side,
            price: row.get("limitPx").and_then(json_num).filter(|p| *p > 0.0),
            qty: report.qty,
            filled_qty: report.filled_qty,
            status: report.status.as_str().to_string(),
            ts_ms: report.ts,
        })
        .collect();
    Ok((orders, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `frontendOpenOrders` body as the venue words it (recorded shape: strings for numbers, the
    /// order's `oid` as a number, the coin as the symbol).
    fn body() -> Value {
        json!([
            {"coin": "BTC", "side": "B", "limitPx": "60700.0", "sz": "0.0002", "origSz": "0.0002",
             "oid": 168136755771512u64, "timestamp": 1_761_279_876_000i64, "orderType": "Limit",
             "cloid": "0x00000000000000000000000000abcdef"},
            {"coin": "ETH", "side": "A", "limitPx": "0.0", "sz": "1.0", "origSz": "3.0",
             "oid": 7u64, "timestamp": 1_761_279_877_000i64, "orderType": "Stop Market"},
        ])
    }

    /// Each row keeps its limit price (a stop's `0.0` is "none"), side, fill progress, status word and
    /// time; the venue's own `cloid` rides as the client id.
    #[test]
    fn an_open_order_keeps_its_price_side_progress_and_time() {
        let (orders, truncated) = open_orders_from(&body()).unwrap();
        assert!(!truncated);
        let (a, b) = (&orders[0], &orders[1]);
        assert_eq!((a.symbol.as_str(), a.side, a.price), ("BTC", 1, Some(60700.0)));
        assert_eq!((a.qty, a.filled_qty, a.status.as_str()), (0.0002, 0.0, "ACCEPTED"));
        assert_eq!(a.venue_order_id, "168136755771512");
        assert_eq!(a.client_order_id.as_deref(), Some("0x00000000000000000000000000abcdef"));
        assert_eq!(a.ts_ms, 1_761_279_876_000);
        assert_eq!((b.side, b.price, b.filled_qty), (-1, None, 2.0));
        assert_eq!(b.status, "PARTIALLY_FILLED");
    }

    #[test]
    fn an_empty_list_is_an_answer_and_a_non_list_is_an_error() {
        let (orders, truncated) = open_orders_from(&json!([])).unwrap();
        assert!(orders.is_empty() && !truncated);
        assert!(open_orders_from(&json!({"error": "x"})).is_err());
    }

    #[test]
    fn rows_past_the_cap_are_cut_and_said_to_be() {
        let rows: Vec<Value> = (0..=MAX_VENUE_OPEN_ORDERS)
            .map(|i| json!({"coin": "BTC", "side": "B", "limitPx": "1.0", "sz": "1", "origSz": "1", "oid": i}))
            .collect();
        let (orders, truncated) = open_orders_from(&Value::Array(rows)).unwrap();
        assert_eq!(orders.len(), MAX_VENUE_OPEN_ORDERS);
        assert!(truncated);
    }

    /// An account with no key is refused in words before any network.
    #[test]
    fn an_account_with_no_key_is_refused_before_any_network() {
        use crate::adapter::HyperliquidVenueAdapter;
        use vike_bridge_core::venue_adapter::VenueAdapter;
        use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
        use vike_model::AccountTier;
        let fx = AdapterFixture::new(&[]);
        let why = HyperliquidVenueAdapter
            .venue_read(&fx.inputs(AccountTier::Demo), VenueReadKind::OpenOrders)
            .unwrap_err();
        assert!(why.contains("holds no hyperliquid key"), "{why}");
    }

    /// Positions are not this bridge's to read: the operator is told in the one sentence every bridge
    /// without a read says, and that is said BEFORE the key is looked at (this account holds none, so a
    /// refusal about the key would mean the kind got past the front door).
    #[test]
    fn a_positions_read_is_not_offered_before_anything_is_loaded() {
        use crate::adapter::HyperliquidVenueAdapter;
        use vike_bridge_core::venue_adapter::VenueAdapter;
        use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
        use vike_model::AccountTier;
        let fx = AdapterFixture::new(&[]);
        let why = HyperliquidVenueAdapter
            .venue_read(&fx.inputs(AccountTier::Demo), VenueReadKind::Positions)
            .unwrap_err();
        assert_eq!(why, not_offered_words("hyperliquid", VenueReadKind::Positions));
    }
}
