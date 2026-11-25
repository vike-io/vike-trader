//! **Every frame of this protocol crosses each of the hub codec's forms as the value JSON carries**
//! (`vike_hub_protocol::frame`'s `BodyCodec`, `vike_hub_protocol::features::FEATURE_WIRE_MSGPACK`): one sample of EVERY [`Request`] variant (this module's
//! parent's `one_of_every_request`) and of every [`Response`] variant ([`one_of_every_response`], numbered by an
//! exhaustive match, so a new variant fails to compile here until it has a row), plus every datahub reply of the
//! committed wire goldens (`fixtures/datahub_wire/`) and the kept lists recorded from the venues
//! (`crates/vike-catalog/tests/fixtures/instruments/`). A value is the same when its JSON is the same: every float,
//! string and option compared as JSON compares it.

use serde::Serialize;
use serde::de::DeserializeOwned;
use vike_hub_protocol::frame::{BodyCodec, decode_body, encode_frame, encode_reply};

use super::*;
use crate::catalog::{
    CatalogListing, CatalogOutcome, InstrumentListsReply, OperatorInstrumentsReply,
};
use crate::market::{MarketDataFrame, MarketDataSessionId, MarketDataSpec};
use crate::tool_feeds::{
    CalendarEconomicReply, CalendarEquityReply, DatahubPollOutcome, NewsReply,
};
use vike_data::{BarDay, ExecutionSeries, SeriesCoverage};
use vike_marketdata::{Bar, TickQuote, TickTrade};

const FORMS: [BodyCodec; 3] = [BodyCodec::Json, BodyCodec::Msgpack, BodyCodec::MsgpackLz4];

/// `value`'s body in `codec`'s request form (an LZ4 request is always LZ4) or reply form (LZ4 where it shrinks).
fn body_of<T: Serialize>(codec: BodyCodec, value: &T, reply: bool) -> Vec<u8> {
    let frame = if reply { encode_reply(codec, value) } else { encode_frame(codec, value) };
    frame.expect("the sample encodes").split_off(4)
}

/// `value` written in each form, as a request and as a reply, read back, and written again: the same JSON value
/// each way, and the same bytes the second time (a form that read a value back changed would write other bytes).
fn crosses_every_form<T: Serialize + DeserializeOwned>(value: &T, what: &str) {
    let json = serde_json::to_value(value).expect("the sample writes as JSON");
    for codec in FORMS {
        let request = body_of(codec, value, false);
        assert_eq!(BodyCodec::of(&request), codec, "{what}: the body names its form");
        for (body, reply) in [(request, false), (body_of(codec, value, true), true)] {
            let back: T =
                decode_body(&body).unwrap_or_else(|e| panic!("{what} from {codec:?}: {e}"));
            assert_eq!(serde_json::to_value(&back).expect("json"), json, "{what} via {codec:?}");
            assert_eq!(body_of(codec, &back, reply), body, "{what}: {codec:?} writes it once");
        }
    }
}

/// The variant each response maps to, through a match with NO `_` arm.
fn response_variant_index(r: &Response) -> usize {
    match r {
        Response::Pong => 0,
        Response::Error(_) => 1,
        Response::Bars(_) => 2,
        Response::Quotes(_) => 3,
        Response::Trades(_) => 4,
        Response::BookUpdates(_) => 5,
        Response::Depth(_) => 6,
        Response::Cohort(_) => 7,
        Response::PerpMetrics(_) => 8,
        Response::Texts(_) => 9,
        Response::Chain(_) => 10,
        Response::FinancialFacts(_) => 11,
        Response::InstitutionalHoldings(_) => 12,
        Response::TextScores(_) => 13,
        Response::Equity(_) => 14,
        Response::ExecFills(_) => 15,
        Response::ExecOrders(_) => 16,
        Response::Forecasts(_) => 17,
        Response::Properties(_) => 18,
        Response::SeriesList(_) => 19,
        Response::Inventory(_) => 20,
        Response::ExecutionSeries(_) => 21,
        Response::SeriesGaps(_) => 22,
        Response::SeriesFacts(_) => 23,
        Response::SeriesDays(_) => 24,
        Response::Coverage(_) => 25,
        Response::BackfillDone(_) => 26,
        Response::RunningBackfills(_) => 27,
        Response::BackfillsCancelled(_) => 28,
        Response::HistoryChannels(_) => 29,
        Response::GatewayStatus(_) => 30,
        Response::GatewayLinks(_) => 31,
        Response::CredentialsChanged(_) => 32,
        Response::SeriesSeeded(_) => 33,
        Response::VenueCatalog(_) => 34,
        Response::OptionsChain(_) => 35,
        Response::OptionsRefreshed { .. } => 36,
        Response::NewsFeed(_) => 37,
        Response::EconomicCalendar(_) => 38,
        Response::EquityCalendar(_) => 39,
        Response::Deleted(_) => 40,
        Response::ArchiveImported(_) => 41,
        Response::ArchiveFetched(_) => 42,
        Response::MarketDataSubscribed { .. } => 43,
        Response::MarketDataUpdated { .. } => 44,
        Response::StrategyOpened { .. } => 45,
        Response::MarketData(_) => 46,
        Response::CsvUploadReady => 47,
        Response::CsvUploadReceived { .. } => 48,
        Response::CsvUploaded(_) => 49,
        Response::SymbolSets(_) => 50,
        Response::InstrumentLists(_) => 51,
        Response::InstrumentListsRefreshed(_) => 52,
        Response::OperatorInstruments(_) => 53,
        Response::SplitWarnings(_) => 54,
        Response::LiveFamilyTokens(_) => 55,
        Response::ExecutionStreamOpened { .. } => 56,
        Response::ExecutionStreams(_) => 57,
        Response::ExecutionAccountPositions(_) => 58,
        Response::Refused(_) => 59,
    }
}

