//! Where each refusal is RAISED: the evidence a published refusal row carries (`module`, `on_type`, `on_fn`).
//!
//! A type says what a refusal is and what it says, never which function constructs it, so this is the one
//! fact about a refusal declared beside it rather than derived: an exhaustive `match` per id enum, so a new
//! variant does not build until it says where it is raised (or that it is a carrier, forwarding a text another
//! surface owns). `super::profile_surface_tests`' `every_refusal_site_names_its_variant` holds each site to
//! the file it names: the file constructs that variant, and declares that function.
//!
//! One site per file a refusal is raised in, its first function: the row the published schema carries.

use vike_fills::refusals::CostRefusalId;

use crate::run::refusals::ProfileRefusalId;

/// One place a refusal is raised: a repo-relative file, the enclosing `impl` type if any, the function.
#[derive(Debug, Clone, Copy)]
pub(super) struct Site {
    pub(super) file: &'static str,
    pub(super) on_type: Option<&'static str>,
    pub(super) on_fn: &'static str,
}

/// Whether a profile refusal is a row of the published refusals, and where it is raised.
pub(super) enum Raised {
    /// Raised by the profile's own checks, at these sites.
    At(&'static [Site]),
    /// Forwards a text another surface owns (`crate::run::refusals`' module doc): no row of its own.
    Carrier,
}

const RUN_PROFILE_VALIDATE_RS: &str = "crates/vike-backtest/src/run/profile/validate.rs";
const WALKFORWARD_WINDOWS_RS: &str = "crates/vike-backtest/src/walkforward/windows.rs";
const RUN_PROFILE_DATA_CFG_RS: &str = "crates/vike-backtest/src/run/profile/data_cfg.rs";
const RUN_PROFILE_RESOLUTION_CONFIG_RS: &str =
    "crates/vike-backtest/src/run/profile/resolution_config.rs";
const RUN_PROFILE_PERMUTATION_CONFIG_RS: &str =
    "crates/vike-backtest/src/run/profile/permutation_config.rs";
const RUN_DATA_PLAN_INPUTS_RS: &str = "crates/vike-backtest/src/run/data_plan/inputs.rs";
const RUN_PROFILE_MOUNTS_CONFIG_RS: &str = "crates/vike-backtest/src/run/profile/mounts_config.rs";
const HUB_STUDIO_RUN_STRATEGY_RS: &str = "crates/vike-backtesthub/src/studio_run/strategy.rs";
const RUN_PROFILE_RS: &str = "crates/vike-backtest/src/run/profile.rs";
const RUN_PROFILE_WALKFORWARD_CONFIG_RS: &str =
    "crates/vike-backtest/src/run/profile/walkforward_config.rs";
const RUN_PROFILE_ENGINE_CONFIG_RS: &str = "crates/vike-backtest/src/run/profile/engine_config.rs";
const RUN_PROFILE_SIZER_CONFIG_RS: &str = "crates/vike-backtest/src/run/profile/sizer_config.rs";
const RUN_COSTS_RS: &str = "crates/vike-backtest/src/run/costs.rs";
const PERMUTATION_RS: &str = "crates/vike-backtest/src/permutation.rs";
const WALKFORWARD_TICKS_RS: &str = "crates/vike-backtest/src/walkforward/ticks.rs";
const WALKFORWARD_RUNNER_RS: &str = "crates/vike-backtest/src/walkforward/runner.rs";
const WALKFORWARD_COMBINATORIAL_RS: &str = "crates/vike-backtest/src/walkforward/combinatorial.rs";
const SEARCH_PARAMSCAN_RS: &str = "crates/vike-backtest/src/search/paramscan.rs";
const SEARCH_EULER_RS: &str = "crates/vike-backtest/src/search/euler.rs";
const FILLS_COSTS_RS: &str = "crates/vike-fills/src/costs.rs";

const RUN_PROFILE_VALIDATE_PROFILE_BACKTEST_REFUSALS: &[Site] =
    &[Site { file: RUN_PROFILE_VALIDATE_RS, on_type: Some("ProfileBacktest"), on_fn: "refusals" }];
const WALKFORWARD_WINDOWS_RESOLVE_WINDOWS_ON: &[Site] =
    &[Site { file: WALKFORWARD_WINDOWS_RS, on_type: None, on_fn: "resolve_windows_on" }];
const RUN_PROFILE_VALIDATE_PROFILE_BACKTEST_VALIDATE_DATA_SLICE: &[Site] = &[Site {
    file: RUN_PROFILE_VALIDATE_RS,
    on_type: Some("ProfileBacktest"),
    on_fn: "validate_data_slice",
}];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_OPTIONS_UNDERLYING_INTERVAL_MS: &[Site] = &[Site {
    file: RUN_PROFILE_DATA_CFG_RS,
    on_type: Some("ProfileDataOptions"),
    on_fn: "underlying_interval_ms",
}];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_BAR_SOURCE: &[Site] =
    &[Site { file: RUN_PROFILE_DATA_CFG_RS, on_type: Some("ProfileData"), on_fn: "bar_source" }];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_CSV_FORMAT: &[Site] =
    &[Site { file: RUN_PROFILE_DATA_CFG_RS, on_type: Some("ProfileData"), on_fn: "csv_format" }];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_OPTIONS_CHECK: &[Site] =
    &[Site { file: RUN_PROFILE_DATA_CFG_RS, on_type: Some("ProfileData"), on_fn: "options_check" }];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_VALIDATE_HEARD: &[Site] = &[Site {
    file: RUN_PROFILE_DATA_CFG_RS,
    on_type: Some("ProfileData"),
    on_fn: "validate_heard",
}];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_WARMUP_STEPS: &[Site] =
    &[Site { file: RUN_PROFILE_DATA_CFG_RS, on_type: Some("ProfileData"), on_fn: "warmup_steps" }];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_DETAIL_INTERVAL_MS: &[Site] = &[Site {
    file: RUN_PROFILE_DATA_CFG_RS,
    on_type: Some("ProfileData"),
    on_fn: "detail_interval_ms",
}];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_ON_GAP: &[Site] =
    &[Site { file: RUN_PROFILE_DATA_CFG_RS, on_type: Some("ProfileData"), on_fn: "on_gap" }];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_MAX_GAP_MS: &[Site] =
    &[Site { file: RUN_PROFILE_DATA_CFG_RS, on_type: Some("ProfileData"), on_fn: "max_gap_ms" }];
const RUN_PROFILE_DATA_CFG_PROFILE_DATA_UNIVERSE_MODE: &[Site] =
    &[Site { file: RUN_PROFILE_DATA_CFG_RS, on_type: Some("ProfileData"), on_fn: "universe_mode" }];
const RUN_PROFILE_DATA_CFG_REFUSE_INVALID_DELAYS: &[Site] =
    &[Site { file: RUN_PROFILE_DATA_CFG_RS, on_type: None, on_fn: "refuse_invalid_delays" }];
const RUN_PROFILE_RESOLUTION_CONFIG_PROFILE_ENGINE_RESOLUTION_VALIDATE_SHAPE: &[Site] = &[Site {
    file: RUN_PROFILE_RESOLUTION_CONFIG_RS,
    on_type: Some("ProfileEngineResolution"),
    on_fn: "validate_shape",
}];
const RUN_PROFILE_RESOLUTION_CONFIG_PROFILE_ENGINE_RESOLUTION_BUILD: &[Site] = &[Site {
    file: RUN_PROFILE_RESOLUTION_CONFIG_RS,
    on_type: Some("ProfileEngineResolution"),
    on_fn: "build",
}];
const RUN_PROFILE_PERMUTATION_CONFIG_PROFILE_PERMUTATION_RANK_METRIC: &[Site] = &[Site {
    file: RUN_PROFILE_PERMUTATION_CONFIG_RS,
    on_type: Some("ProfilePermutation"),
    on_fn: "rank_metric",
}];
const RUN_DATA_PLAN_INPUTS_RESOLVE_INPUTS: &[Site] =
    &[Site { file: RUN_DATA_PLAN_INPUTS_RS, on_type: None, on_fn: "resolve_inputs" }];
const RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_ONE_STRATEGY: &[Site] = &[Site {
    file: RUN_PROFILE_MOUNTS_CONFIG_RS,
    on_type: Some("ProfileBacktest"),
    on_fn: "one_strategy",
}];
const RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_MOUNT_REFUSALS: &[Site] = &[Site {
    file: RUN_PROFILE_MOUNTS_CONFIG_RS,
    on_type: Some("ProfileBacktest"),
    on_fn: "mount_refusals",
}];
const RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_MOUNT_LAWS: &[Site] = &[Site {
    file: RUN_PROFILE_MOUNTS_CONFIG_RS,
    on_type: Some("ProfileBacktest"),
    on_fn: "mount_laws",
}];
const RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_MOUNT_COSTS: &[Site] = &[Site {
    file: RUN_PROFILE_MOUNTS_CONFIG_RS,
    on_type: Some("ProfileBacktest"),
    on_fn: "mount_costs",
}];
const RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_MOUNT_INPUTS: &[Site] = &[Site {
    file: RUN_PROFILE_MOUNTS_CONFIG_RS,
    on_type: Some("ProfileBacktest"),
    on_fn: "mount_inputs",
}];
const RUN_PROFILE_MOUNTS_CONFIG_REFUSE_STRATEGY_BESIDE_MOUNTS: &[Site] = &[Site {
    file: RUN_PROFILE_MOUNTS_CONFIG_RS,
    on_type: None,
    on_fn: "refuse_strategy_beside_mounts",
}];
const HUB_STUDIO_RUN_STRATEGY_STRATEGY_FILES_PREPARE: &[Site] =
    &[Site { file: HUB_STUDIO_RUN_STRATEGY_RS, on_type: Some("StrategyFiles"), on_fn: "prepare" }];
const RUN_PROFILE_PROFILE_STRATEGY_PATH_UNDER: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: Some("ProfileStrategy"), on_fn: "path_under" }];
const RUN_PROFILE_PROFILE_STRATEGY_READ_FILE: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: Some("ProfileStrategy"), on_fn: "read_file" }];
const RUN_PROFILE_PROFILE_STRATEGY_ACCEPT_TEXT: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: Some("ProfileStrategy"), on_fn: "accept_text" }];
const RUN_PROFILE_PROFILE_STRATEGY_UNREAD_FILE: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: Some("ProfileStrategy"), on_fn: "unread_file" }];
const RUN_PROFILE_PROFILE_STRATEGY_RHAI_COMPILE_ERROR: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: Some("ProfileStrategy"), on_fn: "rhai_compile_error" }];
const RUN_PROFILE_PROFILE_STRATEGY_NO_SANDBOX_ROUTE: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: Some("ProfileStrategy"), on_fn: "no_sandbox_route" }];
const RUN_PROFILE_REFUSE_RETIRED_SELECTION: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: None, on_fn: "refuse_retired_selection" }];
const RUN_PROFILE_PROFILE_BACKTEST_SLICE_TEXT: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: Some("ProfileBacktest"), on_fn: "slice_text" }];
const RUN_PROFILE_DECIDE_MODE: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: None, on_fn: "decide_mode" }];
const RUN_PROFILE_MARGIN_MODE: &[Site] =
    &[Site { file: RUN_PROFILE_RS, on_type: None, on_fn: "margin_mode" }];
