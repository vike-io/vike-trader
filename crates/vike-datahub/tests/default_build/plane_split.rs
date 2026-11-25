//! The DATA half of ruling 7's split: this daemon answers a COMPUTE verb with a clean, named
//! `Response::Error` and keeps the connection, and it advertises none of them.
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0 requires BOTH
//! directions to be tested. The other half —`vike-backend backtesthub --addr` refusing a DATA verb —
//! is `crates/vike-backtesthub/tests/backtest_plane.rs`, and the two are deliberate mirror images: the
//! same helper (`vike_hub_protocol::plane`'s `wrong_plane_message`) writes both texts, so a change
//! to one refusal cannot leave the other behind.
//!
//! Hermetic: an ephemeral `127.0.0.1:0` listener over `HistoryStoreMemory`, no prod store, no network.

use std::assert_matches;
use std::sync::Arc;

use vike_backtesthub_client::proto::{Request, request_kind};
use vike_data::HistoryStoreMemory;
use vike_datahub_client::DatahubClient;
use vike_datahub_client::proto::Response;
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::frame::{read_frame, read_frame_raw, write_frame};
use vike_hub_protocol::handshake::HandshakeReply;
use vike_hub_protocol::plane::Plane;

use vike_datahub::test_support::{authed_stream, keys, spawn_server};

/// A minimal, valid bar-mode profile — a REAL payload, so what is being proved is the refusal
/// rather than a decode failure that would look the same from the outside.
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

/// Every COMPUTE verb, with a payload a server would actually accept if it served them.
///
/// ⚠ Derived-checked rather than hand-trusted: `every_backtest_verb_is_covered` below asserts this
/// list is exactly the [`Plane::Backtest`] set, so a new backtest verb reddens this file until its
/// author adds a sample.
fn compute_requests() -> Vec<Request> {
    use vike_backtesthub_client::wire_studio::{
        DataSliceWire, ParamscanWire, ReplayKindWire, StrategySpecWire, WalkforwardWire,
    };
    let slice = || {
        Box::new(DataSliceWire {
            venue: "binance".into(),
            symbols: vec!["BTCUSDT".into()],
            interval: "1d".into(),
            start: None,
            end: None,
            kind: ReplayKindWire::Bars,
        })
    };
    let spec = || StrategySpecWire::Rhai("fn on_bar(){}".to_string());
    vec![
        Request::RunBacktest(MINIMAL_BAR_PROFILE.to_string()),
        Request::RunParamscanProfile {
            profile_toml: MINIMAL_BAR_PROFILE.to_string(),
            rank_by: None,
            search: None,
        },
        Request::RunWalkforwardProfile { profile_toml: MINIMAL_BAR_PROFILE.to_string() },
        Request::ListStrategies,
        Request::RunSlice { spec: spec(), slice: slice(), params: None },
        Request::RunParamscan {
            spec: spec(),
            slice: slice(),
            paramscan: ParamscanWire { axes: Vec::new() },
            params: None,
        },
        Request::RunWalkforward {
            spec: spec(),
            slice: slice(),
            walkforward: WalkforwardWire::fixed(2),
            params: None,
        },
        // Ruling R1's research-plane study, the EIGHTH backtest verb and the one that arrived
        // after ruling 7 moved the other seven. `vike_backtesthub_client::proto`'s `FEATURE_STUDY`
        // named the backtesthub as its server before the verb existed.
        Request::RunStudy(Box::new(vike_backtesthub_client::proto::StudyRunRequestWire {
            study: "cohort".to_string(),
            recipe_toml: String::new(),
            from: "1".to_string(),
            to: "2".to_string(),
            ..vike_backtesthub_client::proto::StudyRunRequestWire::default()
        })),
        // The NAMED RUN pair (`docs/decisions/0064-a-named-run-carries-no-source.md`): scope Observe,
        // plane Backtest.
        Request::NamedStrategies,
        Request::RunNamed(Box::new(vike_backtesthub_client::named_run::NamedRunSpec {
            strategy: "buy_hold".to_string(),
            params: Vec::new(),
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1h".to_string(),
            start: 0,
            end: 3_600_000,
        })),
        // The strategy-file pair a Studio lists and opens a file with (plan 2b), plane Backtest.
        Request::ListStrategyFiles,
        Request::ReadStrategyFile { file: "trend/momentum.rs".to_string() },
        Request::SaveStrategyFile {
            file: "trend/momentum.rs".to_string(),
            text: "//! x\n".to_string(),
        },
        // The Studio's Build, which the backtesthub forwards to the strategy builder.
        Request::BuildStrategyFile {
            file: "trend/momentum.rs".to_string(),
            text: "//! x\n".to_string(),
            sdk: None,
        },
        // The summarized Studio run and its two reads (decision 0078's amendment of 2026-11-16).
        Request::Summarized(Box::new(Request::RunSlice {
            spec: spec(),
            slice: slice(),
            params: None,
        })),
        Request::ReadTrades { run: "a-1".to_string(), point: None, page: 0, page_size: 200 },
        Request::ReadPoint { run: "a-1".to_string(), point: 1 },
        // The whole curve on request (decision 0078's amendment of 2026-11-18).
        Request::ReadCurve { run: "a-1".to_string(), page: 0, page_size: 100_000 },
        // The server's studies and saved runs (the owner's ruling of 2026-11-19: studies run only on
        // the server).
        Request::ListStudies,
        Request::ListRuns { page: 0, page_size: 200 },
        Request::ReadRunArtifact {
            run_id: "1763424000-1".to_string(),
            file: "fold_scores.csv".to_string(),
            offset: 0,
            length: 0,
        },
        // The versioned strategy SDK's source (decision 0296, verdict 3).
        Request::ListSdks,
        Request::ReadSdkFile {
            sdk: "sdk-3".to_string(),
            // Two literals: xtask's read-as-text scan (`SOURCES_READ_AS_TEXT`) would take one for a read.
            path: concat!("crates", "/vike-tactics/src/lib.rs").to_string(),
        },
        // A summarized profile search's later pages (decision 0078's amendment of 2026-11-19).
        Request::ReadParamscanRows { run: "a-1".to_string(), page: 1, page_size: 1000 },
        // A run verb run again although a saved run would answer it (decision 0078's amendment of 2026-11-20).
        Request::Fresh(Box::new(Request::RunSlice { spec: spec(), slice: slice(), params: None })),
    ]
}

