//! **The backtest plane's bytes, frozen before its protocol left this crate.** Every `Request` and
//! `Response` variant the backtesthub speaks, built with every optional field both present and absent,
//! encodes to EXACTLY the line committed for it, and that line decodes and re-encodes to itself. The files'
//! first lines are the handshake's, held through `vike_hub_protocol::handshake`'s types, so the deletion of
//! this protocol's copies (decision 0288's amendment of 2026-11-21) is proven to have moved no byte.
//!
//! Exact string equality, not `tests/wire_tag_fixtures.rs`' subset: this guards a MOVE, where a key
//! that changed its order or a field that appeared is the defect. A legitimate additive field later
//! adds a new line here; it never edits one.
//!
//! The expected bytes sit in `fixtures/`, out of reach of an edit under `crates/`. A mismatch prints
//! this build's whole encoding, so the capture is a run of this test; re-capturing to make it pass is
//! the failure this file exists to catch.

use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;
use vike_analytics::results::{TradingResults, TradingResultsExtendedMetrics};
use vike_analytics::walkforward::{WalkforwardResultsStep, WalkforwardWindowTimes};
use vike_analytics::{
    TRADING_RESULTS_TABLE_SCHEMA, TradingResultsTable, TradingResultsTableMetricValues,
};
use vike_backtesthub_client::msgpack_body::write_reply;
use vike_backtesthub_client::named_run::{
    NamedRunOutcome, NamedRunParam, NamedRunRefusal, NamedRunRoster, NamedRunSpec,
};
use vike_backtesthub_client::proto::{
    Request, Response, SearchSelectionWire, StudyRunRequestWire, WireStrategyFile,
};
use vike_backtesthub_client::sdk_source::{WireSdk, WireSdkEntry, WireSdkFile, WireSdkListing};
use vike_backtesthub_client::study::{
    StudyTierWire, WireListedStudy, WireRunArtifact, WireRunPage, WireRunRow, WireStudyListing,
};
use vike_backtesthub_client::summary::{
    CurvePageWire, ParamscanPointWire, ParamscanProfileSummaryWire, ParamscanRowSummaryWire,
    ParamscanRowsPageWire, ParamscanSummaryWire, ReturnsHistogramWire, RunSummaryWire,
    TradesPageWire, WalkforwardSummaryWire,
};
use vike_backtesthub_client::wire_studio::{
    CostModelStampWire, DataSliceWire, ParamscanEntryWire, ParamscanWire, ReplayKindWire,
    StrategyEngineConfigWire, StrategySpecWire, StudioParamscanWire, TradingRecordWire,
    WalkforwardResultsWire, WalkforwardWindowSearchWire, WalkforwardWire,
};
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::frame::{BodyCodec, decode_body, encode_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_model::params::ParamValue;
use vike_model::{OrderDrop, Trade};

const REQUEST_LINES: &str = "fixtures/datahub_wire/v7_backtest_plane_requests.jsonl";
const RESPONSE_LINES: &str = "fixtures/datahub_wire/v7_backtest_plane_responses.jsonl";

/// A sample SDK file's path, spelled in two literals: xtask's scan for another crate's `.rs` read as text
/// (`xtask/src/ci/tables/readers.rs`'s `SOURCES_READ_AS_TEXT`) would take a whole one for a read of the real
/// file, and the path-key gate (`crates/vike-ops/tests/hygiene/path_key_gate.rs`) for a key git must track.
const TACTICS_LIB: &str = concat!("crates", "/vike-tactics/src/lib.rs");

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn golden_lines(rel: &str) -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("{rel} could not be read ({e}): restore it from git"));
    text.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// Encode `samples` and hold them to `golden` (lines of `rel`), line for line and in both directions.
fn assert_frozen<T: Serialize + DeserializeOwned>(samples: &[T], golden: &[String], rel: &str) {
    let actual: Vec<String> =
        samples.iter().map(|s| serde_json::to_string(s).expect("the wire types encode")).collect();
    assert_eq!(
        actual,
        golden,
        "THE BACKTEST PLANE'S BYTES CHANGED: {rel} is frozen. Fix the code, not the file.\nTHIS BUILD \
         ENCODES:\n{}\n",
        actual.join("\n")
    );
    for line in golden {
        let decoded: T = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("a frozen frame no longer decodes ({e}): {line}"));
        assert_eq!(&serde_json::to_string(&decoded).expect("re-encode"), line);
    }
}

fn slice(kind: ReplayKindWire, start: Option<i64>, end: Option<i64>) -> Box<DataSliceWire> {
    Box::new(DataSliceWire {
        venue: "binance".to_string(),
        symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        interval: "1m".to_string(),
        start,
        end,
        kind,
    })
}

fn engine_params() -> StrategyEngineConfigWire {
    StrategyEngineConfigWire {
        cash: Some(5000.5),
        fee_rate: Some(0.001),
        slippage: Some(0.25),
        ..StrategyEngineConfigWire::default()
    }
}

fn grid() -> ParamscanWire {
    ParamscanWire {
        axes: vec![(
            "fast".to_string(),
            vec![ParamValue::Int(5), ParamValue::Float(7.5), ParamValue::Float(10.0)],
        )],
    }
}

