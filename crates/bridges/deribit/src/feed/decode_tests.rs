use super::*;
use serde_json::json;
use std::assert_matches;

/// A wire-faithful `book.*` SNAPSHOT frame (documented shape: `[action, price, amount]`
/// triplets as JSON NUMBERS, `change_id` the anchor).
fn snapshot_frame(change_id: u64, bids: &[L2BookLevel], asks: &[L2BookLevel]) -> String {
    json!({
            "jsonrpc": "2.0", "method": "subscription",
            "params": {"channel": "book.BTC-PERPETUAL.100ms", "data": {
                "type": "snapshot", "timestamp": 1_700_000_000_000i64,
                "instrument_name": "BTC-PERPETUAL", "change_id": change_id,
                "bids": bids.iter().map(|&L2BookLevel { price: p, qty: q }| json!(["new", p, q])).collect::<Vec<_>>(),
                "asks": asks.iter().map(|&L2BookLevel { price: p, qty: q }| json!(["new", p, q])).collect::<Vec<_>>(),
            }}
        })
        .to_string()
}

/// A wire-faithful `book.*` CHANGE frame (`prev_change_id` chains to the previous frame).
fn change_frame(previous: u64, change_id: u64, bids: &[serde_json::Value]) -> String {
    json!({
        "jsonrpc": "2.0", "method": "subscription",
        "params": {"channel": "book.BTC-PERPETUAL.100ms", "data": {
            "type": "change", "timestamp": 1_700_000_000_100i64,
            "instrument_name": "BTC-PERPETUAL",
            "prev_change_id": previous, "change_id": change_id,
            "bids": bids, "asks": [],
        }}
    })
    .to_string()
}

#[test]
fn channel_builders_are_verbatim() {
    assert_eq!(book_channel("BTC-PERPETUAL"), "book.BTC-PERPETUAL.100ms");
    assert_eq!(trades_channel("BTC-PERPETUAL"), "trades.BTC-PERPETUAL.100ms");
    assert_eq!(quote_channel("ETH-PERPETUAL"), "quote.ETH-PERPETUAL");
    assert_eq!(chart_channel("BTC-PERPETUAL", "1"), "chart.trades.BTC-PERPETUAL.1");
}

#[test]
fn subscribe_frame_is_a_public_subscribe_rpc() {
    let sub = public_subscribe_frame(&["book.BTC-PERPETUAL.100ms".to_string()]);
    let v: Value = serde_json::from_str(&sub).unwrap();
    assert_eq!(v["method"], "public/subscribe");
    assert_eq!(v["params"]["channels"][0], "book.BTC-PERPETUAL.100ms");
}

// ── book: the DeltaSync chain ───────────────────────────────────────────────────────────────

#[test]
fn snapshot_seeds_the_book_at_change_id() {
    let mut book = L2Book::new(0.5);
    let event = route_frame(
        &snapshot_frame(
            297_000,
            &[L2BookLevel::new(60_000.0, 5.0), L2BookLevel::new(59_999.5, 2.0)],
            &[L2BookLevel::new(60_000.5, 4.0)],
        ),
        "BTC-PERPETUAL",
        &mut book,
    );
    assert_eq!(event, DataClientEvent::BookUpdated { timestamp_ms: 1_700_000_000_000 });
    assert_eq!(book.last_sequence, 297_000);
    assert_eq!(book.best_bid(), Some(L2BookLevel::new(60_000.0, 5.0)));
    assert_eq!(book.best_ask(), Some(L2BookLevel::new(60_000.5, 4.0)));
}

#[test]
fn a_chained_change_folds_and_advances_the_anchor() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(
            100,
            &[L2BookLevel::new(60_000.0, 5.0)],
            &[L2BookLevel::new(60_000.5, 4.0)],
        ),
        "s",
        &mut book,
    );
    // change_id values JUMP (venue-global) — only prev_change_id must chain.
    let event =
        route_frame(&change_frame(100, 137, &[json!(["change", 60_000.0, 8.0])]), "s", &mut book);
    assert_eq!(event, DataClientEvent::BookUpdated { timestamp_ms: 1_700_000_000_100 });
    assert_eq!(book.last_sequence, 137);
    assert_eq!(book.best_bid(), Some(L2BookLevel::new(60_000.0, 8.0)));
}

