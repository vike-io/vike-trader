//! The backtesthub half of ruling 7's split: `vike-backend backtesthub --addr` serves the seven compute
//! verbs and refuses every DATA verb by name, keeping the connection.
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0 requires BOTH
//! directions to be tested. The other half — the data daemon refusing a backtest verb — is
//! `crates/vike-datahub/tests/default_build/plane_split.rs`, and the two are deliberate mirror images: one helper
//! (`vike_hub_protocol::plane`'s `wrong_plane_message`) writes both texts, so a change to one
//! refusal cannot leave the other behind.
//!
//! It also carries the round-trips that used to live in `vike-datahub`'s `roundtrip.rs` and moved
//! here with the verbs: `RunBacktest` over the wire, the invalid-profile refusal, the
//! `list_strategies` roster, and the profile-shaped sweep on a store-less build.
//!
//! Hermetic: an ephemeral `127.0.0.1:0` listener over `HistoryStoreMemory`, no prod store, no network.

use std::assert_matches;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use vike_backtesthub::backtesthub_server::{StudyDirs, serve_authed};
use vike_backtesthub::named_run::NamedRunLane;
use vike_backtesthub::test_support::spawn_server;
use vike_backtesthub_client::BacktesthubClient;
use vike_backtesthub_client::named_run::{
    NamedRunOutcome, NamedRunParam, NamedRunRefusal, NamedRunSpec,
};
use vike_backtesthub_client::proto::{
    FEATURE_NAMED_RUN, FEATURE_SEARCH_METHOD, FEATURE_SLIPPAGE_MODEL, FEATURE_STRATEGY_FILES,
    FEATURE_STUDY, FEATURE_WALKFORWARD_SEARCH, Request, Response, SearchSelectionWire,
    StudyRunRequestWire,
};
use vike_backtesthub_client::wire_studio::{DataSliceWire, ReplayKindWire, StrategySpecWire};
use vike_data::{HistoryStore, HistoryStoreMemory, StoreKindId};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT;
use vike_hub_protocol::plane::Plane;

/// A minimal, valid bar-mode profile. Over an empty `HistoryStoreMemory` it loads no bars, so the run
/// closes no trades — enough to exercise the full request → engine → report → response path.
///
/// ⚠ Moved here from `crates/vike-datahub/tests/default_build/roundtrip.rs` with the verbs that consume it.
const MINIMAL_BAR_PROFILE: &str = r#"
[data]
venue = "binance"
symbols = ["BTCUSDT"]
kind = "bar"
interval = "1d"
from = "0"
to = "100000"

[engine]
cash = 1000.0

