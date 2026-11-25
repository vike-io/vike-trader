use super::*;
use crate::execution::event_mapper::cloid_from_order_client_id;
use crate::instrument::properties::HyperliquidInstruments;
use crate::refusals::HyperliquidCatalogRefusal;
use std::sync::Arc;
use vike_execution::recon::{
    ReconcileDivergence, ReconcileMode, ReconcilePolicy, RestoreScope, RestoredOrderRef, diff,
    plan_restore, resolve,
};
use vike_model::events::Event;
use vike_model::{ReconcileDivergenceKind, VenueNetwork};

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
    assert_eq!(open.symbol, "BTC", "coin carried verbatim (the client names it)");
    assert_eq!(open.venue_order_id.as_str(), "91490942", "numeric oid stringified");
    assert_eq!(open.order_client_id.as_deref(), Some("0xabc"));
    assert_eq!(open.side, 1, "B -> +1");
    assert_eq!(open.order_type, vike_model::OrderType::Limit);
    assert_eq!(open.qty, 0.1);
    assert_eq!(open.filled_qty, 0.0);
    assert_eq!(open.average_price, 0.0);
    assert_eq!(open.status, vike_model::OrderStatus::Accepted, "fully resting -> ACCEPTED");
    assert_eq!(open.timestamp, 1681247412573);

    let partial = &r[1];
    assert_eq!(partial.symbol, "@107", "spot coin string carried verbatim");
    assert_eq!(partial.order_client_id, None, "absent cloid -> None");
    assert_eq!(partial.side, -1, "A -> -1");
    assert_eq!(partial.qty, 5.0, "qty = origSz");
    assert_eq!(partial.filled_qty, 3.0, "filled = origSz - remaining sz");
    assert_eq!(
        partial.status,
        vike_model::OrderStatus::PartiallyFilled,
        "some filled -> PARTIALLY_FILLED"
    );
    assert_eq!((open.price, open.trigger_price), (Some(50000.0), None), "a limit's `limitPx`");
    assert_eq!(partial.price, Some(30.0));
}

/// The terms of the trigger words: `triggerPx` the level, `limitPx` the limit of a `Stop Limit` or a
/// `Take Profit Limit` only (a `Stop Market`'s or `Take Profit Market`'s `limitPx` is the slippage bound
/// `order_wire.rs`'s `build_trigger_kind` puts off the trigger). ⚠ Not a capture:
/// `frontendOpenOrders`' documented fields with the trigger words.
#[test]
fn parse_orders_reads_a_trigger_rows_terms() {
    let row = |ty: &str| {
        format!(
            r#"{{"coin":"BTC","side":"A","limitPx":"57000.0","sz":"0.1","origSz":"0.1","oid":5,"timestamp":1,
                "orderType":"{ty}","isTrigger":true,"triggerPx":"58000.0","triggerCondition":"Price below 58000","cloid":"0xabc"}}"#
        )
    };
    let body = format!(
        "[{},{},{},{}]",
        row("Stop Market"),
        row("Stop Limit"),
        row("Take Profit Market"),
        row("Take Profit Limit")
    );
    let terms: Vec<_> = parse_orders(&body)
        .unwrap()
        .iter()
        .map(|r| (r.order_type.clone(), r.price, r.trigger_price, r.trail, r.trail_percent))
        .collect();
    use vike_model::OrderType::{Stop, StopLimit, TakeProfit};
    assert_eq!(
        terms,
        [
            (Stop, None, Some(58000.0), None, None),
            (StopLimit, Some(57000.0), Some(58000.0), None, None),
            (TakeProfit, None, Some(58000.0), None, None),
            (TakeProfit, Some(57000.0), Some(58000.0), None, None),
        ]
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
    assert_eq!(taker.order_client_id.as_deref(), Some("0xc1"));
    assert_eq!(taker.side, 1, "B -> +1");
    assert_eq!(taker.last_qty, 0.1);
    assert_eq!(taker.last_price, 50000.0);
    assert_eq!(taker.commission, 2.5, "positive fee = cost");
    assert_eq!(taker.commission_asset, "USDC");
    assert_eq!(taker.liquidity_side, LiquiditySide::Taker, "crossed:true -> Taker");
    assert_eq!(taker.timestamp, 111);

    let maker = &r[1];
    assert_eq!(maker.trade_id.as_str(), "901");
    assert_eq!(maker.order_client_id, None, "absent cloid -> None");
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
    assert_eq!(r[0].average_price, 29000.0);
    assert_eq!(r[0].timestamp, 1681222254710, "top-level snapshot time stamps every row");
    assert_eq!(r[0].margin_mode, MarginMode::Cross, "leverage.type cross -> Cross");
    assert_eq!(r[0].isolated_margin, None);

    assert_eq!(r[1].qty, -2.0, "negative szi -> short (szi already signed)");
    assert_eq!(r[1].margin_mode, MarginMode::Isolated, "leverage.type isolated -> Isolated");
    assert_eq!(r[1].isolated_margin, None, "no verified HL per-position wallet field");
    assert_eq!(r[2].qty, 0.0, "flat leg survives with qty 0");
    assert_eq!(r[2].margin_mode, MarginMode::Cross, "absent leverage -> fail-safe Cross");
}

