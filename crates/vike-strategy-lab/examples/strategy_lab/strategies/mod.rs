//! The lab's rows. A row whose rule the strategy gallery holds names it by its gallery name (decision 0269, W5): a
//! `STRATEGIES` row through `crate::desk::from_gallery`, a `PLANS` row through `crate::desk::gallery_on` with the row's own
//! `Terms`. Every row of this set is the gallery's (2026-11-10: the closed-trade and 1-minute rows moved with the order
//! language's reads, and the four files the licence review left out were deleted). A rule the lab would keep is a new file
//! here, one `mod` line and one row.

use crate::desk::{Plan, Terms, from_gallery, gallery_on};
use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

/// 2026-05-01 00:00 UTC, the last bar of a reference list: a position still open there is booked at its close.
pub const END_2026_05_01: i64 = 1_777_593_600_000;
/// 2026-04-06 00:00 UTC, the same for the lists that end there.
pub const END_2026_04_06: i64 = 1_775_433_600_000;
/// 2026-04-21 00:00 UTC, the same for the lists that end there.
pub const END_2026_04_21: i64 = 1_776_729_600_000;
/// 2026-04-05 00:00 UTC, for a flat list whose capture stopped before `END_2026_04_06`. Nothing records where it stopped:
/// the list only bounds it, and every bar inside those bounds gives the same trades.
pub const END_2026_04_05: i64 = 1_775_347_200_000;
/// 2026-04-01 00:00 UTC, the last bar of a reference list that books a position still open there.
pub const END_2026_04_01: i64 = 1_775_001_600_000;
/// 2026-05-04 00:00 UTC, the same.
pub const END_2026_05_04: i64 = 1_777_852_800_000;
/// 2026-04-30 21:45 UTC, the same. List-b books a position at this close; list-a closes it at the next open, a bar later.
pub const END_2026_04_30_2145: i64 = 1_777_585_500_000;

/// Indicators warm up on every bar held, from 2020.
pub const FROM_START: i64 = 0;
/// 2025-04-05 00:00 UTC, the first bar a reference list's indicators saw. A slow-settling indicator such as the DI lines
/// still remembers where it started, so this is how the list was made rather than a tuning choice.
pub const FROM_2025_04_05: i64 = 1_743_811_200_000;
/// 2025-03-31 00:00 UTC, the same for the lists that start there. A signal latched while flat still remembers it.
pub const FROM_2025_03_31: i64 = 1_743_379_200_000;
/// 2025-04-05 00:15 UTC, the first bar a capture states outright.
pub const FROM_2025_04_05_0015: i64 = 1_743_812_100_000;

pub struct Entry {
    /// Also names the reference trade lists, `<name>.list_a.csv` and `<name>.list_b.csv`.
    pub name: &'static str,
    /// The first bar fed to the strategy.
    pub warm_from_ms: i64,
    /// The last bar of the reference list. It differs between strategies, so each row states its own.
    pub end_ms: i64,
    pub build: fn() -> Box<dyn Rule + Send>,
}

const fn row(name: &'static str, end_ms: i64, build: fn() -> Box<dyn Rule + Send>) -> Entry {
    Entry { name, warm_from_ms: FROM_START, end_ms, build }
}