#[test]
fn a_delete_action_removes_the_level() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(
            100,
            &[L2BookLevel::new(60_000.0, 5.0), L2BookLevel::new(59_999.5, 2.0)],
            &[L2BookLevel::new(60_000.5, 4.0)],
        ),
        "s",
        &mut book,
    );
    // The documented delete shape: amount 0 on the wire; the decoder forces 0.0 regardless.
    let event =
        route_frame(&change_frame(100, 101, &[json!(["delete", 60_000.0, 0.0])]), "s", &mut book);
    assert_eq!(event, DataClientEvent::BookUpdated { timestamp_ms: 1_700_000_000_100 });
    assert_eq!(
        book.best_bid(),
        Some(L2BookLevel::new(59_999.5, 2.0)),
        "the deleted top level is gone"
    );
}

#[test]
fn a_replayed_change_is_ignored_and_the_book_untouched() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(
            100,
            &[L2BookLevel::new(60_000.0, 5.0)],
            &[L2BookLevel::new(60_000.5, 4.0)],
        ),
        "s",
        &mut book,
    );
    route_frame(&change_frame(100, 137, &[json!(["change", 60_000.0, 8.0])]), "s", &mut book);
    // The SAME frame again: change_id already reflected → Ignored (NOT Resync, even though its
    // prev_change_id no longer matches the advanced anchor — the check-order rule).
    let event =
        route_frame(&change_frame(100, 137, &[json!(["change", 60_000.0, 999.0])]), "s", &mut book);
    assert_eq!(event, DataClientEvent::Ignored);
    assert_eq!(book.last_sequence, 137, "anchor unmoved");
    assert_eq!(book.best_bid(), Some(L2BookLevel::new(60_000.0, 8.0)), "book unmoved");
}

#[test]
fn a_broken_chain_answers_resync_and_does_not_fold() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(
            100,
            &[L2BookLevel::new(60_000.0, 5.0)],
            &[L2BookLevel::new(60_000.5, 4.0)],
        ),
        "s",
        &mut book,
    );
    // prev_change_id 104 != anchor 100 — the venue dropped a message between them.
    let event =
        route_frame(&change_frame(104, 105, &[json!(["change", 60_000.0, 999.0])]), "s", &mut book);
    assert_eq!(event, DataClientEvent::Resync);
    assert_eq!(book.last_sequence, 100, "anchor unmoved across the gap");
    assert_eq!(
        book.best_bid(),
        Some(L2BookLevel::new(60_000.0, 5.0)),
        "the gapped delta was NOT folded"
    );
}

#[test]
fn a_post_gap_snapshot_reseeds_cleanly() {
    let mut book = L2Book::new(0.5);
    route_frame(
        &snapshot_frame(
            100,
            &[L2BookLevel::new(60_000.0, 5.0)],
            &[L2BookLevel::new(60_000.5, 4.0)],
        ),
        "s",
        &mut book,
    );
    route_frame(&change_frame(104, 105, &[]), "s", &mut book); // gap
    let event = route_frame(
        &snapshot_frame(
            300,
            &[L2BookLevel::new(60_000.5, 3.0)],
            &[L2BookLevel::new(60_001.0, 4.0)],
        ),
        "s",
        &mut book,
    );
    assert_eq!(event, DataClientEvent::BookUpdated { timestamp_ms: 1_700_000_000_000 });
    assert_eq!(book.last_sequence, 300);
    assert_eq!(book.best_bid(), Some(L2BookLevel::new(60_000.5, 3.0)));
}

#[test]
fn parse_book_snapshot_answers_only_snapshots() {
    let frame = snapshot_frame(
        297_000,
        &[L2BookLevel::new(60_000.0, 5.0)],
        &[L2BookLevel::new(60_000.5, 4.0)],
    );
    let (seq, bids, asks) = parse_book_snapshot(&serde_json::from_str(&frame).unwrap()).unwrap();
    assert_eq!(seq, 297_000);
    assert_eq!(bids, vec![L2BookLevel::new(60_000.0, 5.0)]);
    assert_eq!(asks, vec![L2BookLevel::new(60_000.5, 4.0)]);
    let change = change_frame(297_000, 297_001, &[]);
    assert!(parse_book_snapshot(&serde_json::from_str(&change).unwrap()).is_none());
}

