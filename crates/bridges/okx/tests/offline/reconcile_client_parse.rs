//! Pure parse tests for the okx `ReconcileClient` report parsers (Task 11) — NO network, just
//! representative JSON bodies shaped like OKX's real V5 REST responses (`GET
//! /api/v5/trade/orders-pending`, `GET /api/v5/trade/fills`, `GET /api/v5/account/positions`).
//! Asserts the tricky mappings the brief calls out explicitly: the contracts→base conversion via
//! `ct_val` (positions AND orders AND fills all quote size in contracts, price fields never
//! rescaled), the already-signed `pos` field, side sign (`buy`/`sell`), venue-status
//! normalization (`live`→`ACCEPTED`), the maker/taker `execType` flag, and the negative-fee sign
//! convention (`commission = -fee`).

use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FeeSchedule, MarginMode};
use vike_okx::reconcile::{
    parse_balance, parse_fee_rate, parse_fills, parse_orders_pending, parse_positions,
};

/// `/api/v5/account/trade-fee` `data[0]` -> FeeSchedule. OKX reports NEGATIVE fractions for a
/// charge; the parser negates (cost > 0). USDT-margined SWAP prefers `makerU`/`takerU`.
#[test]
fn parses_trade_fee_negating_okx_sign() {
    let body = r#"[{"category":"1","maker":"-0.0002","taker":"-0.0005","makerU":"-0.0002","takerU":"-0.0005","instType":"SWAP","ts":"1700000000000"}]"#;
    let s = parse_fee_rate(body).unwrap().expect("fee row present");
    assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 });
}

/// A maker rebate (positive OKX rate) negates to a negative bps (rebate), per the sign convention.
#[test]
fn parses_trade_fee_maker_rebate_as_negative_bps() {
    let body =
        r#"[{"maker":"0.0001","taker":"-0.0005","makerU":"","takerU":"","instType":"SWAP"}]"#;
    let s = parse_fee_rate(body).unwrap().expect("fee row present");
    // makerU/takerU empty -> falls back to maker/taker; maker +0.0001 -> -1 bps (rebate)
    assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: -1.0, taker_bps: 5.0 });
}

/// An empty `data` array -> None (fail-soft to the static default).
#[test]
fn empty_trade_fee_is_none() {
    assert_eq!(parse_fee_rate("[]").unwrap(), None);
}

/// The exact shape from the task brief: a SWAP order in the `live` state.
#[test]
fn parses_live_order_and_normalizes_status_to_accepted() {
    let body = r#"[{"instId":"BTC-USDT-SWAP","ordId":"312269865356374016","clOrdId":"b1","px":"30000","sz":"3","ordType":"limit","side":"buy","posSide":"net","accFillSz":"0","avgPx":"","state":"live","uTime":"1618235248028"}]"#;
    let reports = parse_orders_pending(body, 0.01).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.status, vike_model::OrderStatus::Accepted, "live -> ACCEPTED");
    assert_eq!(r.side, 1, "buy -> +1");
    assert_eq!(r.average_price, 0.0, "empty avgPx -> 0.0, no divide-by-zero/parse panic");
}

#[test]
fn order_row_rescales_sz_and_acc_fill_sz_contracts_to_base() {
    let body = r#"[{"instId":"BTC-USDT-SWAP","ordId":"555","clOrdId":"c1","px":"30000","sz":"300","ordType":"limit","side":"sell","posSide":"net","accFillSz":"120","avgPx":"30010.5","state":"partially_filled","uTime":"1700000002000"}]"#;
    // ct_val = 0.01 base per contract (BTC-USDT-SWAP's real grid).
    let reports = parse_orders_pending(body, 0.01).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.qty, 3.0, "300 contracts * 0.01 ct_val = 3.0 base");
    assert_eq!(r.filled_qty, 1.2, "120 contracts * 0.01 ct_val = 1.2 base");
    assert_eq!(r.average_price, 30010.5, "price fields are NEVER rescaled by ct_val");
    assert_eq!(r.status, vike_model::OrderStatus::PartiallyFilled);
    assert_eq!(r.side, -1, "sell -> -1");
    assert_eq!(r.venue, "okx");
    assert_eq!(r.symbol, "BTC-USDT-SWAP");
    assert_eq!(r.venue_order_id.as_str(), "555");
    assert_eq!(r.order_client_id.as_deref(), Some("c1"));
    assert_eq!(r.timestamp, 1700000002000);
    assert_eq!(r.price, Some(30000.0), "a limit row's `px` is its limit, never rescaled");
    assert_eq!((r.trigger_price, r.trail, r.trail_percent), (None, None, None));
}

