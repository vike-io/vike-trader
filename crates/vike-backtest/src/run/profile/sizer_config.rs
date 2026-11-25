//! The `[engine.sizer]` table: `ProfileEngineSizer`, the position-sizer chain and its depth limit, and
//! `SizerKind`, the `kind` roster with what each kind requires.

use serde::Deserialize;

use crate::HarnessError;
use crate::run::refusals::ProfileRefusal;
use vike_portfolio::sizing::{
    PositionSizer, PositionSizerDrawdownThrottle, PositionSizerFixedDollar,
    PositionSizerFixedShares, PositionSizerMaxRiskPercent, PositionSizerPassThrough,
    PositionSizerPercentEquity, PositionSizerPercentVolatility, PositionSizerPortfolioHeat,
};

#[cfg(doc)]
use super::ProfileEngine;

/// TOML shape of the opt-in position sizer ([`ProfileEngine::sizer`]) — how a strategy's requested
/// size becomes an order size, via [`vike_portfolio::sizing`]'s swappable `PositionSizer`
/// framework (the WealthLab PosSizer port). Every concrete sizer that crate ships gets a `kind`
/// row here and NOTHING else — a new sizer added there with no row here is unreachable from a
/// profile, not silently mapped to the nearest existing one.
///
/// Two kinds — `"portfolio_heat"` and `"drawdown_throttle"` — WRAP a base sizer rather than
/// standing alone (see [`vike_portfolio::sizing::PositionSizerPortfolioHeat`]/[`vike_portfolio::sizing::PositionSizerDrawdownThrottle`]);
/// the wrapped sizer nests under `[engine.sizer.base]`, which is why [`Self::base`] is boxed —
/// this type nests itself.
///
/// ```toml
/// [engine.sizer]
/// kind = "portfolio_heat"
/// max_heat = 0.10
/// [engine.sizer.base]
/// kind = "fixed_dollar"
/// amount = 1000.0
/// ```
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileEngineSizer {
    /// One of `"pass_through"` | `"fixed_dollar"` | `"fixed_shares"` | `"percent_equity"` |
    /// `"percent_volatility"` | `"max_risk_percent"` | `"portfolio_heat"` | `"drawdown_throttle"` — every
    /// concrete [`vike_portfolio::sizing::PositionSizer`] this crate ships and nothing else
    /// ([`Self::build`]).
    pub kind: String,
    /// [`vike_portfolio::sizing::PositionSizerFixedDollar::amount`] — fixed cash notional per entry. Required for
    /// `"fixed_dollar"`.
    #[serde(default)]
    pub amount: Option<f64>,
    /// [`vike_portfolio::sizing::PositionSizerFixedShares::shares`] — fixed share/contract count per entry.
    /// Required for `"fixed_shares"`.
    #[serde(default)]
    pub shares: Option<f64>,
    /// The knob `"percent_equity"` / `"percent_volatility"` / `"max_risk_percent"` each read as their own
    /// fraction of equity (target notional, ATR-risk budget, and stop-risk budget respectively —
    /// see [`vike_portfolio::sizing`]'s doc on each). Required for those three kinds.
    #[serde(default)]
    pub percent: Option<f64>,
    /// [`vike_portfolio::sizing::PositionSizerPortfolioHeat::max_heat`] — total open-risk cap as a fraction of
    /// equity. Required for `"portfolio_heat"`.
    #[serde(default)]
    pub max_heat: Option<f64>,
    /// [`vike_portfolio::sizing::PositionSizerDrawdownThrottle::sensitivity`]. Required for `"drawdown_throttle"`.
    #[serde(default)]
    pub sensitivity: Option<f64>,
    /// [`vike_portfolio::sizing::PositionSizerDrawdownThrottle::floor`]. Required for `"drawdown_throttle"`.
    #[serde(default)]
    pub floor: Option<f64>,
    /// The wrapped sizer for `"portfolio_heat"` / `"drawdown_throttle"` — required for those two
    /// kinds ([`Self::build`] refuses their absence), and REFUSED under every other kind, which
    /// reads it nowhere: a base under a scalar kind would run one sizer while the profile read as
    /// a chain. Boxed because `ProfileEngineSizer` nests itself — see [`MAX_SIZER_DEPTH`] for the bound on
    /// how far.
    #[serde(default)]
    pub base: Option<Box<ProfileEngineSizer>>,
}