// ── trades ──────────────────────────────────────────────────────────────────────────────────

/// Documented `trades.*` shape: an ARRAY of rows; `direction` is the TAKER side; `amount` the
/// venue contract unit (USD on the inverse perp).
const TRADES_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"trades.BTC-PERPETUAL.100ms","data":[{"trade_seq":30289432,"trade_id":"48079254","timestamp":1590484512188,"tick_direction":2,"price":8950.0,"mark_price":8948.9,"instrument_name":"BTC-PERPETUAL","index_price":8955.88,"direction":"sell","amount":10.0},{"trade_seq":30289433,"trade_id":"48079255","timestamp":1590484512188,"tick_direction":2,"price":8949.5,"mark_price":8948.9,"instrument_name":"BTC-PERPETUAL","index_price":8955.88,"direction":"buy","amount":20.0}]}}"#;

#[test]
fn trades_frame_decodes_every_row_with_the_taker_side_mapping() {
    let mut book = L2Book::new(0.5);
    let DataClientEvent::Trades(rows) = route_frame(TRADES_FRAME, "BTC-PERPETUAL", &mut book)
    else {
        panic!("expected Trades");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].price, 8950.0);
    assert_eq!(rows[0].size, 10.0, "amount verbatim — venue contract units");
    assert!(rows[0].is_buyer_maker, "taker sold → buyer was the maker");
    assert!(!rows[1].is_buyer_maker, "taker bought");
    assert_eq!(rows[0].timestamp, 1_590_484_512_188);
    assert_eq!(rows[0].symbol, "BTC-PERPETUAL");
    assert_eq!(rows[0].local_timestamp, 0, "receive stamp is the pump's job");
}

#[test]
fn a_malformed_trade_row_is_skipped_not_fatal() {
    let frame = json!({
        "jsonrpc": "2.0", "method": "subscription",
        "params": {"channel": "trades.BTC-PERPETUAL.100ms", "data": [
            {"instrument_name": "BTC-PERPETUAL", "price": 8950.0, "amount": 10.0,
             "direction": "sell", "timestamp": 1i64},
            {"instrument_name": "BTC-PERPETUAL", "price": 0.0, "amount": 10.0,
             "direction": "sell", "timestamp": 2i64},
            {"instrument_name": "BTC-PERPETUAL", "price": 8950.0, "amount": 10.0,
             "direction": "??", "timestamp": 3i64}
        ]}
    })
    .to_string();
    let mut book = L2Book::new(0.5);
    let DataClientEvent::Trades(rows) = route_frame(&frame, "s", &mut book) else {
        panic!("expected Trades");
    };
    assert_eq!(rows.len(), 1, "zero-price and unknown-direction rows dropped in place");
}

// ── quotes ──────────────────────────────────────────────────────────────────────────────────

/// Documented `quote.*` shape: a SINGLE object, both sides + amounts, `timestamp` epoch-ms.
const QUOTE_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"quote.BTC-PERPETUAL","data":{"timestamp":1550658624149,"instrument_name":"BTC-PERPETUAL","best_bid_price":3914.97,"best_bid_amount":40.0,"best_ask_price":3915.5,"best_ask_amount":50.0}}}"#;

#[test]
fn quote_frame_decodes_to_a_two_sided_quote() {
    let mut book = L2Book::new(0.5);
    let DataClientEvent::Quote(q) = route_frame(QUOTE_FRAME, "BTC-PERPETUAL", &mut book) else {
        panic!("expected Quote");
    };
    assert_eq!((q.bid, q.bid_size, q.ask, q.ask_size), (3914.97, 40.0, 3915.5, 50.0));
    assert_eq!(q.timestamp, 1_550_658_624_149);
    assert_eq!(q.symbol, "BTC-PERPETUAL");
    assert_eq!(q.local_timestamp, 0);
}

