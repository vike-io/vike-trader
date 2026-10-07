//! **What a Studio run is COSTED under, resolved from the slice the DTO door already carries** —
//! and, when it cannot be resolved, said out loud rather than silently left at zero.
//!
//! Ruling: `docs/decisions/0063-the-studio-optimizer-derives-its-cost-model-and-declares-what-it-cannot.md`.
//!
//! # The defect this exists to end
//!
//! Every Studio run before this module was a **zero-cost backtest**, and nothing in its report said
//! so. `EngineParams`' `Default` sets a zero fee rate and zero slippage; `crate::wire_run`'s
//! `to_engine_params` starts from that default and applies only the fields a caller SENT, and the
//! GUI's remote path sends `None` on all three verbs. So the fixed walk has been reporting
//! confident numbers chosen under no cost model at all. That is worse than the hazard `0047`
//! feared when it refused the Studio a search arm, and it is the first thing the arm has to fix —
//! because a SEARCH ranks candidates, and a cost model is where ranking stops scaling and starts
//! CHOOSING.
//!
//! # Derived, not carried
//!
//! `0047`'s load-bearing claim was that the DTO cannot carry the cost model a winner is chosen
//! under. It does not need to. `WireSlice` already carries `venue` and `symbols`, and
//! `vike_model::money::fees`' `fee_schedule_for` maps a fee LANE to the same `FeeSchedule` the live path
//! reads. So the three flat scalars stop being the only expressible thing and become an explicit
//! OVERRIDE, which is a strictly smaller change than widening the wire.
//!
//! ⚠ **The lane, never the bare venue id.** `fee_schedule_for`'s own doc forbids keying it off a
//! venue string: binance and aster each front a SPOT and a PERP order API behind one id, selected
//! by a trailing `.P` on the symbol, and the two are priced differently.
//! `vike_catalog::fee_lane` owns that split. Deriving from `venue` alone would charge a perp
//! slice the spot rates — and a RANKING under that is worse than a scaling under it, because the
//! maker/taker asymmetry differs per lane.
//!
//! # What deriving is worth on day one, stated rather than oversold
//!
//! The engine DOES classify a fill as maker or taker (`SimBroker::apply_fill` takes an `is_maker`
//! and selects the rate with it), so the mechanism this plugs into is real. Two qualifications
//! bound what it buys, and both are `0063`'s:
//!
//! * the classification reads the order KIND, not crossing aggressiveness — a marketable limit
//!   books as a maker fill, and `OrderKind::LimitClose` books as a taker one;
//! * every strategy `crate::templates` ships is a market order, so the shipped population realises
//!   a 100% taker mix and the split never engages.
//!
//! **So what a derived schedule buys TODAY is the taker rate going from zero to the real one, not
//! maker/taker discrimination.** That is the large and correct change; the split starts
//! discriminating when authors write limit orders, and it is worth nothing on a lane that prices
//! maker and taker identically. [`FillMix`] is what makes that visible to the person reading a
//! report rather than only to a reader of this module.
//!
//! # What it cannot reach
//!
//! [`NOT_MODELLED`] names the three terms that can flip a ranking and cannot be derived from
//! anything a slice carries. They are part of the stamp, not a footnote: a statement that says only
//! what IS modelled invites a reader to infer that nothing else exists.

use vike_analytics::BacktestResult;
use vike_model::{FeeSchedule, fee_schedule_for};
use vike_sim::EngineParams;

/// WHERE a run's cost model came from — the first thing the stamp reports, because a reader who
/// cannot separate a derived schedule from an overridden flat rate from no cost model at all is in
/// exactly the position `0047` warned about.
#[derive(Debug, Clone, PartialEq)]
pub enum CostSource {
    /// Resolved from the slice: `lane` is the `vike_catalog::fee_lane` key that answered, spelled
    /// out so a reader can see whether the `.P` perp split applied.
    Derived {
        /// The fee LANE, which is `venue` for most venues and a sub-key (`binance-perp`,
        /// `aster-perp`, `aster-perp-usd1`) for the two that front two order APIs behind one id.
        lane: String,
    },
    /// The caller sent a flat `fee_rate`, and it WINS. Named `Override` because that is what it is:
    /// an instruction that displaces the derivation, not a fallback taken when one failed.
    Override {
        /// The flat fraction (NOT bps) the caller sent, applied to every fill of every candidate.
        fee_rate: f64,
    },
    /// No cost model applied — the run is priced at zero, and `reason` says why rather than leaving
    /// a reader to infer that trading is free.
    None {
        /// Why nothing was derived, in a sentence a report can print.
        reason: String,
    },
}

