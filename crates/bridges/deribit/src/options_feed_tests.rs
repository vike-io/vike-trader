use super::*;

/// Captured verbatim from mainnet (2026-07-26), trimmed to two rows. `iv` DECIMAL, `mark_price`
/// COIN units — the units this whole feed exists to carry faithfully.
const FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"markprice.options.btc_usd","data":[{"timestamp":1785079023870,"iv":0.3706,"instrument_name":"BTC-31JUL26-67000-C","mark_price":0.005},{"timestamp":1785079023870,"iv":0.6527,"instrument_name":"BTC-31JUL26-106000-P","mark_price":0.6376}]}}"#;

#[test]
fn channel_builder_lowercases() {
    assert_eq!(markprice_options_channel("btc_usd"), "markprice.options.btc_usd");
    assert_eq!(markprice_options_channel("ETH_USD"), "markprice.options.eth_usd");
}

#[test]
fn parses_captured_frame_iv_decimal_mark_coin() {
    let v: Value = serde_json::from_str(FRAME).unwrap();
    let rows = parse_markprice_options(&v).unwrap();
    assert_eq!(rows.len(), 2);
    // iv stored VERBATIM (decimal, no ÷100); mark_price left in COIN units (scaled downstream).
    assert_eq!(
        rows[0],
        MarkPriceRow {
            instrument_name: "BTC-31JUL26-67000-C".into(),
            mark_price: 0.005,
            iv: 0.3706,
        }
    );
    assert_eq!(rows[1].instrument_name, "BTC-31JUL26-106000-P");
    assert_eq!(rows[1].iv, 0.6527);
    assert_eq!(rows[1].mark_price, 0.6376);
}

#[test]
fn wrong_channel_and_non_subscription_reject() {
    // a DVOL frame rides `subscription` too, but a different channel — must not be mistaken.
    let dvol = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "deribit_volatility_index.btc_usd", "data": {"volatility": 50.0}}
    });
    assert!(parse_markprice_options(&dvol).is_none());
    // a JSON-RPC subscribe response is not a `subscription` notification.
    let resp = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": "ok"});
    assert!(parse_markprice_options(&resp).is_none());
}

#[test]
fn malformed_row_skipped_not_fatal() {
    let v = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "markprice.options.btc_usd", "data": [
            {"instrument_name": "BTC-31JUL26-67000-C", "mark_price": 0.005, "iv": 0.37},
            {"instrument_name": "BAD-NO-MARK", "iv": 0.4},
            {"mark_price": 0.1, "iv": 0.4}
        ]}
    });
    let rows = parse_markprice_options(&v).unwrap();
    assert_eq!(rows.len(), 1, "only the complete row survives; bad rows skipped, frame kept");
    assert_eq!(rows[0].instrument_name, "BTC-31JUL26-67000-C");
}

#[test]
fn empty_delta_is_a_valid_frame() {
    let v = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "markprice.options.eth_usd", "data": []}
    });
    assert_eq!(parse_markprice_options(&v).unwrap().len(), 0);
}

#[test]
fn on_frame_confirms_valid_ignores_junk() {
    let mut batches: Vec<Vec<MarkPriceRow>> = Vec::new();
    let mut sink = |rows: Vec<MarkPriceRow>| batches.push(rows);
    assert_eq!(on_markprice_frame(FRAME, &mut sink), FrameOutcome::Confirm);
    assert_eq!(on_markprice_frame("not json", &mut sink), FrameOutcome::Ignore);
    assert_eq!(
        on_markprice_frame(r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#, &mut sink),
        FrameOutcome::Ignore
    );
    assert_eq!(batches.len(), 1, "only the one valid frame delivered a batch");
    assert_eq!(batches[0].len(), 2);
}