#[test]
fn a_one_sided_quote_is_ignored_never_fabricated() {
    let frame = json!({
        "jsonrpc": "2.0", "method": "subscription",
        "params": {"channel": "quote.BTC-PERPETUAL", "data": {
            "timestamp": 1i64, "instrument_name": "BTC-PERPETUAL",
            "best_bid_price": 3914.97, "best_bid_amount": 40.0
        }}
    })
    .to_string();
    let mut book = L2Book::new(0.5);
    assert_eq!(route_frame(&frame, "s", &mut book), DataClientEvent::Ignored);
}

// ── chart bars ──────────────────────────────────────────────────────────────────────────────

/// Documented `chart.trades.*` shape: `tick` = bar-open ms, `volume` BASE units, `cost` quote
/// notional (unread — the REST reader's column choice, pinned here too).
const CHART_FRAME: &str = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"chart.trades.BTC-PERPETUAL.1","data":{"volume":0.05219351,"tick":1573645080000,"open":8869.79,"low":8788.25,"high":8870.31,"cost":463.0,"close":8791.25}}}"#;

#[test]
fn chart_frame_decodes_volume_base_not_cost() {
    let bar = parse_chart_bar(&serde_json::from_str(CHART_FRAME).unwrap()).unwrap();
    assert_eq!(bar.timestamp, 1_573_645_080_000);
    assert_eq!((bar.open, bar.high, bar.low, bar.close), (8869.79, 8870.31, 8788.25, 8791.25));
    assert_eq!(bar.volume, 0.05219351, "volume = BASE units; cost (463.0) must NOT be read");
}

#[test]
fn chart_parse_rejects_other_channels_and_replies() {
    assert!(parse_chart_bar(&serde_json::from_str::<Value>(QUOTE_FRAME).unwrap()).is_none());
    let reply = json!({"jsonrpc": "2.0", "id": 1, "result": ["chart.trades.BTC-PERPETUAL.1"]});
    assert!(parse_chart_bar(&reply).is_none());
}

// ── the JSON-RPC reply lane ─────────────────────────────────────────────────────────────────

#[test]
fn rpc_replies_classify_ack_error_empty_and_keepalive() {
    let ack = json!({"jsonrpc": "2.0", "id": 1, "result": ["book.BTC-PERPETUAL.100ms"]});
    assert_eq!(classify_rpc_reply(&ack), FeedRpcReply::Ack);
    let empty = json!({"jsonrpc": "2.0", "id": 1, "result": []});
    assert_matches!(
        classify_rpc_reply(&empty),
        FeedRpcReply::Error(_),
        "an empty subscribe result is a dead subscription, not a success"
    );
    let err = json!({"jsonrpc": "2.0", "id": 1,
            "error": {"code": -32602, "message": "Invalid params"}});
    let FeedRpcReply::Error(msg) = classify_rpc_reply(&err) else { panic!("expected Error") };
    assert!(msg.contains("-32602") && msg.contains("Invalid params"));
    // the public/test keepalive reply: a result OBJECT — activity, not an ack.
    let ping = json!({"jsonrpc": "2.0", "id": 9929, "result": {"version": "1.2.26"}});
    assert_eq!(classify_rpc_reply(&ping), FeedRpcReply::Other);
    // a subscription notification is not a reply.
    let notif: Value = serde_json::from_str(QUOTE_FRAME).unwrap();
    assert_eq!(classify_rpc_reply(&notif), FeedRpcReply::NotReply);
}

#[test]
fn junk_and_unknown_channels_are_ignored() {
    let mut book = L2Book::new(0.5);
    assert_eq!(route_frame("not json", "s", &mut book), DataClientEvent::Ignored);
    assert_eq!(
        route_frame(
            r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"deribit_price_index.btc_usd","data":{"price":60000.0}}}"#,
            "s",
            &mut book
        ),
        DataClientEvent::Ignored
    );
    // a reply frame through the book router: Ignored (the reply lane classifies it).
    assert_eq!(
        route_frame(r#"{"jsonrpc":"2.0","id":1,"result":["x"]}"#, "s", &mut book),
        DataClientEvent::Ignored
    );
}

