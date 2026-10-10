//! "Arbitrary input never panics" harness for every PUBLIC IG wire decoder:
//!
//! - `vike_ig::lightstreamer::{parse_frame, percent_decode, MergeItemState}` — the TLCP codec every
//!   market-data WS text frame crosses first (`CONOK` / `U,<sub>,<item>,<f1>|<f2>…` with its `^n`
//!   runs and percent-escapes / `REQERR` / `END` …);
//! - `vike_ig::market_data::{QuoteFold, CandleFold}` — the stateful MERGE folds the update frames
//!   drive (fed a SEQUENCE through ONE instance, the way the feed thread does);
//! - `vike_ig::{decode_trade_confirm, map_confirm, unresolved_confirm}` — the `CONFIRMS` / `/confirms`
//!   mappers behind the exec lanes;
//! - `vike_ig::recon_client::{parse_positions, parse_working_orders, parse_activity_fills,
//!   parse_balance, ms_to_ig_datetime}` — the reconcile report parsers;
//! - `vike_ig::{parse_prices, parse_ig_time_utc, parse_markets, resolution}` and
//!   `vike_ig::market_data::{ig_scale, market_item, chart_item}`, `vike_ig::market_feed::ls_ws_url`.
//!
//! The property is TOTALITY: a hostile, truncated or wrong-typed frame may decode to nothing (or an
//! `Err`), but it must never panic the feed / stream thread (a dead thread is a venue that silently
//! goes quiet), and it must never fabricate an event flood. Each decoder is fed (a) arbitrary text,
//! (b) lossy-decoded byte noise and (c) structured input built from the decoder's REAL field names
//! and TLCP grammar, so the generator reaches the branches instead of bouncing off the first
//! `.get()`. Outputs are asserted only against the cheap bounds that must always hold.
//!
//! The private decoders (`event_mapper::parse_update_line`, `stream::parse_conok`, `is_conerr`,
//! `rebase_host`) are covered by the sibling unit-test file `src/stream_props.rs`.
//!
//! A minimized counterexample is a REAL bug: commit the `.proptest-regressions` seed beside this
//! file and report it.

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_ig::lightstreamer::{
    FieldDelta, LsFrame, LsUpdate, MergeItemState, create_session_request, parse_frame,
    percent_decode, subscribe_request,
};
use vike_ig::market_data::{CandleFold, QuoteFold, chart_item, ig_scale, market_item};
use vike_ig::market_feed::ls_ws_url;
use vike_ig::recon_client::{
    ms_to_ig_datetime, parse_activity_fills, parse_balance, parse_positions, parse_working_orders,
};
use vike_ig::{
    IgApiError, decode_trade_confirm, map_confirm, parse_ig_time_utc, parse_markets, parse_prices,
    resolution, unresolved_confirm,
};

/// Real IG REST / `CONFIRMS` field names across every JSON decoder above.
const KEYS: &[&str] = &[
    "prices",
    "openPrice",
    "highPrice",
    "lowPrice",
    "closePrice",
    "bid",
    "ask",
    "snapshotTimeUTC",
    "lastTradedVolume",
    "markets",
    "epic",
    "instrumentType",
    "instrumentName",
    "positions",
    "position",
    "market",
    "direction",
    "size",
    "level",
    "createdDateUTC",
    "dealId",
    "dealReference",
    "currency",
    "workingOrders",
    "workingOrderData",
    "orderType",
    "orderSize",
    "marketData",
    "activities",
    "type",
    "status",
    "dealStatus",
    "reason",
    "details",
    "date",
    "accounts",
    "accountId",
    "balance",
    "affectedDeals",
];

/// Real dispatch values and epics.
const WORDS: &[&str] = &[
    "BUY",
    "SELL",
    "ACCEPTED",
    "REJECTED",
    "OPEN",
    "AMENDED",
    "CLOSED",
    "PARTIALLY_CLOSED",
    "DELETED",
    "POSITION",
    "LIMIT",
    "STOP",
    "CURRENCIES",
    "INDICES",
    "SHARES",
    "CS.D.EURUSD.MINI.IP",
    "DIAAA1",
    "",
];