const HUB_STUDIO_RUN_STRATEGY_LOADED_FILE_BUILDER: &[Site] =
    &[Site { file: HUB_STUDIO_RUN_STRATEGY_RS, on_type: None, on_fn: "loaded_file_builder" }];
const HUB_STUDIO_RUN_STRATEGY_STRATEGY_FILES_PREPARE_MOUNT: &[Site] = &[Site {
    file: HUB_STUDIO_RUN_STRATEGY_RS,
    on_type: Some("StrategyFiles"),
    on_fn: "prepare_mount",
}];
const HUB_STUDIO_RUN_STRATEGY_STRATEGY_CONFIG: &[Site] =
    &[Site { file: HUB_STUDIO_RUN_STRATEGY_RS, on_type: None, on_fn: "strategy_config" }];
const RUN_PROFILE_WALKFORWARD_CONFIG_SPAN_FIELD: &[Site] =
    &[Site { file: RUN_PROFILE_WALKFORWARD_CONFIG_RS, on_type: None, on_fn: "span_field" }];
const RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_WINDOW_FORM: &[Site] = &[Site {
    file: RUN_PROFILE_WALKFORWARD_CONFIG_RS,
    on_type: Some("ProfileWalkforward"),
    on_fn: "window_form",
}];
const RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_COMBINATORIAL_FORM: &[Site] = &[Site {
    file: RUN_PROFILE_WALKFORWARD_CONFIG_RS,
    on_type: Some("ProfileWalkforward"),
    on_fn: "combinatorial_form",
}];
const RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_WINDOW_SEARCH: &[Site] = &[Site {
    file: RUN_PROFILE_WALKFORWARD_CONFIG_RS,
    on_type: Some("ProfileWalkforward"),
    on_fn: "window_search",
}];
const RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_WALK_MODE: &[Site] = &[Site {
    file: RUN_PROFILE_WALKFORWARD_CONFIG_RS,
    on_type: Some("ProfileWalkforward"),
    on_fn: "walk_mode",
}];
const RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_RANK_METRIC: &[Site] = &[Site {
    file: RUN_PROFILE_WALKFORWARD_CONFIG_RS,
    on_type: Some("ProfileWalkforward"),
    on_fn: "rank_metric",
}];
const RUN_PROFILE_ENGINE_CONFIG_CHECKED_MARGIN_CALL: &[Site] =
    &[Site { file: RUN_PROFILE_ENGINE_CONFIG_RS, on_type: None, on_fn: "checked_margin_call" }];
