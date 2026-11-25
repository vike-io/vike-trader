//! `venue_read` — an OPERATOR's read of a Hyperliquid account, asked of the venue now: its balance and
//! its open orders (`vike_model::VenueReadKind`; the gatewayhub's `TradeUp::VenueRead` asks it).
//!
//! Both are keyless `POST /info` reads against the account's MASTER address, on the network the
//! account's tier selects (`live` is mainnet, every lower tier testnet): the same two calls the
//! reconcile client makes (`frontendOpenOrders`, and the account value of `clearinghouseState` or the
//! spot USDC row of a unified account), plus the spot wallet (`spotClearinghouseState`), so the answer is
//! account-wide, not the mounted coin's. The key signs nothing here. Nothing is folded and nothing changes at
//! the venue.
//!
//! The master address is the account row's `ACCOUNT_ADDRESS` when it names one, else the venue's own
//! `userRole` answer for the key's address ([`crate::adapter::resolve_master`]); a venue that did not
//! confirm it is an error in words, never a read of the agent wallet's (empty) book. The reads ride the
//! per-IP weight window the newest adapter start opened ([`crate::adapter::adapter_window_transport`]),
//! as the leverage confirmation's do ([`crate::account::leverage`]).
//!
//! The balance is the PERP class's figure, read as the reconcile client reads it (`read_balance`): a margin
//! account's `marginSummary.accountValue`, or for a unified-account / portfolio-margin account the USDC that
//! collateralises its perps. Beside it is every coin of the SPOT wallet with money in it
//! (`VenueReadBalance::other_assets`, each `<COIN> (spot wallet)`), so a coin of the user's own that a spot
//! strategy could take for its position is seen. A unified account's USDC row IS the figure and is not listed
//! twice. The figure and the coins come off one `spotClearinghouseState` answer.
//!
//! An open order names its instrument by the USER name (decision 0290: `BTC` is `BTCUSDC.P`, `@107` is
//! `HYPEUSDC`), read off the core universe (`meta` + `spotMeta`, two more keyless reads) named by this
//! process's kept list, as the reconcile client names its rows; a coin no kept row names (a builder-dex coin, the
//! universe reading the core dexes only) keeps the venue's coin.

use serde_json::{Value, json};
use vike_bridge_core::json::json_number;
use vike_bridge_core::venue_adapter::AdapterInputs;
use vike_model::{
    AssetClass, VenueRead, VenueReadAssetBalance, VenueReadBalance, VenueReadKind,
    VenueReadOpenOrder, not_offered_words,
};
use vike_venues::venue_currency::HYPERLIQUID;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol};

use crate::account::leverage::unread;
use crate::config;
use crate::instrument::properties::HyperliquidInstruments;
use crate::instrument::symbology::Symbology;
use crate::reconcile::{Collateral, parse_abstraction, parse_orders, read_balance};
use crate::refusals::{HyperliquidAccountRefusal, HyperliquidVenueReadRefusal};
use vike_utility::errors::CarriedRefusal;

/// The answer to `kind` for the account in `inputs`. `Err` is the operator's sentence.
pub(crate) fn venue_read(
    inputs: &AdapterInputs<'_>,
    kind: VenueReadKind,
) -> Result<VenueRead, CarriedRefusal> {
    // Not read here (the node's positions are the engine's own, and the venue's `clearinghouseState` would
    // be a second source for the same book): said before the key is loaded or the master looked up, so
    // a refused kind costs the venue nothing.
    if kind == VenueReadKind::Positions {
        return Err(not_offered_words("hyperliquid", kind).into());
    }
    let hl = config::load(inputs.tier, &inputs.credentials)
        .ok_or_else(|| HyperliquidAccountRefusal::NoKey.carried())?;
    let rest =
        config::base_url(hl.network, inputs.settings.urls(), UrlChannel::Trade, UrlProtocol::Rest)
            .map_err(|e| e.to_string())?;
    let transport = crate::adapter::adapter_window_transport(hl.network, rest);
    let user = match hl.account_address {
        Some(address) => address,
        None => {
            // The key IS the account, or an agent wallet for another: only the venue knows, and an
            // unconfirmed answer would read a different (empty) book.
            let signer =
                crate::signing::Signer::from_private_key(hl.private_key.expose(), hl.network)
                    .map_err(|why| HyperliquidAccountRefusal::KeyUnusable { why }.carried())?;
            let master =
                crate::adapter::resolve_master(&transport, signer.address(), inputs.account);
            if !master.confirmed {
                return Err(HyperliquidAccountRefusal::MasterUnconfirmed.carried());
            }
            master.address
        }
    };
    match kind {
        VenueReadKind::Balance => {
            balance_read(&user, |req| transport.info(req).map(|v| v.to_string()).map_err(|e| e.msg))
                .map(VenueRead::Balance)
        }
        VenueReadKind::OpenOrders => {
            let body = transport
                .info(&json!({ "type": "frontendOpenOrders", "user": user }))
                .map_err(|e| unread("frontendOpenOrders")(e).carried())?;
            let universe = HyperliquidInstruments::load(&transport, false)
                .map_err(|e| unread("the listing that names the coins")(e).carried())?;
            let (orders, truncated) = open_orders_from(&body, universe.symbology())?;
            Ok(VenueRead::OpenOrders { orders, truncated })
        }
        VenueReadKind::Positions => Err(not_offered_words("hyperliquid", kind).into()),
    }
}