/// The epics the report parsers are mounted with.
const EPICS: &[&str] = &["CS.D.EURUSD.MINI.IP", "IX.D.DAX.IFD.IP", ""];

/// Epoch-ms open times for the candle fold: a few adjacent candles, then the extremes.
const UTMS: &[&str] = &[
    "1700000000000",
    "1700000060000",
    "1700000120000",
    "0",
    "-1",
    "9223372036854775807",
    "9223372036854775808",
];

/// IG's `snapshotTimeUTC` / `createdDateUTC` / `date` spelling with every numeric field free: a
/// signed month or day (`2022--1-15`) is the shape that wraps under an `as u32` cast.
fn arb_ig_time() -> impl Strategy<Value = String> {
    "[-+0-9][0-9]{3}-[-+0-9][0-9]-[-+0-9][0-9][T ][-+0-9][0-9]:[-+0-9][0-9]:[-+0-9][0-9]"
}

fn arb_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        prop_oneof![Just(0i64), Just(-1), Just(i64::MAX), Just(i64::MIN), any::<i64>()]
            .prop_map(Value::from),
        Just(Value::from(u64::MAX)),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        prop::sample::select(vec![
            "NaN",
            "inf",
            "-inf",
            "-0",
            "0",
            "1e999",
            "-1e999",
            "",
            " ",
            ".",
            "+1",
            "-1",
            "0x10",
            "9999999999999999999999999999999999999999",
        ])
        .prop_map(|s| Value::String(s.to_string())),
        "[+-]?[0-9]{1,30}(\\.[0-9]{1,30})?".prop_map(Value::String),
        prop::sample::select(WORDS).prop_map(|s| Value::String(s.to_string())),
        arb_ig_time().prop_map(Value::String),
        any::<String>().prop_map(Value::String),
        Just(Value::Array(Vec::new())),
        Just(Value::Object(Map::new())),
    ]
}

/// Arbitrary JSON, depth <= 4, object keys drawn mostly from the real field names.
fn arb_json() -> impl Strategy<Value = Value> {
    let key = prop_oneof![
        8 => prop::sample::select(KEYS).prop_map(String::from),
        1 => "[a-zA-Z_]{1,8}",
    ];
    arb_leaf().prop_recursive(4, 64, 6, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((key.clone(), inner), 0..8)
                .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
        ]
    })
}

/// Raw bytes -> text the way a lossy socket read would produce it.
fn arb_noise() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<u8>(), 0..512)
        .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn set(mut v: Value, key: &str, child: Value) -> Value {
    if let Value::Object(m) = &mut v {
        m.insert(key.to_string(), child);
    }
    v
}

/// An object over exactly `keys`, each present with high probability and holding a hostile leaf.
fn shaped(keys: &'static [&'static str]) -> impl Strategy<Value = Value> {
    prop::collection::vec(prop::option::weighted(0.85, arb_leaf()), keys.len()..=keys.len())
        .prop_map(move |vals| {
            Value::Object(
                keys.iter()
                    .zip(vals)
                    .filter_map(|(k, v)| v.map(|v| ((*k).to_string(), v)))
                    .collect(),
            )
        })
}

/// A value drawn from the decoder's real dispatch words five times in six, a hostile leaf otherwise.
fn mostly(words: &'static [&'static str]) -> impl Strategy<Value = Value> {
    prop_oneof![
        5 => prop::sample::select(words).prop_map(|s| Value::String(s.to_string())),
        1 => arb_leaf(),
    ]
}

/// `base` with `key` overwritten by a value from `val`.
fn with(
    base: impl Strategy<Value = Value>,
    key: &'static str,
    val: impl Strategy<Value = Value>,
) -> impl Strategy<Value = Value> {
    (base, val).prop_map(move |(b, v)| set(b, key, v))
}

