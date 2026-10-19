//! Pure decoder fixtures for the cpapi MARKET-DATA client (decision 0140) — the no-network CI gate,
//! the market-data sibling of `tests/ibkr_cpapi_decode.rs`. Each payload is shaped after IBKR's
//! public Client Portal docs (the `smd` stream row, the snapshot row, the history body); ⚠ none is a
//! capture yet — `tests/ibkr_cpapi_mktdata_smoke.rs` is what reads the real rows, and a fixture
//! here that the gateway contradicts is corrected from that output, never the other way round.
#![cfg(feature = "ibkr-cpapi")]

use serde_json::json;
use vike_ibkr::cpapi_feed::decode::{
    PriceMark, SMD_FIELDS, SmdAccumulator, availability_delay, decode_history, history_bar_param,
    history_period, parse_md_price, parse_md_size, smd_conid, smd_subscribe, smd_unsubscribe,
    snapshot_fields, snapshot_row,
};
use vike_model::DataDelay;

const NOW: i64 = 1_700_000_000_123;

/// A stock row marked delayed (`6509` = `D…`) yields a quote AND a print that both say Delayed.
#[test]
fn a_delayed_stock_row_is_marked_delayed() {
    let mut acc = SmdAccumulator::default();
    let row = json!({
        "server_id": "q0", "conid": 265598, "_updated": 1_702_334_859_712_i64,
        "topic": "smd+265598", "6509": "DPB",
        "31": "193.18", "84": "193.17", "85": "200", "86": "193.19", "88": "1,100", "7059": "100"
    });
    let up = acc.apply(&row, NOW);
    assert_eq!(up.delay, Some(DataDelay::DELAYED));
    let q = up.quote.expect("both sides known");
    assert_eq!((q.quote.bid, q.quote.ask), (193.17, 193.19));
    assert_eq!((q.quote.bid_size, q.quote.ask_size), (1_100.0, 200.0), "88 = bid size, 85 = ask");
    assert_eq!((q.quote.ts, q.quote.local_ts), (1_702_334_859_712, NOW));
    assert_eq!(q.delay, Some(DataDelay::DELAYED));
    let t = up.trade.expect("an unprefixed last is a print");
    assert_eq!((t.trade.price, t.trade.size), (193.18, 100.0));
    assert_eq!(t.delay, Some(DataDelay::DELAYED), "a row states no lag");
}

/// A forex row marked real-time (`R…`) says RealTime; a pair prints no last, so no trade.
#[test]
fn a_real_time_forex_row_is_marked_real_time() {
    let mut acc = SmdAccumulator::default();
    let up = acc.apply(
        &json!({ "topic": "smd+12087792", "6509": "RpB", "84": "1.08512", "86": "1.08514",
                 "88": "2.5M", "85": "1M" }),
        NOW,
    );
    assert_eq!(up.delay, Some(DataDelay::RealTime));
    let q = up.quote.expect("quote");
    assert_eq!(q.delay, Some(DataDelay::RealTime));
    assert_eq!((q.quote.bid_size, q.quote.ask_size), (2_500_000.0, 1_000_000.0));
    assert_eq!(q.quote.ts, NOW, "no _updated: stamped with the receive time");
    assert!(up.trade.is_none());
}

/// Rows carry only what CHANGED: every field is sticky, the marker included, and a quote is not
/// emitted until both sides are known.
#[test]
fn fields_are_sticky_across_delta_rows() {
    let mut acc = SmdAccumulator::default();
    let first = acc.apply(&json!({ "6509": "DPB", "84": 10.0, "88": 5 }), NOW);
    assert!(first.quote.is_none(), "no ask yet");
    let second = acc.apply(&json!({ "86": 10.5 }), NOW);
    let q = second.quote.expect("now both sides");
    assert_eq!((q.quote.bid, q.quote.ask, q.quote.bid_size), (10.0, 10.5, 5.0));
    assert_eq!(q.delay, Some(DataDelay::DELAYED), "the marker survives a row without it");
    let last = acc.apply(&json!({ "31": 10.25, "7059": 3 }), NOW).trade.expect("print");
    assert_eq!(last.trade.size, 3.0);
    let again = acc.apply(&json!({ "31": 10.3 }), NOW).trade.expect("print");
    assert_eq!(again.trade.size, 3.0, "an absent 7059 is the same size, not zero");
    assert!(acc.apply(&json!({ "31": 10.3 }), NOW).quote.is_none(), "a print is not a quote");
}

