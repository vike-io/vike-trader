//! Pure parse tests for the okx `ReconClient` report parsers (Task 11) — NO network, just
//! representative JSON bodies shaped like OKX's real V5 REST responses (`GET
//! /api/v5/trade/orders-pending`, `GET /api/v5/trade/fills`, `GET /api/v5/account/positions`).
//! Asserts the tricky mappings the brief calls out explicitly: the contracts→base conversion via
//! `ct_val` (positions AND orders AND fills all quote size in contracts, price fields never
//! rescaled), the already-signed `pos` field, side sign (`buy`/`sell`), venue-status
//! normalization (`live`→`ACCEPTED`), the maker/taker `execType` flag, and the negative-fee sign
//! convention (`commission = -fee`).

use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FeeSchedule, MarginMode};
use vike_okx::recon_client::{
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
    assert_eq!(r.status, "ACCEPTED", "live -> ACCEPTED");
    assert_eq!(r.side, 1, "buy -> +1");
    assert_eq!(r.avg_px, 0.0, "empty avgPx -> 0.0, no divide-by-zero/parse panic");
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
    assert_eq!(r.avg_px, 30010.5, "price fields are NEVER rescaled by ct_val");
    assert_eq!(r.status, "PARTIALLY_FILLED");
    assert_eq!(r.side, -1, "sell -> -1");
    assert_eq!(r.venue, "okx");
    assert_eq!(r.symbol, "BTC-USDT-SWAP");
    assert_eq!(r.venue_order_id.as_str(), "555");
    assert_eq!(r.client_order_id.as_deref(), Some("c1"));
    assert_eq!(r.ts, 1700000002000);
}

#[test]
fn order_row_empty_client_order_id_normalizes_to_none() {
    let body = r#"[{"instId":"BTC-USDT-SWAP","ordId":"999","clOrdId":"","px":"30000","sz":"1","ordType":"market","side":"buy","posSide":"net","accFillSz":"0","avgPx":"","state":"filled","uTime":"1700000003000"}]"#;
    let reports = parse_orders_pending(body, 0.01).unwrap();
    assert_eq!(reports[0].client_order_id, None, "empty clOrdId -> None (externally placed)");
    assert_eq!(reports[0].status, "FILLED");
    assert_eq!(reports[0].order_type, "market");
}

/// The exact shape from the task brief: a fill with maker liquidity and a fee charge.
#[test]
fn parses_maker_fill_with_negative_fee_charge() {
    let body = r#"[{"instId":"BTC-USDT-SWAP","tradeId":"17361620","ordId":"467872924962541567","clOrdId":"","side":"buy","fillSz":"50","fillPx":"29000.0","fee":"-0.145","feeCcy":"USDT","execType":"M","posSide":"net","ts":"1621927314985"}]"#;
    let reports = parse_fills(body, 0.01).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.last_qty, 0.5, "50 contracts * 0.01 ct_val = 0.5 base");
    assert_eq!(r.last_px, 29000.0, "price is NEVER rescaled");
    assert_eq!(r.commission, 0.145, "fee -0.145 (a charge) -> commission = +0.145");
    assert_eq!(r.commission_asset, "USDT");
    assert_eq!(r.liquidity_side, LiquiditySide::Maker, "execType M -> Maker");
    assert_eq!(r.side, 1, "buy -> +1");
    assert_eq!(r.trade_id.as_str(), "17361620");
    assert_eq!(r.venue_order_id.as_str(), "467872924962541567");
    assert_eq!(r.client_order_id, None, "empty clOrdId -> None");
    assert_eq!(r.venue, "okx");
    assert_eq!(r.ts, 1621927314985);
}

#[test]
fn parses_taker_fill_with_positive_fee_rebate() {
    let body = r#"[{"instId":"BTC-USDT-SWAP","tradeId":"17361621","ordId":"100","clOrdId":"c2","side":"sell","fillSz":"10","fillPx":"29500.0","fee":"0.02","feeCcy":"USDT","execType":"T","posSide":"net","ts":"1621927315000"}]"#;
    let reports = parse_fills(body, 0.01).unwrap();
    let r = &reports[0];
    assert_eq!(r.side, -1, "sell -> -1");
    assert_eq!(r.liquidity_side, LiquiditySide::Taker, "execType T -> Taker");
    assert_eq!(r.commission, -0.02, "fee +0.02 (a rebate) -> commission = -0.02");
    assert_eq!(r.client_order_id.as_deref(), Some("c2"));
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
    assert_eq!(reports[0].avg_px, 29000.0, "price is NEVER rescaled");

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
/// free + frozen; NOT `availBal`, which excludes funds locked by open orders/positions). Same
/// field `perp::OkxPerpRest::fetch_usdt_balance` already extracts.
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
fn recon_client_factory_builds_for_any_credentialed_symbol() {
    let creds = vike_bridge_core::Credentials {
        api_key: "test-key".to_string(),
        api_secret: "test-secret".to_string(),
        passphrase: Some("test-pass".to_string()),
    };
    assert!(
        vike_okx::recon_client(&creds, "BTC-USDT-SWAP", 0.01, false).is_some(),
        "construction is pure/infallible (no network) — always Some"
    );
}

// --- the REQUEST side: which query params each report fetch actually sends -----------------------
//
// ⚠ Everything above tests the PARSER, and a parser cannot see a request that the venue rejected.
// `fetch_fee_rates` is fail-soft — a 400 falls through to the static default fee schedule — so a
// malformed request produced no error anywhere, no log line, and a maker strategy sized against
// default fees instead of the account's real ones. It took a live mount to notice.

/// The captured `(path, params)` of every signed GET. Shared through an `Arc` rather than read back
/// off the client, so the assertions need no accessor on the production type.
type Calls = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<(String, serde_json::Value)>)>>>;

/// A transport that records every signed GET and answers with a canned body. No network — the point
/// is the REQUEST, not the response.
struct CapturedGet {
    calls: Calls,
    reply: serde_json::Value,
}

impl vike_okx::transport::OkxTransport for CapturedGet {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        _method: &str,
        params: &[(&str, serde_json::Value)],
        _signer: &vike_bridge_core::signer::OkxV5Signer,
    ) -> Result<serde_json::Value, vike_bridge_core::VenueApiError> {
        self.calls.lock().unwrap().push((
            path.to_string(),
            params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        ));
        Ok(self.reply.clone())
    }

    /// The recon client signs every read; nothing here may reach the public lane.
    fn public(
        &self,
        _base: &str,
        path: &str,
        _params: &[(&str, String)],
    ) -> Result<serde_json::Value, vike_bridge_core::VenueApiError> {
        panic!("the recon client must use the SIGNED lane, not public ({path})");
    }
}

/// **`/api/v5/account/trade-fee` takes `instType` ALONE — `instId` alongside it is a 400.**
///
/// MEASURED against the live OKX demo endpoint (`x-simulated-trading: 1`) during the first real
/// vike-tradehub CEX mount:
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
    use vike_exec::recon::ReconClient;

    let calls: Calls = Default::default();
    let client = vike_okx::OkxReconClient::new(
        vike_bridge_core::signer::OkxV5Signer::new(
            &vike_bridge_core::Credentials {
                api_key: "k".to_string(),
                api_secret: "s".to_string(),
                passphrase: Some("p".to_string()),
            },
            vike_model::now_ms,
        ),
        CapturedGet {
            calls: std::sync::Arc::clone(&calls),
            // The REAL demo envelope, trimmed — captured in the session that found this. The FULL
            // envelope, not a bare `data` array, because `OkxReconClient::signed` runs the
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