[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

/// Every DATA verb, with a payload the data daemon would accept — so what is proved is the refusal,
/// not a decode failure that would look the same from outside.
fn data_requests() -> Vec<vike_datahub_client::proto::Request> {
    use vike_data::SeriesId;
    use vike_datahub_client::proto::{Request, SeriesSelector};
    vec![
        Request::LoadBars {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: None,
            end: None,
            limit: None,
            source: None,
        },
        Request::ScanQuotes {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
            source: None,
        },
        Request::ScanTrades {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::PropertiesAsOf { venue: "binance".into(), symbol: "BTCUSDT".into(), timestamp: 0 },
        // The SIX tick-level and research reads `docs/decisions/0084` added to the data plane. They
        // belong here for the same reason their neighbours do: this daemon holds the COMPUTE store
        // handle, so it must refuse them BY NAME rather than answer from the wrong store.
        Request::ScanBookUpdates {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanDepth {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        // ⚠ `asset`, not `symbol` — this verb's middle field is the family's odd one out.
        Request::ScanCohort {
            venue: "binance".into(),
            asset: "BTC".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanPerpMetrics {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanEquity {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanExecFills {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanExecOrders {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ListSeries,
        Request::Inventory,
        Request::SeriesGaps {
            id: SeriesId::per_symbol(
                StoreKindId::Bar,
                "binance",
                "BTCUSDT",
                Some("1h".to_string()),
            ),
        },
        // 0084's seventh verb: the data plane's, so this daemon refuses it by name like the rest.
        Request::SeriesFacts {
            id: SeriesId::per_symbol(
                StoreKindId::Bar,
                "binance",
                "BTCUSDT",
                Some("1h".to_string()),
            ),
        },
        Request::Coverage,
        Request::Backfill {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: 0,
            end: 1,
        },
        Request::DeleteSeries {
            selector: SeriesSelector::new(StoreKindId::Bar, "binance"),
            produced_by: Some("klines:".to_string()),
            dry_run: true,
        },
        // The archive import: the data daemon's store and the data daemon's imports directory, so
        // this daemon must refuse it by name rather than look for files of its own.
        Request::ImportArchive(vike_datahub_client::archive::ImportSpec {
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
        // The data daemon's registry of RUNNING backfills — listed and stopped there, and refused
        // here by name: this daemon runs no backfill.
        Request::ListBackfills,
        Request::CancelBackfill {
            venue: "oanda".into(),
            symbol: "EUR_USD".into(),
            interval: "5s".into(),
        },
        // The history-channels read — its overlay is the data daemon's own, so it is refused here
        // by name like the rest (`docs/decisions/0102`).
        Request::HistoryChannels,
        // ...and the data daemon's lanes, each refused by name: it holds the collector table, the
        // venue egress, the provider caches and the broker, and this daemon holds none of them.
        Request::SeedSeries {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            class: None,
        },
        Request::VenueCatalog { venue: "binance".into() },
        Request::OptionsChain {
            venue: "deribit".into(),
            underlying: "BTC".into(),
            expiries: Vec::new(),
        },
        Request::NewsFeed { if_generation: None },
        Request::EconomicCalendar { if_generation: None },
        Request::EquityCalendar { week_start: "2026-10-12".into(), if_generation: None },
        Request::MarketDataSubscribe { specs: Vec::new() },
        Request::MarketDataUpdate {
            session: vike_datahub_client::market::MarketDataSessionId(7),
            add: Vec::new(),
            remove: Vec::new(),
        },
        Request::StrategyOpen { venue: "binance".into(), network: None },
        Request::GatewayStatus,
        Request::GatewayLinks { since_ms: 0, route: None, venue: None },
        Request::StrategyFundingHistory {
            venue: "binance".into(),
            symbol: "BTCUSDT.P".into(),
            start: 0,
            end: 1,
        },
        // The lane's second store read (an input's bars, decision 0271): the data daemon's store.
        Request::StrategyInputBars {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            source: None,
            start: 0,
            end: 1,
        },
        // A user's CSV upload writes the data daemon's store (decision 0272), so it is refused here by
        // name like every other store verb.
        Request::CsvUpload(vike_datahub_client::upload::CsvUploadSpec {
            target: vike_datahub_client::archive::ImportTarget {
                venue: "user".into(),
                symbol: "USDT-USD".into(),
                interval: "1d".into(),
                label: "coinbase".into(),
                file_format: vike_data::csv_bars::CsvBarsFormatWords::default(),
                columns: None,
            },
            bytes: 10,
            dry_run: true,
            head: false,
        }),
        Request::CsvUploadChunk { text: "ts,close\n".into() },
        Request::CsvUploadEnd,
        // The options listing's refresh: its refusal line is appended to the frozen file, the lines
        // above it unmoved, and the symbol-set and instrument-list verbs after it the same way.
        Request::OptionsRefresh { venue: "deribit".into() },
        Request::SymbolSets,
        Request::SetSymbolSet {
            set: vike_data::datasets::SymbolSet {
                name: "Crypto Majors".into(),
                symbols: vec!["BTCUSDT".into(), "ETHUSDT".into()],
                provider: "binance".into(),
                interval: "1m".into(),
                benchmark: String::new(),
                user: true,
            },
        },
        Request::DeleteSymbolSet { name: "Crypto Majors".into() },
        Request::InstrumentLists,
        Request::RefreshInstrumentLists { venue: Some("binance".into()) },
        Request::OperatorInstruments {
            venue: "ig".into(),
            add: vec!["EURUSD".into()],
            remove: Vec::new(),
            stated: None,
        },
        // The archive DOWNLOAD (DATA-S3): the data daemon's bucket client and imports folder, refused here by name.
        Request::FetchArchive(vike_datahub_client::archive_fetch::FetchSpec {
            format: "dukascopy-bi5".to_string(),
            dataset: "EURUSD".to_string(),
            from_day: 0,
            to_day: 0,
            go: false,
            concurrency: 8,
        }),
    ]
}

/// The bytes this daemon answers each data verb with, one line per [`data_requests`] row.
const FROZEN_REFUSALS: &str = "fixtures/datahub_wire/v7_wrong_plane_refusals_at_backtesthub.jsonl";

/// ⚠ **The refusal's EXACT BYTES, frozen before the data protocol left this daemon's enum.** A client
/// that dialled the wrong hub reads this sentence, so a refactor that loses it (a parse failure, a
/// dropped socket, a hang) reddens here rather than on an operator's screen.
#[test]
fn the_refusal_bytes_of_every_data_verb_are_frozen() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut stream = TcpStream::connect(addr).expect("connect");
    let actual: Vec<String> = data_requests()
        .iter()
        .map(|request| {
            write_frame(&mut stream, request).expect("write");
            let body = vike_hub_protocol::frame::read_frame_raw(&mut stream)
                .expect("answered, never dropped");
            String::from_utf8(body).expect("a frame body is UTF-8 JSON")
        })
        .collect();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let golden = std::fs::read_to_string(root.join(FROZEN_REFUSALS)).unwrap_or_default();
    let golden: Vec<&str> = golden.lines().collect();
    assert_eq!(
        actual,
        golden,
        "{FROZEN_REFUSALS} is frozen; fix the daemon, not the file.\nTHIS BUILD ANSWERS:\n{}\n",
        actual.join("\n")
    );
}

fn spawn_keyed() -> (SocketAddr, vike_hub_protocol::auth::HubKeys) {
    let keys = || {
        vike_hub_protocol::auth::HubKeys::new(
            b"observe-placeholder".to_vec(),
            b"control-placeholder".to_vec(),
        )
    };
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let served = keys();
    thread::spawn(move || {
        let _ = serve_authed(
            listener,
            store,
            StudyDirs::working_directory(),
            Some(served),
            NamedRunLane::DISARMED,
            SERVER_IDLE_READ_TIMEOUT,
        );
    });
    (addr, keys())
}

/// Before `Hello` on a KEYED daemon a data verb is an unauthenticated verb like any other: the same
/// words, whichever plane it belongs to, and the socket closes.
#[test]
fn a_data_verb_before_hello_on_a_keyed_daemon_is_told_to_say_hello_first() {
    let (addr, _keys) = spawn_keyed();
    for request in data_requests() {
        let mut stream = TcpStream::connect(addr).expect("connect");
        write_frame(&mut stream, &request).expect("write");
        assert_matches!(
            read_frame::<_, Response>(&mut stream),
            Ok(Response::AuthDenied { reason }) if reason == "not authenticated: expected Hello first"
        );
    }
}

/// ⚠ THE PLANE IS ASKED BEFORE THE SCOPE: a read-scope connection sending a verb this daemon does not
/// serve is told the address it wants, not that it needs a bigger key.
#[test]
fn a_data_verb_on_a_read_scope_connection_is_told_the_address_not_a_scope() {
    use vike_hub_protocol::auth::{DATAHUB_DOMAIN, Scope, sign};

    let (addr, keys) = spawn_keyed();
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(
        &mut stream,
        &Request::Hello { proto_version: vike_backtesthub_client::proto::PROTO_VERSION },
    )
    .expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce.expect("a keyed daemon's Welcome carries a nonce"),
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = sign(
        DATAHUB_DOMAIN,
        keys.key_for(Scope::Read),
        &nonce,
        vike_backtesthub_client::proto::PROTO_VERSION,
        Scope::Read,
    );
    write_frame(&mut stream, &Request::Auth { scope: Scope::Read, mac }).expect("auth");
    assert_matches!(read_frame::<_, Response>(&mut stream), Ok(Response::AuthOk { .. }));
    for request in data_requests() {
        let kind = vike_datahub_client::proto::request_kind(&request);
        write_frame(&mut stream, &request).expect("write");
        match read_frame::<_, Response>(&mut stream).expect("the connection survives") {
            Response::Error(msg) => {
                assert!(msg.starts_with(&format!("{kind} is a Data-plane verb")), "{msg}");
                assert!(!msg.contains("scope"), "not a scope question: {msg}");
            }
            other => panic!("{kind} must be refused by plane, got {other:?}"),
        }
    }
}

/// The sample set IS the data plane — no more, no less, so a new data verb reddens this file until
/// its author adds a sample.
#[test]
fn every_data_verb_is_covered() {
    let mut samples: Vec<&str> =
        data_requests().iter().map(vike_datahub_client::proto::request_kind).collect();
    samples.sort_unstable();
    let mut roster = Plane::Data.verbs().to_vec();
    roster.sort_unstable();
    assert_eq!(samples, roster);
}

/// ⚠ THE MIRROR REFUSAL: every DATA verb answers `Response::Error` naming the daemon that serves it,
/// and the CONNECTION SURVIVES. EVERY sample on ONE connection, which is what makes the survival
/// claim mean anything.
///
/// ⚠ This said "all ten" until `docs/decisions/0084` added six verbs at once. A count in prose is
/// the thing this tree has watched rot most often, and the number bought nothing the loop below
/// does not already state — it drives whatever `data_requests` holds, so the claim is exactly as
/// wide as that set and cannot disagree with it.
#[test]
fn a_data_verb_is_refused_by_name_and_the_connection_survives() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for request in data_requests() {
        let kind = vike_datahub_client::proto::request_kind(&request);
        write_frame(&mut stream, &request).expect("write");
        let response: Response = read_frame(&mut stream).expect("the connection must survive");
        match response {
            Response::Error(msg) => {
                assert!(msg.contains(kind), "the refusal names the verb: {msg}");
                assert!(
                    msg.contains("vike-backend datahub"),
                    "the refusal names the daemon that DOES serve it: {msg}"
                );
                assert!(
                    msg.contains("config.datahub_addr"),
                    "the refusal names the settings key the client dialled from: {msg}"
                );
            }
            other => panic!("{kind} must be refused, not served: {other:?}"),
        }
    }
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert_matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong));
}

/// `RunBacktest` round-trips and the accept loop survives it — the test that used to prove this
/// against `vike-datahub`, now against the daemon that actually runs the engine.
#[test]
fn run_backtest_round_trips_and_server_survives() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    client.ping().expect("ping");

    // Over an empty store the run returns either a Report (JSON text) or a clean server-side Error
    // — BOTH well-formed Responses. The point: no hang and no panic that kills the loop.
    match client.run_backtest(MINIMAL_BAR_PROFILE) {
        Ok(json) => {
            let v: serde_json::Value =
                serde_json::from_str(&json).expect("report must be valid JSON");
            assert!(v.is_object(), "report JSON must be an object");
        }
        Err(msg) => assert!(!msg.is_empty(), "a server-side error must carry a message"),
    }

    let mut client2 = BacktesthubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after a backtest request");
}

/// `MINIMAL_BAR_PROFILE` with an `[engine.resolution]` table carrying `resolution`, plus `extra`.
fn resolution_profile(resolution: &str, extra: &str) -> String {
    format!(
        "{MINIMAL_BAR_PROFILE}\n[engine.resolution]\nkind = \"binary_outcome\"\n{resolution}\n\
         {extra}\n"
    )
}

/// ⚠ **A profile that arrives over the wire may not make the daemon open a file**, on any of the
/// three verbs a wire profile reaches: `[engine.resolution] path` has no profile directory to resolve
/// against there, so it would name anything on the daemon's box, and the `{path}: {error}` answer
/// would say whether it exists. So a file that exists and one that does not get ONE answer.
///
/// ⚠ Regular files only, on purpose: on a daemon without this refusal a device here would be read
/// to the end, on whichever box runs the test.
#[test]
fn a_wire_profile_may_not_name_a_resolution_file_on_any_verb() {
    let dir = tempfile::tempdir().expect("temp dir");
    let real = dir.path().join("winners.csv");
    std::fs::write(&real, "btc-updown-5m-1775001600,0\n").expect("a real winners file");
    let missing = dir.path().join("no-such.csv");
    let path_line = |p: &std::path::Path| format!("path = {:?}", p.display().to_string());

    let mut client =
        BacktesthubClient::connect(spawn_server(vike_strategies_check::stand_in)).expect("connect");
    let mut ask = |verb: &str, toml: &str| match verb {
        "run_backtest" => client.run_backtest(toml),
        "run_sweep" => client.run_paramscan_profile(toml, None, None),
        "run_walk_forward" => client.run_walkforward_profile(toml),
        other => panic!("not a verb: {other}"),
    };
    for (verb, extra) in [
        ("run_backtest", ""),
        ("run_sweep", "[paramscan]\nsize = [1.0, 2.0]\n"),
        ("run_walk_forward", "[walkforward]\nn_splits = 2\n"),
    ] {
        let there = ask(verb, &resolution_profile(&path_line(&real), extra))
            .expect_err("a file that exists is refused");
        let absent = ask(verb, &resolution_profile(&path_line(&missing), extra))
            .expect_err("a file that does not exist is refused");
        assert_eq!(there, absent, "{verb}: the answer says whether the file exists");
        assert!(there.contains("[engine.resolution] path"), "{verb} names the key: {there}");
        assert!(there.contains("[engine.resolution.winners]"), "{verb} says what to do: {there}");
        assert!(there.contains("--local"), "{verb} says what to do: {there}");
    }
}

/// ⚠ **`data.file` is a file on the daemon's box too** (decision 0268), and it is refused on every verb
/// before the first read, a file that exists and one that does not alike.
#[test]
fn a_wire_profile_may_not_name_a_data_file_on_any_verb() {
    let dir = tempfile::tempdir().expect("temp dir");
    let real = dir.path().join("bars.csv");
    std::fs::write(&real, "time,open,high,low,close,volume\n1700000000,1,1,1,1,1\n").unwrap();
    let missing = dir.path().join("no-such.csv");
    let with_file = |p: &std::path::Path, extra: &str| {
        let line = format!("file = {:?}\n", p.display().to_string());
        format!(
            "{}{extra}",
            MINIMAL_BAR_PROFILE.replacen("kind = \"bar\"\n", &format!("kind = \"bar\"\n{line}"), 1)
        )
    };

    let mut client =
        BacktesthubClient::connect(spawn_server(vike_strategies_check::stand_in)).expect("connect");
    let mut ask = |verb: &str, toml: &str| match verb {
        "run_backtest" => client.run_backtest(toml),
        "run_sweep" => client.run_paramscan_profile(toml, None, None),
        "run_walk_forward" => client.run_walkforward_profile(toml),
        other => panic!("not a verb: {other}"),
    };
    for (verb, extra) in [
        ("run_backtest", ""),
        ("run_sweep", "[paramscan]\nsize = [1.0, 2.0]\n"),
        ("run_walk_forward", "[walkforward]\nn_splits = 2\n"),
    ] {
        let there = ask(verb, &with_file(&real, extra)).expect_err("a file that exists is refused");
        let absent = ask(verb, &with_file(&missing, extra)).expect_err("an absent one is refused");
        assert_eq!(there, absent, "{verb}: the answer says whether the file exists");
        assert!(there.contains("`data.file`") && there.contains("--local"), "{verb}: {there}");
    }
}

/// The inline table is the spelling that crosses the wire, and it still does: the fence is on the
/// file, not on the settlement source.
#[test]
fn inline_winners_still_cross_the_wire() {
    let mut client =
        BacktesthubClient::connect(spawn_server(vike_strategies_check::stand_in)).expect("connect");
    let toml =
        resolution_profile("[engine.resolution.winners]\n\"btc-updown-5m-1775001600\" = 0", "");
    let json = client.run_backtest(&toml).expect("inline winners run");
    let report: serde_json::Value = serde_json::from_str(&json).expect("a report");
    assert!(report.is_object(), "the report is JSON: {json}");
}

/// A malformed profile gets a clean `Response::Error`, and the server keeps serving — one bad
/// request never takes down the accept loop. (`from = "999999"` > `to = "100000"` fails
/// `ProfileBacktest::from_toml_str`'s validation SERVER-side, which is where the client ships it.)
#[test]
fn invalid_profile_yields_error_not_a_dropped_connection() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let invalid = MINIMAL_BAR_PROFILE.replace("from = \"0\"", "from = \"999999\"");

    let mut client = BacktesthubClient::connect(addr).expect("connect");
    match client.run_backtest(&invalid) {
        Err(msg) => assert!(!msg.is_empty(), "expected a server-side validation error"),
        Ok(_) => panic!("an invalid (from > to) profile must not produce a report"),
    }

    let mut client2 = BacktesthubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after an invalid request");
}

/// The `list_strategies` verb round-trips, and the handshake advertises it — the roster IS the
/// daemon's `strategies/` folder: every `.rs` and `.rhai` file under it, subfolders included, by its
/// path, sorted, a broken file listed too, and nothing else in the folder.
#[test]
fn list_strategies_round_trips_the_folder() {
    let folder = tempfile::tempdir().expect("a strategies folder");
    std::fs::create_dir_all(folder.path().join("trend")).unwrap();
    std::fs::write(folder.path().join("trend/demo.rhai"), "fn on_bar() {}\n").unwrap();
    std::fs::write(folder.path().join("broken.rs"), "no header\n").unwrap();
    std::fs::write(folder.path().join("notes.txt"), "not a strategy\n").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let study = StudyDirs {
        strategies: vike_backtesthub::studio_run::StrategyFiles::hub(folder.path().to_path_buf()),
        ..StudyDirs::working_directory()
    };
    thread::spawn(move || {
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
        let _ = serve_authed(
            listener,
            store,
            study,
            None,
            NamedRunLane::DISARMED,
            SERVER_IDLE_READ_TIMEOUT,
        );
    });
    let mut client = BacktesthubClient::connect(addr).expect("connect");

    let roster = client.list_strategies().expect("list_strategies must return the roster");
    assert_eq!(roster, vec!["broken.rs".to_string(), "trend/demo.rhai".to_string()]);
    assert!(
        client.features().iter().any(|f| f == "list_strategies"),
        "server must advertise the list_strategies feature: {:?}",
        client.features()
    );
}

/// **What a Studio lists and opens** (`FEATURE_STRATEGY_FILES`): every file of the folder with its
/// states in the scan's words, the `edited` mark against the shipped twin, and one file's text. A
/// path that leaves the folder is refused with the scan's words, and the connection survives.
#[test]
fn the_strategy_files_are_listed_with_their_states_and_opened_by_path() {
    let project = tempfile::tempdir().expect("a project");
    let dir = project.path().join("strategies");
    let shipped = project.path().join("vendor").join("vike-strategies");
    std::fs::create_dir_all(dir.join("trend")).unwrap();
    std::fs::create_dir_all(shipped.join("trend")).unwrap();
    let example = "//! Planted.\n//!\n//! ```toml\n//! [strategy]\n//! ```\npub fn build() {}\n";
    std::fs::write(dir.join("trend/example.rs"), example).unwrap();
    std::fs::write(shipped.join("trend/example.rs"), "//! the shipped text\n").unwrap();
    std::fs::write(dir.join("broken.rhai"), "fn on_bar() {}\n").unwrap();
    let addr = spawn_files_server(dir.clone(), None);
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    assert!(client.features().iter().any(|f| f == FEATURE_STRATEGY_FILES));

    let files = client.list_strategy_files().expect("the folder is listed");
    let paths: Vec<&str> = files.iter().map(|f| f.file.as_str()).collect();
    assert_eq!(paths, ["broken.rhai", "trend/example.rs"]);
    assert!(files[0].broken.starts_with("no header"), "{:?}", files[0]);
    assert_eq!((files[0].build.as_str(), files[0].edited), ("", false), "{:?}", files[0]);
    assert_eq!(files[1].build, "not built", "{:?}", files[1]);
    assert!(files[1].broken.is_empty() && files[1].edited, "{:?}", files[1]);

    assert_eq!(client.read_strategy_file("trend/example.rs").expect("opened"), example);
    let refused = client.read_strategy_file("../secret.rs").expect_err("outside the folder");
    assert!(refused.contains("not a path inside the strategies folder"), "{refused}");
    let missing = client.read_strategy_file("trend/none.rs").expect_err("no such file");
    assert!(missing.contains("cannot read it"), "{missing}");
    client.ping().expect("the connection survives every refusal");
}

/// A backtesthub over the strategies folder `dir`, keyed with `keys` (key-less when `None`); its
/// address.
fn spawn_files_server(
    dir: std::path::PathBuf,
    keys: Option<vike_hub_protocol::auth::HubKeys>,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let study = StudyDirs {
        strategies: vike_backtesthub::studio_run::StrategyFiles::hub(dir),
        ..StudyDirs::working_directory()
    };
    thread::spawn(move || {
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
        let _ = serve_authed(
            listener,
            store,
            study,
            keys,
            NamedRunLane::DISARMED,
            SERVER_IDLE_READ_TIMEOUT,
        );
    });
    addr
}

/// The hub key pair the strategy-file save tests serve and sign with.
fn files_keys() -> vike_hub_protocol::auth::HubKeys {
    vike_hub_protocol::auth::HubKeys::new(b"observe-files".to_vec(), b"control-files".to_vec())
}

/// A Control client of a keyed backtesthub over `dir`.
fn control_client(dir: &std::path::Path) -> BacktesthubClient {
    let addr = spawn_files_server(dir.to_path_buf(), Some(files_keys()));
    BacktesthubClient::connect_authed(addr, &files_keys(), vike_hub_protocol::auth::Scope::Write)
        .expect("Control connect")
}

/// A strategy file with a header, as a save sends it.
const SAVED_RS: &str = "//! Saved.\n//!\n//! ```toml\n//! [strategy]\n//! ```\npub fn build() {}\n";

/// **A Studio save writes the file, answers its new sha256 and states, and an overwrite is atomic**
/// (the owner's ruling of 2026-11-13: the backtesthub manages the strategy files). The folder that
/// did not exist is created; the second save replaces the text whole and leaves no temporary file
/// beside it; the list shows what the answer said; a `.rhai` file with no header saves and is listed
/// broken, as the scan lists it.
#[test]
fn a_saved_strategy_file_answers_its_sha_and_state_and_an_overwrite_is_atomic() {
    let project = tempfile::tempdir().expect("a project");
    let dir = project.path().join("strategies");
    let mut client = control_client(&dir);

    let (sha, entry) = client.save_strategy_file("trend/saved.rs", SAVED_RS).expect("saved");
    assert_eq!(sha, vike_hub_protocol::hash::sha256_hex(SAVED_RS.as_bytes()));
    assert_eq!(entry.file, "trend/saved.rs");
    assert_eq!(entry.build, "not built", "{entry:?}");
    assert!(entry.broken.is_empty() && !entry.edited, "{entry:?}");
    assert_eq!(std::fs::read_to_string(dir.join("trend/saved.rs")).unwrap(), SAVED_RS);

    let second = SAVED_RS.replace("Saved.", "Saved again.");
    let (sha2, _) = client.save_strategy_file("trend/saved.rs", &second).expect("overwritten");
    assert_ne!(sha2, sha);
    assert_eq!(std::fs::read_to_string(dir.join("trend/saved.rs")).unwrap(), second);
    let leftovers: Vec<String> = std::fs::read_dir(dir.join("trend"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, ["saved.rs"], "the temporary file is renamed over, never left");

    let (_, rhai) = client.save_strategy_file("note.rhai", "fn on_bar() {}\n").expect("saved");
    assert!(rhai.broken.starts_with("no header") && rhai.build.is_empty(), "{rhai:?}");
    let listed = client.list_strategy_files().expect("listed");
    assert_eq!(
        listed.iter().find(|f| f.file == "trend/saved.rs").map(|f| f.build.as_str()),
        Some("not built")
    );
    assert!(listed.iter().any(|f| f.file == "note.rhai" && f.broken.starts_with("no header")));
}

/// **A save path that leaves the folder, or is not a strategy file, is refused and writes nothing**:
/// absolute, `..`, another extension, empty; and on unix a folder on the way that is a link out of
/// the folder, and a destination that is a link. The connection survives every refusal.
#[test]
fn a_save_path_outside_the_folder_or_of_another_kind_is_refused_and_writes_nothing() {
    let project = tempfile::tempdir().expect("a project");
    let dir = project.path().join("strategies");
    std::fs::create_dir_all(&dir).unwrap();
    let mut client = control_client(&dir);
    let absolute = project.path().join("abs.rs").display().to_string();
    for bad in ["../escape.rs", "a/../../escape.rs", absolute.as_str(), "trend/x.txt", ""] {
        let err = client.save_strategy_file(bad, SAVED_RS).expect_err(bad);
        assert!(
            err.contains("not a path inside the strategies folder")
                || err.contains("neither a .rs nor a .rhai"),
            "{bad:?}: {err}"
        );
    }
    assert!(!project.path().join("escape.rs").exists() && !project.path().join("abs.rs").exists());

    #[cfg(unix)]
    {
        let outside = project.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("linked")).unwrap();
        let err =
            client.save_strategy_file("linked/evil.rs", SAVED_RS).expect_err("a linked folder");
        assert!(err.contains("is not saved") && err.contains("link"), "{err}");
        assert!(!outside.join("evil.rs").exists(), "nothing was written outside");

        std::fs::write(outside.join("theirs.rs"), "keep\n").unwrap();
        std::os::unix::fs::symlink(outside.join("theirs.rs"), dir.join("theirs.rs")).unwrap();
        let err = client.save_strategy_file("theirs.rs", SAVED_RS).expect_err("a linked file");
        assert!(err.contains("a link is not written through"), "{err}");
        assert_eq!(std::fs::read_to_string(outside.join("theirs.rs")).unwrap(), "keep\n");
    }
    client.ping().expect("the connection survives every refusal");
}

/// **A save needs the Control scope**: an Observe connection to a keyed daemon is refused by the
/// scope check, and a daemon with NO hub keys (which authenticates nothing) refuses every save in
/// words. Nothing is written either way.
#[test]
fn a_save_needs_the_control_scope_and_a_keyless_daemon_refuses_it() {
    let project = tempfile::tempdir().expect("a project");
    let dir = project.path().join("strategies");
    std::fs::create_dir_all(&dir).unwrap();

    let keyed = spawn_files_server(dir.clone(), Some(files_keys()));
    let mut observe = BacktesthubClient::connect_authed(
        keyed,
        &files_keys(),
        vike_hub_protocol::auth::Scope::Read,
    )
    .expect("Observe");
    let err = observe.save_strategy_file("a.rs", SAVED_RS).expect_err("Observe may not save");
    assert!(err.contains("SaveStrategyFile requires the Control scope"), "{err}");
    assert!(observe.list_strategy_files().is_ok(), "...while it may list");

    let keyless = spawn_files_server(dir.clone(), None);
    let mut anyone = BacktesthubClient::connect(keyless).expect("connect");
    let err = anyone.save_strategy_file("a.rs", SAVED_RS).expect_err("no keys, no save");
    assert!(err.contains("has no hub keys"), "{err}");
    assert!(!dir.join("a.rs").exists(), "nothing was written");
}

/// ⚠ The three STUDIO verbs are advertised by EVERY backtesthub and dispatched to
/// `vike-studio-core`'s real runners — there is no table to mount and no Studio-less mode (the
/// `StudioRunTable` seam this file used to drive with stub closures is deleted, and with it the
/// test that asserted an unmounted daemon refused the three by name and advertised none).
///
/// Over the empty `HistoryStoreMemory` the real runner cannot produce a result, so what this proves is the
/// DISPATCH and the advertisement: the answer is the runner's own classified, kind-first error
/// (`vike_studio_core::RunError::wire_line`), not the old "served only by a build that mounts the Studio
/// runners" refusal, and the connection survives it. The computation itself is
/// `crates/vike-backtesthub/tests/studio_wire_parity.rs`'s gate.
#[test]
fn the_studio_verbs_are_advertised_and_dispatched_to_the_real_runners() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    for advertised in ["run_slice", "run_paramscan", "run_walkforward"] {
        assert!(
            client.features().iter().any(|f| f == advertised),
            "every backtesthub must advertise {advertised}: {:?}",
            client.features()
        );
    }
    let err = client
        .run_slice(
            StrategySpecWire::Rhai("fn on_bar(){}".to_string()),
            DataSliceWire {
                venue: "binance".into(),
                symbols: vec!["BTCUSDT".into()],
                interval: "1d".into(),
                start: None,
                end: None,
                kind: ReplayKindWire::Bars,
            },
            None,
        )
        .expect_err("an empty store cannot satisfy a real RunSlice");
    assert!(
        ["compile: ", "data: ", "strategy: "].iter().any(|kind| err.contains(kind)),
        "the answer is the real runner's classified error, kind first: {err}"
    );
    assert!(!err.contains("mounts the Studio runners"), "the Studio-less refusal is gone: {err}");
    client.ping().expect("the connection survives a runner's error");
}

// ---------------------------------------------------------------------------------------------
// Stage 7: the SEARCH SELECTOR on the wire, and the research plane's study verb.
// ---------------------------------------------------------------------------------------------

/// [`MINIMAL_BAR_PROFILE`] plus the grid a parameter search needs. The axis is `size`, which the
/// `buy_hold` strategy above genuinely declares.
///
/// The SECTION is `[paramscan]`, and only that since the owner's ruling 20 (phase 4): a `[sweep]`
/// table is refused at load, held by `crates/vike-backtest/src/harness/profile/tests/core_and_data.rs`'s
/// `the_grid_section_is_paramscan_and_sweep_is_refused`.
fn paramscan_profile() -> String {
    format!("{MINIMAL_BAR_PROFILE}\n[paramscan]\nsize = [1.0, 2.0]\n")
}

/// THE §15.1 ONE-ROSTER GATE'S SERVER LEG: every method the PROTOCOL says it can carry is one this
/// daemon resolves. A fifth entry in `SEARCH_METHODS` with no arm in `search::select` reddens here
/// as well as in the engine's own pin — and this is the leg that proves it over a SOCKET.
///
/// The store is empty, so a run closes no trades and the report is uninteresting. What is asserted
/// is that the method was ACCEPTED: the answer is not an "invalid --optimizer" refusal.
#[test]
fn every_protocol_method_is_accepted_by_the_daemon() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for name in vike_model::run_request::search_methods::SEARCH_METHODS {
        let search = SearchSelectionWire {
            optimizer: Some(name.to_string()),
            // genetic REQUIRES a seed; supplying one for every method would be refused by the
            // owners rule, so it is supplied for exactly the method that owns it.
            seed: (name == "genetic").then(|| "7".to_string()),
            ..SearchSelectionWire::default()
        };
        write_frame(
            &mut stream,
            &Request::RunParamscanProfile {
                profile_toml: paramscan_profile(),
                rank_by: None,
                search: Some(search),
            },
        )
        .expect("write");
        match read_frame::<_, Response>(&mut stream).expect("the connection must survive") {
            Response::ParamscanReport(_) => {}
            Response::Error(msg) => panic!("{name} was refused: {msg}"),
            other => panic!("{name}: expected ParamscanReport, got {other:?}"),
        }
    }
}

/// A knob under a method that does not own it is refused BY NAME, over the wire, with the SAME
/// sentence `--local` produces — the property `search::select` exists for. Before stage 7 this argv
/// could not reach the daemon at all: `vike-cli` refused it at parse.
///
/// ⚠ Two rows, and the SECOND is the one that would rot quietly. `--trials` has ONE owner (tpe) and
/// `--seed` has TWO (tpe AND genetic), and `search::select::refuse_unowned_knob` renders EVERY owner
/// rather than the first — an operator who wrote `--seed 7` under the grid must be told about
/// genetic as well. A single-owner row cannot tell those two renderings apart.
#[test]
fn a_knob_under_the_wrong_method_is_refused_over_the_wire() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for (label, search, owners) in [
        (
            "--trials",
            SearchSelectionWire {
                optimizer: Some("grid".to_string()),
                trials: Some("8".to_string()),
                ..SearchSelectionWire::default()
            },
            &["tpe"][..],
        ),
        (
            "--seed",
            SearchSelectionWire {
                optimizer: Some("grid".to_string()),
                seed: Some("7".to_string()),
                ..SearchSelectionWire::default()
            },
            &["tpe", "genetic"][..],
        ),
    ] {
        write_frame(
            &mut stream,
            &Request::RunParamscanProfile {
                profile_toml: paramscan_profile(),
                rank_by: None,
                search: Some(search),
            },
        )
        .expect("write");
        match read_frame::<_, Response>(&mut stream).expect("the connection must survive") {
            Response::Error(msg) => {
                assert!(msg.contains(label), "names the knob: {msg}");
                assert!(msg.contains("grid"), "names the method that was selected: {msg}");
                for owner in owners {
                    assert!(msg.contains(owner), "names EVERY owner ({owner}): {msg}");
                }
            }
            other => panic!("{label} under grid must be refused, got {other:?}"),
        }
    }
    // …and the connection survives a refused request, like every other refusal on this daemon.
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert_matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong));
}