/// The balance answer (the module doc), its `/info` reads injected so a test drives it offline: `userAbstraction`,
/// `spotClearinghouseState`, and `clearinghouseState` for a perp-wallet account. Any failed read fails the answer.
fn balance_read(
    user: &str,
    info: impl Fn(&Value) -> Result<String, String>,
) -> Result<VenueReadBalance, CarriedRefusal> {
    let collateral =
        parse_abstraction(&info(&json!({ "type": "userAbstraction", "user": user }))?)?;
    let spot_request = json!({ "type": "spotClearinghouseState", "user": user });
    let spot = info(&spot_request)?;
    // The figure's own spot read is the answer already in hand, so the figure and the coins cannot disagree.
    let figure = read_balance(
        AssetClass::CryptoPerp,
        user,
        || Ok(collateral),
        |req| {
            if *req == spot_request { Ok(spot.clone()) } else { info(req) }
        },
    )?
    .ok_or_else(|| HyperliquidVenueReadRefusal::NoBalance.carried())?;
    Ok(VenueReadBalance {
        asset: HYPERLIQUID.settle_coins[0].to_string(),
        balance: figure,
        allowances: Vec::new(),
        other_assets: spot_coins(&spot, collateral)?,
    })
}

/// What follows a coin of the spot wallet in the answer, so its USDC never reads as the perp figure's.
const SPOT_LABEL: &str = "(spot wallet)";

/// `spotClearinghouseState` → every coin with money in it, in the venue's order and named as the venue names it
/// (an outcome token is its `+<encoding>`), at its `total` (which includes what open orders hold), labelled
/// [`SPOT_LABEL`]. A unified account's USDC row IS the figure ([`Collateral::SpotUsdc`]) and is left out. A row
/// with no `total` holds none (the crate's `parse_*` rule); a body with no `balances` array is malformed, never an
/// empty wallet.
fn spot_coins(
    body: &str,
    collateral: Collateral,
) -> Result<Vec<VenueReadAssetBalance>, CarriedRefusal> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let rows = v
        .get("balances")
        .and_then(Value::as_array)
        .ok_or_else(|| HyperliquidVenueReadRefusal::SpotBalancesNotAList.carried())?;
    let figure_coin = (collateral == Collateral::SpotUsdc).then_some(HYPERLIQUID.settle_coins[0]);
    Ok(rows
        .iter()
        .filter_map(|row| {
            let coin =
                row.get("coin").and_then(Value::as_str).filter(|c| Some(*c) != figure_coin)?;
            let balance = row.get("total").and_then(json_number).filter(|t| *t != 0.0)?;
            Some(VenueReadAssetBalance { asset: format!("{coin} {SPOT_LABEL}"), balance })
        })
        .collect())
}