/// **A perpetual's ticker, RECORDED on 2026-11-08** (`ticker.BTC-PERPETUAL.agg2`, the public mainnet socket,
/// no key): `current_funding` is the 8-hour rate in force, and the FIRST ticker a feed sees stands for its
/// whole hour, so it is settled hourly at one eighth (the model `parse_ticker_funding` states); the
/// settlement is the next whole hour after the ticker's own stamp, and the price is the mark's. The
/// recorded rate is `0.0` (the venue's dead band); a stated rate is divided the same way. Any other
/// channel states nothing.
#[test]
fn a_recorded_perpetual_ticker_states_its_funding_settled_hourly() {
    let recorded = r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"ticker.BTC-PERPETUAL.agg2","data":{"timestamp":1794153424816,"state":"open","stats":{"high":76103.5,"low":75485.5,"price_change":0.4236,"volume":7187.09513624,"volume_usd":544654580.0,"volume_notional":544654580.0},"index_price":75973.68,"instrument_name":"BTC-PERPETUAL","last_price":75982.5,"settlement_price":75893.25,"min_price":74841.0,"max_price":77121.0,"open_interest":696832680,"mark_price":75980.96,"interest_value":0.001131853782497749,"current_funding":0.0,"estimated_delivery_price":75973.68,"funding_8h":3.3e-6,"best_ask_price":75982.5,"best_bid_price":75982.0,"best_ask_amount":121260.0,"best_bid_amount":74210.0}}}"#;
    let v: Value = serde_json::from_str(recorded).unwrap();
    let f = parse_ticker_funding(&v, "BTC-PERPETUAL", &mut FundingIntegral::new())
        .expect("a perpetual's ticker");
    assert_eq!(
        f,
        FundingRate {
            timestamp: 1_794_153_424_816,
            symbol: "BTC-PERPETUAL".to_string(),
            rate: 0.0,
            interval_ms: Some(3_600_000),
            next_timestamp: Some(1_794_153_600_000),
            predicted: None,
            price: None,
        }
    );
    let stated: Value = serde_json::from_str(
        &recorded.replace(r#""current_funding":0.0"#, r#""current_funding":0.0008"#),
    )
    .unwrap();
    let rate =
        parse_ticker_funding(&stated, "BTC-PERPETUAL", &mut FundingIntegral::new()).unwrap().rate;
    assert!((rate - 0.0001).abs() < 1e-18, "{rate}");
    assert_eq!(ticker_channel("BTC-PERPETUAL"), "ticker.BTC-PERPETUAL.agg2");
    let chart: Value = serde_json::from_str(
        r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"chart.trades.BTC-PERPETUAL.1","data":{"tick":1,"open":1.0,"high":1.0,"low":1.0,"close":1.0,"volume":0.0}}}"#,
    )
    .unwrap();
    assert_eq!(parse_ticker_funding(&chart, "BTC-PERPETUAL", &mut FundingIntegral::new()), None);
}

