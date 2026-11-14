//! **The backtest plane's bytes, frozen before its protocol left this crate.** Every `Request` and
//! `Response` variant the backtesthub speaks, built with every optional field both present and absent,
//! encodes to EXACTLY the line committed for it, and that line decodes and re-encodes to itself.
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
use vike_analytics::result::WalkforwardWindow;
use vike_backtesthub_client::named_run::{
    NamedRunOutcome, NamedRunParam, NamedRunRefusal, NamedRunRoster, NamedRunSpec,
};
use vike_backtesthub_client::proto::{
    Request, Response, SearchSelectionWire, StudyRunRequestWire, WireStrategyFile,
};
use vike_backtesthub_client::wire_studio::{
    BacktestResultWire, CostModelStampWire, DataSliceWire, ParamscanEntryWire, ParamscanWire,
    ReplayKindWire, StrategyEngineConfigWire, StrategySpecWire, StudioParamscanWire,
    WalkforwardReportWire, WalkforwardWindowSearchWire, WalkforwardWire,
};
use vike_hub_protocol::auth::Scope;
use vike_model::params::ParamValue;
use vike_model::{OrderDrop, Trade};

const REQUEST_LINES: &str = "fixtures/datahub_wire/v7_backtest_plane_requests.jsonl";
const RESPONSE_LINES: &str = "fixtures/datahub_wire/v7_backtest_plane_responses.jsonl";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn golden_lines(rel: &str) -> Vec<String> {
    let text = std::fs::read_to_string(repo_root().join(rel))
        .unwrap_or_else(|e| panic!("{rel} could not be read ({e}): restore it from git"));
    text.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// Encode `samples` and hold them to `rel`, line for line and in both directions.
fn assert_frozen<T: Serialize + DeserializeOwned>(samples: &[T], rel: &str) {
    let actual: Vec<String> =
        samples.iter().map(|s| serde_json::to_string(s).expect("the wire types encode")).collect();
    let golden = golden_lines(rel);
    assert_eq!(
        actual,
        golden,
        "THE BACKTEST PLANE'S BYTES CHANGED: {rel} is frozen. Fix the code, not the file.\nTHIS BUILD \
         ENCODES:\n{}\n",
        actual.join("\n")
    );
    for line in &golden {
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

fn requests() -> Vec<Request> {
    let rhai = || StrategySpecWire::Rhai("fn on_bar() {}".to_string());
    vec![
        Request::Hello { proto_version: 7 },
        Request::Auth { scope: Scope::Read, mac: vec![1, 2, 3] },
        Request::Auth { scope: Scope::Write, mac: vec![] },
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
    ]
}

fn run_result() -> BacktestResultWire {
    BacktestResultWire {
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

fn responses() -> Vec<Response> {
    vec![
        Response::Welcome {
            proto_version: 7,
            features: vec!["backtest".to_string(), "auth".to_string()],
            nonce: None,
        },
        Response::Welcome { proto_version: 7, features: vec![], nonce: Some([7u8; 32]) },
        Response::AuthOk { scope: Scope::Read },
        Response::AuthOk { scope: Scope::Write },
        Response::AuthDenied { reason: "bad mac".to_string() },
        Response::Pong,
        Response::Report("{\"name\":\"a\"}".to_string()),
        Response::RunResult(run_result()),
        Response::RunResult(BacktestResultWire::default()),
        Response::RunResult(BacktestResultWire {
            funding_paid: -0.5,
            ..BacktestResultWire::default()
        }),
        // Added 2026-11-11: a coin ledger's settle price (A14 (3)), a new line, no edit.
        Response::RunResult(BacktestResultWire {
            settle_price: Some(66_000.0),
            ..BacktestResultWire::default()
        }),
        // Added 2026-11-12: the orders a run refused, grouped (owner ruling, part C), a new line,
        // no edit: the two frames above with no refusal keep their bytes.
        Response::RunResult(BacktestResultWire {
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
            ..BacktestResultWire::default()
        }),
        Response::ParamscanResult(StudioParamscanWire {
            entries: vec![ParamscanEntryWire {
                overrides: vec![
                    ("fast".to_string(), ParamValue::Int(5)),
                    ("slow".to_string(), ParamValue::Float(10.5)),
                ],
                result: BacktestResultWire::default(),
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
        Response::WalkforwardResult(WalkforwardReportWire {
            strategy_file: String::new(),
            strategy_sha256: String::new(),
            windows: vec![
                WalkforwardWindow {
                    test_range: (0, 10),
                    out_of_sample_return: 0.05,
                    chosen_params: None,
                },
                WalkforwardWindow {
                    test_range: (10, 20),
                    out_of_sample_return: -0.5,
                    chosen_params: Some(vec![("fast".to_string(), ParamValue::Int(8))]),
                },
            ],
            out_of_sample_equity_curve: vec![1000.0, 1050.5],
            out_of_sample_return: 0.05,
            out_of_sample_sharpe: 1.5,
            walkforward_consistency: 0.5,
            cost_model: Some(cost_model()),
            data_warnings: Vec::new(),
            funding_paid: 0.0,
            account_currency: None,
        }),
        Response::WalkforwardResult(WalkforwardReportWire {
            strategy_file: String::new(),
            strategy_sha256: String::new(),
            windows: vec![],
            out_of_sample_equity_curve: vec![],
            out_of_sample_return: 0.0,
            out_of_sample_sharpe: 0.0,
            walkforward_consistency: 0.0,
            cost_model: None,
            data_warnings: Vec::new(),
            funding_paid: 0.0,
            account_currency: None,
        }),
        Response::WalkforwardResult(WalkforwardReportWire {
            strategy_file: String::new(),
            strategy_sha256: String::new(),
            windows: vec![],
            out_of_sample_equity_curve: vec![],
            out_of_sample_return: 0.0,
            out_of_sample_sharpe: 0.0,
            walkforward_consistency: 0.0,
            cost_model: None,
            data_warnings: vec!["BTCUSDT.P: no funding history".to_string()],
            funding_paid: 0.0,
            account_currency: None,
        }),
        Response::WalkforwardResult(WalkforwardReportWire {
            strategy_file: String::new(),
            strategy_sha256: String::new(),
            windows: vec![],
            out_of_sample_equity_curve: vec![],
            out_of_sample_return: 0.0,
            out_of_sample_sharpe: 0.0,
            walkforward_consistency: 0.0,
            cost_model: None,
            data_warnings: Vec::new(),
            funding_paid: -0.25,
            account_currency: None,
        }),
        // Added 2026-11-11: a walk's currency off the dollar (A14 (3)), a new line, no edit.
        Response::WalkforwardResult(WalkforwardReportWire {
            strategy_file: String::new(),
            strategy_sha256: String::new(),
            windows: vec![],
            out_of_sample_equity_curve: vec![],
            out_of_sample_return: 0.0,
            out_of_sample_sharpe: 0.0,
            walkforward_consistency: 0.0,
            cost_model: None,
            data_warnings: Vec::new(),
            funding_paid: 0.0,
            account_currency: Some("JPY".to_string()),
        }),
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
    ]
}

#[test]
fn every_backtest_plane_request_keeps_its_bytes() {
    assert_frozen(&requests(), REQUEST_LINES);
}

#[test]
fn every_backtest_plane_response_keeps_its_bytes() {
    assert_frozen(&responses(), RESPONSE_LINES);
}
