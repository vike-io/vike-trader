//! Set B's rows: each names the shipped strategy file of its rule (`gallery!`, decision 0269, W5; `strategies/` at the
//! repository root) with the row's own `Terms`. The four that read a higher bar before it closes are files too since
//! 2026-11-14 (`strategies/looks_into_the_future/`, decision 0271's amendment: a backtest serves them their inputs' whole
//! series), so no rule is compiled here. The rows whose licence the review left unsettled were deleted (2026-11-10). Adding a
//! row is one `PLANS` line, the list sorted and one line per strategy, so branches merge as a union of lines.

use crate::desk::{Plan, Terms};
use crate::sizing::margin_call;

/// A row of `PLANS`: the strategy's name, which also names its reference row, and its rule.
pub struct Listed {
    pub name: &'static str,
    pub build: fn() -> Box<dyn Plan>,
    /// Each lower series of the row's own symbol its reference read from a later bar than the lab holds, by its input's name
    /// and that bar's open in ms: the reference's platform serves a lower timeframe only so far back (`main.rs`'s rule 7).
    /// The run's store holds each from that bar on. Empty for every other row.
    pub inputs_from: &'static [(&'static str, i64)],
}

/// A row whose rule the gallery holds, by the row's name or (`=> "<name>"`) the gallery's own, run on the row's `Terms`, and
/// (`inputs_from: [..]`) the first bar of each lower series its reference read (`Listed::inputs_from`).
macro_rules! gallery {
    ($m:ident, $terms:expr) => {
        gallery!($m, $terms, inputs_from: [])
    };
    ($m:ident, $terms:expr, inputs_from: [$(($input:literal, $from:literal)),* $(,)?]) => {
        Listed {
            name: stringify!($m),
            build: || crate::desk::gallery_on(stringify!($m), $terms),
            inputs_from: &[$(($input, $from)),*],
        }
    };
    ($m:ident => $g:literal, $terms:expr) => {
        Listed { name: stringify!($m), build: || crate::desk::gallery_on($g, $terms), inputs_from: &[] }
    };
}

