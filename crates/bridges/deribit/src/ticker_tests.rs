use super::*;

/// Captured verbatim from mainnet (2026-07-26). `best_bid_price`/`best_ask_price`/`mark_price`
/// COIN units; `mark_iv` a PERCENT; `volume` nested under `stats` — the exact wire this feed
/// exists to carry faithfully. `params.data` is a SINGLE OBJECT (not the markprice array). The
/// capture carries no `timestamp`, so its row's `ts` is the receive time.
const TICKER_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"ticker.BTC-27JUL26-58000-C.100ms","data":{"instrument_name":"BTC-27JUL26-58000-C","best_bid_price":0.1015,"best_ask_price":0.106,"mark_price":0.1036,"mark_iv":66.33,"open_interest":0.0,"stats":{"volume":0.0},"underlying_price":64701.57,"greeks":{"delta":0.99992,"gamma":0.0,"vega":0.00863,"theta":-0.2863,"rho":1.10672}}}}"#;

/// A receive time distinct from every wire `timestamp`, so a test can tell which one `ts` took.
const RECV: i64 = 1_785_079_999_999;

#[test]
fn channel_builder_is_verbatim_instrument_dot_interval() {
    assert_eq!(ticker_channel("BTC-27JUL26-58000-C", "100ms"), "ticker.BTC-27JUL26-58000-C.100ms");
    // instrument ids are case-SENSITIVE — passed through verbatim (unlike the index channel).
    assert_eq!(ticker_channel("SOL_USDC-25SEP26-45-P", "raw"), "ticker.SOL_USDC-25SEP26-45-P.raw");
}

#[test]
fn parses_captured_frame_bidask_coin_iv_percent_volume_nested() {
    let v: Value = serde_json::from_str(TICKER_FRAME).unwrap();
    let row = parse_ticker(&v, RECV).unwrap();
    let mut want = OptionTicker::new(RECV, "BTC-27JUL26-58000-C");
    want.bid = Some(0.1015); // COIN units — scaled to USD downstream, NOT here
    want.ask = Some(0.106);
    want.mark = Some(0.1036);
    want.mark_iv = Some(66.33 / 100.0); // wire PERCENT → decimal HERE (the one conversion)
    want.open_interest = Some(0.0);
    want.volume = Some(0.0); // read from the nested stats.volume
    want.underlying_price = Some(64701.57);
    // the nested `greeks` object, verbatim in Deribit's convention (no unit conversion)
    want.delta = Some(0.99992);
    want.gamma = Some(0.0); // a PRESENT zero is the wire's value, kept
    want.vega = Some(0.00863);
    want.theta = Some(-0.2863);
    want.rho = Some(1.10672);
    assert_eq!(row, want);
}

/// A ticker frame with EVERY key `parse_ticker` reads. ⚠ (per Deribit docs, no captured payload)
/// for `best_bid_amount`, `best_ask_amount`, `last_price`, `bid_iv`, `ask_iv` and `index_price`:
/// the captured [`TICKER_FRAME`] carries none of them, so their shape (top-level numbers, IVs as
/// percents like `mark_iv`) is the `ticker.{instrument_name}.{interval}` notification as Deribit's
/// public API reference documents it. The rest is the capture's values. The keys `OptionTicker`
/// has no field for (`stats.high`/`low`/`price_change`/`volume_usd`, `interest_rate`, `state`,
/// `settlement_price`, `min_price`/`max_price`, `underlying_index`, `estimated_delivery_price`)
/// ride along to prove they are ignored, not mistaken for another field.
const TICKER_FRAME_FULL: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"ticker.BTC-27JUL26-58000-C.100ms","data":{"timestamp":1785079023870,"instrument_name":"BTC-27JUL26-58000-C","state":"open","best_bid_price":0.1015,"best_bid_amount":12.5,"best_ask_price":0.106,"best_ask_amount":3.0,"last_price":0.104,"mark_price":0.1036,"mark_iv":66.33,"bid_iv":61.2,"ask_iv":71.8,"open_interest":0.0,"stats":{"volume":4.2,"volume_usd":27000.0,"high":0.11,"low":0.1,"price_change":-1.5},"underlying_price":64701.57,"underlying_index":"BTC-27JUL26","index_price":64688.12,"interest_rate":0.0,"settlement_price":0.1,"min_price":0.0745,"max_price":0.137,"estimated_delivery_price":64688.12,"greeks":{"delta":0.99992,"gamma":0.0,"vega":0.00863,"theta":-0.2863,"rho":1.10672}}}}"#;

#[test]
fn every_field_the_frame_carries_lands_in_the_row() {
    let v: Value = serde_json::from_str(TICKER_FRAME_FULL).unwrap();
    let row = parse_ticker(&v, RECV).unwrap();
    let want = OptionTicker {
        ts: 1_785_079_023_870, // the wire timestamp, not RECV
        symbol: "BTC-27JUL26-58000-C".to_string(),
        bid: Some(0.1015),
        ask: Some(0.106),
        bid_size: Some(12.5),
        ask_size: Some(3.0),
        last: Some(0.104),
        mark: Some(0.1036),
        mark_iv: Some(66.33 / 100.0),
        bid_iv: Some(61.2 / 100.0), // percent → decimal, the same ONE conversion as mark_iv
        ask_iv: Some(71.8 / 100.0),
        delta: Some(0.99992),
        gamma: Some(0.0),
        vega: Some(0.00863),  // per vol point, verbatim
        theta: Some(-0.2863), // per calendar day, verbatim
        rho: Some(1.10672),
        open_interest: Some(0.0),
        volume: Some(4.2), // stats.volume, not stats.volume_usd
        underlying_price: Some(64701.57),
        index_price: Some(64688.12),
    };
    assert_eq!(row, want);
}