/// `{ key: [row, …] }`.
fn rows(row: impl Strategy<Value = Value>, key: &'static str) -> impl Strategy<Value = Value> {
    prop::collection::vec(row, 0..6)
        .prop_map(move |r| set(Value::Object(Map::new()), key, Value::Array(r)))
}

/// A positive, finite number: a `size` / `level` that clears the "nothing executable" guard.
fn arb_size() -> impl Strategy<Value = Value> {
    prop_oneof![
        5 => (0.0001f64..1.0e6).prop_map(Value::from),
        1 => arb_leaf(),
    ]
}

/// A deal confirm: `/confirms/{ref}` and the streamed `CONFIRMS` share this shape.
fn arb_confirm() -> impl Strategy<Value = Value> {
    let base = shaped(&["dealId", "dealReference", "epic", "reason"]);
    let base = with(base, "dealStatus", mostly(&["ACCEPTED", "REJECTED"]));
    let base =
        with(base, "status", mostly(&["OPEN", "AMENDED", "CLOSED", "PARTIALLY_CLOSED", "DELETED"]));
    let base = with(base, "direction", mostly(&["BUY", "SELL"]));
    let base = with(base, "size", arb_size());
    with(base, "level", arb_size())
}

/// `GET /positions` body: rows of `{position:{…}, market:{epic}}`.
fn arb_positions_body() -> impl Strategy<Value = Value> {
    let pos = with(shaped(&["level", "dealId", "currency"]), "size", arb_size());
    let pos = with(pos, "direction", mostly(&["BUY", "SELL"]));
    let pos = with(pos, "createdDateUTC", arb_ig_time().prop_map(Value::String));
    let market = with(Just(Value::Object(Map::new())), "epic", mostly(EPICS));
    let row = with(with(Just(Value::Object(Map::new())), "position", pos), "market", market);
    rows(row, "positions")
}

/// `GET /workingorders` body: rows of `{workingOrderData:{…}, marketData:{epic}}`.
fn arb_working_orders_body() -> impl Strategy<Value = Value> {
    let wo = with(shaped(&["dealId", "orderSize"]), "epic", mostly(EPICS));
    let wo = with(wo, "direction", mostly(&["BUY", "SELL"]));
    let wo = with(wo, "orderType", mostly(&["LIMIT", "STOP"]));
    let wo = with(wo, "createdDateUTC", arb_ig_time().prop_map(Value::String));
    let market = with(Just(Value::Object(Map::new())), "epic", mostly(EPICS));
    let row =
        with(with(Just(Value::Object(Map::new())), "workingOrderData", wo), "marketData", market);
    rows(row, "workingOrders")
}

/// `GET /history/activity` body: rows of executed-deal activities with a `details` block.
fn arb_activities_body() -> impl Strategy<Value = Value> {
    let details =
        with(shaped(&["dealReference", "size", "level"]), "direction", mostly(&["BUY", "SELL"]));
    let row = with(shaped(&["dealId"]), "epic", mostly(EPICS));
    let row = with(row, "type", mostly(&["POSITION"]));
    let row = with(row, "status", mostly(&["ACCEPTED", "REJECTED"]));
    let row = with(row, "date", arb_ig_time().prop_map(Value::String));
    let row = with(row, "details", details);
    rows(row, "activities")
}

/// `GET /accounts` body.
fn arb_accounts_body() -> impl Strategy<Value = Value> {
    let balance = with(Just(Value::Object(Map::new())), "balance", arb_leaf());
    let row = with(shaped(&[]), "accountId", mostly(&["ABC12", ""]));
    rows(with(row, "balance", balance), "accounts")
}

