//! [`MountPolicy`] — the projection of [`vike_config::Policy`] that the venue mount actually
//! CONSUMES, and the seam that finally gives the typed settings system a consumer.
//!
//! Phases 1–5 of the settings-unification design built `vike-config` in full — the four types, the
//! layered loader, the per-type override power, the removed-env refusals — and wired it into
//! exactly one place: `vike-app`/`vike-cli`/`vike-tradehub`'s order-notional preview. **No venue
//! mount ever read a `Policy`.** This module is the edge that changes that.
//!
//! ## Why a projection and not `Option<&Policy>` straight through
//!
//! Because "wired" must be readable at the seam. Not every field of a `Policy` handed to
//! [`crate::make_engine`] binds something at a venue arm, and a reader (or a future editor) seeing
//! `policy.max_leverage` available inside `make_engine` would reasonably assume it is enforced
//! there — it is not, and quietly assuming it is, is the failure this whole program exists to
//! remove. The struct below therefore names exactly what the mount applies, and the `From` impl
//! below names — with its reason — every field it deliberately drops. **No count is written here**:
//! this paragraph said "five fields; today exactly ONE of them binds anything" and was wrong within
//! one change of being written. The `From` impl IS the list.
//!
//! ## The completeness gate is the compiler
//!
//! [`MountPolicy::from`] destructures [`vike_config::Policy`] EXHAUSTIVELY (no `..`). A new field on
//! `Policy` therefore breaks that one line and forces a carried/not-carried decision with a written
//! reason, exactly as `vike_model::VENUES` forces a row in every per-venue capability table. A
//! runtime "did we consider every field?" test cannot do this — there is nothing to iterate — so
//! the pattern IS the gate.
//!
//! ## What is NOT carried today, and why (the honest half)
//!
//! - **`max_leverage`** — carrying it would BREAK the byte-identical rule. Its policy default is
//!   `1.0` ("no leverage"), which is the conservative end for a *ceiling*, but the mount's live
//!   leverage already arrives through `vike_exec::ProfileRisk::max_leverage` → the `im_requirement`
//!   rescue, whose absent value means "the buying-power gate keeps its 1× default". Treating the
//!   policy default as a binding ceiling would clamp every deployment with no `policy.toml` to 1×
//!   — a behaviour change on upgrade, from a file nobody wrote. Reconciling the two authorities
//!   (which wins, and what an absent file means for each) is its own phase.
//! - **`max_notional_per_order`** — genuinely consumed, but at the ORDER surfaces (`vike-app`'s
//!   order-entry preview cap, `vike-tradehub`'s control-server limits, `vike-cli trade`/`mcp`'s
//!   advisory guardrail), which Phase 5 wired. Folding it into `vike_exec::RiskLimits` here as a
//!   second, tighter ceiling is defensible and byte-identical when absent — but it also moves
//!   `require_live_risk_budget`'s refusal, whose PRE-CONNECT probe reads the run profile alone and
//!   whose diagnostic names the profile's `[risk]` table as the fix. That refusal is a live-trading
//!   gate with its own tests and its own operator-facing error text; changing which authorities
//!   satisfy it is a decision, not plumbing, so it is not made here.
//!
//!   ⚠ **This bullet used to say `max_notional_per_order` / `max_total_exposure`, and to claim
//!   both "ARE consumed".** Only the first ever was. `Policy::max_total_exposure` was read by no
//!   code path in the workspace — not here, not at any order surface — so an operator who set it
//!   in `policy.toml` got validation, no complaint, and no ceiling. The field has been removed
//!   (`vike_config::PolicyPatch::max_total_exposure` is now a tombstone that REFUSES the key and
//!   names the run profile's `[risk]` table, where the same knob is real, enforced by
//!   `RiskGate::check_inner`'s `over-max-exposure` lane, and already mandatory for a live mount).
//!   A `//` comment beside a `_` binding is exactly as green when it is false, which is why
//!   `crates/vike-config/tests/policy_is_consumed.rs` now verifies each field's claimed consumer
//!   by opening the file and looking for the read.
//!
//!   ⚠ **`rate.max_utilization` was a third bullet here, and it is GONE** — and it is worth saying
//!   why, because this doc got it wrong twice in the same list. The bullet called it a
//!   request-PACING ceiling that binds `Preferences::rate_utilization`, "and the pacing itself
//!   lives in the bridges' rate limiters". The first clause was true; the second was not. The
//!   bridges take their target fraction from the compiled-in
//!   `vike_model::rate_limits::DEFAULT_UTILIZATION`, and NOTHING read the preference — so the
//!   ceiling bounded a dead value, `max_total_exposure`'s defect a second time on the same page.
//!   Both halves are refused tombstones now (`vike_config::PolicyPatch::rate`), and
//!   `crates/vike-config/tests/policy_is_consumed.rs` gained the rule that would have caught it: a
//!   claimed consumer inside `crates/vike-config/` — which the clamp was — does not count.
//!
//! ## What IS carried
//!
//! [`MountPolicy::halt_admit`] — how much evidence the HALT sentinel demands before letting a
//! submit out. Its default (`admit`) is byte-identical to the rule every venue used before it
//! existed, and its other value (`verify`) is REAL on exactly one venue, cTrader — the only adapter
//! that holds a position book at its halt boundary. ⚠ The interesting half of the wiring is the
//! other arms: an operator who sets `verify` and mounts binance must be TOLD, at mount, that it
//! degraded and why, because "I set verify" and "verify is doing anything here" are different
//! facts. `vike_model::effective_halt_admit` answers both questions from one call, and
//! `vike_model::halt_verify_support` carries the per-venue reason.
//!
//! [`MountPolicy::venues`] — **the one ceiling whose DEFAULT changes behaviour**, and the reason
//! this stage is not byte-identical to anything. Every other field on this struct defaults to "the
//! venue keeps its compiled-in literal"; this one defaults to `paper` for every venue, because the
//! defect it closes is that a venue arms on credential PRESENCE alone (MEASURED on the CI box: a
//! one-venue run profile holding NINE live authenticated exec sessions, aster among them, whose
//! keys are `ASTER_LIVE_*` and which has no `{VENUE}_MAINNET` flag to refuse). A ceiling whose
//! absent value armed everything would not be a ceiling. The upgrade cost of that choice — a
//! credentialled box dropping to all-paper on its first start after this lands — is paid by
//! [`crate::venue_arming_migration`], which warns and does not refuse (see that function's doc for
//! why a refusal is the wrong disposition for a CAPABILITY).
//!
//! [`MountPolicy::market_slippage`] — the aggression band a venue with **no native market order**
//! prices its emulated market (and stop-market) orders at. Hyperliquid is the only venue on the
//! roster with that shape (`vike_model::market_slippage`'s module doc carries the survey), so a
//! band that reaches one venue arm is COMPLETE coverage here, not partial wiring: every other venue
//! sends the venue's own `MARKET` wire value and has no band to configure. #1051 built the bound
//! (`vike_bridge_core::market_slippage::resolve_market_slippage`) and the seam
//! (`vike_hyperliquid::HyperliquidExecutionClient::spawn_with_market_slippage`); this is the
//! operator value finally reaching them.