/// `--rank-by multi` needed no new wire field — the existing `rank_by` string could always carry it
/// — and was refused only because this daemon resolved it through `RankMetric::from_str_ci`, whose
/// four arms have no `multi`. It resolves now, and the report says so in its own `rank_by`.
#[test]
fn the_composite_ranking_is_served() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(
        &mut stream,
        &Request::RunParamscanProfile {
            profile_toml: paramscan_profile(),
            rank_by: Some("multi".to_string()),
            search: None,
        },
    )
    .expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::ParamscanReport(json) => {
            assert!(json.contains("\"rank_by\":\"multi\""), "the answer names it: {json}");
        }
        other => panic!("expected ParamscanReport, got {other:?}"),
    }
}

/// A method named on a profile with NO grid table is refused by the table's own message — the same
/// sentence the local engine's own pre-flight produces. This is what makes `vike-cli`'s routing
/// change safe: sending such a run HERE is how it gets an answer, rather than silently becoming a
/// single backtest over `RunBacktest`.
#[test]
fn a_method_without_a_grid_is_refused_by_the_missing_table() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(
        &mut stream,
        &Request::RunParamscanProfile {
            profile_toml: MINIMAL_BAR_PROFILE.to_string(),
            rank_by: None,
            search: Some(SearchSelectionWire {
                optimizer: Some("tpe".to_string()),
                ..SearchSelectionWire::default()
            }),
        },
    )
    .expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::Error(msg) => {
            assert!(msg.contains("[paramscan]"), "names the missing table: {msg}");
        }
        other => panic!("expected an error, got {other:?}"),
    }
}