/// **Every term `0063` ruled on that this run did NOT price** — carried INTO the result rather than
/// left in the record, because a statement that says only what IS modelled invites a reader to
/// infer that nothing else exists.
///
/// ⚠ **Two CLASSES, and the entries say which, because the difference is what a reader would do
/// about it.** The first three are `0063`'s DECLARED RESIDUALS: they can flip a winner and nothing
/// in a slice implies them, so no amount of wire surface fixes them. The last two are DTO
/// OMISSIONS — `0063` ruled the information is server-side already and a boolean would reach it,
/// and that boolean is NOT BUILT HERE. They are listed for the same reason the residuals are: a
/// stamp naming three absences would be read as "and everything else was priced", which is exactly
/// the inference this list exists to prevent. When either boolean ships, its row LEAVES this list —
/// the record's own "any declared residual becomes expressible" clause, applied to the half that
/// was always expressible.
///
/// ⚠ This is deliberately not a list of everything an `[engine]` table can express.
/// `vike_backtest::harness::run`'s `bar_engine_params` assigns a far wider surface, several of
/// whose knobs flip a ranking on the same reasoning — and that function's own doc records that a
/// prose list of it "was sixteen fields short by the end of the branch that added them, and any
/// list written here rots the same way". The authority for that surface is `EngineCfg`'s field set,
/// gated by `crates/vike-ops/tests/wiring/engine_cfg_reaches_the_engine.rs`. These are named because
/// `0063` RULED on them one at a time, each on two axes (can it flip a ranking, can the DTO reach
/// it), not because they are the whole of what a profile can say.
pub const NOT_MODELLED: [&str; 5] = [
    "impact — [engine.impact]'s coefficients are a CALIBRATION, not a venue fact; nothing in a \
     slice implies them. (0047 measured this arm and it did NOT reorder its grid, even at 5,000x.)",
    "risk — the [risk] budget is an operator's, not a property of the data, and its gate REFUSES \
     orders outright: admissibility, which is strictly stronger than re-weighting",
    "resolution — the binary-outcome winners map is a server-side path or a hand-written table, \
     and a path must not cross this wire",
    "snap_to_properties (DTO omission, not a residual) — the PIT instrument grid is server-side \
     already and a boolean would reach it, but none is built: fills are NOT quantized to tick/step \
     and an opening fill below min_qty/min_notional is NOT dropped, so small-size candidates keep \
     trades a real grid would delete. 0063 also warns it is not a blind switch-on — snapping \
     re-tags bars with the venue, which can break a symbol-inferring strategy",
    "attach_funding (DTO omission, not a residual) — perp funding is NOT charged: this door loads \
     bars without the funding join, so a carry-holding candidate is costed as if holding were \
     free. It pushes the OPPOSITE way from fees (proportional to position x holding time, so it \
     penalises PATIENT candidates where fees penalise churning ones), and the two do not cancel",
];

/// The cost model a run resolved, before it ran.
#[derive(Debug, Clone, PartialEq)]
pub struct CostModel {
    /// Derived / override / none — see [`CostSource`].
    pub source: CostSource,
    /// The resolved schedule, `Some` only for [`CostSource::Derived`].
    pub schedule: Option<FeeSchedule>,
    /// The maker fraction the engine will charge (NOT bps).
    pub maker_rate: f64,
    /// The taker fraction the engine will charge (NOT bps).
    pub taker_rate: f64,
}