/// A `/prices/{epic}` body: rows of `{snapshotTimeUTC, …Price:{bid, ask}}`.
fn arb_prices_body() -> impl Strategy<Value = Value> {
    let row = with(shaped(&["lastTradedVolume"]), "openPrice", shaped(&["bid", "ask"]));
    let row = with(row, "highPrice", shaped(&["bid", "ask"]));
    let row = with(row, "lowPrice", shaped(&["bid", "ask"]));
    let row = with(row, "closePrice", shaped(&["bid", "ask"]));
    let row = with(row, "snapshotTimeUTC", arb_ig_time().prop_map(Value::String));
    rows(row, "prices")
}

// ---- TLCP text -------------------------------------------------------------------------------

/// One `U` field segment: every sentinel (`""` unchanged, `#` null, `$` empty), `^n` runs up to and
/// past the 4096 cap, percent-escapes (valid, truncated, non-hex), numbers and free text.
fn arb_field_seg() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("#".to_string()),
        Just("$".to_string()),
        "\\^[0-9]{1,6}",
        "[0-9]{1,2}\\.[0-9]{1,5}",
        "[0-9]{10,14}",
        "(%[0-9A-Fa-f]{2}){1,3}",
        "[%0-9A-Fa-fé€😀]{0,8}",
        "[^|\\r\\n]{0,10}",
    ]
}

/// A well-formed-looking `U,<sub>,<item>,<f1>|<f2>…` line (sub / item up to 12 digits: the `u32`
/// overflow edge).
fn arb_update_line() -> impl Strategy<Value = String> {
    (
        prop_oneof![3 => Just("1".to_string()), 1 => "[0-9]{1,12}"],
        prop_oneof![3 => Just("1".to_string()), 1 => "[0-9]{1,12}"],
        prop::collection::vec(arb_field_seg(), 0..14),
    )
        .prop_map(|(sub, item, fields)| format!("U,{sub},{item},{}", fields.join("|")))
}

/// The non-update TLCP notifications, with every numeric slot free.
fn arb_control_line() -> impl Strategy<Value = String> {
    prop_oneof![
        ("[^,]{0,12}", "[0-9]{0,22}", "[0-9]{0,22}", "[^,]{0,20}")
            .prop_map(|(a, b, c, d)| format!("CONOK,{a},{b},{c},{d}")),
        ("[0-9]{0,3}", "[^\\r\\n]{0,20}").prop_map(|(c, m)| format!("CONERR,{c},{m}")),
        ("[0-9]{0,12}", "[0-9]{0,12}", "[0-9]{0,12}")
            .prop_map(|(a, b, c)| format!("SUBOK,{a},{b},{c}")),
        ("[0-9]{0,4}", "[0-9]{0,4}", "[^\\r\\n]{0,20}")
            .prop_map(|(r, c, m)| format!("REQERR,{r},{c},{m}")),
        ("[0-9]{0,3}", "[^\\r\\n]{0,20}").prop_map(|(c, m)| format!("END,{c},{m}")),
        Just("LOOP,5000".to_string()),
        Just("PROBE".to_string()),
        Just("REQOK".to_string()),
        Just("U".to_string()),
        Just("U,".to_string()),
        Just("CONOK".to_string()),
    ]
}

/// One TLCP line: mostly the grammar, sometimes text or noise.
fn arb_tlcp_line() -> impl Strategy<Value = String> {
    prop_oneof![
        5 => arb_update_line(),
        3 => arb_control_line(),
        1 => any::<String>(),
        1 => arb_noise(),
    ]
}

fn arb_delta() -> impl Strategy<Value = FieldDelta> {
    prop_oneof![
        Just(FieldDelta::Unchanged),
        Just(FieldDelta::Null),
        prop_oneof![
            "[0-9]{1,2}\\.[0-9]{1,5}",
            "[+-]?[0-9]{1,20}",
            prop::sample::select(UTMS).prop_map(String::from),
            prop::sample::select(vec!["0", "1", "TRADEABLE", "CLOSED", "NaN", "inf", ""])
                .prop_map(String::from),
            any::<String>(),
        ]
        .prop_map(FieldDelta::Value),
    ]
}

