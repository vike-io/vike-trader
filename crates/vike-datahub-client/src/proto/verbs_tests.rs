use std::io::Cursor;

use crate::proto::*;
use vike_data::StoreKindId;
use vike_hub_protocol::auth::{VerbScope, scope_admits};
use vike_hub_protocol::conn::ReplyWait;
use vike_hub_protocol::plane::Plane;
use vike_model::StreamLabel;

/// The variant each request maps to, through a match with NO `_` arm — so a new verb fails to
/// COMPILE here until it is given a row, and then fails
/// [`every_verb_is_classified_and_names_itself_by_its_wire_tag`] until it is given a sample.
fn variant_index(r: &Request) -> usize {
    match r {
        Request::Hello { .. } => 0,
        Request::Auth { .. } => 1,
        Request::Ping => 2,
        Request::LoadBars { .. } => 3,
        Request::ScanQuotes { .. } => 4,
        Request::ScanTrades { .. } => 5,
        Request::ScanBookUpdates { .. } => 6,
        Request::ScanDepth { .. } => 7,
        Request::ScanCohort { .. } => 8,
        Request::ScanPerpMetrics { .. } => 9,
        Request::ScanEquity { .. } => 10,
        Request::ScanExecFills { .. } => 11,
        Request::PropertiesAsOf { .. } => 12,
        Request::ListSeries => 13,
        Request::Inventory => 14,
        Request::SeriesFacts { .. } => 15,
        Request::SeriesGaps { .. } => 16,
        Request::Coverage => 17,
        Request::Backfill { .. } => 18,
        Request::ImportArchive(_) => 19,
        Request::SeedSeries { .. } => 20,
        Request::VenueCatalog { .. } => 21,
        Request::DeleteSeries { .. } => 22,
        Request::MarketDataSubscribe { .. } => 23,
        Request::MarketDataUpdate { .. } => 24,
        Request::ListBackfills => 25,
        Request::CancelBackfill { .. } => 26,
        Request::HistoryChannels => 27,
        Request::ScanExecOrders { .. } => 28,
        Request::OptionsChain { .. } => 29,
        Request::NewsFeed { .. } => 30,
        Request::EconomicCalendar { .. } => 31,
        Request::EquityCalendar { .. } => 32,
        Request::StrategyOpen { .. } => 33,
        Request::GatewayStatus => 34,
        Request::GatewayLinks { .. } => 35,
        Request::StrategyFundingHistory { .. } => 36,
        Request::CsvUpload(_) => 37,
        Request::CsvUploadChunk { .. } => 38,
        Request::CsvUploadEnd => 39,
        Request::StrategyInputBars { .. } => 40,
        Request::OptionsRefresh { .. } => 41,
        Request::SymbolSets => 42,
        Request::SetSymbolSet { .. } => 43,
        Request::DeleteSymbolSet { .. } => 44,
        Request::InstrumentLists => 45,
        Request::RefreshInstrumentLists { .. } => 46,
        Request::FetchArchive(_) => 47,
        Request::OperatorInstruments { .. } => 48,
    }
}

/// How many [`Request`] variants [`variant_index`] numbers.
const REQUEST_VARIANTS: usize = 49;