impl CostModel {
    /// **Resolve the cost model for one slice. This is the PRECEDENCE, and it is the whole of it:**
    ///
    /// 1. **an explicit `fee_rate` OVERRIDES** — if the caller sent one, it is used and NOTHING is
    ///    derived;
    /// 2. **otherwise the slice's own fee LANE is DERIVED** — `vike_catalog::fee_lane` over
    ///    `venue` + each symbol, then `vike_model::money::fees`' `fee_schedule_for`;
    /// 3. **otherwise nothing applies, and the reason is recorded** — an empty symbol list, or a
    ///    multi-symbol slice whose symbols resolve to DIFFERENT lanes, which one
    ///    `EngineParams::fee_schedule` cannot express.
    ///
    /// ⚠ **Step 1 must not merely be checked first, it must SUPPRESS step 2**, and the engine is
    /// why: `StrategyEngine::new` prefers `EngineParams::fee_schedule` over `fee_rate` whenever a
    /// schedule is present, so deriving a schedule BESIDE a caller's flat rate would discard the
    /// override in silence — a caller who asked for 10 bps would get the lane's rates and a report
    /// that said `override`. [`Self::apply`] is where that is enforced, and
    /// `an_override_suppresses_the_derivation` is the test.
    ///
    /// ⚠ `Some(0.0)` is an OVERRIDE, not an absence. A caller who deliberately prices a run at zero
    /// is saying something, and the stamp distinguishes it from a run nothing was derived for —
    /// which is the distinction `0063` requires the three sources to carry.
    ///
    /// The DEFAULT registry answers, not the opt-in prediction-market curve
    /// (`fee_schedule_for_with_pm_curve`): a curve whose effective rate is price-dependent has no
    /// flat equivalent, `maker_taker_rates` reports a zero pair for it, and opting a Studio slice
    /// into it silently is exactly the "configured but charging zero" misreport the variant name in
    /// the stamp exists to prevent. Polymarket therefore resolves `Free` here and SAYS `Free`.
    pub fn resolve(venue: &str, symbols: &[String], fee_rate_override: Option<f64>) -> CostModel {
        if let Some(fee_rate) = fee_rate_override {
            return CostModel {
                source: CostSource::Override { fee_rate },
                schedule: None,
                maker_rate: fee_rate,
                taker_rate: fee_rate,
            };
        }
        let Some(first) = symbols.first() else {
            return CostModel::none(
                "the slice names no symbol, so no fee lane resolves — every fill is priced at zero",
            );
        };
        let lane = vike_catalog::fee_lane(venue, first);
        // A multi-symbol slice can straddle two lanes (`BTCUSDT` + `BTCUSDT.P` on binance), and
        // `EngineParams` carries ONE schedule. Charging either lane's rates to both halves would be
        // the bare-venue bug wearing a different hat, so this refuses to guess and says so.
        if let Some(other) = symbols.iter().find(|s| vike_catalog::fee_lane(venue, s) != lane) {
            return CostModel::none(format!(
                "this slice's symbols resolve to different fee lanes ({lane} for {first}, {} for \
                 {other}) and one engine carries one schedule — every fill is priced at zero",
                vike_catalog::fee_lane(venue, other)
            ));
        }
        let schedule = fee_schedule_for(lane);
        let (maker_rate, taker_rate) = schedule.maker_taker_rates();
        CostModel {
            source: CostSource::Derived { lane: lane.to_string() },
            schedule: Some(schedule),
            maker_rate,
            taker_rate,
        }
    }

    /// The zero model with its reason attached — never reachable without one.
    fn none(reason: impl Into<String>) -> CostModel {
        CostModel {
            source: CostSource::None { reason: reason.into() },
            schedule: None,
            maker_rate: 0.0,
            taker_rate: 0.0,
        }
    }

    /// Apply this model to the engine params a run will use.
    ///
    /// ⚠ **The DERIVED arm is the only one that writes anything**, and that asymmetry IS the
    /// precedence: an override has already been applied to `params.fee_rate` by the caller that
    /// read it off the wire, and writing a schedule here as well would displace it (see
    /// [`Self::resolve`]'s second warning). The none arm writes nothing because zero is what
    /// `EngineParams::default` already is — the difference between the two is REPORTED, not
    /// computed.
    pub fn apply(&self, params: &mut EngineParams) {
        if let Some(schedule) = self.schedule {
            params.fee_schedule = Some(schedule);
        }
    }

