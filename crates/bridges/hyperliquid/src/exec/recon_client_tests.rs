use super::*;
use crate::base::config::Network;

// --- parse_orders (frontendOpenOrders: a bare array; rows all resting, no status field) ---

#[test]
fn parse_orders_open_and_partial_derive_status_from_fill_progress() {
    // frontendOpenOrders: top-level ARRAY; oid/timestamp are NUMBERS; px/sz are STRINGS.
    let body = r#"[
            {"coin":"BTC","side":"B","limitPx":"50000.0","sz":"0.1","origSz":"0.1","oid":91490942,"timestamp":1681247412573,"orderType":"Limit","cloid":"0xabc","reduceOnly":false},
            {"coin":"@107","side":"A","limitPx":"30.0","sz":"2.0","origSz":"5.0","oid":91490943,"timestamp":1681247412600,"orderType":"Limit"}
        ]"#;
    let r = parse_orders(body).unwrap();
    assert_eq!(r.len(), 2);

    let open = &r[0];
    assert_eq!(open.venue, "hyperliquid");
    assert_eq!(open.symbol, "BTC", "coin carried verbatim (== unified perp symbol)");
    assert_eq!(open.venue_order_id.as_str(), "91490942", "numeric oid stringified");
    assert_eq!(open.client_order_id.as_deref(), Some("0xabc"));
    assert_eq!(open.side, 1, "B -> +1");
    assert_eq!(open.order_type, vike_model::OrderType::Limit);
    assert_eq!(open.qty, 0.1);
    assert_eq!(open.filled_qty, 0.0);
    assert_eq!(open.avg_px, 0.0);
    assert_eq!(open.status, vike_model::OrderStatus::Accepted, "fully resting -> ACCEPTED");
    assert_eq!(open.ts, 1681247412573);

    let partial = &r[1];
    assert_eq!(partial.symbol, "@107", "spot coin string carried verbatim");
    assert_eq!(partial.client_order_id, None, "absent cloid -> None");
    assert_eq!(partial.side, -1, "A -> -1");
    assert_eq!(partial.qty, 5.0, "qty = origSz");
    assert_eq!(partial.filled_qty, 3.0, "filled = origSz - remaining sz");
    assert_eq!(
        partial.status,
        vike_model::OrderStatus::PartiallyFilled,
        "some filled -> PARTIALLY_FILLED"
    );
}

#[test]
fn parse_orders_uses_explicit_status_when_present() {
    // A status-bearing row (not the frontendOpenOrders norm) exercises normalize_order_status.
    let body = r#"[
            {"coin":"ETH","side":"B","sz":"1.0","origSz":"1.0","oid":1,"timestamp":1,"status":"marginCanceled"}
        ]"#;
    let r = parse_orders(body).unwrap();
    assert_eq!(r[0].status, vike_model::OrderStatus::Canceled, "*Canceled suffix -> CANCELED");
}

#[test]
fn normalize_order_status_maps_the_hl_vocabulary_by_suffix() {
    assert_eq!(normalize_order_status("open"), vike_model::OrderStatus::Accepted);
    assert_eq!(normalize_order_status("resting"), vike_model::OrderStatus::Accepted);
    assert_eq!(normalize_order_status("filled"), vike_model::OrderStatus::Filled);
    assert_eq!(normalize_order_status("triggered"), vike_model::OrderStatus::Triggered);
    assert_eq!(normalize_order_status("canceled"), vike_model::OrderStatus::Canceled);
    assert_eq!(normalize_order_status("reduceOnlyCanceled"), vike_model::OrderStatus::Canceled);
    assert_eq!(normalize_order_status("scheduledCancel"), vike_model::OrderStatus::Canceled);
    assert_eq!(normalize_order_status("rejected"), vike_model::OrderStatus::Rejected);
    assert_eq!(normalize_order_status("tickRejected"), vike_model::OrderStatus::Rejected);
    assert_eq!(
        normalize_order_status("somethingNew"),
        vike_model::OrderStatus::Accepted,
        "an unknown word is live (warned), never passed through"
    );
}

// --- parse_fills (userFills / userFillsByTime: a bare array) ---

