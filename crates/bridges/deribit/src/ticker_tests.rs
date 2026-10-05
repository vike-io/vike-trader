use super::*;

/// Captured verbatim from mainnet (2026-07-26). `best_bid_price`/`best_ask_price`/`mark_price`
/// COIN units; `mark_iv` a PERCENT; `volume` nested under `stats` — the exact wire this feed
/// exists to carry faithfully. `params.data` is a SINGLE OBJECT (not the markprice array).
const TICKER_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"ticker.BTC-27JUL26-58000-C.100ms","data":{"instrument_name":"BTC-27JUL26-58000-C","best_bid_price":0.1015,"best_ask_price":0.106,"mark_price":0.1036,"mark_iv":66.33,"open_interest":0.0,"stats":{"volume":0.0},"underlying_price":64701.57,"greeks":{"delta":0.99992,"gamma":0.0,"vega":0.00863,"theta":-0.2863,"rho":1.10672}}}}"#;

#[test]
fn channel_builder_is_verbatim_instrument_dot_interval() {
    assert_eq!(ticker_channel("BTC-27JUL26-58000-C", "100ms"), "ticker.BTC-27JUL26-58000-C.100ms");
    // instrument ids are case-SENSITIVE — passed through verbatim (unlike the index channel).
    assert_eq!(ticker_channel("SOL_USDC-25SEP26-45-P", "raw"), "ticker.SOL_USDC-25SEP26-45-P.raw");
}

#[test]
fn parses_captured_frame_bidask_coin_iv_percent_volume_nested() {
    let v: Value = serde_json::from_str(TICKER_FRAME).unwrap();
    let row = parse_ticker(&v).unwrap();
    assert_eq!(
        row,
        TickerRow {
            instrument_name: "BTC-27JUL26-58000-C".into(),
            best_bid: Some(0.1015), // COIN units — scaled to USD downstream, NOT here
            best_ask: Some(0.106),
            mark_price: Some(0.1036),
            mark_iv: Some(66.33), // PERCENT — ÷100 downstream, stored verbatim here
            open_interest: Some(0.0),
            volume: Some(0.0), // read from the nested stats.volume
            underlying_price: Some(64701.57),
        }
    );
}

#[test]
fn wrong_channel_and_non_subscription_and_missing_name_reject() {
    // markprice.options rides `subscription` too, with an ARRAY data — must not be mistaken.
    let mp = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "markprice.options.btc_usd",
                   "data": [{"instrument_name": "X", "mark_price": 0.1, "iv": 0.4}]}
    });
    assert!(parse_ticker(&mp).is_none());
    // a JSON-RPC subscribe response is not a `subscription` notification.
    let resp = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": ["ticker.BTC-27JUL26-58000-C.100ms"]});
    assert!(parse_ticker(&resp).is_none());
    // a ticker frame whose data carries no instrument_name has no grid join key → not a row.
    let noname = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "ticker.BTC-27JUL26-58000-C.100ms", "data": {"best_bid_price": 0.1}}
    });
    assert!(parse_ticker(&noname).is_none());
}

#[test]
fn absent_optional_fields_are_none_not_zero() {
    // a sparse ticker (no bid, no stats object) → absent stays absent, never a fabricated 0.0.
    let v = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "ticker.BTC-27JUL26-58000-C.100ms", "data": {
            "instrument_name": "BTC-27JUL26-58000-C", "best_ask_price": 0.11, "mark_price": 0.1
        }}
    });
    let row = parse_ticker(&v).unwrap();
    assert_eq!(row.best_bid, None, "absent bid stays None");
    assert_eq!(row.best_ask, Some(0.11));
    assert_eq!(row.volume, None, "no stats object → None volume, not 0.0");
    assert_eq!(row.mark_iv, None);
}

#[test]
fn on_frame_confirms_valid_ignores_junk() {
    let mut rows: Vec<TickerRow> = Vec::new();
    let mut sink = |r: TickerRow| rows.push(r);
    assert_eq!(on_ticker_frame(TICKER_FRAME, &mut sink), FrameOutcome::Confirm);
    assert_eq!(on_ticker_frame("not json", &mut sink), FrameOutcome::Ignore);
    assert_eq!(
        on_ticker_frame(r#"{"jsonrpc":"2.0","id":1,"result":["ok"]}"#, &mut sink),
        FrameOutcome::Ignore
    );
    assert_eq!(rows.len(), 1, "only the one valid frame delivered a row");
    assert_eq!(rows[0].instrument_name, "BTC-27JUL26-58000-C");
}