/// How many [`Response`] variants [`response_variant_index`] numbers.
const RESPONSE_VARIANTS: usize = 60;

fn bar(i: i64) -> Bar {
    Bar {
        timestamp: 1_700_000_000_000 + i * 60_000,
        open: 50_000.5 + i as f64,
        high: 50_010.25,
        low: 49_990.125,
        close: 0.1 + 0.2,
        volume: 12.345_678_9,
        funding: (i % 2 == 0).then_some(-0.000_1),
        bid: None,
        ask: Some(50_001.0),
        symbol: Some("BTCUSDT".into()),
    }
}

/// The kept lists recorded from the venues, the rows `InstrumentLists` carries (binance's and okx's).
fn recorded_kept_lists() -> Vec<vike_catalog::VenueInstrumentList> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../vike-catalog/tests/fixtures/instruments");
    ["binance.json", "okx.json"]
        .iter()
        .map(|file| {
            let text =
                std::fs::read_to_string(format!("{dir}/{file}")).expect("a recorded kept list");
            serde_json::from_str(&text).expect("a recorded kept list parses")
        })
        .collect()
}

/// One sample of EVERY [`Response`] variant: real rows where the hot replies carry them, the smallest
/// well-formed payload elsewhere (the loopback suites cross the rows of each read in the negotiated form).
fn one_of_every_response() -> Vec<Response> {
    let series = SeriesId::per_symbol(StoreKindId::Bar, "binance", "BTCUSDT", Some("1h".into()));
    let coverage = SeriesCoverage {
        first_timestamp: 1,
        last_timestamp: 2,
        rows: 3,
        bytes: 4,
        parts: 5,
        dates: 6,
    };
    let spec = MarketDataSpec {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        lane: StreamLabel::Trades,
        depth_levels: Some(20),
    };
    let run = BackfillRunWire {
        id: 7,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        start: 1,
        end: 2,
        peer: Some("127.0.0.1:5".into()),
        started_ms: 3,
        elapsed_ms: 4,
        lane: "klines".into(),
        stoppable: true,
        cancelled: false,
        boundaries: 9,
    };
    let from = |json: serde_json::Value| -> Response {
        serde_json::from_value(json).expect("a sample response parses")
    };
    let instrument_lists = InstrumentListsReply {
        venues: recorded_kept_lists(),
        operator: Vec::new(),
        splits: Vec::new(),
    };
    vec![
        Response::Pong,
        Response::Error("a refusal keeps its words: «ü» \u{1F600}".into()),
        Response::Refused(crate::refusals::DatahubRefusal::StrategyLaneFull { cap: 3 }.into()),
        Response::Bars((0..500).map(bar).collect()),
        Response::Quotes(vec![TickQuote {
            timestamp: 1,
            local_timestamp: 2,
            bid: 99.5,
            ask: 100.25,
            bid_size: 0.001,
            ask_size: 3.0,
            symbol: "BTCUSDT".into(),
        }]),
        Response::Trades(vec![TickTrade {
            timestamp: 1,
            local_timestamp: 2,
            price: 100.125,
            size: 0.5,
            is_buyer_maker: true,
            symbol: "BTCUSDT".into(),
        }]),
        Response::BookUpdates(Vec::new()),
        Response::Depth(Vec::new()),
        Response::Cohort(Vec::new()),
        Response::PerpMetrics(Vec::new()),
        Response::Texts(Vec::new()),
        Response::Chain(Vec::new()),
        Response::FinancialFacts(Vec::new()),
        Response::InstitutionalHoldings(Vec::new()),
        Response::TextScores(Vec::new()),
        Response::Equity(vec![vike_model::EquitySample {
            timestamp: 1,
            venue: "binance".into(),
            equity: 10_000.5,
            realized: -1.25,
            unrealized: 0.0,
            missing_prices: 2,
        }]),
        Response::ExecFills(Vec::new()),
        Response::ExecOrders(Vec::new()),
        Response::Forecasts(Vec::new()),
        Response::Properties(Some(Box::default())),
        Response::SeriesList(vec![series.clone(); 2_000]),
        Response::Inventory(vec![(series.clone(), coverage.clone())]),
        Response::ExecutionSeries(ExecutionSeries {
            series: vec![(series.clone(), coverage.clone())],
            venues: [("binance".to_string(), ["BTCUSDT".to_string()].into())].into(),
        }),
        Response::SeriesGaps(
            (0..300).map(|i| (i * 86_400_000, i * 86_400_000 + 3_600_000)).collect(),
        ),
        Response::SeriesFacts(Box::new((coverage.clone(), vec!["k1".into(), "k2".into()]))),
        Response::SeriesDays(Some(
            (0..3_363)
                .map(|d| BarDay {
                    day: d * 86_400_000,
                    stamp: d as u64 * 7,
                    provisional: d == 3_362,
                })
                .collect(),
        )),
        Response::Coverage(Vec::new()),
        Response::BackfillDone(BackfillDone {
            rows_written: 3,
            first_timestamp: Some(1),
            last_timestamp: None,
        }),
        Response::RunningBackfills(vec![run.clone()]),
        Response::BackfillsCancelled(BackfillCancelDone {
            flagged: vec![run],
            unstoppable: Vec::new(),
        }),
        from(serde_json::json!({"HistoryChannels": {"as_of_ms": 5, "venues": []}})),
        from(serde_json::json!({"GatewayStatus": {"reached": false, "error": "refused"}})),
        from(serde_json::json!({"GatewayLinks": {"rows": [], "truncated": true}})),
        from(
            serde_json::json!({"CredentialsChanged": {"reached": true, "reconnected": ["binance"]}}),
        ),
        Response::SeriesSeeded(SeedDone {
            armed: true,
            repeated: false,
            rows_written: 4,
            range: Some((1, 2)),
            first_timestamp: Some(1),
            last_timestamp: Some(2),
        }),
        Response::VenueCatalog(CatalogListing {
            venue: "binance".into(),
            outcome: CatalogOutcome::NotArmed,
        }),
        from(serde_json::json!({"OptionsChain": {
            "venue": "deribit", "underlying": "BTC", "served_underlyings": ["BTC", "ETH"], "outcome": "NotArmed"
        }})),
        Response::OptionsRefreshed { venue: "deribit".into(), underlyings: vec!["BTC".into()] },
        Response::NewsFeed(NewsReply { outcome: DatahubPollOutcome::Warming }),
        Response::EconomicCalendar(CalendarEconomicReply {
            outcome: DatahubPollOutcome::Unchanged { generation: 4 },
        }),
        Response::EquityCalendar(CalendarEquityReply {
            earnings: DatahubPollOutcome::NotArmed,
            dividends: DatahubPollOutcome::Warming,
            ipos: DatahubPollOutcome::NotConfigured { key_name: "FMP_API_KEY".into() },
        }),
        Response::Deleted(DeleteDone {
            plan: RemovalPlan {
                selector: SeriesSelector {
                    kind: StoreKindId::Bar,
                    venue: "binance".into(),
                    symbol: Some("BTCUSDT".into()),
                    group: None,
                    interval: None,
                    source: None,
                },
                produced_by: None,
                series: Vec::new(),
            },
            outcome: None,
        }),
        from(serde_json::json!({"ArchiveImported": {"plan": {
            "format": "dukascopy-bi5", "dataset": "EURUSD", "venue": "dukascopy", "server_dir": "/x",
            "dir": "Present", "admission": "ok",
            "inventory": {"daily_files": 1, "first_day": 1, "last_day": 2, "other_layout_days": [],
                          "other_objects": 0, "other_bytes": 0, "skipped": []},
            "from_day": 1, "to_day": 2, "days": [], "gaps": [3], "bars": ["1m"], "series": null
        }, "outcome": null}})),
        from(serde_json::json!({"ArchiveFetched": {"plan": {
            "source": "s3", "server_dir": "/x", "list_requests": 1, "listed": 2, "listed_bytes": 3, "held": 4,
            "missing": []
        }, "fetched": null}})),
        Response::MarketDataSubscribed {
            session: MarketDataSessionId(u128::MAX),
            accepted: vec![spec.clone()],
            refused: Vec::new(),
            heartbeat_ms: 5_000,
        },
        Response::MarketDataUpdated {
            accepted: Vec::new(),
            refused: Vec::new(),
            released: vec![spec],
        },
        Response::StrategyOpened { session: 3, heartbeat_ms: 5_000 },
        Response::MarketData(Box::new(MarketDataFrame::Trades {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            ticks: Vec::new(),
            sequence: 8,
        })),
        Response::CsvUploadReady,
        Response::CsvUploadReceived { bytes: 1_024 },
        Response::CsvUploaded(Box::default()),
        Response::SymbolSets(Vec::new()),
        Response::InstrumentLists(instrument_lists),
        Response::InstrumentListsRefreshed(Vec::new()),
        Response::OperatorInstruments(OperatorInstrumentsReply {
            outcomes: Vec::new(),
            rows: Vec::new(),
            list_error: Some("a list error".into()),
        }),
        from(
            serde_json::json!({"SplitWarnings": [{"venue": "alpaca", "symbol": "NVDA", "date": "2024-06-10",
            "new_shares": 10, "old_shares": 1, "price_before": 1208.88, "price_after": 121.79}]}),
        ),
        from(serde_json::json!({"LiveFamilyTokens": [{"family": "btc-updown", "token": "123",
            "outcome": "Up", "end_ms": null}]})),
        Response::ExecutionStreamOpened {
            stored_through: Some(9),
            stored_through_recorded_ms: None,
            heartbeat_ms: 1_000,
        },
        Response::ExecutionStreams(Vec::new()),
        Response::ExecutionAccountPositions(Vec::new()),
    ]
}