// --- parse_balance (per class) ---

#[test]
fn parse_balance_perp_reads_account_value() {
    let body = r#"{"marginSummary":{"accountValue":"1234.5","totalMarginUsed":"10.0"},"withdrawable":"1200.0"}"#;
    assert_eq!(parse_balance(body, AssetClass::CryptoPerp).unwrap(), Some(1234.5));
}

#[test]
fn parse_balance_spot_reads_usdc_total() {
    let body = r#"{"balances":[
            {"coin":"PURR","total":"100.0","hold":"0.0"},
            {"coin":"USDC","total":"555.25","hold":"5.0"}
        ]}"#;
    assert_eq!(parse_balance(body, AssetClass::CryptoSpot).unwrap(), Some(555.25));
}

#[test]
fn parse_balance_none_when_field_or_usdc_absent() {
    assert_eq!(parse_balance(r#"{"marginSummary":{}}"#, AssetClass::CryptoPerp).unwrap(), None);
    assert_eq!(
        parse_balance(r#"{"balances":[{"coin":"PURR","total":"1.0"}]}"#, AssetClass::CryptoSpot)
            .unwrap(),
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
    class: AssetClass,
    abstraction: Result<Collateral, String>,
    bodies: &[(&str, &str)],
) -> (Result<Option<f64>, String>, Vec<String>) {
    let sent = std::cell::RefCell::new(Vec::new());
    let out = read_balance(
        class,
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

/// **A class hyperliquid lists none of is refused by name, never read as a perp or a spot pair**
/// (`crate::config`'s `unlisted_class`): the balance read asks nothing, and the pure parsers say the same.
#[test]
fn a_class_this_venue_lists_none_of_is_refused_by_name() {
    let (out, sent) = drive(AssetClass::Equity, Err("must not be asked".into()), &[]);
    let why = out.expect_err("an equity is no hyperliquid class");
    let refusal = HyperliquidCatalogRefusal::UnlistedClass { class: AssetClass::Equity };
    assert_eq!(why, refusal.to_string());
    assert!(sent.is_empty(), "nothing is asked: {sent:?}");
    assert_eq!(parse_balance("{}", AssetClass::Equity), Err(why.clone()));
    assert_eq!(parse_user_fees("{}", AssetClass::Equity), Err(why));
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
        AssetClass::CryptoPerp,
        Ok(Collateral::SpotUsdc),
        &[("clearinghouseState", PERP_ZERO), ("spotClearinghouseState", SPOT_TESTNET)],
    );
    assert_eq!(testnet.0, Ok(Some(978.764673)), "testnet account 10: spot USDC, not perp 0");
    assert_eq!(testnet.1, ["spotClearinghouseState"], "the perp summary is not needed");

    let mainnet = drive(
        AssetClass::CryptoPerp,
        Ok(Collateral::SpotUsdc),
        &[("clearinghouseState", PERP_ZERO), ("spotClearinghouseState", SPOT_MAINNET)],
    );
    assert_eq!(mainnet.0, Ok(Some(8.8)), "mainnet account 11");
}

#[test]
fn unified_account_with_no_usdc_row_is_a_real_zero_but_no_balances_array_is_an_error() {
    let empty = drive(
        AssetClass::CryptoPerp,
        Ok(Collateral::SpotUsdc),
        &[("spotClearinghouseState", r#"{"balances":[{"coin":"PURR","total":"1.0"}]}"#)],
    );
    assert_eq!(empty.0, Ok(Some(0.0)));
    let malformed = drive(
        AssetClass::CryptoPerp,
        Ok(Collateral::SpotUsdc),
        &[("spotClearinghouseState", "{}")],
    );
    assert!(malformed.0.is_err(), "a body that is not a balance list is never read as 0");
}

#[test]
fn classic_account_balance_stays_exactly_the_perp_account_value() {
    let body = r#"{"marginSummary":{"accountValue":"1234.5","totalMarginUsed":"10.0"},"withdrawable":"1200.0"}"#;
    // The spot body is deliberately different: a classic account must never look at it.
    let (out, sent) = drive(
        AssetClass::CryptoPerp,
        Ok(Collateral::Perp),
        &[("clearinghouseState", body), ("spotClearinghouseState", SPOT_TESTNET)],
    );
    assert_eq!(out, Ok(Some(1234.5)));
    assert_eq!(sent, ["clearinghouseState"]);
}

#[test]
fn a_failed_read_fails_the_whole_balance_never_zero() {
    // The spot read fails on a unified account.
    let (out, _) = drive(
        AssetClass::CryptoPerp,
        Ok(Collateral::SpotUsdc),
        &[("clearinghouseState", PERP_ZERO)],
    );
    assert!(out.unwrap_err().contains("spotClearinghouseState"));
    // The abstraction read fails: nothing else is read, and the answer is the error.
    let (out, sent) = drive(
        AssetClass::CryptoPerp,
        Err("userAbstraction: connection refused".into()),
        &[("clearinghouseState", PERP_ZERO), ("spotClearinghouseState", SPOT_TESTNET)],
    );
    assert!(out.unwrap_err().contains("userAbstraction"));
    assert!(sent.is_empty(), "no fallback read after a failed abstraction read");
}

#[test]
fn spot_product_reads_spot_usdc_without_asking_the_abstraction() {
    let (out, sent) = drive(
        AssetClass::CryptoSpot,
        Err("must not be asked".into()),
        &[("spotClearinghouseState", SPOT_MAINNET)],
    );
    assert_eq!(out, Ok(Some(8.8)));
    assert_eq!(sent, ["spotClearinghouseState"]);
}

#[test]
fn the_abstraction_is_cached_for_the_ttl_and_a_failure_is_not_cached() {
    let c = ReconcileClientHyperliquid::new(
        crate::test_support::seed_transport(VenueNetwork::Testnet),
        "0xmaster",
        AssetClass::CryptoPerp,
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
    assert!(parse_balance("nope", AssetClass::CryptoPerp).is_err());
}

// --- the client constructs offline and IS a ReconcileClient ---

#[test]
fn client_constructs_offline_and_impls_reconcile_client() {
    let t = crate::test_support::seed_transport(VenueNetwork::Testnet);
    let c = ReconcileClientHyperliquid::new(t, "0xmaster", AssetClass::CryptoPerp);
    // compile-time proof the trait is implemented (no network touched)
    let _dyn: &dyn ReconcileClient = &c;
}

/// The `recon_client` free-fn factory (ReconFactory seam, wave-2 task 6) is the SAME
/// construction, just already type-erased to `Box<dyn ReconcileClient>` — no network touched.
#[test]
fn reconcile_client_factory_constructs_offline() {
    let t = crate::test_support::seed_transport(VenueNetwork::Testnet);
    let _boxed: Box<dyn ReconcileClient> = recon_client(t, "0xmaster", AssetClass::CryptoPerp);
}

// --- decision 0218: spot pairs and builder-dex coins in the reconcile reads ---

/// The account's universe (decision 0290's user names): the perps `BTCUSDC.P` (coin `BTC`) and `KPEPEUSDC.P`, the spot
/// pair `HYPEUSDC` (coin `@107`), and the builder dexes `xyz` (`XYZ:TSLAUSDC.P`, `XYZ:AAPLUSDC.P`) and `flx`
/// (`FLX:GOLDUSDC.P`), both collateralised in USDC.
fn spot_universe() -> SharedInstruments {
    let fetch = |body: &Value| -> Result<Value, VenueApiError> {
        Ok(match (body["type"].as_str(), body.get("dex").and_then(Value::as_str)) {
            (Some("meta"), None) => json!({"universe":[
                {"name":"BTC","szDecimals":5,"maxLeverage":40},
                {"name":"kPEPE","szDecimals":0,"maxLeverage":10}
            ]}),
            (Some("spotMeta"), _) => json!({
                "tokens":[
                    {"name":"USDC","szDecimals":8,"index":0},
                    {"name":"HYPE","szDecimals":2,"index":150}
                ],
                "universe":[{"name":"@107","tokens":[150,0],"index":107}]
            }),
            (Some("perpDexs"), _) => json!([null, {"name":"xyz"}, {"name":"flx"}]),
            (Some("meta"), Some(dex)) => {
                let coins: &[&str] = if dex == "xyz" { &["TSLA", "AAPL"] } else { &["GOLD"] };
                let rows: Vec<Value> = coins
                    .iter()
                    .map(|c| json!({"name": format!("{dex}:{c}"), "szDecimals": 2, "maxLeverage": 10}))
                    .collect();
                json!({"universe": rows, "collateralToken": 0})
            }
            _ => json!({}),
        })
    };
    let dexes = ["xyz".to_string(), "flx".to_string()];
    let universe =
        HyperliquidInstruments::load_with_dexes(fetch, None, false, &dexes).expect("loads");
    SharedInstruments::new(Arc::new(universe))
}

fn hype_pair(universe: &SharedInstruments) -> InstrumentRef {
    universe.current().symbology().by_symbol("HYPEUSDC").expect("HYPEUSDC").clone()
}

/// A holding is the BASE token's balance row: the held `total` is the quantity, the row books `Cash`, and a pair
/// the account holds nothing (or dust) of is a FLAT row that is still present. The price is `0.0`, NOT `entryNtl /
/// total`: the wallet's blended cost basis is not the strategy's (the spot rule, owner 2026-11-11), and a priced
/// row was what let a confirm or `hybrid` adopt the user's own coins at it.
#[test]
fn an_admitted_spot_pair_reports_its_balance_row_as_a_cash_position() {
    let universe = spot_universe();
    let pair = hype_pair(&universe);
    let body = r#"{"balances":[
        {"coin":"USDC","token":0,"total":"500.0","hold":"0.0","entryNtl":"0.0"},
        {"coin":"HYPE","token":150,"total":"10.0","hold":"2.0","entryNtl":"370.0"}
    ]}"#;
    let rows = parse_spot_positions(body, std::slice::from_ref(&pair), 7).expect("parses");
    assert_eq!(rows.len(), 1);
    let p = &rows[0];
    assert_eq!(
        (p.symbol.as_str(), p.qty, p.average_price, p.timestamp),
        ("HYPEUSDC", 10.0, 0.0, 7)
    );
    assert_eq!(p.margin_mode, MarginMode::Cash);
    assert_eq!(p.position_side, PositionSide::Both);

    // None held, or less than one size step (HYPE's step is 0.01): a flat row, still present.
    let dust = r#"{"balances":[{"coin":"HYPE","total":"0.00000004","entryNtl":"0.0000036"}]}"#;
    for held in [r#"{"balances":[]}"#, dust] {
        let rows = parse_spot_positions(held, std::slice::from_ref(&pair), 7).expect("parses");
        assert_eq!((rows.len(), rows[0].qty, rows[0].average_price), (1, 0.0, 0.0), "{held}");
    }
    // A malformed body is an error, never "flat".
    assert!(parse_spot_positions("{}", std::slice::from_ref(&pair), 7).is_err());
    assert!(parse_spot_positions("nope", std::slice::from_ref(&pair), 7).is_err());
}

/// The client learns what its reads must span from the engine's admissions (user names, decision 0290), read
/// through the universe: a builder-dex coin adds its dex (once, in the VENUE's spelling `xyz`), a spot pair adds its
/// holding, a core perp adds nothing, a coin the universe does not hold adds nothing yet, and the list is capped by
/// the engine's own cap.
#[test]
fn the_client_reads_what_the_engine_was_admitted_and_only_that() {
    let universe = spot_universe();
    let client = ReconcileClientHyperliquid::new(
        crate::test_support::seed_transport(VenueNetwork::Testnet),
        "0xmaster",
        AssetClass::CryptoPerp,
    )
    .with_instruments(universe.clone());
    assert!(client.admitted_dexes().is_empty() && client.spot_pairs().is_empty());
    for symbol in [
        "KPEPEUSDC.P",
        "BTCUSDC.P",
        "XYZ:TSLAUSDC.P",
        "XYZ:AAPLUSDC.P",
        "FLX:GOLDUSDC.P",
        "HYPEUSDC",
        "HYPEUSDC",
        "NEWUSDC",
        "ABC:NEWUSDC.P",
    ] {
        client.admit_symbol(symbol);
    }
    assert_eq!(client.admitted_dexes(), ["xyz", "flx"], "one read per dex, in admission order");
    let pairs: Vec<String> = client.spot_pairs().into_iter().map(|i| i.symbol).collect();
    assert_eq!(pairs, ["HYPEUSDC"], "a pair the universe does not list is not read");
    // The coin of a row is named by the engine's user name once the universe is given, verbatim without it.
    assert_eq!(client.symbol_of("@107").map(|i| i.symbol).as_deref(), Some("HYPEUSDC"));
    assert!(client.symbol_of("BTC").is_some_and(|i| i.symbol == "BTCUSDC.P"));
    assert!(client.symbol_of("@999").is_none());
    let bare = ReconcileClientHyperliquid::new(
        crate::test_support::seed_transport(VenueNetwork::Testnet),
        "0xmaster",
        AssetClass::CryptoPerp,
    );
    assert!(bare.symbol_of("@107").is_none() && bare.spot_pairs().is_empty());

    for i in 0..(vike_execution::MAX_EXTRA_SYMBOLS + 3) {
        client.admit_symbol(&format!("D{i}:COINUSDC.P"));
    }
    assert!(client.admitted.symbols().len() <= vike_execution::MAX_EXTRA_SYMBOLS, "bounded");
}

/// **A symbol the engine gives back frees its place** (owner ruling F-3, decision 0205): with the list full, the
/// engine's next admission after a release is read. Without the forget the released place stayed taken, so the
/// 17th distinct dex coin or spot pair of a mount was never read and a restart could retire its resting orders.
#[test]
fn a_symbol_the_engine_gives_back_frees_its_place_for_the_next_admission() {
    let client = ReconcileClientHyperliquid::new(
        crate::test_support::seed_transport(VenueNetwork::Testnet),
        "0xmaster",
        AssetClass::CryptoPerp,
    );
    for i in 0..vike_execution::MAX_EXTRA_SYMBOLS {
        client.admit_symbol(&format!("D{i}:COINUSDC.P"));
    }
    let reads = |symbol: &str| client.admitted.symbols().iter().any(|s| s == symbol);
    client.admit_symbol("LATE:COINUSDC.P");
    assert!(!reads("LATE:COINUSDC.P"), "the list is full");

    client.forget_symbol("D0:COINUSDC.P");
    client.forget_symbol("d1:coinusdc.p"); // the engine's own spelling only, as it admitted it
    client.forget_symbol("NEVER:COINUSDC.P"); // never held: a no-op
    client.admit_symbol("LATE:COINUSDC.P");
    assert!(reads("LATE:COINUSDC.P"), "the freed place is read: {:?}", client.admitted.symbols());
    assert!(!reads("D0:COINUSDC.P") && reads("D1:COINUSDC.P"), "{:?}", client.admitted.symbols());
    assert_eq!(client.admitted.symbols().len(), vike_execution::MAX_EXTRA_SYMBOLS);
}

/// A spot-MOUNTED account's own pair is read like an admitted one (the spot rule): without it the mounted pair's
/// holding was never reported, so a wallet holding less than the strategy bought went unseen, and on an account that
/// also holds a perpetual the engine's own holding read as an `OrphanLocalPosition`. A perp mount adds nothing, and a
/// mounted pair the engine also admits is read once.
#[test]
fn a_spot_mounted_account_reads_its_own_pair() {
    let universe = spot_universe();
    let client = |mounted: &str| {
        ReconcileClientHyperliquid::new(
            crate::test_support::seed_transport(VenueNetwork::Testnet),
            "0xmaster",
            AssetClass::CryptoSpot,
        )
        .with_instruments(universe.clone())
        .with_mounted_symbol(mounted)
    };
    let pairs = |c: &ReconcileClientHyperliquid| -> Vec<String> {
        c.spot_pairs().into_iter().map(|i| i.symbol).collect()
    };
    let spot = client("HYPEUSDC");
    assert_eq!(pairs(&spot), ["HYPEUSDC"]);
    spot.admit_symbol("HYPEUSDC");
    assert_eq!(pairs(&spot), ["HYPEUSDC"], "read once");
    assert!(pairs(&client("BTCUSDC.P")).is_empty(), "a perp mount reads no spot pair");
}

// --- a row of our own order, read back through the account's `cloid -> order_client_id` map ---
//
// The bodies are the venue's documented rows, every field kept (nothing on disk is a captured frame:
// `crates/bridges/hyperliquid/CLAUDE.md`). `frontendOpenOrders` carrying `cloid` is live-proven by the
// testnet pass below and `hyperliquid_restore_smoke.rs`; a `cloid` on a REST `userFills` row is the shape
// `parse_fills` and the stream's mapper read, and no recorded row on the CI box confirms it.

/// The one resting order of the testnet pass of 2026-11-07 01:04:04 UTC, which held BOTH
/// `UnknownOrder order_client_id=Some("0xc776…")` and `OrphanLocalOrder ef7670ad0` (the the CI box tradehub journal).
const OURS: &str = "ef7670ad0";
const OURS_CLOID: &str = "0xc7761cce9090015a452bc9d6efa28e37";
const FOREIGN_CLOID: &str = "0x0123456789abcdef0123456789abcdef";

fn open_orders_body(coin: &str, cloid: &str) -> String {
    format!(
        r#"[{{"coin":"{coin}","isPositionTpsl":false,"isTrigger":false,"limitPx":"38123.0","oid":42177013,
            "orderType":"Limit","origSz":"0.0003","reduceOnly":false,"side":"B","sz":"0.0003",
            "timestamp":1794013444000,"triggerCondition":"N/A","triggerPx":"0.0","children":[],
            "tif":"Gtc","cloid":"{cloid}"}}]"#
    )
}

fn user_fills_body(coin: &str, cloid: &str) -> String {
    format!(
        r#"[{{"closedPnl":"0.0","coin":"{coin}","crossed":false,"dir":"Open Long",
            "hash":"0xa166e3fa63c25663024b03f2e0da011a00307e4017503868fa5a7bba9d1ae5c8","oid":42177013,
            "px":"38123.0","side":"B","startPosition":"0.0","sz":"0.0003","time":1794013444000,
            "fee":"0.001","feeToken":"USDC","tid":118906512037719,"cloid":"{cloid}"}}]"#
    )
}