/// The handshake's frames, the first lines of [`REQUEST_LINES`]: `vike_hub_protocol::handshake`'s types since this
/// protocol's copies were deleted (decision 0288's amendment of 2026-11-21), and the same bytes.
fn handshake_requests() -> Vec<HandshakeFrame> {
    vec![
        HandshakeFrame::Hello { proto_version: 7 },
        HandshakeFrame::Auth { scope: Scope::Read, mac: vec![1, 2, 3] },
        HandshakeFrame::Auth { scope: Scope::Write, mac: vec![] },
    ]
}

/// The replies to them, the first lines of [`RESPONSE_LINES`].
fn handshake_responses() -> Vec<HandshakeReply> {
    vec![
        HandshakeReply::Welcome {
            proto_version: 7,
            features: vec!["backtest".to_string(), "auth".to_string()],
            nonce: None,
        },
        HandshakeReply::Welcome { proto_version: 7, features: vec![], nonce: Some([7u8; 32]) },
        HandshakeReply::AuthOk { scope: Scope::Read, admitted: None },
        HandshakeReply::AuthOk { scope: Scope::Write, admitted: None },
        HandshakeReply::AuthDenied { reason: "bad mac".to_string() },
    ]
}

fn requests() -> Vec<Request> {
    let rhai = || StrategySpecWire::Rhai("fn on_bar() {}".to_string());
    vec![
        Request::Ping,
        Request::RunBacktest("[data]\nvenue = \"binance\"\n".to_string()),
        Request::RunSlice {
            spec: rhai(),
            slice: slice(ReplayKindWire::Bars, Some(1), Some(2)),
            params: None,
        },
        Request::RunSlice {
            spec: StrategySpecWire::Native {
                name: "buy_hold".to_string(),
                params_toml: "size = 1.0\n".to_string(),
            },
            slice: slice(ReplayKindWire::Ticks, None, None),
            params: Some(engine_params()),
        },
        Request::RunSlice {
            spec: StrategySpecWire::Plugin {
                name: "mine".to_string(),
                sha: "ab".repeat(32),
                params_toml: String::new(),
            },
            slice: slice(ReplayKindWire::Bars, None, Some(9)),
            params: Some(StrategyEngineConfigWire::default()),
        },
        Request::RunParamscan {
            spec: rhai(),
            slice: slice(ReplayKindWire::Bars, None, None),
            paramscan: grid(),
            params: None,
        },
        Request::RunParamscan {
            spec: rhai(),
            slice: slice(ReplayKindWire::Bars, None, None),
            paramscan: ParamscanWire::default(),
            params: Some(engine_params()),
        },
        Request::RunWalkforward {
            spec: rhai(),
            slice: slice(ReplayKindWire::Bars, None, None),
            walkforward: WalkforwardWire::fixed(4),
            params: None,
        },
        Request::RunWalkforward {
            spec: rhai(),
            slice: slice(ReplayKindWire::Bars, None, None),
            walkforward: WalkforwardWire::searching(
                4,
                WalkforwardWindowSearchWire {
                    method: "paramscan".to_string(),
                    grid: grid(),
                    rank_by: Some("return".to_string()),
                },
            ),
            params: Some(engine_params()),
        },
        Request::RunWalkforward {
            spec: rhai(),
            slice: slice(ReplayKindWire::Bars, None, None),
            walkforward: WalkforwardWire::searching(
                2,
                WalkforwardWindowSearchWire {
                    method: "none".to_string(),
                    grid: ParamscanWire::default(),
                    rank_by: None,
                },
            ),
            params: None,
        },
        Request::RunParamscanProfile {
            profile_toml: "[paramscan]\nfast = [5, 10]\n".to_string(),
            rank_by: None,
            search: None,
        },
        Request::RunParamscanProfile {
            profile_toml: "[paramscan]\nfast = [5, 10]\n".to_string(),
            rank_by: Some("multi".to_string()),
            search: Some(SearchSelectionWire {
                optimizer: Some("tpe".to_string()),
                euler_depth: Some("2".to_string()),
                trials: Some("128".to_string()),
                seed: Some("7".to_string()),
            }),
        },
        Request::RunWalkforwardProfile {
            profile_toml: "[walkforward]\nn_splits = 3\n".to_string(),
        },
        Request::RunStudy(Box::new(StudyRunRequestWire {
            study: "cohort".to_string(),
            recipe_toml: "[learner]\nnum_iterations = 400\n".to_string(),
            from: "2026-04-07T05".to_string(),
            to: "1785906000".to_string(),
            ..StudyRunRequestWire::default()
        })),
        Request::ListStrategies,
        Request::NamedStrategies,
        Request::RunNamed(Box::new(NamedRunSpec {
            strategy: "buy_hold".to_string(),
            params: vec![
                ("rungs".to_string(), NamedRunParam::Int(4)),
                ("size".to_string(), NamedRunParam::Num(2.5)),
                ("on".to_string(), NamedRunParam::Flag(true)),
            ],
            venue: "binance".to_string(),
            symbol: "BTCUSDT".to_string(),
            interval: "1h".to_string(),
            start: 0,
            end: 3_600_000,
        })),
        // Added 2026-11-10: the slippage model (`FEATURE_SLIPPAGE_MODEL`), a new line, no edit.
        Request::RunSlice {
            spec: rhai(),
            slice: slice(ReplayKindWire::Bars, None, None),
            params: Some(StrategyEngineConfigWire {
                slippage: Some(2.0),
                slippage_model: Some("ticks".to_string()),
                slippage_tick: Some(0.01),
                ..StrategyEngineConfigWire::default()
            }),
        },
        // Added 2026-11-13: a strategy FILE of the server's folder (plan 2b), a new line, no edit.
        Request::RunSlice {
            spec: StrategySpecWire::File {
                file: "trend/buy_hold.rs".to_string(),
                params_toml: "size = 1.0\n".to_string(),
                sdk: None,
            },
            slice: slice(ReplayKindWire::Bars, None, None),
            params: None,
        },
        // Added 2026-11-13: the strategy-file list and read (`FEATURE_STRATEGY_FILES`), new lines.
        Request::ListStrategyFiles,
        Request::ReadStrategyFile { file: "trend/momentum.rs".to_string() },
        Request::SaveStrategyFile {
            file: "trend/momentum.rs".to_string(),
            text: "//! Momentum.\npub fn build() {}\n".to_string(),
        },
        // Added 2026-11-16: the Studio's Build through this daemon (`FEATURE_BUILD_STRATEGY_FILE`), a new line.
        Request::BuildStrategyFile {
            file: "trend/momentum.rs".to_string(),
            text: "//! Momentum.\npub fn build() {}\n".to_string(),
            sdk: None,
        },
        // Added 2026-11-16: the summarized Studio runs (`FEATURE_RUN_SUMMARY`), new lines.
        Request::Summarized(Box::new(Request::RunSlice {
            spec: rhai(),
            slice: slice(ReplayKindWire::Bars, None, None),
            params: None,
        })),
        Request::ReadTrades { run: "18f2a3-1".to_string(), point: None, page: 2, page_size: 200 },
        Request::ReadTrades {
            run: "18f2a3-1".to_string(),
            point: Some(3),
            page: 0,
            page_size: 200,
        },
        Request::ReadPoint { run: "18f2a3-1".to_string(), point: 3 },
        // Added 2026-11-18: the whole curve on request (`FEATURE_RUN_CURVE`), new lines.
        Request::Summarized(Box::new(Request::RunBacktest("name = 'sma'".to_string()))),
        Request::ReadCurve { run: "18f2a3-1".to_string(), page: 1, page_size: 100_000 },
        Request::ReadCurve { run: "1763424000-18f2a3-0".to_string(), page: 0, page_size: 500_000 },
        // Added 2026-11-19: a study client that reads a result the server could not save
        // (`report_unsaved_result`), a new line.
        Request::RunStudy(Box::new(StudyRunRequestWire {
            study: "cohort".to_string(),
            recipe_toml: "[learner]\nnum_iterations = 400\n".to_string(),
            from: "2026-04-07T05".to_string(),
            to: "1785906000".to_string(),
            report_unsaved_result: true,
            ..StudyRunRequestWire::default()
        })),
        // Added 2026-11-19: the server's studies and saved runs (`FEATURE_STUDY_FILES`,
        // `FEATURE_RUN_FILES`), new lines.
        Request::RunStudy(Box::new(StudyRunRequestWire {
            study: "fixture_bars".to_string(),
            recipe_toml: String::new(),
            from: String::new(),
            to: "1785906000".to_string(),
            report_unsaved_result: true,
            tier: Some(StudyTierWire::Rhai),
            recipe: Some("baseline.toml".to_string()),
        })),
        Request::ListStudies,
        Request::ListRuns { page: 1, page_size: 200 },
        Request::ReadRunArtifact {
            run_id: "1763424000-18f2a3-0".to_string(),
            file: "fold_scores.csv".to_string(),
            offset: 65_536,
            length: 0,
        },
        // Added 2026-11-19: the SDK a Studio run and Build name (`FEATURE_STRATEGY_SDK`), new lines.
        Request::RunSlice {
            spec: StrategySpecWire::File {
                file: "trend/buy_hold.rs".to_string(),
                params_toml: "size = 1.0\n".to_string(),
                sdk: Some(4),
            },
            slice: slice(ReplayKindWire::Bars, None, None),
            params: None,
        },
        Request::BuildStrategyFile {
            file: "trend/momentum.rs".to_string(),
            text: "//! Momentum.\npub fn build() {}\n".to_string(),
            sdk: Some(4),
        },
        // Added 2026-11-19: the versioned strategy SDK's source (`FEATURE_SDK_SOURCE`, decision 0296),
        // new lines.
        Request::ListSdks,
        Request::ReadSdkFile { sdk: "sdk-3".to_string(), path: TACTICS_LIB.to_string() },
        // Added 2026-11-19: a profile's summarized search (`FEATURE_PARAMSCAN_PROFILE_SUMMARY`), new lines.
        Request::Summarized(Box::new(Request::RunParamscanProfile {
            profile_toml: "[paramscan]\nfast = [5, 10]\n".to_string(),
            rank_by: Some("return".to_string()),
            search: None,
        })),
        Request::ReadParamscanRows { run: "18f2a3-1".to_string(), page: 1, page_size: 1000 },
        // Added 2026-11-20: a run asked fresh (`FEATURE_RUN_FROM_SAVED`, decision 0078's amendment of that day), a new line.
        Request::Fresh(Box::new(Request::Summarized(Box::new(Request::RunBacktest(
            "[data]\n".to_string(),
        ))))),
        // Added 2026-11-20: a Studio run's funding switch (`FEATURE_ATTACH_FUNDING`) and a Rhai buffer's parameters
        // (`FEATURE_RHAI_PARAMETERS`), new lines.
        Request::RunSlice {
            spec: StrategySpecWire::RhaiWithParameters {
                source: "fn on_bar() {}".to_string(),
                parameters_toml: "fast = 7\ncommission = 0.1\n".to_string(),
            },
            slice: slice(ReplayKindWire::Bars, None, None),
            params: Some(StrategyEngineConfigWire {
                cash: Some(5000.5),
                attach_funding: Some(false),
                ..StrategyEngineConfigWire::default()
            }),
        },
    ]
}