/// The TWIN of the refusal above, one verb over: a profile that DECLARES a grid is refused by
/// `RunBacktest`, which runs one point, instead of being answered with a single backtest that
/// silently ignored the grid. The `[sweep]` spelling is the same table (a serde alias), so it is
/// refused too; an EMPTY table is no grid (`ProfileBacktest::is_paramscan`) and still runs.
#[test]
fn a_grid_profile_is_refused_by_the_single_backtest_verb() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for profile_toml in
        [paramscan_profile(), format!("{MINIMAL_BAR_PROFILE}\n[paramscan]\nsize = [1.0, 2.0]\n")]
    {
        write_frame(&mut stream, &Request::RunBacktest(profile_toml)).expect("write");
        match read_frame::<_, Response>(&mut stream).expect("the connection must survive") {
            Response::Error(msg) => {
                assert!(msg.contains("[paramscan]"), "names the grid table: {msg}");
                assert!(msg.contains("RunParamscanProfile"), "names the verb that runs it: {msg}");
                assert!(msg.contains("run_sweep"), "names the MCP tool that runs it: {msg}");
            }
            other => panic!("a grid profile must be refused by RunBacktest, got {other:?}"),
        }
    }
    // An EMPTY grid table is no search: the verb runs it as the one backtest it always was.
    write_frame(
        &mut stream,
        &Request::RunBacktest(format!("{MINIMAL_BAR_PROFILE}\n[paramscan]\n")),
    )
    .expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::Report(json) => {
            let v: serde_json::Value = serde_json::from_str(&json).expect("report is JSON");
            assert!(v.get("rows").is_none(), "a BacktestReport, not a ranked search: {json}");
        }
        other => panic!("an empty [paramscan] table is one backtest, got {other:?}"),
    }
}