/// The client the live start builds: the account's universe ([`spot_universe`]) and its `cloid` map.
fn client_over(cloids: OrderClientIdVenueRegistry) -> ReconcileClientHyperliquid {
    ReconcileClientHyperliquid::new(
        crate::test_support::seed_transport(VenueNetwork::Testnet),
        "0xmaster",
        AssetClass::CryptoPerp,
    )
    .with_instruments(spot_universe())
    .with_cloids(cloids)
}

/// What `fetch_order_status_reports` makes of `body`, without its network read.
fn orders_read(client: &ReconcileClientHyperliquid, body: &str) -> Vec<OrderStatusReport> {
    let mut rows = parse_orders(body).expect("a frontendOpenOrders body");
    for row in &mut rows {
        client.name_order(row);
    }
    rows
}

/// What `fetch_fill_reports` makes of `body`, without its network read.
fn fills_read(client: &ReconcileClientHyperliquid, body: &str) -> Vec<FillReport> {
    let mut rows = parse_fills(body).expect("a userFills body");
    for row in &mut rows {
        client.name_fill(row);
    }
    rows
}

/// An engine on `BTCUSDC.P` whose registry holds `order_client_id` live, as the one that placed it does.
fn engine_holding(
    order_client_id: &str,
) -> vike_execution::Engine<vike_execution::testing::RecordingClient> {
    let mut engine = vike_execution::testing::EngineBuilder {
        venue: VENUE.to_string(),
        symbol: "BTCUSDC.P".to_string(),
        ..Default::default()
    }
    .build();
    let mut held = parse_orders(&open_orders_body("BTC", order_client_id)).expect("a row");
    for row in &mut held {
        row.symbol = "BTCUSDC.P".to_string();
    }
    assert_eq!(engine.reregister_orders(&held), 1, "the engine holds {order_client_id}");
    engine
}

