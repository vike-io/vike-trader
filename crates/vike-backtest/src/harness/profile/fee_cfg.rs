//! The `[engine.fee]` table: `FeeCfg`, a fee schedule named by shape or by venue.

use std::collections::BTreeSet;

use serde::Deserialize;

use crate::harness::HarnessError;
use crate::hist_replay::SeriesRef;
use vike_model::FeeSchedule;

#[cfg(doc)]
use super::{BacktestProfile, EngineCfg};

/// TOML shape of the opt-in fee schedule ([`EngineCfg::fee`]) — a [`vike_model::FeeSchedule`]
/// named from a profile, by SHAPE or by VENUE.
///
/// # Why a shape rather than a rate
///
/// [`EngineCfg::fee_rate`] is one flat fraction of notional, and `vike_model::money::fees` expresses
/// five shapes of which only one IS that. This table used to reach exactly one of the other four
/// (`probability_scaled`), so the remaining three were shapes a profile could not cost a run at.
/// The two that take real money when they are missing:
///
/// * **`per_share_with_floor`** — the Interactive Brokers equities fixed schedule
///   ([`vike_model::FeeSchedule::PerShareWithFloor`], whose real rates live in
///   `crates/vike-model/src/money/fees.rs`'s `fee_schedule_for` `"ibkr"` arm): a per-share fee, a
///   per-order MINIMUM, and a percent-of-notional cap. **The minimum is the whole point**, and no
///   flat rate has one: a flat fee scales with size all the way to zero, so a small-size
///   high-frequency configuration backtests as viable and is then eaten by the broker minimum
///   live. That is the failure this key exists to surface before the money is real, and it is
///   why `min` is REQUIRED rather than defaulted — a forgotten floor is exactly the profile that
///   reads as priced and prices at nothing.
/// * **`percent_of_underlying`** — Deribit's options rule, `min(bps × underlying, cap × premium)`
///   ([`vike_model::FeeSchedule::PercentOfUnderlying`]). The cap is the buyer-protecting half and
///   binds for cheap deep-OTM options, so it too is REQUIRED — see the field doc, where a zero
///   cap is refused because `FeeSchedule::commission` would then charge exactly nothing.
///
/// ⚠ **What this crate still cannot do with the Deribit shape, stated here because the type name
/// promises more than the engine delivers.** The accurate figure needs the UNDERLYING price and
/// this engine never has one: `FeeSchedule::commission_with_underlying` has NO caller anywhere in
/// `vike-backtest`, and the one caller in the tree
/// (`crates/vike-paper/src/lib.rs`'s `PaperExecutionClient::commission_for`) is gated on an
/// `underlying_source` that no production code supplies. So a `percent_of_underlying` run is
/// charged the premium-cap-bounded approximation `min(bps × premium, cap × premium)`, which
/// UNDERSTATES the real fee for an option priced well below its underlying. Configuring the shape
/// still buys the cap and the correct type; it does not buy the underlying leg.
///
/// # Why a VENUE and not a number
///
/// `kind = "venue"` costs the run at the venue's own published schedule — the SAME answer the
/// paper/live mount gets, reached through the same two functions rather than a table copied here:
/// `vike_catalog::fee_lane(venue, symbol)` then [`vike_model::fee_schedule_for`], which is
/// verbatim what `crates/vike-mount/src/engine.rs`'s `make_engine` computes for its `static_default`.
/// The lane resolution is the load-bearing half: a `BTCUSDT.P` symbol resolves to
/// `"binance-perp"`, and before the mount learned that, every `.P` paper mount was charged the
/// SPOT row. A backtest that hand-types a rate can disagree with the paper mount of the same
/// instrument for no reason but the missing lookup, and that disagreement is invisible in both
/// reports.
///
/// ```toml
/// [engine.fee]
/// kind = "probability_scaled"
/// taker_rate = 0.072          # the live `cheap_np` cost: 0.072·p·(1−p) per share
///
/// # ...or the IBKR equities shape, whose $ minimum a flat rate cannot express:
/// # kind = "per_share_with_floor"
/// # per_share = 0.005
/// # min = 1.0
/// # max_pct = 0.005
///
/// # ...or "cost this run the way the mount would":
/// # kind = "venue"
/// # venue = "binance"         # symbol defaults to the run's own series on that venue
/// ```
///
/// # What reaches the fill EXACTLY, and what flattens
///
/// [`vike_sim::StrategyEngine::new`] routes a schedule one of two ways (its own comment is the
/// authority): [`vike_model::FeeSchedule::PercentMakerTaker`] and
/// [`vike_model::FeeSchedule::Free`] FLATTEN to the `(maker, taker)` fractions the frozen
/// `size × price × rate × multiplier` fold has always taken — for those two a flat fraction IS
/// the whole shape — and every other shape is carried to the fill site and applied through
/// `FeeSchedule::commission` at the price the fill transacted at. That inversion is what makes
/// this key mean anything: `maker_taker_rates()` reports `(0.0, 0.0)` for `PerShareWithFloor`, so
/// a flattened floor charges ZERO while the profile reads as priced.
///
/// ⚠ **One wrinkle on `per_share_with_floor` that this table cannot police**: the exact path
/// multiplies the schedule's answer by the symbol's CONTRACT MULTIPLIER, which is right for the
/// notional cap and wrong for the per-share term and the floor (a `$1.00` minimum is one dollar,
/// not one dollar per contract). It is exact at the default `engine.multiplier = 1.0`, which is
/// what every equities profile runs at. A refusal would have to read `[engine]` and this struct
/// at once, which only [`BacktestProfile::refusals`] can do.
///
/// NOTE none of this changes [`vike_model::fee_schedule_for`]'s registry default for
/// `"polymarket"` (still `Free`) — the paper fill path and the snapshot cost display read that
/// registry, and flipping it would move existing users' numbers. A profile that wants the
/// verified V2 curve asks for it, either by naming the rates under `probability_scaled` or with
/// the `pm_curve` flag below.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeeCfg {
    /// Which [`vike_model::FeeSchedule`] shape this table describes: `"probability_scaled"` |
    /// `"per_share_with_floor"` | `"percent_of_underlying"` | `"percent_maker_taker"` |
    /// `"venue"` | `"free"`.
    ///
    /// ⚠ The accepted set is [`FeeCfg::KINDS`] and the unknown-`kind` refusal is BUILT from it,
    /// so the roster an operator is shown cannot drift from the roster [`FeeCfg::build_for`]
    /// matches on. An unrecognised value fails the profile at load — never a silent fallback,
    /// because a fallback here prices every fill at something the operator did not write.
    pub kind: String,
    /// `probability_scaled`: the TAKER fraction of the `p(1−p)`-scaled share count.
    #[serde(default)]
    pub taker_rate: f64,
    /// `probability_scaled`: the MAKER fraction, before any rebate.
    #[serde(default)]
    pub maker_rate: f64,
    /// `probability_scaled`: the maker's rebate as a share of the EQUIVALENT TAKER fee, so a
    /// zero `maker_rate` with a positive share is a NEGATIVE maker commission (a rebate — the
    /// sign convention on [`vike_model::FeeSchedule`]).
    ///
    /// ⚠ These three are plain defaulted scalars rather than `Option`s, which is why the
    /// cross-kind rule below can only refuse a NON-ZERO one under another kind: serde cannot
    /// tell `taker_rate = 0.0` from an absent key, and a written zero configures nothing either
    /// way. Every knob added since is an `Option`, which is what lets a wrong-kind knob be
    /// refused rather than ignored.
    #[serde(default)]
    pub maker_rebate_share: f64,
    /// `per_share_with_floor`: the per-SHARE fee. REQUIRED by that kind.
    #[serde(default)]
    pub per_share: Option<f64>,
    /// `per_share_with_floor`: the per-order MINIMUM. REQUIRED by that kind, and required
    /// deliberately — an absent floor defaults to no floor, which is the flat-rate behaviour the
    /// whole shape exists to escape. Write `min = 0.0` to assert there is genuinely none.
    #[serde(default)]
    pub min: Option<f64>,
    /// `per_share_with_floor`: the maximum fraction of trade value the fee may reach. Optional —
    /// absent is `0.0`, which `FeeSchedule::commission` treats as NO ceiling (it caps only on a
    /// positive notional bound), so omitting it charges the uncapped per-share-or-floor figure.
    /// That default is safe in the direction that matters: it can only over-charge.
    #[serde(default)]
    pub max_pct: Option<f64>,
    /// `percent_of_underlying`: basis points of the UNDERLYING notional. REQUIRED by that kind.
    #[serde(default)]
    pub bps: Option<f64>,
    /// `percent_of_underlying`: the cap, as a fraction of the PREMIUM notional (Deribit's real
    /// rule caps the underlying term at 12.5 % of the premium). REQUIRED, and a non-positive
    /// value is REFUSED rather than read as "no cap": `FeeSchedule::commission` on this shape is
    /// `min(bps × premium, cap × premium)`, so a zero cap makes the whole commission zero — the
    /// silent free backtest, wearing a configured fee model.
    #[serde(default)]
    pub premium_cap_pct: Option<f64>,
    /// `percent_maker_taker`: maker basis points of quote-notional. At least one of this and
    /// [`Self::taker_bps`] is required; the other defaults to `0.0`.
    ///
    /// This kind is the one shape [`EngineCfg::fee_rate`] can already express, and it is here so
    /// a profile can say the two SIDES apart (`fee_rate` charges one number to both). A
    /// genuinely zero-fee venue writes the zero explicitly, which makes it an assertion instead
    /// of an omission.
    #[serde(default)]
    pub maker_bps: Option<f64>,
    /// `percent_maker_taker`: taker basis points of quote-notional. See [`Self::maker_bps`].
    #[serde(default)]
    pub taker_bps: Option<f64>,
    /// `venue`: which venue's published schedule to cost the run at. REQUIRED by that kind, and
    /// it is not inferred from `[data]` — `EngineParams::fee_schedule` is ONE schedule for the
    /// whole run, so on a cross-venue `[[data.series]]` slice there is no single right answer to
    /// infer and naming it is the operator asserting which venue's book this run is priced at.
    #[serde(default)]
    pub venue: Option<String>,
    /// `venue`: which SYMBOL selects the venue's fee LANE. Absent (the normal case) resolves from
    /// the run's own series on [`Self::venue`], which is what keeps the `.P` suffix from being
    /// typed twice — see [`FeeCfg::build_for`] for the two ways that resolution refuses.
    ///
    /// Set it outright when the run's series carry a symbol spelling the lane resolver cannot
    /// read, or to state the lane a mixed run is costed at.
    #[serde(default)]
    pub symbol: Option<String>,
    /// `venue`, polymarket only: take the verified 2026 V2 fee regime
    /// ([`vike_model::POLYMARKET_V2_FEE_CURVE`], via
    /// [`vike_model::fee_schedule_for_with_pm_curve`]) instead of the registry's `Free`.
    ///
    /// ⚠ It exists because `kind = "venue"` on polymarket otherwise costs the run at EXACTLY
    /// ZERO — `fee_schedule_for("polymarket")` is a deliberate `Free` that cannot be flipped
    /// without moving every existing consumer's numbers — and a free prediction-market backtest
    /// is the flattering direction. Absent or `false` is byte-identical to what the mount
    /// resolves. `true` on any other venue is REFUSED: that function delegates to
    /// [`vike_model::fee_schedule_for`] for every venue but polymarket, so the flag would
    /// configure nothing while the operator read the run as curve-priced.
    #[serde(default)]
    pub pm_curve: Option<bool>,
}