/// The HANDSHAKE half: this daemon advertises the capability UNCONDITIONALLY, which is what lets a
/// client refuse locally instead of sending a frame an older peer would silently downgrade.
#[test]
fn the_backtesthub_advertises_the_search_capability() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let client = BacktesthubClient::connect(addr).expect("connect");
    assert!(
        client.features().iter().any(|f| f == FEATURE_SEARCH_METHOD),
        "{:?}",
        client.features()
    );
}

/// **The walk-forward SEARCH capability is advertised by every backtesthub.** It rides
/// `Request::RunWalkforward`, whose runner is `vike-studio-core`'s and is called directly, so there
/// is no mount for it to depend on any more. (It used to be MOUNT-conditional, and the test asserted
/// both directions: an unmounted daemon must not advertise a search it cannot run. That direction
/// went with the Studio-less mode.) The other capability is asserted beside it so the check cannot
/// pass by an empty list.
#[test]
fn the_daemon_advertises_the_walkforward_search_capability() {
    let client =
        BacktesthubClient::connect(spawn_server(vike_strategies_check::stand_in)).expect("connect");
    assert!(
        client.features().iter().any(|f| f == FEATURE_WALKFORWARD_SEARCH),
        "a daemon whose Studio runners can search must say so: {:?}",
        client.features()
    );
    assert!(
        client.features().iter().any(|f| f == FEATURE_SLIPPAGE_MODEL),
        "a daemon that writes the Studio's slippage model into [engine.slippage] says so: {:?}",
        client.features()
    );
    assert!(
        client.features().iter().any(|f| f == FEATURE_SEARCH_METHOD),
        "…and the profile-verb one is there too: {:?}",
        client.features()
    );
}