#[test]
fn parse_fills_taker_buy_and_maker_sell_rebate() {
    let body = r#"[
            {"coin":"BTC","px":"50000.0","sz":"0.1","side":"B","oid":100,"cloid":"0xc1","tid":900,"fee":"2.5","feeToken":"USDC","crossed":true,"time":111},
            {"coin":"ETH","px":"3000.0","sz":"1.0","side":"A","oid":101,"tid":901,"fee":"-0.3","feeToken":"USDC","crossed":false,"time":222}
        ]"#;
    let r = parse_fills(body).unwrap();
    assert_eq!(r.len(), 2);

    let taker = &r[0];
    assert_eq!(taker.venue, "hyperliquid");
    assert_eq!(taker.symbol, "BTC");
    assert_eq!(taker.trade_id.as_str(), "900", "numeric tid stringified");
    assert_eq!(taker.venue_order_id.as_str(), "100");
    assert_eq!(taker.client_order_id.as_deref(), Some("0xc1"));
    assert_eq!(taker.side, 1, "B -> +1");
    assert_eq!(taker.last_qty, 0.1);
    assert_eq!(taker.last_px, 50000.0);
    assert_eq!(taker.commission, 2.5, "positive fee = cost");
    assert_eq!(taker.commission_asset, "USDC");
    assert_eq!(taker.liquidity_side, LiquiditySide::Taker, "crossed:true -> Taker");
    assert_eq!(taker.ts, 111);

    let maker = &r[1];
    assert_eq!(maker.trade_id.as_str(), "901");
    assert_eq!(maker.client_order_id, None, "absent cloid -> None");
    assert_eq!(maker.side, -1, "A -> -1");
    assert_eq!(maker.commission, -0.3, "negative fee = maker rebate, signed as-is");
    assert_eq!(maker.liquidity_side, LiquiditySide::Maker, "crossed:false -> Maker");
}

/// A `tid`-less reconcile fill row is SKIPPED, not admitted with an empty id. This gates the
/// `TradeId::new` handling in [`parse_fills`]: reverting it to `unwrap_or_default()` turns the
/// asserted length from 1 back to 3 and re-arms the hazard — an id-less `FillReport` can never
/// match `seen_trade_ids`, so it manufactures a `MissingFill` divergence, which is one of the
/// two kinds the `hybrid` policy AUTO-APPLIES (booking the fill a second time, unattended).
#[test]
fn parse_fills_skips_a_row_with_no_tid() {
    let body = r#"[
            {"coin":"BTC","px":"1.0","sz":"1.0","side":"B","oid":1,"fee":"0","feeToken":"USDC","crossed":true,"time":1},
            {"coin":"BTC","px":"1.0","sz":"1.0","side":"B","oid":2,"tid":"","fee":"0","feeToken":"USDC","crossed":true,"time":2},
            {"coin":"BTC","px":"1.0","sz":"1.0","side":"B","oid":3,"tid":903,"fee":"0","feeToken":"USDC","crossed":true,"time":3}
        ]"#;
    let r = parse_fills(body).unwrap();
    assert_eq!(r.len(), 1, "the absent-tid and empty-tid rows are both skipped");
    assert_eq!(r[0].trade_id, "903", "only the identifiable fill is reported");
}

// --- parse_positions (clearinghouseState.assetPositions[].position; szi signed) ---

#[test]
fn parse_positions_signed_szi_long_short_and_kept_flat_leg() {
    let body = r#"{
            "marginSummary":{"accountValue":"1234.5"},
            "time":1681222254710,
            "assetPositions":[
                {"type":"oneWay","position":{"coin":"BTC","szi":"0.5","entryPx":"29000.0","leverage":{"type":"cross","value":20}}},
                {"type":"oneWay","position":{"coin":"ETH","szi":"-2.0","entryPx":"1800.0","leverage":{"type":"isolated","value":10}}},
                {"type":"oneWay","position":{"coin":"SOL","szi":"0.0","entryPx":"0.0"}}
            ]
        }"#;
    let r = parse_positions(body).unwrap();
    assert_eq!(r.len(), 3, "every row kept, flat leg included");

    assert_eq!(r[0].venue, "hyperliquid");
    assert_eq!(r[0].symbol, "BTC");
    assert_eq!(r[0].position_side, PositionSide::Both, "HL one-way -> Both");
    assert_eq!(r[0].qty, 0.5, "positive szi -> long");
    assert_eq!(r[0].avg_px, 29000.0);
    assert_eq!(r[0].ts, 1681222254710, "top-level snapshot time stamps every row");
    assert_eq!(r[0].margin_mode, MarginMode::Cross, "leverage.type cross -> Cross");
    assert_eq!(r[0].isolated_margin, None);

    assert_eq!(r[1].qty, -2.0, "negative szi -> short (szi already signed)");
    assert_eq!(r[1].margin_mode, MarginMode::Isolated, "leverage.type isolated -> Isolated");
    assert_eq!(r[1].isolated_margin, None, "no verified HL per-position wallet field");
    assert_eq!(r[2].qty, 0.0, "flat leg survives with qty 0");
    assert_eq!(r[2].margin_mode, MarginMode::Cross, "absent leverage -> fail-safe Cross");
}