fn kinds(held: &[ReconcileDivergence]) -> Vec<ReconcileDivergenceKind> {
    held.iter().map(ReconcileDivergence::kind).collect()
}

fn quarantine() -> ReconcilePolicy {
    ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() }
}

/// **The bug.** One resting order of ours read as two divergences on every pass it rested: the venue's
/// row under the hash (`UnknownOrder`), our order under the order client id (`OrphanLocalOrder`). The first half is
/// the symptom as measured, off a client with no map; the second is the client the live start builds.
#[test]
fn our_resting_order_reads_by_its_order_client_id_so_the_pass_raises_nothing() {
    assert_eq!(cloid_from_order_client_id(OURS), OURS_CLOID, "the measured pair is one order");
    let engine = engine_holding(OURS);
    let local = engine.local_view();
    let body = open_orders_body("BTC", OURS_CLOID);

    let raw = orders_read(&client_over(OrderClientIdVenueRegistry::new()), &body);
    assert_eq!(
        kinds(&diff(&raw, &[], &[], &local.as_view(), None)),
        [ReconcileDivergenceKind::UnknownOrder, ReconcileDivergenceKind::OrphanLocalOrder],
        "the testnet symptom"
    );

    let cloids = OrderClientIdVenueRegistry::new();
    assert_eq!(cloids.register(OURS), OURS_CLOID, "what a submit writes");
    let rows = orders_read(&client_over(cloids), &body);
    assert_eq!(rows[0].order_client_id.as_deref(), Some(OURS));
    assert_eq!(rows[0].venue_order_id.as_str(), "42177013");
    assert_eq!(rows[0].status, vike_model::OrderStatus::Accepted);
    assert!(kinds(&diff(&rows, &[], &[], &local.as_view(), None)).is_empty());
}