    /// The schedule's VARIANT name, or `"none"` when nothing was derived.
    ///
    /// ⚠ **Load-bearing on its own, and not a decoration.** Several lanes resolve to shapes that
    /// report a ZERO pair through `maker_taker_rates` — the per-share-with-floor equities shape and
    /// the prediction-market curve both do, because neither is expressible as a flat pair. Without
    /// the variant, "maker 0, taker 0" reads as free trading when it may instead mean *this
    /// schedule has no flat equivalent*. An exhaustive match on purpose: a variant added to
    /// `FeeSchedule` reddens this line rather than falling through to a label that would lie.
    ///
    /// It lives here rather than on `FeeSchedule` because the Studio's stamp is its only consumer
    /// and the exhaustiveness property is identical either way.
    pub fn variant(&self) -> &'static str {
        match self.schedule {
            None => "none",
            Some(FeeSchedule::PercentMakerTaker { .. }) => "PercentMakerTaker",
            Some(FeeSchedule::PerShareWithFloor { .. }) => "PerShareWithFloor",
            Some(FeeSchedule::PercentOfUnderlying { .. }) => "PercentOfUnderlying",
            Some(FeeSchedule::ProbabilityScaled { .. }) => "ProbabilityScaled",
            Some(FeeSchedule::Free) => "Free",
        }
    }

    /// The source as the machine token a report prints: `"derived"` / `"override"` / `"none"`.
    pub fn source_token(&self) -> &'static str {
        match self.source {
            CostSource::Derived { .. } => "derived",
            CostSource::Override { .. } => "override",
            CostSource::None { .. } => "none",
        }
    }

    /// The resolved lane, for [`CostSource::Derived`] only.
    pub fn lane(&self) -> Option<&str> {
        match &self.source {
            CostSource::Derived { lane } => Some(lane.as_str()),
            _ => None,
        }
    }

    /// Why nothing was derived, for [`CostSource::None`] only.
    pub fn reason(&self) -> Option<&str> {
        match &self.source {
            CostSource::None { reason } => Some(reason.as_str()),
            _ => None,
        }
    }
}

/// The REALISED maker/taker mix and the commission a run actually paid, accumulated off the
/// `BacktestResult`s a run produced.
///
/// ⚠ **`0063` calls this the highest-value field in the stamp, and the reason is the scoping
/// finding above**: a derived schedule that never touched its maker side and one that mattered look
/// identical without it. The shipped Studio starters are all market orders, so a report that named
/// a maker/taker schedule and nothing else would let a reader believe a split was doing work it
/// could not be doing.
///
/// It sums the ENGINE's own counters (`BacktestResult::maker_fills` / `taker_fills` /
/// `fees_paid`), never a re-derivation — `0063`'s "computed, never restated" constraint applied one
/// type down.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FillMix {
    /// Fills the engine booked as MAKER.
    pub maker_fills: u64,
    /// Fills the engine booked as TAKER.
    pub taker_fills: u64,
    /// Total commission charged (signed — a rebate-bearing schedule contributes negative terms).
    pub fees_paid: f64,
}

impl FillMix {
    /// Fold one run's counters in. Called once per OUT-OF-SAMPLE window on the walk-forward path,
    /// so the mix describes the runs the report's numbers came from — never the training scores a
    /// search discarded, which no reported curve contains.
    pub fn add(&mut self, result: &BacktestResult) {
        self.maker_fills += result.maker_fills;
        self.taker_fills += result.taker_fills;
        self.fees_paid += result.fees_paid;
    }

    /// This run's counters as a fresh mix — the single-run spelling of [`Self::add`].
    pub fn of(result: &BacktestResult) -> Self {
        let mut mix = FillMix::default();
        mix.add(result);
        mix
    }
}

#[path = "cost_model_tests.rs"]
#[cfg(test)]
mod cost_model_tests;