/// Bound on how many `ProfileEngineSizer` nodes one `[engine.sizer]` chain may nest (the top-level table
/// counts as depth 1; each `[engine.sizer.base…]` adds one).
///
/// `ProfileEngineSizer` is the first self-referential config struct in this file — `ProfileEngineResolution`
/// is flat — and [`ProfileEngineSizer::build`]'s recursion through [`ProfileEngineSizer::base`] is
/// one stack frame per level. Only the two WRAPPING kinds (`"portfolio_heat"`,
/// `"drawdown_throttle"`) ever consume a `base` at all, so the deepest MEANINGFUL profile composes
/// both of them once around one terminal scalar sizer — three `ProfileEngineSizer` nodes, e.g.
/// `drawdown_throttle` -> `portfolio_heat` -> `fixed_dollar`. `4` leaves exactly one level of
/// headroom past that (e.g. layering the same wrapping kind twice, such as two `portfolio_heat`
/// tiers at different `max_heat` caps) without leaving the bound so loose that an arbitrarily deep
/// `[engine.sizer.base.base.base…]` chain — no config file has a legitimate reason to nest further
/// than a human would hand-write — reads as accepted rather than refused.
///
/// ⚠ This bounds [`ProfileEngineSizer::build`]'s OWN recursion, not `toml`'s — and MEASURED (module test
/// `sizer_depth_probe`, ignored; the Task 12 fix-round report carries the full numbers) is a
/// genuine gap between the two. Deserializing the TOML into nested `ProfileEngineSizer`/`Box<ProfileEngineSizer>`
/// values happens BEFORE `build` (or `validate`, which calls it) ever runs, so for a chain a
/// little past this bound (measured: past ~50-79 levels) the `toml` crate's OWN internal
/// recursion limit fires FIRST, during `toml::from_str` itself, as a generic
/// `HarnessError::Parse("recursion limit")` that never mentions `engine.sizer` — this bound is
/// unreachable for those profiles, not merely redundant. It does NOT crash: no stack overflow was
/// observed at any depth tried. But the cost of DISCOVERING that limit is not free — measured
/// growth is roughly quadratic in nesting depth (depth 100 ⇒ ~5ms, depth 5,000 ⇒ ~8.3s), so a
/// sufficiently large adversarial chain (depth 50,000 was tried) turns "returns a clean error"
/// into "does not return inside several minutes" well before any crash would occur. This is a
/// property of the `toml` crate's handling of deeply-dotted table paths in general, not something
/// specific to `ProfileEngineSizer` or fixable from inside `build`/`validate` — it is a documented residual,
/// not a guard this bound provides.
pub const MAX_SIZER_DEPTH: usize = 4;

vike_utility::word_enum! {
    /// One `[engine.sizer]` `kind`: every concrete [`vike_portfolio::sizing::PositionSizer`] this crate
    /// ships, with the knobs its sizer cannot be built without ([`Self::requires`]) and whether it wraps
    /// a base sizer ([`Self::wraps_base`]). [`ProfileEngineSizer::build`] matches on it, and the
    /// profile's published schema lists it (`profile.json`'s `SizerKindName` roster and `sizer_kinds`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SizerKind {
        PassThrough => "pass_through",
        FixedDollar => "fixed_dollar",
        FixedShares => "fixed_shares",
        PercentEquity => "percent_equity",
        PercentVolatility => "percent_volatility",
        MaxRiskPercent => "max_risk_percent",
        PortfolioHeat => "portfolio_heat",
        DrawdownThrottle => "drawdown_throttle",
    }
}

impl SizerKind {
    /// The `[engine.sizer]` knobs this kind's sizer cannot be built without, in the order
    /// [`ProfileEngineSizer::build`] asks for them (`tests/sizer.rs` holds the two equal).
    #[must_use]
    pub const fn requires(self) -> &'static [&'static str] {
        match self {
            SizerKind::PassThrough => &[],
            SizerKind::FixedDollar => &["amount"],
            SizerKind::FixedShares => &["shares"],
            SizerKind::PercentEquity | SizerKind::PercentVolatility | SizerKind::MaxRiskPercent => {
                &["percent"]
            }
            SizerKind::PortfolioHeat => &["max_heat"],
            SizerKind::DrawdownThrottle => &["sensitivity", "floor"],
        }
    }

    /// It wraps the sizer `[engine.sizer.base]` declares, which it requires; every other kind reads
    /// no base and REFUSES one.
    #[must_use]
    pub const fn wraps_base(self) -> bool {
        matches!(self, SizerKind::PortfolioHeat | SizerKind::DrawdownThrottle)
    }
}