/// A fill row of our order names our order client id too, so the fill a confirm would book lands on the order and
/// its mount and not on a hash; and under `quarantine` nothing folds, exactly as before.
#[test]
fn a_fill_row_of_our_order_names_our_order_client_id_and_quarantine_folds_nothing() {
    let cloids = OrderClientIdVenueRegistry::new();
    cloids.register(OURS);
    let client = client_over(cloids);
    let fills = fills_read(&client, &user_fills_body("BTC", OURS_CLOID));
    assert_eq!(fills[0].order_client_id.as_deref(), Some(OURS));
    assert_eq!(fills[0].trade_id.as_str(), "118906512037719");
    assert_eq!(fills[0].symbol, "BTCUSDC.P", "the coin `BTC` is named by its user name");

    let engine = engine_holding(OURS);
    let local = engine.local_view();
    let orders = orders_read(&client, &open_orders_body("BTC", OURS_CLOID));
    let held = diff(&orders, &fills, &[], &local.as_view(), None);
    assert_eq!(kinds(&held), [ReconcileDivergenceKind::MissingFill]);

    let recon = resolve(held, &quarantine(), None, None);
    assert!(recon.events.is_empty(), "quarantine folds nothing: {:?}", recon.events);
    match recon.alerts[0].proposed_events.as_slice() {
        [Event::Fill(f)] => assert_eq!(f.order_client_id, OURS),
        other => panic!("one proposed fill under our order client id, got {other:?}"),
    }
}

