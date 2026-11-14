//! The `[engine.slippage]` table: `ProfileEngineSlippage`, the ONE place a run states its slippage, with ONE
//! model (the owner's ruling of 2026-11-10).
//!
//! ```toml
//! [engine.slippage]
//! model = "none"            # off; no table at all is the same
//! # model = "percent"       value = 0.0005        a fraction of the price against the trader
//! # model = "ticks"         ticks = 2, tick = 0.01  that many ticks of `tick` against the trader
//! # model = "market_impact" base = 0.0002          the optional constant part, plus the impact model
//! ```
//!
//! - **One model at a time.** A key of another model is refused with a plain message
//!   ([`ProfileEngineSlippage::check`]), so a table can never price a fill two ways.
//! - **The old spellings are refused, not read** (no migration, the owner's standing rule): the scalar
//!   `engine.slippage = <fraction>` and the table `[engine.impact]` ([`refuse_old_spelling`]), each
//!   with a message that shows the new spelling. The old flat fraction is `percent`; the old impact on
//!   top of a flat fraction is `market_impact` with that fraction as `base`.
//! - **The same three engine fields as before** ([`slippage_terms`]): `percent` is
//!   `vike_simulator::StrategyEngineConfig::slippage`, `ticks` is `vike_simulator::StrategyEngineConfig::slippage_amount`
//!   (`ticks * tick`, a price amount every fill no limit bounds pays), `market_impact` is
//!   `vike_simulator::StrategyEngineConfig::impact` with `base` as `slippage`. An equivalent old profile gives the
//!   same bits.
//! - Never in a strategy's header (owner ruled): slippage is the run's.

use std::sync::Arc;

use serde::Deserialize;

use crate::harness::HarnessError;

/// The TOML shape of `[engine.slippage]`. Every key but `model` is optional, and each belongs to one
/// model; [`Self::check`] refuses a key of another model.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileEngineSlippage {
    /// Which model prices a fill's slippage: `none`, `percent`, `ticks` or `market_impact`.
    pub model: String,
    /// `percent`: the fraction of the price a fill moves against the trader (`0.0005` is 0.05%).
    #[serde(default)]
    pub value: Option<f64>,
    /// `ticks`: how many ticks a fill moves against the trader. Every fill no limit bounds pays it
    /// (a market, stop or trailing fill, the protective stop, a forced close); a limit fill pays none.
    #[serde(default)]
    pub ticks: Option<f64>,
    /// `ticks`: the price one tick is, in the series' own quote units (`0.01`).
    #[serde(default)]
    pub tick: Option<f64>,
    /// `market_impact`: a constant fraction of the price every fill pays beside the impact (absent:
    /// none).
    #[serde(default)]
    pub base: Option<f64>,
    /// `market_impact`: the execution horizon, in this run's periods (bars, or trade prints in tick
    /// mode, [`vike_simulator::ImpactModelAlmgrenChriss::exec_time`]). Absent: `1.0`. ⚠ It scales the TEMPORARY term
    /// only, so it cannot move an L2-lane charge: [`Self::gamma`] can.
    #[serde(default)]
    pub exec_time: Option<f64>,
    /// `market_impact`: the rolling sigma and volume lookback, in bars (trade prints in tick mode).
    /// Absent: [`vike_simulator::DEFAULT_IMPACT_WINDOW`], a trading month of daily bars, so a tick profile
    /// should set it.
    #[serde(default)]
    pub window: Option<usize>,
    /// `market_impact`: the permanent-impact coefficient. Absent: the published
    /// [`vike_simulator::AC_GAMMA`]. The one lever an L2-lane charge has, and a linear one.
    #[serde(default)]
    pub gamma: Option<f64>,
    /// `market_impact`: the temporary-impact coefficient. Absent: the published [`vike_simulator::AC_ETA`].
    /// It moves the bar and L1-tick lanes; an L2 fill that walked a book already paid this half.
    #[serde(default)]
    pub eta: Option<f64>,
}

/// The model words, each with the keys it owns: the roster [`ProfileEngineSlippage::check`] judges a table by.
const MODELS: [(&str, &[&str]); 4] = [
    ("none", &[]),
    ("percent", &["value"]),
    ("ticks", &["ticks", "tick"]),
    ("market_impact", &["base", "exec_time", "window", "gamma", "eta"]),
];