// --- parse_balance (per Product) ---

#[test]
fn parse_balance_perp_reads_account_value() {
    let body = r#"{"marginSummary":{"accountValue":"1234.5","totalMarginUsed":"10.0"},"withdrawable":"1200.0"}"#;
    assert_eq!(parse_balance(body, Product::Perp).unwrap(), Some(1234.5));
}

#[test]
fn parse_balance_spot_reads_usdc_total() {
    let body = r#"{"balances":[
            {"coin":"PURR","total":"100.0","hold":"0.0"},
            {"coin":"USDC","total":"555.25","hold":"5.0"}
        ]}"#;
    assert_eq!(parse_balance(body, Product::Spot).unwrap(), Some(555.25));
}

#[test]
fn parse_balance_none_when_field_or_usdc_absent() {
    assert_eq!(parse_balance(r#"{"marginSummary":{}}"#, Product::Perp).unwrap(), None);
    assert_eq!(
        parse_balance(r#"{"balances":[{"coin":"PURR","total":"1.0"}]}"#, Product::Spot).unwrap(),
        None
    );
}

// --- unified-account balance (the 2026-10-17 live finding: perp summary 0, spot USDC funded) ---
//
// Fixtures are the PUBLIC /info reads of the one address (0x8f0a…5bf5), taken 2026-10-17: the same
// address is `unifiedAccount` on both networks, perp side 0, spot USDC funded.

const PERP_ZERO: &str = r#"{"marginSummary":{"accountValue":"0.0","totalMarginUsed":"0.0"},"crossMarginSummary":{"accountValue":"0.0"},"withdrawable":"0.0","assetPositions":[]}"#;
const SPOT_TESTNET: &str =
    r#"{"balances":[{"coin":"USDC","token":0,"total":"978.764673","hold":"0.0"}]}"#;
const SPOT_MAINNET: &str = r#"{"balances":[{"coin":"USDC","token":0,"total":"8.8","hold":"0.0"}]}"#;

/// Drive [`read_balance`] with scripted `/info` bodies keyed by request `type`; a type with no entry
/// is a failed read. Returns the result and the request types it sent, in order.
fn drive(
    product: Product,
    abstraction: Result<Collateral, String>,
    bodies: &[(&str, &str)],
) -> (Result<Option<f64>, String>, Vec<String>) {
    let sent = std::cell::RefCell::new(Vec::new());
    let out = read_balance(
        product,
        "0xmaster",
        || abstraction,
        |req| {
            let t = req["type"].as_str().unwrap().to_string();
            sent.borrow_mut().push(t.clone());
            bodies
                .iter()
                .find(|(k, _)| *k == t)
                .map(|(_, b)| b.to_string())
                .ok_or_else(|| format!("{t}: connection refused"))
        },
    );
    (out, sent.into_inner())
}

#[test]
fn parse_abstraction_words() {
    for w in ["unifiedAccount", "portfolioMargin"] {
        assert_eq!(parse_abstraction(&format!("\"{w}\"")), Ok(Collateral::SpotUsdc), "{w}");
    }
    for w in ["default", "disabled", "dexAbstraction"] {
        assert_eq!(parse_abstraction(&format!("\"{w}\"")), Ok(Collateral::Perp), "{w}");
    }
    // An unknown word keeps today's perp-only read (warned), see `parse_abstraction`.
    assert_eq!(parse_abstraction("\"somethingNew\""), Ok(Collateral::Perp));
    // Not a string / not JSON: an error, never a guess.
    assert!(parse_abstraction("null").is_err());
    assert!(parse_abstraction("{}").is_err());
    assert!(parse_abstraction("nope").is_err());
}

#[test]
fn unified_account_balance_is_spot_usdc_on_testnet_and_mainnet() {
    let testnet = drive(
        Product::Perp,
        Ok(Collateral::SpotUsdc),
        &[("clearinghouseState", PERP_ZERO), ("spotClearinghouseState", SPOT_TESTNET)],
    );
    assert_eq!(testnet.0, Ok(Some(978.764673)), "testnet account 10: spot USDC, not perp 0");
    assert_eq!(testnet.1, ["spotClearinghouseState"], "the perp summary is not needed");

    let mainnet = drive(
        Product::Perp,
        Ok(Collateral::SpotUsdc),
        &[("clearinghouseState", PERP_ZERO), ("spotClearinghouseState", SPOT_MAINNET)],
    );
    assert_eq!(mainnet.0, Ok(Some(8.8)), "mainnet account 11");
}

#[test]
fn unified_account_with_no_usdc_row_is_a_real_zero_but_no_balances_array_is_an_error() {
    let empty = drive(
        Product::Perp,
        Ok(Collateral::SpotUsdc),
        &[("spotClearinghouseState", r#"{"balances":[{"coin":"PURR","total":"1.0"}]}"#)],
    );
    assert_eq!(empty.0, Ok(Some(0.0)));
    let malformed =
        drive(Product::Perp, Ok(Collateral::SpotUsdc), &[("spotClearinghouseState", "{}")]);
    assert!(malformed.0.is_err(), "a body that is not a balance list is never read as 0");
}

#[test]
fn classic_account_balance_stays_exactly_the_perp_account_value() {
    let body = r#"{"marginSummary":{"accountValue":"1234.5","totalMarginUsed":"10.0"},"withdrawable":"1200.0"}"#;
    // The spot body is deliberately different: a classic account must never look at it.
    let (out, sent) = drive(
        Product::Perp,
        Ok(Collateral::Perp),
        &[("clearinghouseState", body), ("spotClearinghouseState", SPOT_TESTNET)],
    );
    assert_eq!(out, Ok(Some(1234.5)));
    assert_eq!(sent, ["clearinghouseState"]);
}

#[test]
fn a_failed_read_fails_the_whole_balance_never_zero() {
    // The spot read fails on a unified account.
    let (out, _) =
        drive(Product::Perp, Ok(Collateral::SpotUsdc), &[("clearinghouseState", PERP_ZERO)]);
    assert!(out.unwrap_err().contains("spotClearinghouseState"));
    // The abstraction read fails: nothing else is read, and the answer is the error.
    let (out, sent) = drive(
        Product::Perp,
        Err("userAbstraction: connection refused".into()),
        &[("clearinghouseState", PERP_ZERO), ("spotClearinghouseState", SPOT_TESTNET)],
    );
    assert!(out.unwrap_err().contains("userAbstraction"));
    assert!(sent.is_empty(), "no fallback read after a failed abstraction read");
}

#[test]
fn spot_product_reads_spot_usdc_without_asking_the_abstraction() {
    let (out, sent) = drive(
        Product::Spot,
        Err("must not be asked".into()),
        &[("spotClearinghouseState", SPOT_MAINNET)],
    );
    assert_eq!(out, Ok(Some(8.8)));
    assert_eq!(sent, ["spotClearinghouseState"]);
}

#[test]
fn the_abstraction_is_cached_for_the_ttl_and_a_failure_is_not_cached() {
    let c = HyperliquidReconClient::new(
        HyperliquidTransport::new(Network::Testnet),
        "0xmaster",
        Product::Perp,
    );
    let t0 = Instant::now();
    assert!(c.collateral(t0, || Err("down".into())).is_err());
    assert_eq!(c.collateral(t0, || Ok(Collateral::SpotUsdc)), Ok(Collateral::SpotUsdc));
    let within = t0 + ABSTRACTION_TTL - Duration::from_secs(1);
    assert_eq!(c.collateral(within, || panic!("served from the cache")), Ok(Collateral::SpotUsdc));
    let after = t0 + ABSTRACTION_TTL;
    assert_eq!(c.collateral(after, || Ok(Collateral::Perp)), Ok(Collateral::Perp), "refetched");
}

// --- robustness: malformed bodies are errors, not panics ---

#[test]
fn malformed_bodies_are_errors_not_panics() {
    assert!(parse_orders("not json").is_err());
    assert!(parse_orders("{}").is_err(), "an object (not the expected array) is an error");
    assert!(parse_fills("null").is_err());
    assert!(parse_fills("42").is_err());
    assert!(parse_positions("\"oops\"").is_err());
    assert!(parse_positions("{}").is_err(), "no assetPositions key is an error");
    assert!(parse_balance("nope", Product::Perp).is_err());
}

// --- the client constructs offline and IS a ReconClient ---

#[test]
fn client_constructs_offline_and_impls_recon_client() {
    let t = HyperliquidTransport::new(Network::Testnet);
    let c = HyperliquidReconClient::new(t, "0xmaster", Product::Perp);
    // compile-time proof the trait is implemented (no network touched)
    let _dyn: &dyn ReconClient = &c;
}

/// The `recon_client` free-fn factory (ReconFactory seam, wave-2 task 6) is the SAME
/// construction, just already type-erased to `Box<dyn ReconClient>` — no network touched.
#[test]
fn recon_client_factory_constructs_offline() {
    let t = HyperliquidTransport::new(Network::Testnet);
    let _boxed: Box<dyn ReconClient> = recon_client(t, "0xmaster", Product::Perp);
}