/// The sample set IS the backtest plane — no more, no less.
#[test]
fn every_backtest_verb_is_covered() {
    let mut samples: Vec<&str> = compute_requests().iter().map(request_kind).collect();
    samples.sort_unstable();
    let mut roster = Plane::Backtest.verbs().to_vec();
    roster.sort_unstable();
    assert_eq!(
        samples, roster,
        "seven from ruling 7, R1's study, 0064's named pair, the strategy-file verbs, the Build"
    );
}

/// The bytes the data daemon answers each backtest verb with, one line per [`compute_requests`] row.
const FROZEN_REFUSALS: &str = "fixtures/datahub_wire/v7_wrong_plane_refusals_at_datahub.jsonl";

/// ⚠ **The refusal's EXACT BYTES, frozen before the backtesthub's protocol left this daemon's enum.**
/// A client that dialled the wrong hub reads this sentence, so a refactor that loses it (a parse
/// failure, a dropped socket, a hang) reddens here rather than on an operator's screen.
#[test]
fn the_refusal_bytes_of_every_backtest_verb_are_frozen() {
    let addr = spawn_server(Arc::new(HistoryStoreMemory::new()));
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    let actual: Vec<String> = compute_requests()
        .iter()
        .map(|request| {
            write_frame(&mut stream, request).expect("write");
            let body = read_frame_raw(&mut stream).expect("answered, never dropped");
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

/// Before `Hello` on a KEYED daemon a backtest verb is an unauthenticated verb like any other: the
/// same words, whichever plane it belongs to, and the socket closes.
#[test]
fn a_backtest_verb_before_hello_on_a_keyed_daemon_is_told_to_say_hello_first() {
    let addr = spawn_keyed();
    for request in compute_requests() {
        let mut stream = std::net::TcpStream::connect(addr).expect("connect");
        write_frame(&mut stream, &request).expect("write");
        assert_matches!(
            read_frame::<_, HandshakeReply>(&mut stream),
            Ok(HandshakeReply::AuthDenied { reason }) if reason == "not authenticated: expected Hello first"
        );
    }
}

/// ⚠ THE PLANE IS ASKED BEFORE THE SCOPE: a read-scope connection sending a verb this daemon does not
/// serve is told the address it wants, not that it needs a bigger key.
#[test]
fn a_backtest_verb_on_a_read_scope_connection_is_told_the_address_not_a_scope() {
    let addr = spawn_keyed();
    let mut stream = authed_stream(addr, &keys(), Scope::Read);
    for request in compute_requests() {
        let kind = request_kind(&request);
        write_frame(&mut stream, &request).expect("write");
        match read_frame::<_, Response>(&mut stream).expect("the connection survives") {
            Response::Error(msg) => {
                assert_eq!(
                    msg,
                    format!(
                        "{kind} is a Backtest-plane verb and this is the Data daemon — it is \
                         served by `vike-backend backtesthub --addr`. Point the client at that \
                         daemon (config.backtest_addr); this connection stays open and every \
                         Data-plane verb still works on it."
                    ),
                    "the address it wants, not a scope question"
                );
            }
            other => panic!("{kind} must be refused by plane, got {other:?}"),
        }
    }
}

/// ⚠ **The hazard, closed against the REAL data daemon rather than asserted.** The unguarded
/// backtesthub constructor, handed a Write key and pointed at the keyed datahub, opens no session on
/// the plane that carries `Backfill` and `DeleteSeries`: since backtesthub protocol 9 the two daemons'
/// numbers differ, so it names the DATA daemon at the handshake and never signs
/// (`vike_hub_protocol::conn::WrongDaemon`); at equal numbers it signed under the backtesthub's own
/// separator (decision 0288's amendment) and was refused `bad mac`. The guarded one, at the same
/// address, is refused before it signs. And asked for the DATA plane under the datahub's separator it
/// succeeds, which is the proof that the real server's `Welcome` classifies as that plane (so its
/// sentinel is the one the guard compares).
#[test]
fn against_the_real_keyed_datahub_a_backtesthub_login_opens_no_store_session() {
    use vike_backtesthub_client::BacktesthubClient;
    use vike_hub_protocol::auth::DATAHUB_DOMAIN;
    use vike_hub_protocol::conn::{HubConnection, WrongDaemon};

    let addr = spawn_keyed();

    let err = BacktesthubClient::connect_authed(addr, &keys(), Scope::Write)
        .expect_err("the unguarded backtesthub login opens nothing at the datahub");
    let wrong = WrongDaemon::of(&err).expect("named as the other daemon, before it signs");
    assert_eq!(wrong.found, Plane::Data, "{err}");
    assert!(err.to_string().contains("DATA daemon"), "{err}");

    let err = BacktesthubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Backtest)
        .expect_err("the guard refuses the data daemon for a backtest-plane key");
    assert!(err.to_string().contains("DATA daemon"), "{err}");

    let data = HubConnection::connect_authed_on(
        addr,
        &keys(),
        Scope::Read,
        Plane::Data,
        DATAHUB_DOMAIN,
        vike_datahub_client::PROTO_VERSION,
    )
    .expect("the real datahub's Welcome must classify as the DATA plane");
    assert_eq!(data.authenticated_scope(), Some(Scope::Read));
}

fn spawn_keyed() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let store: Arc<dyn vike_data::HistoryStore + Send + Sync> =
            Arc::new(HistoryStoreMemory::new());
        let _ = vike_datahub::serve_authed(listener, store, None, Some(keys()), None, None, None);
    });
    addr
}