pub const STRATEGIES: &[Entry] = &[
    row("sma_152_close_cross", END_2026_05_01, || from_gallery("sma_152_close_cross")),
    row("rsi_14_cross_50", END_2026_05_01, || from_gallery("rsi_14_cross_50")),
    row("macd_12_26_9_signal_cross", END_2026_05_01, || from_gallery("macd_12_26_9_signal_cross")),
    row("hma_55_close_cross", END_2026_05_01, || from_gallery("hma_55_close_cross")),
    row("rsi_14_above_50", END_2026_05_01, || from_gallery("rsi_14_above_50")),
    row("williams_14_bands", END_2026_05_01, || from_gallery("williams_14_bands")),
    row("mfi_14_bands_20_80", END_2026_05_01, || from_gallery("mfi_14_bands_20_80")),
    row("cmo_9_zero_cross", END_2026_05_01, || from_gallery("cmo_9_zero_cross")),
    row("obv_ema_cross", END_2026_05_01, || from_gallery("obv_ema_cross")),
    row("accdist_ema_cross", END_2026_05_01, || from_gallery("accdist_ema_cross")),
    row("cog_10_signal_cross", END_2026_05_01, || from_gallery("cog_10_signal_cross")),
    row("hma_fast_slow_cross", END_2026_04_06, || from_gallery("hma_fast_slow_cross")),
    row("median_ema_cross", END_2026_04_06, || from_gallery("median_ema_cross")),
    row("vwma_sma_cross", END_2026_04_06, || from_gallery("vwma_sma_cross")),
    row("macd_above_signal", END_2026_05_01, || from_gallery("macd_above_signal")),
    row("rsi_14_above_55_below_45", END_2026_05_01, || from_gallery("rsi_14_above_55_below_45")),
    row("source_subscript_hl2", END_2026_05_01, || from_gallery("source_subscript_hl2")),
    row("source_runtime_override_high", END_2026_05_01, || {
        from_gallery("source_runtime_override_high")
    }),
    row("rsi14_bands_30_70", END_2026_05_01, || from_gallery("rsi14_bands_30_70")),
    row("tsi_25_13_signal_cross", END_2026_05_01, || from_gallery("tsi_25_13_signal_cross")),
    row("process_on_close_false", END_2026_05_01, || from_gallery("process_on_close_false")),
    row("rsi_ema_signal_cross", END_2026_04_06, || from_gallery("rsi_ema_signal_cross")),
    Entry {
        name: "barstate_isconfirmed_magnifier_off",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("barstate_isconfirmed_magnifier_off"),
    },
    row("cci_threshold_cross", END_2026_04_06, || from_gallery("cci_threshold_cross")),
    row("linreg_stdev_channel_revert", END_2026_04_06, || {
        from_gallery("linreg_stdev_channel_revert")
    }),
    Entry {
        name: "dmi_di_cross",
        warm_from_ms: FROM_2025_04_05,
        end_ms: END_2026_04_06,
        build: || from_gallery("dmi_di_cross"),
    },
    row("volume_spike_atr_breakout", END_2026_04_06, || from_gallery("volume_spike_atr_breakout")),
    row("bb_rsi_mean_reversion", END_2026_04_06, || from_gallery("bb_rsi_mean_reversion")),
    row("donchian_channel_breakout", END_2026_04_06, || from_gallery("donchian_channel_breakout")),
    row("elder_ray_bull_bear_power", END_2026_04_06, || from_gallery("elder_ray_bull_bear_power")),
    row("percentrank_mean_reversion", END_2026_04_05, || {
        from_gallery("percentrank_mean_reversion")
    }),
    row("supertrend_adx_filter", END_2026_04_06, || from_gallery("supertrend_adx_filter")),
    row("rsi_bb_self_bands", END_2026_04_06, || from_gallery("rsi_bb_self_bands")),
    Entry {
        name: "kama_style_efficiency_ratio",
        warm_from_ms: FROM_2025_04_05,
        end_ms: END_2026_04_06,
        build: || from_gallery("kama_style_efficiency_ratio"),
    },
    row("dual_thrust_open_anchored_range", END_2026_04_06, || {
        from_gallery("dual_thrust_open_anchored_range")
    }),
    row("engulfing_candle_pattern", END_2026_04_06, || from_gallery("engulfing_candle_pattern")),
    row("sma_dual_cross", END_2026_05_01, || from_gallery("sma_dual_cross")),
    row("stoch_slow_k_d_cross", END_2026_05_01, || from_gallery("stoch_slow_k_d_cross")),
    row("inside_bar_engulfing", END_2026_05_01, || from_gallery("inside_bar_engulfing")),
    row("keltner_channel_break", END_2026_05_01, || from_gallery("keltner_channel_break")),
    row("sar_flip_entry", END_2026_05_01, || from_gallery("sar_flip_entry")),
    row("pivot_confirmed_break", END_2026_05_01, || from_gallery("pivot_confirmed_break")),
    row("stdev_sma_expansion_break", END_2026_05_01, || from_gallery("stdev_sma_expansion_break")),
    Entry {
        name: "multi_indicator_score_composite",
        warm_from_ms: FROM_2025_04_05,
        end_ms: END_2026_04_06,
        build: || from_gallery("multi_indicator_score_composite"),
    },
    row("atr_trailing_stop_state", END_2026_04_06, || from_gallery("atr_trailing_stop_state")),
    row("na_nz_fixnan_history_chain", END_2026_04_01, || {
        from_gallery("na_nz_fixnan_history_chain")
    }),
    row("bb_kc_squeeze_breakout", END_2026_04_05, || from_gallery("bb_kc_squeeze_breakout")),
    row("chandelier_exit_direction", END_2026_04_06, || from_gallery("chandelier_exit_direction")),
    row("dual_ma_switch_dispatch", END_2026_04_06, || from_gallery("dual_ma_switch_dispatch")),
    row("ema_ribbon_stack_transition", END_2026_04_06, || {
        from_gallery("ema_ribbon_stack_transition")
    }),
    row("highestbars_lowestbars_breakout", END_2026_04_06, || {
        from_gallery("highestbars_lowestbars_breakout")
    }),
    row("macd_histogram_reversal", END_2026_04_06, || from_gallery("macd_histogram_reversal")),
    Entry {
        name: "momentum_roc_zero_cross",
        warm_from_ms: FROM_2025_04_05_0015,
        end_ms: END_2026_04_06,
        build: || from_gallery("momentum_roc_zero_cross"),
    },
    row("nvi_pvi_cross", END_2026_05_01, || from_gallery("nvi_pvi_cross")),
    row("pvt_ema_cross", END_2026_05_01, || from_gallery("pvt_ema_cross")),
    row("range_filter_var_band", END_2026_04_06, || from_gallery("range_filter_var_band")),
    row("rci_14_zero_cross", END_2026_05_01, || from_gallery("rci_14_zero_cross")),
    row("rsi_macd_and_continuous_state", END_2026_05_01, || {
        from_gallery("rsi_macd_and_continuous_state")
    }),
    row("supertrend_direction_flip", END_2026_05_01, || from_gallery("supertrend_direction_flip")),
    row("triple_sma_stack_latch", END_2026_04_06, || from_gallery("triple_sma_stack_latch")),
    Entry {
        name: "combo",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("combo"),
    },
    row("rsi_pullback_latch", END_2026_05_01, || from_gallery("rsi_pullback_latch")),
    Entry {
        name: "three_tier_ema_state",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("three_tier_ema_state"),
    },
    row("linreg_slope_channel", END_2026_05_01, || from_gallery("linreg_slope_channel")),
    row("pivot_bos_trigger", END_2026_05_01, || from_gallery("pivot_bos_trigger")),
    row("combo_2", END_2026_05_01, || from_gallery("combo_2")),
    row("ema_atr_breakout_band", END_2026_05_01, || from_gallery("ema_atr_breakout_band")),
    row("combo_4", END_2026_05_01, || from_gallery("combo_4")),
    Entry {
        name: "kalman_filter_1d",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("kalman_filter_1d"),
    },
    row("pivot_strength_5", END_2026_05_01, || from_gallery("pivot_strength_5")),
    row("rsi_smooth_divergence", END_2026_05_01, || from_gallery("rsi_smooth_divergence")),
    row("vol_zscore_anomaly", END_2026_05_01, || from_gallery("vol_zscore_anomaly")),
    row("cumulative_volume_delta", END_2026_05_01, || from_gallery("cumulative_volume_delta")),
    Entry {
        name: "manual_adx_regime",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("manual_adx_regime"),
    },
    row("rolling_highest_lowest", END_2026_05_01, || from_gallery("rolling_highest_lowest")),
    row("fast_ma_cross_trigger", END_2026_05_01, || from_gallery("fast_ma_cross_trigger")),
    row("alert_templates", END_2026_05_01, || from_gallery("alert_templates")),
    row("explicit_reverse", END_2026_05_01, || from_gallery("explicit_reverse")),
    Entry {
        name: "adx_regime_classify",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("adx_regime_classify"),
    },
    row("pivot_liquidity_sweep", END_2026_05_01, || from_gallery("pivot_liquidity_sweep")),
    row("pressure_gauge", END_2026_05_01, || from_gallery("pressure_gauge")),
    row("rsi_macd_momentum", END_2026_05_01, || from_gallery("rsi_macd_momentum")),
    row("three_ema_bias_score", END_2026_05_01, || from_gallery("three_ema_bias_score")),
    row("bb_kc_squeeze_release", END_2026_05_01, || from_gallery("bb_kc_squeeze_release")),
    Entry {
        name: "combo_3",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("combo_3"),
    },
    Entry {
        name: "quad_ema_stack",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("quad_ema_stack"),
    },
    row("max_bars_back_500", END_2026_05_01, || from_gallery("max_bars_back_500")),
    row("margin_100_pct", END_2026_05_01, || from_gallery("margin_100_pct")),
    row("pivot_hh_ll", END_2026_05_01, || from_gallery("pivot_hh_ll")),
    row("wick_pierce_close_back", END_2026_05_01, || from_gallery("wick_pierce_close_back")),
    row("wait_one_continuation", END_2026_05_01, || from_gallery("wait_one_continuation")),
    row("combo_5", END_2026_05_01, || from_gallery("combo_5")),
    row("combo_6", END_2026_05_01, || from_gallery("combo_6")),
    row("pivot_state_machine", END_2026_05_01, || from_gallery("pivot_state_machine")),
    row("state_edge_detector", END_2026_05_01, || from_gallery("state_edge_detector")),
    row("trend_momentum_structure_gate", END_2026_05_01, || {
        from_gallery("trend_momentum_structure_gate")
    }),
    row("combo_7", END_2026_05_01, || from_gallery("combo_7")),
    row("mtf_htf_weekly_sma_cross", END_2026_05_01, || from_gallery("mtf_htf_weekly_sma_cross")),
    row("mtf_daily_prev_high_break", END_2026_04_21, || from_gallery("mtf_daily_prev_high_break")),
    row("mtf_htf_60_rsi14_inside_security", END_2026_04_21, || {
        from_gallery("mtf_htf_60_rsi14_inside_security")
    }),
    row("mtf_htf_60_volume_spike", END_2026_04_21, || from_gallery("mtf_htf_60_volume_spike")),
    Entry {
        name: "mtf_daily_array_median_percentrank",
        warm_from_ms: FROM_2025_04_05,
        end_ms: END_2026_04_06,
        build: || from_gallery("mtf_daily_array_median_percentrank"),
    },
    row("mtf_htf_60_close_change_baseline", END_2026_04_06, || {
        from_gallery("mtf_htf_60_close_change_baseline")
    }),
    row("mtf_htf_60_close_roll", END_2026_04_21, || from_gallery("mtf_htf_60_close_roll")),
    row("mtf_htf_60_sma20_inside_security", END_2026_04_21, || {
        from_gallery("mtf_htf_60_sma20_inside_security")
    }),
    row("mtf_dual_tf_60_240_rising", END_2026_04_21, || from_gallery("mtf_dual_tf_60_240_rising")),
    row("vwap_bands_breakout_1sigma", END_2026_05_01, || {
        from_gallery("vwap_bands_breakout_1sigma")
    }),
    row("vwap_bands_mean_reversion_2sigma", END_2026_05_01, || {
        from_gallery("vwap_bands_mean_reversion_2sigma")
    }),
    row("session_hour_minute_pulse_filter", END_2026_04_01, || {
        from_gallery("session_hour_minute_pulse_filter")
    }),
    row("timeframe_main_period_self_adaptive", END_2026_05_01, || {
        from_gallery("timeframe_main_period_self_adaptive")
    }),
    row("session_ny_spring_forward_dst", END_2026_05_01, || {
        from_gallery("session_ny_spring_forward_dst")
    }),
    row("session_tz_newyork", END_2026_05_01, || from_gallery("session_tz_newyork")),
    row("pivot_point_levels_break", END_2026_05_01, || from_gallery("pivot_point_levels_break")),
    row("udt_method_scalar_return", END_2026_05_01, || from_gallery("udt_method_scalar_return")),
    row("udt_method_uses_history_globals", END_2026_05_01, || {
        from_gallery("udt_method_uses_history_globals")
    }),
    row("udt_method_default_param_kwargs", END_2026_05_01, || {
        from_gallery("udt_method_default_param_kwargs")
    }),
    row("udt_method_mutating_self_ref", END_2026_05_01, || {
        from_gallery("udt_method_mutating_self_ref")
    }),
    row("udt_method_uses_na_nz_fixnan", END_2026_05_01, || {
        from_gallery("udt_method_uses_na_nz_fixnan")
    }),
    row("udt_method_udt_return_from_func", END_2026_05_01, || {
        from_gallery("udt_method_udt_return_from_func")
    }),
    row("drawing_delete_halt", END_2026_05_01, || from_gallery("drawing_delete_halt")),
    Entry {
        name: "drawing_line_level_breakout",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("drawing_line_level_breakout"),
    },
    Entry {
        name: "drawing_box_zone_reentry",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("drawing_box_zone_reentry"),
    },
    Entry {
        name: "mtf_daily_ema26_warmup",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_04_01,
        build: || from_gallery("mtf_daily_ema26_warmup"),
    },
    row("mtf_triple_tf_close_confluence", END_2026_04_01, || {
        from_gallery("mtf_triple_tf_close_confluence")
    }),
    row("mtf_htf_60_gaps_on_roll", END_2026_04_01, || from_gallery("mtf_htf_60_gaps_on_roll")),
    Entry {
        name: "mtf_roll_state_60_240_d_minimal",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_04_01,
        build: || from_gallery("mtf_roll_state_60_240_d_minimal"),
    },
    row("mtf_triple_tf_macd_hist_confluence", END_2026_04_01, || {
        from_gallery("mtf_triple_tf_macd_hist_confluence")
    }),
    row("recompute_mtf_rsi_macd_bb", END_2026_05_01, || from_gallery("recompute_mtf_rsi_macd_bb")),
    row("cooldown_daily_cap", END_2026_05_01, || from_gallery("cooldown_daily_cap")),
    row("session_window_nbar_exit", END_2026_05_01, || from_gallery("session_window_nbar_exit")),
    row("na_deep_history_int_na", END_2026_05_01, || from_gallery("na_deep_history_int_na")),
    row("analyzer_self_test_multi_mode", END_2026_04_21, || {
        from_gallery("analyzer_self_test_multi_mode")
    }),
    row("udt_method_receives_ta_series_param", END_2026_05_01, || {
        from_gallery("udt_method_receives_ta_series_param")
    }),
    row("udt_method_var_instance_streak", END_2026_05_01, || {
        from_gallery("udt_method_var_instance_streak")
    }),
    row("udt_method_in_for_loop", END_2026_05_01, || from_gallery("udt_method_in_for_loop")),
    row("udt_method_in_while_loop", END_2026_05_01, || from_gallery("udt_method_in_while_loop")),
    row("udt_method_windowed_method_chain", END_2026_05_01, || {
        from_gallery("udt_method_windowed_method_chain")
    }),
    row("udt_method_drives_strategy_entry", END_2026_05_01, || {
        from_gallery("udt_method_drives_strategy_entry")
    }),
    row("udt_method_in_switch_arms", END_2026_05_01, || from_gallery("udt_method_in_switch_arms")),
    row("udt_method_on_array_element", END_2026_05_01, || {
        from_gallery("udt_method_on_array_element")
    }),
    row("udt_method_in_if_else_branch", END_2026_05_01, || {
        from_gallery("udt_method_in_if_else_branch")
    }),
    row("udt_method_calls_sibling_cumulative", END_2026_05_01, || {
        from_gallery("udt_method_calls_sibling_cumulative")
    }),
    Entry {
        name: "matrix_bool_mask_transpose_roundtrip",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("matrix_bool_mask_transpose_roundtrip"),
    },
    Entry {
        name: "matrix_bool_mask_no_transpose",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("matrix_bool_mask_no_transpose"),
    },
    Entry {
        name: "matrix_bool_mask_explicit_utc_tz",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("matrix_bool_mask_explicit_utc_tz"),
    },
    Entry {
        name: "matrix_bool_regime_mask_24x7",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("matrix_bool_regime_mask_24x7"),
    },
    row("matrix_covariance_eigen_pca", END_2026_04_06, || {
        from_gallery("matrix_covariance_eigen_pca")
    }),
    row("matrix_eigen_rank_deficient_cov", END_2026_05_01, || {
        from_gallery("matrix_eigen_rank_deficient_cov")
    }),
    row("drawing_chartpoint_trendline", END_2026_05_01, || {
        from_gallery("drawing_chartpoint_trendline")
    }),
    row("drawing_udt_alias_identity", END_2026_05_01, || {
        from_gallery("drawing_udt_alias_identity")
    }),
    row("drawing_visual_noise_geometry", END_2026_05_01, || {
        from_gallery("drawing_visual_noise_geometry")
    }),
    row("map_regime_threshold_lookup", END_2026_04_06, || {
        from_gallery("map_regime_threshold_lookup")
    }),
    row("str_match_regex_filter", END_2026_05_01, || from_gallery("str_match_regex_filter")),
    Entry {
        name: "calc_on_every_tick",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("calc_on_every_tick"),
    },
    row("pivot_array_unshift_pop", END_2026_04_06, || from_gallery("pivot_array_unshift_pop")),
    Entry {
        name: "line_new_projection",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("line_new_projection"),
    },
    Entry {
        name: "barstate_isconfirmed_magnifier_on",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("barstate_isconfirmed_magnifier_on"),
    },
    row("recompute_alma_sar_corr_magnifier", END_2026_05_01, || {
        from_gallery("recompute_alma_sar_corr_magnifier")
    }),
    Entry {
        name: "fvg_active_zones",
        warm_from_ms: FROM_2025_03_31,
        end_ms: END_2026_05_01,
        build: || from_gallery("fvg_active_zones"),
    },
    row("combo_9", END_2026_05_01, || from_gallery("combo_9")),
    Entry {
        name: "udt_regime_stack_stress",
        warm_from_ms: FROM_2025_04_05,
        end_ms: END_2026_04_06,
        build: || from_gallery("udt_regime_stack_stress"),
    },
];