#[test]
fn order_row_empty_order_client_id_normalizes_to_none() {
    let body = r#"[{"instId":"BTC-USDT-SWAP","ordId":"999","clOrdId":"","px":"30000","sz":"1","ordType":"market","side":"buy","posSide":"net","accFillSz":"0","avgPx":"","state":"filled","uTime":"1700000003000"}]"#;
    let reports = parse_orders_pending(body, 0.01).unwrap();
    assert_eq!(reports[0].order_client_id, None, "empty clOrdId -> None (externally placed)");
    assert_eq!(reports[0].status, vike_model::OrderStatus::Filled);
    assert_eq!(reports[0].order_type, vike_model::OrderType::Market);
    assert_eq!(reports[0].price, None, "a market row states no limit, whatever its `px`");
}

/// The exact shape from the task brief: a fill with maker liquidity and a fee charge.
#[test]
fn parses_maker_fill_with_negative_fee_charge() {
    let body = r#"[{"instId":"BTC-USDT-SWAP","tradeId":"17361620","ordId":"467872924962541567","clOrdId":"","subType":"1","side":"buy","fillSz":"50","fillPx":"29000.0","fee":"-0.145","feeCcy":"USDT","execType":"M","posSide":"net","ts":"1621927314985"}]"#;
    let reports = parse_fills(body, 0.01).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.last_qty, 0.5, "50 contracts * 0.01 ct_val = 0.5 base");
    assert_eq!(r.last_price, 29000.0, "price is NEVER rescaled");
    assert_eq!(r.commission, 0.145, "fee -0.145 (a charge) -> commission = +0.145");
    assert_eq!(r.commission_asset, "USDT");
    assert_eq!(r.liquidity_side, LiquiditySide::Maker, "execType M -> Maker");
    assert_eq!(r.side, 1, "buy -> +1");
    assert_eq!(r.trade_id.as_str(), "17361620");
    assert_eq!(r.venue_order_id.as_str(), "467872924962541567");
    assert_eq!(r.order_client_id, None, "empty clOrdId -> None");
    assert_eq!(r.venue, "okx");
    assert_eq!(r.timestamp, 1621927314985);
}

#[test]
fn parses_taker_fill_with_positive_fee_rebate() {
    let body = r#"[{"instId":"BTC-USDT-SWAP","tradeId":"17361621","ordId":"100","clOrdId":"c2","subType":"2","side":"sell","fillSz":"10","fillPx":"29500.0","fee":"0.02","feeCcy":"USDT","execType":"T","posSide":"net","ts":"1621927315000"}]"#;
    let reports = parse_fills(body, 0.01).unwrap();
    let r = &reports[0];
    assert_eq!(r.side, -1, "sell -> -1");
    assert_eq!(r.liquidity_side, LiquiditySide::Taker, "execType T -> Taker");
    assert_eq!(r.commission, -0.02, "fee +0.02 (a rebate) -> commission = -0.02");
    assert_eq!(r.order_client_id.as_deref(), Some("c2"));
}