fn run_result() -> TradingRecordWire {
    TradingRecordWire {
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        equity_curve: vec![1000.0, 1010.25],
        equity_timestamp: vec![1, 2],
        final_equity: 1010.25,
        n_trades: 1,
        per_symbol_pnl: vec![("BTCUSDT".to_string(), 10.25)],
        trades: vec![Trade {
            entry_price: 100.0,
            exit_price: 110.5,
            size: 1.0,
            pnl: 10.5,
            fees: 0.25,
            entry_timestamp: 1,
            exit_timestamp: 2,
            symbol: "BTCUSDT".to_string(),
            mae: 0.01,
            mfe: 0.02,
            is_long: true,
        }],
        stale_deferrals: 3,
        session_deferrals: 4,
        cost_model: Some(cost_model()),
        data_warnings: Vec::new(),
        funding_paid: 0.0,
        settle_price: None,
        refused_orders: Vec::new(),
    }
}

fn cost_model() -> CostModelStampWire {
    CostModelStampWire {
        source: "derived".to_string(),
        lane: Some("binance-perp".to_string()),
        variant: "PercentMakerTaker".to_string(),
        maker_rate: 0.0002,
        taker_rate: 0.0005,
        reason: Some("why".to_string()),
        maker_fills: 5,
        taker_fills: 6,
        fees_paid: -1.5,
        rank_metric: Some("sharpe".to_string()),
        not_modelled: vec!["funding".to_string()],
    }
}

