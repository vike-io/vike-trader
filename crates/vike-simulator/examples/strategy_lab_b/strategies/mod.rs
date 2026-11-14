//! Set B's rows. A row whose rule the strategy gallery holds names it (`gallery!`, decision 0269, W5) with the row's own
//! `Terms`; the files here are the rules the gallery does not take: one that reads a higher bar before it closes, which no read
//! of the order language gives (`crate::timeframe::Higher::looking_ahead`, the period's final values). The rows whose licence
//! the review left unsettled were deleted (2026-11-10). Adding a rule the lab keeps is a new file here, one `mod` line and one
//! `PLANS` row, each list sorted and one line per strategy, so branches merge as a union of lines.

use crate::desk::{Plan, Terms};
use crate::sizing::margin_call;

mod ajay_r5_41;
mod enhanced_ob_retest_strategy_v7_0;
mod htf_high_low_repaint_strategy;
mod optimized_heikin_ashi_strategy_with_buy_sell_opt;

/// A row of `PLANS`: the strategy's name, which also names its reference row, and its rule.
pub struct Listed {
    pub name: &'static str,
    pub build: fn() -> Box<dyn Plan>,
}

macro_rules! row {
    ($m:ident) => {
        Listed { name: stringify!($m), build: $m::new }
    };
}

/// A row whose rule the gallery holds, by the row's name or (`=> "<name>"`) the gallery's own, run on the row's `Terms`.
macro_rules! gallery {
    ($m:ident, $terms:expr) => {
        Listed { name: stringify!($m), build: || crate::desk::gallery_on(stringify!($m), $terms) }
    };
    ($m:ident => $g:literal, $terms:expr) => {
        Listed { name: stringify!($m), build: || crate::desk::gallery_on($g, $terms) }
    };
}