pub struct PlanEntry {
    /// Also names the reference trade lists, `<name>.list_a.csv` and `<name>.list_b.csv`.
    pub name: &'static str,
    /// The first bar fed to the strategy.
    pub warm_from_ms: i64,
    /// The first bar orders may be placed on. `None` is the signal bar of list-b's first trade, which only holds for an
    /// entry that fills on the next bar: a resting entry may have been placed long before it filled.
    pub trade_from_ms: Option<i64>,
    /// The last bar of the reference list.
    pub end_ms: i64,
    pub build: fn() -> Box<dyn Plan>,
    /// The rule reads or walks 1-minute bars, so they are read for it.
    pub minutes: bool,
}

const fn plan_row(name: &'static str, end_ms: i64, build: fn() -> Box<dyn Plan>) -> PlanEntry {
    PlanEntry { name, warm_from_ms: FROM_START, trade_from_ms: None, end_ms, build, minutes: false }
}

pub const PLANS: &[PlanEntry] = &[
    COST_CONTROL,
    plan_row("analyzer_cashflow_cash_costs", END_2026_05_04, || {
        gallery_on(
            "analyzer_cashflow_cash_costs",
            Terms { cash: 75_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("order_process_on_close_true", END_2026_05_01, || {
        gallery_on("order_process_on_close_true", Terms::PLAIN)
    }),
    plan_row("analyzer_anvil_percent_costs", END_2026_05_04, || {
        gallery_on(
            "analyzer_anvil_percent_costs",
            Terms { cash: 100_000.0, slip_ticks: 2.0, ..Terms::PLAIN },
        )
    }),
    plan_row("oca_multi_bracket_isolation", END_2026_05_01, || {
        gallery_on("oca_multi_bracket_isolation", Terms::PLAIN)
    }),
    plan_row("session_borough_new_york", END_2026_05_04, || {
        gallery_on(
            "session_borough_new_york",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("mantle_keltner_regime", END_2026_05_04, || {
        gallery_on(
            "mantle_keltner_regime",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("udt_method_reads_strategy_state", END_2026_05_01, || {
        gallery_on("udt_method_reads_strategy_state", Terms::PLAIN)
    }),
    plan_row("order_flashpoint_immediate_close", END_2026_05_04, || {
        gallery_on(
            "order_flashpoint_immediate_close",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("plumbline_pvt_vwma", END_2026_05_04, || {
        gallery_on("plumbline_pvt_vwma", Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN })
    }),
    plan_row("torque_tsi_signal", END_2026_05_04, || {
        gallery_on("torque_tsi_signal", Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN })
    }),
    plan_row("math_kiln_power_efficiency", END_2026_05_04, || {
        gallery_on(
            "math_kiln_power_efficiency",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("mtf_rangefinder_daily_reference", END_2026_05_04, || {
        gallery_on(
            "mtf_rangefinder_daily_reference",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("quarry_alma_correlation", END_2026_05_04, || {
        gallery_on(
            "quarry_alma_correlation",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("sextant_cci_hma", END_2026_05_04, || {
        gallery_on("sextant_cci_hma", Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN })
    }),
    plan_row("zenith_rci_wpr", END_2026_05_04, || {
        gallery_on("zenith_rci_wpr", Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN })
    }),
    plan_row("aperture_cog_linreg", END_2026_05_04, || {
        gallery_on(
            "aperture_cog_linreg",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("pendulum_sar_rotation", END_2026_05_04, || {
        gallery_on(
            "pendulum_sar_rotation",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("conduit_obv_accdist", END_2026_05_04, || {
        gallery_on(
            "conduit_obv_accdist",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("tally_volume_index_roc", END_2026_05_04, || {
        gallery_on(
            "tally_volume_index_roc",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("map_archive_presence", END_2026_05_04, || {
        gallery_on(
            "map_archive_presence",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("order_tranche_partial_market_close", END_2026_05_04, || {
        gallery_on(
            "order_tranche_partial_market_close",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("stats_ledger_outcome_throttle", END_2026_05_04, || {
        gallery_on(
            "stats_ledger_outcome_throttle",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("stats_eventrades_zero_pnl_count", END_2026_05_01, || {
        gallery_on("stats_eventrades_zero_pnl_count", Terms::PLAIN)
    }),
    plan_row("oca_exit_bracket_internal_cancel", END_2026_05_01, || {
        gallery_on("oca_exit_bracket_internal_cancel", Terms::PLAIN)
    }),
    plan_row("pyramid_cash_fractional_commission", END_2026_05_01, || {
        gallery_on("pyramid_cash_fractional_commission", Terms::PLAIN)
    }),
    plan_row("mtf_htf_confluence_manual_trail", END_2026_05_01, || {
        gallery_on("mtf_htf_confluence_manual_trail", Terms::PLAIN)
    }),
    plan_row("syntax_glyph_string_regex", END_2026_05_04, || {
        gallery_on(
            "syntax_glyph_string_regex",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("udt_tessellate_tuple_method", END_2026_05_04, || {
        gallery_on(
            "udt_tessellate_tuple_method",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("analyzer_parity_choch_bos_isolator", END_2026_05_01, || {
        gallery_on("analyzer_parity_choch_bos_isolator", Terms::PLAIN)
    }),
    plan_row("udt_courier_return_method", END_2026_05_04, || {
        gallery_on(
            "udt_courier_return_method",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("udt_switchboard_method_arms", END_2026_05_04, || {
        gallery_on(
            "udt_switchboard_method_arms",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("udt_loopback_method_for", END_2026_05_04, || {
        gallery_on(
            "udt_loopback_method_for",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("array_spectrum_deque", END_2026_05_04, || {
        gallery_on(
            "array_spectrum_deque",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("matrix_eigen_covariance", END_2026_05_04, || {
        gallery_on(
            "matrix_eigen_covariance",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("udt_breaker_method_while", END_2026_05_04, || {
        gallery_on(
            "udt_breaker_method_while",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("matrix_cadence_transpose_pairs", END_2026_05_04, || {
        gallery_on(
            "matrix_cadence_transpose_pairs",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("timeframe_dial_main_period", END_2026_05_04, || {
        gallery_on(
            "timeframe_dial_main_period",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    PlanEntry {
        trade_from_ms: Some(FROM_2025_03_31),
        ..plan_row("order_stop_cancel_no_regime_close", END_2026_05_01, || {
            gallery_on("order_stop_cancel_no_regime_close", Terms::PLAIN)
        })
    },
    plan_row("session_compass_utc_market_window", END_2026_05_04, || {
        gallery_on(
            "session_compass_utc_market_window",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("calendar_waypoint_weekday_macd", END_2026_05_04, || {
        gallery_on(
            "calendar_waypoint_weekday_macd",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("syntax_caliper_scalar_switch", END_2026_05_04, || {
        gallery_on(
            "syntax_caliper_scalar_switch",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("enum_relay_regime", END_2026_05_04, || {
        gallery_on("enum_relay_regime", Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN })
    }),
    plan_row("math_forge_log_channel", END_2026_05_04, || {
        gallery_on(
            "math_forge_log_channel",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("meridian_dmi_regime", END_2026_05_04, || {
        gallery_on(
            "meridian_dmi_regime",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("keystone_udf_tuple", END_2026_05_04, || {
        gallery_on("keystone_udf_tuple", Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN })
    }),
    plan_row("map_mosaic_regime_weight", END_2026_05_04, || {
        gallery_on(
            "map_mosaic_regime_weight",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("matrix_prism_factor_vote", END_2026_05_04, || {
        gallery_on(
            "matrix_prism_factor_vote",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("na_ledger_history_anchor", END_2026_05_04, || {
        gallery_on(
            "na_ledger_history_anchor",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    PlanEntry {
        trade_from_ms: Some(FROM_2025_03_31),
        ..plan_row("order_vector_stop_breakout", END_2026_05_04, || {
            gallery_on(
                "order_vector_stop_breakout",
                Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
            )
        })
    },
    PlanEntry {
        trade_from_ms: Some(FROM_2025_03_31),
        ..plan_row("order_harbor_stop_limit", END_2026_05_04, || {
            gallery_on(
                "order_harbor_stop_limit",
                Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
            )
        })
    },
    PlanEntry {
        trade_from_ms: Some(FROM_2025_03_31),
        ..plan_row("order_stop_entry_cancel_opposite", END_2026_05_01, || {
            gallery_on("order_stop_entry_cancel_opposite", Terms::PLAIN)
        })
    },
    plan_row("order_range_expansion_pending_stop", END_2026_05_01, || {
        gallery_on("order_range_expansion_pending_stop", Terms::PLAIN)
    }),
    plan_row("order_keystone_limit_replace", END_2026_05_04, || {
        gallery_on(
            "order_keystone_limit_replace",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("pyramid_deferred_flip_close_all", END_2026_05_01, || {
        gallery_on("pyramid_deferred_flip_close_all", Terms::PLAIN)
    }),
    plan_row("tight_tp_sl_points", END_2026_05_01, || {
        gallery_on("tight_tp_sl_points", Terms::PLAIN)
    }),
    plan_row("udt_method_extra_primitive_args", END_2026_05_01, || {
        gallery_on("udt_method_extra_primitive_args", Terms::PLAIN)
    }),
    plan_row("udt_method_feeds_strategy_exit_prices", END_2026_05_01, || {
        gallery_on("udt_method_feeds_strategy_exit_prices", Terms::PLAIN)
    }),
    plan_row("udt_method_uses_math_funcs", END_2026_05_01, || {
        gallery_on("udt_method_uses_math_funcs", Terms::PLAIN)
    }),
    plan_row("udt_method_tuple_return_destructure", END_2026_05_01, || {
        gallery_on("udt_method_tuple_return_destructure", Terms::PLAIN)
    }),
    plan_row("volume_tide_vwap_mfi", END_2026_05_04, || {
        gallery_on(
            "volume_tide_vwap_mfi",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("bands_constellation_mean_reversion", END_2026_05_04, || {
        gallery_on(
            "bands_constellation_mean_reversion",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("mtf_cascade_tuple", END_2026_05_04, || {
        gallery_on("mtf_cascade_tuple", Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN })
    }),
    plan_row("mtf_orbit_trend", END_2026_05_04, || {
        gallery_on("mtf_orbit_trend", Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN })
    }),
    plan_row("combo_10", END_2026_05_01, || gallery_on("combo_10", Terms::PLAIN)),
    plan_row("bracket_compass_partial_ladder", END_2026_05_04, || {
        gallery_on(
            "bracket_compass_partial_ladder",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("bracket_revision_exit_replace", END_2026_05_04, || {
        gallery_on(
            "bracket_revision_exit_replace",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("bracket_tp_sl_oca_reduce_isolate", END_2026_05_01, || {
        gallery_on("bracket_tp_sl_oca_reduce_isolate", Terms::PLAIN)
    }),
    plan_row("pivot_atr_stop_target", END_2026_04_06, || {
        gallery_on("pivot_atr_stop_target", Terms::PLAIN)
    }),
    plan_row("bracket_aurora_market_atr", END_2026_05_04, || {
        gallery_on(
            "bracket_aurora_market_atr",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("bracket_rivet_calc_on_fill", END_2026_05_04, || {
        gallery_on(
            "bracket_rivet_calc_on_fill",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("order_switchback_all_in_reversal", END_2026_05_04, || {
        gallery_on("order_switchback_all_in_reversal", Terms { cash: 100_000.0, ..Terms::PLAIN })
    }),
    plan_row("cap_max_intraday_filled_orders_isolate", END_2026_05_01, || {
        gallery_on("cap_max_intraday_filled_orders_isolate", Terms::PLAIN)
    }),
    plan_row("cap_gatekeeper_intraday_risk", END_2026_05_04, || {
        gallery_on(
            "cap_gatekeeper_intraday_risk",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("closedtrades_risk_introspection", END_2026_04_06, || {
        gallery_on("closedtrades_risk_introspection", Terms::PLAIN)
    }),
    plan_row("bracket_cap_range_pending_stop", END_2026_05_01, || {
        gallery_on("bracket_cap_range_pending_stop", Terms::PLAIN)
    }),
    plan_row("order_horizon_next_bar_close", END_2026_05_04, || {
        gallery_on(
            "order_horizon_next_bar_close",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("order_crosscurrent_opposite_market_pair", END_2026_05_04, || {
        gallery_on(
            "order_crosscurrent_opposite_market_pair",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("pyramid_terrace_staged_entry", END_2026_05_04, || {
        gallery_on(
            "pyramid_terrace_staged_entry",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("control_heliograph_while_channel", END_2026_05_04, || {
        gallery_on(
            "control_heliograph_while_channel",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("udt_sentinel_method_regime", END_2026_05_04, || {
        gallery_on(
            "udt_sentinel_method_regime",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("array_atlas_momentum_rotation", END_2026_05_04, || {
        gallery_on(
            "array_atlas_momentum_rotation",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    PlanEntry {
        warm_from_ms: FROM_2025_03_31,
        ..plan_row("pivot_anchored_bracket", END_2026_05_01, || {
            gallery_on("pivot_anchored_bracket", Terms::PLAIN)
        })
    },
    plan_row("barstate_lattice_confirmed_pivot", END_2026_05_04, || {
        gallery_on(
            "barstate_lattice_confirmed_pivot",
            Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN },
        )
    }),
    plan_row("mtf_htf_confluence_static_bracket", END_2026_05_01, || {
        gallery_on("mtf_htf_confluence_static_bracket", Terms::PLAIN)
    }),
    PlanEntry {
        warm_from_ms: FROM_2025_03_31,
        ..plan_row("combo_11", END_2026_05_01, || gallery_on("combo_11", Terms::PLAIN))
    },
    PlanEntry {
        minutes: true,
        ..plan_row("ltf_numeric_float_ratio15", END_2026_04_30_2145, || {
            gallery_on("ltf_numeric_float_ratio15", Terms::PLAIN)
        })
    },
    PlanEntry {
        minutes: true,
        ..plan_row("ltf_bool_array_bull_majority", END_2026_04_30_2145, || {
            gallery_on("ltf_bool_array_bull_majority", Terms::PLAIN)
        })
    },
    PlanEntry {
        minutes: true,
        ..plan_row("magnifier_tick_dist_endpoints", END_2026_05_01, || {
            gallery_on("magnifier_tick_dist_endpoints", Terms { magnifier: true, ..Terms::PLAIN })
        })
    },
    PlanEntry {
        minutes: true,
        ..plan_row("magnifier_tick_dist_volume_weighted_on", END_2026_05_01, || {
            gallery_on(
                "magnifier_tick_dist_volume_weighted_on",
                Terms { magnifier: true, ..Terms::PLAIN },
            )
        })
    },
    PlanEntry {
        minutes: true,
        ..plan_row("magnifier_tick_dist_endpoints_rsi_cross", END_2026_05_01, || {
            gallery_on(
                "magnifier_tick_dist_endpoints_rsi_cross",
                Terms { magnifier: true, ..Terms::PLAIN },
            )
        })
    },
];

/// The strategy `crate::desk::cost_control` runs again with a wrong commission.
pub const COST_CONTROL: PlanEntry = plan_row("gyroscope_cmo_median", END_2026_05_04, || {
    gallery_on("gyroscope_cmo_median", Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN })
});

/// The strategy the control run compares against, and a rule that must not match it: the close against SMA(151), where the
/// gallery's `sma_152_close_cross` reads SMA(152).
pub const CONTROL_NAME: &str = "sma_152_close_cross";
pub fn control_wrong_period() -> Box<dyn Rule + Send> {
    Box::new(SignalRule(WrongPeriod {
        sma: IndicatorLine::new("sma", &[151.0]),
        cross: Cross::new(),
    }))
}

struct WrongPeriod {
    sma: IndicatorLine,
    cross: Cross,
}

impl SignalSource for WrongPeriod {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let sma = self.sma.next(bar)[0];
        self.cross.step(bar.close, sma)
    }
}