/// `frontendOpenOrders` (a top-level array) → the open orders with the limit price each rests at (the
/// reconcile report's `price`: the row's `limitPx` on `Limit`, `Stop Limit` and `Take Profit Limit`
/// only, since a `Stop Market`'s or `Take Profit Market`'s `limitPx` is the slippage bound our own
/// `build_trigger_kind` put off the trigger, never a price the order named), a trigger kind's `triggerPx`
/// and the `orderType` in the platform's word, cut at [`vike_model::MAX_VENUE_OPEN_ORDERS`] with the cut said.
/// The rows go through the reconcile parser, one report per row in order, so the two reads cannot disagree
/// about a side, a size, a status, a price or a trigger. Each row's coin is named by `symbology`'s user name
/// (the module doc), a coin it does not name kept as the venue sent it.
pub(crate) fn open_orders_from(
    body: &Value,
    symbology: &Symbology,
) -> Result<(Vec<VenueReadOpenOrder>, bool), CarriedRefusal> {
    body.as_array().ok_or_else(|| HyperliquidVenueReadRefusal::NotAList.carried())?;
    let mut orders = parse_orders(&body.to_string())?;
    for order in &mut orders {
        if let Some(user) = symbology.symbol_for_coin(&order.symbol) {
            order.symbol = user.to_string();
        }
    }
    Ok(vike_model::open_orders_read(orders))
}

#[cfg(test)]
mod tests {
    use vike_model::MAX_VENUE_OPEN_ORDERS;

    use super::*;

    /// A `frontendOpenOrders` body in the venue's documented shape (strings for numbers, the order's
    /// `oid` as a number, the coin as the symbol; not a capture). The two trigger rows are what our
    /// `build_trigger_kind` sends for a sell stop at 3000 and a sell take-profit at 3300 with a limit:
    /// the `Stop Market`'s `limitPx` is its protective bound off the trigger (3000 · (1 − 5%) here),
    /// the `Take Profit Limit`'s the limit the order named.
    fn body() -> Value {
        json!([
            {"coin": "BTC", "side": "B", "limitPx": "60700.0", "sz": "0.0002", "origSz": "0.0002",
             "oid": 168136755771512u64, "timestamp": 1_761_279_876_000i64, "orderType": "Limit",
             "cloid": "0x00000000000000000000000000abcdef"},
            {"coin": "ETH", "side": "A", "limitPx": "2850.0", "sz": "1.0", "origSz": "1.0",
             "oid": 7u64, "timestamp": 1_761_279_877_000i64, "orderType": "Stop Market",
             "isTrigger": true, "triggerPx": "3000.0", "triggerCondition": "Price below 3000",
             "reduceOnly": true},
            {"coin": "ETH", "side": "A", "limitPx": "3290.0", "sz": "1.0", "origSz": "1.0",
             "oid": 8u64, "timestamp": 1_761_279_878_000i64, "orderType": "Take Profit Limit",
             "isTrigger": true, "triggerPx": "3300.0", "triggerCondition": "Price above 3300",
             "reduceOnly": true},
            {"coin": "SOL", "side": "A", "limitPx": "150.0", "sz": "1.0", "origSz": "3.0",
             "oid": 9u64, "timestamp": 1_761_279_879_000i64, "orderType": "Limit"},
        ])
    }

    /// The core universe a read names its coins by: the perpetuals `BTC`, `ETH` and `SOL` and the spot pair
    /// `@107` (`HYPE/USDC`), named by the crate's one test kept list (`crate::test_support::kept_rows`), which
    /// keeps no `SOL` row.
    fn universe() -> Symbology {
        let perp = |coin: &str| json!({"name": coin, "szDecimals": 2, "maxLeverage": 10});
        let meta = json!({"universe": [perp("BTC"), perp("ETH"), perp("SOL")]});
        let spot = json!({
            "tokens": [
                {"name": "USDC", "szDecimals": 8, "index": 0},
                {"name": "HYPE", "szDecimals": 2, "index": 150}
            ],
            "universe": [{"name": "@107", "tokens": [150, 0], "index": 107}]
        });
        Symbology::from_meta(&meta, &spot)
    }

