//! [`resolved_params`]: the mount log's echo of what each declared key RESOLVED to.

use toml::Value;

use super::{BuyHold, harness_cooldown_ms, harness_venue, harness_venue_map};
use crate::{
    AnchorMode, DcaAccumulate, FundingCapture, FundingCarryController, Grid, MomentumController,
    PairsZScore, TrailingScalper,
};

#[cfg(doc)]
use super::gates::PARAM_GATES;
#[cfg(doc)]
use super::keys::{PARAM_KEYS, mistyped_params};

/// What `name`'s reader ACTUALLY resolved out of `params` — every declared key with the value the
/// strategy will run with, in [`PARAM_KEYS`] order. `None` for a name with no enumerated key set
/// (the two maker aliases) or no row at all.
///
/// ⚠ This is the honest half of a mount log, and it is not the same thing as echoing the profile:
/// a printed TOML table is a claim about the operator, false exactly when it differs from what the
/// daemon runs. [`mistyped_params`] removes one cause of divergence; a reader may still CLAMP
/// (`read_rungs`' `i.max(0)`, `PairsZScore`'s `period.round().max(2.0)`), fall back on an
/// unrecognised string (`anchor` ⇒ `"first"`, `side` ⇒ LONG) or supply an unmentioned default
/// (`venue = "sim"`), and every one of those shows up HERE.
///
/// It re-runs the SAME pure `from_params` the mount used, so the values cannot disagree with the
/// mounted ones; WHICH knobs get reported is pinned to [`PARAM_KEYS`] by
/// `resolved_params_reports_exactly_the_declared_keys_in_order`.
///
/// ⚠ **It reports what each knob RESOLVED TO. It claims nothing about what is IN FORCE**, and no
/// example of an inert key belongs here: that IS a positive claim about consumption, checked by
/// nobody. The authority on a knob nothing reads is `crates/vike-strategy/tests/param_gates.rs`;
/// [`PARAM_GATES`]' own doc carries why the annotated echo was deleted.
pub fn resolved_params(name: &str, params: &Value) -> Option<Vec<(&'static str, String)>> {
    fn num(v: f64) -> String {
        format!("{v}")
    }
    fn opt_num(v: Option<f64>) -> String {
        v.map(num).unwrap_or_else(|| "(unarmed)".to_string())
    }
    fn opt_ms(v: Option<i64>) -> String {
        v.map(|i| i.to_string()).unwrap_or_else(|| "(unarmed)".to_string())
    }
    /// An OPTIONAL symbol, in the THREE states it actually has — absent, explicitly EMPTY, and set.
    ///
    /// ⚠ **Absent and empty are not the same mount.** `None` takes the symbol off the feed;
    /// `Some("")` is a STATED value every reader this serves refuses to trade on (`BuyHold::buy`,
    /// `crates/vike-strategy/src/strategies/funding_capture.rs`'s `on_bar` and both of
    /// `crates/vike-strategy/src/strategies/grid_dca.rs`'s `drive` methods guard on `is_empty()`),
    /// proven by `the_empty_symbol_this_echo_reports_really_does_stop_the_strategy`. It mirrors
    /// that `is_empty()` exactly (no trimming): a whitespace symbol IS routed.
    fn opt_sym(v: &Option<String>) -> String {
        match v.as_deref() {
            None => "(from the feed)".to_string(),
            Some("") => "(empty — this strategy cannot trade)".to_string(),
            Some(s) => s.to_string(),
        }
    }
    /// A REQUIRED symbol left empty. ⚠ Not cosmetic: `PairsZScore` with either leg unset never
    /// routes an order (there is no single-symbol fallback for a two-leg trade), so an echo that
    /// printed an empty string would show a mount that cannot trade as if it were configured.
    fn req_sym(v: &str) -> String {
        if v.is_empty() { "(unset — this leg cannot route)".to_string() } else { v.to_string() }
    }
    fn anchor(m: AnchorMode) -> String {
        match m {
            AnchorMode::Fixed => "fixed",
            AnchorMode::FirstPrice => "first",
        }
        .to_string()
    }
    fn venues(map: &[(String, String)]) -> String {
        if map.is_empty() {
            return "(none)".to_string();
        }
        map.iter().map(|(s, v)| format!("{s}:{v}")).collect::<Vec<_>>().join(",")
    }

    Some(match name {
        "buy_hold" => {
            let s = BuyHold::from_params(params);
            vec![("size", num(s.size)), ("symbol", opt_sym(&s.symbol))]
        }
        "grid" => {
            let g = Grid::from_params(params);
            vec![
                ("anchor", anchor(g.anchor_mode)),
                ("anchor_price", num(g.anchor_price)),
                ("step", num(g.step)),
                ("rungs", g.rungs.to_string()),
                ("size", num(g.size)),
                ("band", num(g.band)),
                ("bounded01", g.bounded01.to_string()),
                ("tick", num(g.tick)),
                ("symbol", opt_sym(&g.symbol)),
            ]
        }
        "dca_accumulate" => {
            let d = DcaAccumulate::from_params(params);
            vec![
                ("side", if d.side < 0 { "short" } else { "long" }.to_string()),
                ("anchor", anchor(d.anchor_mode)),
                ("anchor_price", num(d.anchor_price)),
                ("step", num(d.step)),
                ("rungs", d.rungs.to_string()),
                ("size", num(d.size)),
                ("tp", num(d.tp)),
                ("symbol", opt_sym(&d.symbol)),
            ]
        }
        "trailing_scalper" => {
            let t = TrailingScalper::from_params(params);
            vec![
                ("qty", num(t.qty)),
                ("half_spread", num(t.half_spread)),
                ("exit_delay_ms", t.exit_delay_ms.to_string()),
                ("profit_target", num(t.profit_target)),
                ("entry_open_delay_ms", t.entry_open_delay_ms.to_string()),
                ("entry_cutoff_before_close_ms", t.entry_cutoff_before_close_ms.to_string()),
                ("market_open_ms", t.market_open_ms.to_string()),
                ("market_close_ms", t.market_close_ms.to_string()),
            ]
        }
        "momentum" => {
            let c = MomentumController::from_params(params);
            let b = c.barriers;
            vec![
                ("qty", num(c.qty)),
                ("threshold", num(c.threshold)),
                ("tp", opt_num(b.take_profit)),
                ("sl", opt_num(b.stop_loss)),
                ("time_limit_ms", opt_ms(b.time_limit_ms)),
                ("trailing", opt_num(b.trailing)),
                ("venue", harness_venue(params).to_string()),
                ("cooldown_ms", harness_cooldown_ms(params).to_string()),
                ("venues", venues(&harness_venue_map(params))),
            ]
        }
        "funding_carry" => {
            let c = FundingCarryController::from_params(params);
            let b = c.barriers();
            vec![
                (
                    "symbol",
                    if c.symbol().is_empty() {
                        "(both legs)".to_string()
                    } else {
                        c.symbol().to_string()
                    },
                ),
                ("qty", num(c.qty())),
                ("tp", opt_num(b.take_profit)),
                ("sl", opt_num(b.stop_loss)),
                ("time_limit_ms", opt_ms(b.time_limit_ms)),
                ("trailing", opt_num(b.trailing)),
                ("hold_periods", num(c.hold_periods())),
                ("entry_threshold", num(c.entry_threshold())),
                ("venue", harness_venue(params).to_string()),
                ("cooldown_ms", harness_cooldown_ms(params).to_string()),
                ("venues", venues(&harness_venue_map(params))),
            ]
        }
        "funding_capture" => {
            let f = FundingCapture::from_params(params);
            vec![
                ("threshold", num(f.threshold)),
                ("qty", num(f.qty)),
                ("symbol", opt_sym(&f.symbol)),
            ]
        }
        "pairs_zscore" => {
            let p = PairsZScore::from_params(params);
            vec![
                ("symbol_a", req_sym(&p.symbol_a)),
                ("symbol_b", req_sym(&p.symbol_b)),
                ("period", p.period.to_string()),
                ("entry_z", num(p.entry_z)),
                ("exit_z", num(p.exit_z)),
                ("beta", num(p.beta)),
                ("notional", num(p.notional)),
                ("taker_fee", num(p.taker_fee)),
                ("half_spread_bps", num(p.half_spread_bps)),
                ("hold_intervals", num(p.hold_intervals)),
                ("funding_a", num(p.funding_a)),
                ("funding_b", num(p.funding_b)),
                ("max_half_life", num(p.max_half_life)),
            ]
        }
        _ => return None,
    })
}