/// A price slot of a candle update: a decimal five times in six.
fn arb_price_delta() -> impl Strategy<Value = FieldDelta> {
    prop_oneof![
        5 => "[0-9]\\.[0-9]{1,5}".prop_map(FieldDelta::Value),
        1 => arb_delta(),
    ]
}

/// A `CHART` update in the real 11-slot layout (`UTM`, 8 prices, `LTV`, `CONS_END`), the open time
/// drawn from a few adjacent candles so rollover and `CONS_END` closes both run.
fn arb_candle_update() -> impl Strategy<Value = LsUpdate> {
    (
        prop_oneof![
            4 => prop::sample::select(UTMS).prop_map(|s| FieldDelta::Value(s.to_string())),
            1 => arb_delta(),
        ],
        prop::collection::vec(arb_price_delta(), 8..=8),
        arb_delta(),
        prop_oneof![
            3 => Just(FieldDelta::Value("1".to_string())),
            3 => Just(FieldDelta::Value("0".to_string())),
            1 => arb_delta(),
        ],
    )
        .prop_map(|(utm, prices, ltv, cons_end)| {
            let mut fields = vec![utm];
            fields.extend(prices);
            fields.push(ltv);
            fields.push(cons_end);
            LsUpdate { sub_id: 1, item: 1, fields }
        })
}