    /// Each row keeps its limit price, side, fill progress, status word and time; the venue's own
    /// `cloid` rides as the client id. A `Stop Market`'s `limitPx` is our slippage bound and shows as
    /// no price; a `Take Profit Limit`'s is its limit. Each trigger row lists its `triggerPx` and its kind;
    /// a limit lists no trigger. Each coin is its user name (decision 0290), a coin no kept row names its own.
    #[test]
    fn an_open_order_keeps_its_price_side_progress_and_time() {
        let (orders, truncated) = open_orders_from(&body(), &universe()).unwrap();
        assert!(!truncated);
        let symbols: Vec<&str> = orders.iter().map(|o| o.symbol.as_str()).collect();
        assert_eq!(symbols, ["BTCUSDC.P", "ETHUSDC.P", "ETHUSDC.P", "SOL"]);
        let pair = json!([{"coin": "@107", "side": "B", "limitPx": "20.0", "sz": "1", "origSz": "1",
                           "oid": 1}]);
        let (spot, _) = open_orders_from(&pair, &universe()).unwrap();
        assert_eq!(spot[0].symbol, "HYPEUSDC", "the spot pair's coin is its user name");
        let (a, stop, take, partial) = (&orders[0], &orders[1], &orders[2], &orders[3]);
        assert_eq!((a.side, a.price), (1, Some(60700.0)));
        assert_eq!((a.qty, a.filled_qty, a.status.as_str()), (0.0002, 0.0, "ACCEPTED"));
        assert_eq!(a.venue_order_id, "168136755771512");
        assert_eq!(a.order_client_id.as_deref(), Some("0x00000000000000000000000000abcdef"));
        assert_eq!(a.timestamp_ms, 1_761_279_876_000);
        assert_eq!((a.trigger_price, a.order_type.as_str()), (None, "limit"));
        assert_eq!((stop.side, stop.price, stop.status.as_str()), (-1, None, "ACCEPTED"));
        assert_eq!((stop.trigger_price, stop.order_type.as_str()), (Some(3000.0), "stop"));
        assert_eq!(take.price, Some(3290.0));
        assert_eq!((take.trigger_price, take.order_type.as_str()), (Some(3300.0), "take_profit"));
        assert_eq!((partial.price, partial.filled_qty), (Some(150.0), 2.0));
        assert_eq!(partial.status, "PARTIALLY_FILLED");
    }

    #[test]
    fn an_empty_list_is_an_answer_and_a_non_list_is_an_error() {
        let (orders, truncated) = open_orders_from(&json!([]), &universe()).unwrap();
        assert!(orders.is_empty() && !truncated);
        assert_eq!(
            open_orders_from(&json!({"error": "x"}), &universe()).err(),
            Some(HyperliquidVenueReadRefusal::NotAList.carried())
        );
    }

    #[test]
    fn rows_past_the_cap_are_cut_and_said_to_be() {
        let rows: Vec<Value> = (0..=MAX_VENUE_OPEN_ORDERS)
            .map(|i| json!({"coin": "BTC", "side": "B", "limitPx": "1.0", "sz": "1", "origSz": "1", "oid": i}))
            .collect();
        let (orders, truncated) = open_orders_from(&Value::Array(rows), &universe()).unwrap();
        assert_eq!(orders.len(), MAX_VENUE_OPEN_ORDERS);
        assert!(truncated);
    }