/// One sample of EVERY [`Request`] variant.
fn one_of_every_request() -> Vec<Request> {
    let series =
        || SeriesId::per_symbol(StoreKindId::Bar, "binance", "BTCUSDT", Some("1h".to_string()));
    let venue = "binance".to_string();
    let symbol = "BTCUSDT".to_string();
    let market_data = crate::market::MarketDataSpec {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        lane: StreamLabel::Trades,
        depth_levels: None,
    };
    vec![
        Request::Hello { proto_version: PROTO_VERSION },
        Request::Auth { scope: Scope::Read, mac: vec![0u8; 32] },
        Request::Ping,
        Request::LoadBars {
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            start: None,
            end: None,
            limit: None,
            source: None,
        },
        Request::ScanQuotes {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
            source: None,
        },
        Request::ScanTrades {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanBookUpdates {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanDepth {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanCohort {
            venue: venue.clone(),
            asset: "BTC".to_string(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanPerpMetrics {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanEquity {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanExecFills {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanExecOrders {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
        },
        Request::PropertiesAsOf { venue: venue.clone(), symbol: symbol.clone(), timestamp: 0 },
        Request::ListSeries,
        Request::Inventory,
        Request::SeriesFacts { id: series() },
        Request::SeriesGaps { id: series() },
        Request::Coverage,
        Request::Backfill {
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            start: 0,
            end: 1,
        },
        Request::ImportArchive(crate::archive::ImportSpec {
            format: "dukascopy-bi5".to_string(),
            dataset: "EURUSD".to_string(),
            from_day: None,
            to_day: None,
            bars: vec!["1m".to_string()],
            dry_run: true,
            verify: false,
            replace_holed: false,
            target: None,
        }),
        Request::FetchArchive(crate::archive_fetch::FetchSpec {
            format: "dukascopy-bi5".to_string(),
            dataset: "EURUSD".to_string(),
            from_day: 0,
            to_day: 0,
            go: false,
            concurrency: 8,
        }),
        Request::SeedSeries {
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            class: None,
        },
        Request::VenueCatalog { venue: venue.clone() },
        Request::DeleteSeries {
            selector: SeriesSelector::new(StoreKindId::Bar, "binance"),
            produced_by: Some("klines:".to_string()),
            dry_run: true,
        },
        Request::MarketDataSubscribe { specs: vec![market_data.clone()] },
        Request::MarketDataUpdate {
            session: crate::market::MarketDataSessionId(7),
            add: vec![market_data],
            remove: vec![],
        },
        Request::ListBackfills,
        Request::CancelBackfill { venue, symbol, interval: "1h".to_string() },
        Request::HistoryChannels,
        Request::GatewayStatus,
        Request::GatewayLinks {
            since_ms: 1,
            route: Some("bybit#DEMO".to_string()),
            venue: Some("bybit".to_string()),
        },
        Request::OptionsChain {
            venue: "deribit".to_string(),
            underlying: "BTC".to_string(),
            expiries: vec!["2026-06-26".to_string()],
        },
        Request::OptionsRefresh { venue: "deribit".to_string() },
        Request::NewsFeed { if_generation: Some(3) },
        Request::EconomicCalendar { if_generation: None },
        Request::EquityCalendar { week_start: "2026-10-12".to_string(), if_generation: Some(9) },
        Request::StrategyOpen {
            venue: "hyperliquid".to_string(),
            network: Some("testnet".to_string()),
        },
        Request::StrategyFundingHistory {
            venue: "binance".to_string(),
            symbol: "BTCUSDT.P".to_string(),
            start: 1,
            end: 2,
        },
        Request::StrategyInputBars {
            venue: "binance".to_string(),
            symbol: "USDT-USD".to_string(),
            interval: "1d".to_string(),
            source: Some("user-coinbase".to_string()),
            start: 1,
            end: 2,
        },
        Request::CsvUpload(crate::upload::CsvUploadSpec {
            target: crate::archive::ImportTarget {
                venue: "user".to_string(),
                symbol: "USDT-USD".to_string(),
                interval: "1d".to_string(),
                label: "coinbase".to_string(),
                file_format: vike_data::csv_bars::CsvBarsFormatWords::default(),
                columns: None,
            },
            bytes: 10,
            dry_run: true,
            head: false,
        }),
        Request::CsvUploadChunk { text: "ts,close\n".to_string() },
        Request::CsvUploadEnd,
        Request::SymbolSets,
        Request::SetSymbolSet {
            set: vike_data::datasets::SymbolSet {
                name: "Crypto Majors".to_string(),
                symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
                provider: "binance".to_string(),
                interval: "1m".to_string(),
                benchmark: String::new(),
                user: true,
            },
        },
        Request::DeleteSymbolSet { name: "Crypto Majors".to_string() },
        Request::InstrumentLists,
        Request::RefreshInstrumentLists { venue: Some("binance".to_string()) },
        Request::OperatorInstruments {
            venue: "ig".to_string(),
            add: vec!["EURUSD".to_string()],
            remove: Vec::new(),
            stated: None,
        },
    ]
}

/// A request's WIRE TAG, read off its encoding — the externally-tagged key, or the bare string of a
/// unit variant.
fn wire_tag(r: &Request) -> String {
    match serde_json::to_value(r).expect("every request encodes") {
        serde_json::Value::String(s) => s,
        serde_json::Value::Object(m) => {
            assert_eq!(m.len(), 1, "an externally-tagged request has ONE outer key: {m:?}");
            m.keys().next().expect("one key").clone()
        }
        other => panic!("a request encoded as neither a tag nor a tagged object: {other}"),
    }
}

/// ⚠ **A NEW VERB CANNOT BE MISSED.** [`variant_index`] has no `_` arm, so a variant added to
/// [`Request`] fails to compile until it is numbered; this test then fails until the sample set
/// carries it — and for every sample it holds the exhaustive classifiers to one rule a compiler cannot
/// check: [`request_kind`] names the verb by the tag the WIRE carries, the one an operator can grep a
/// capture for. ([`required_scope`] is exhaustive by construction; WHICH arm it chose is pinned verb by
/// verb in
/// `crates/vike-datahub/tests/default_build/auth_roundtrip/scope_split.rs`'s
/// `the_verb_scope_classification_is_pinned` and, for this crate's newest verb, below.)
#[test]
fn every_verb_is_classified_and_names_itself_by_its_wire_tag() {
    let samples = one_of_every_request();
    let mut seen = [false; REQUEST_VARIANTS];
    for r in &samples {
        let i = variant_index(r);
        assert!(i < REQUEST_VARIANTS, "REQUEST_VARIANTS is stale: {r:?} maps to {i}");
        seen[i] = true;
        assert_eq!(request_kind(r), wire_tag(r), "request_kind must name the WIRE tag: {r:?}");
        // Exhaustive and total: neither may panic on any variant.
        let _ = (required_scope(r), reply_wait(r));
    }
    let missing: Vec<usize> = (0..REQUEST_VARIANTS).filter(|i| !seen[*i]).collect();
    assert!(missing.is_empty(), "variant indices with no sample: {missing:?}");
}

/// The tags serde itself says this enum has, off the `expected one of ...` list of an unknown tag. That
/// list is what a daemon of this plane refuses a frame with, so reading it here means the test sees the
/// enum the wire sees, and no sample list can go stale.
fn the_enums_own_tags() -> Vec<String> {
    let err = serde_json::from_str::<Request>(r#""__no_such_verb__""#)
        .expect_err("an unknown tag does not decode")
        .to_string();
    assert!(err.contains("unknown variant `__no_such_verb__`"), "{err}");
    let list = err.split("expected one of").nth(1).expect("serde lists the variants");
    list.split('`').skip(1).step_by(2).map(str::to_string).collect()
}

/// ⚠ **THE WRONG-PLANE REFUSAL STANDS ON THIS.** The BACKTEST daemon names a verb of this plane only if
/// the tag is on `vike_hub_protocol::plane`'s roster, and that roster cannot name this enum's variants by
/// itself. So: this enum's tags are exactly that roster plus the three frames both daemons serve. A verb
/// added here without its row would be answered `undecodable` by the other daemon, a parse failure where
/// an operator is owed the address.
#[test]
fn this_enum_is_the_data_planes_roster_and_the_shared_frames() {
    let mut own = the_enums_own_tags();
    own.sort();
    let mut roster: Vec<String> = Plane::Data.verbs().iter().map(|v| v.to_string()).collect();
    roster.extend(["Hello", "Auth", "Ping"].map(str::to_string));
    roster.sort();
    assert_eq!(own, roster);
}

/// ⚠ WHAT SERDE DOES WITH A VERB OF THE OTHER PLANE, the fact the refusal is built on: it is an `Err`
/// naming the tag, for a unit tag and a tagged object alike, never a panic and never a default.
#[test]
fn a_verb_of_the_other_plane_does_not_decode_here() {
    for body in [&br#""ListStrategies""#[..], br#"{"RunBacktest":"[data]"}"#] {
        let err = serde_json::from_slice::<Request>(body).expect_err("not this enum's").to_string();
        assert!(err.contains("unknown variant"), "{err}");
    }
    assert_eq!(
        vike_hub_protocol::plane::foreign_verb(br#""ListStrategies""#, Plane::Data),
        Some("ListStrategies")
    );
}

/// ⚠ WHICH VERBS WAIT UNBOUNDED, pinned verb by verb over the full sample set: [`reply_wait`] is
/// exhaustive, so a new verb cannot compile without a class, but WHICH class it got is a judgement.
/// A verb moved INTO `Unbounded` parks its caller on a dead link; one moved OUT clips a live
/// backfill or import at 300 s.
#[test]
fn only_the_client_sized_verbs_wait_unbounded() {
    let unbounded =
        ["Backfill", "ImportArchive", "FetchArchive", "CsvUploadEnd", "OperatorInstruments"];
    for r in one_of_every_request() {
        let kind = request_kind(&r);
        let expected = match kind {
            "Hello" | "Auth" => ReplyWait::Handshake,
            k if unbounded.contains(&k) => ReplyWait::Unbounded,
            _ => ReplyWait::Bounded,
        };
        assert_eq!(reply_wait(&r), expected, "{kind}");
    }
}

/// ⚠ THE ARCHIVE IMPORT'S THREE CLASSIFIERS, asserted rather than trusted to the compiler: it
/// WRITES the data daemon's store, so it is a DATA-plane verb, it is Control-scoped for the
/// reasons `required_scope`'s arm carries, and it names itself by its wire tag.
#[test]
fn the_import_verb_is_a_control_scoped_data_verb() {
    let r = Request::ImportArchive(crate::archive::ImportSpec {
        format: "dukascopy-bi5".to_string(),
        dataset: "EURUSD".to_string(),
        from_day: None,
        to_day: None,
        bars: vec![],
        dry_run: true,
        verify: false,
        replace_holed: false,
        target: None,
    });
    assert!(Plane::Data.verbs().contains(&request_kind(&r)));
    assert_eq!(required_scope(&r), VerbScope::Write);
    assert_eq!(request_kind(&r), "ImportArchive");
    // ...and an Observe credential may not send it, while a Control one may.
    assert!(!scope_admits(Scope::Read, required_scope(&r)));
    assert!(scope_admits(Scope::Write, required_scope(&r)));
}

/// ⚠ THE CSV UPLOAD'S THREE VERBS (decision 0272): data-plane verbs an OBSERVE key may send, which is
/// the whole point of them (the PC holds no Control key), and a Trade key may not. The import, the
/// backfill and the delete stay Control: the upload widened no other verb.
#[test]
fn the_upload_verbs_are_observe_scoped_data_verbs_and_nothing_else_moved() {
    let upload: Vec<Request> = one_of_every_request()
        .into_iter()
        .filter(|r| request_kind(r).starts_with("CsvUpload"))
        .collect();
    assert_eq!(upload.len(), 3);
    for r in &upload {
        assert!(Plane::Data.verbs().contains(&request_kind(r)), "{r:?}");
        assert_eq!(required_scope(r), VerbScope::Read, "{r:?}");
        assert!(scope_admits(Scope::Read, required_scope(r)), "an observe key may upload");
        assert!(!scope_admits(Scope::Trade, required_scope(r)), "a lane key may not");
    }
    for r in one_of_every_request() {
        if matches!(
            request_kind(&r),
            "Backfill" | "ImportArchive" | "FetchArchive" | "DeleteSeries"
        ) {
            assert!(!scope_admits(Scope::Read, required_scope(&r)), "{r:?} stays Control");
        }
    }
}

/// ⚠ THE BACKFILL REGISTRY'S TWO VERBS, asserted rather than trusted to the compiler —
/// `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`.
/// Both are DATA-plane (the registry is the data daemon's); the list is Observe and the cancel is
/// Control — an Observe key may see what the operator is fetching and may not stop any of it.
#[test]
fn the_backfill_registry_verbs_are_an_observe_list_and_a_control_cancel() {
    let list = Request::ListBackfills;
    let cancel = Request::CancelBackfill {
        venue: "oanda".to_string(),
        symbol: "EUR_USD".to_string(),
        interval: "5s".to_string(),
    };
    for r in [&list, &cancel] {
        assert!(Plane::Data.verbs().contains(&request_kind(r)), "{r:?}");
    }
    assert_eq!(required_scope(&list), VerbScope::Read);
    assert_eq!(required_scope(&cancel), VerbScope::Write);
    assert!(scope_admits(Scope::Read, required_scope(&list)));
    assert!(!scope_admits(Scope::Read, required_scope(&cancel)), "Observe must not cancel");
    assert!(scope_admits(Scope::Write, required_scope(&cancel)));
    assert_eq!(request_kind(&list), "ListBackfills");
    assert_eq!(request_kind(&cancel), "CancelBackfill");
}

/// The gatewayhub's state is an Observe-scoped, server-bounded data verb, a bare unit tag on the wire, and a
/// reply survives the frame codec both ways: one from a NEWER server keeps decoding (its extra field is
/// dropped), and a down gatewayhub's writes no `report` key at all.
/// The history is a read, it says its version-free feature, a row's absent numbers are absent on the wire, and a reply
/// from a newer datahub with a field this client has never heard of still reads.
#[test]
fn the_session_history_is_an_observe_scoped_verb_and_its_answer_round_trips() {
    use crate::gateway_links::GatewayLinksReply;
    use vike_model::LinkSessionRow;
    assert_eq!(FEATURE_GATEWAY_LINKS, "gateway_links");
    let ask = Request::GatewayLinks { since_ms: 5, route: None, venue: None };
    assert_eq!(required_scope(&ask), VerbScope::Read);
    assert_eq!(request_kind(&ask), "GatewayLinks");
    assert_eq!(
        serde_json::to_value(&ask).unwrap(),
        serde_json::json!({"GatewayLinks": {"since_ms": 5}})
    );

    let row = LinkSessionRow {
        venue: "aster".to_string(),
        route: "aster".to_string(),
        plane: "trade".to_string(),
        lane: "aster-userdata".to_string(),
        opened_ms: 1_000,
        closed_ms: Some(907_000),
        lived_s: Some(906.0),
        how: Some("server closed: 1000".to_string()),
        ours: false,
        next_open_s: Some(1.5),
    };
    let reply = GatewayLinksReply { rows: vec![row], truncated: false };
    let mut buffer = Vec::new();
    write_frame(&mut buffer, &Response::GatewayLinks(reply.clone())).unwrap();
    match read_frame::<_, Response>(&mut buffer.as_slice()).unwrap() {
        Response::GatewayLinks(got) => assert_eq!(got, reply),
        other => panic!("expected GatewayLinks, got {other:?}"),
    }
    let newer: GatewayLinksReply =
        serde_json::from_str(r#"{"rows":[],"a_field_from_the_future":1}"#).unwrap();
    assert!(newer.rows.is_empty() && !newer.truncated);
}

#[test]
fn the_gateway_status_read_is_an_observe_scoped_data_verb_and_its_answer_round_trips() {
    use crate::gateway_status::{
        GatewayBudgetCounter, GatewayStatusReply, GatewayStatusReportWire,
    };
    assert_eq!(FEATURE_GATEWAY_STATUS, "gateway_status");
    let r = Request::GatewayStatus;
    assert!(Plane::Data.verbs().contains(&request_kind(&r)));
    assert_eq!(required_scope(&r), VerbScope::Read);
    assert!(scope_admits(Scope::Read, required_scope(&r)), "an Observe key may ask");
    assert_eq!(reply_wait(&r), ReplyWait::Bounded);
    assert_eq!(serde_json::to_value(&r).unwrap(), serde_json::json!("GatewayStatus"));

    let up = GatewayStatusReply {
        reached: true,
        error: None,
        report: Some(GatewayStatusReportWire {
            epoch: 7,
            started_ms: 1_000,
            uptime_ms: 3_600_000,
            connections: 2,
            stream_sessions: 1,
            fetches_running: 0,
            market_data_venues: vec!["binance".to_string()],
            credentialed_data_venues: Vec::new(),
            budget: Some(vec![GatewayBudgetCounter {
                venue: "binance".to_string(),
                host: "api".to_string(),
                scope: "ip".to_string(),
                class: "order".to_string(),
                used: 3,
                cap: 50,
                window_ms: 10_000,
                would_throttle: 0,
            }]),
        }),
    };
    let down = GatewayStatusReply {
        reached: false,
        error: Some("gatewayhub 127.0.0.1:7: cannot connect: refused".to_string()),
        report: None,
    };
    for reply in [up, down.clone()] {
        let mut buffer: Vec<u8> = Vec::new();
        write_frame(&mut buffer, &Response::GatewayStatus(reply.clone())).unwrap();
        match read_frame::<_, Response>(&mut Cursor::new(buffer)).unwrap() {
            Response::GatewayStatus(got) => assert_eq!(got, reply),
            other => panic!("expected GatewayStatus, got {other:?}"),
        }
    }
    let wire = serde_json::to_value(&down).unwrap();
    assert!(wire.get("report").is_none(), "{wire}");

    let newer: GatewayStatusReply =
        serde_json::from_value(serde_json::json!({"reached": false, "new_field": 1})).unwrap();
    assert_eq!((newer.reached, newer.error, newer.report), (false, None, None));

    // A datahub older than `budget` sends a report without it: decoded, and read as "said nothing".
    let older: GatewayStatusReportWire = serde_json::from_value(serde_json::json!({
        "epoch": 7, "started_ms": 1, "uptime_ms": 2, "connections": 0, "stream_sessions": 0,
        "fetches_running": 0,
    }))
    .unwrap();
    assert_eq!(older.budget, None);
}

/// ⚠ THE HISTORY-CHANNELS READ'S THREE CLASSIFIERS, asserted rather than trusted to the compiler —
/// `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md`. A DATA-plane verb (its
/// overlay is the data daemon's table, store and credential store), Observe-scoped, a bare unit tag
/// on the wire — and its answer, the client's own compiled table here, survives the frame codec.
#[test]
fn the_history_channels_read_is_an_observe_scoped_data_verb_and_its_answer_round_trips() {
    let r = Request::HistoryChannels;
    assert!(Plane::Data.verbs().contains(&request_kind(&r)));
    assert_eq!(required_scope(&r), VerbScope::Read);
    assert!(scope_admits(Scope::Read, required_scope(&r)), "an Observe key may ask");
    assert_eq!(request_kind(&r), "HistoryChannels");
    assert_eq!(
        serde_json::to_value(&r).expect("encode"),
        serde_json::json!("HistoryChannels"),
        "the request names NOTHING — a bare unit tag"
    );

    let report = crate::history::compiled_report(1_790_899_200_000);
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &Response::HistoryChannels(report.clone())).unwrap();
    match read_frame::<_, Response>(&mut Cursor::new(buffer)).unwrap() {
        Response::HistoryChannels(got) => assert_eq!(got, report),
        other => panic!("expected HistoryChannels, got {other:?}"),
    }
    assert_eq!(FEATURE_HISTORY_CHANNELS, "history_channels");
}

/// ⚠ THE ORDER-LIFECYCLE READ'S THREE CLASSIFIERS, asserted rather than trusted to the compiler: a
/// DATA-plane `HistoryStore` read (the data daemon's store), Observe-scoped like the six 0084 reads
/// beside it (it writes nothing), named by its wire tag — and its reply survives the frame codec.
#[test]
fn scan_execution_orders_is_a_data_plane_read() {
    let r = Request::ScanExecOrders {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        start: Some(1),
        end: Some(2),
        limit: Some(3),
    };
    assert!(Plane::Data.verbs().contains(&request_kind(&r)));
    assert_eq!(required_scope(&r), VerbScope::Read);
    assert!(scope_admits(Scope::Read, required_scope(&r)), "an Observe key may ask");
    assert_eq!(request_kind(&r), "ScanExecOrders");
}

/// ⚠ THE OPTIONS-CHAIN VERB'S THREE CLASSIFIERS, asserted rather than trusted to the compiler: the
/// DATA daemon serves it (it links the venue bridge and runs the poll), it is Observe-scoped for
/// the catalog's reason (`required_scope`'s arm), and it names itself by its wire tag — and both of
/// its frames survive the codec, every outcome of the reply included.
#[test]
fn the_options_chain_verb_is_an_observe_scoped_data_verb_and_its_frames_round_trip() {
    use crate::options::{OptionsChainOutcome, OptionsChainReply, OptionsRefusal};
    let r = Request::OptionsChain {
        venue: "deribit".to_string(),
        underlying: "BTC".to_string(),
        expiries: vec!["2026-06-26".to_string(), "2026-07-31".to_string()],
    };
    assert!(Plane::Data.verbs().contains(&request_kind(&r)));
    assert_eq!(required_scope(&r), VerbScope::Read);
    assert!(scope_admits(Scope::Read, required_scope(&r)), "an Observe key may ask");
    assert_eq!(request_kind(&r), "OptionsChain");
    assert_eq!(FEATURE_OPTIONS_CHAIN, "options_chain");

    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &r).unwrap();
    match read_frame::<_, Request>(&mut Cursor::new(buffer)).unwrap() {
        Request::OptionsChain { venue, underlying, expiries } => {
            assert_eq!((venue.as_str(), underlying.as_str()), ("deribit", "BTC"));
            assert_eq!(expiries, ["2026-06-26", "2026-07-31"]);
        }
        other => panic!("expected OptionsChain, got {other:?}"),
    }

    let mut chains = vike_marketdata::OptionChainSet {
        default_expiry: "2026-06-26".to_string(),
        ..Default::default()
    };
    chains.expiries.push(vike_marketdata::OptionChainExpiry {
        date: "2026-06-26".to_string(),
        dte: 3,
        label: "26 Jun".to_string(),
    });
    for outcome in [
        OptionsChainOutcome::Chains(chains),
        OptionsChainOutcome::NotArmed,
        OptionsChainOutcome::Refused(OptionsRefusal::UnknownVenue {
            supported: vec!["deribit".to_string()],
        }),
        OptionsChainOutcome::Refused(OptionsRefusal::UnknownUnderlying {
            supported: vec!["BTC".to_string()],
        }),
        OptionsChainOutcome::Refused(OptionsRefusal::TooManyExpiries { max: 8 }),
        OptionsChainOutcome::Refused(OptionsRefusal::NotReady { why: "warming".to_string() }),
    ] {
        // The served set rides every outcome, in canonical order — and SOL (the USDC book) with it.
        let served = match outcome {
            OptionsChainOutcome::NotArmed => Vec::new(),
            _ => vec!["BTC".to_string(), "ETH".to_string(), "SOL".to_string()],
        };
        let reply = OptionsChainReply {
            venue: "deribit".into(),
            underlying: "BTC".into(),
            served_underlyings: served,
            outcome,
        };
        let mut buffer: Vec<u8> = Vec::new();
        write_frame(&mut buffer, &Response::OptionsChain(reply.clone())).unwrap();
        match read_frame::<_, Response>(&mut Cursor::new(buffer)).unwrap() {
            Response::OptionsChain(got) => assert_eq!(got, reply),
            other => panic!("expected OptionsChain, got {other:?}"),
        }
    }
    // ...and the field is ON the wire under its own name, so a client can read it off any reply.
    let reply = OptionsChainReply {
        venue: "deribit".into(),
        underlying: "SOL".into(),
        served_underlyings: vec!["BTC".into(), "ETH".into(), "SOL".into()],
        outcome: OptionsChainOutcome::NotArmed,
    };
    let doc = serde_json::to_value(Response::OptionsChain(reply)).expect("encode");
    assert_eq!(
        doc["OptionsChain"]["served_underlyings"],
        serde_json::json!(["BTC", "ETH", "SOL"]),
        "{doc}"
    );
}

/// ⚠ THE OPTIONS LISTING'S REFRESH, its classifiers asserted rather than trusted to the compiler: a
/// DATA verb, Observe-scoped (the gatewayhub bounds the venue read, `required_scope`'s arm), bounded,
/// named by its wire tag, and its frames pinned to their exact bytes (the request's golden and the
/// reply's), which a rename of a tag or a field reddens.
#[test]
fn the_options_refresh_verb_is_an_observe_scoped_data_verb_and_its_frames_are_pinned() {
    let r = Request::OptionsRefresh { venue: "deribit".to_string() };
    assert!(Plane::Data.verbs().contains(&request_kind(&r)));
    assert_eq!(required_scope(&r), VerbScope::Read);
    assert!(scope_admits(Scope::Read, required_scope(&r)), "an Observe key may ask");
    assert_eq!(reply_wait(&r), ReplyWait::Bounded);
    assert_eq!(request_kind(&r), "OptionsRefresh");
    assert_eq!(FEATURE_OPTIONS_REFRESH, "options_refresh");
    assert_eq!(
        serde_json::to_string(&r).expect("encode"),
        r#"{"OptionsRefresh":{"venue":"deribit"}}"#
    );
    let reply = Response::OptionsRefreshed {
        venue: "deribit".to_string(),
        underlyings: vec!["BTC".to_string(), "ETH".to_string(), "SOL".to_string()],
    };
    assert_eq!(
        serde_json::to_string(&reply).expect("encode"),
        r#"{"OptionsRefreshed":{"venue":"deribit","underlyings":["BTC","ETH","SOL"]}}"#
    );
    let mut buffer: Vec<u8> = Vec::new();
    write_frame(&mut buffer, &reply).unwrap();
    match read_frame::<_, Response>(&mut Cursor::new(buffer)).unwrap() {
        Response::OptionsRefreshed { venue, underlyings } => {
            assert_eq!((venue.as_str(), underlyings.len()), ("deribit", 3));
        }
        other => panic!("expected OptionsRefreshed, got {other:?}"),
    }
}

/// ⚠ THE THREE TOOL-FEED VERBS' CLASSIFIERS, asserted rather than trusted to the compiler: the DATA
/// daemon serves them, an Observe key may ask, the reply wait is the server-bounded class (cache
/// reads), each names itself by its wire tag, and the frames — every outcome of every reply
/// included — survive the codec.
#[test]
fn the_tool_feed_verbs_are_observe_scoped_server_bounded_data_verbs_and_their_frames_round_trip() {
    use crate::tool_feeds::{
        CalendarDividendRow, CalendarEarningsRow, CalendarEconomicReply, CalendarEquityReply,
        CalendarEvent, CalendarIpoRow, NewsItem, NewsReply, ToolFeedOutcome, ToolFeedRefusal,
    };
    assert_eq!(FEATURE_TOOL_FEEDS, "tool_feeds");
    let requests = [
        (Request::NewsFeed { if_generation: Some(4) }, "NewsFeed"),
        (Request::EconomicCalendar { if_generation: None }, "EconomicCalendar"),
        (
            Request::EquityCalendar {
                week_start: "2026-10-12".to_string(),
                if_generation: Some(1),
            },
            "EquityCalendar",
        ),
    ];
    for (r, kind) in requests {
        assert!(Plane::Data.verbs().contains(&kind), "{kind}");
        assert_eq!(required_scope(&r), VerbScope::Read, "{kind}");
        assert!(scope_admits(Scope::Read, required_scope(&r)), "an Observe key may ask: {kind}");
        assert_eq!(reply_wait(&r), ReplyWait::Bounded, "{kind}");
        assert_eq!(request_kind(&r), kind);
        let mut buffer: Vec<u8> = Vec::new();
        write_frame(&mut buffer, &r).unwrap();
        let back = read_frame::<_, Request>(&mut Cursor::new(buffer)).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&r).unwrap());
    }
    // an absent `if_generation` decodes as None: a client that never held one may omit the key
    let bare: Request = serde_json::from_value(serde_json::json!({"NewsFeed": {}})).unwrap();
    assert!(matches!(bare, Request::NewsFeed { if_generation: None }));

    fn outcomes<T: Clone>(rows: Vec<T>) -> Vec<ToolFeedOutcome<T>> {
        vec![
            ToolFeedOutcome::Snapshot {
                rows,
                fetched_at_ms: 1_760_000_000_000,
                generation: 12,
                sources_ok: 15,
                sources_total: 16,
                cadence_secs: 300,
            },
            ToolFeedOutcome::Snapshot {
                rows: Vec::new(),
                fetched_at_ms: 1,
                generation: 1,
                sources_ok: 0,
                sources_total: 1,
                cadence_secs: 0,
            },
            ToolFeedOutcome::Unchanged { generation: 12 },
            ToolFeedOutcome::Warming,
            ToolFeedOutcome::NotConfigured { key_name: "FINNHUB_API_KEY".to_string() },
            ToolFeedOutcome::NotArmed,
            ToolFeedOutcome::Refused(ToolFeedRefusal::WeekOutOfRange {
                allowed_from: "2026-10-05".to_string(),
                allowed_to: "2026-10-19".to_string(),
            }),
        ]
    }
    fn round_trip(response: Response) {
        let mut buffer: Vec<u8> = Vec::new();
        write_frame(&mut buffer, &response).unwrap();
        let back = read_frame::<_, Response>(&mut Cursor::new(buffer)).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&response).unwrap());
        match (&response, &back) {
            (Response::NewsFeed(a), Response::NewsFeed(b)) => assert_eq!(a, b),
            (Response::EconomicCalendar(a), Response::EconomicCalendar(b)) => assert_eq!(a, b),
            (Response::EquityCalendar(a), Response::EquityCalendar(b)) => assert_eq!(a, b),
            other => panic!("the variant changed on the wire: {other:?}"),
        }
    }
    let news = NewsItem {
        source: "CoinDesk".to_string(),
        title: "BTC rallies".to_string(),
        summary: "A summary.".to_string(),
        url: "https://example.test/a".to_string(),
        market: "crypto".to_string(),
        tags: vec!["Markets".to_string()],
        timestamp_ms: 1_760_000_000_000,
    };
    let event = CalendarEvent {
        timestamp_ms: 1_760_000_000_000,
        currency: "USD".to_string(),
        title: "CPI".to_string(),
        impact: "High".to_string(),
        forecast: "0.3%".to_string(),
        previous: "0.2%".to_string(),
        actual: String::new(),
    };
    let earn = CalendarEarningsRow {
        date: "2026-10-13".to_string(),
        symbol: "JPM".to_string(),
        hour: "Pre-mkt".to_string(),
        eps_est: "4.80".to_string(),
        eps_act: "5.00".to_string(),
        surprise: "+4.2%".to_string(),
    };
    let div = CalendarDividendRow {
        date: "2026-10-14".to_string(),
        symbol: "KO".to_string(),
        pay_date: "2026-11-01".to_string(),
        amount: "0.51 $".to_string(),
        yield_percent: "3.00%".to_string(),
        freq: "Quarterly".to_string(),
    };
    let ipo = CalendarIpoRow {
        date: "2026-10-15".to_string(),
        symbol: "NEW".to_string(),
        company: "New Co".to_string(),
        exchange: "NASDAQ".to_string(),
        price: "$18.00".to_string(),
        shares: "10,000,000".to_string(),
        status: "Upcoming".to_string(),
    };
    for outcome in outcomes(vec![news]) {
        round_trip(Response::NewsFeed(NewsReply { outcome }));
    }
    for outcome in outcomes(vec![event]) {
        round_trip(Response::EconomicCalendar(CalendarEconomicReply { outcome }));
    }
    for ((earnings, dividends), ipos) in
        outcomes(vec![earn]).into_iter().zip(outcomes(vec![div])).zip(outcomes(vec![ipo]))
    {
        round_trip(Response::EquityCalendar(CalendarEquityReply { earnings, dividends, ipos }));
    }
}

/// **Who may call each verb, per authenticated scope**: every verb of the sample set against every
/// `Scope`. `Scope::Trade` is not a superset in either direction (it sends the strategy lane's open and
/// nothing else, no read, no backfill, no handshake frame, and no other scope opens the lane), and
/// `Scope::Account` sends nothing.
#[test]
fn who_may_call_each_verb_is_pinned_per_scope() {
    const WRITE: [&str; 9] = [
        "Backfill",
        "CancelBackfill",
        "ImportArchive",
        "FetchArchive",
        "DeleteSeries",
        "SetSymbolSet",
        "DeleteSymbolSet",
        "RefreshInstrumentLists",
        "OperatorInstruments",
    ];
    let kinds: Vec<&str> = one_of_every_request().iter().map(request_kind).collect();
    for w in WRITE {
        assert!(kinds.contains(&w), "the sample set lacks the Write verb {w}");
    }
    for r in one_of_every_request() {
        let kind = request_kind(&r);
        let callers: &[Scope] = match kind {
            "Hello" | "Auth" => &[],
            "StrategyOpen" | "StrategyFundingHistory" | "StrategyInputBars" => &[Scope::Trade],
            k if WRITE.contains(&k) => &[Scope::Write],
            _ => &[Scope::Read, Scope::Write],
        };
        for authed in [Scope::Read, Scope::Write, Scope::Account, Scope::Trade] {
            let admitted = scope_admits(authed, required_scope(&r));
            assert_eq!(admitted, callers.contains(&authed), "{authed:?} {kind}");
        }
    }
}

/// **The strategy lane's open and its two store reads (stored funding, an input's bars) are the ONLY `Trade`
/// verbs**, data-plane verbs whose replies the server bounds, and no scope but `Trade` may send any of them:
/// over the whole sample set, so a fourth verb that slips into the `Trade` scope (or a lane verb that slips
/// out of it) fails here by name.
#[test]
fn the_strategy_lane_verbs_are_the_only_trade_verbs() {
    const LANE: [&str; 3] = ["StrategyOpen", "StrategyFundingHistory", "StrategyInputBars"];
    let trade_verbs: Vec<&str> = one_of_every_request()
        .iter()
        .filter(|r| required_scope(r) == VerbScope::Trade)
        .map(request_kind)
        .collect();
    assert_eq!(trade_verbs, LANE, "the lane's three verbs");

    let open = Request::StrategyOpen { venue: "binance".to_string(), network: None };
    let funding = Request::StrategyFundingHistory {
        venue: "binance".to_string(),
        symbol: "BTCUSDT.P".to_string(),
        start: 0,
        end: 1,
    };
    let input = Request::StrategyInputBars {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1h".to_string(),
        source: None,
        start: 0,
        end: 1,
    };
    for r in [&open, &funding, &input] {
        assert!(Plane::Data.verbs().contains(&request_kind(r)), "the data daemon serves {r:?}");
        assert_eq!(reply_wait(r), ReplyWait::Bounded);
        for authed in [Scope::Read, Scope::Write, Scope::Account] {
            assert!(!scope_admits(authed, required_scope(r)), "{authed:?} must not send {r:?}");
        }
        assert!(scope_admits(Scope::Trade, required_scope(r)));
    }
    assert_eq!(request_kind(&open), "StrategyOpen");
    assert_eq!(request_kind(&funding), "StrategyFundingHistory");
    assert_eq!(request_kind(&input), "StrategyInputBars");
    // ...and a Trade connection can send NOTHING else (a handshake frame included).
    for r in one_of_every_request() {
        let sendable = scope_admits(Scope::Trade, required_scope(&r));
        assert_eq!(sendable, LANE.contains(&request_kind(&r)), "{r:?}");
    }
}

/// **The open's bytes**: no `network` key on the main network (so a peer that predates the field reads
/// the same frame), the word on a test network, and the reply carries the heartbeat it states.
#[test]
fn the_strategy_lane_open_and_its_reply_have_the_bytes_the_lane_speaks() {
    let main = Request::StrategyOpen { venue: "binance".to_string(), network: None };
    assert_eq!(
        serde_json::to_string(&main).expect("encodes"),
        r#"{"StrategyOpen":{"venue":"binance"}}"#
    );
    let test = Request::StrategyOpen {
        venue: "hyperliquid".to_string(),
        network: Some(STRATEGY_NETWORK_TESTNET.to_string()),
    };
    assert_eq!(
        serde_json::to_string(&test).expect("encodes"),
        r#"{"StrategyOpen":{"venue":"hyperliquid","network":"testnet"}}"#
    );
    for r in [&main, &test] {
        let back: Request =
            serde_json::from_str(&serde_json::to_string(r).expect("encodes")).expect("decodes");
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(r).unwrap());
    }
    let reply = Response::StrategyOpened { session: 3, heartbeat_ms: 5_000 };
    assert_eq!(
        serde_json::to_string(&reply).expect("encodes"),
        r#"{"StrategyOpened":{"session":3,"heartbeat_ms":5000}}"#
    );
    match serde_json::from_str::<Response>(
        r#"{"StrategyOpened":{"session":3,"heartbeat_ms":5000}}"#,
    ) {
        Ok(Response::StrategyOpened { session: 3, heartbeat_ms: 5_000 }) => {}
        other => panic!("{other:?}"),
    }
}

/// The per-venue lane entry round-trips through its pair, reads ONE prefix and never a `market_data_venue=`
/// one, and the lane's feature strings are whole-string distinct from the market-data plane's.
#[test]
fn the_strategy_venue_feature_round_trips_and_is_not_the_market_data_venue_entry() {
    let features = vec![
        strategy_venue_feature("binance"),
        market_data_venue_feature("okx"),
        "strategy_venue=  ".to_string(),
        strategy_venue_feature("hyperliquid"),
        FEATURE_STRATEGY_LANE.to_string(),
    ];
    assert_eq!(advertised_strategy_venues(&features), vec!["binance", "hyperliquid"]);
    assert_eq!(
        advertised_market_data_venues(&features),
        vec!["okx"],
        "the market data reader never sees a lane entry"
    );
    for lane in [
        FEATURE_STRATEGY_LANE_QUOTES,
        FEATURE_STRATEGY_LANE_BARS,
        FEATURE_STRATEGY_LANE_TICKS,
        FEATURE_STRATEGY_LANE_BOOK,
    ] {
        assert_ne!(lane, FEATURE_STRATEGY_LANE);
        assert!(lane.starts_with(FEATURE_STRATEGY_LANE), "{lane}");
    }
    assert!(!FEATURE_STRATEGY_LANE.starts_with(FEATURE_STRATEGY_VENUE_PREFIX));
}