pub const PLANS: &[Listed] = &[
    gallery!(abcd_strategy, Terms::PLAIN),
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
    gallery!(adx_dmi_trend, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    row!(ajay_r5_41),
    gallery!(alculation_a_position_size_based_on_risk, Terms { cash: 100_000.0, ..Terms::PLAIN }),
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
        Terms {
            cash: 10_000.0,
            slip_ticks: 2.0,
            fill_at_close: true,
            account_usd: true,
            ..Terms::PLAIN
        }
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
    gallery!(bollinger_band_breakout, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(bollinger_bands_ema_9, Terms { cash: 1_000.0, fill_at_close: true, ..Terms::PLAIN }),
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
        Terms { cash: 10_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(
        buy_sell_strategy_depends_on_ao_stoch_rsi_atr_by,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        buy_sell_strategy_with_z_score_tradedots,
        Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(camarilla_pivot_points_v2_backtest, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(captain_backtest_model_tfo, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(
        cci_rsi_ema_strategy_with_percentage_or_atr_tp_s,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        chandelier_exit_zlsma_strategy,
        Terms { cash: 1_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
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
    gallery!(d_rock_s_ma_strategy, Terms { cash: 25_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(
        davin_s_10_200ma_pullback_on_spy_strategy_v2_0,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(dax_shooter_5m_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(dca, Terms { cash: 1_000_000.0, fill_at_close: true, ..Terms::PLAIN }),
    gallery!(dca_bot_emulator, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(delta_rsi_strategy_with_filters, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        dema_atr_strategy_primeautomation,
        Terms { cash: 10_000.0, fill_at_close: true, whole_entry_fee: true, ..Terms::PLAIN }
    ),
    gallery!(divergence_for_many_indicators_v4_st, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(donchain_bo, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(double_bottom_and_top_hunter, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(
        double_exponential_moving_average_8_20_63_strate,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(double_triple_pattern_ts_indie, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(ds_entry_exit_trade_v01_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(dual_moving_average, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(
        dynamic_rsi_momentum,
        Terms { cash: 1_000_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(easy_sys_420, Terms { cash: 100_000.0, slip_ticks: 1.0, tick: 0.01, ..Terms::PLAIN }),
    gallery!(
        edma_scalping_strategy_exponentially_deviating_m,
        Terms { cash: 10_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        ee_sweep_return_strategy_v1,
        Terms { cash: 50_000.0, slip_ticks: 1.0, fill_at_close: true, ..Terms::PLAIN }
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
    row!(enhanced_ob_retest_strategy_v7_0),
    gallery!(eurusd_1min_strat_risk, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(evwma_vwap_cross_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(evwma_vwap_macd_strategy_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(fibonacci_rsi_strategy, Terms { cash: 10_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(fixed_percent_stop_loss_take_profit, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        flawless_victory_strategy,
        Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(
        flexisupertrend_strategy_presenttrading,
        Terms { cash: 10_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(forex_fractal_ema_scalper, Terms::PLAIN),
    gallery!(fractal_breakout_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(fractal_strat_kl, Terms { cash: 1_000_000_000.0, ..Terms::PLAIN }),
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
    gallery!(heikin_ashi_supertrend, Terms { cash: 100.0, fill_at_close: true, ..Terms::PLAIN }),
    gallery!(hhll, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
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
    row!(htf_high_low_repaint_strategy),
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
    gallery!(inside_bar_strategy_w_sl, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(
        intellect_city_world_cycle_ath_atl_timeframe_1d,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        investments_swing_trading_strategy_for_different,
        Terms { cash: 100_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(inwcoin_martingale_strategy_for_bitcoin, Terms { cash: 20_000.0, ..Terms::PLAIN }),
    gallery!(kahlman_hullma_wt_cross_strategy, Terms { cash: 100.0, ..Terms::PLAIN }),
    gallery!(kalki_tfxbot, Terms { cash: 100.0, fill_at_close: true, ..Terms::PLAIN }),
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
    gallery!(lanz_strategy_4_0_backtest, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(larry_connors_strategy_josemetal, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(linear_regression_channel_breakout_strategy, Terms::PLAIN),
    gallery!(liquidity_sweep_filter_strategy_algoalpha_x_pine => "liquidity_sweep_filter_strategy_algoalpha", Terms { cash: 100_000.0, slip_ticks: 2.0, fill_at_close: true, ..Terms::PLAIN }),
    gallery!(
        liquidity_sweep_reversal_strategy,
        Terms { cash: 10_000.0, slip_ticks: 1.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(lube, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(luxalgo_smc_pro_ultimate, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(ma_10_20_crossover, Terms::PLAIN),
    gallery!(ma_bollinger_bands_rsi_strategy, Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(ma_candles_supertrend_strategy, Terms { cash: 20_000.0, ..Terms::PLAIN }),
    gallery!(macandles_linearregression_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(macd_bb_rsi_alorse, Terms { cash: 1_000.0, account_usd: true, ..Terms::PLAIN }),
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
        Terms { cash: 100_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(
        maximized_scalping_on_trend_by_coinrule,
        Terms { cash: 1_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(
        mcginley_t3_flow_campaign_nick789,
        Terms { cash: 100_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(monthly_returns_in_pinescript_strategies => "monthly_returns_in_strategies", Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(mov_avg_speed_strategy, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(moving_average_displaced_envelope_atrts, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(moving_average_scalper, Terms { cash: 1_000.0, ..Terms::PLAIN }),
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
    row!(optimized_heikin_ashi_strategy_with_buy_sell_opt),
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
    gallery!(pts_golden_edge, Terms::PLAIN),
    gallery!(qqe_mod_ssl_hybrid_waddah_attar_explosion, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(quantnomad_heikin_ashi_psar_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(
        quasimodo_pattern_strategy_back_test_tradingfind,
        Terms { cash: 1_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        rally_base_drop_snd_pivots_strategy_luxalgo_x_pi,
        Terms { cash: 100_000.0, slip_ticks: 2.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(random, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(range_strat_macd_rsi, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        reversal_trading_bot_strategy_bullbyte_final_ver,
        Terms {
            cash: 1_000.0,
            fill_at_close: true,
            shortfall_cover: margin_call(100.0, 100.0),
            ..Terms::PLAIN
        }
    ),
    gallery!(rob_booker_adx_breakout, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(rsi_and_sma, Terms { cash: 1_000.0, fill_at_close: true, ..Terms::PLAIN }),
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
    gallery!(
        rsi_strategy_primeautomation,
        Terms { cash: 10_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
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
        Terms { cash: 50_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(s_2xtp_sl_to_entry, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        s_3_ema_stochastic_rsi_atr,
        Terms {
            cash: 1_000.0,
            slip_ticks: 2.0,
            fill_at_close: true,
            account_usd: true,
            ..Terms::PLAIN
        }
    ),
    gallery!(
        s_3ls_3_line_strike_strategy_kintsugi_trading,
        Terms { cash: 10_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(s_5_ema_strategy, Terms { fill_at_close: true, ..Terms::PLAIN }),
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
    gallery!(
        smart_money_concept_uncle_sam,
        Terms { cash: 1_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(
        smart_money_pivot_strategy_jason_kasei,
        Terms { cash: 10_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(
        smc_strategy_with_premium_equilibrium_and_discou,
        Terms { cash: 1_000_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        so_trade_top_me_dia_de_8_augusto_backes,
        Terms { cash: 10_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(sr_trend_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(ssl_channel_cross, Terms { cash: 100_000.0, ..Terms::PLAIN }),
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
        Terms { cash: 2_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
    gallery!(
        supertrend_regime_confluence,
        Terms { cash: 10_000.0, slip_ticks: 2.0, ..Terms::PLAIN }
    ),
    gallery!(
        supertrend_strategy,
        Terms { cash: 10_000_000.0, fill_at_close: true, ..Terms::PLAIN }
    ),
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
    gallery!(trailing_sl_strategy_quantnomad, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(trailing_stop_loss_to_long_and_short, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(trailing_take_profit, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(traling_sl_target, Terms { cash: 1_000_000.0, fill_at_close: true, ..Terms::PLAIN }),
    gallery!(trend_direction_helper, Terms { cash: 100_000.0, account_usd: true, ..Terms::PLAIN }),
    gallery!(trend_surfers_breakout_alerts, Terms { cash: 2_000.0, ..Terms::PLAIN }),
    gallery!(
        trend_surfers_premium_breakout,
        Terms { cash: 100_000.0, committed_exits: true, ..Terms::PLAIN }
    ),
    gallery!(trendline_breakout_strategy_kedarc_quant, Terms { cash: 100_000.0, ..Terms::PLAIN }),
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
        Terms {
            cash: 1_000.0,
            slip_ticks: 2.0,
            fill_at_close: true,
            account_usd: true,
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
    gallery!(
        volume_vigilante_long_only_opening_range_breakou,
        Terms {
            cash: 10_000.0,
            fill_at_close: true,
            shortfall_cover: margin_call(100.0, 100.0),
            ..Terms::PLAIN
        }
    ),
    gallery!(vwap_fibo_dev_extensions_strategy, Terms { cash: 100_000.0, ..Terms::PLAIN }),
    gallery!(vwap_trendfollow_strategy_wbburgin, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        vwma_with_knn_machine_learning_mfi_adx_by_lastgu,
        Terms { cash: 100_000.0, ..Terms::PLAIN }
    ),
    gallery!(
        wolfe_strategy_trendoscope,
        Terms { cash: 1000.0, shortfall_cover: margin_call(100.0, 100.0), ..Terms::PLAIN }
    ),
    gallery!(
        wombo_combo_100_200ema_vwap_macd,
        Terms { cash: 20_000.0, slip_ticks: 2.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(wyckoff_range_strategy_by_deperp, Terms { cash: 1_000.0, ..Terms::PLAIN }),
    gallery!(
        x48_strategy_ma_multi_type_cross_tpsl_swing_surf,
        Terms { cash: 200.0, account_usd: true, ..Terms::PLAIN }
    ),
    gallery!(xauusd_scalping_v2_agresiv, Terms::PLAIN),
    gallery!(yinyang_rsi_volume_trend_strategy, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
    gallery!(zendog_v3_backtest_dca_bot_3commas => "zendog_v3_backtest_dca_bot", Terms { cash: 10_000.0, ..Terms::PLAIN }),
    gallery!(zero_lag_ma_trend_following, Terms { cash: 1_000_000.0, ..Terms::PLAIN }),
];

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