impl FeeCfg {
    /// The accepted `kind` values — the ONE roster, both matched on by [`Self::build_for`] and
    /// printed by the unknown-`kind` refusal, so an operator can never be shown a set the code
    /// does not accept.
    pub const KINDS: &[&str] = &[
        "probability_scaled",
        "per_share_with_floor",
        "percent_of_underlying",
        "percent_maker_taker",
        "venue",
        "free",
    ];

    /// Every knob this table declares that is actually SET, paired with the ONE `kind` that
    /// reads it.
    ///
    /// Each knob belongs to exactly one kind, which is what makes the cross-kind rule in
    /// [`Self::validate_shape`] a single loop rather than a pairwise matrix — and what makes a
    /// NEW knob join that rule by adding one row here instead of by being remembered.
    fn set_keys(&self) -> Vec<(&'static str, &'static str)> {
        let mut out = Vec::new();
        for (key, owner, is_set) in [
            ("taker_rate", "probability_scaled", self.taker_rate != 0.0),
            ("maker_rate", "probability_scaled", self.maker_rate != 0.0),
            ("maker_rebate_share", "probability_scaled", self.maker_rebate_share != 0.0),
            ("per_share", "per_share_with_floor", self.per_share.is_some()),
            ("min", "per_share_with_floor", self.min.is_some()),
            ("max_pct", "per_share_with_floor", self.max_pct.is_some()),
            ("bps", "percent_of_underlying", self.bps.is_some()),
            ("premium_cap_pct", "percent_of_underlying", self.premium_cap_pct.is_some()),
            ("maker_bps", "percent_maker_taker", self.maker_bps.is_some()),
            ("taker_bps", "percent_maker_taker", self.taker_bps.is_some()),
            ("venue", "venue", self.venue.is_some()),
            ("symbol", "venue", self.symbol.is_some()),
            ("pm_curve", "venue", self.pm_curve.is_some()),
        ] {
            if is_set {
                out.push((key, owner));
            }
        }
        out
    }

