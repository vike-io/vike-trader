//! The backtesthub half of ruling 7's split: `vike-backend backtesthub --addr` serves the seven compute
//! verbs and refuses every DATA verb by name, keeping the connection.
//!
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §0 requires BOTH
//! directions to be tested. The other half — the data daemon refusing a backtest verb — is
//! `crates/vike-datahub/tests/plane_split.rs`, and the two are deliberate mirror images: one helper
//! (`vike_datahub_client::proto`'s `wrong_plane_message`) writes both texts, so a change to one
//! refusal cannot leave the other behind.
//!
//! It also carries the round-trips that used to live in `vike-datahub`'s `roundtrip.rs` and moved
//! here with the verbs: `RunBacktest` over the wire, the invalid-profile refusal, the
//! `list_strategies` roster, and the profile-shaped sweep on a store-less build.
//!
//! Hermetic: an ephemeral `127.0.0.1:0` listener over `MemHistStore`, no prod store, no network.

use std::assert_matches;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use vike_backtest::named_run::NamedRunLane;
use vike_backtesthub::backtesthub_server::{StudyDirs, serve, serve_authed};
use vike_data::{HistStore, MemHistStore, StoreKindId};
use vike_datahub_client::named_run::{NamedParam, NamedRunOutcome, NamedRunRefusal, NamedRunSpec};
use vike_datahub_client::proto::{Plane, Request, Response, plane_of, request_kind, write_frame};
use vike_datahub_client::wire_studio::{WireSlice, WireSliceKind, WireSpec};
use vike_datahub_client::{DatahubClient, WireSearch, WireStudy, proto::read_frame};
use vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT;

/// A minimal, valid bar-mode profile. Over an empty `MemHistStore` it loads no bars, so the run
/// closes no trades — enough to exercise the full request → engine → report → response path.
///
/// ⚠ Moved here from `crates/vike-datahub/tests/roundtrip.rs` with the verbs that consume it.
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
name = "buy_hold"
[strategy.params]
size = 1.0
symbol = "BTCUSDT"
"#;

/// Bind an ephemeral loopback listener and spawn the backtesthub over a fresh in-memory store on
/// a detached thread. Every backtesthub serves the Studio runners and the study runner now (they are
/// `vike-studio-core`'s, called directly), so this one serves all of them.
fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = serve(listener, store);
    });
    addr
}

/// Every DATA verb, with a payload the data daemon would accept — so what is proved is the refusal,
/// not a decode failure that would look the same from outside.
fn data_requests() -> Vec<Request> {
    use vike_data::SeriesId;
    use vike_datahub_client::proto::SeriesSelector;
    vec![
        Request::LoadBars {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1h".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanQuotes {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::ScanTrades {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            start: None,
            end: None,
            limit: None,
        },
        Request::PropertiesAsOf { venue: "binance".into(), symbol: "BTCUSDT".into(), ts: 0 },
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
    ]
}

/// The sample set IS the data plane — no more, no less, so a new data verb reddens this file until
/// its author adds a sample.
#[test]
fn every_data_verb_is_covered() {
    assert!(
        data_requests().iter().all(|r| plane_of(r) == Plane::Data),
        "a sample in this file is not a data verb"
    );
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
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for request in data_requests() {
        let kind = request_kind(&request);
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
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");
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

    let mut client2 = DatahubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after a backtest request");
}

/// A malformed profile gets a clean `Response::Error`, and the server keeps serving — one bad
/// request never takes down the accept loop. (`from = "999999"` > `to = "100000"` fails
/// `BacktestProfile::from_toml_str`'s validation SERVER-side, which is where the client ships it.)
#[test]
fn invalid_profile_yields_error_not_a_dropped_connection() {
    let addr = spawn_server();
    let invalid = MINIMAL_BAR_PROFILE.replace("from = \"0\"", "from = \"999999\"");

    let mut client = DatahubClient::connect(addr).expect("connect");
    match client.run_backtest(&invalid) {
        Err(msg) => assert!(!msg.is_empty(), "expected a server-side validation error"),
        Ok(_) => panic!("an invalid (from > to) profile must not produce a report"),
    }

    let mut client2 = DatahubClient::connect(addr).expect("reconnect");
    client2.ping().expect("server still serving after an invalid request");
}

/// The `list_strategies` verb round-trips, and the handshake advertises it — the roster is
/// store-independent, which is exactly why ruling 7 moved it here beside this binary's own `--list` flag, the
/// other transport for the same constant.
#[test]
fn list_strategies_round_trips_the_native_roster() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");

    let roster = client.list_strategies().expect("list_strategies must return the roster");
    assert!(!roster.is_empty(), "the native strategy roster must be non-empty");
    assert!(roster.iter().any(|s| s == "buy_hold"), "roster must contain buy_hold: {roster:?}");
    // The rhai SCRIPT arm is deliberately excluded from the native roster.
    assert!(!roster.iter().any(|s| s == "rhai"), "rhai must NOT be in the native roster");

    assert!(
        client.features().iter().any(|f| f == "list_strategies"),
        "server must advertise the list_strategies feature: {:?}",
        client.features()
    );
    // ...and it is the SAME constant the `--list` flag reads, which is the property that made this
    // verb belong on this daemon.
    assert_eq!(roster.len(), vike_backtest::harness::STRATEGIES.len());
}