/// A `MARKET` update in the real 4-slot layout (`BID`, `OFFER`, `UPDATE_TIME`, `MARKET_STATE`) or a
/// free one.
fn arb_quote_update() -> impl Strategy<Value = LsUpdate> {
    prop_oneof![
        (arb_price_delta(), arb_price_delta(), arb_delta(), arb_delta())
            .prop_map(|(b, o, t, s)| LsUpdate { sub_id: 1, item: 1, fields: vec![b, o, t, s] }),
        prop::collection::vec(arb_delta(), 0..14).prop_map(|fields| LsUpdate {
            sub_id: 1,
            item: 1,
            fields
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // ---- TLCP codec -------------------------------------------------------------------------

    /// (a) `parse_frame` / `percent_decode` are total over arbitrary text; the only `None` is the
    /// empty line.
    #[test]
    fn tlcp_codec_survives_arbitrary_text(text in any::<String>()) {
        let frame = parse_frame(&text);
        prop_assert_eq!(frame.is_none(), text.is_empty());
        let _ = percent_decode(&text);
    }

    #[test]
    fn tlcp_codec_survives_byte_noise(text in arb_noise()) {
        let _ = parse_frame(&text);
        let _ = percent_decode(&text);
    }

    /// (b) ...and over grammar-shaped lines. An update never carries more than one `^4096` run per
    /// field segment, so its field count is bounded by what the line can hold.
    #[test]
    fn tlcp_codec_survives_grammar_shaped_lines(line in arb_tlcp_line()) {
        if let Some(LsFrame::Update(u)) = parse_frame(&line) {
            prop_assert!(u.fields.len() <= 4096 * line.len(), "{} fields from {} bytes", u.fields.len(), line.len());
        }
    }

    /// Percent-escapes at every boundary, multi-byte characters included.
    #[test]
    fn percent_decode_survives_escape_soup(text in "[%0-9A-Fa-fé€😀 ]{0,32}") {
        let _ = percent_decode(&text);
    }

    /// The request builders accept any user / password / group text.
    #[test]
    fn request_builders_survive_arbitrary_text(
        user in any::<String>(),
        password in any::<String>(),
        group in any::<String>(),
        req_id in any::<u32>(),
        sub_id in any::<u32>(),
        snapshot in any::<bool>(),
    ) {
        let _ = create_session_request(&user, &password);
        let _ = subscribe_request(req_id, sub_id, "MERGE", &group, "BID OFFER", snapshot);
    }

    /// One MERGE state instance over a run of update deltas, read back at arbitrary indices.
    #[test]
    fn merge_item_state_survives_a_delta_sequence(
        width in 0usize..16,
        updates in prop::collection::vec(prop::collection::vec(arb_delta(), 0..20), 1..8),
        idx in 0usize..40,
    ) {
        let mut state = MergeItemState::new(width);
        for fields in &updates {
            state.apply(fields);
            let _ = state.get(idx);
            let _ = state.get_f64(idx);
            let _ = state.get_i64(idx);
        }
    }

    // ---- market-data folds ------------------------------------------------------------------

    /// (c) A run of candle updates folded through ONE `CandleFold` never panics; one update closes
    /// at most the previous candle and forms the next.
    #[test]
    fn candle_fold_survives_an_update_sequence(
        updates in prop::collection::vec(arb_candle_update(), 1..10),
    ) {
        let mut fold = CandleFold::new();
        for u in &updates {
            let out = fold.on_update(u);
            prop_assert!(out.len() <= 2, "{} candle events from one update", out.len());
        }
    }

    #[test]
    fn candle_fold_survives_free_updates(
        updates in prop::collection::vec(
            prop::collection::vec(arb_delta(), 0..14)
                .prop_map(|fields| LsUpdate { sub_id: 1, item: 1, fields }),
            1..10,
        ),
    ) {
        let mut fold = CandleFold::new();
        for u in &updates {
            let _ = fold.on_update(u);
        }
    }

    /// (c) ...and a run of quote updates through ONE `QuoteFold`.
    #[test]
    fn quote_fold_survives_an_update_sequence(
        updates in prop::collection::vec(arb_quote_update(), 1..10),
    ) {
        let mut fold = QuoteFold::new();
        for u in &updates {
            let _ = fold.on_update(u);
        }
    }

    /// The feed's `on_text` shape end to end: one WS message bundling several CR-LF-terminated TLCP
    /// lines, each parsed and (for an update) folded into both lanes' state.
    #[test]
    fn a_bundled_ws_message_survives_the_whole_decode(
        messages in prop::collection::vec(
            prop::collection::vec(arb_tlcp_line(), 0..6).prop_map(|ls| ls.join("\r\n")),
            1..5,
        ),
    ) {
        let mut quotes = QuoteFold::new();
        let mut candles = CandleFold::new();
        for text in &messages {
            for line in text.lines().map(str::trim_end) {
                if let Some(LsFrame::Update(u)) = parse_frame(line) {
                    let _ = quotes.on_update(&u);
                    let out = candles.on_update(&u);
                    prop_assert!(out.len() <= 2);
                }
            }
        }
    }

    // ---- deal confirms ----------------------------------------------------------------------

    /// `decode_trade_confirm` (streamed) and `map_confirm` (sync) over arbitrary JSON and shaped
    /// confirms: the streamed lane emits at most the dual-publish pair, the sync lane at most
    /// Accepted + Fill + OrderFilled.
    #[test]
    fn confirm_mappers_survive_structured_json(
        free in arb_json(),
        shaped_confirm in arb_confirm(),
        coid in any::<String>(),
        ts in any::<i64>(),
        market in any::<bool>(),
    ) {
        for confirm in [&free, &shaped_confirm] {
            let streamed = decode_trade_confirm(confirm, &coid, ts);
            prop_assert!(streamed.len() <= 2, "event flood: {} events", streamed.len());
            let sync = map_confirm(&coid, ts, market, confirm);
            prop_assert!(sync.len() <= 3, "event flood: {} events", sync.len());
        }
    }

    /// (c) A run of confirms for one order, as a reconnect replay delivers them.
    #[test]
    fn confirm_mappers_survive_a_replay_sequence(
        confirms in prop::collection::vec(prop_oneof![arb_confirm(), arb_json()], 1..8),
        coid in any::<String>(),
    ) {
        for c in &confirms {
            let _ = decode_trade_confirm(c, &coid, 1);
            let _ = map_confirm(&coid, 1, true, c);
        }
    }

    /// The optimistic resolution of a `/confirms` that never answered is always exactly one event.
    #[test]
    fn unresolved_confirm_is_always_one_event(
        coid in any::<String>(),
        deal_ref in any::<String>(),
        status in any::<u16>(),
        message in any::<String>(),
    ) {
        let out = unresolved_confirm(&coid, 0, &deal_ref, &IgApiError { status, message });
        prop_assert_eq!(out.len(), 1);
    }

    // ---- reconcile report parsers -----------------------------------------------------------

    /// All four report parsers are total over arbitrary text and byte noise.
    #[test]
    fn recon_parsers_survive_arbitrary_text(
        text in prop_oneof![any::<String>(), arb_noise()],
        epic in prop::sample::select(EPICS),
    ) {
        let _ = parse_positions(&text, epic);
        let _ = parse_working_orders(&text, epic);
        let _ = parse_activity_fills(&text, epic);
        let _ = parse_balance(&text, epic);
    }

    /// ...and over arbitrary JSON.
    #[test]
    fn recon_parsers_survive_structured_json(
        v in arb_json(),
        epic in prop::sample::select(EPICS),
    ) {
        let body = v.to_string();
        let _ = parse_positions(&body, epic);
        let _ = parse_working_orders(&body, epic);
        let _ = parse_activity_fills(&body, epic);
        let _ = parse_balance(&body, epic);
    }

    /// ...and over bodies shaped like the real endpoints, so the row closures run on hostile fields
    /// (signed months included); a position query always answers one row and the others never
    /// invent rows.
    #[test]
    fn recon_parsers_survive_shaped_bodies(
        positions in arb_positions_body(),
        working in arb_working_orders_body(),
        activities in arb_activities_body(),
        accounts in arb_accounts_body(),
        epic in prop::sample::select(EPICS),
    ) {
        let p = parse_positions(&positions.to_string(), epic).expect("a positions array");
        prop_assert_eq!(p.len(), 1);
        let n = working["workingOrders"].as_array().map_or(0, Vec::len);
        prop_assert!(parse_working_orders(&working.to_string(), epic).expect("an array").len() <= n);
        let n = activities["activities"].as_array().map_or(0, Vec::len);
        prop_assert!(parse_activity_fills(&activities.to_string(), epic).expect("an array").len() <= n);
        let _ = parse_balance(&accounts.to_string(), "ABC12").expect("an accounts array");
    }

    // ---- prices, catalog, time and symbol helpers -------------------------------------------

    #[test]
    fn prices_and_catalog_survive_structured_json(v in arb_json(), shaped_prices in arb_prices_body()) {
        let _ = parse_prices(&v);
        let _ = parse_markets(&v);
        let n = shaped_prices["prices"].as_array().map_or(0, Vec::len);
        prop_assert!(parse_prices(&shaped_prices).len() <= n);
    }

    /// `parse_ig_time_utc` is total over arbitrary text and over the signed-field spelling.
    #[test]
    fn ig_time_survives_arbitrary_text(text in any::<String>(), shaped_time in arb_ig_time()) {
        let _ = parse_ig_time_utc(&text);
        let _ = parse_ig_time_utc(&shaped_time);
    }

    #[test]
    fn datetime_and_symbol_helpers_survive_arbitrary_input(ms in any::<i64>(), text in any::<String>()) {
        let rendered = ms_to_ig_datetime(ms);
        prop_assert!(rendered.len() >= 19, "{rendered:?}");
        let _ = ls_ws_url(&text);
        let _ = resolution(&text);
        let _ = ig_scale(&text);
        let _ = market_item(&text);
        let _ = chart_item(&text, &text);
    }
}
