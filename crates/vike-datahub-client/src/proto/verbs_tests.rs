use std::io::Cursor;

use crate::proto::*;

/// [`welcome_plane`] names a plane only when the `Welcome` names exactly ONE: a pre-split daemon
/// (both sentinels) and an unknown peer (neither) answer `None`, and so does a feature that
/// merely CONTAINS a sentinel — capability strings are compared whole, never by substring.
#[test]
fn a_welcome_names_a_plane_only_when_it_names_exactly_one() {
    let f = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let compute = f(&[COMPUTE_PLANE_SENTINEL, STUDIO_RUNNER_SENTINEL, FEATURE_AUTH]);
    let data = f(&[DATA_PLANE_SENTINEL, FEATURE_BACKFILL, FEATURE_AUTH]);
    assert_eq!(welcome_plane(&compute), Some(Plane::Compute));
    assert_eq!(welcome_plane(&data), Some(Plane::Data));
    // A pre-split daemon serves BOTH planes, so it is neither — a Write session there carries
    // the store verbs.
    let both = f(&[COMPUTE_PLANE_SENTINEL, DATA_PLANE_SENTINEL, FEATURE_AUTH]);
    assert_eq!(welcome_plane(&both), None);
    assert_eq!(welcome_plane(&f(&[FEATURE_AUTH])), None);
    assert_eq!(welcome_plane(&[]), None);
    // Whole-string equality: a longer token that happens to start with a sentinel is not it.
    let lookalike = f(&["backtest_v2", "load_bars_v2"]);
    assert_eq!(welcome_plane(&lookalike), None);
    // ...and the three sentinels are three different tokens.
    assert_ne!(COMPUTE_PLANE_SENTINEL, DATA_PLANE_SENTINEL);
    assert_ne!(STUDIO_RUNNER_SENTINEL, COMPUTE_PLANE_SENTINEL);
}

/// ⚠ THE THREE CLASSIFIERS, asserted rather than trusted to the compiler. Each of them is
/// exhaustive, so a missing arm is a build failure — but WHICH arm was chosen is a judgement,
/// and these are the three judgements: a study RUNS an engine (compute), it WRITES a run
/// directory on the backend (Control, which `crates/vike-cli/src/cmd/study.rs`'s connect
/// already negotiates under), and it names itself in a refusal.
#[test]
fn the_study_verb_is_a_control_scoped_compute_verb() {
    let r = Request::RunStudy(Box::new(WireStudy {
        study: "cohort".to_string(),
        recipe_toml: String::new(),
        from: "1".to_string(),
        to: "2".to_string(),
    }));
    assert_eq!(plane_of(&r), Plane::Compute);
    assert_eq!(request_kind(&r), "RunStudy");
    assert_eq!(required_scope(&r), VerbScope::Write);
}

/// The variant each request maps to, through a match with NO `_` arm — so a new verb fails to
/// COMPILE here until it is given a row, and then fails
/// [`every_verb_is_classified_and_names_itself_by_its_wire_tag`] until it is given a sample.
fn variant_index(r: &Request) -> usize {
    match r {
        Request::Hello { .. } => 0,
        Request::Auth { .. } => 1,
        Request::Ping => 2,
        Request::RunBacktest(_) => 3,
        Request::RunSlice { .. } => 4,
        Request::RunParamscan { .. } => 5,
        Request::RunWalkforward { .. } => 6,
        Request::RunParamscanProfile { .. } => 7,
        Request::RunWalkforwardProfile { .. } => 8,
        Request::RunStudy(_) => 9,
        Request::LoadBars { .. } => 10,
        Request::ScanQuotes { .. } => 11,
        Request::ScanTrades { .. } => 12,
        Request::ScanBookUpdates { .. } => 13,
        Request::ScanDepth { .. } => 14,
        Request::ScanCohort { .. } => 15,
        Request::ScanPerpMetrics { .. } => 16,
        Request::ScanEquity { .. } => 17,
        Request::ScanExecFills { .. } => 18,
        Request::PropertiesAsOf { .. } => 19,
        Request::ListSeries => 20,
        Request::Inventory => 21,
        Request::SeriesFacts { .. } => 22,
        Request::SeriesGaps { .. } => 23,
        Request::Coverage => 24,
        Request::ListStrategies => 25,
        Request::NamedStrategies => 26,
        Request::RunNamed(_) => 27,
        Request::Backfill { .. } => 28,
        Request::ImportArchive(_) => 29,
        Request::SeedSeries { .. } => 30,
        Request::VenueCatalog { .. } => 31,
        Request::DeleteSeries { .. } => 32,
        Request::MdSubscribe { .. } => 33,
        Request::MdUpdate { .. } => 34,
        Request::ListBackfills => 35,
        Request::CancelBackfill { .. } => 36,
        Request::HistoryChannels => 37,
    }
}