/// A `C` (prior close) or `H` (halted) last is a level, not a trade.
#[test]
fn a_close_or_halted_last_is_not_a_print() {
    let mut acc = SmdAccumulator::default();
    assert!(acc.apply(&json!({ "31": "C193.00" }), NOW).trade.is_none());
    assert!(acc.apply(&json!({ "31": "H193.00" }), NOW).trade.is_none());
    assert_eq!(parse_md_price(&json!("C193.00")), Some((193.0, PriceMark::Close)));
    assert_eq!(parse_md_price(&json!("H1,193.5")), Some((1193.5, PriceMark::Halted)));
    assert_eq!(parse_md_price(&json!(-1.5)), Some((-1.5, PriceMark::Traded)));
    assert_eq!(parse_md_price(&json!("n/a")), None);
}

/// Every availability letter onto the workspace label: frozen and delayed-frozen are both
/// `Frozen`, and not-subscribed or an unknown letter is NO label.
#[test]
fn availability_letters_decode() {
    for (code, want) in [
        ("RpB", Some(DataDelay::RealTime)),
        ("DPB", Some(DataDelay::DELAYED)),
        ("Z", Some(DataDelay::Frozen)),
        ("YP", Some(DataDelay::Frozen)),
        ("N", None),
        ("", None),
        ("?", None),
    ] {
        assert_eq!(availability_delay(code), want, "{code:?}");
    }
}

/// Sizes: separators, magnitude suffixes, and refusal of a negative or non-finite size.
#[test]
fn sizes_parse_or_refuse() {
    assert_eq!(parse_md_size(&json!("1,100")), Some(1_100.0));
    assert_eq!(parse_md_size(&json!("1.2K")), Some(1_200.0));
    assert_eq!(parse_md_size(&json!(" 3M ")), Some(3_000_000.0));
    assert_eq!(parse_md_size(&json!(7)), Some(7.0));
    assert_eq!(parse_md_size(&json!("-1")), None);
    assert_eq!(parse_md_size(&json!("NaN")), None);
    assert_eq!(parse_md_size(&json!("K")), None);
}

/// The subscribe/unsubscribe frames and the topic → conId routing.
#[test]
fn smd_frames_and_routing() {
    assert_eq!(
        smd_subscribe(265598),
        r#"smd+265598+{"fields":["31","84","85","86","88","7059","6509"]}"#
    );
    assert_eq!(smd_unsubscribe(265598), "umd+265598+{}");
    assert_eq!(snapshot_fields(), SMD_FIELDS.join(","));
    assert_eq!(smd_conid(&json!({ "topic": "smd+265598" })), Some(265598));
    assert_eq!(smd_conid(&json!({ "topic": "smd", "conid": "8314" })), Some(8314));
    assert_eq!(smd_conid(&json!({ "topic": "sor", "conid": 1 })), None, "another topic");
    assert_eq!(smd_conid(&json!({ "conid": 1 })), None, "no topic");
}

/// A snapshot row is data only when it carries a field; the first request's preflight row (the
/// conId alone) and another conId's row are not.
#[test]
fn a_snapshot_row_must_carry_data() {
    let preflight = json!([{ "conid": 265598, "conidEx": "265598", "server_id": "q0" }]);
    assert!(snapshot_row(&preflight, 265598).is_none());
    let body = json!([
        { "conid": 8314, "84": "1.0" },
        { "conid": "265598", "6509": "DPB", "84": "193.17", "86": "193.19" }
    ]);
    let row = snapshot_row(&body, 265598).expect("the data row");
    let mut acc = SmdAccumulator::default();
    let q = acc.apply(row, NOW).quote.expect("a snapshot row seeds the first quote");
    assert_eq!((q.quote.bid, q.delay), (193.17, Some(DataDelay::DELAYED)));
    assert!(snapshot_row(&json!({}), 1).is_none());
}