/// A `trade/fills` row of the account's own order (`subType` 2, a sell), and the same execution as a liquidation
/// (`subType` 104, OKX's negative `tradeId`, no `execType`: "not applicable to system orders such as ADL and
/// liquidation"), the shapes OKX V5's "Get transaction details" documents.
const OKX_TRADE_ROW: &str = r#"{"instId":"BTC-USDT-SWAP","tradeId":"17361622","ordId":"101","clOrdId":"c3","billId":"9","subType":"2","side":"sell","fillSz":"10","fillPx":"29500.0","fee":"-0.02","feeCcy":"USDT","execType":"T","posSide":"net","ts":"1621927316000"}"#;
const OKX_LIQUIDATION_ROW: &str = r#"{"instId":"BTC-USDT-SWAP","tradeId":"-17361623","ordId":"102","clOrdId":"","billId":"10","subType":"104","side":"sell","fillSz":"10","fillPx":"28000.0","fee":"-0.05","feeCcy":"USDT","execType":"","posSide":"net","ts":"1621927317000"}"#;

/// **A liquidation is not a fill**: the order stream books it as `PositionLiquidated` (its `tradeId` goes to
/// `seen_liq_ids`, never `seen_trade_ids`), so reported here it was an order-client-id-less `MissingFill` that `hybrid` auto-applies
/// as a second close.
#[test]
fn a_liquidation_row_is_not_a_fill() {
    assert!(parse_fills(&format!("[{OKX_LIQUIDATION_ROW}]"), 0.01).unwrap().is_empty());
}

#[test]
fn a_trade_row_with_its_sub_type_is_still_a_fill() {
    let reports = parse_fills(&format!("[{OKX_TRADE_ROW}]"), 0.01).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].trade_id.as_str(), "17361622");
    assert_eq!(reports[0].order_client_id.as_deref(), Some("c3"));
}

/// **A mixed body keeps its ordinary trades**: an ADL (125), a delivery (112) and an easy-convert (236) row go with the
/// liquidation; a trade that opens a long (3) stays.
#[test]
fn a_mixed_body_keeps_only_its_ordinary_trades() {
    let as_sub = |sub: &str, id: &str| {
        OKX_TRADE_ROW
            .replace(r#""subType":"2""#, &format!(r#""subType":"{sub}""#))
            .replace("17361622", id)
    };
    let body = format!(
        "[{OKX_LIQUIDATION_ROW},{OKX_TRADE_ROW},{},{},{},{}]",
        as_sub("125", "-1"),
        as_sub("112", "2"),
        as_sub("236", "3"),
        as_sub("3", "4"),
    );
    let reports = parse_fills(&body, 0.01).unwrap();
    let ids: Vec<&str> = reports.iter().map(|r| r.trade_id.as_str()).collect();
    assert_eq!(ids, ["17361622", "4"]);
}

/// **A row with no `subType` is not a fill** (OKX documents it on every row; bybit drops a row with no `execType`
/// alike): without it a liquidation cannot be told from a trade. A sibling that carries one still reports.
#[test]
fn a_row_with_no_sub_type_is_not_a_fill() {
    let untyped = OKX_TRADE_ROW.replace(r#""subType":"2","#, "").replace("17361622", "5");
    let blank =
        OKX_TRADE_ROW.replace(r#""subType":"2""#, r#""subType":"""#).replace("17361622", "6");
    let reports = parse_fills(&format!("[{untyped},{blank},{OKX_TRADE_ROW}]"), 0.01).unwrap();
    let ids: Vec<&str> = reports.iter().map(|r| r.trade_id.as_str()).collect();
    assert_eq!(ids, ["17361622"]);
}

/// `pos` is ALREADY SIGNED contracts — the highest-risk mapping. One-way (`net`) leg AND hedge
/// long/short legs, all rescaled to base via `ct_val`, never re-signed by `posSide`.
#[test]
fn positions_rescale_signed_contracts_to_base_one_way_and_hedge() {
    let body = r#"[
        {"instId":"BTC-USDT-SWAP","posSide":"net","pos":"250","avgPx":"29000.0","uTime":"1700000010000"}
    ]"#;
    let reports = parse_positions(body, "BTC-USDT-SWAP", 0.01).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].qty, 2.5, "250 contracts * 0.01 ct_val = 2.5 base (long)");
    assert_eq!(reports[0].position_side, PositionSide::Both, "posSide net -> Both");
    assert_eq!(reports[0].average_price, 29000.0, "price is NEVER rescaled");

    let hedge_body = r#"[
        {"instId":"BTC-USDT-SWAP","posSide":"long","pos":"250","avgPx":"29000.0","uTime":"1700000010000"},
        {"instId":"BTC-USDT-SWAP","posSide":"short","pos":"-100","avgPx":"29200.0","uTime":"1700000012000"}
    ]"#;
    let hedge = parse_positions(hedge_body, "BTC-USDT-SWAP", 0.01).unwrap();
    assert_eq!(hedge.len(), 2);
    assert_eq!(hedge[0].qty, 2.5);
    assert_eq!(hedge[0].position_side, PositionSide::Long, "posSide long -> Long");
    assert_eq!(hedge[1].qty, -1.0, "-100 contracts * 0.01 = -1.0 base — SHORT stays negative");
    assert_eq!(hedge[1].position_side, PositionSide::Short, "posSide short -> Short");
}