const RUN_PROFILE_ENGINE_CONFIG_RESOLVE_LEDGER_CURRENCY: &[Site] =
    &[Site { file: RUN_PROFILE_ENGINE_CONFIG_RS, on_type: None, on_fn: "resolve_ledger_currency" }];
const RUN_PROFILE_ENGINE_CONFIG_RESOLVE_CONVERSIONS: &[Site] =
    &[Site { file: RUN_PROFILE_ENGINE_CONFIG_RS, on_type: None, on_fn: "resolve_conversions" }];
const RUN_PROFILE_ENGINE_CONFIG_PROFILE_ENGINE_QUEUE_MODEL_KIND: &[Site] = &[Site {
    file: RUN_PROFILE_ENGINE_CONFIG_RS,
    on_type: Some("ProfileEngine"),
    on_fn: "queue_model_kind",
}];
const RUN_PROFILE_ENGINE_CONFIG_PROFILE_ENGINE_FILL_MODEL_KIND: &[Site] = &[Site {
    file: RUN_PROFILE_ENGINE_CONFIG_RS,
    on_type: Some("ProfileEngine"),
    on_fn: "fill_model_kind",
}];
const RUN_PROFILE_SIZER_CONFIG_PROFILE_ENGINE_SIZER_BUILD_AT_DEPTH: &[Site] = &[Site {
    file: RUN_PROFILE_SIZER_CONFIG_RS,
    on_type: Some("ProfileEngineSizer"),
    on_fn: "build_at_depth",
}];
const RUN_PROFILE_SIZER_CONFIG_PROFILE_ENGINE_SIZER_NEED: &[Site] = &[Site {
    file: RUN_PROFILE_SIZER_CONFIG_RS,
    on_type: Some("ProfileEngineSizer"),
    on_fn: "need",
}];
const RUN_COSTS_REFUSE_RUN_LEVEL_COSTS: &[Site] =
    &[Site { file: RUN_COSTS_RS, on_type: None, on_fn: "refuse_run_level_costs" }];
const PERMUTATION_RUN_PERMUTATION_PROFILE: &[Site] =
    &[Site { file: PERMUTATION_RS, on_type: None, on_fn: "run_permutation_profile" }];
const PERMUTATION_PERMUTATION_RUN: &[Site] =
    &[Site { file: PERMUTATION_RS, on_type: None, on_fn: "permutation_run" }];
const WALKFORWARD_WINDOWS_SPLITS_TOO_MANY: &[Site] =
    &[Site { file: WALKFORWARD_WINDOWS_RS, on_type: None, on_fn: "splits_too_many" }];
const WALKFORWARD_WINDOWS_COMBINATORIAL_SPLITS_ON: &[Site] =
    &[Site { file: WALKFORWARD_WINDOWS_RS, on_type: None, on_fn: "combinatorial_splits_on" }];
const WALKFORWARD_WINDOWS_TO_BARS: &[Site] =
    &[Site { file: WALKFORWARD_WINDOWS_RS, on_type: None, on_fn: "to_bars" }];
const WALKFORWARD_TICKS_TICK_WALK_PREFLIGHT: &[Site] =
    &[Site { file: WALKFORWARD_TICKS_RS, on_type: Some("TickWalk"), on_fn: "preflight" }];
const WALKFORWARD_TICKS_TICK_WALK: &[Site] =
    &[Site { file: WALKFORWARD_TICKS_RS, on_type: None, on_fn: "tick_walk" }];