/// A spot row is named twice: the pair by the engine's user name (`@107` is `HYPEUSDC`, decision 0290), the order
/// by our order client id.
#[test]
fn a_spot_row_is_named_by_its_pair_and_by_its_order_client_id() {
    let cloids = OrderClientIdVenueRegistry::new();
    cloids.register(OURS);
    let client = client_over(cloids);
    let order = &orders_read(&client, &open_orders_body("@107", OURS_CLOID))[0];
    assert_eq!((order.symbol.as_str(), order.order_client_id.as_deref()), ("HYPEUSDC", Some(OURS)));
    let fill = &fills_read(&client, &user_fills_body("@107", OURS_CLOID))[0];
    assert_eq!((fill.symbol.as_str(), fill.order_client_id.as_deref()), ("HYPEUSDC", Some(OURS)));
}

/// **A position row is named by its USER name** (decision 0290): the venue's `BTC` is `BTCUSDC.P`, a builder-dex
/// `xyz:TSLA` is `XYZ:TSLAUSDC.P`, so the engine finds its own position; a coin the universe does not know keeps the
/// venue's word.
#[test]
fn a_position_row_is_named_by_its_user_name() {
    let client = client_over(OrderClientIdVenueRegistry::new());
    let body = r#"{"time":1,"assetPositions":[
        {"type":"oneWay","position":{"coin":"BTC","szi":"0.5","entryPx":"29000.0"}},
        {"type":"oneWay","position":{"coin":"xyz:TSLA","szi":"-1.0","entryPx":"400.0"}},
        {"type":"oneWay","position":{"coin":"SOL","szi":"2.0","entryPx":"150.0"}}
    ]}"#;
    let mut rows = parse_positions(body).expect("a clearinghouseState body");
    for row in &mut rows {
        client.name_position(row);
    }
    let named: Vec<&str> = rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(named, ["BTCUSDC.P", "XYZ:TSLAUSDC.P", "SOL"]);
}