/// Margin-mode step-2: `mgnMode` "isolated" -> Isolated + the row's `margin` balance; "cross"
/// (with OKX's empty-string `margin`) and an ABSENT `mgnMode` both land the fail-safe Cross/None.
/// The order path honors `OrderRequest.margin_mode` via `perp::swap_td_mode` (unset = "cross").
#[test]
fn positions_map_mgn_mode_to_margin_mode() {
    let body = r#"[
        {"instId":"BTC-USDT-SWAP","posSide":"net","pos":"250","avgPx":"29000.0","mgnMode":"isolated","margin":"812.5","uTime":"1700000010000"},
        {"instId":"ETH-USDT-SWAP","posSide":"net","pos":"10","avgPx":"1800.0","mgnMode":"cross","margin":"","uTime":"1700000011000"},
        {"instId":"XRP-USDT-SWAP","posSide":"net","pos":"5","avgPx":"0.5","uTime":"1700000012000"}
    ]"#;
    let reports = parse_positions(body, "BTC-USDT-SWAP", 0.01).unwrap();
    assert_eq!(reports.len(), 3);
    assert_eq!(reports[0].margin_mode, MarginMode::Isolated, "mgnMode isolated -> Isolated");
    assert_eq!(reports[0].isolated_margin, Some(812.5), "isolated `margin` balance surfaced");
    assert_eq!(reports[1].margin_mode, MarginMode::Cross, "mgnMode cross -> Cross");
    assert_eq!(reports[1].isolated_margin, None, "cross empty-string margin stays None");
    assert_eq!(reports[2].margin_mode, MarginMode::Cross, "absent mgnMode -> fail-safe Cross");
    assert_eq!(reports[2].isolated_margin, None);
}

/// OKX omits a symbol entirely once flat (no `pos: "0"` row) — unlike Binance's `positionRisk`.
/// `recon::diff::diff` needs a report ROW present to detect "local still shows a position OKX has
/// since closed", so an empty `data` array must synthesize one flat row for the queried symbol.
#[test]
fn empty_positions_array_synthesizes_a_flat_row_for_the_symbol() {
    let reports = parse_positions("[]", "BTC-USDT-SWAP", 0.01).unwrap();
    assert_eq!(reports.len(), 1, "a flat row must be synthesized, not silently dropped");
    assert_eq!(reports[0].qty, 0.0);
    assert_eq!(reports[0].symbol, "BTC-USDT-SWAP");
    assert_eq!(reports[0].venue, "okx");
}