/// ⚠ THE REFUSAL: every backtest verb answers `Response::Error` naming the command that serves it,
/// and the CONNECTION SURVIVES — one bad request is a bad *request*, never a bad *connection*. All
/// seven are sent on ONE connection, which is what makes the survival claim mean something.
#[test]
fn a_backtest_verb_is_refused_by_name_and_the_connection_survives() {
    let addr = spawn_server(Arc::new(HistoryStoreMemory::new()));
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for request in compute_requests() {
        let kind = request_kind(&request);
        write_frame(&mut stream, &request).expect("write");
        let response: Response = read_frame(&mut stream).expect("the connection must survive");
        match response {
            Response::Error(msg) => {
                assert!(msg.contains(kind), "the refusal names the verb: {msg}");
                assert!(
                    msg.contains("vike-backend backtesthub --addr"),
                    "the refusal names the command that DOES serve it: {msg}"
                );
                assert!(
                    msg.contains("config.backtest_addr"),
                    "the refusal names the settings key the client dialled from: {msg}"
                );
            }
            other => panic!("{kind} must be refused, not served: {other:?}"),
        }
    }
    // ...and the SAME connection still answers a data verb, which is the whole point of a refusal
    // that is not a drop.
    write_frame(&mut stream, &Request::Ping).expect("write ping");
    assert_matches!(read_frame::<_, Response>(&mut stream), Ok(Response::Pong));
}

/// The HANDSHAKE half: a `Welcome` from this daemon advertises no backtest verb, so a client that
/// reads the feature list never sends one. Advertisement and refusal are two legs of one contract —
/// the advertisement stops a well-behaved client, the refusal holds for a client that does not read
/// it, and neither substitutes for the other.
#[test]
fn the_handshake_advertises_no_backtest_verb() {
    let addr = spawn_server(Arc::new(HistoryStoreMemory::new()));
    let client = DatahubClient::connect(addr).expect("connect");
    for gone in [
        "backtest",
        "list_strategies",
        "run_paramscan_profile",
        "run_walkforward_profile",
        "run_slice",
        "run_sweep",
        "run_walkforward",
        // ⚠ Two CAPABILITY strings rather than verb names, and they belong to the backtesthub
        // for the same reason the verbs do: `search_method` says `RunParamscanProfile` can carry a
        // search METHOD, `study` says a study runner is MOUNTED. Neither is anything this server
        // can answer.
        "search_method",
        "study",
    ] {
        assert!(
            !client.features().iter().any(|f| f == gone),
            "the data daemon must not advertise {gone}: {:?}",
            client.features()
        );
    }
    // ...while the data verbs it DOES serve are still advertised, so this test cannot pass by the
    // feature list being empty.
    assert!(client.features().iter().any(|f| f == "load_bars"), "{:?}", client.features());
}