/// Every request variant crosses every form as its JSON value: the client's frames.
#[test]
fn every_request_crosses_msgpack_and_lz4_as_the_value_json_carries() {
    for request in one_of_every_request() {
        crosses_every_form(&request, request_kind(&request));
    }
}

/// Every response variant crosses every form as its JSON value, each variant sampled (the exhaustive index
/// counts them): the datahub's frames, the hot replies with real rows.
#[test]
fn every_response_crosses_msgpack_and_lz4_as_the_value_json_carries() {
    let responses = one_of_every_response();
    let covered: std::collections::BTreeSet<usize> =
        responses.iter().map(response_variant_index).collect();
    assert_eq!(covered.len(), RESPONSE_VARIANTS, "a Response variant has no sample");
    for (i, response) in responses.iter().enumerate() {
        crosses_every_form(response, &format!("response sample {i}"));
    }
}

/// Every datahub reply and request of the committed wire goldens crosses every form as its JSON value: what a
/// real peer of an older build sent and an older one reads, msgpack-read and written back the same.
#[test]
fn every_golden_datahub_frame_crosses_every_form() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/datahub_wire");
    let mut crossed = 0;
    for entry in std::fs::read_dir(dir).expect("the datahub goldens") {
        let path = entry.expect("an entry").path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        for line in std::fs::read_to_string(&path).expect("a golden").lines() {
            let line = line.trim();
            if let Ok(response) = serde_json::from_str::<Response>(line) {
                crosses_every_form(&response, &path.display().to_string());
                crossed += 1;
            } else if let Ok(request) = serde_json::from_str::<Request>(line) {
                crosses_every_form(&request, &path.display().to_string());
                crossed += 1;
            }
        }
    }
    assert!(crossed > 20, "only {crossed} golden frames were read");
}

/// A value crosses msgpack with every float's bits, the non-finite ones too (JSON writes those `null`): an
/// optional `funding` that is NaN reads back NaN, where JSON reads `None`.
#[test]
fn a_msgpack_bar_keeps_every_bit() {
    let mut odd = bar(1);
    odd.funding = Some(f64::NAN);
    odd.open = -0.0;
    odd.high = f64::MIN_POSITIVE;
    for codec in [BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
        let body = body_of(codec, &Response::Bars(vec![odd.clone()]), false);
        let Response::Bars(back) = decode_body(&body).expect("decodes") else { panic!("not bars") };
        let bits = |b: &Bar| [b.open, b.high, b.funding.expect("present")].map(f64::to_bits);
        assert_eq!(bits(&back[0]), bits(&odd), "{codec:?}");
    }
}