/// A defensive `pos: "0"` row, if OKX ever DOES send one, must survive parsing (never filtered) —
/// same "keep flat rows" philosophy as Binance's `positionRisk` parser.
#[test]
fn a_present_flat_row_is_kept_not_filtered() {
    let body = r#"[{"instId":"BTC-USDT-SWAP","posSide":"net","pos":"0","avgPx":"0.0","uTime":"1700000011000"}]"#;
    let reports = parse_positions(body, "BTC-USDT-SWAP", 0.01).unwrap();
    assert_eq!(reports.len(), 1, "the present flat row must survive parsing");
    assert_eq!(reports[0].qty, 0.0);
}

#[test]
fn malformed_body_is_an_error_not_a_panic() {
    assert!(parse_orders_pending("not json", 0.01).is_err());
    assert!(parse_orders_pending("{}", 0.01).is_err(), "an object, not an array, is an error");
    assert!(parse_fills("null", 0.01).is_err());
    assert!(parse_positions("42", "BTC-USDT-SWAP", 0.01).is_err());
    assert!(parse_positions("\"oops\"", "BTC-USDT-SWAP", 0.01).is_err());
}

/// `GET /api/v5/account/balance` `data[].details[]` -> the account's USDT `cashBal` (total cash —
/// free + frozen; NOT `availBal`, which excludes funds locked by open orders/positions).
#[test]
fn parses_account_balance_usdt_cash() {
    let body = r#"[{"adjEq":"","totalEq":"1520.5","details":[
        {"ccy":"BTC","cashBal":"0.01","availBal":"0.01"},
        {"ccy":"USDT","cashBal":"1500.25","availBal":"1400.0"}
    ]}]"#;
    assert_eq!(parse_balance(body).unwrap(), Some(1500.25));
}

#[test]
fn account_balance_missing_usdt_detail_is_none() {
    let body = r#"[{"details":[{"ccy":"BTC","cashBal":"0.01"}]}]"#;
    assert_eq!(parse_balance(body).unwrap(), None);
}

#[test]
fn account_balance_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_balance("not json").is_err());
    assert!(parse_balance("{}").is_err(), "an object, not an array, is an error");
    assert!(parse_balance("null").is_err());
}

// --- the `recon_client` factory (ReconFactory seam, wave-2 task 6) — no network, just proves the
// wiring is infallible ------------------------------------------------------------------------

#[test]
fn reconcile_client_factory_builds_for_any_credentialed_symbol() {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "test-key".to_string().into(),
        api_secret: "test-secret".to_string().into(),
        passphrase: Some("test-pass".to_string().into()),
    };
    assert!(
        vike_okx::recon_client(&creds, "BTC-USDT-SWAP", 0.01, &vike_okx::test_support::demo_net())
            .is_some(),
        "construction is pure/infallible (no network) — always Some"
    );
}

// --- the REQUEST side: which query params each report fetch actually sends -----------------------
//
// ⚠ Everything above tests the PARSER, and a parser cannot see a request that the venue rejected.
// `fetch_fee_rates` is fail-soft — a 400 falls through to the static default fee schedule — so a
// malformed request produced no error anywhere, no log line, and a maker strategy sized against
// default fees instead of the account's real ones. It took a live start to notice.

/// The captured `(path, params)` of every signed GET. Shared through an `Arc` rather than read back
/// off the client, so the assertions need no accessor on the production type.
type Calls = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<(String, serde_json::Value)>)>>>;

/// A transport that records every signed GET and answers with a canned body. No network — the point
/// is the REQUEST, not the response.
struct CapturedGet {
    calls: Calls,
    reply: serde_json::Value,
    /// The answer of the public lane, which only a spot pair's fee read asks (its `groupId`).
    public: Option<serde_json::Value>,
}