impl ProfileEngineSizer {
    /// Resolve the declared kind into the engine's trait object.
    ///
    /// ⚠ An unknown spelling — a kind missing one of its own required knobs — or a `base` chain
    /// past [`MAX_SIZER_DEPTH`] — is a `HarnessError::Validation` naming the valid set (or the
    /// limit), never a silent fallback to "no sizing", which would run the whole backtest unsized
    /// while the operator read the report as a sized one. This is the
    /// [`ProfileEngine::fill_model_kind`] rule.
    pub fn build(&self) -> Result<Box<dyn PositionSizer>, HarnessError> {
        self.build_at_depth(1)
    }

    /// [`Self::build`]'s actual recursion, carrying the depth of `self` in the chain (the
    /// outermost `[engine.sizer]` table is depth 1) so a `base` past [`MAX_SIZER_DEPTH`] is
    /// refused before it is even matched against a `kind`, bounding this function's own stack
    /// usage to `MAX_SIZER_DEPTH + 1` frames regardless of how deep the DESERIALIZED value it was
    /// handed already is.
    fn build_at_depth(&self, depth: usize) -> Result<Box<dyn PositionSizer>, HarnessError> {
        if depth > MAX_SIZER_DEPTH {
            return Err(HarnessError::from(ProfileRefusal::EngineSizerNestingTooDeep {
                depth,
                bases: ".base".repeat(depth - 1),
            }));
        }
        fn need(field: &str, kind: &str, v: Option<f64>) -> Result<f64, HarnessError> {
            v.ok_or_else(|| {
                HarnessError::from(ProfileRefusal::EngineSizerKnobRequired {
                    field: field.to_string(),
                    kind: kind.to_string(),
                })
            })
        }
        let kind = self.kind.trim().to_ascii_lowercase();
        let Some(sizer_kind) = SizerKind::from_word(&kind) else {
            return Err(HarnessError::from(ProfileRefusal::EngineSizerKindUnknown { other: kind }));
        };
        let sizer: Box<dyn PositionSizer> = match sizer_kind {
            SizerKind::PassThrough => Box::new(PositionSizerPassThrough),
            // A profile states neither sizer's commission nor its quantity step: both stay off (`Default`).
            SizerKind::FixedDollar => Box::new(PositionSizerFixedDollar {
                amount: need("amount", &kind, self.amount)?,
                ..Default::default()
            }),
            SizerKind::FixedShares => {
                Box::new(PositionSizerFixedShares { shares: need("shares", &kind, self.shares)? })
            }
            SizerKind::PercentEquity => Box::new(PositionSizerPercentEquity {
                percent: need("percent", &kind, self.percent)?,
                ..Default::default()
            }),
            SizerKind::PercentVolatility => Box::new(PositionSizerPercentVolatility {
                percent: need("percent", &kind, self.percent)?,
            }),
            SizerKind::MaxRiskPercent => Box::new(PositionSizerMaxRiskPercent {
                percent: need("percent", &kind, self.percent)?,
            }),
            SizerKind::PortfolioHeat => {
                let base = self.base.as_ref().ok_or_else(|| {
                    HarnessError::from(ProfileRefusal::EngineSizerPortfolioHeatNeedsBase)
                })?;
                Box::new(PositionSizerPortfolioHeat {
                    base: base.build_at_depth(depth + 1)?,
                    max_heat: need("max_heat", &kind, self.max_heat)?,
                })
            }
            SizerKind::DrawdownThrottle => {
                let base = self.base.as_ref().ok_or_else(|| {
                    HarnessError::from(ProfileRefusal::EngineSizerDrawdownThrottleNeedsBase)
                })?;
                Box::new(PositionSizerDrawdownThrottle {
                    base: base.build_at_depth(depth + 1)?,
                    sensitivity: need("sensitivity", &kind, self.sensitivity)?,
                    floor: need("floor", &kind, self.floor)?,
                })
            }
        };
        // ⚠ Only the WRAPPING arms above ever read [`Self::base`]; every other arm builds one sizer and
        // never looks at it. So a `[engine.sizer.base]` table under a scalar kind parses, validates,
        // and runs ONE sizer while the operator reads the profile as a chain — the silent-no-op class
        // this whole surface exists to refuse. Checked here, AFTER the match, so an unknown kind still
        // gets its own message above, which is the more useful one. `SizerKind::wraps_base` is the one
        // list of wrapping kinds, and `tests/sizer.rs` holds it equal to the arms that read `base`.
        if self.base.is_some() && !sizer_kind.wraps_base() {
            return Err(HarnessError::from(ProfileRefusal::EngineSizerBaseUnderScalarKind {
                kind: kind.clone(),
                bases: ".base".repeat(depth - 1),
            }));
        }
        Ok(sizer)
    }
}