/// `RunStudy` is served by every daemon too: it reaches `vike-studio-core`'s real study runner, whose
/// FIRST step is parsing the recipe, so an unparsable recipe is answered with that runner's own clean
/// error (nothing is run and nothing is written) and the CONNECTION SURVIVES. This is the
/// replacement for the two tests that drove a stub `StudyRunFn` and an unmounted daemon's refusal —
/// both gone with the injected seam; the study itself is `vike-studio-core`'s own tests' business.
#[test]
fn a_study_with_an_unparsable_recipe_is_refused_by_the_real_runner_and_the_connection_survives() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(
        &mut stream,
        &Request::RunStudy(Box::new(StudyRunRequestWire {
            study: "cohort".to_string(),
            recipe_toml: "not = [valid".to_string(),
            from: "1".to_string(),
            to: "2".to_string(),
        })),
    )
    .expect("write");
    match read_frame::<_, Response>(&mut stream).expect("the connection must survive") {
        Response::Error(msg) => {
            assert!(msg.contains("recipe TOML did not parse"), "the real runner's refusal: {msg}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert_matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong));
}

/// THE ADVERTISEMENT: every backtesthub advertises the study capability beside the unconditional
/// search one (it was a MOUNT fact, conditional on a study runner being handed in, until the runner
/// became a direct call).
#[test]
fn every_daemon_advertises_the_study_capability() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let client = BacktesthubClient::connect(addr).expect("connect");
    assert!(client.features().iter().any(|f| f == FEATURE_STUDY), "{:?}", client.features());
    assert!(
        client.features().iter().any(|f| f == FEATURE_SEARCH_METHOD),
        "…and the search one, so this cannot pass by an empty list: {:?}",
        client.features()
    );
}

// ---- the NAMED RUN (docs/decisions/0064-a-named-run-carries-no-source.md) -----------------------

/// A window this daemon will read: three days of `1d` bars, well inside `NAMED_RUN_MAX_BARS`.
fn named_spec(strategy: &str) -> NamedRunSpec {
    NamedRunSpec {
        strategy: strategy.to_string(),
        params: Vec::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1d".into(),
        start: 0,
        end: 86_400_000 * 3,
    }
}

/// Bind an ephemeral loopback listener and spawn the backtesthub with the NAMED-RUN lane ARMED.
fn spawn_server_named_run_armed() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let _ = serve_authed(
            listener,
            store,
            StudyDirs::working_directory(),
            None,
            NamedRunLane::ARMED,
            SERVER_IDLE_READ_TIMEOUT,
        );
    });
    addr
}