/// A history body: bars ascending, a duplicated stamp keeps the later row, an unusable row is
/// skipped, and the body's delay marker is read.
#[test]
fn a_history_body_decodes_bars_and_its_delay() {
    let body = json!({
        "symbol": "AAPL", "barLength": 60, "mdAvailability": "S", "mktDataDelay": 900000,
        "data": [
            { "t": 1_692_345_660_000_i64, "o": 173.5, "h": 173.6, "l": 173.4, "c": 173.55, "v": 900.0 },
            { "t": 1_692_345_600_000_i64, "o": 173.4, "h": 173.48, "l": 173.0, "c": 173.24, "v": 8174.0 },
            { "t": 1_692_345_660_000_i64, "o": 1.0, "h": 1.0, "l": 1.0, "c": 1.0 },
            { "t": 0, "o": 1.0, "h": 1.0, "l": 1.0, "c": 1.0 },
            { "t": 1_692_345_720_000_i64, "o": "x", "h": 1.0, "l": 1.0, "c": 1.0 }
        ]
    });
    let h = decode_history(&body);
    let ts: Vec<i64> = h.bars.iter().map(|b| b.ts).collect();
    assert_eq!(ts, vec![1_692_345_600_000, 1_692_345_660_000]);
    assert_eq!(h.bars[1].close, 1.0, "the later row of a duplicated stamp wins");
    assert_eq!(h.bars[1].volume, 0.0, "a missing volume is zero");
    assert_eq!(
        h.delay,
        Some(DataDelay::DELAYED),
        "`S` is a letter nothing reads, so a positive mktDataDelay decides, with no `lag_ms`: \
         what the number measures is disputed"
    );
    assert_eq!(h.mkt_data_delay, Some(900_000));

    let live = decode_history(&json!({ "mdAvailability": "RpB", "mktDataDelay": 0, "data": [] }));
    assert_eq!((live.delay, live.mkt_data_delay), (Some(DataDelay::RealTime), None));
    assert!(live.bars.is_empty());
    assert_eq!(decode_history(&json!("garbage")).bars.len(), 0);
}

/// A letter IBKR documents for `mdAvailability` outranks a positive `mktDataDelay`. IBKR's own
/// `/iserver/marketdata/history` pages describe that number as the milliseconds the REQUEST took
/// ("taken to satisfy this historical data request"; its sample body reads `0` beside a separate
/// `travelTime`), so a real-time series whose request took a few milliseconds must not be labelled
/// delayed — the label goes to the bars stream before any live row and the core keeps one label per
/// symbol. The number still decides when the letter says nothing (the `S` test above).
#[test]
fn a_documented_availability_letter_outranks_a_positive_mkt_data_delay() {
    for (letter, delay, want) in [
        ("RpB", 37, DataDelay::RealTime),
        ("R", 900_000, DataDelay::RealTime),
        ("ZpB", 37, DataDelay::Frozen),
        ("YpB", 37, DataDelay::Frozen),
        ("DpB", 37, DataDelay::DELAYED),
        ("DpB", 0, DataDelay::DELAYED),
    ] {
        let h = decode_history(&json!({ "mdAvailability": letter, "mktDataDelay": delay }));
        assert_eq!(h.delay, Some(want), "mdAvailability {letter:?}, mktDataDelay {delay}");
        assert_eq!(
            h.mkt_data_delay,
            (delay > 0).then_some(delay),
            "the raw number is still carried as sent, for the capture that settles it"
        );
    }
    // no letter at all: the number alone still decides, as before
    let bare = decode_history(&json!({ "mktDataDelay": 37, "data": [] }));
    assert_eq!(bare.delay, Some(DataDelay::DELAYED));
    assert_eq!(decode_history(&json!({ "mktDataDelay": 0 })).delay, None);
}

/// The history `bar=` word is exact or absent, and a seed `period=` stays within the endpoint's
/// 1,000-point ceiling.
#[test]
fn history_request_words() {
    for (ms, word) in [
        (60_000, "1min"),
        (300_000, "5min"),
        (900_000, "15min"),
        (3_600_000, "1h"),
        (14_400_000, "4h"),
        (86_400_000, "1d"),
        (604_800_000, "1w"),
    ] {
        assert_eq!(history_bar_param(ms), Some(word), "{ms}");
    }
    for ms in [0, 5_000, 45 * 60_000, 6 * 3_600_000] {
        assert_eq!(history_bar_param(ms), None, "{ms}");
    }
    assert_eq!(history_period(60_000), "8h", "1d of 1min bars is 1,440 points");
    assert_eq!(history_period(300_000), "1d");
    assert_eq!(history_period(86_400_000), "100d");
    assert_eq!(history_period(604_800_000), "700d");
    assert_eq!(history_period(30 * 86_400_000), "1000d", "capped");
}