    /// Every check that needs neither the data slice nor any I/O: the `kind` itself, that each
    /// SET knob belongs to that kind, that each numeric knob is finite and non-negative, and that
    /// the kind's REQUIRED knobs are present.
    ///
    /// Split out of [`Self::build_for`] for the reason `ResolutionCfg::validate_shape` was: this
    /// half is answerable at LOAD, and [`BacktestProfile::refusals`] runs it there through
    /// [`Self::build`]. The slice-dependent half cannot be, and says so at its own site.
    pub(super) fn validate_shape(&self) -> Result<(), HarnessError> {
        let kind = self.kind.as_str();
        if !Self::KINDS.contains(&kind) {
            return Err(HarnessError::Validation(format!(
                "unknown engine.fee.kind {kind:?} (known: {}; a single flat fraction of notional \
                 may also stay on engine.fee_rate, which is the same shape as \
                 percent_maker_taker charged to both sides)",
                Self::KINDS.join(" | ")
            )));
        }
        for (key, owner) in self.set_keys() {
            if owner != kind {
                return Err(HarnessError::Validation(format!(
                    "engine.fee.{key} is a knob of kind {owner:?}, and this table declares kind \
                     {kind:?} — nothing would read it. A COST knob that configures nothing is \
                     worse than an absent one: the operator reads the profile as priced at what \
                     they wrote and the run prices at something else. Set the kind this knob \
                     belongs to, or drop the knob."
                )));
            }
        }
        for (name, v) in [
            ("taker_rate", Some(self.taker_rate)),
            ("maker_rate", Some(self.maker_rate)),
            ("maker_rebate_share", Some(self.maker_rebate_share)),
            ("per_share", self.per_share),
            ("min", self.min),
            ("max_pct", self.max_pct),
            ("bps", self.bps),
            ("premium_cap_pct", self.premium_cap_pct),
            ("maker_bps", self.maker_bps),
            ("taker_bps", self.taker_bps),
        ] {
            let Some(v) = v else { continue };
            if !v.is_finite() || v < 0.0 {
                return Err(HarnessError::Validation(format!(
                    "engine.fee.{name} must be finite and >= 0, got {v}"
                )));
            }
        }
        match kind {
            "per_share_with_floor" => {
                if self.per_share.is_none() || self.min.is_none() {
                    return Err(HarnessError::Validation(
                        "engine.fee.kind = \"per_share_with_floor\" needs BOTH per_share and \
                         min. The floor is the whole reason this shape exists — a per-share fee \
                         with no minimum is a flat rate wearing a different name, and it is the \
                         configuration that makes a small-size high-frequency run backtest as \
                         viable and then lose to the broker minimum live. Write min = 0.0 to \
                         assert there is genuinely no floor."
                            .to_string(),
                    ));
                }
            }
            "percent_of_underlying" => {
                if self.bps.is_none() || self.premium_cap_pct.is_none() {
                    return Err(HarnessError::Validation(
                        "engine.fee.kind = \"percent_of_underlying\" needs BOTH bps and \
                         premium_cap_pct — the cap is half the rule it models \
                         (min(bps x underlying, cap x premium)), and it is the half that binds \
                         for a cheap deep-OTM option."
                            .to_string(),
                    ));
                }
                if self.premium_cap_pct.is_some_and(|c| c <= 0.0) {
                    return Err(HarnessError::Validation(format!(
                        "engine.fee.premium_cap_pct must be > 0, got {} — a zero cap is not \
                         \"no cap\". FeeSchedule::commission on this shape is \
                         min(bps x premium, cap x premium), so a zero cap zeroes the whole \
                         commission and the run pays no fee at all while the profile reads as \
                         fee-modelled. crates/vike-model/src/money/fees.rs's fee_schedule_for carries \
                         the real Deribit cap in its \"deribit\" arm.",
                        self.premium_cap_pct.unwrap_or(0.0)
                    )));
                }
            }
            "percent_maker_taker" => {
                if self.maker_bps.is_none() && self.taker_bps.is_none() {
                    return Err(HarnessError::Validation(
                        "engine.fee.kind = \"percent_maker_taker\" needs at least one of \
                         maker_bps / taker_bps — a table naming neither describes no cost, which \
                         is what kind = \"free\" says on purpose. A genuinely zero-fee side is \
                         written as the explicit 0.0, so the zero is an assertion rather than an \
                         omission."
                            .to_string(),
                    ));
                }
            }
            "venue" => {
                if self.venue.is_none() {
                    return Err(HarnessError::Validation(
                        "engine.fee.kind = \"venue\" needs engine.fee.venue — it is not inferred \
                         from [data]. EngineParams::fee_schedule is ONE schedule for the whole \
                         run, so a cross-venue slice has no single answer to infer, and naming \
                         the venue is the operator asserting which venue's book this run is \
                         costed at."
                            .to_string(),
                    ));
                }
                if self.pm_curve == Some(true) && self.venue.as_deref() != Some("polymarket") {
                    return Err(HarnessError::Validation(format!(
                        "engine.fee.pm_curve is polymarket-only, and this table names venue {:?} \
                         — vike_model::fee_schedule_for_with_pm_curve delegates to \
                         fee_schedule_for for every other venue, so the flag would configure \
                         nothing while the run was read as curve-priced. Drop it.",
                        self.venue.as_deref().unwrap_or("")
                    )));
                }
            }
            // Every accepted kind either has an arm above or requires no knob at all
            // (`probability_scaled`, whose three rates are all optional, and `free`, which takes
            // none). Membership was checked at the top of this function, so an unrecognised kind
            // never reaches here — an empty arm rather than a panic, because aborting the process
            // is not a thing a config path may do.
            _ => {}
        }
        Ok(())
    }