/// **The capability is a BUILD fact, so EVERY backtesthub advertises it — armed or not.**
///
/// This is the leg that makes an OLD server and an UNARMED one two different sentences rather than
/// one silence (0064's decision 8). `FEATURE_STUDY` is unconditional now too (it was a MOUNT fact,
/// conditional on a study runner being handed in, until the runner became a direct call).
#[test]
fn every_backtesthub_advertises_the_named_run_capability() {
    for addr in [spawn_server(vike_strategies_check::stand_in), spawn_server_named_run_armed()] {
        let client = BacktesthubClient::connect(addr).expect("connect");
        assert!(
            client.features().iter().any(|f| f == FEATURE_NAMED_RUN),
            "{:?}",
            client.features()
        );
    }
}

/// **An UNARMED daemon ANSWERS BOTH VERBS and runs nothing** — 0064's decision 8, over a real
/// socket. The roster it reports is EMPTY — the arming gates the NAMES as well as the run (decision
/// 8 leg 3), so a picker renders `NamedRunRoster::unarmed_note` rather than the bare empty list.
#[test]
fn an_unarmed_daemon_answers_the_named_run_and_runs_nothing() {
    let addr = spawn_server(vike_strategies_check::stand_in);
    let mut client = BacktesthubClient::connect(addr).expect("connect");

    let roster = client.named_strategies().expect("an unarmed daemon still answers the roster");
    assert!(!roster.armed, "this daemon was not armed");
    // ...and it NAMES NOTHING — 0064's decision 8 leg 3, the reason arming gates the roster and not
    // only the run. The client renders `NamedRunRoster::unarmed_note`, so this is a teaching refusal
    // rather than an empty answer.
    assert!(
        roster.strategies.is_empty(),
        "an UNARMED daemon published its operator's strategy names over the wire: {:?}",
        roster.strategies
    );

    let outcome = client.run_named(&named_spec("buy_hold")).expect("it answers the run too");
    assert_eq!(outcome, NamedRunOutcome::NotArmed);
}

/// **The two rosters are DIFFERENT** — 0064's decision 7. `ListStrategies` answers the daemon's
/// strategy FILES; the named-run roster names compiled strategies, and since plan 2b none is
/// compiled into the server, so even an ARMED daemon names none.
#[test]
fn the_named_roster_is_not_the_list_strategies_roster() {
    // ⚠ ARMED, necessarily: an unarmed daemon names nothing at all (decision 8 leg 3), so the
    // emptiness below would be the arming and not the roster.
    let addr = spawn_server_named_run_armed();
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    let roster = client.named_strategies().expect("the named-run roster");
    assert!(roster.armed, "this daemon was armed");
    assert!(roster.strategies.is_empty(), "no strategy is compiled in: {:?}", roster.strategies);
}

/// **An ARMED daemon resolves no name over a real socket** (plan 2b) — a shipped file's stem and the
/// script path alike are refused as unknown, with this server's own (empty) roster rather than a
/// shrug.
#[test]
fn an_armed_daemon_resolves_no_name_and_still_cannot_be_asked_for_a_script() {
    let addr = spawn_server_named_run_armed();
    let mut client = BacktesthubClient::connect(addr).expect("connect");
    assert!(client.named_strategies().expect("roster").armed);

    assert_eq!(
        client.run_named(&named_spec("buy_hold")).expect("an armed daemon answers"),
        NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy { known: Vec::new() })
    );

    // ...and `rhai` is refused as an UNKNOWN NAME carrying the roster — not as a compile error,
    // because there is no compiler at the end of this path to raise one.
    match client.run_named(&named_spec("rhai")).expect("the script name answers") {
        NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy { known }) => {
            assert!(!known.iter().any(|k| k == "rhai"), "{known:?}");
        }
        other => panic!("expected an unknown-strategy refusal, got {other:?}"),
    }
}