/// A step's or a walk's results, built by hand so its bytes are the wire type's alone (no metric
/// function runs): the required scalars, an infinite profit factor (written `null`), and every
/// optional block absent.
fn results(final_equity: f64, total_return: f64, sharpe: f64) -> TradingResults {
    TradingResults {
        name: None,
        final_equity,
        total_return,
        n_trades: 1,
        win_rate: 1.0,
        sharpe,
        max_drawdown: 0.0,
        profit_factor: f64::INFINITY,
        funding_paid: 0.0,
        per_symbol_pnl: Vec::new(),
        zero_trade: None,
        invalid: None,
        extended: None,
        honesty: None,
        realism: None,
        data_warnings: Vec::new(),
        in_dollars: None,
        closed_trades: None,
        strategy_file: String::new(),
        strategy_sha256: String::new(),
        strategy_sdk: None,
        returns_interval_ms: None,
        mounts: None,
        strategy_failures: Default::default(),
        source: None,
        period: None,
    }
}

/// The two steps of a searched walk: a fixed one and one that chose `fast = 8`.
fn walkforward_steps() -> Vec<WalkforwardResultsStep> {
    vec![
        WalkforwardResultsStep {
            test_range: (0, 10),
            window_times: None,
            chosen_params: None,
            results: results(1050.5, 0.0505, 1.25),
        },
        WalkforwardResultsStep {
            test_range: (10, 20),
            window_times: None,
            chosen_params: Some(vec![("fast".to_string(), ParamValue::Int(8))]),
            results: results(500.0, -0.5, -2.0),
        },
    ]
}

/// [`walkforward_steps`] with each window's times (2026-11-20): the first window's train half from 0, the second a
/// combinatorial path's, with no train half.
fn timed_walkforward_steps() -> Vec<WalkforwardResultsStep> {
    let mut steps = walkforward_steps();
    steps[0].window_times = Some(WalkforwardWindowTimes {
        train_start: Some(0),
        train_end: Some(540_000),
        test_start: 600_000,
        test_end: 1_140_000,
    });
    steps[1].window_times = Some(WalkforwardWindowTimes {
        train_start: None,
        train_end: None,
        test_start: 1_200_000,
        test_end: 1_740_000,
    });
    steps
}