/// ⚠ The three STUDIO verbs are advertised by EVERY backtesthub and dispatched to
/// `vike-studio-core`'s real runners — there is no table to mount and no Studio-less mode (the
/// `StudioRunTable` seam this file used to drive with stub closures is deleted, and with it the
/// test that asserted an unmounted daemon refused the three by name and advertised none).
///
/// Over the empty `MemHistStore` the real runner cannot produce a result, so what this proves is the
/// DISPATCH and the advertisement: the answer is the runner's own classified, kind-first error
/// (`WireRunError::to_error_string`), not the old "served only by a build that mounts the Studio
/// runners" refusal, and the connection survives it. The computation itself is
/// `crates/vike-studio-core/tests/studio_wire_parity.rs`'s gate.
#[test]
fn the_studio_verbs_are_advertised_and_dispatched_to_the_real_runners() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");
    for advertised in ["run_slice", "run_sweep", "run_walkforward"] {
        assert!(
            client.features().iter().any(|f| f == advertised),
            "every backtesthub must advertise {advertised}: {:?}",
            client.features()
        );
    }
    let err = client
        .run_slice(
            WireSpec::Rhai("fn on_bar(){}".to_string()),
            WireSlice {
                venue: "binance".into(),
                symbols: vec!["BTCUSDT".into()],
                interval: "1d".into(),
                start: None,
                end: None,
                kind: WireSliceKind::Bars,
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
/// The SECTION is `[paramscan]`, which is what the parser calls it since stage 7; `[sweep]` is a
/// permanent serde alias, held open by `crates/vike-backtest/src/harness/profile/tests/core_and_data.rs`'s
/// `both_the_new_section_and_the_legacy_one_load_and_parse_identically`.
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
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for name in vike_model::run_request::search_methods::SEARCH_METHODS {
        let search = WireSearch {
            optimizer: Some(name.to_string()),
            // genetic REQUIRES a seed; supplying one for every method would be refused by the
            // owners rule, so it is supplied for exactly the method that owns it.
            seed: (name == "genetic").then(|| "7".to_string()),
            ..WireSearch::default()
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
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for (label, search, owners) in [
        (
            "--trials",
            WireSearch {
                optimizer: Some("grid".to_string()),
                trials: Some("8".to_string()),
                ..WireSearch::default()
            },
            &["tpe"][..],
        ),
        (
            "--seed",
            WireSearch {
                optimizer: Some("grid".to_string()),
                seed: Some("7".to_string()),
                ..WireSearch::default()
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
    let addr = spawn_server();
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
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(
        &mut stream,
        &Request::RunParamscanProfile {
            profile_toml: MINIMAL_BAR_PROFILE.to_string(),
            rank_by: None,
            search: Some(WireSearch {
                optimizer: Some("tpe".to_string()),
                ..WireSearch::default()
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
/// refused too; an EMPTY table is no grid (`BacktestProfile::is_paramscan`) and still runs.
#[test]
fn a_grid_profile_is_refused_by_the_single_backtest_verb() {
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    for profile_toml in
        [paramscan_profile(), format!("{MINIMAL_BAR_PROFILE}\n[sweep]\nsize = [1.0, 2.0]\n")]
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
    let addr = spawn_server();
    let client = DatahubClient::connect(addr).expect("connect");
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_SEARCH_METHOD),
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
    let client = DatahubClient::connect(spawn_server()).expect("connect");
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_WALKFORWARD_SEARCH),
        "a daemon whose Studio runners can search must say so: {:?}",
        client.features()
    );
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_SEARCH_METHOD),
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
    let addr = spawn_server();
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    write_frame(
        &mut stream,
        &Request::RunStudy(Box::new(WireStudy {
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
    let addr = spawn_server();
    let client = DatahubClient::connect(addr).expect("connect");
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_STUDY),
        "{:?}",
        client.features()
    );
    assert!(
        client.features().iter().any(|f| f == vike_datahub_client::FEATURE_SEARCH_METHOD),
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
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
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
    for addr in [spawn_server(), spawn_server_named_run_armed()] {
        let client = DatahubClient::connect(addr).expect("connect");
        assert!(
            client.features().iter().any(|f| f == vike_datahub_client::FEATURE_NAMED_RUN),
            "{:?}",
            client.features()
        );
    }
}

/// **An UNARMED daemon ANSWERS BOTH VERBS and runs nothing** — 0064's decision 8, over a real
/// socket. The roster it reports is EMPTY — the arming gates the NAMES as well as the run (decision
/// 8 leg 3), so a picker renders `NamedRoster::unarmed_note` rather than the bare empty list.
#[test]
fn an_unarmed_daemon_answers_the_named_run_and_runs_nothing() {
    let addr = spawn_server();
    let mut client = DatahubClient::connect(addr).expect("connect");

    let roster = client.named_strategies().expect("an unarmed daemon still answers the roster");
    assert!(!roster.armed, "this daemon was not armed");
    // ...and it NAMES NOTHING — 0064's decision 8 leg 3, the reason arming gates the roster and not
    // only the run. The client renders `NamedRoster::unarmed_note`, so this is a teaching refusal
    // rather than an empty answer.
    assert!(
        roster.strategies.is_empty(),
        "an UNARMED daemon published its operator's strategy names over the wire: {:?}",
        roster.strategies
    );

    let outcome = client.run_named(&named_spec("buy_hold")).expect("it answers the run too");
    assert_eq!(outcome, NamedRunOutcome::NotArmed);
}

/// **The two rosters are DIFFERENT, and a picker built on the wrong one is wrong in both
/// directions** — 0064's decision 7. `ListStrategies` answers `harness::STRATEGIES`, which carries
/// the simulator-only arms that sit beside the Rhai compiler; the named-run roster cannot resolve
/// those, and it does resolve the operator's own user strategies, which `ListStrategies` has never
/// enumerated at all.
#[test]
fn the_named_roster_is_not_the_list_strategies_roster() {
    // ⚠ ARMED, necessarily: an unarmed daemon names nothing at all (decision 8 leg 3), so this
    // comparison would pass vacuously against `spawn_server()` — the kind of green that measures
    // the wrong thing.
    let addr = spawn_server_named_run_armed();
    let mut client = DatahubClient::connect(addr).expect("connect");
    let native = client.list_strategies().expect("the simulator roster");
    let named = client.named_strategies().expect("the named-run roster").strategies;

    assert!(!named.is_empty(), "the named-run roster must never be empty");
    // Neither roster carries the script arm...
    assert!(!native.iter().any(|s| s == "rhai"));
    assert!(!named.iter().any(|s| s == "rhai"));
    // ...but the DEFERRED simulator arms are on one and not the other, which is the whole point.
    let deferred: Vec<&str> = vike_strategy::SIMULATOR_ONLY.iter().map(|(n, _)| *n).collect();
    assert!(
        deferred.iter().any(|d| native.iter().any(|s| s == d)),
        "the simulator roster must carry the deferred arms, or this test proves nothing: {native:?}"
    );
    assert!(
        !deferred.iter().any(|d| named.iter().any(|s| s == d)),
        "a DEFERRED simulator arm is on the named-run roster. Those arms live in vike-backtest, \
         which CAN name vike-script, so they are outside the compiler-free closure — admitting one \
         is a REOPENER of docs/decisions/0064-a-named-run-carries-no-source.md, not a config \
         change. named={named:?}"
    );
}

/// **An ARMED daemon runs one pass over a real socket** — and the script path is still unreachable
/// by NAME, answered with this server's own roster rather than with a shrug.
#[test]
fn an_armed_daemon_runs_a_named_strategy_and_still_cannot_be_asked_for_a_script() {
    let addr = spawn_server_named_run_armed();
    let mut client = DatahubClient::connect(addr).expect("connect");
    assert!(client.named_strategies().expect("roster").armed);

    // The store is empty, so the run closes no trades — the same thing
    // `run_backtest_round_trips_and_server_survives` relies on. What is proven is the whole path.
    match client.run_named(&named_spec("buy_hold")).expect("an armed daemon runs") {
        NamedRunOutcome::Ran { report_json, .. } => {
            assert!(report_json.contains("buy_hold"), "the report names the run: {report_json}");
        }
        other => panic!("expected a run, got {other:?}"),
    }

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
    spec.params.push((vike_model::RESERVED_SRC_KEY.to_string(), NamedParam::Int(1)));
    write_frame(&mut stream, &Request::RunNamed(Box::new(spec))).expect("write");
    match read_frame::<_, Response>(&mut stream).expect("read") {
        Response::Error(msg) => {
            assert!(msg.contains("carries no source"), "{msg}");
            assert!(msg.contains("--script"), "and it names the verb that DOES ship one: {msg}");
        }
        other => panic!("a `src` param must be refused, got {other:?}"),
    }
}

/// The named-run pair is COMPUTE-plane — the classification whose opposite would have the data
/// daemon try to answer verbs it holds no roster and no engine for.
#[test]
fn the_named_run_pair_is_backtest_plane() {
    for r in [Request::NamedStrategies, Request::RunNamed(Box::new(named_spec("buy_hold")))] {
        assert_eq!(plane_of(&r), Plane::Backtest, "{r:?}");
    }
}

/// **The REAL keyed backtesthub is the plane a backtest-plane Write key may sign toward** — and
/// the data plane is not what it answers as.
///
/// `vike_datahub_client::DatahubClient::connect_authed_on` refuses to sign unless the pre-auth
/// `Welcome` names the plane it was asked for, which it reads through
/// `vike_datahub_client::welcome_plane` against `BACKTEST_PLANE_SENTINEL`. This daemon's
/// `served_features` pushes that constant; this test is what keeps the two one fact rather than
/// two spellings, over a real socket: a server that stopped pushing it would refuse every keyed
/// Studio Run, and this reddens first. The keyed and the key-less daemon classify the same way: the
/// Studio feature strings change the list and must not change the
/// plane. The data daemon's mirror image is
/// `crates/vike-datahub-client/tests/plane_guard.rs`'s
/// `against_the_real_keyed_datahub_only_the_guard_stands_between_a_write_key_and_a_store_session`.
#[test]
fn a_keyed_backtesthub_is_the_plane_a_backtest_key_may_sign_toward() {
    use vike_datahub_client::{STUDIO_RUNNER_SENTINEL, welcome_plane};
    use vike_hub_proto::auth::{HubKeys, Scope};

    let keys = || HubKeys::new(b"observe-placeholder".to_vec(), b"control-placeholder".to_vec());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
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

    let client = DatahubClient::connect_authed_on(addr, &keys(), Scope::Write, Plane::Backtest)
        .expect("the backtesthub's Welcome must classify as the BACKTEST plane");
    assert_eq!(client.authenticated_scope(), Some(Scope::Write));
    assert_eq!(welcome_plane(client.features()), Some(Plane::Backtest), "{:?}", client.features());
    drop(client);

    let err = DatahubClient::connect_authed_on(addr, &keys(), Scope::Read, Plane::Data)
        .expect_err("asked for the DATA plane, the backtesthub must be refused");
    assert!(err.to_string().contains("backtesthub"), "{err}");

    // ...and the key-less daemon, which advertises the Studio runners, classifies the same way.
    let mounted = spawn_server();
    let client = DatahubClient::connect(mounted).expect("connect");
    assert!(
        client.features().iter().any(|f| f == STUDIO_RUNNER_SENTINEL),
        "{:?}",
        client.features()
    );
    assert_eq!(welcome_plane(client.features()), Some(Plane::Backtest), "{:?}", client.features());
}

/// Two `Ping`s PIPELINED in one write: the backtesthub's second `Pong` is written while its
/// first is still unacknowledged, and must not wait for this client's delayed ACK — the accepted
/// socket carries `TCP_NODELAY` (`vike_hub_proto::frame::configure_hub_stream`).
/// `crates/vike-datahub/tests/wire_latency.rs`'s `back_to_back_answers_cost_no_delayed_ack` argues
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
    let mut s = TcpStream::connect(spawn_server()).expect("connect");
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
         missing TCP_NODELAY (vike_hub_proto::frame::configure_hub_stream)"
    );
}