/// **A foreign order stays foreign.** A `cloid` the map does not hold keeps the venue's hash, which
/// names no local order: the pass holds its `UnknownOrder`, and nothing is proposed, folded or cancelled
/// for it (a foreign order is cancelled by hand only). A row with no `cloid` stays `None`.
#[test]
fn a_cloid_the_map_does_not_hold_stays_foreign() {
    let cloids = OrderClientIdVenueRegistry::new();
    cloids.register(OURS);
    let client = client_over(cloids);

    let rows = orders_read(&client, &open_orders_body("BTC", FOREIGN_CLOID));
    assert_eq!(rows[0].order_client_id.as_deref(), Some(FOREIGN_CLOID));
    let fills = fills_read(&client, &user_fills_body("BTC", FOREIGN_CLOID));
    assert_eq!(fills[0].order_client_id.as_deref(), Some(FOREIGN_CLOID));

    let bare = r#"[{"coin":"BTC","limitPx":"1.0","oid":7,"orderType":"Limit","origSz":"1.0","side":"B",
                    "sz":"1.0","timestamp":1,"cloid":null}]"#;
    assert_eq!(orders_read(&client, bare)[0].order_client_id, None);

    let engine = engine_holding(OURS);
    let local = engine.local_view();
    let held = diff(&rows, &[], &[], &local.as_view(), None);
    assert_eq!(
        kinds(&held),
        [ReconcileDivergenceKind::UnknownOrder, ReconcileDivergenceKind::OrphanLocalOrder],
        "the foreign row never stands in for our order"
    );
    let recon = resolve(held, &quarantine(), None, None);
    assert!(recon.events.is_empty());
    assert!(recon.alerts.iter().all(|a| a.proposed_events.is_empty()));
}

/// A TRADEHUB restart alone leaves the gatewayhub's session, and so the map, as it was. The row then
/// arrives already named, the core's translation finds no hash to rewrite, and the planner adopts it by
/// the order client id (the restart that empties the map is `execution/execution_tests.rs`'s
/// `after_a_restart_a_row_keeps_the_hash_until_the_adoption_names_it`).
#[test]
fn a_tradehub_restart_keeps_the_map_and_the_planner_adopts_the_named_row() {
    let cloids = OrderClientIdVenueRegistry::new();
    cloids.register(OURS);
    let rows = orders_read(&client_over(cloids), &open_orders_body("BTC", OURS_CLOID));
    let scope = RestoreScope {
        venue: VENUE.to_string(),
        symbols: vec!["BTCUSDC.P".to_string()],
        ..Default::default()
    };
    let restored = RestoredOrderRef {
        order_client_id: OURS.to_string(),
        venue: Some(VENUE.to_string()),
        symbol: Some("BTCUSDC.P".to_string()),
        ..Default::default()
    };
    let plan = plan_restore(&scope, &[restored], &rows, &[]);
    assert_eq!(plan.adopt.len(), 1, "{plan:?}");
    assert_eq!(plan.adopt[0].symbol, "BTCUSDC.P");
}

/// `live_runtime_for_account` cannot run offline (it needs a key and the venue), and every test above
/// passes without the one line that makes the fix live, so that line is held by its text.
#[test]
fn the_live_start_hands_the_reconcile_client_its_execution_clients_map() {
    let adapter = include_str!("../adapter.rs");
    // The one map: a clone into the exec client's spawn, the map itself into the reconcile client.
    assert!(adapter.contains("        cloids.clone(),\n"));
    assert!(adapter.contains("recon.with_cloids(cloids)"));
}

// --- the account's fee tier (FEES-AUDIT 2026-11-08) ---

/// `userFees` as the TESTNET answered it on 2026-11-08 (a public, keyless read of the zero address: the
/// account rows of a base-tier account with no discount; the wire shape any account's answer has).
const USER_FEES_TESTNET: &str =
    include_str!("../../tests/fixtures/user_fees/testnet_base_tier_2026-11-08.json");

/// Drive [`read_fee_rates`] with `body` as the `/info` answer; returns the result and the requests sent.
fn drive_fees(
    class: AssetClass,
    mounted: Option<&str>,
    body: &str,
) -> (Result<Option<FeeSchedule>, String>, Vec<Value>) {
    let sent = std::cell::RefCell::new(Vec::new());
    let out = read_fee_rates(class, mounted, "0xmaster", |req| {
        sent.borrow_mut().push(req.clone());
        Ok(body.to_string())
    });
    (out, sent.into_inner())
}

/// **The account's own rate is read** (`userFees` of the MASTER address), for the mount's product: the
/// recorded base-tier account pays 1.5/4.5 bps on a perpetual and 4/7 bps on a spot pair.
///
/// Base: the client kept the trait's `Ok(None)`, so the mount was priced by the static row whatever the
/// account's tier, staking or referral discount.
#[test]
fn the_accounts_fee_tier_is_read_for_the_mounts_product() {
    let (perp, sent) = drive_fees(AssetClass::CryptoPerp, Some("BTCUSDC.P"), USER_FEES_TESTNET);
    assert_eq!(perp, Ok(Some(FeeSchedule::from_fractions(0.00015, 0.00045))));
    assert_eq!(sent, vec![json!({"type": "userFees", "user": "0xmaster"})]);
    let (spot, _) = drive_fees(AssetClass::CryptoSpot, Some("HYPEUSDC"), USER_FEES_TESTNET);
    assert_eq!(spot, Ok(Some(FeeSchedule::from_fractions(0.0004, 0.0007))));
}