/// A walk's overall results with every block the walk fills: its data warnings, its funding, its
/// strategy file, and the extended metrics with an infinite `calmar` (written `null`, read back).
fn walkforward_overall() -> TradingResults {
    TradingResults {
        funding_paid: -0.25,
        extended: Some(TradingResultsExtendedMetrics {
            calmar: f64::INFINITY,
            ..TradingResultsExtendedMetrics::default()
        }),
        data_warnings: vec!["BTCUSDT.P: no funding history".to_string()],
        strategy_file: "trend/momentum.rs".to_string(),
        strategy_sha256: "0123456789abcdef".repeat(4),
        ..results(1050.5, 0.0505, 1.5)
    }
}

fn responses() -> Vec<Response> {
    vec![
        Response::Pong,
        Response::Report("{\"name\":\"a\"}".to_string()),
        Response::RunResult(run_result()),
        Response::RunResult(TradingRecordWire::default()),
        Response::RunResult(TradingRecordWire {
            funding_paid: -0.5,
            ..TradingRecordWire::default()
        }),
        // Added 2026-11-11: a coin ledger's settle price (A14 (3)), a new line, no edit.
        Response::RunResult(TradingRecordWire {
            settle_price: Some(66_000.0),
            ..TradingRecordWire::default()
        }),
        // Added 2026-11-12: the orders a run refused, grouped (owner ruling, part C), a new line,
        // no edit: the two frames above with no refusal keep their bytes.
        Response::RunResult(TradingRecordWire {
            refused_orders: vec![(
                OrderDrop {
                    symbol: "BTCUSDT".to_string(),
                    reason:
                        "a spot account cannot sell more than it holds: it has 1, this sells 2; \
                             nothing was sent"
                            .to_string(),
                    size: 2.0,
                    weight: 1.0,
                },
                3,
            )],
            ..TradingRecordWire::default()
        }),
        Response::ParamscanResult(StudioParamscanWire {
            entries: vec![ParamscanEntryWire {
                overrides: vec![
                    ("fast".to_string(), ParamValue::Int(5)),
                    ("slow".to_string(), ParamValue::Float(10.5)),
                ],
                result: TradingRecordWire::default(),
            }],
            dsr: Some(0.5),
            pbo: None,
            best_index: 0,
            cost_model: Some(cost_model()),
        }),
        Response::ParamscanResult(StudioParamscanWire {
            entries: vec![],
            dsr: None,
            pbo: Some(0.25),
            best_index: 0,
            cost_model: None,
        }),
        // RE-CAPTURED with backtesthub protocol 9 (package 2 step 4, the walk-forward results
        // through the shared `TradingResults`): the one sanctioned edit of this file's walk-forward
        // lines, where five lines of `windows` and four numbers stood. The walk's data warnings,
        // funding and strategy file now ride its `overall`, so one line holds what three held.
        Response::WalkforwardResult(Box::new(WalkforwardResultsWire {
            steps: walkforward_steps(),
            overall: walkforward_overall(),
            equity_curve: vec![1000.0, 1050.5],
            equity_timestamp: Vec::new(),
            profitable_step_share: 0.5,
            account_currency: None,
            cost_model: Some(cost_model()),
        })),
        Response::WalkforwardResult(Box::new(WalkforwardResultsWire {
            steps: vec![],
            overall: results(0.0, 0.0, 0.0),
            equity_curve: vec![],
            equity_timestamp: Vec::new(),
            profitable_step_share: 0.0,
            account_currency: None,
            cost_model: None,
        })),
        Response::WalkforwardResult(Box::new(WalkforwardResultsWire {
            steps: vec![],
            overall: results(0.0, 0.0, 0.0),
            equity_curve: vec![],
            equity_timestamp: Vec::new(),
            profitable_step_share: 0.0,
            account_currency: Some("JPY".to_string()),
            cost_model: None,
        })),
        Response::ParamscanReport("{\"rows\":[]}".to_string()),
        Response::WalkforwardReport("{\"windows\":[]}".to_string()),
        Response::StudyReport("{\"run\":\"r\"}".to_string()),
        Response::Error("compile: nope".to_string()),
        Response::Strategies(vec!["buy_hold".to_string(), "grid".to_string()]),
        Response::NamedStrategies(NamedRunRoster {
            armed: true,
            strategies: vec!["buy_hold".to_string()],
        }),
        Response::NamedStrategies(NamedRunRoster { armed: false, strategies: vec![] }),
        Response::NamedRun(Box::new(NamedRunOutcome::Ran {
            result: Box::new(run_result()),
            report_json: "{\"name\":\"a\"}".to_string(),
        })),
        Response::NamedRun(Box::new(NamedRunOutcome::NotArmed)),
        Response::NamedRun(Box::new(NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy {
            known: vec!["buy_hold".to_string()],
        }))),
        Response::NamedRun(Box::new(NamedRunOutcome::Refused(NamedRunRefusal::NoSlot {
            limit: 4,
        }))),
        // Added 2026-11-13: the strategy-file list and read (`FEATURE_STRATEGY_FILES`), new lines.
        Response::StrategyFiles(vec![
            WireStrategyFile {
                file: "trend/momentum.rs".to_string(),
                build: "built 0123abcd".to_string(),
                broken: String::new(),
                edited: true,
            },
            WireStrategyFile {
                file: "other/note.rhai".to_string(),
                build: String::new(),
                broken: "no header: the file has no ```toml block".to_string(),
                edited: false,
            },
        ]),
        Response::StrategyFileText {
            file: "trend/momentum.rs".to_string(),
            text: "//! Momentum.\npub fn build() {}\n".to_string(),
        },
        Response::StrategyFileSaved {
            sha256: "0123456789abcdef".repeat(4),
            file: WireStrategyFile {
                file: "trend/momentum.rs".to_string(),
                build: "not built".to_string(),
                broken: String::new(),
                edited: false,
            },
        },
        // Added 2026-11-16: the Studio's Build through this daemon (`FEATURE_BUILD_STRATEGY_FILE`), a new line.
        Response::StrategyFileBuilt { sha256: "0123456789abcdef".repeat(4) },
        // Added 2026-11-16: the summarized Studio runs (`FEATURE_RUN_SUMMARY`), new lines. The first
        // and the scan's `best` RE-CAPTURED with backtesthub protocol 9 for the summary's
        // `source_label` (the one label of the results); the empty one keeps its bytes.
        Response::RunSummary(Box::new(run_summary())),
        Response::RunSummary(Box::default()),
        Response::ParamscanSummary(Box::new(ParamscanSummaryWire {
            run: "18f2a3-1".to_string(),
            points: vec![ParamscanPointWire {
                overrides: vec![
                    ("fast".to_string(), ParamValue::Int(5)),
                    ("slow".to_string(), ParamValue::Float(10.5)),
                ],
                sharpe: Some(1.25),
                total_return: None,
                final_equity: 1010.25,
                n_trades: 1,
                max_drawdown: None,
                invalid: None,
            }],
            best: Some(RunSummaryWire { point: Some(0), ..run_summary() }),
            dsr: Some(0.5),
            pbo: None,
            best_index: 0,
            cost_model: Some(cost_model()),
            answered_from_saved_run: String::new(),
        })),
        // RE-CAPTURED with backtesthub protocol 9, as the `WalkforwardResult` lines above.
        Response::WalkforwardSummary(Box::new(WalkforwardSummaryWire {
            steps: walkforward_steps()[..1].to_vec(),
            overall: walkforward_overall(),
            equity_curve: vec![(0, 1000.0), (9, 1050.5)],
            equity_curve_timestamp: Vec::new(),
            equity_curve_len: 10,
            profitable_step_share: 0.5,
            cost_model: None,
            answered_from_saved_run: String::new(),
        })),
        Response::TradesPage(TradesPageWire {
            total: 401,
            page: 2,
            page_size: 200,
            trades: run_result().trades,
        }),
        // Added 2026-11-16: the combinatorial walk's paths and the permutation test (book C1), new lines.
        Response::CombinatorialWalkforwardReport("{\"paths\":[]}".to_string()),
        Response::PermutationReport("{\"p_value\":1.0}".to_string()),
        // Added 2026-11-18: the whole curve on request (`FEATURE_RUN_CURVE`), new lines.
        Response::CurvePage(CurvePageWire {
            total: 3,
            page: 1,
            page_size: 2,
            equity: vec![1010.25],
            timestamp: vec![120_000],
        }),
        Response::CurvePage(CurvePageWire {
            total: 2,
            page: 0,
            page_size: 100_000,
            equity: vec![1000.0, 1010.25],
            timestamp: Vec::new(),
        }),
        Response::RunSummary(Box::new(RunSummaryWire {
            saved_run: "1763424000-18f2a3-0".to_string(),
            ..run_summary()
        })),
        // Added 2026-11-19: the server's studies and saved runs (`FEATURE_STUDY_FILES`,
        // `FEATURE_RUN_FILES`), new lines.
        Response::Studies(WireStudyListing {
            root: "/srv/vike/user_data/research/studies".to_string(),
            studies: vec![
                WireListedStudy {
                    name: "fixture_bars".to_string(),
                    tier: StudyTierWire::Rhai,
                    recipes: vec!["baseline.toml".to_string()],
                },
                WireListedStudy {
                    name: "cohort".to_string(),
                    tier: StudyTierWire::Rust,
                    recipes: Vec::new(),
                },
            ],
            diagnostics: vec!["rhai/stray.rhai: a study is a FOLDER".to_string()],
        }),
        Response::Runs(WireRunPage {
            root: "/srv/vike/user_data/runs".to_string(),
            total: 3,
            page: 0,
            page_size: 200,
            runs: vec![WireRunRow {
                run_id: "1763424000-18f2a3-0".to_string(),
                kind: "study".to_string(),
                produced_by: "vike-backend backtesthub".to_string(),
                started_at: "2025-11-18T00:00:00Z".to_string(),
                has_report: true,
            }],
            diagnostics: vec!["1763424001-1: unfinished".to_string()],
            diagnostic_total: 1,
        }),
        Response::RunArtifact(WireRunArtifact {
            run_id: "1763424000-18f2a3-0".to_string(),
            file: "fold_scores.csv".to_string(),
            offset: 0,
            total: 11,
            data_base64: "Zm9sZCxzY29yZQo=".to_string(),
        }),
        // Added 2026-11-19: the versioned strategy SDK's source (`FEATURE_SDK_SOURCE`, decision 0296),
        // new lines.
        Response::Sdks(WireSdkListing {
            root: "/srv/vike/vendor/vike-strategy-source".to_string(),
            sdks: vec![
                WireSdk {
                    name: "sdk-3".to_string(),
                    number: Some(3),
                    commit: "a3ca741107".to_string(),
                    abi_version: Some(14),
                    usable: true,
                    why_unusable: String::new(),
                },
                WireSdk {
                    name: "sdk-1".to_string(),
                    number: Some(1),
                    commit: String::new(),
                    abi_version: None,
                    usable: false,
                    why_unusable: "its marker cannot be read".to_string(),
                },
            ],
        }),
        Response::SdkFile(WireSdkFile {
            sdk: "sdk-3".to_string(),
            path: TACTICS_LIB.to_string(),
            folder: false,
            text: "//! Tactics.\n".to_string(),
            entries: Vec::new(),
        }),
        Response::SdkFile(WireSdkFile {
            sdk: "sdk-dev".to_string(),
            path: "crates".to_string(),
            folder: true,
            text: String::new(),
            entries: vec![
                WireSdkEntry { name: "vike-tactics".to_string(), folder: true },
                WireSdkEntry { name: "README.md".to_string(), folder: false },
            ],
        }),
        // Added 2026-11-19: a profile's summarized search (`FEATURE_PARAMSCAN_PROFILE_SUMMARY`), new lines: a point
        // that ran (its metric table, three numbers recorded) and one that failed, then a page.
        Response::ParamscanProfileSummary(Box::new(ParamscanProfileSummaryWire {
            run: "18f2a3-1".to_string(),
            saved_run: "1763424000-18f2a3-0".to_string(),
            total: 2,
            page_size: 1000,
            rows: vec![ran_row(), failed_row()],
            rank_by: "total_return".to_string(),
            summary: None,
            account_currency: None,
            strategy_file: "trend/momentum.rs".to_string(),
            strategy_sha256: String::new(),
            strategy_sdk: None,
            data_warnings: vec!["BTCUSDT: a gap".to_string()],
            strategy_failures: Default::default(),
            answered_from_saved_run: String::new(),
        })),
        Response::ParamscanRows(ParamscanRowsPageWire {
            total: 2,
            page: 0,
            page_size: 2,
            rows: vec![ran_row(), ran_row()],
        }),
        // A search of a file built against SDK 1: the summary states the SDK the whole report states.
        Response::ParamscanProfileSummary(Box::new(ParamscanProfileSummaryWire {
            run: "18f2a3-2".to_string(),
            saved_run: String::new(),
            total: 0,
            page_size: 1000,
            rows: Vec::new(),
            rank_by: "sharpe".to_string(),
            summary: None,
            account_currency: None,
            strategy_file: "trend/momentum.rs".to_string(),
            strategy_sha256: String::new(),
            strategy_sdk: Some(1),
            data_warnings: Vec::new(),
            strategy_failures: Default::default(),
            answered_from_saved_run: String::new(),
        })),
        // Added 2026-11-20: a summary answered from the run the backtesthub saved (`FEATURE_RUN_FROM_SAVED`), a new
        // line.
        Response::RunSummary(Box::new(RunSummaryWire {
            saved_run: "1763424000-18f2a3-0".to_string(),
            answered_from_saved_run: "1763424000-18f2a3-0".to_string(),
            ..run_summary()
        })),
        // Added 2026-11-20: the summaries' curve times, a scan point's drawdown and a walk's window times (reply
        // fields, decision 0112 verdict 3: no word), new lines.
        Response::RunSummary(Box::new(RunSummaryWire {
            curve_timestamp: vec![60_000, 120_000],
            ..run_summary()
        })),
        Response::ParamscanSummary(Box::new(ParamscanSummaryWire {
            run: "18f2a3-1".to_string(),
            points: vec![ParamscanPointWire {
                overrides: vec![("fast".to_string(), ParamValue::Int(5))],
                sharpe: Some(1.25),
                total_return: Some(0.5),
                final_equity: 1010.25,
                n_trades: 1,
                max_drawdown: Some(0.125),
                invalid: None,
            }],
            best: None,
            dsr: None,
            pbo: None,
            best_index: 0,
            cost_model: None,
            answered_from_saved_run: String::new(),
        })),
        Response::WalkforwardSummary(Box::new(WalkforwardSummaryWire {
            steps: timed_walkforward_steps()[..1].to_vec(),
            overall: walkforward_overall(),
            equity_curve: vec![(0, 1000.0), (9, 1050.5)],
            equity_curve_timestamp: vec![600_000, 1_140_000],
            equity_curve_len: 10,
            profitable_step_share: 0.5,
            cost_model: None,
            answered_from_saved_run: String::new(),
        })),
        Response::WalkforwardResult(Box::new(WalkforwardResultsWire {
            steps: timed_walkforward_steps(),
            overall: walkforward_overall(),
            equity_curve: vec![1000.0, 1050.5],
            // In memory only: the whole answer carries no stitched times.
            equity_timestamp: Vec::new(),
            profitable_step_share: 0.5,
            account_currency: None,
            cost_model: Some(cost_model()),
        })),
    ]
}