impl RestTransportJson for CapturedGet {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &dyn vike_bridge_core::signer::SignerJson,
    ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
        self.calls.lock().unwrap().push((
            path.to_string(),
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        ));
        Ok(self.reply.clone())
    }

    // The recon client signs every read but a spot pair's `public/instruments` row; a test that
    // answers no public call proves none is made.
    fn public(
        &self,
        _base: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<serde_json::Value, vike_bridge_core::transport::VenueApiError> {
        let reply = self.public.clone().expect("the recon client must use the SIGNED lane here");
        self.calls.lock().unwrap().push((
            path.to_string(),
            params.iter().map(|(k, v)| (k.to_string(), serde_json::json!(v))).collect(),
        ));
        Ok(reply)
    }
}

/// **`/api/v5/account/trade-fee` takes `instType` ALONE — `instId` alongside it is a 400.**
///
/// MEASURED against the live OKX demo endpoint (`x-simulated-trading: 1`) during the first real
/// vike-tradehub CEX start:
///
/// ```text
/// ?instType=SWAP&instId=BTC-USDT-SWAP -> HTTP 400 {"code":"50016","msg":"instId and instType don't match"}
/// ?instType=SWAP                      -> HTTP 200 {"code":"0","data":[{"makerU":"-0.0002","takerU":"-0.0005",...}]}
/// ```
///
/// OKX applies `instId` to SPOT/MARGIN only and rejects the pair rather than ignoring the narrower
/// key. Because the fetch is fail-soft, the rejection was invisible: every pass silently kept the
/// STATIC default fee schedule. MUTATION: add `("instId", json!(self.symbol))` back to
/// `fetch_fee_rates` and this goes red on the `instId` assert.
#[test]
fn the_fee_fetch_sends_inst_type_alone() {
    use vike_execution::recon::ReconcileClient;

    let calls: Calls = Default::default();
    let client = vike_okx::ReconcileClientOkx::new(
        vike_okx::signer::OkxV5Signer::new(
            &vike_secrets::credentials::Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            vike_model::now_ms,
        ),
        CapturedGet {
            calls: std::sync::Arc::clone(&calls),
            // The REAL demo envelope, trimmed — captured in the session that found this. The FULL
            // envelope, not a bare `data` array, because `ReconcileClientOkx::signed` runs the
            // response through `unwrap_okx` before the parser ever sees it.
            reply: serde_json::json!({
                "code": "0",
                "msg": "",
                "data": [{
                    "instType": "SWAP", "maker": "-0.0002", "taker": "-0.0005",
                    "makerU": "-0.0002", "takerU": "-0.0005", "level": "Lv1",
                    "ruleType": "normal", "ts": "1786220668413"
                }]
            }),
            public: None,
        },
        "https://www.okx.com",
        "BTC-USDT-SWAP",
        0.01,
    );

    let schedule = client.fetch_fee_rates().expect("the fetch must succeed");
    assert_eq!(
        schedule,
        Some(FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 }),
        "the live demo account's real SWAP rates must reach the schedule"
    );

    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "one signed GET, got {calls:?}");
    let (path, params) = &calls[0];
    assert!(path.ends_with("/account/trade-fee"), "unexpected path {path}");
    let keys: Vec<&str> = params.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        vec!["instType"],
        "⚠ trade-fee takes instType ALONE — OKX rejects `instId` alongside it with 50016 (HTTP \
         400), and the fetch is fail-soft, so the rejection silently reverts every pass to the \
         STATIC default fee schedule. Got {keys:?}"
    );
    assert_eq!(params[0].1, serde_json::json!("SWAP"), "this client mounts SWAP instruments");
}

/// The GETs `client` makes for one `fetch_fee_rates`, over a transport answering `reply` signed and
/// `public` on the public lane.
fn fee_fetch(
    symbol: &str,
    reply: serde_json::Value,
    public: Option<serde_json::Value>,
) -> (Option<FeeSchedule>, Calls) {
    use vike_execution::recon::ReconcileClient;

    let calls: Calls = Default::default();
    let client = vike_okx::ReconcileClientOkx::new(
        vike_okx::signer::OkxV5Signer::new(
            &vike_secrets::credentials::Credentials {
                api_key: "k".to_string().into(),
                api_secret: "s".to_string().into(),
                passphrase: Some("p".to_string().into()),
            },
            vike_model::now_ms,
        ),
        CapturedGet { calls: std::sync::Arc::clone(&calls), reply, public },
        "https://www.okx.com",
        symbol,
        1.0,
    );
    (client.fetch_fee_rates().expect("the fetch must succeed"), calls)
}