use vike_config::{Policy, VenueMode, VenuePolicy};
use vike_model::HaltAdmit;

/// The hard ceilings [`crate::make_engine`] applies, and nothing else.
///
/// [`Default`] is exactly "no `policy.toml` on this machine" — every scalar field `None`, every
/// venue at [`VenueMode::Paper`], every venue keeping its own compiled-in literal. That equivalence
/// is what makes the whole wiring safe to land: a caller that passes `None`, a caller that passes
/// `&MountPolicy::default()`, and a deployment with no policy file are the same three things, gated
/// by `an_absent_policy_file_is_exactly_the_mount_default` below.
///
/// ⚠ **No longer `Copy`** — [`Self::venues`] owns a `BTreeMap`. Every call site already passes it as
/// `Option<&MountPolicy>` (`vike_run::NodeConfig` holds one and hands out `Some(&cfg.policy)`), so
/// nothing depended on the copy; a future site that wants an owned one clones.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MountPolicy {
    /// Aggression band for an EMULATED market order, as a fraction (`0.002` = 0.2 %).
    ///
    /// `None` — the default — means the venue keeps its own historical literal, byte-identically.
    /// Passed through `vike_hyperliquid::exec::market_slippage_for` at the arm, which bounds it
    /// (clamped, warned, never widened past the venue's own default) — so the value here is the
    /// operator's REQUEST, not the applied band. See [`vike_config::Policy::market_slippage`].
    pub market_slippage: Option<f64>,

    /// How much evidence the HALT sentinel demands before letting a submit out.
    ///
    /// [`HaltAdmit::Admit`] — the default, and what a machine with no `policy.toml` gets — is
    /// byte-identical to the flag-trusting rule every venue used before this field existed.
    /// [`HaltAdmit::Verify`] is applied at the cTrader arm and degrades everywhere else;
    /// `vike_model::halt_verify_support` is the per-venue authority and
    /// `vike_model::effective_halt_admit` is what [`crate::make_engine`] calls to REPORT the
    /// degrade at mount. Like `market_slippage`, the value here is the operator's REQUEST, not the
    /// applied mode — which is exactly why the mount has to say what it did with it.
    pub halt_admit: HaltAdmit,

    /// **How far this deployment permits each venue to arm** — the per-venue CEILING, read at the
    /// top of [`crate::make_engine`] through [`Self::venue_mode`] and folded as
    /// [`VenueMode::cap`] (`min`, never `max`), so a line here can only ever REFUSE an arming the
    /// credentials would have produced.
    ///
    /// [`VenuePolicy::default()`] — every venue at [`VenueMode::Paper`] — is what a machine with no
    /// `policy.toml` gets, and it is NOT byte-identical to the mount before this field existed:
    /// this is the one ceiling whose default BITES. That is the whole point (credential presence
    /// stops being the only gate) and it is also the one upgrade hazard in the settings system, so
    /// it is announced rather than discovered — see [`crate::venue_arming_migration`].
    pub venues: VenuePolicy,
}