pub const PLANS: &[Listed] = &[
    gallery!(abcd_strategy, Terms::PLAIN),
    gallery!(accurate_buy_sell_5_mins_tf_by_rr, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        adaptive_orb_sessions,
        Terms {
            cash: 50_000.0,
            slip_ticks: 2.0,
            magnifier: true,
            shortfall_cover: margin_call(1.0, 1.0),
            account_usd: true,
            ..Terms::PLAIN
        }
    ),
    gallery!(
        adaptive_trend_flow_strategy_with_filters_for_sp,
        Terms {
            cash: 1_000.0,
            slip_ticks: 2.0,
            shortfall_cover: margin_call(20.0, 20.0),
            ..Terms::PLAIN
        }
    ),
    gallery!(advanced_position_management_mr_rakun, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        advanced_vwap_momentum_pullback_strategy_trend_t,
        Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(adx_dmi_trend, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        ai_supertrend_x_pivot_percentile_strategy_presen,
        Terms { cash: 10_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(ajay_r5_41, Terms::PLAIN),
    gallery!(all_in_one_strategy, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(alphatrend, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(alphatrend_strategy, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(atr_parabolic_sar_strategy_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(atr_rsi_strategy_v2_liwei666, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(atr_trailing_stop_strategy_by_ceyhun, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(atr_trailing_stoploss_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        atr_trend_strategy_with_moving_average_fixed_tp,
        Terms { cash: 10_000.0, slip_ticks: 2.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        aurora_kama_trend,
        Terms {
            cash: 10_000.0,
            slip_ticks: 1.0,
            shortfall_cover: margin_call(100.0, 100.0),
            ..Terms::PLAIN
        }
    ),
    gallery!(
        auto_fib_golden_pocket_band_autofib_moving_avera,
        Terms { cash: 10_000.0, slip_ticks: 2.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        automated_bitcoin_btc_investment_strategy,
        Terms { cash: 5_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(b_xtrender_backtest_edition_quanttherapy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(backtest_single_ema_cross, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        backtest_template_backtest_terminal,
        Terms { cash: 10_000.0, slip_ticks: 2.0, ..Terms::PLAIN }
    ),
    gallery!(backtesting_module, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(backtesting_on_non_standard_charts_caution_pinec => "backtesting_on_non_standard_charts_caution", Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        band_zigzag_trendfollower_strategy_trendoscope,
        Terms { cash: 1000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(
        bbtrend_w_supertrend_decision_strategy_presenttr,
        Terms { cash: 10_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(best_atr_stop_multiple_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(best_trailing_stop_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(bill_williams_alligator_improved, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        bjorgum_double_tap,
        Terms { cash: 1_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(blackcat_l2_reversal_labels_strategy, Terms::PLAIN),
    // 1m from 2026-06-05 07:46, 5m from 2025-08-31 13:10, 15m from 2025-06-01 00:00
    gallery!(
        bmm_liberty_strategy_elite,
        Terms { cash: 10_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN },
        inputs_from: [("1m", 1_780_645_560_000), ("5m", 1_756_645_800_000), ("15m", 1_748_736_000_000)]
    ),
    gallery!(bollinger_band_breakout, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(bollinger_bands_ema_9, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        bollinger_bands_enhanced_strategy,
        Terms { cash: 10_000.0, slip_ticks: 5.0, ..Terms::PLAIN }
    ),
    gallery!(bollinger_bands_filter, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(bollinger_bands_rsi_and_adx_trading_system, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(bollinger_bands_strategy_with_alerts_incl_pendin, Terms::PLAIN),
    gallery!(bollinger_stop_strategy, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(
        boom_hunter_hull_suite_volatility_oscillator_str,
        Terms { cash: 1_000.0, ..Terms::PLAIN }
    ),
    gallery!(breakout_trend_follower, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(breaks_and_retests_free990, Terms::PLAIN),
    gallery!(bsl_ssl_sweep_fvg_strategy_fixed_entry_logic, Terms::PLAIN),
    // 1m from 2026-06-18 18:46
    gallery!(btc_15_min, Terms { cash: 10_000.0, ..Terms::PLAIN }, inputs_from: [("1m", 1_781_808_360_000)]),
    gallery!(btc_bot, Terms { cash: 10_000.0, whole_entry_fee: true, ..Terms::PLAIN }),
    gallery!(
        btcusd_supertrend_ema_trend_filter_1h,
        Terms { cash: 10_000.0, slip_ticks: 1.0, ..Terms::PLAIN }
    ),
    gallery!(btfd_strategy_3min, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(
        built_in_kelly_ratio_for_position_sizing,
        Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        buy_sell_bullish_engulfing_strategy_the_quant_sc,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        buy_sell_strategy_depends_on_ao_stoch_rsi_atr_by,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        buy_sell_strategy_with_z_score_tradedots,
        Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(buythedip, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(camarilla_pivot_points_v2_backtest, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(captain_backtest_model_tfo, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(
        cci_rsi_ema_strategy_with_percentage_or_atr_tp_s,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(chandelier_exit_strategy, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(chandelier_exit_zlsma_strategy, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(chop_zone_entry_strategy_dmi_psar_exit, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(combo_2_20_ema_accelerator_oscillator_ac, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(combo_backtest_123_reversal_rsi, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        combo_backtest_123_reversal_smart_money_index_sm,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        combo_backtest_123_reversal_statistical_volatili,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(coral_trend_pullback_strategy_tradeiq, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        crossing_moving_averages_with_adx_filter,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(crypto_crew_university, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(crypto_scalper, Terms { cash: 100.0, ..Terms::PLAIN }),
    gallery!(crypto_zigzag_rsi_strategy_15min, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(custom_screener_with_alerts_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    // 5m from 2025-10-19 03:45.
    gallery!(
        customizable_htf_macd_strategy_v1_2,
        Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN },
        inputs_from: [("5m", 1_760_845_500_000)]
    ),
    // 5m from 2025-10-11 12:50.
    gallery!(
        customizable_htf_macd_strategy_v2_0,
        Terms { cash: 1_000_000.0, account_usd: true, ..Terms::PLAIN },
        inputs_from: [("5m", 1_760_187_000_000)]
    ),
    gallery!(d_rock_s_ma_strategy, Terms { cash: 25_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        davin_s_10_200ma_pullback_on_spy_strategy_v2_0,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(dax_shooter_5m_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(dca, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(dca_bot_emulator, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(delta_rsi_strategy_with_filters, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        dema_atr_strategy_primeautomation,
        Terms { cash: 10_000.0, whole_entry_fee: true, ..Terms::PLAIN }
    ),
    gallery!(divergence_for_many_indicators_v4_st, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    // 1m from 2026-07-21 20:02.
    gallery!(
        divergence_iq_tradingiq,
        Terms {
            cash: 1_000.0,
            slip_ticks: 1.0,
            shortfall_cover: margin_call(100.0, 100.0),
            ..Terms::PLAIN
        },
        inputs_from: [("1m", 1_784_664_120_000)]
    ),
    gallery!(donchain_bo, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(double_bottom_and_top_hunter, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(
        double_exponential_moving_average_8_20_63_strate,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(double_triple_pattern_ts_indie, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(ds_entry_exit_trade_v01_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(dual_moving_average, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(dynamic_rsi_momentum, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(easy_sys_420, Terms { cash: 100_000.0, slip_ticks: 1.0, tick: 0.01, ..Terms::PLAIN }),
    gallery!(
        edma_scalping_strategy_exponentially_deviating_m,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        ee_sweep_return_strategy_v1,
        Terms { cash: 50_000.0, slip_ticks: 1.0, ..Terms::PLAIN }
    ),
    gallery!(elder_ray_bull_power_tp_and_sl, Terms::PLAIN),
    gallery!(
        elliott_wave_with_supertrend_exit_strategy_prese,
        Terms { cash: 10_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(ema_cross_rsi_adx_strategy_v2, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(ema_crossover_strategy, Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(ema_crossover_strategy_with_take_profit_and_cand, Terms::PLAIN),
    gallery!(ema_long_only_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        ema_rsi_atr_hidden_div_strat,
        Terms { cash: 5_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(ema_scalp, Terms::PLAIN),
    gallery!(ema_slope_rsi_oscilator_strategy, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(ema_slope_trend_follower, Terms { cash: 10_000.0, slip_ticks: 2.0, ..Terms::PLAIN }),
    gallery!(emperorbtc_s_vwap_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(enhanced_ob_retest_strategy_v7_0, Terms::PLAIN),
    gallery!(eurusd_1min_strat_risk, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(evwma_vwap_cross_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(evwma_vwap_macd_strategy_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        fib_confluence_scanner,
        Terms {
            cash: 10_000.0,
            shortfall_cover: margin_call(100.0, 100.0),
            whole_entry_fee: true,
            ..Terms::PLAIN
        }
    ),
    gallery!(fibonacci_rsi_strategy, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        fibonacci_trend_reversals,
        Terms { cash: 1_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(fixed_percent_stop_loss_take_profit, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        flawless_victory_strategy,
        Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        flexisupertrend_strategy_presenttrading,
        Terms { cash: 10_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        flytv_a_real_fruit_fly_connectome_trading_on_you,
        Terms { cash: 10_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(forex_fractal_ema_scalper, Terms::PLAIN),
    gallery!(fractal_breakout_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(fractal_strat_kl, Terms { cash: 1_000_000_000.0, ..Terms::PLAIN }),
    gallery!(fs_atr_ps_ma, Terms { cash: 100.0, ..Terms::PLAIN }),
    gallery!(
        gaussian_channel_strategy,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(
        gold_intraday_ema_bb_vwap_atr_sl_tp_session_week,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(gold_scalping_strategy_with_precise_entries, Terms::PLAIN),
    gallery!(golden_triangle_strategy, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        golden_trident_swing_anchored_vwap_trend_system,
        Terms { cash: 10_000.0, slip_ticks: 2.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        golden_vector_trend_orchestrator_gvto,
        Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(grab_trading_system, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(gradient_trend_filter_strategy_chartprime_pinein => "gradient_trend_filter_strategy_chartprime", Terms { cash: 10_000.0, slip_ticks: 2.0, ..Terms::PLAIN }),
    gallery!(heikin_ashi_supertrend, Terms { cash: 100.0, ..Terms::PLAIN }),
    // 5m from 2025-10-01 00:00.
    gallery!(
        heikin_kaufman_non_repaint,
        Terms { cash: 100_000.0, ..Terms::PLAIN },
        inputs_from: [("5m", 1_759_276_800_000)]
    ),
    gallery!(hhll, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(high_low_channel_swing, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        higher_high_lower_low_strategy,
        Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        highest_high_and_lowest_low_channel_strategy,
        Terms { cash: 1_000_000.0, ..Terms::PLAIN }
    ),
    gallery!(how_to_set_time_ranges, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(how_to_use_leverage_and_margin_in_pinescript => "how_to_use_leverage_and_margin", Terms { cash: 100_000.0, shortfall_cover: margin_call(50.0 / 30.0, 50.0 / 30.0), ..Terms::PLAIN }),
    gallery!(htf_high_low_repaint_strategy, Terms { cash: 20_000.0, ..Terms::PLAIN }),
    gallery!(
        hulk_grid_algorithm_v2_the_quant_science,
        Terms {
            cash: 10_000.0,
            slip_ticks: 5.0,
            shortfall_cover: margin_call(100.0, 100.0),
            ..Terms::PLAIN
        }
    ),
    // 1m from 2026-06-14 22:50.
    gallery!(
        hulk_scalper_x35_leverage,
        Terms { cash: 100.0, ..Terms::PLAIN },
        inputs_from: [("1m", 1_781_477_400_000)]
    ),
    gallery!(hull_fisher, Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        hull_moving_average_vs_candle,
        Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN }
    ),
    gallery!(hull_suite_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        ichimoku_backtester_with_optional_rsi_tp_and_sl,
        Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(ichimoku_cloud, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        ichimoku_daily_candle_x_hull_ma_x_macd,
        Terms { cash: 10_000.0, slip_ticks: 1.0, ..Terms::PLAIN }
    ),
    gallery!(ichimoku_with_macd_cmf_tsi, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(ict_entry_v1_ts_indie, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(ict_entry_v2_ts_indie, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(inside_bar_strategy_w_sl, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(
        intellect_city_world_cycle_ath_atl_timeframe_1d,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        investments_swing_trading_strategy_for_different,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(inwcoin_martingale_strategy_for_bitcoin, Terms { cash: 20_000.0, ..Terms::PLAIN }),
    gallery!(
        joint_conditions_strategy_suite_tradingconnector,
        Terms { cash: 1_000_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(kahlman_hullma_wt_cross_strategy, Terms { cash: 100.0, ..Terms::PLAIN }),
    gallery!(kalki_tfxbot, Terms { cash: 100.0, ..Terms::PLAIN }),
    gallery!(
        kama_strategy_kaufman_s_adaptive_moving_average,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        keltner_channel_eth_usdt_1h,
        Terms { cash: 1_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(kitchen_ilovealgotrading, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(kpl_swing_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(ksr_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        lanz_strategy_2_0_backtest,
        Terms { shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(lanz_strategy_4_0_backtest, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(larry_connors_strategy_josemetal, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(linear_regression_channel_breakout_strategy, Terms::PLAIN),
    gallery!(liquidity_sweep_breakout_lsb, Terms::PLAIN),
    gallery!(liquidity_sweep_filter_strategy_algoalpha_x_pine => "liquidity_sweep_filter_strategy_algoalpha", Terms { cash: 100_000.0, slip_ticks: 2.0, ..Terms::PLAIN }),
    gallery!(
        liquidity_sweep_reversal_strategy,
        Terms { cash: 10_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(loft_strategy_v4, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(london_breakout_classic, Terms::PLAIN),
    gallery!(lube, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(luxalgo_smc_pro_ultimate, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(ma_10_20_crossover, Terms::PLAIN),
    gallery!(ma_bollinger_bands_rsi_strategy, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(ma_candles_supertrend_strategy, Terms { cash: 20_000.0, ..Terms::PLAIN }),
    gallery!(macandles_linearregression_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(macd_ashi_bars_w_th_sl_tp, Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN }),
    gallery!(macd_bb_rsi_alorse, Terms { cash: 1_000.0, account_usd: true, ..Terms::PLAIN }),
    // 15m from 2025-08-01 00:00.
    gallery!(
        macd_ema_strategy,
        Terms { cash: 5_000.0, account_usd: true, ..Terms::PLAIN },
        inputs_from: [("15m", 1_754_006_400_000)]
    ),
    gallery!(
        macd_liquidity_tracker_strategy_quant_trading,
        Terms { cash: 1_000.0, slip_ticks: 3.0, ..Terms::PLAIN }
    ),
    gallery!(
        macd_pullback_sniper_trend_adx_filtered_strategy,
        Terms {
            cash: 10_000.0,
            slip_ticks: 2.0,
            shortfall_cover: margin_call(100.0, 100.0),
            ..Terms::PLAIN
        }
    ),
    gallery!(macd_reloaded, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(macd_rsi, Terms { cash: 100.0, ..Terms::PLAIN }),
    gallery!(macd_rsi_rvol_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        macd_strategy,
        Terms { cash: 100.0, whole_entry_fee: true, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        market_trend_levels_non_repainting_bigbeluga_x_p,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(maximized_scalping_on_trend_by_coinrule, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(mcginley_t3_flow_campaign_nick789, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(monthly_returns_in_pinescript_strategies => "monthly_returns_in_strategies", Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(mov_avg_speed_strategy, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(moving_average_displaced_envelope_atrts, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(moving_average_scalper, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(mtf_layered_rsi_bitcoin_bot_wbburgin, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(mtf_screener_ma_cross_system, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(mtf_trailing_sl_strategy_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        multi_step_flexisupertrend_strategy_presenttradi,
        Terms { cash: 10_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        multi_supertrend_with_no_repaint_htf_option_stra,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    // 5m from 2025-09-10 23:05, 15m from 2025-07-01 00:00.
    gallery!(
        multi_timeframe_rsi_buy_sell_strategy_tradedots,
        Terms { cash: 10_000.0, ..Terms::PLAIN },
        inputs_from: [("5m", 1_757_545_500_000), ("15m", 1_751_328_000_000)]
    ),
    gallery!(my_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(nadaraya_watson_envelope_strategy, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(non_repainting_renko_emulation_strategy_pineindi => "non_repainting_renko_emulation_strategy", Terms { cash: 10_000.0, slip_ticks: 2.0, ..Terms::PLAIN }),
    gallery!(nononsense_forex, Terms { cash: 100.0, ..Terms::PLAIN }),
    gallery!(noro_s_trend_ribbon_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(noro_s_trendma_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        on_balance_volume_oscillator_strategy_quantnomad,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    // 5m from 2026-07-13 00:00; the 90-minute frame is built from the 30-minute bars from the window on.
    gallery!(
        open_open_cross_strategy_r7_1b_revised_by_mz,
        Terms { cash: 1_000_000.0, ..Terms::PLAIN },
        inputs_from: [("5m", 1_783_900_800_000)]
    ),
    gallery!(opening_range_breakout_orb_heikin_ashi_spy_5min, Terms::PLAIN),
    gallery!(optimized_heikin_ashi_strategy_with_buy_sell_opt, Terms { cash: 1.0, ..Terms::PLAIN }),
    gallery!(
        optimized_rsi_strategy_buy_the_dips_by_coinrule,
        Terms { cash: 1_000.0, ..Terms::PLAIN }
    ),
    gallery!(optimized_trend_tracker, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(orb_strategy_lucitech, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(parabolic_sar_strategy_on_close_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        peri_bollinger_mean_reversion_v2,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(
        pivot_of_pivot_reversal_strategy_quantnomad,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(pivot_point_reversal_rsi_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(pivot_point_supertrend_backtest, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        pivot_reversal_strategy_alerts_via_tradingconnec,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(planb_quant_investing_101, Terms { cash: 1_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(pmax_explorer, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(pmax_on_rsi_w_t3_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(pps, Terms { cash: 500_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        precision_confluence_trading_strategy_joat,
        Terms {
            cash: 10_000.0,
            slip_ticks: 2.0,
            shortfall_cover: margin_call(100.0, 100.0),
            ..Terms::PLAIN
        }
    ),
    gallery!(
        previous_day_high_and_low_breakout_strategy,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        price_and_volume_breakout_buy_strategy_tradedots,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(price_ema_with_stock, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(profit_maximizer_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        profit_maximizer_strategy_long_short,
        Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        pt_premarket_breakout_strategy,
        Terms { cash: 4_000.0, slip_ticks: 3.0, ..Terms::PLAIN }
    ),
    gallery!(pts_golden_edge, Terms::PLAIN),
    gallery!(qqe_mod_ssl_hybrid_waddah_attar_explosion, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(quantnomad_heikin_ashi_psar_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    // 5m from 2025-10-19 11:15.
    gallery!(
        quantumforextrader_solusdt_sniperfusion_strategy,
        Terms {
            cash: 10_000.0,
            slip_ticks: 2.0,
            shortfall_cover: margin_call(100.0, 100.0),
            ..Terms::PLAIN
        },
        inputs_from: [("5m", 1_760_872_500_000)]
    ),
    gallery!(
        quasimodo_pattern_strategy_back_test_tradingfind,
        Terms { cash: 1_000.0, ..Terms::PLAIN }
    ),
    gallery!(qullamaggie_breakout_v2, Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        rally_base_drop_snd_pivots_strategy_luxalgo_x_pi,
        Terms { cash: 100_000.0, slip_ticks: 2.0, ..Terms::PLAIN }
    ),
    gallery!(random, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(range_strat_macd_rsi, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(rci_strategy_pineindicators => "rci_strategy", Terms { cash: 100_000.0, slip_ticks: 2.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }),
    gallery!(
        reversal_trading_bot_strategy_bullbyte_final_ver,
        Terms { cash: 1_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(rob_booker_adx_breakout, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        rsi_adaptive_t3_sar_strategy_primeautomation,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(rsi_and_sma, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        rsi_and_smoothed_rsi_bull_div_strategy_bigbitsio,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(rsi_crossover_strategy, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(rsi_crossover_strategy_with_compounding_monthly, Terms::PLAIN),
    gallery!(rsi_divergence_indicator, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(rsi_divergence_strategy, Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        rsi_macd_multi_timeframe_strategy,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(rsi_on_vwap_upgraded_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(rsi_strategy_primeautomation, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(
        rsi_strategy_with_alerts_via_tradingconnector_to,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(rsi_trend_crypto, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(rsi_trend_reversal, Terms::PLAIN),
    gallery!(rsi_vwap_indicator, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(rsi_w_pattern_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        s_20_200_sma_bot_strategy_clean_usable_version,
        Terms { cash: 50_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        s_2mars_ma_bb_supertrend,
        Terms {
            cash: 10_000.0,
            shortfall_cover: margin_call(100.0, 100.0),
            account_usd: true,
            ..Terms::PLAIN
        }
    ),
    gallery!(s_2xtp_sl_to_entry, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        s_3_ema_stochastic_rsi_atr,
        Terms { cash: 1_000.0, slip_ticks: 2.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        s_3ls_3_line_strike_strategy_kintsugi_trading,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(s_5_ema_strategy, Terms::PLAIN),
    gallery!(s_5min_macd_scalp_by_joel_3commas => "s_5min_macd_scalp_by_joel", Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(
        s_72s_strategy_adaptive_hull_moving_average_pt_1,
        Terms { cash: 1_000.0, slip_ticks: 3.0, ..Terms::PLAIN }
    ),
    gallery!(s_888_bot_backtest, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(scalping_against_trapped_countertrend, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        scalping_trading_system_crypto_and_stocks,
        Terms { cash: 1_000_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        simple_and_profitable_scalping_strategy_forexsig,
        Terms { cash: 1_000.0, ..Terms::PLAIN }
    ),
    gallery!(simple_harmonic_pattern_strategy, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(simple_rsi_strategy, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(sl1_pips_after_tp1_ma, Terms { cash: 100.0, ..Terms::PLAIN }),
    gallery!(
        smart_grid_scalping_pullback_strategy_bullbyte,
        Terms { cash: 100_000.0, slip_ticks: 1.0, ..Terms::PLAIN }
    ),
    gallery!(smart_money_concept_uncle_sam, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(smart_money_pivot_strategy_jason_kasei, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(
        smc_strategy_with_premium_equilibrium_and_discou,
        Terms { cash: 1_000_000.0, ..Terms::PLAIN }
    ),
    gallery!(so_trade_top_me_dia_de_8_augusto_backes, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    // 15m from 2025-08-01 00:00.
    gallery!(
        solution_zigma,
        Terms { cash: 1_000_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN },
        inputs_from: [("15m", 1_754_006_400_000)]
    ),
    gallery!(sr_trend_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(ssl_channel_cross, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(ssl_exit_arrow_strategy, Terms { cash: 5_000.0, ..Terms::PLAIN }),
    gallery!(state_aware_ma_cross_strategy, Terms::PLAIN),
    gallery!(stepped_trailing_strategy_example, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(stochastic_optimized_trend_tracker, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(stochastic_trend, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(stop_loss_and_take_profit_in_example, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        strategy_follow_line_mtf_any_mm_average,
        Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(strategy_hma_50_supertrend_sniper, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(strategy_macd_vs_bb, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        strategy_myth_busting_12_osgfc_supertrend_myn,
        Terms { cash: 1_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        strategy_myth_busting_1_ut_bot_stc_hull_myn,
        Terms { cash: 500.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        strategy_rs_open_session_breakout_trader,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(strategy_template_v2, Terms { cash: 20_000.0, ..Terms::PLAIN }),
    gallery!(super_guppy_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(super_scalper_5_min_15_min, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(super_trend_v_strategy_version, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(supertrend_advance, Terms::PLAIN),
    gallery!(
        supertrend_ai_strategy_adaptive,
        Terms { cash: 10_000.0, slip_ticks: 2.0, ..Terms::PLAIN }
    ),
    gallery!(
        supertrend_atr_with_trailing_stop_loss,
        Terms { cash: 1_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(supertrend_backtester, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(supertrend_cloud_strategy, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        supertrend_multi_time_frame_long_and_short_tradi,
        Terms { cash: 2_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        supertrend_regime_confluence,
        Terms { cash: 10_000.0, slip_ticks: 2.0, ..Terms::PLAIN }
    ),
    gallery!(supertrend_strategy, Terms { cash: 10_000_000.0, ..Terms::PLAIN }),
    gallery!(supertrend_strategy_rcrbto, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(supertrex_strategy, Terms { cash: 10_000.0, whole_entry_fee: true, ..Terms::PLAIN }),
    gallery!(supply_demand_zones_order_block_pro_fusion_auto, Terms::PLAIN),
    gallery!(support_resistance_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(take_profit_multi_timeframe, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(tasc_2026_03_one_percent_a_week, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        the_flash_strategy_momentum_rsi_ema_crossover_at,
        Terms { cash: 1_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        ticktock_ut_entry_signal_stop,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(
        tradeiq_crazy_scalping_trading_strategy_kaspricc,
        Terms { cash: 1_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        tradepro_s_trading_idea_cipher_b_divergence_ema,
        Terms { cash: 5_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        tradingview_alerts_to_mt4_mt5_forex_indices_comm => "alerts_to_mt4_mt5_forex_indices_comm",
        Terms { cash: 1_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        tradingview_alerts_to_mt4_mt5_strategy_example => "alerts_to_mt4_mt5_strategy_example",
        Terms { cash: 1_000.0, ..Terms::PLAIN }
    ),
    gallery!(trailing_sl_strategy_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(trailing_take_profit, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        trailing_take_profit_trailing_stop_loss,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(traling_sl_target, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(trend_direction_helper, Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(trend_surfers_breakout_alerts, Terms { cash: 2_000.0, ..Terms::PLAIN }),
    gallery!(
        trend_surfers_premium_breakout,
        Terms { cash: 100_000.0, committed_exits: true, ..Terms::PLAIN }
    ),
    gallery!(
        trend_trader_remastered_strategy,
        Terms {
            cash: 10_000.0,
            shortfall_cover: margin_call(100.0, 100.0),
            whole_entry_fee: true,
            account_usd: true,
            ..Terms::PLAIN
        }
    ),
    gallery!(trendline_breakout_strategy_kedarc_quant, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(trendlines_with_alerts, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        trendmaster_pro_2_3_with_alerts,
        Terms { shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(triangular_hull_moving_average_bigbeluga_x_pinei => "triangular_hull_moving_average_bigbeluga", Terms { cash: 1_000.0, slip_ticks: 2.0, ..Terms::PLAIN }),
    gallery!(
        triple_cci_strategy_mfi_confirmed_skyrexio,
        Terms { cash: 10_000.0, slip_ticks: 5.0, ..Terms::PLAIN }
    ),
    gallery!(
        triple_ema_qqe_trend_following_strategy_tradedot,
        Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(triple_ema_rsi_atr, Terms { cash: 1_000_000.0, slip_ticks: 3.0, ..Terms::PLAIN }),
    gallery!(triple_ema_scalper, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(triple_ema_trend, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    // 15m from 2025-07-01 00:00.
    gallery!(
        tripple_super_trend_strategy,
        Terms { cash: 5_000.0, account_usd: true, ..Terms::PLAIN },
        inputs_from: [("15m", 1_751_328_000_000)]
    ),
    gallery!(
        tristan_s_multi_indicator_reversal_strategy,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(tsi_cci_hull, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(tsi_cci_hull_uqfaeu, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(tsihullbot, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(turtle_system, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(turtle_trading_strategy_donchian_atr, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(twin_optimized_trend_tracker, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(
        two_bar_fib_retrace_strategy_futures,
        Terms {
            cash: 100_000.0,
            slip_ticks: 1.0,
            shortfall_cover: margin_call(100.0, 100.0),
            ..Terms::PLAIN
        }
    ),
    gallery!(u_c_hareketli_ortalama_str, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(uhl_ma_system_strategy_analysis, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        ultimate_bullish_divergence_for_rsi_mfi_rvsi_obv,
        Terms { cash: 1_000.0, slip_ticks: 2.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        ultimate_prop_firm_artillery,
        Terms {
            cash: 10_000.0,
            slip_ticks: 1.0,
            shortfall_cover: margin_call(5.0, 5.0),
            ties_in_placement_order: true,
            ..Terms::PLAIN
        }
    ),
    gallery!(
        universal_breakout_strategy_kedarc_quant,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(ut_bot_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(ut_bot_strategy_vynmpw, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        ut_bot_v2_atr_trailing_stop,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(ut_bot_v5, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(
        v2_0_tristan_s_multi_indicator_reversal_strategy,
        Terms { cash: 100_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(
        v_5_3_ema_rsi_with_trail_stop,
        Terms { cash: 1_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        vegas_supertrend_enhanced_strategy_presenttradin,
        Terms { cash: 10_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        volume_confirmed_support_resistance_breakout_str,
        Terms { cash: 10_000.0, slip_ticks: 2.0, ..Terms::PLAIN }
    ),
    gallery!(volume_difference_delta_cycle_oscillator, Terms::PLAIN),
    gallery!(
        volume_vigilante_long_only_opening_range_breakou,
        Terms { cash: 10_000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(vwap_fibo_dev_extensions_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(vwap_trendfollow_strategy_wbburgin, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        vwma_with_knn_machine_learning_mfi_adx_by_lastgu,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(vxd_cloud, Terms { cash: 100.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        wolfe_strategy_trendoscope,
        Terms { cash: 1000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(
        wombo_combo_100_200ema_vwap_macd,
        Terms { cash: 20_000.0, slip_ticks: 2.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(wyckoff_range_strategy_by_deperp, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(x48_strategy_channel_breakout_consecutive_17in1, Terms { cash: 50.0, ..Terms::PLAIN }),
    gallery!(
        x48_strategy_ma_multi_type_cross_tpsl_swing_surf,
        Terms { cash: 200.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(xauusd_scalping_v2_agresiv, Terms::PLAIN),
    gallery!(yinyang_rsi_volume_trend_strategy, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(zendog_v2_backtest_dca_bot_3commas => "zendog_v2_backtest_dca_bot", Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(zendog_v3_backtest_dca_bot_3commas => "zendog_v3_backtest_dca_bot", Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(zero_lag_ma_trend_following, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
];

/// A row the owner accepted on numbers that miss its reference by more than the tolerance, its run's numbers recorded: it
/// PASSES on exactly these, its closed trades and its net to the bit, and FAILS on any other (a run that moves fails, and so
/// does one that comes to match its reference: the row then leaves this list). The tolerance every other row is judged by
/// is not widened (decision 0021). Its readings take part in no control: a reading that misses a reference the row itself
/// misses refutes nothing.
pub struct Accepted {
    pub name: &'static str,
    pub trades: usize,
    pub net: f64,
    /// Why, and the owner's dated ruling, as the row's line prints it.
    pub why: &'static str,
}

impl Accepted {
    /// Whether a run gave exactly the recorded numbers, the net to the bit.
    pub fn holds(&self, trades: usize, net: f64) -> bool {
        trades == self.trades && net.to_bits() == self.net.to_bits()
    }
}

/// The rows the owner accepted, sorted by name.
pub const ACCEPTED: &[Accepted] = &[
    // The reference's count, 669, and a net 0.023 over its -1771.633667 (BTCUSDT 30 minutes, a USD account), with the
    // magnifier on (`Terms::magnifier`, the reference's setting) and the same to the bit on each bar's own walk (no fill
    // moves, the dumps identical): booked in USDT it is -1770.56, and one day's USDT-USD close moved by a tick brings it
    // inside the tolerance, so the miss is the USD account's conversion (IMP-I8b, 2026-11-15; INT, 2026-11-16). Imported with
    // this net on the owner's ruling of 2026-11-16 ("ADD"), which covers both runs.
    Accepted {
        name: "adaptive_orb_sessions",
        trades: 669,
        net: -1771.6102973033521,
        why: "owner 2026-11-16: net 0.023 over, the USD conversion; the magnifier moves no fill",
    },
    // The reference's count, 180, and a net 0.195 under its -303305.40625 (BTCUSDT 30 minutes, a USD account): a replay of
    // its fills at the USDT-USD rates gives this net, and a rate 0.00001 off on one of six days closes the gap, so likely
    // the conversion (IMP-I4, 2026-11-16). Imported with this net on the owner's ruling of 2026-11-16 ("ADD"); the owner's
    // rule for the USD rate, when it lands, re-measures it.
    Accepted {
        name: "customizable_htf_macd_strategy_v2_0",
        trades: 180,
        net: -303305.60088526463,
        why: "owner 2026-11-16: net 0.195 under, likely the USD conversion",
    },
    // One trade more than the reference's 234 and a net 2.02 over its -94.374191 (Bybit WIFUSDT perpetual, 5 minutes). Not the
    // margin call: every cut equals a twin of the reference's rules (SIM-MARGIN-SHORT, 2026-11-16). The lab runs on the
    // venue's tick of today, 0.0001, where the window's bars had 0.00001 (a short's first target fills 0.00007 off on
    // 2026-07-23 03:10), and the file's trend line ties the open on 2026-08-14 19:05 by a float (0.1359 against
    // 0.13590000000000002) where the reference does not enter. Imported with these numbers on the owner's ruling of
    // 2026-11-15 ("ADD").
    Accepted {
        name: "fibonacci_trend_reversals",
        trades: 235,
        net: -92.3492218839998,
        why: "owner 2026-11-15: one trade more, net 2.02 over; today's 0.0001 tick, a float tie",
    },
    // The reference's count, 1229, and a net 1.34 over its -92.767136 (30 minutes): the reference ran on CAPITALCOM:EURUSD, a CFD feed, and this row on Dukascopy's EURUSD, its bars of the mid price (on
    // the bid side 1221 and -94.566015), another feed's prices (IMP-I7a, 2026-11-16). Imported with these numbers on the
    // owner's ruling of 2026-11-16 ("ARE MATCHED").
    Accepted {
        name: "fs_atr_ps_ma",
        trades: 1229,
        net: -91.43025499999938,
        why: "owner 2026-11-16: net 1.34 over, another feed (Dukascopy EURUSD, mid)",
    },
    // One trade more than the reference's 1342 and a net 735.38 over its 33429.488281 (BTCUSDT 4 hours), the same with its
    // exits unbound; vike's ALMA does not floor its offset (floored to a whole bar it gives 1348 and 28329.27); no cause found
    // (IMP-I5, 2026-11-15). Imported with these numbers on the owner's ruling of 2026-11-15 ("ADD").
    Accepted {
        name: "high_low_channel_swing",
        trades: 1343,
        net: 34164.87000000006,
        why: "owner 2026-11-15: one trade more, net 735.38 over, no cause found",
    },
    // The reference's count, 95, and a net 2.77 over its 301.911896 (LTCUSDT 30 minutes) at the venue's quantity step 0.001;
    // no cause found (IMP-I5, 2026-11-15). Imported with this net on the owner's ruling of 2026-11-15 ("ADD").
    Accepted {
        name: "hulk_grid_algorithm_v2_the_quant_science",
        trades: 95,
        net: 304.6836600100001,
        why: "owner 2026-11-15: net 2.77 over, no cause found",
    },
    // The reference's count, 47, and a net 229.51 over its -1039.028198 (NASDAQ:AAPL 30 minutes from 2020-01-02): the
    // reference ran on the exchange's share, this row on Dukascopy's AAPL.CFD, read split-adjusted for the 4:1 split of
    // 2020-08-31 by the operator's split row (decision 0289; unadjusted it was 46 and -1369.98), another feed's price levels
    // (measured 2026-11-17). Imported with these numbers on the owner's ruling of 2026-11-17.
    Accepted {
        name: "joint_conditions_strategy_suite_tradingconnector",
        trades: 47,
        net: -809.5157000000052,
        why: "owner 2026-11-17: net 229.51 over, another feed's price levels (Dukascopy AAPL.CFD, split-adjusted)",
    },
    // Three trades more than the reference's 38 and a net 34.83 over its 140.753326 (30 minutes): the reference ran on CAPITALCOM:EURUSD, a CFD feed, and this row on Dukascopy's EURUSD, its bars of the bid
    // (on the mid price 45 and 135.736705), another feed's prices; the held ticks have none from 2026-09-23 00:00 to 06:32
    // UTC, inside the window (IMP-I7a, 2026-11-16). Imported with these numbers on the owner's ruling of 2026-11-16 ("ARE
    // MATCHED").
    Accepted {
        name: "lanz_strategy_2_0_backtest",
        trades: 41,
        net: 175.5791900000006,
        why: "owner 2026-11-16: three trades more, net 34.83 over, another feed (Dukascopy EURUSD, bid), a hole on 2026-09-23",
    },
    // The reference's count, 183, and a net 501.12 over its 4464607 (BTCUSDT 4 hours, a USD account trading from 2021-01), its
    // USDT converted at Coinbase's USDT-USD and one-to-one before that series' first day, 2021-05-04 (the owner's ruling of
    // 2026-11-16, decision 0243 amended); booked in USDT throughout it is 4465760.05. The 30 rows before 2021-05-04 carry the
    // gap (IMP-D3C). Imported with this net on the owner's ruling of 2026-11-16 ("OK" at 4,465,108 against 4,464,607).
    Accepted {
        name: "loft_strategy_v4",
        trades: 183,
        net: 4465108.115756779,
        why: "owner 2026-11-16: net 501.12 over, USDT one-to-one before 2021-05-04",
    },
    // The reference's count, 61, and a net 644.74 over its -2196.727295 (DOGEUSDT 5 minutes, its daily input), about 6 USDT a
    // tick on a trade: a replay outside the engine gives this net to the last decimal and no stop and limit grid reading at the
    // 0.00001 tick reaches the reference's (IMP-I5, 2026-11-15). Imported with this net on the owner's ruling of 2026-11-15
    // ("ADD").
    Accepted {
        name: "pt_premarket_breakout_strategy",
        trades: 61,
        net: -1551.9829000001087,
        why: "owner 2026-11-15: net 644.74 over, no cause found",
    },
    // The owner's ruling of 2026-11-15 ("if 5 cents - it is ok!!"): 225 trades = 225, net -35834.595117 against the
    // reference's -35834.542969 (0.052148 off, 12 times the tolerance), the same to the digit on the lab of 2026-11-09 and on
    // the whole series of decision 0271's item 9. The miss is the USD account's conversion (the USDT-USD rate of a few days),
    // not the rule: booked in USDT it misses by far.
    Accepted {
        name: "qullamaggie_breakout_v2",
        trades: 225,
        net: -35834.59511672929,
        why: "owner 2026-11-15: 5 cents off on the USD conversion",
    },
    // 197 trades more than the reference's 5738 and a net 9532.93 over its -77588.953125 (BTCUSDT 4 hours, mean-rank ties,
    // a 100% margin on both sides). Not the margin call: every cut equals a twin of the reference's rules
    // (SIM-MARGIN-SHORT, 2026-11-16). On 2017-08-27 08:00 the margin check refuses the long's reversing entry, and the
    // reference then also cancels the rule's same-bar close of the short, which this run fills; on 2017-10-10 08:00 the
    // rank correlation equals its 14-bar average (900/11), which vike's running-sum average puts one ulp under, so only
    // the reference sees the cross. Imported with these numbers on the owner's ruling of 2026-11-15 ("ADD").
    Accepted {
        name: "rci_strategy_pineindicators",
        trades: 5935,
        net: -68056.01833471115,
        why: "owner 2026-11-15: 197 trades more, net 9532.93 over; a refused reversal's close, an average one ulp under",
    },
    // Two trades fewer than the reference's 472 and a net 8.47 over its -583.517395: every trade from 2025-01-04 10:00 on is
    // the same under each reading of the T3 chain's early state, the reference has two more closed in the first three days,
    // and some 40 readings of that state reach neither (IMP-I4, 2026-11-16; set B L3 batch 4 measured the same 470).
    // Imported with these numbers on the owner's ruling of 2026-11-16 ("ADD").
    Accepted {
        name: "rsi_adaptive_t3_sar_strategy_primeautomation",
        trades: 470,
        net: -575.0477504515395,
        why: "owner 2026-11-16: two trades fewer, net 8.47 over, the T3 chain's early state",
    },
    // One trade more than the reference's 707 and a net 4.17 over its -626.084717: the entry is a stop order at the stop
    // price, already through the market, read as filling at the bar's close under process-on-close; no other reading was
    // run (IMP-I8, 2026-11-15). Imported with these numbers on the owner's ruling of 2026-11-15 ("ADD").
    Accepted {
        name: "s_2mars_ma_bb_supertrend",
        trades: 708,
        net: -621.9131371029532,
        why: "owner 2026-11-15: one trade more, net 4.17 over, no cause found",
    },
    // One trade more than the reference's 872 and a net 67.60 over its -187.600006 (30 minutes): the reference ran on CAPITALCOM:EURUSD, a CFD feed, and this row on Dukascopy's EURUSD, its bars of the mid
    // price (on the bid side 877 and -182.2), another feed's prices (IMP-I7a, 2026-11-16). Imported with these numbers on
    // the owner's ruling of 2026-11-16 ("ARE MATCHED").
    Accepted {
        name: "strategy_rs_open_session_breakout_trader",
        trades: 873,
        net: -120.00000000000719,
        why: "owner 2026-11-16: one trade more, net 67.60 over, another feed (Dukascopy EURUSD, mid)",
    },
    // 16 trades more than the reference's 4381 and a net 464.10 over its -6000.899902 (4 hours): the reference ran on CAPITALCOM:EURUSD, a CFD feed, and this row on Dukascopy's EURUSD, its bars of the mid
    // price (on the bid side 4400 and -6048.1), another feed's prices (IMP-I7a, 2026-11-16). Imported with these numbers on
    // the owner's ruling of 2026-11-16 ("ARE MATCHED").
    Accepted {
        name: "tradingview_alerts_to_mt4_mt5_forex_indices_comm",
        trades: 4397,
        net: -5536.800000000029,
        why: "owner 2026-11-16: 16 trades more, net 464.10 over, another feed (Dukascopy EURUSD, mid)",
    },
    // One trade fewer than the reference's 8038 and a net 486.00 over its 4515.743164 (30 minutes): the reference ran on CAPITALCOM:EURUSD, a CFD feed, and this row on Dukascopy's EURUSD, its bars of the
    // mid price (on the bid side 8001 and 4361.743995), another feed's prices (IMP-I7a, 2026-11-16). Imported with these
    // numbers on the owner's ruling of 2026-11-16 ("ARE MATCHED").
    Accepted {
        name: "tradingview_alerts_to_mt4_mt5_strategy_example",
        trades: 8037,
        net: 5001.7427049983835,
        why: "owner 2026-11-16: one trade fewer, net 486.00 over, another feed (Dukascopy EURUSD, mid)",
    },
    // The reference's count, 1308, and a net 1158.19 over its -22222.197266 (5.2%): no stop or trail fills on this window,
    // only reversals and margin cuts, so the gap is in the margin call or in the size of the reversals of all of the account's
    // worth; no cause found (IMP-I8, 2026-11-15). Imported with this net on the owner's ruling of 2026-11-15 ("ADD").
    Accepted {
        name: "trailing_take_profit_trailing_stop_loss",
        trades: 1308,
        net: -21064.00278530015,
        why: "owner 2026-11-15: net 5.2% off, no cause found",
    },
    // The reference's count, 2015, and a net 0.063 over its -253.325546: the take profit's and the re-entry's quantities
    // rounded each other way give no nearer net (the re-entry unrounded is the nearest); no cause found (IMP-I8,
    // 2026-11-15). Imported with this net on the owner's ruling of 2026-11-15 ("ADD").
    Accepted {
        name: "trend_trader_remastered_strategy",
        trades: 2015,
        net: -253.2622580032863,
        why: "owner 2026-11-15: net 0.063 over, no cause found",
    },
    // The reference's count, 1592, and a net 1.14 over its -24030.519531, no cause known: level roundings, raw levels,
    // bar-walk ties and frozen levels give 0.55 to 5.21 off or worse (set B level 2, batch 9, 2026-11-08). Imported with
    // this net on the owner's ruling of 2026-11-15.
    Accepted {
        name: "trendlines_with_alerts",
        trades: 1592,
        net: -24029.38000000071,
        why: "owner 2026-11-15: 1.14 over, no cause known",
    },
    // Two trades more than the reference's 143 and a net 28.72 under its 2772.349609 (BTCUSDT 30 minutes, a USD account, its
    // 15-minute input from 2025-07-01): the 15-minute reading at the first or the last lower bar, the RSI's form and the
    // stochastic's construct move the net, never the count; no cause found (IMP-I4, 2026-11-16). Imported with these numbers
    // on the owner's ruling of 2026-11-16 ("ADD").
    Accepted {
        name: "tripple_super_trend_strategy",
        trades: 145,
        net: 2743.633194871648,
        why: "owner 2026-11-16: two trades more, net 28.72 under, no cause found",
    },
    // Two trades fewer than the reference's 145 and a net 4601.91 over its -458562 (ETHUSDT 4 hours): the venue's 4-hour bars
    // hold 19984 bars in the window where the reference counted 19987 (IMP-I5, 2026-11-15). Imported with these numbers on
    // the owner's ruling of 2026-11-15 ("ADD").
    Accepted {
        name: "volume_difference_delta_cycle_oscillator",
        trades: 143,
        net: -453960.0898790004,
        why: "owner 2026-11-15: two trades fewer, net 4601.91 over, 3 bars fewer in the venue's series",
    },
    // One trade more than the reference's 3677 and a net 0.022 under its -105.361972: the reference has no trade before
    // 2025-01-01 11:30 UTC, and this file's first trade (a short from 04:30, filled by its stop) is the whole difference,
    // every later trade the same to 1e-6; nothing in the original delays its orders, so starting them later would be tuning
    // (IMP-I8b, 2026-11-15). Imported with these numbers on the owner's ruling of 2026-11-16 ("ADD").
    Accepted {
        name: "x48_strategy_channel_breakout_consecutive_17in1",
        trades: 3678,
        net: -105.38395932139906,
        why: "owner 2026-11-16: one trade more (the first), net 0.022 under",
    },
];

/// An accepted row passes on its own numbers alone: one trade more or less, a net one bit off, or its reference's net, each
/// fails; and every accepted row is a `PLANS` row, once.
pub fn self_check() {
    let a = Accepted { name: "x", trades: 225, net: -35834.595117, why: "" };
    assert!(a.holds(225, -35834.595117));
    assert!(!a.holds(224, -35834.595117) && !a.holds(226, -35834.595117));
    let one_bit_off = f64::from_bits((-35834.595117f64).to_bits() + 1);
    assert!(!a.holds(225, one_bit_off) && !a.holds(225, -35834.542969));
    assert!(
        ACCEPTED.windows(2).all(|w| w[0].name < w[1].name),
        "ACCEPTED is sorted, one row a name"
    );
    assert!(
        ACCEPTED.iter().all(|a| PLANS.iter().any(|p| p.name == a.name)),
        "an ACCEPTED row is a PLANS row"
    );
}

/// The strategy `main.rs`'s cost control runs again with a wrong commission.
pub const COST_CONTROL: &str = "ichimoku_cloud";

/// One strategy with its window, as `crate::desk` runs it.
#[expect(
    dead_code,
    reason = "`name` is read only by desk's trade-list check, which set B has no lists for"
)]
pub struct PlanEntry {
    pub name: &'static str,
    /// The first bar fed to the strategy.
    pub warm_from_ms: i64,
    /// The first bar orders may be placed on.
    pub trade_from_ms: Option<i64>,
    /// The window's last bar.
    pub end_ms: i64,
    pub build: fn() -> Box<dyn Plan>,
}