/// How many [`Request`] variants [`variant_index`] numbers.
const REQUEST_VARIANTS: usize = 38;

/// One sample of EVERY [`Request`] variant.
fn one_of_every_request() -> Vec<Request> {
    use crate::wire_studio::WireSliceKind;
    let slice = || {
        Box::new(WireSlice {
            venue: "binance".to_string(),
            symbols: vec!["BTCUSDT".to_string()],
            interval: "1m".to_string(),
            start: None,
            end: None,
            kind: WireSliceKind::Bars,
        })
    };
    let spec = || WireSpec::Rhai("fn on_bar() {}".to_string());
    let series = || SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1h".to_string()));
    let venue = "binance".to_string();
    let symbol = "BTCUSDT".to_string();
    let md = crate::market::MdSpec {
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        lane: crate::market::MdLane::Trades,
        depth_levels: None,
    };
    vec![
        Request::Hello { proto_version: PROTO_VERSION },
        Request::Auth { scope: Scope::Read, mac: vec![0u8; 32] },
        Request::Ping,
        Request::RunBacktest(String::new()),
        Request::RunSlice { spec: spec(), slice: slice(), params: None },
        Request::RunParamscan {
            spec: spec(),
            slice: slice(),
            paramscan: WireParamscan { axes: vec![] },
            params: None,
        },
        Request::RunWalkforward {
            spec: spec(),
            slice: slice(),
            walkforward: WireWalkforward::fixed(2),
            params: None,
        },
        Request::RunParamscanProfile { profile_toml: String::new(), rank_by: None, search: None },
        Request::RunWalkforwardProfile { profile_toml: String::new() },
        Request::RunStudy(Box::new(WireStudy {
            study: "cohort".to_string(),
            recipe_toml: String::new(),
            from: "1".to_string(),
            to: "2".to_string(),
        })),
        Request::LoadBars {
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanQuotes {
            venue: venue.clone(),
            symbol: symbol.clone(),
            start: None,
            end: None,
            limit: None,
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
        Request::ScanExecFills { venue: venue.clone(), symbol: symbol.clone() },
        Request::PropertiesAsOf { venue: venue.clone(), symbol: symbol.clone(), ts: 0 },
        Request::ListSeries,
        Request::Inventory,
        Request::SeriesFacts { id: series() },
        Request::SeriesGaps { id: series() },
        Request::Coverage,
        Request::ListStrategies,
        Request::NamedStrategies,
        Request::RunNamed(Box::new(crate::named_run::NamedRunSpec {
            strategy: "buy_hold".to_string(),
            params: Vec::new(),
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            start: 0,
            end: 3_600_000,
        })),
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
        }),
        Request::SeedSeries {
            venue: venue.clone(),
            symbol: symbol.clone(),
            interval: "1h".to_string(),
            class: None,
        },
        Request::VenueCatalog { venue: venue.clone() },
        Request::DeleteSeries {
            selector: SeriesSelector::new("bar", "binance"),
            produced_by: Some("klines:".to_string()),
            dry_run: true,
        },
        Request::MdSubscribe { specs: vec![md.clone()] },
        Request::MdUpdate { session: crate::market::MdSessionId(7), add: vec![md], remove: vec![] },
        Request::ListBackfills,
        Request::CancelBackfill { venue, symbol, interval: "1h".to_string() },
        Request::HistoryChannels,
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
/// carries it — and for every sample it holds the three exhaustive classifiers to one rule a
/// compiler cannot check: [`request_kind`] names the verb by the tag the WIRE carries, the one an
/// operator can grep a capture for. ([`plane_of`] and [`required_scope`] are exhaustive by
/// construction; WHICH arm they chose is pinned verb by verb in
/// `crates/vike-datahub/tests/auth_roundtrip/scope_split.rs`'s
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
        let _ = (plane_of(r), required_scope(r));
    }
    let missing: Vec<usize> = (0..REQUEST_VARIANTS).filter(|i| !seen[*i]).collect();
    assert!(missing.is_empty(), "variant indices with no sample: {missing:?}");
}