    /// Resolve to a [`vike_model::FeeSchedule`] WITHOUT the run's data slice — the door
    /// [`BacktestProfile::refusals`] drives at load, where the slice-dependent half of
    /// `kind = "venue"` cannot be answered.
    ///
    /// Identical to [`Self::build_for`] for every shape kind. For `kind = "venue"` with no
    /// explicit [`Self::symbol`] it resolves the venue's BARE lane, which on both dual-lane
    /// venues is the SPOT row — the more expensive one of the two
    /// (`crates/vike-model/src/money/fees.rs`'s `fee_schedule_for` `"binance"`/`"binance-perp"` arms
    /// carry the measurement), so this door over-charges rather than flatters. The harness never
    /// takes it for a real run: `crate::harness::run`'s two `EngineParams` construction sites
    /// both call [`Self::build_for`] with the profile's resolved series.
    pub fn build(&self) -> Result<FeeSchedule, HarnessError> {
        self.build_for(&[])
    }

    /// Resolve to a [`vike_model::FeeSchedule`] for a run over `series` — the door the harness
    /// uses, and the only one that can get a `kind = "venue"` LANE right.
    ///
    /// ⚠ **Two refusals here are RUN-TIME where every neighbour's is load-time, and the reason is
    /// structural rather than a preference**: they compare this table against the resolved data
    /// slice, and [`BacktestProfile::refusals`] is the only function that sees both. That is the
    /// same split `ResolutionCfg::build` already carries for its winners-coverage check. They
    /// always fire — both `EngineParams` construction sites reach this before a bar is loaded —
    /// but they fire when the run starts rather than when the profile loads, so a `--validate`
    /// pass does not show them.
    pub fn build_for(&self, series: &[SeriesRef]) -> Result<FeeSchedule, HarnessError> {
        self.validate_shape()?;
        // Every `unwrap_or` below is unreachable: `validate_shape` has already refused an absent
        // REQUIRED knob, and the fallback is the identity of the optional ones.
        match self.kind.as_str() {
            "probability_scaled" => Ok(FeeSchedule::ProbabilityScaled {
                taker_rate: self.taker_rate,
                maker_rate: self.maker_rate,
                maker_rebate_share: self.maker_rebate_share,
            }),
            "per_share_with_floor" => Ok(FeeSchedule::PerShareWithFloor {
                per_share: self.per_share.unwrap_or(0.0),
                min: self.min.unwrap_or(0.0),
                max_pct: self.max_pct.unwrap_or(0.0),
            }),
            "percent_of_underlying" => Ok(FeeSchedule::PercentOfUnderlying {
                bps: self.bps.unwrap_or(0.0),
                premium_cap_pct: self.premium_cap_pct.unwrap_or(0.0),
            }),
            "percent_maker_taker" => Ok(FeeSchedule::PercentMakerTaker {
                maker_bps: self.maker_bps.unwrap_or(0.0),
                taker_bps: self.taker_bps.unwrap_or(0.0),
            }),
            "venue" => {
                let venue = self.venue.as_deref().unwrap_or("");
                let lane = self.fee_lane_for(venue, series)?;
                Ok(if self.pm_curve == Some(true) {
                    vike_model::fee_schedule_for_with_pm_curve(lane)
                } else {
                    vike_model::fee_schedule_for(lane)
                })
            }
            "free" => Ok(FeeSchedule::Free),
            // Unreachable: `validate_shape` refused anything outside `Self::KINDS` before this
            // match ran. It is spelled as the unknown-kind refusal rather than as a panic so the
            // two doors can never disagree about what is accepted.
            other => Err(HarnessError::Validation(format!(
                "unknown engine.fee.kind {other:?} (known: {})",
                Self::KINDS.join(" | ")
            ))),
        }
    }