impl MountPolicy {
    /// This venue's arming ceiling — the value [`crate::make_engine`] caps the mount with.
    ///
    /// A venue string the roster does not carry answers [`VenueMode::Paper`], which is
    /// [`VenuePolicy::get`]'s rule and the safe one: an unrecognised venue label must not arm.
    #[must_use]
    pub fn venue_mode(&self, venue: &str) -> VenueMode {
        self.venues.get(venue)
    }
}

impl From<&Policy> for MountPolicy {
    /// Project the loaded policy onto what the mount applies.
    ///
    /// ⚠ The destructure is EXHAUSTIVE on purpose — see this module's doc. Do not add `..`: a new
    /// `Policy` field must break this line and be given a home or a written reason, never be
    /// silently dropped into a settings system that looks wired and is not.
    fn from(policy: &Policy) -> Self {
        let Policy {
            // Not carried — the mount's leverage authority is `ProfileRisk`/`im_requirement`, and
            // this field's `1.0` default would clamp every policy-file-less deployment to 1×.
            max_leverage: _,
            // Not carried — genuinely consumed at the order surfaces (Phase 5). Folding it into
            // `RiskLimits` moves `require_live_risk_budget`'s refusal, a decision, not plumbing.
            max_notional_per_order: _,
            // CARRIED. The one field with a venue seam waiting for it (#1051).
            market_slippage,
            // CARRIED. Reaches the cTrader arm (the only adapter with a position book at its halt
            // boundary) and is REPORTED at every other arm when an operator asked for `verify` and
            // the venue cannot honour it.
            halt_admit,
            // CARRIED, as of stage 3 — the per-venue arming CEILING (`vike_config::VenueMode`,
            // ordered `paper < demo < live`), folded at the top of `make_engine_with_legs` as
            // `mode.cap(whatever this arm would have decided)`: `min`, never `max`, so the file can
            // only ever REFUSE an arming the credentials would have produced.
            //
            // ⚠ CLONED, not copied: this is the one `Policy` field that owns an allocation, and it
            // is what cost `MountPolicy` its `Copy`. Once per mount, per venue — a fourteen-entry
            // `BTreeMap` of `&'static str` keys, beside a mount that is about to open a socket.
            venues,
        } = policy;
        MountPolicy {
            market_slippage: *market_slippage,
            halt_admit: *halt_admit,
            venues: venues.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vike_hyperliquid::exec::market_slippage_for;

    /// THE load-bearing property of this whole phase, end to end and network-free: **no
    /// `policy.toml` on disk ⇒ every venue gets exactly today's value.** Driven through the REAL
    /// loader (`load(None, &{})` is precisely "no home directory, no project file, no
    /// environment") rather than by asserting `Policy::default()`, so a future default that stopped
    /// being the no-file answer would fail here too.
    #[test]
    fn an_absent_policy_file_is_exactly_the_mount_default() {
        let settings = vike_config::load(None, &HashMap::new()).unwrap();
        let mount = MountPolicy::from(&settings.policy);
        assert_eq!(mount, MountPolicy::default());
        assert_eq!(mount.market_slippage, None, "no file ⇒ no band ⇒ the venue's own literal");
        // …and the halt sentinel keeps trusting the caller's `reduce_only` flag, on every venue,
        // exactly as it did before this field existed. This is the byte-identical claim for the
        // kill switch, asserted through the REAL loader rather than about `Policy::default()`.
        assert_eq!(mount.halt_admit, HaltAdmit::Admit, "no file ⇒ the flag-trusting halt rule");
        for venue in vike_model::VENUES {
            assert_eq!(
                vike_model::effective_halt_admit(mount.halt_admit, venue),
                (HaltAdmit::Admit, None),
                "{venue} must be in `admit` with NOTHING to report — a default that logged would \
                 make the no-change claim false in the trace file too"
            );
        }
        // …and that `None` resolves, at the venue, to the exact literal hyperliquid priced with
        // before any of this existed. This is the byte-identical claim, asserted rather than
        // asserted-about.
        assert_eq!(market_slippage_for(mount.market_slippage), 0.05);
        // ⚠ THE ONE FIELD FOR WHICH "no file" IS NOT "no change": every venue reads `paper`, so the
        // mount refuses every arming credential presence would have produced. Asserted here, in the
        // test that owns the no-file claim, so the exception is stated where the rule is.
        for venue in vike_model::VENUES {
            assert_eq!(
                mount.venue_mode(venue),
                vike_config::VenueMode::Paper,
                "{venue}: an unstated arming ceiling must be the SAFE end, not the wide one"
            );
        }
        // …and nobody stated it, which is the fact the migration warning self-silences on.
        assert!(!mount.venues.is_declared(), "no file ⇒ nobody declared an arming");
    }

    /// The other half: a value in the file DOES reach the venue's resolver, verbatim. Pure — the
    /// file→`Policy` half is gated by `vike-config`'s own `tests/load.rs`; this pins the projection
    /// and the venue hand-off, which is the part that was missing.
    #[test]
    fn a_configured_band_reaches_the_venue_resolver_verbatim() {
        let policy = Policy { market_slippage: Some(0.002), ..Policy::default() };
        assert_eq!(MountPolicy::from(&policy).market_slippage, Some(0.002));
        assert_eq!(market_slippage_for(Some(0.002)), 0.002);
    }

    /// Threading a policy through the mount can only ever TIGHTEN the band, never widen it — the
    /// bound lives at the venue (`vike_bridge_core::market_slippage`), so this edge cannot be used
    /// to route around it. Pinned here as well as there because THIS is the path an operator's file
    /// now travels.
    #[test]
    fn no_policy_value_can_widen_the_venue_default() {
        for wide in [0.0500001, 0.1, 0.5, 50.0, f64::INFINITY, f64::NAN] {
            let policy = Policy { market_slippage: Some(wide), ..Policy::default() };
            let applied = market_slippage_for(MountPolicy::from(&policy).market_slippage);
            assert!(applied <= 0.05, "{wide} widened the applied band to {applied}");
            assert!(applied.is_finite(), "{wide} produced a non-finite band");
        }
    }

    /// The halt-admit knob travels the whole way — `policy.toml` value → `Policy` → `MountPolicy`
    /// → the mode `make_engine` puts in force — and lands DIFFERENTLY per venue, which is the point
    /// of carrying it rather than assuming it applies everywhere.
    #[test]
    fn a_configured_verify_reaches_the_one_venue_that_can_honour_it_and_degrades_elsewhere() {
        let policy = Policy { halt_admit: HaltAdmit::Verify, ..Policy::default() };
        let mount = MountPolicy::from(&policy);
        assert_eq!(mount.halt_admit, HaltAdmit::Verify, "the projection must carry it verbatim");

        // cTrader honours it: the only adapter with a position book at its halt boundary.
        assert_eq!(
            vike_model::effective_halt_admit(mount.halt_admit, "ctrader"),
            (HaltAdmit::Verify, None)
        );
        // Everywhere else it degrades — and, load-bearing, with a REASON to report. A silent
        // degrade is the defect this wiring exists to remove, not the feature.
        for venue in vike_model::VENUES.iter().filter(|v| **v != "ctrader") {
            let (effective, why) = vike_model::effective_halt_admit(mount.halt_admit, venue);
            assert_eq!(effective, HaltAdmit::Admit, "{venue}");
            assert!(why.is_some(), "{venue} degraded with nothing to tell the operator");
        }
    }

    /// `Default` is not merely *similar* to an absent file — it is the same value, so a call site
    /// that has no policy to pass and one that passes the default are interchangeable.
    #[test]
    fn the_default_is_the_no_policy_answer() {
        assert_eq!(MountPolicy::default().market_slippage, None);
        assert_eq!(MountPolicy::default().halt_admit, HaltAdmit::Admit);
        assert_eq!(MountPolicy::default().venue_mode("bybit"), VenueMode::Paper);
        assert_eq!(MountPolicy::from(&Policy::default()), MountPolicy::default());
    }

    /// **The ceiling travels the whole way and lands PER VENUE** — a `Policy` naming two venues
    /// projects onto a `MountPolicy` that answers differently for each, while every venue the file
    /// did not name keeps the safe default.
    ///
    /// ⚠ Built through `VenuePolicy::declare` rather than by parsing a `[venues]` table, because
    /// `Policy::apply` is `pub(crate)` to vike-config and this crate cannot reach it. The FILE half
    /// — `[venues]` → `Policy` → `is_declared` — is gated where it lives, by
    /// `crates/vike-config/src/policy.rs`'s `a_venues_table_sets_what_it_names_and_inherits_the_rest`
    /// and `an_all_paper_table_declares_while_no_table_does_not`; what this pins is the half those
    /// cannot see, the PROJECTION.
    #[test]
    fn a_venues_table_reaches_the_projection_per_venue() {
        let policy = Policy {
            venues: vike_config::VenuePolicy::default()
                .declare("bybit", VenueMode::Live)
                .declare("binance", VenueMode::Demo),
            ..Policy::default()
        };
        let mount = MountPolicy::from(&policy);
        assert_eq!(mount.venue_mode("bybit"), VenueMode::Live);
        assert_eq!(mount.venue_mode("binance"), VenueMode::Demo);
        assert_eq!(mount.venue_mode("okx"), VenueMode::Paper, "an unnamed venue keeps the default");
        assert_eq!(mount.venue_mode("not-a-venue"), VenueMode::Paper, "and so does a non-venue");
        assert!(mount.venues.is_declared(), "the file stated an arming; the projection carries it");
    }
}