/// The `[paramscan]` keys that vary a run's slippage rather than a strategy param, each with the
/// model that owns it ([`crate::harness::paramscan::profile_with_overrides`]); every other `engine.*`
/// grid key is refused.
pub(crate) const SWEPT_KEYS: [(&str, &str); 2] =
    [("engine.slippage.ticks", "ticks"), ("engine.slippage.value", "percent")];

/// What a run's slippage resolves to: the three engine fields it fills.
pub(crate) struct ProfileEngineSlippageTerms {
    /// `vike_simulator::StrategyEngineConfig::slippage`, a fraction of the price.
    pub(crate) fraction: f64,
    /// `vike_simulator::StrategyEngineConfig::slippage_amount`, a price amount.
    pub(crate) amount: f64,
    /// `vike_simulator::StrategyEngineConfig::impact` and its `impact_window`.
    pub(crate) impact: Option<(Arc<dyn vike_simulator::ImpactModel>, usize)>,
}

impl ProfileEngineSlippage {
    /// Each key this table states, by name.
    fn stated(&self) -> [(&'static str, bool); 8] {
        [
            ("value", self.value.is_some()),
            ("ticks", self.ticks.is_some()),
            ("tick", self.tick.is_some()),
            ("base", self.base.is_some()),
            ("exec_time", self.exec_time.is_some()),
            ("window", self.window.is_some()),
            ("gamma", self.gamma.is_some()),
            ("eta", self.eta.is_some()),
        ]
    }

    /// Refuse an unknown model, a key of another model, a missing key and a number that cannot be a
    /// price move, each in words naming the key.
    pub fn check(&self) -> Result<(), HarnessError> {
        let Some((_, own)) = MODELS.iter().find(|(m, _)| *m == self.model) else {
            return Err(HarnessError::Validation(format!(
                "unknown engine.slippage.model {:?} (want none | percent | ticks | market_impact; \
                 no [engine.slippage] table is none)",
                self.model
            )));
        };
        for (key, set) in self.stated() {
            if set && !own.contains(&key) {
                let owner = MODELS.iter().find(|(_, keys)| keys.contains(&key)).map_or("", |m| m.0);
                return Err(HarnessError::Validation(format!(
                    "engine.slippage.{key} belongs to the {owner} model, and this table's model is \
                     {:?}: slippage takes one model at a time, so keep the keys of one model only",
                    self.model
                )));
            }
        }
        let need = |key: &str, v: Option<f64>, what: &str| {
            v.map(|_| ()).ok_or_else(|| {
                HarnessError::Validation(format!(
                    "engine.slippage.model = {:?} needs {key}: {what}",
                    self.model
                ))
            })
        };
        match self.model.as_str() {
            "percent" => need(
                "value",
                self.value,
                "the fraction of the price a fill moves against the trader (0.0005 is 0.05%)",
            )?,
            "ticks" => {
                need("ticks", self.ticks, "how many ticks a fill moves against the trader")?;
                need("tick", self.tick, "the price one tick is (0.01)")?;
            }
            _ => {}
        }
        for (key, v) in [("value", self.value), ("ticks", self.ticks), ("base", self.base)] {
            if let Some(v) = v
                && !(v.is_finite() && v >= 0.0)
            {
                return Err(HarnessError::Validation(format!(
                    "engine.slippage.{key} must be a number >= 0, got {v}"
                )));
            }
        }
        if let Some(tick) = self.tick
            && !(tick.is_finite() && tick > 0.0)
        {
            return Err(HarnessError::Validation(format!(
                "engine.slippage.tick must be a price > 0, got {tick}"
            )));
        }
        if let Some(t) = self.exec_time
            && !(t.is_finite() && t > 0.0)
        {
            return Err(HarnessError::Validation(format!(
                "engine.slippage.exec_time must be > 0, got {t}"
            )));
        }
        if let Some(w) = self.window
            && w < 3
        {
            return Err(HarnessError::Validation(format!(
                "engine.slippage.window must be >= 3 (two returns), got {w}"
            )));
        }
        // Both coefficients > 0 and finite. A NEGATIVE one makes the cost fall with size, a model
        // that PAYS a large order, which the fill site relies on never seeing; zero would silently
        // disarm the L2 lane's only charge, and not naming the knob already means the default.
        for (key, v) in [("gamma", self.gamma), ("eta", self.eta)] {
            if let Some(v) = v
                && !(v.is_finite() && v > 0.0)
            {
                return Err(HarnessError::Validation(format!(
                    "engine.slippage.{key} must be > 0 and finite, got {v}"
                )));
            }
        }
        Ok(())
    }

    /// The `market_impact` knobs, each its default where the table states none:
    /// `(exec_time, window, gamma, eta)`.
    pub(crate) fn impact_knobs(&self) -> (f64, usize, f64, f64) {
        (
            self.exec_time.unwrap_or(1.0),
            self.window.unwrap_or(vike_simulator::DEFAULT_IMPACT_WINDOW),
            self.gamma.unwrap_or(vike_simulator::AC_GAMMA),
            self.eta.unwrap_or(vike_simulator::AC_ETA),
        )
    }

    /// Set one swept key ([`SWEPT_KEYS`]) to `v`; any other key is not this table's.
    pub(crate) fn set_swept(&mut self, key: &str, v: f64) {
        match key {
            "engine.slippage.ticks" => self.ticks = Some(v),
            "engine.slippage.value" => self.value = Some(v),
            _ => {}
        }
    }
}

/// `config` resolved to the engine's three slippage fields. Absent (or `none`) is no slippage, exactly
/// as a profile with no slippage always was. A free function taking the table for the
/// `engine_cfg_reaches_the_engine` gate's reason: each `StrategyEngineConfig` site spells the read
/// `profile.engine.slippage`.
pub(crate) fn slippage_terms(
    config: Option<&ProfileEngineSlippage>,
) -> Result<ProfileEngineSlippageTerms, HarnessError> {
    let none = ProfileEngineSlippageTerms { fraction: 0.0, amount: 0.0, impact: None };
    let Some(c) = config else { return Ok(none) };
    c.check()?;
    Ok(match c.model.as_str() {
        "percent" => ProfileEngineSlippageTerms { fraction: c.value.unwrap_or(0.0), ..none },
        // The lab's `slip_ticks * tick`, in that order.
        "ticks" => ProfileEngineSlippageTerms {
            amount: c.ticks.unwrap_or(0.0) * c.tick.unwrap_or(0.0),
            ..none
        },
        "market_impact" => {
            let (exec_time, window, gamma, eta) = c.impact_knobs();
            let model =
                vike_simulator::ImpactModelAlmgrenChriss::with_coefficients(gamma, eta, exec_time);
            ProfileEngineSlippageTerms {
                fraction: c.base.unwrap_or(0.0),
                impact: Some((Arc::new(model), window)),
                ..none
            }
        }
        _ => none,
    })
}

/// Refuse the two retired spellings, read off the profile's raw table before it is typed: the scalar
/// `engine.slippage = <fraction>` and the table `[engine.impact]`. Each message shows the new
/// spelling; nothing is migrated.
pub(crate) fn refuse_old_spelling(doc: &toml::Table) -> Result<(), HarnessError> {
    let Some(engine) = doc.get("engine").and_then(toml::Value::as_table) else { return Ok(()) };
    let old = engine.get("slippage").filter(|v| v.is_float() || v.is_integer());
    if engine.contains_key("impact") {
        let base = old.map_or_else(String::new, |v| format!(", base = {v}"));
        return Err(HarnessError::Validation(format!(
            "[engine.impact] is the old spelling and is no longer read: market impact is a model of \
             [engine.slippage] now. Write [engine.slippage] with model = \"market_impact\"{base} and \
             the same exec_time, window, gamma and eta"
        )));
    }
    if let Some(v) = old {
        return Err(HarnessError::Validation(format!(
            "engine.slippage = {v} is the old spelling and is no longer read: slippage is the table \
             [engine.slippage] with one model now. Write [engine.slippage] with model = \"percent\" \
             and value = {v} for the same slippage, or model = \"none\" for none"
        )));
    }
    Ok(())
}