#[test]
fn null_last_price_and_absent_or_partial_greeks_stay_none() {
    // before an option's first trade `last_price` is `null`; a frame may omit the greeks object or
    // carry only some of its keys — every absent value stays None, never a fabricated 0.0.
    let v = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "ticker.BTC-27JUL26-58000-C.100ms", "data": {
            "instrument_name": "BTC-27JUL26-58000-C", "last_price": null, "bid_iv": 0.0,
            "greeks": {"delta": 0.5}
        }}
    });
    let row = parse_ticker(&v, RECV).unwrap();
    let mut want = OptionTicker::new(RECV, "BTC-27JUL26-58000-C");
    want.bid_iv = Some(0.0); // present zero kept as sent
    want.delta = Some(0.5);
    assert_eq!(row, want, "null last and missing greeks are None");
    // a greeks value of the wrong type is absent, not a panic
    let junk = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "ticker.X.100ms", "data": {"instrument_name": "X", "greeks": [1, 2]}}
    });
    let row = parse_ticker(&junk, RECV).unwrap();
    assert_eq!((row.delta, row.vega, row.theta), (None, None, None));
}

/// The converted row's exact values for the captured frame: the wire's percent `66.33` lands as the
/// decimal `0.6633`, `ts` falls back to the receive time (the capture has no `timestamp`), and
/// `symbol` is the venue's own name verbatim.
#[test]
fn ticker_row_iv_is_decimal_symbol_is_the_venue_name_ts_is_receive_time_without_a_timestamp() {
    let v: Value = serde_json::from_str(TICKER_FRAME).unwrap();
    let row = parse_ticker(&v, RECV).unwrap();
    assert_eq!(row.mark_iv, Some(0.6633), "66.33 % → 0.6633");
    assert_eq!(row.symbol, "BTC-27JUL26-58000-C", "venue name verbatim, not SYMBOL.VENUE");
    assert_eq!(row.ts, RECV, "no wire timestamp → the receive time");
    // the frame's greeks are decoded verbatim; the keys the capture lacks stay absent
    assert_eq!(row.theta, Some(-0.2863), "theta per day, verbatim");
    assert_eq!(
        (row.bid_size, row.ask_size, row.last, row.bid_iv, row.ask_iv, row.index_price),
        (None, None, None, None, None, None)
    );
}

#[test]
fn ticker_row_takes_the_wire_timestamp_when_the_frame_carries_one() {
    let v = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "ticker.BTC-27JUL26-58000-C.100ms", "data": {
            "instrument_name": "BTC-27JUL26-58000-C", "timestamp": 1_785_079_023_870_i64,
            "mark_iv": 50.0
        }}
    });
    let row = parse_ticker(&v, RECV).unwrap();
    assert_eq!(row.ts, 1_785_079_023_870);
    assert_eq!(row.mark_iv, Some(0.5));
}

#[test]
fn wrong_channel_and_non_subscription_and_missing_name_reject() {
    // markprice.options rides `subscription` too, with an ARRAY data — must not be mistaken.
    let mp = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "markprice.options.btc_usd",
                   "data": [{"instrument_name": "X", "mark_price": 0.1, "iv": 0.4}]}
    });
    assert!(parse_ticker(&mp, RECV).is_none());
    // a JSON-RPC subscribe response is not a `subscription` notification.
    let resp = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": ["ticker.BTC-27JUL26-58000-C.100ms"]});
    assert!(parse_ticker(&resp, RECV).is_none());
    // a ticker frame whose data carries no instrument_name has no grid join key → not a row.
    let noname = serde_json::json!({
        "method": "subscription",
        "params": {"channel": "ticker.BTC-27JUL26-58000-C.100ms", "data": {"best_bid_price": 0.1}}
    });
    assert!(parse_ticker(&noname, RECV).is_none());
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
    let row = parse_ticker(&v, RECV).unwrap();
    assert_eq!(row.bid, None, "absent bid stays None");
    assert_eq!(row.ask, Some(0.11));
    assert_eq!(row.volume, None, "no stats object → None volume, not 0.0");
    assert_eq!(row.mark_iv, None);
    // and the whole row: only what the frame carried is set
    let mut want = OptionTicker::new(RECV, "BTC-27JUL26-58000-C");
    want.ask = Some(0.11);
    want.mark = Some(0.1);
    assert_eq!(row, want, "every absent field is None, not 0.0");
}

#[test]
fn on_frame_confirms_valid_ignores_junk() {
    let mut rows: Vec<OptionTicker> = Vec::new();
    let mut sink = |r: OptionTicker| rows.push(r);
    assert_eq!(on_ticker_frame(TICKER_FRAME, RECV, &mut sink), FrameOutcome::Confirm);
    assert_eq!(on_ticker_frame("not json", RECV, &mut sink), FrameOutcome::Ignore);
    assert_eq!(
        on_ticker_frame(r#"{"jsonrpc":"2.0","id":1,"result":["ok"]}"#, RECV, &mut sink),
        FrameOutcome::Ignore
    );
    assert_eq!(rows.len(), 1, "only the one valid frame delivered a row");
    assert_eq!(rows[0].symbol, "BTC-27JUL26-58000-C");
}
