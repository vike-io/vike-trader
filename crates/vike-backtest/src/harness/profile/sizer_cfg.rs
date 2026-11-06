//! The `[engine.sizer]` table: `SizerCfg`, the position-sizer chain and its depth limit.

use serde::Deserialize;

use crate::harness::HarnessError;
use vike_analytics::sizing::{
    DrawdownThrottleSizer, FixedDollarSizer, FixedSharesSizer, MaxRiskPctSizer, PassThroughSizer,
    PctEquitySizer, PctVolatilitySizer, PortfolioHeatSizer, PositionSizer,
};

#[cfg(doc)]
use super::EngineCfg;

/// TOML shape of the opt-in position sizer ([`EngineCfg::sizer`]) — how a strategy's requested
/// size becomes an order size, via [`vike_analytics::sizing`]'s swappable `PositionSizer`
/// framework (the WealthLab PosSizer port). Every concrete sizer that crate ships gets a `kind`
/// row here and NOTHING else — a new sizer added there with no row here is unreachable from a
/// profile, not silently mapped to the nearest existing one.
///
/// Two kinds — `"portfolio_heat"` and `"drawdown_throttle"` — WRAP a base sizer rather than
/// standing alone (see [`vike_analytics::sizing::PortfolioHeatSizer`]/[`vike_analytics::sizing::DrawdownThrottleSizer`]);
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
#[serde(deny_unknown_fields)]
pub struct SizerCfg {
    /// One of `"pass_through"` | `"fixed_dollar"` | `"fixed_shares"` | `"pct_equity"` |
    /// `"pct_volatility"` | `"max_risk_pct"` | `"portfolio_heat"` | `"drawdown_throttle"` — every
    /// concrete [`vike_analytics::sizing::PositionSizer`] this crate ships and nothing else
    /// ([`Self::build`]).
    pub kind: String,
    /// [`vike_analytics::sizing::FixedDollarSizer::amount`] — fixed cash notional per entry. Required for
    /// `"fixed_dollar"`.
    #[serde(default)]
    pub amount: Option<f64>,
    /// [`vike_analytics::sizing::FixedSharesSizer::shares`] — fixed share/contract count per entry.
    /// Required for `"fixed_shares"`.
    #[serde(default)]
    pub shares: Option<f64>,
    /// The knob `"pct_equity"` / `"pct_volatility"` / `"max_risk_pct"` each read as their own
    /// fraction of equity (target notional, ATR-risk budget, and stop-risk budget respectively —
    /// see [`vike_analytics::sizing`]'s doc on each). Required for those three kinds.
    #[serde(default)]
    pub pct: Option<f64>,
    /// [`vike_analytics::sizing::PortfolioHeatSizer::max_heat`] — total open-risk cap as a fraction of
    /// equity. Required for `"portfolio_heat"`.
    #[serde(default)]
    pub max_heat: Option<f64>,
    /// [`vike_analytics::sizing::DrawdownThrottleSizer::sensitivity`]. Required for `"drawdown_throttle"`.
    #[serde(default)]
    pub sensitivity: Option<f64>,
    /// [`vike_analytics::sizing::DrawdownThrottleSizer::floor`]. Required for `"drawdown_throttle"`.
    #[serde(default)]
    pub floor: Option<f64>,
    /// The wrapped sizer for `"portfolio_heat"` / `"drawdown_throttle"` — required for those two
    /// kinds ([`Self::build`] refuses their absence), and REFUSED under every other kind, which
    /// reads it nowhere: a base under a scalar kind would run one sizer while the profile read as
    /// a chain. Boxed because `SizerCfg` nests itself — see [`MAX_SIZER_DEPTH`] for the bound on
    /// how far.
    #[serde(default)]
    pub base: Option<Box<SizerCfg>>,
}

/// Bound on how many `SizerCfg` nodes one `[engine.sizer]` chain may nest (the top-level table
/// counts as depth 1; each `[engine.sizer.base…]` adds one).
///
/// `SizerCfg` is the first self-referential config struct in this file — `FeeCfg`/`ImpactCfg`/
/// `ResolutionCfg` are all flat — and [`SizerCfg::build`]'s recursion through [`SizerCfg::base`] is
/// one stack frame per level. Only the two WRAPPING kinds (`"portfolio_heat"`,
/// `"drawdown_throttle"`) ever consume a `base` at all, so the deepest MEANINGFUL profile composes
/// both of them once around one terminal scalar sizer — three `SizerCfg` nodes, e.g.
/// `drawdown_throttle` -> `portfolio_heat` -> `fixed_dollar`. `4` leaves exactly one level of
/// headroom past that (e.g. layering the same wrapping kind twice, such as two `portfolio_heat`
/// tiers at different `max_heat` caps) without leaving the bound so loose that an arbitrarily deep
/// `[engine.sizer.base.base.base…]` chain — no config file has a legitimate reason to nest further
/// than a human would hand-write — reads as accepted rather than refused.
///
/// ⚠ This bounds [`SizerCfg::build`]'s OWN recursion, not `toml`'s — and MEASURED (module test
/// `sizer_depth_probe`, ignored; the Task 12 fix-round report carries the full numbers) is a
/// genuine gap between the two. Deserializing the TOML into nested `SizerCfg`/`Box<SizerCfg>`
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
/// specific to `SizerCfg` or fixable from inside `build`/`validate` — it is a documented residual,
/// not a guard this bound provides.
pub const MAX_SIZER_DEPTH: usize = 4;