/// A summarized search's point that ran: its metric table with three numbers recorded and the rest not.
fn ran_row() -> ParamscanRowSummaryWire {
    let mut metrics = TradingResultsTableMetricValues::unrecorded();
    for (id, value) in [("final_equity", 1010.25), ("n_trades", 3.0), ("sharpe", 1.25)] {
        assert!(metrics.set(id, Some(value)), "{id} is a catalog id");
    }
    ParamscanRowSummaryWire {
        overrides: vec![("fast".to_string(), ParamValue::Int(5))],
        report: Some(TradingResultsTable {
            schema: TRADING_RESULTS_TABLE_SCHEMA,
            name: None,
            invalid: None,
            metrics,
        }),
        error: None,
        score: None,
    }
}

/// A summarized search's point that failed, ranked by an objective.
fn failed_row() -> ParamscanRowSummaryWire {
    ParamscanRowSummaryWire {
        overrides: vec![("fast".to_string(), ParamValue::Int(10))],
        report: None,
        error: Some("strategy: no bars".to_string()),
        score: Some(-0.5),
    }
}

fn run_summary() -> RunSummaryWire {
    RunSummaryWire {
        run: "18f2a3-1".to_string(),
        saved_run: String::new(),
        point: None,
        rows: vec![("final_equity".to_string(), "1010.25".to_string())],
        invalid: None,
        sharpe: Some(1.25),
        total_return: Some(0.010_25),
        final_equity: 1010.25,
        n_trades: 1,
        probabilistic_sharpe: None,
        curve: vec![(0, 1000.0), (1, 1010.25)],
        // An older server's summary: no times beside its curve.
        curve_timestamp: Vec::new(),
        curve_len: 2,
        returns_histogram: ReturnsHistogramWire { low: 0.010_25, width: 1e-9, counts: vec![1, 0] },
        refused: vec![("cash gate".to_string(), 3)],
        // An older server's summary: no detail beside its refusals.
        refused_detail: Vec::new(),
        data_warnings: vec!["BTCUSDT: a gap".to_string()],
        cost_model: Some(cost_model()),
        strategy_file: "trend/momentum.rs".to_string(),
        strategy_sha256: "0123456789abcdef".repeat(4),
        source_label: Some(
            "backtest trend/momentum.rs 1970-01-01 00:00 – 1970-01-01 00:00 UTC".to_string(),
        ),
        // An older server's summary: no results table, so the pinned bytes are the ones it sent.
        results: None,
        answered_from_saved_run: String::new(),
    }
}