const WALKFORWARD_TICKS_TICK_COMBINATORIAL: &[Site] =
    &[Site { file: WALKFORWARD_TICKS_RS, on_type: None, on_fn: "tick_combinatorial" }];
const WALKFORWARD_RUNNER_WALKFORWARD_PREFLIGHT: &[Site] =
    &[Site { file: WALKFORWARD_RUNNER_RS, on_type: None, on_fn: "walkforward_preflight" }];
const WALKFORWARD_RUNNER_WALKFORWARD_TABLE: &[Site] =
    &[Site { file: WALKFORWARD_RUNNER_RS, on_type: None, on_fn: "walkforward_table" }];
const WALKFORWARD_RUNNER_ONE_SERIES_PREFLIGHT: &[Site] =
    &[Site { file: WALKFORWARD_RUNNER_RS, on_type: None, on_fn: "one_series_preflight" }];
const WALKFORWARD_RUNNER_REFUSE_A_SEARCH_WITHOUT_A_GRID: &[Site] =
    &[Site { file: WALKFORWARD_RUNNER_RS, on_type: None, on_fn: "refuse_a_search_without_a_grid" }];
const WALKFORWARD_COMBINATORIAL_RUN_WALKFORWARD_COMBINATORIAL_PROFILE: &[Site] = &[Site {
    file: WALKFORWARD_COMBINATORIAL_RS,
    on_type: None,
    on_fn: "run_walkforward_combinatorial_profile",
}];
const WALKFORWARD_COMBINATORIAL_COMBINATORIAL_WALK: &[Site] =
    &[Site { file: WALKFORWARD_COMBINATORIAL_RS, on_type: None, on_fn: "combinatorial_walk" }];
const WALKFORWARD_COMBINATORIAL_REFUSE_A_SEARCHED_CUT_WITHOUT_A_GRID: &[Site] = &[Site {
    file: WALKFORWARD_COMBINATORIAL_RS,
    on_type: None,
    on_fn: "refuse_a_searched_cut_without_a_grid",
}];
const WALKFORWARD_COMBINATORIAL_REFUSE_AN_UNRUNNABLE_CUT: &[Site] = &[Site {
    file: WALKFORWARD_COMBINATORIAL_RS,
    on_type: None,
    on_fn: "refuse_an_unrunnable_cut",
}];
const WALKFORWARD_COMBINATORIAL_COMBINATORIAL_PATHS: &[Site] =
    &[Site { file: WALKFORWARD_COMBINATORIAL_RS, on_type: None, on_fn: "combinatorial_paths" }];
const SEARCH_PARAMSCAN_EXPAND_PARAMSCAN_OVERRIDES: &[Site] =
    &[Site { file: SEARCH_PARAMSCAN_RS, on_type: None, on_fn: "expand_paramscan_overrides" }];
const SEARCH_PARAMSCAN_SWEEP_AXIS_ARRAY: &[Site] =
    &[Site { file: SEARCH_PARAMSCAN_RS, on_type: None, on_fn: "sweep_axis_array" }];
const SEARCH_PARAMSCAN_NUMERIC_SWEEP_AXES: &[Site] =
    &[Site { file: SEARCH_PARAMSCAN_RS, on_type: None, on_fn: "numeric_sweep_axes" }];
const SEARCH_EULER_NUMERIC_AXES: &[Site] =
    &[Site { file: SEARCH_EULER_RS, on_type: None, on_fn: "numeric_axes" }];
const FILLS_COSTS_AMOUNT: &[Site] =
    &[Site { file: FILLS_COSTS_RS, on_type: None, on_fn: "amount" }];
const FILLS_COSTS_WORD: &[Site] = &[Site { file: FILLS_COSTS_RS, on_type: None, on_fn: "word" }];
const FILLS_COSTS_COMMISSION_OF: &[Site] =
    &[Site { file: FILLS_COSTS_RS, on_type: None, on_fn: "commission_of" }];
const FILLS_COSTS_SLIPPAGE_OF: &[Site] =
    &[Site { file: FILLS_COSTS_RS, on_type: None, on_fn: "slippage_of" }];
const FILLS_COSTS_AMOUNT_OF_TICK: &[Site] =
    &[Site { file: FILLS_COSTS_RS, on_type: None, on_fn: "amount_of_tick" }];
const WALKFORWARD_SPLITS_BELOW_ONE_SITES: &[Site] =
    &[RUN_PROFILE_VALIDATE_PROFILE_BACKTEST_REFUSALS[0], WALKFORWARD_WINDOWS_RESOLVE_WINDOWS_ON[0]];