impl SizerCfg {
    /// Resolve the declared kind into the engine's trait object.
    ///
    /// ⚠ An unknown spelling — a kind missing one of its own required knobs — or a `base` chain
    /// past [`MAX_SIZER_DEPTH`] — is a `HarnessError::Validation` naming the valid set (or the
    /// limit), never a silent fallback to "no sizing", which would run the whole backtest unsized
    /// while the operator read the report as a sized one. This is the
    /// [`EngineCfg::fill_model_kind`] rule.
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
            return Err(HarnessError::Validation(format!(
                "engine.sizer nesting is {depth} levels deep ([engine.sizer{}]) — \
                 MAX_SIZER_DEPTH is {MAX_SIZER_DEPTH}; flatten the base chain (no composition of \
                 the two wrapping kinds needs to nest this deep)",
                ".base".repeat(depth - 1)
            )));
        }
        fn need(field: &str, kind: &str, v: Option<f64>) -> Result<f64, HarnessError> {
            v.ok_or_else(|| {
                HarnessError::Validation(format!(
                    "engine.sizer.{field} is required when engine.sizer.kind = {kind:?}"
                ))
            })
        }
        let kind = self.kind.trim().to_ascii_lowercase();
        let sizer: Box<dyn PositionSizer> = match kind.as_str() {
            "pass_through" => Box::new(PassThroughSizer),
            "fixed_dollar" => {
                Box::new(FixedDollarSizer { amount: need("amount", &kind, self.amount)? })
            }
            "fixed_shares" => {
                Box::new(FixedSharesSizer { shares: need("shares", &kind, self.shares)? })
            }
            "pct_equity" => Box::new(PctEquitySizer { pct: need("pct", &kind, self.pct)? }),
            "pct_volatility" => Box::new(PctVolatilitySizer { pct: need("pct", &kind, self.pct)? }),
            "max_risk_pct" => Box::new(MaxRiskPctSizer { pct: need("pct", &kind, self.pct)? }),
            "portfolio_heat" => {
                let base = self.base.as_ref().ok_or_else(|| {
                    HarnessError::Validation(
                        "engine.sizer.kind = \"portfolio_heat\" wraps a base sizer — add an \
                         [engine.sizer.base] table naming it"
                            .to_string(),
                    )
                })?;
                Box::new(PortfolioHeatSizer {
                    base: base.build_at_depth(depth + 1)?,
                    max_heat: need("max_heat", &kind, self.max_heat)?,
                })
            }
            "drawdown_throttle" => {
                let base = self.base.as_ref().ok_or_else(|| {
                    HarnessError::Validation(
                        "engine.sizer.kind = \"drawdown_throttle\" wraps a base sizer — add an \
                         [engine.sizer.base] table naming it"
                            .to_string(),
                    )
                })?;
                Box::new(DrawdownThrottleSizer {
                    base: base.build_at_depth(depth + 1)?,
                    sensitivity: need("sensitivity", &kind, self.sensitivity)?,
                    floor: need("floor", &kind, self.floor)?,
                })
            }
            other => {
                return Err(HarnessError::Validation(format!(
                    "unknown engine.sizer.kind {other:?} (want pass_through | fixed_dollar | \
                     fixed_shares | pct_equity | pct_volatility | max_risk_pct | portfolio_heat \
                     | drawdown_throttle)"
                )));
            }
        };
        // ⚠ Only the two WRAPPING arms above ever read [`Self::base`]; every other arm builds one
        // sizer and never looks at it. So a `[engine.sizer.base]` table under a scalar kind
        // parses, validates, and runs ONE sizer while the operator reads the profile as a chain —
        // the silent-no-op class this whole surface exists to refuse. Checked here, AFTER the
        // match, so exactly one list of wrapping kinds exists (an unknown kind still gets its own
        // message above, which is the more useful one). ⚠ A new wrapping kind must be added to
        // this `matches!` as well as to the match above, or its base reads as forbidden.
        if self.base.is_some() && !matches!(kind.as_str(), "portfolio_heat" | "drawdown_throttle") {
            return Err(HarnessError::Validation(format!(
                "engine.sizer.kind = {kind:?} does not wrap a base sizer, but \
                 [engine.sizer{}.base] is set — only \"portfolio_heat\" and \"drawdown_throttle\" \
                 read it, so the chain would run {kind:?} alone. Remove the base table, or name a \
                 wrapping kind",
                ".base".repeat(depth - 1)
            )));
        }
        Ok(sizer)
    }
}