/// The staking discount is already inside `userCrossRate` (the venue's own example: 0.000315 against the
/// 0.00045 base at 30 %); the referral discount is applied on top, to a maker fee and the taker rate, and a
/// maker REBATE is left whole (the venue's fee formula).
#[test]
fn the_referral_discount_applies_on_top_and_never_to_a_rebate() {
    let discounted = r#"{"userCrossRate":"0.000315","userAddRate":"0.000105",
        "userSpotCrossRate":"0.00049","userSpotAddRate":"0.00028","activeReferralDiscount":"0.04"}"#;
    let (perp, _) = drive_fees(AssetClass::CryptoPerp, None, discounted);
    assert_eq!(perp, Ok(Some(FeeSchedule::from_fractions(0.000105 * 0.96, 0.000315 * 0.96))));
    let rebate =
        r#"{"userCrossRate":"0.00024","userAddRate":"-0.00002","activeReferralDiscount":"0.04"}"#;
    let (mm, _) = drive_fees(AssetClass::CryptoPerp, None, rebate);
    assert_eq!(mm, Ok(Some(FeeSchedule::from_fractions(-0.00002, 0.00024 * 0.96))));
    assert_eq!(
        drive_fees(AssetClass::CryptoSpot, None, rebate).0,
        Ok(None),
        "no spot rows: no schedule"
    );
    assert!(drive_fees(AssetClass::CryptoPerp, None, "\"oops\"").0.is_err(), "not an object");
}

/// **A HIP-3 builder-dex mount is refused by name, before anything is asked**: its fee is the account's
/// perp rate times the deployer's scale, which no read states, and the core rate would understate it.
#[test]
fn a_hip3_mount_is_refused_by_name_and_asks_nothing() {
    let ((out, sent), log) = vike_log::capture::captured(|| {
        drive_fees(AssetClass::CryptoPerp, Some("XYZ:TSLAUSDC.P"), USER_FEES_TESTNET)
    });
    let why = out.expect_err("a HIP-3 mount is not priced at the core rate");
    let refusal = HyperliquidReconcileRefusal::Hip3Fee { symbol: "XYZ:TSLAUSDC.P".to_string() };
    assert_eq!(why, refusal.to_string());
    assert!(sent.is_empty(), "nothing is asked: {sent:?}");
    assert!(
        log.iter().any(|e| e.level == tracing::Level::WARN && e.field("why") == Some(why.as_str()))
    );
}

/// The live start names the mounted symbol to the reconcile client (the HIP-3 refusal reads it); it cannot
/// run offline, so the line is held by its text, like the map above.
#[test]
fn the_live_start_names_the_mounted_symbol_to_the_reconcile_client() {
    assert!(include_str!("../adapter.rs").contains(".with_mounted_symbol(symbol)"));
}

// --- B-5: a rate limit on a read made for an admitted symbol silences the pass ---

/// The whole HTTP reply a loopback `/info` answers a request body with.
type Answer = fn(&str) -> &'static str;

use crate::test_support::info_server;

const EMPTY_LIST: &str = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n[]";

fn throttled_dex(body: &str) -> &'static str {
    if body.contains("\"dex\"") {
        "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 4\r\nconnection: close\r\n\r\nslow"
    } else {
        EMPTY_LIST
    }
}

fn refused_dex(body: &str) -> &'static str {
    if body.contains("\"dex\"") {
        "HTTP/1.1 400 Bad Request\r\ncontent-length: 7\r\nconnection: close\r\n\r\nbad dex"
    } else {
        EMPTY_LIST
    }
}

/// **Owner ruling B-5.** The dex read made for an admitted `XYZ:` coin (the venue's dex `xyz`) answered 429 (after the `/info`
/// door's own bounded retries) fails the fetch in the venue's words and makes the next fetch refuse before
/// it asks anything; any other refusal fails that fetch alone, and the next one asks again.
#[test]
fn a_rate_limited_admitted_read_silences_the_pass_and_any_other_refusal_does_not() {
    let cases: [(Answer, &str, bool); 2] =
        [(throttled_dex, "slow", true), (refused_dex, "bad dex", false)];
    for (answer, words, pauses) in cases {
        let (url, seen) = info_server(answer);
        let client = ReconcileClientHyperliquid::new(
            HyperliquidTransport::new(VenueNetwork::Testnet, &url),
            "0xmaster",
            AssetClass::CryptoPerp,
        )
        .with_instruments(spot_universe());
        client.admit_symbol("XYZ:TSLAUSDC.P");
        assert_eq!(client.fetch_order_status_reports(0).expect_err("the dex read failed"), words);
        let fills = client.fetch_fill_reports(0);
        let asked = seen.lock().unwrap().iter().filter(|b| b.contains("userFills")).count();
        if pauses {
            let why = fills.expect_err("the pass is silent");
            assert!(why.contains("backing off"), "{why}");
            assert_eq!(asked, 0, "nothing is asked while the pass is silent");
        } else {
            assert!(fills.is_ok(), "{fills:?}");
            assert_eq!(asked, 1, "the next fetch asks again");
        }
    }
}