/// The hour's funding is the stated 8-hour rate integrated over the hour, each rate held until the next:
/// a rate that holds all hour is one eighth of it; a rate that changes mid-hour is time-weighted (the
/// current rate held to the hour's end until the last ticker makes it the whole hour); a new hour starts
/// with the rate in force across its start; an older ticker states nothing.
#[test]
fn the_hours_funding_is_the_stated_rate_integrated_over_the_hour() {
    const H: i64 = 3_600_000;
    let h0 = 1_794_160_800_000; // a whole hour
    let near = |got: Option<f64>, want: f64| {
        let got = got.expect("a rate");
        assert!((got - want).abs() < 1e-18, "{got} vs {want}");
    };
    let mut f = FundingIntegral::new();
    // the first ticker, 15 min into the hour, stands for the whole hour so far
    near(f.observe(h0 + H / 4, 8e-4), 1e-4);
    // the rate halves at the half hour: 30 min at 8e-4 and 30 min (held to the end) at 4e-4
    near(f.observe(h0 + H / 2, 4e-4), (8e-4 * 0.5 + 4e-4 * 0.5) / 8.0);
    // a ticker older than the last one states nothing and changes nothing
    assert_eq!(f.observe(h0 + H / 4, 1.0), None);
    // the last ticker before the hour: 4e-4 held from the half hour to here, then 0 for the last second
    near(f.observe(h0 + H - 1_000, 0.0), (8e-4 * 1_800_000.0 + 4e-4 * 1_799_000.0) / 28_800_000.0);
    // the next hour opens with the rate in force across its start (0.0), then 2e-4 from 6 min in
    near(f.observe(h0 + H + H / 10, 2e-4), 2e-4 * 0.9 / 8.0);
    // a gap of several hours (a reconnect) holds the last rate across the new hour's start
    near(f.observe(h0 + 5 * H + H / 2, 0.0), 2e-4 * 0.5 / 8.0);
    // through the frame parser: the settlement is the next whole hour, the rate the integral
    let frame = |ts: i64, cf: f64| -> Value {
        json!({"jsonrpc": "2.0", "method": "subscription", "params": {
            "channel": "ticker.ETH-PERPETUAL.agg2",
            "data": {"timestamp": ts, "current_funding": cf, "instrument_name": "ETH-PERPETUAL"}}})
    };
    let mut g = FundingIntegral::new();
    let first = parse_ticker_funding(&frame(h0 + 10, 8e-4), "ETH-PERPETUAL", &mut g).unwrap();
    assert_eq!(first.next_timestamp, Some(h0 + H));
    near(Some(first.rate), 1e-4);
    let later = parse_ticker_funding(&frame(h0 + H / 2, 0.0), "ETH-PERPETUAL", &mut g).unwrap();
    assert_eq!(later.next_timestamp, Some(h0 + H));
    near(Some(later.rate), 8e-4 * (H / 2) as f64 / 28_800_000.0);
    assert_eq!(parse_ticker_funding(&frame(h0, 1.0), "ETH-PERPETUAL", &mut g), None, "older");
}

/// **One hour of `ticker.BTC-PERPETUAL.agg2`, RECORDED on 2026-11-08** from 19:00 to 20:00 UTC (the public
/// mainnet socket, no key; the fixture's header says how it was reduced). Folded frame by frame, the last
/// observation before 20:00 settles that hour within 0.1% of what the venue published for it (`interest_1h`
/// of `public/get_funding_rate_history` at 20:00, in the fixture's header; it lands 0.014% off), where one
/// eighth of the last stated rate, the once-an-hour sample this replaced, missed it by 20%.
#[test]
fn a_recorded_hour_of_tickers_settles_at_the_venues_published_hourly_accrual() {
    const FIXTURE: &str =
        include_str!("../../tests/fixtures/deribit_ticker_funding_btc_perpetual_hour.csv");
    const HOUR_END: i64 = 1_794_168_000_000; // 2026-11-08 20:00 UTC
    const INTEREST_1H: f64 = 1.8729539954460736e-6;
    let mut integral = FundingIntegral::new();
    let (mut settled, mut last_rate_8h) = (None, f64::NAN);
    for row in FIXTURE.lines().filter(|l| !l.starts_with('#') && !l.starts_with("timestamp")) {
        let (ts, cf) = row.split_once(',').expect("timestamp,current_funding");
        let (ts, cf): (i64, f64) = (ts.parse().unwrap(), cf.parse().unwrap());
        let frame = json!({"jsonrpc": "2.0", "method": "subscription", "params": {
            "channel": "ticker.BTC-PERPETUAL.agg2", "data": {"timestamp": ts, "current_funding": cf}}});
        settled = parse_ticker_funding(&frame, "BTC-PERPETUAL", &mut integral);
        last_rate_8h = cf;
    }
    let settled = settled.expect("a recorded hour");
    assert_eq!(settled.next_timestamp, Some(HOUR_END));
    let miss = |rate: f64| (rate - INTEREST_1H).abs() / INTEREST_1H;
    assert!(miss(settled.rate) < 0.001, "integral {} vs interest_1h {INTEREST_1H}", settled.rate);
    assert!(miss(last_rate_8h / 8.0) > 0.15, "the hourly sample {}", last_rate_8h / 8.0);
}