/// ⚠ THE ARCHIVE IMPORT'S THREE CLASSIFIERS, asserted rather than trusted to the compiler: it
/// WRITES the data daemon's store, so it is a DATA-plane verb (a `Plane::Compute` answer would have
/// the data daemon refuse its own verb with a wrong-plane message), it is Control-scoped for the
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
    });
    assert_eq!(plane_of(&r), Plane::Data);
    assert_eq!(required_scope(&r), VerbScope::Write);
    assert_eq!(request_kind(&r), "ImportArchive");
    // ...and an Observe credential may not send it, while a Control one may.
    assert!(!scope_admits(Scope::Read, required_scope(&r)));
    assert!(scope_admits(Scope::Write, required_scope(&r)));
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
        assert_eq!(plane_of(r), Plane::Data, "{r:?}");
    }
    assert_eq!(required_scope(&list), VerbScope::Read);
    assert_eq!(required_scope(&cancel), VerbScope::Write);
    assert!(scope_admits(Scope::Read, required_scope(&list)));
    assert!(!scope_admits(Scope::Read, required_scope(&cancel)), "Observe must not cancel");
    assert!(scope_admits(Scope::Write, required_scope(&cancel)));
    assert_eq!(request_kind(&list), "ListBackfills");
    assert_eq!(request_kind(&cancel), "CancelBackfill");
}

/// ⚠ THE HISTORY-CHANNELS READ'S THREE CLASSIFIERS, asserted rather than trusted to the compiler —
/// `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md`. A DATA-plane verb (its
/// overlay is the data daemon's table, store and credential store), Observe-scoped, a bare unit tag
/// on the wire — and its answer, the client's own compiled table here, survives the frame codec.
#[test]
fn the_history_channels_read_is_an_observe_scoped_data_verb_and_its_answer_round_trips() {
    let r = Request::HistoryChannels;
    assert_eq!(plane_of(&r), Plane::Data);
    assert_eq!(required_scope(&r), VerbScope::Read);
    assert!(scope_admits(Scope::Read, required_scope(&r)), "an Observe key may ask");
    assert_eq!(request_kind(&r), "HistoryChannels");
    assert_eq!(
        serde_json::to_value(&r).expect("encode"),
        serde_json::json!("HistoryChannels"),
        "the request names NOTHING — a bare unit tag"
    );

    let report = crate::history::compiled_report(1_790_899_200_000);
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Response::HistoryChannels(report.clone())).unwrap();
    match read_frame::<_, Response>(&mut Cursor::new(buf)).unwrap() {
        Response::HistoryChannels(got) => assert_eq!(got, report),
        other => panic!("expected HistoryChannels, got {other:?}"),
    }
    assert_eq!(FEATURE_HISTORY_CHANNELS, "history_channels");
}