    /// A spot wallet in the venue's documented shape (decimal strings; not a capture): USDC, a HYPE holding with
    /// part of it held by an open order, an emptied PURR row and a row with no `total`.
    const SPOT: &str = r#"{"balances":[
        {"coin":"USDC","token":0,"total":"120.5","hold":"0.0"},
        {"coin":"HYPE","token":150,"total":"3.25","hold":"1.0"},
        {"coin":"PURR","token":1,"total":"0.0","hold":"0.0"},
        {"coin":"UBTC","token":197}
    ]}"#;

    /// [`balance_read`] over scripted `/info` bodies keyed by request `type` (`abstraction` the
    /// `userAbstraction` word); a type with no body is a failed read. Returns the answer and the types asked.
    fn balance(
        abstraction: &str,
        bodies: &[(&str, &str)],
    ) -> (Result<VenueReadBalance, CarriedRefusal>, Vec<String>) {
        let sent = std::cell::RefCell::new(Vec::new());
        let word = format!("\"{abstraction}\"");
        let out = balance_read("0xmaster", |req| {
            let t = req["type"].as_str().unwrap().to_string();
            sent.borrow_mut().push(t.clone());
            let body = if t == "userAbstraction" {
                Some(word.as_str())
            } else {
                bodies.iter().find(|(k, _)| *k == t).map(|(_, b)| *b)
            };
            body.map(str::to_string).ok_or_else(|| format!("{t}: connection refused"))
        });
        (out, sent.into_inner())
    }

    fn asset(asset: &str, balance: f64) -> VenueReadAssetBalance {
        VenueReadAssetBalance { asset: asset.into(), balance }
    }

    /// **A perp-wallet account's balance lists every spot coin with money in it** (plan P4 #14): the figure is
    /// the perp account value, and beside it the spot wallet's USDC and HYPE (its `total`, the held part in),
    /// each labelled as the spot wallet's so the spot USDC never reads as the perp figure; an emptied row and a
    /// row with no `total` are not listed.
    #[test]
    fn a_perp_wallet_account_lists_every_spot_coin_beside_the_perp_figure() {
        let perp = r#"{"marginSummary":{"accountValue":"1000.0"}}"#;
        let (out, sent) =
            balance("default", &[("clearinghouseState", perp), ("spotClearinghouseState", SPOT)]);
        let read = out.unwrap();
        assert_eq!((read.asset.as_str(), read.balance), ("USDC", 1000.0));
        assert_eq!(
            read.other_assets,
            [asset("USDC (spot wallet)", 120.5), asset("HYPE (spot wallet)", 3.25)]
        );
        assert_eq!(sent, ["userAbstraction", "spotClearinghouseState", "clearinghouseState"]);
    }

    /// **A unified account's USDC row is the figure, listed once**: the figure is the spot USDC (the perp
    /// summary is not asked), the other coins stand beside it, and the spot wallet is read ONCE, so the figure
    /// and the coins come off the same answer.
    #[test]
    fn a_unified_accounts_usdc_is_the_figure_and_its_other_coins_stand_beside_it() {
        let (out, sent) = balance("unifiedAccount", &[("spotClearinghouseState", SPOT)]);
        let read = out.unwrap();
        assert_eq!((read.asset.as_str(), read.balance), ("USDC", 120.5));
        assert_eq!(read.other_assets, [asset("HYPE (spot wallet)", 3.25)]);
        assert_eq!(sent, ["userAbstraction", "spotClearinghouseState"]);
    }

    /// Fail-closed: a spot wallet that cannot be read, or answers no `balances` array, fails the whole answer,
    /// never a figure with the coins silently missing.
    #[test]
    fn an_unread_or_malformed_spot_wallet_fails_the_whole_answer() {
        let perp = r#"{"marginSummary":{"accountValue":"1000.0"}}"#;
        let (unread, _) = balance("default", &[("clearinghouseState", perp)]);
        assert_eq!(unread, Err("spotClearinghouseState: connection refused".into()));
        let (malformed, _) =
            balance("default", &[("clearinghouseState", perp), ("spotClearinghouseState", "{}")]);
        assert_eq!(malformed, Err(HyperliquidVenueReadRefusal::SpotBalancesNotAList.carried()));
        let text = malformed.expect_err("malformed").to_string();
        assert_eq!(text, "spotClearinghouseState: expected a `balances` array");
    }

    /// The adapter's refusal of a `kind` read for a demo account that holds no key.
    fn keyless_refusal(kind: VenueReadKind) -> CarriedRefusal {
        use crate::adapter::VenueAdapterHyperliquid;
        use vike_bridge_core::venue_adapter::VenueAdapter;
        use vike_bridge_core::venue_adapter_fixture::AdapterFixture;
        let fx = AdapterFixture::new(&[]);
        VenueAdapterHyperliquid
            .venue_read(&fx.inputs(vike_model::AccountTier::Demo), kind)
            .unwrap_err()
    }

    /// An account with no key is refused in words before any network.
    #[test]
    fn an_account_with_no_key_is_refused_before_any_network() {
        let why = keyless_refusal(VenueReadKind::OpenOrders);
        assert_eq!(why, HyperliquidAccountRefusal::NoKey.carried());
    }

    /// Positions are not this bridge's to read: the operator is told in the one sentence every bridge
    /// without a read says, and that is said BEFORE the key is looked at (this account holds none, so a
    /// refusal about the key would mean the kind got past the front door).
    #[test]
    fn a_positions_read_is_not_offered_before_anything_is_loaded() {
        let why = keyless_refusal(VenueReadKind::Positions);
        assert_eq!(why, not_offered_words("hyperliquid", VenueReadKind::Positions).into());
    }
}
