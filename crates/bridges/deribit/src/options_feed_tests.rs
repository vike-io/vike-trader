use super::*;

/// Captured verbatim from mainnet (2026-07-26), trimmed to two rows. `iv` DECIMAL, `mark_price`
/// COIN units — the units this whole feed exists to carry faithfully.
const FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"markprice.options.btc_usd","data":[{"timestamp":1785079023870,"iv":0.3706,"instrument_name":"BTC-31JUL26-67000-C","mark_price":0.005},{"timestamp":1785079023870,"iv":0.6527,"instrument_name":"BTC-31JUL26-106000-P","mark_price":0.6376}]}}"#;

/// A receive time distinct from every wire `timestamp`, so a test can tell which one `ts` took.
const RECV: i64 = 1_785_079_999_999;

#[test]
fn channel_builder_lowercases() {
    assert_eq!(markprice_options_channel("btc_usd"), "markprice.options.btc_usd");
    assert_eq!(markprice_options_channel("ETH_USD"), "markprice.options.eth_usd");
}

#[test]
fn parses_captured_frame_iv_decimal_mark_coin() {
    let v: Value = serde_json::from_str(FRAME).unwrap();
    let rows = parse_markprice_options(&v, RECV).unwrap();
    assert_eq!(rows.len(), 2);
    // iv stored VERBATIM (decimal, no ÷100); mark_price left in COIN units (scaled downstream).
    let mut want = OptionTicker::new(1_785_079_023_870, "BTC-31JUL26-67000-C");
    want.mark = Some(0.005);
    want.mark_iv = Some(0.3706);
    assert_eq!(rows[0], want);
    assert_eq!(rows[1].symbol, "BTC-31JUL26-106000-P");
    assert_eq!(rows[1].mark_iv, Some(0.6527));
    assert_eq!(rows[1].mark, Some(0.6376));
}

/// The converted row, field by field, for the captured frame: `symbol` is the venue's own name (no
/// `.DERIBIT` suffix), `ts` is the row's wire `timestamp` (not the receive time), `mark_iv` is the
/// wire's decimal verbatim, and every field the channel does not carry is `None`.
#[test]
fn markprice_row_is_an_option_ticker_with_wire_ts_venue_name_and_only_mark_and_iv() {
    let v: Value = serde_json::from_str(FRAME).unwrap();
    let rows = parse_markprice_options(&v, RECV).unwrap();
    let r = &rows[1];
    assert_eq!(r.symbol, "BTC-31JUL26-106000-P", "venue name verbatim, not SYMBOL.VENUE");
    assert_eq!(r.ts, 1_785_079_023_870, "ts is the wire timestamp when the row carries one");
    assert_eq!(r.mark, Some(0.6376));
    assert_eq!(r.mark_iv, Some(0.6527), "decimal on this channel: no ÷100");
    let mut want = OptionTicker::new(r.ts, r.symbol.clone());
    want.mark = r.mark;
    want.mark_iv = r.mark_iv;
    assert_eq!(*r, want, "no field beyond mark/mark_iv is filled — bid/ask/greeks stay None");
}

#[test]
fn markprice_row_without_a_timestamp_takes_the_receive_time() {
    let v = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "markprice.options.btc_usd", "data": [
            {"instrument_name": "BTC-31JUL26-67000-C", "mark_price": 0.005, "iv": 0.37}
        ]}
    });
    let rows = parse_markprice_options(&v, RECV).unwrap();
    assert_eq!(rows[0].ts, RECV);
}

#[test]
fn wrong_channel_and_non_subscription_reject() {
    // a DVOL frame rides `subscription` too, but a different channel — must not be mistaken.
    let dvol = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "deribit_volatility_index.btc_usd", "data": {"volatility": 50.0}}
    });
    assert!(parse_markprice_options(&dvol, RECV).is_none());
    // a JSON-RPC subscribe response is not a `subscription` notification.
    let resp = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": "ok"});
    assert!(parse_markprice_options(&resp, RECV).is_none());
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
    let rows = parse_markprice_options(&v, RECV).unwrap();
    assert_eq!(rows.len(), 1, "only the complete row survives; bad rows skipped, frame kept");
    assert_eq!(rows[0].symbol, "BTC-31JUL26-67000-C");
}

#[test]
fn empty_delta_is_a_valid_frame() {
    let v = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "markprice.options.eth_usd", "data": []}
    });
    assert_eq!(parse_markprice_options(&v, RECV).unwrap().len(), 0);
}

#[test]
fn on_frame_confirms_valid_ignores_junk() {
    let mut batches: Vec<Vec<OptionTicker>> = Vec::new();
    let mut sink = |rows: Vec<OptionTicker>| batches.push(rows);
    assert_eq!(on_markprice_frame(FRAME, RECV, &mut sink), FrameOutcome::Confirm);
    assert_eq!(on_markprice_frame("not json", RECV, &mut sink), FrameOutcome::Ignore);
    assert_eq!(
        on_markprice_frame(r#"{"jsonrpc":"2.0","id":1,"result":"ok"}"#, RECV, &mut sink),
        FrameOutcome::Ignore
    );
    assert_eq!(batches.len(), 1, "only the one valid frame delivered a batch");
    assert_eq!(batches[0].len(), 2);
}