/// The demo's recorded answers of 2026-11-08: the SPOT `trade-fee` reply and `pair`'s own
/// `public/instruments` reply.
fn recorded_spot(pair: &str) -> (serde_json::Value, serde_json::Value) {
    let json = |body: &str| -> serde_json::Value {
        serde_json::from_str(body).expect("the capture is JSON")
    };
    let fee = json(include_str!("../fixtures/trade_fee/demo_spot_2026-11-08.json"));
    let rows = json(include_str!("../fixtures/instruments/demo_spot_groups_2026-11-08.json"));
    (fee, rows[pair].clone())
}

/// **The fee read asks the MOUNTED instrument's class** (FEES-AUDIT 2026-11-08): a spot pair asks
/// `instType=SPOT` (still alone: `instId` is never sent) and its USDT-pair rate reaches the schedule; a
/// dated future asks `FUTURES`. The replies are the demo account's own (`tests/fixtures/trade_fee/`, read
/// on 2026-11-08 and re-serialized from the probe's print): SPOT states 8/10 bps with `makerU`/`takerU`
/// empty, so the parser falls back to `maker`/`taker`; the swap tier it used to read is 2/5.
///
/// Base: every mount asked `instType=SWAP`, so a spot mount was priced at the account's swap tier.
#[test]
fn the_fee_fetch_asks_the_mounted_instruments_class() {
    let (spot, row) = recorded_spot("BTC-USDT");
    let (schedule, calls) = fee_fetch("BTC-USDT", spot, Some(row));
    assert_eq!(schedule, Some(FeeSchedule::PercentMakerTaker { maker_bps: 8.0, taker_bps: 10.0 }));
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "the fee read, then the pair's row: {calls:?}");
    assert_eq!(
        calls[0].1,
        vec![("instType".to_string(), serde_json::json!("SPOT"))],
        "a spot pair asks its own class, instType alone"
    );

    let futures: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/trade_fee/demo_futures_2026-11-08.json"))
            .expect("the capture is JSON");
    let (schedule, calls) = fee_fetch("BTC-USDT-261225", futures, None);
    assert_eq!(schedule, Some(FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.0 }));
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls[0].1, vec![("instType".to_string(), serde_json::json!("FUTURES"))]);
}

/// **A spot pair pays its OWN fee group's rate.** The demo's SPOT `trade-fee` answer states group 1's
/// rate at the top level (8/10 bps) and lists 25 groups; each pair's `public/instruments` row names
/// its group. Recorded the same day: SOL-BTC is in group 2 (5/7 bps) and USDC-USDT in group 11 (0/0).
/// The read asks the pair's row (`instType=SPOT`, `instId`) on the public lane.
///
/// Base: every spot pair was priced at the top-level 8/10, whatever its group.
#[test]
fn a_spot_pair_pays_its_own_fee_groups_rate() {
    for (pair, maker_bps, taker_bps) in [("SOL-BTC", 5.0, 7.0), ("USDC-USDT", 0.0, 0.0)] {
        let (spot, row) = recorded_spot(pair);
        let (schedule, calls) = fee_fetch(pair, spot, Some(row));
        assert_eq!(
            schedule,
            Some(FeeSchedule::PercentMakerTaker { maker_bps, taker_bps }),
            "{pair}"
        );
        let calls = calls.lock().unwrap().clone();
        assert!(calls[1].0.ends_with("/public/instruments"), "{calls:?}");
        assert_eq!(
            calls[1].1,
            vec![
                ("instType".to_string(), serde_json::json!("SPOT")),
                ("instId".to_string(), serde_json::json!(pair)),
            ]
        );
    }
}