/// Where `id` is raised, or that it is a carrier.
pub(super) fn profile_raised_at(id: ProfileRefusalId) -> Raised {
    match id {
        ProfileRefusalId::EngineCashNotPositive
        | ProfileRefusalId::RiskMaximumOrdersPerWindowWallClock
        | ProfileRefusalId::EngineMarginCallBarModeOnly
        | ProfileRefusalId::EngineMarginCallWithLiquidation
        | ProfileRefusalId::EngineWholeEntryFeeNeedsClosedTrades
        | ProfileRefusalId::EngineFeedLatencyTickModeOnly
        | ProfileRefusalId::EngineQueueModelTickModeOnly
        | ProfileRefusalId::EngineEquitySampleEveryTickModeOnly
        | ProfileRefusalId::EngineCashGateBarModeOnly
        | ProfileRefusalId::EngineInBarBarModeOnly
        | ProfileRefusalId::EngineInBarWalkWithSharedCash
        | ProfileRefusalId::EngineDecideSimultaneousBarModeOnly
        | ProfileRefusalId::EngineDecideSimultaneousWithRiskGate
        | ProfileRefusalId::EngineDecideSimultaneousWithDetailInterval
        | ProfileRefusalId::EngineTimeframesBarModeOnly
        | ProfileRefusalId::DataIntervalInvalidForTimeframes
        | ProfileRefusalId::EngineTimeframesZeroLength
        | ProfileRefusalId::EngineTimeframesNotAnInterval
        | ProfileRefusalId::EngineTimeframesNotCoarser
        | ProfileRefusalId::EngineMaintenanceMarginNegative
        | ProfileRefusalId::EngineLiquidationBufferNegative
        | ProfileRefusalId::EngineVolumeLimitOutOfRange
        | ProfileRefusalId::EngineMultiplierNotPositive
        | ProfileRefusalId::EngineMultipliersNotPositive
        | ProfileRefusalId::EngineMultipliersNotLoaded
        | ProfileRefusalId::EngineMultiplierWrongUnit
        | ProfileRefusalId::EngineRiskFreeRateInvalid
        | ProfileRefusalId::EngineLeverageNotPositive
        | ProfileRefusalId::EngineMountLeverageCapInvalid
        | ProfileRefusalId::EngineClampToLeverageWithRisk
        | ProfileRefusalId::EngineClampToLeverageNeedsLeverage
        | ProfileRefusalId::EngineLeverageWithRisk
        | ProfileRefusalId::EngineSettlementPeriodNotPositive
        | ProfileRefusalId::WalkforwardSearchNeedsParamscan
        | ProfileRefusalId::PermutationPermutationsBelowOne
        | ProfileRefusalId::PermutationWithWalkforward
        | ProfileRefusalId::PermutationTrainWithoutParamscan
        | ProfileRefusalId::PermutationParamscanWithoutTrain
        | ProfileRefusalId::DataOnGapWithoutRequireCoverage
        | ProfileRefusalId::DataMaxGapWithoutRequireCoverage
        | ProfileRefusalId::DataDetailIntervalWithCashGate
        | ProfileRefusalId::DataDetailIntervalWithWalkforward
        | ProfileRefusalId::InputsRowUnparsed
        | ProfileRefusalId::DataOptionsLedgerNotDollar
        | ProfileRefusalId::StrategySdkWithRhaiFile
        | ProfileRefusalId::DataFromAfterTo => {
            Raised::At(RUN_PROFILE_VALIDATE_PROFILE_BACKTEST_REFUSALS)
        }
        ProfileRefusalId::WalkforwardSplitsBelowOne => {
            Raised::At(WALKFORWARD_SPLITS_BELOW_ONE_SITES)
        }
        ProfileRefusalId::DataTwoSliceForms
        | ProfileRefusalId::DataSymbolsEmptyWithoutSeries
        | ProfileRefusalId::DataVenueMissing
        | ProfileRefusalId::DataSymbolsEmpty
        | ProfileRefusalId::DataSeriesForeignAccount
        | ProfileRefusalId::DataDuplicateSymbol
        | ProfileRefusalId::DataSeriesKindTickModeOnly
        | ProfileRefusalId::EngineSnapToPropertiesCrossVenue => {
            Raised::At(RUN_PROFILE_VALIDATE_PROFILE_BACKTEST_VALIDATE_DATA_SLICE)
        }
        ProfileRefusalId::DataOptionsUnderlyingIntervalInvalid => {
            Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_OPTIONS_UNDERLYING_INTERVAL_MS)
        }
        ProfileRefusalId::DataFileFormatWithoutFile
        | ProfileRefusalId::DataSourceAndFileBothSet
        | ProfileRefusalId::DataSourceLaneUnknown
        | ProfileRefusalId::DataFileEmpty
        | ProfileRefusalId::DataSourceAndFileBarModeOnly
        | ProfileRefusalId::DataFileSeriesNotOne
        | ProfileRefusalId::DataFileDetailIntervalUnavailable
        | ProfileRefusalId::DataFileNotInStore => {
            Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_BAR_SOURCE)
        }
        ProfileRefusalId::DataFileFormatInvalid => {
            Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_CSV_FORMAT)
        }
        ProfileRefusalId::DataOptionsTickModeOnly
        | ProfileRefusalId::DataOptionsVenueAndChainMissing
        | ProfileRefusalId::DataOptionsLegsEmpty
        | ProfileRefusalId::DataOptionsLegEmptyOrRepeated
        | ProfileRefusalId::DataOptionsLegIsRunSeries
        | ProfileRefusalId::DataOptionsUnderlyingNotRunSeries
        | ProfileRefusalId::DataOptionsAssumedHalfSpreadOutOfRange => {
            Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_OPTIONS_CHECK)
        }
        ProfileRefusalId::DataHeardStreamsTickModeOnly
        | ProfileRefusalId::DataReferenceOnOwnVenue
        | ProfileRefusalId::DataReferenceKindTrade
        | ProfileRefusalId::DataReferenceListedTwice => {
            Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_VALIDATE_HEARD)
        }
        ProfileRefusalId::DataWarmupSpanInvalid
        | ProfileRefusalId::DataWarmupCalendarSpan
        | ProfileRefusalId::DataWarmupDurationOnTicks
        | ProfileRefusalId::DataWarmupIntervalInvalid
        | ProfileRefusalId::DataWarmupIntervalNotPositive => {
            Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_WARMUP_STEPS)
        }
        ProfileRefusalId::DataDetailIntervalBarModeOnly
        | ProfileRefusalId::DataDetailIntervalInvalid
        | ProfileRefusalId::DataDetailIntervalNotPositive
        | ProfileRefusalId::DataDetailIntervalBaseIntervalInvalid
        | ProfileRefusalId::DataDetailIntervalNotFiner => {
            Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_DETAIL_INTERVAL_MS)
        }
        ProfileRefusalId::DataOnGapUnknown => Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_ON_GAP),
        ProfileRefusalId::DataMaxGapSpanInvalid
        | ProfileRefusalId::DataMaxGapNotPositive
        | ProfileRefusalId::DataMaxGapBarCount
        | ProfileRefusalId::DataMaxGapCalendarSpan => {
            Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_MAX_GAP_MS)
        }
        ProfileRefusalId::DataUniverseUnknown => {
            Raised::At(RUN_PROFILE_DATA_CFG_PROFILE_DATA_UNIVERSE_MODE)
        }
        ProfileRefusalId::DataStreamDelayInvalid => {
            Raised::At(RUN_PROFILE_DATA_CFG_REFUSE_INVALID_DELAYS)
        }
        ProfileRefusalId::EngineResolutionKindUnknown
        | ProfileRefusalId::EngineResolutionWindowSecondsNotPositive
        | ProfileRefusalId::EngineResolutionWinnerNotBinary => {
            Raised::At(RUN_PROFILE_RESOLUTION_CONFIG_PROFILE_ENGINE_RESOLUTION_VALIDATE_SHAPE)
        }
        ProfileRefusalId::EngineResolutionWindowsWithoutWinner => {
            Raised::At(RUN_PROFILE_RESOLUTION_CONFIG_PROFILE_ENGINE_RESOLUTION_BUILD)
        }
        ProfileRefusalId::PermutationRankByUnknown => {
            Raised::At(RUN_PROFILE_PERMUTATION_CONFIG_PROFILE_PERMUTATION_RANK_METRIC)
        }
        ProfileRefusalId::InputsKeyNotDeclared | ProfileRefusalId::InputsNameRefused => {
            Raised::At(RUN_DATA_PLAN_INPUTS_RESOLVE_INPUTS)
        }
        ProfileRefusalId::MountsRouteRunsOneStrategy => {
            Raised::At(RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_ONE_STRATEGY)
        }
        ProfileRefusalId::StrategyOrMountsMissing
        | ProfileRefusalId::StrategyBesideMounts
        | ProfileRefusalId::MountsStrategyFileRefused
        | ProfileRefusalId::MountsStrategySdkBesideRhaiFile
        | ProfileRefusalId::MountsSymbolsEmpty
        | ProfileRefusalId::MountsSymbolsRepeated
        | ProfileRefusalId::MountsMaxLeverageBelowOneOrNotFinite
        | ProfileRefusalId::MountsMaxLeverageAboveEngine
        | ProfileRefusalId::MountsMaxLossNotPositiveOrNotFinite
        | ProfileRefusalId::MountsSymbolsNotInDataSlice
        | ProfileRefusalId::MountsSlotTradedByNoMount
        | ProfileRefusalId::MountsBesideWalkforward
        | ProfileRefusalId::MountsBesideParamscan
        | ProfileRefusalId::MountsBesidePermutation
        | ProfileRefusalId::MountsBesideSimultaneousDecide
        | ProfileRefusalId::MountsOnTickLane
        | ProfileRefusalId::EngineInBarFillAtCloseWithMounts
        | ProfileRefusalId::EngineInBarCommittedStopsWithMounts
        | ProfileRefusalId::EngineClosedTradesOffForPortfolio => {
            Raised::At(RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_MOUNT_REFUSALS)
        }
        ProfileRefusalId::MountsFileFillsAtClose
        | ProfileRefusalId::MountsOptimisticQuotesDiffer => {
            Raised::At(RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_MOUNT_LAWS)
        }
        ProfileRefusalId::MountsCostsAskedWithoutMount | ProfileRefusalId::MountsCostsDiffer => {
            Raised::At(RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_MOUNT_COSTS)
        }
        ProfileRefusalId::MountsInputReadsDiffer => {
            Raised::At(RUN_PROFILE_MOUNTS_CONFIG_PROFILE_BACKTEST_MOUNT_INPUTS)
        }
        ProfileRefusalId::MountsProfileNamesNoStrategyTable => {
            Raised::At(RUN_PROFILE_MOUNTS_CONFIG_REFUSE_STRATEGY_BESIDE_MOUNTS)
        }
        ProfileRefusalId::MountsPreparedAsOneStrategyTable => {
            Raised::At(HUB_STUDIO_RUN_STRATEGY_STRATEGY_FILES_PREPARE)
        }
        ProfileRefusalId::StrategyFilePathRefused => {
            Raised::At(RUN_PROFILE_PROFILE_STRATEGY_PATH_UNDER)
        }
        ProfileRefusalId::StrategyFileIsLink | ProfileRefusalId::StrategyFileUnreadable => {
            Raised::At(RUN_PROFILE_PROFILE_STRATEGY_READ_FILE)
        }
        ProfileRefusalId::StrategyFileHeaderRefused | ProfileRefusalId::StrategyParamsRefused => {
            Raised::At(RUN_PROFILE_PROFILE_STRATEGY_ACCEPT_TEXT)
        }
        ProfileRefusalId::StrategyFileNotRead => {
            Raised::At(RUN_PROFILE_PROFILE_STRATEGY_UNREAD_FILE)
        }
        ProfileRefusalId::StrategyFileRhaiCompileError => {
            Raised::At(RUN_PROFILE_PROFILE_STRATEGY_RHAI_COMPILE_ERROR)
        }
        ProfileRefusalId::StrategyFileNoSandboxRoute => {
            Raised::At(RUN_PROFILE_PROFILE_STRATEGY_NO_SANDBOX_ROUTE)
        }
        ProfileRefusalId::StrategyNameRhaiNoLongerRead
        | ProfileRefusalId::StrategyNameNoLongerRead
        | ProfileRefusalId::StrategyRhaiNoLongerRead => {
            Raised::At(RUN_PROFILE_REFUSE_RETIRED_SELECTION)
        }
        ProfileRefusalId::EngineStudioFeeNoLongerRead
        | ProfileRefusalId::EngineStudioSlippageNoLongerRead => {
            Raised::At(RUN_PROFILE_PROFILE_BACKTEST_SLICE_TEXT)
        }
        ProfileRefusalId::EngineDecideUnknown => Raised::At(RUN_PROFILE_DECIDE_MODE),
        ProfileRefusalId::EngineMarginModeIsolatedNotRun
        | ProfileRefusalId::EngineMarginModeUnknown => Raised::At(RUN_PROFILE_MARGIN_MODE),
        ProfileRefusalId::StrategyParamsNotSerialized => {
            Raised::At(HUB_STUDIO_RUN_STRATEGY_LOADED_FILE_BUILDER)
        }
        ProfileRefusalId::StrategyFileMountNotLoaded => {
            Raised::At(HUB_STUDIO_RUN_STRATEGY_STRATEGY_FILES_PREPARE_MOUNT)
        }
        ProfileRefusalId::StrategyArtifactNoLongerRun => {
            Raised::At(HUB_STUDIO_RUN_STRATEGY_STRATEGY_CONFIG)
        }
        ProfileRefusalId::WalkforwardDurationUnreadable => {
            Raised::At(RUN_PROFILE_WALKFORWARD_CONFIG_SPAN_FIELD)
        }
        ProfileRefusalId::WalkforwardSplitsBesideDurations
        | ProfileRefusalId::WalkforwardNoWindowForm
        | ProfileRefusalId::WalkforwardGapNeedsDurationForm
        | ProfileRefusalId::WalkforwardDurationFormHalfDeclared => {
            Raised::At(RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_WINDOW_FORM)
        }
        ProfileRefusalId::WalkforwardCombinatorialBesideOtherForm
        | ProfileRefusalId::WalkforwardCombinatorialFormHalfDeclared
        | ProfileRefusalId::WalkforwardGroupsCutNoSplit
        | ProfileRefusalId::WalkforwardCombinatorialConfiguresNoModeOrPurge => {
            Raised::At(RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_COMBINATORIAL_FORM)
        }
        ProfileRefusalId::WalkforwardSearchGridRenamed
        | ProfileRefusalId::WalkforwardSearchUnknown => {
            Raised::At(RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_WINDOW_SEARCH)
        }
        ProfileRefusalId::WalkforwardModeUnknown => {
            Raised::At(RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_WALK_MODE)
        }
        ProfileRefusalId::WalkforwardRankByUnknown => {
            Raised::At(RUN_PROFILE_WALKFORWARD_CONFIG_PROFILE_WALKFORWARD_RANK_METRIC)
        }
        ProfileRefusalId::EngineMarginCallRefused => {
            Raised::At(RUN_PROFILE_ENGINE_CONFIG_CHECKED_MARGIN_CALL)
        }
        ProfileRefusalId::EngineAccountCurrencyUnknown => {
            Raised::At(RUN_PROFILE_ENGINE_CONFIG_RESOLVE_LEDGER_CURRENCY)
        }
        ProfileRefusalId::EngineConversionNotACurrencyCode
        | ProfileRefusalId::EngineConversionLedgerOwnCurrency
        | ProfileRefusalId::EngineConversionQuoteTwice
        | ProfileRefusalId::EngineConversionSeriesUnreadable
        | ProfileRefusalId::EngineConversionPairDirectionUnreadable => {
            Raised::At(RUN_PROFILE_ENGINE_CONFIG_RESOLVE_CONVERSIONS)
        }
        ProfileRefusalId::EngineQueueModelPowerNotANumber
        | ProfileRefusalId::EngineQueueModelUnknown
        | ProfileRefusalId::EngineQueueModelRiskAverseRespelled => {
            Raised::At(RUN_PROFILE_ENGINE_CONFIG_PROFILE_ENGINE_QUEUE_MODEL_KIND)
        }
        ProfileRefusalId::EngineFillModelUnknown => {
            Raised::At(RUN_PROFILE_ENGINE_CONFIG_PROFILE_ENGINE_FILL_MODEL_KIND)
        }
        ProfileRefusalId::EngineSizerNestingTooDeep => {
            Raised::At(RUN_PROFILE_SIZER_CONFIG_PROFILE_ENGINE_SIZER_BUILD_AT_DEPTH)
        }
        ProfileRefusalId::EngineSizerKnobRequired
        | ProfileRefusalId::EngineSizerPortfolioHeatNeedsBase
        | ProfileRefusalId::EngineSizerDrawdownThrottleNeedsBase
        | ProfileRefusalId::EngineSizerKindUnknown
        | ProfileRefusalId::EngineSizerBaseUnderScalarKind => {
            Raised::At(RUN_PROFILE_SIZER_CONFIG_PROFILE_ENGINE_SIZER_NEED)
        }
        ProfileRefusalId::EngineFeeRateNoLongerRead
        | ProfileRefusalId::EngineFeeNoLongerRead
        | ProfileRefusalId::EngineSlippageNoLongerRead => {
            Raised::At(RUN_COSTS_REFUSE_RUN_LEVEL_COSTS)
        }
        ProfileRefusalId::PermutationTableMissing => {
            Raised::At(PERMUTATION_RUN_PERMUTATION_PROFILE)
        }
        ProfileRefusalId::PermutationPermutationsZero
        | ProfileRefusalId::PermutationBarModeOnly
        | ProfileRefusalId::PermutationTrainingNeedsParamscan => {
            Raised::At(PERMUTATION_PERMUTATION_RUN)
        }
        ProfileRefusalId::WalkforwardStepShorterThanTest
        | ProfileRefusalId::WalkforwardCombinatorialNotStitched => {
            Raised::At(WALKFORWARD_WINDOWS_RESOLVE_WINDOWS_ON)
        }
        ProfileRefusalId::WalkforwardSplitsTooMany => {
            Raised::At(WALKFORWARD_WINDOWS_SPLITS_TOO_MANY)
        }
        ProfileRefusalId::WalkforwardCombinatorialNeedsGroups => {
            Raised::At(WALKFORWARD_WINDOWS_COMBINATORIAL_SPLITS_ON)
        }
        ProfileRefusalId::WalkforwardSpanMonthsTooLong
        | ProfileRefusalId::WalkforwardSpanNeedsBarInterval
        | ProfileRefusalId::WalkforwardSpanMillisecondsTooLong
        | ProfileRefusalId::WalkforwardSpanShorterThanOneBar => {
            Raised::At(WALKFORWARD_WINDOWS_TO_BARS)
        }
        ProfileRefusalId::WalkforwardTickSeriesNotOne
        | ProfileRefusalId::WalkforwardTickEventsCrossWindowEdge
        | ProfileRefusalId::WalkforwardTickSeedBarIntervalMissing
        | ProfileRefusalId::WalkforwardTickRangeOpenEnded => {
            Raised::At(WALKFORWARD_TICKS_TICK_WALK_PREFLIGHT)
        }
        ProfileRefusalId::WalkforwardTickNoWindows
        | ProfileRefusalId::WalkforwardTickWindowShorterThanWarmup => {
            Raised::At(WALKFORWARD_TICKS_TICK_WALK)
        }
        ProfileRefusalId::WalkforwardTickGroupsMoreThanSeedBars
        | ProfileRefusalId::WalkforwardTickGroupsShorterThanWarmup => {
            Raised::At(WALKFORWARD_TICKS_TICK_COMBINATORIAL)
        }
        ProfileRefusalId::WalkforwardNoWindows
        | ProfileRefusalId::WalkforwardWindowsShorterThanWarmup => {
            Raised::At(WALKFORWARD_RUNNER_WALKFORWARD_PREFLIGHT)
        }
        ProfileRefusalId::WalkforwardTableMissing => {
            Raised::At(WALKFORWARD_RUNNER_WALKFORWARD_TABLE)
        }
        ProfileRefusalId::WalkforwardBarModeOnly | ProfileRefusalId::WalkforwardSeriesNotOne => {
            Raised::At(WALKFORWARD_RUNNER_ONE_SERIES_PREFLIGHT)
        }
        ProfileRefusalId::WalkforwardSearchWithoutGrid => {
            Raised::At(WALKFORWARD_RUNNER_REFUSE_A_SEARCH_WITHOUT_A_GRID)
        }
        ProfileRefusalId::WalkforwardTableMissingForCombinatorial => {
            Raised::At(WALKFORWARD_COMBINATORIAL_RUN_WALKFORWARD_COMBINATORIAL_PROFILE)
        }
        ProfileRefusalId::WalkforwardGroupsMoreThanBars
        | ProfileRefusalId::WalkforwardGroupsShorterThanWarmup => {
            Raised::At(WALKFORWARD_COMBINATORIAL_COMBINATORIAL_WALK)
        }
        ProfileRefusalId::WalkforwardCombinatorialSearchWithoutGrid => {
            Raised::At(WALKFORWARD_COMBINATORIAL_REFUSE_A_SEARCHED_CUT_WITHOUT_A_GRID)
        }
        ProfileRefusalId::WalkforwardCombinatorialCutUnrunnable
        | ProfileRefusalId::WalkforwardCombinatorialTooManyRuns => {
            Raised::At(WALKFORWARD_COMBINATORIAL_REFUSE_AN_UNRUNNABLE_CUT)
        }
        ProfileRefusalId::WalkforwardEmbargoLeavesNoTraining => {
            Raised::At(WALKFORWARD_COMBINATORIAL_COMBINATORIAL_PATHS)
        }
        ProfileRefusalId::ParamscanGridTooLarge => {
            Raised::At(SEARCH_PARAMSCAN_EXPAND_PARAMSCAN_OVERRIDES)
        }
        ProfileRefusalId::ParamscanAxisNotAnArray
        | ProfileRefusalId::ParamscanAxisEmpty
        | ProfileRefusalId::ParamscanAxisNamesEngineKey => {
            Raised::At(SEARCH_PARAMSCAN_SWEEP_AXIS_ARRAY)
        }
        ProfileRefusalId::ParamscanSearchNeedsTable
        | ProfileRefusalId::ParamscanAxisMixesIntegerAndFloat
        | ProfileRefusalId::ParamscanAxisNotNumeric => {
            Raised::At(SEARCH_PARAMSCAN_NUMERIC_SWEEP_AXES)
        }
        ProfileRefusalId::ParamscanEulerTooManyAxes => Raised::At(SEARCH_EULER_NUMERIC_AXES),
        ProfileRefusalId::StrategyStandInRefused
        | ProfileRefusalId::StrategyModelTextsRefused
        | ProfileRefusalId::StrategyFileBuildRefused
        | ProfileRefusalId::StrategyFileLoadRefused
        | ProfileRefusalId::StrategyCostRefused
        | ProfileRefusalId::StrategySlippageRefused
        | ProfileRefusalId::PermutationBarsRefused => Raised::Carrier,
    }
}

/// Where `id` is raised: every cost refusal is a row of the published refusals.
pub(super) fn cost_raised_at(id: CostRefusalId) -> &'static [Site] {
    match id {
        CostRefusalId::NotANonNegativeNumber => FILLS_COSTS_AMOUNT,
        CostRefusalId::NotAWord => FILLS_COSTS_WORD,
        CostRefusalId::CommissionUnitUnknown
        | CostRefusalId::CommissionUnitWithoutCommission
        | CostRefusalId::CommissionStatedWithoutUnit => FILLS_COSTS_COMMISSION_OF,
        CostRefusalId::SlippageUnitUnknown
        | CostRefusalId::SlippageStatedWithoutUnit
        | CostRefusalId::SlippageUnitWithoutSlippage
        | CostRefusalId::SlippageTicksWithoutTick => FILLS_COSTS_SLIPPAGE_OF,
        CostRefusalId::SlippageTickZero => FILLS_COSTS_AMOUNT_OF_TICK,
    }
}