    /// The `vike_model::fee_schedule_for` LANE KEY this `kind = "venue"` table resolves to.
    ///
    /// ⚠ **Reached through `vike_catalog::fee_lane`, never re-derived.** That function owns the
    /// `.P` split and the per-contract-class sub-keys a venue may price apart, and it is the
    /// function `crates/vike-mount/src/engine.rs`'s `make_engine` calls for its own
    /// `static_default` — so the backtest and the paper mount of one instrument resolve the same
    /// row by construction rather than by two tables agreeing.
    ///
    /// With an explicit [`Self::symbol`] that is the whole job. Otherwise the lane comes from the
    /// run's own series ON THAT VENUE, and the two ways that can fail are refused rather than
    /// guessed:
    ///
    /// * **no series on the venue** — there is nothing to read a lane from, and answering with
    ///   the bare venue row would silently price a perp run at spot fees, which is the exact
    ///   defect the lane key was introduced to end.
    /// * **series straddling two lanes** — one `EngineParams::fee_schedule` cannot serve both,
    ///   and a venue's two lanes are priced completely differently, so either choice misprices
    ///   half the run.
    ///
    /// ⚠ Picking the FIRST series would be wrong even where it looks harmless: series order is
    /// meaningful (a non-tradeable reference feed is listed first on purpose), so the first entry
    /// of a cross-venue slice is routinely not on the venue being costed at all.
    fn fee_lane_for<'a>(
        &'a self,
        venue: &'a str,
        series: &[SeriesRef],
    ) -> Result<&'a str, HarnessError> {
        if let Some(sym) = self.symbol.as_deref() {
            return Ok(vike_catalog::fee_lane(venue, sym));
        }
        let mut lanes: BTreeSet<&'a str> = BTreeSet::new();
        for s in series.iter().filter(|s| s.venue == venue) {
            lanes.insert(vike_catalog::fee_lane(venue, &s.symbol));
        }
        match lanes.len() {
            // The slice-free door (`build`): no series were offered at all, so the bare-venue
            // row is the only honest answer and its own doc states the direction of the error.
            0 if series.is_empty() => Ok(vike_catalog::fee_lane(venue, "")),
            0 => Err(HarnessError::Validation(format!(
                "engine.fee.venue = {venue:?} names a venue this run does not load, so there is \
                 no symbol to resolve its fee LANE from — and answering with the bare-venue row \
                 would charge a perp run at the venue's SPOT schedule, the mispricing the lane \
                 key exists to end. This run's series are on: {}. Name a venue the slice trades, \
                 or set engine.fee.symbol outright.",
                series
                    .iter()
                    .map(|s| s.venue.as_str())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            1 => Ok(lanes.into_iter().next().unwrap_or(venue)),
            _ => Err(HarnessError::Validation(format!(
                "engine.fee.kind = \"venue\" resolves ONE schedule for the whole run, and this \
                 run's {venue:?} series straddle {} fee lanes ({}) — a venue prices its lanes \
                 completely differently (crates/vike-model/src/money/fees.rs's fee_schedule_for \
                 carries the measurement per lane), so one row would misprice the other lane. \
                 Split the run per lane, or set engine.fee.symbol to name the lane this run is \
                 costed at.",
                lanes.len(),
                lanes.into_iter().collect::<Vec<_>>().join(", ")
            ))),
        }
    }
}