/// **The SERVER re-checks every bound, and the refusal names the constant it hit.**
///
/// The client refuses the same request locally before writing a frame, so this test goes AROUND the
/// client validator with raw frames — otherwise it would prove the client copy and say nothing
/// about the enforcement, which is the leg that actually matters.
#[test]
fn the_server_refuses_an_over_wide_window_by_name_over_raw_frames() {
    let addr = spawn_server_named_run_armed();
    let mut stream = TcpStream::connect(addr).expect("connect");
    let mut wide = named_spec("buy_hold");
    wide.interval = "1s".into();
    wide.end = 86_400_000 * 30; // ~2.6M one-second bars
    write_frame(&mut stream, &Request::RunNamed(Box::new(wide))).expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::Error(msg) => {
            assert!(msg.contains("NAMED_RUN_MAX_BARS"), "the refusal names its bound: {msg}");
        }
        other => panic!("an over-wide window must be refused, got {other:?}"),
    }
    // ...and the connection SURVIVES a refusal, like every other bad request on this daemon.
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert_matches!(read_frame::<_, Response>(&mut stream).expect("read"), Response::Pong);
}

/// **The reserved SOURCE key is refused by the server too**, and the message says where the script
/// path actually lives. The belt, not the fence — but a belt whose refusal teaches.
#[test]
fn the_server_refuses_a_reserved_source_param_over_raw_frames() {
    let addr = spawn_server_named_run_armed();
    let mut stream = TcpStream::connect(addr).expect("connect");
    let mut spec = named_spec("buy_hold");
    spec.params.push((vike_model::RESERVED_SOURCE_KEY.to_string(), NamedRunParam::Int(1)));
    write_frame(&mut stream, &Request::RunNamed(Box::new(spec))).expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::Error(msg) => {
            assert!(msg.contains("carries no source"), "{msg}");
            assert!(msg.contains("--script"), "and it names the verb that DOES ship one: {msg}");
        }
        other => panic!("a `src` param must be refused, got {other:?}"),
    }
}

/// **The REAL keyed backtesthub is the plane a backtest-plane Write key may sign toward** — and
/// the data plane is not what it answers as.
///
/// `vike_backtesthub_client::BacktesthubClient::connect_authed_on` refuses to sign unless the pre-auth
/// `Welcome` names the plane it was asked for, which it reads through
/// `vike_hub_protocol::plane::welcome_plane` against `BACKTEST_PLANE_SENTINEL`. This daemon's
/// `served_features` pushes that constant; this test is what keeps the two one fact rather than
/// two spellings, over a real socket: a server that stopped pushing it would refuse every keyed
/// Studio Run, and this reddens first. The keyed and the key-less daemon classify the same way: the
/// Studio feature strings change the list and must not change the
/// plane. The data daemon's mirror image is
/// `crates/vike-datahub/tests/default_build/plane_split.rs`'s
/// `against_the_real_keyed_datahub_only_the_guard_stands_between_a_write_key_and_a_store_session`.
#[test]
fn a_keyed_backtesthub_is_the_plane_a_backtest_key_may_sign_toward() {
    use vike_backtesthub_client::proto::STUDIO_RUNNER_SENTINEL;
    use vike_hub_protocol::auth::{HubKeys, Scope};
    use vike_hub_protocol::plane::welcome_plane;

    let keys = || HubKeys::new(b"observe-placeholder".to_vec(), b"control-placeholder".to_vec());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    let served = keys();
    thread::spawn(move || {
        let _ = serve_authed(
            listener,
            store,
            StudyDirs::working_directory(),
            Some(served),
            NamedRunLane::DISARMED,
            SERVER_IDLE_READ_TIMEOUT,
        );
    });

    let client = BacktesthubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Backtest)
        .expect("the backtesthub's Welcome must classify as the BACKTEST plane");
    assert_eq!(client.authenticated_scope(), Some(Scope::Write));
    assert_eq!(welcome_plane(client.features()), Some(Plane::Backtest), "{:?}", client.features());
    drop(client);

    let err = BacktesthubClient::connect_authed_on(addr, &keys(), Scope::Read, Plane::Data)
        .expect_err("asked for the DATA plane, the backtesthub must be refused");
    assert!(err.to_string().contains("backtesthub"), "{err}");

    // ...and the key-less daemon, which advertises the Studio runners, classifies the same way.
    let mounted = spawn_server(vike_strategies_check::stand_in);
    let client = BacktesthubClient::connect(mounted).expect("connect");
    assert!(
        client.features().iter().any(|f| f == STUDIO_RUNNER_SENTINEL),
        "{:?}",
        client.features()
    );
    assert_eq!(welcome_plane(client.features()), Some(Plane::Backtest), "{:?}", client.features());
}

/// Two `Ping`s PIPELINED in one write: the backtesthub's second `Pong` is written while its
/// first is still unacknowledged, and must not wait for this client's delayed ACK — the accepted
/// socket carries `TCP_NODELAY` (`vike_hub_protocol::frame::configure_hub_stream`).
/// `crates/vike-datahub/tests/default_build/wire_latency.rs`'s `back_to_back_answers_cost_no_delayed_ack` argues
/// the shape and the budget (half of Linux's 40 ms delayed-ACK floor per pair, two orders of
/// magnitude from both a stalling server and a healthy one); this is the same check on the other
/// daemon that speaks the datahub protocol.
#[test]
fn back_to_back_answers_cost_no_delayed_ack() {
    use std::io::Write;
    use std::time::{Duration, Instant};

    const PAIRS: u32 = 200;
    const BUDGET: Duration = Duration::from_millis(20 * PAIRS as u64);
    let pong = |s: &mut TcpStream| match read_frame::<_, Response>(s).expect("read") {
        Response::Pong => {}
        other => panic!("a Ping must be answered with Pong, got {other:?}"),
    };
    let mut s = TcpStream::connect(spawn_server(vike_strategies_check::stand_in)).expect("connect");
    write_frame(&mut s, &Request::Ping).expect("write");
    pong(&mut s); // connection setup is not what this measures
    let mut two = Vec::new();
    write_frame(&mut two, &Request::Ping).expect("frame the first Ping");
    write_frame(&mut two, &Request::Ping).expect("frame the second Ping");
    let started = Instant::now();
    for _ in 0..PAIRS {
        s.write_all(&two).expect("write both requests in ONE write");
        pong(&mut s);
        pong(&mut s);
    }
    let took = started.elapsed();
    assert!(
        took < BUDGET,
        "{PAIRS} pipelined pairs took {took:?} (budget {BUDGET:?}): the backtesthub's second \
         answer is waiting for the client's delayed ACK of its first — the accepted socket is \
         missing TCP_NODELAY (vike_hub_protocol::frame::configure_hub_stream)"
    );
}