/// `rel`'s lines, split after the handshake's `frames` first ones: (the handshake's, the verbs').
fn split_golden(rel: &str, frames: usize) -> (Vec<String>, Vec<String>) {
    let mut handshake = golden_lines(rel);
    let verbs = handshake.split_off(frames);
    (handshake, verbs)
}

#[test]
fn every_backtest_plane_request_keeps_its_bytes() {
    let (handshake, verbs) = split_golden(REQUEST_LINES, handshake_requests().len());
    assert_frozen(&handshake_requests(), &handshake, REQUEST_LINES);
    assert_frozen(&requests(), &verbs, REQUEST_LINES);
}

#[test]
fn every_backtest_plane_response_keeps_its_bytes() {
    let (handshake, verbs) = split_golden(RESPONSE_LINES, handshake_responses().len());
    assert_frozen(&handshake_responses(), &handshake, RESPONSE_LINES);
    assert_frozen(&responses(), &verbs, RESPONSE_LINES);
}

/// **The msgpack body** ("JSON only at the edges", 2026-11-19): every pinned message crosses msgpack, plain and
/// LZ4 (a response as the backtesthub writes it, `write_reply`: a report as its tree under LZ4), and reads back the very value. Its JSON
/// line is the pinned one, so nothing a JSON reader sees moved, and its `Debug` holds every float to its shortest
/// round trip.
#[test]
fn every_backtest_plane_message_round_trips_through_msgpack() {
    let (_, request_lines) = split_golden(REQUEST_LINES, handshake_requests().len());
    let (_, response_lines) = split_golden(RESPONSE_LINES, handshake_responses().len());
    for codec in [BodyCodec::Msgpack, BodyCodec::MsgpackLz4] {
        for (request, line) in requests().iter().zip(request_lines.iter().cloned()) {
            let back: Request = decode_body(&encode_frame(codec, request).expect("encodes")[4..])
                .unwrap_or_else(|e| panic!("{codec:?} {line}: {e}"));
            assert_eq!(serde_json::to_string(&back).expect("re-encode"), line, "{codec:?}");
            assert_eq!(format!("{back:?}"), format!("{request:?}"), "{codec:?}");
        }
        for (response, line) in responses().iter().zip(response_lines.iter().cloned()) {
            let mut frame = Vec::new();
            write_reply(&mut frame, codec, response).expect("encodes");
            let back: Response =
                decode_body(&frame[4..]).unwrap_or_else(|e| panic!("{codec:?} {line}: {e}"));
            assert_eq!(serde_json::to_string(&back).expect("re-encode"), line, "{codec:?}");
            assert_eq!(format!("{back:?}"), format!("{response:?}"), "{codec:?}");
        }
    }
}
