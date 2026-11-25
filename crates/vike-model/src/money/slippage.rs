//! `ProfileEngineSlippage`: a slippage table with ONE model (the owner's ruling of 2026-11-10).
//!
//! ```toml
//! model = "none"            # off; no table at all is the same
//! # model = "percent"       value = 0.0005        a fraction of the price against the trader
//! # model = "ticks"         ticks = 2, tick = 0.01  that many ticks of `tick` against the trader
//! # model = "market_impact" base = 0.0002          the optional constant part, plus the impact model
//! ```
//!
//! - **One model at a time.** A key of another model is refused with a plain message
//!   ([`ProfileEngineSlippage::check`]), so a table can never price a fill two ways.
//! - **One shape for every reader**: the slippage a strategy states (`crate::Strategy::stated_slippage`),
//!   and the `config.paper_slippage_table` settings row a `paper`-tier account's book is priced by. It lives
//!   here, not in the backtest, for the reason `[risk]` does
//!   (`docs/decisions/0114-the-risk-config-types-live-in-vike-model.md`): vike-config judges the row with the
//!   real type, and the trade daemon, which links no backtest, reads the same one.
//! - **What a table resolves to** is `vike_fills::market_impact::slippage_terms`, one function for the
//!   backtest's engine and the paper book: `percent` is a fraction of the price, `ticks` a price amount
//!   (`ticks * tick`, paid by every fill no limit bounds), `market_impact` the impact model with `base` as the
//!   fraction.
//! - **A backtest's slippage is a strategy PARAMETER** (the owner's ruling of 2026-11-18, decision 0269's
//!   addendum): the `slippage` param, else the strategy's stated table, else none; there is no venue table of
//!   slippage, and a profile's `[engine.slippage]` is refused by the backtest, which owns that document
//!   (`vike_backtest::run::costs`).

use serde::Deserialize;

/// The TOML shape of a slippage table. Every key but `model` is optional, and each belongs to one
/// model; [`Self::check`] refuses a key of another model.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
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
    /// mode, `vike_fills::market_impact::ImpactModelAlmgrenChriss::exec_time`). Absent: `1.0`. ⚠ It scales the TEMPORARY term
    /// only, so it cannot move an L2-lane charge: [`Self::gamma`] can.
    #[serde(default)]
    pub exec_time: Option<f64>,
    /// `market_impact`: the rolling sigma and volume lookback, in bars (trade prints in tick mode).
    /// Absent: `vike_fills::market_impact::DEFAULT_IMPACT_WINDOW`, a trading month of daily bars, so a tick profile
    /// should set it.
    #[serde(default)]
    pub window: Option<usize>,
    /// `market_impact`: the permanent-impact coefficient. Absent: the published
    /// `vike_fills::market_impact::AC_GAMMA`. The one lever an L2-lane charge has, and a linear one.
    #[serde(default)]
    pub gamma: Option<f64>,
    /// `market_impact`: the temporary-impact coefficient. Absent: the published `vike_fills::market_impact::AC_ETA`.
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
    ///
    /// # Errors
    ///
    /// The first refusal, in words.
    pub fn check(&self) -> Result<(), String> {
        let bad = |message: String| message;
        let Some((_, own)) = MODELS.iter().find(|(m, _)| *m == self.model) else {
            return Err(bad(format!(
                "unknown slippage model {:?} (want none | percent | ticks | market_impact; \
                 no slippage table is none)",
                self.model
            )));
        };
        for (key, set) in self.stated() {
            if set && !own.contains(&key) {
                let owner = MODELS.iter().find(|(_, keys)| keys.contains(&key)).map_or("", |m| m.0);
                return Err(bad(format!(
                    "slippage key {key} belongs to the {owner} model, and this table's model is \
                     {:?}: slippage takes one model at a time, so keep the keys of one model only",
                    self.model
                )));
            }
        }
        let need = |key: &str, v: Option<f64>, what: &str| {
            v.map(|_| ()).ok_or_else(|| {
                bad(format!("slippage model = {:?} needs {key}: {what}", self.model))
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
                return Err(bad(format!("slippage {key} must be a number >= 0, got {v}")));
            }
        }
        if let Some(tick) = self.tick
            && !(tick.is_finite() && tick > 0.0)
        {
            return Err(bad(format!("slippage tick must be a price > 0, got {tick}")));
        }
        if let Some(t) = self.exec_time
            && !(t.is_finite() && t > 0.0)
        {
            return Err(bad(format!("slippage exec_time must be > 0, got {t}")));
        }
        if let Some(w) = self.window
            && w < 3
        {
            return Err(bad(format!("slippage window must be >= 3 (two returns), got {w}")));
        }
        // Both coefficients > 0 and finite. A NEGATIVE one makes the cost fall with size, a model
        // that PAYS a large order, which the fill site relies on never seeing; zero would silently
        // disarm the L2 lane's only charge, and not naming the knob already means the default.
        for (key, v) in [("gamma", self.gamma), ("eta", self.eta)] {
            if let Some(v) = v
                && !(v.is_finite() && v > 0.0)
            {
                return Err(bad(format!("slippage {key} must be > 0 and finite, got {v}")));
            }
        }
        Ok(())
    }
}
