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
//! - **`max_notional_per_order`** — genuinely consumed, but at the ORDER surfaces (`vike-desktop`'s
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
//! (`crates/bridges/hyperliquid/src/exec.rs`'s `spawn_with_market_slippage`); this is the operator
//! value finally reaching them.

use vike_bridge_core::account_directory::AccountDirectory;
use vike_config::{Policy, VenueMode, VenuePolicy};
use vike_model::HaltAdmit;

/// The hard ceilings [`crate::make_engine`] applies — plus TWO facts that are not ceilings and had
/// nowhere else to ride, [`Self::accounts`] and [`Self::venue_settings`], each carrying its own
/// argument.
///
/// ⚠ This line read "and nothing else" until 2026-09-15, and the exception is stated rather than
/// quietly added: the mount and the arming projection must resolve a dukascopy ACCOUNT identically,
/// they are handed this struct and nothing else in common, and a second parameter beside it would
/// let a caller give the two sides different tables. It read "ONE fact" until the venue mount
/// contract added the venue-settings snapshot, which rides here for the same reason.
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
    /// Passed through `crates/bridges/hyperliquid/src/exec.rs`'s `market_slippage_for` by that
    /// bridge's mount (`MountRequest::market_slippage`), which bounds it (clamped, warned, never
    /// widened past the venue's own default) — so the value here is the operator's REQUEST, not the
    /// applied band.
    /// See [`vike_config::Policy::market_slippage`].
    pub market_slippage: Option<f64>,

    /// How much evidence the HALT sentinel demands before letting a submit out.
    ///
    /// [`HaltAdmit::Admit`] — the default, and what a machine with no `policy.toml` gets — is
    /// byte-identical to the flag-trusting rule every venue used before this field existed.
    /// [`HaltAdmit::Verify`] is applied by cTrader's mount and degrades everywhere else;
    /// `vike_model::halt_verify_support` is the per-venue authority and
    /// `vike_model::effective_halt_admit` is what [`crate::make_engine`] calls to REPORT the
    /// degrade at mount. Like `market_slippage`, the value here is the operator's REQUEST, not the
    /// applied mode — which is exactly why the mount has to say what it did with it.
    pub halt_admit: HaltAdmit,

    /// **The ACCOUNT-aggregate open-notional ceiling** this deployment permits, folded onto
    /// `vike_exec::RiskLimits::max_account_exposure` at the end of
    /// [`crate::make_engine_for_account`] — so `vike_exec::RiskGate::check_inner`'s
    /// `over-account-exposure` lane judges every order this engine's account mints.
    ///
    /// `None` — the default, and what a machine with no `policy.toml` gets — is the axis switched
    /// off: the gate runs no comparison, the producer folds no book, and every verdict is
    /// byte-identical to before this field existed. Unlike [`Self::venues`] below, this default
    /// does NOT bite, deliberately; [`vike_config::Policy::max_account_exposure`] argues why an
    /// upgrade may not start refusing orders on a live account from a file nobody wrote.
    ///
    /// ⚠ It is carried where [`vike_config::Policy::max_notional_per_order`] is deliberately NOT,
    /// and the difference is the whole reason this one could land: folding the per-order ceiling
    /// into `RiskLimits` would move `crate::require_live_risk_budget`'s refusal, which is a
    /// live-trading gate with its own operator-facing message. This axis is NOT in that refusal
    /// (`vike_exec::RiskLimits::max_account_exposure`'s doc says why), so there is nothing to move.
    pub max_account_exposure: Option<f64>,

    /// **The ceiling on the EQUITY FIGURE this deployment's sizing and admission lanes may see**,
    /// folded onto `vike_exec::RiskLimits::max_sizing_equity` at the end of
    /// [`crate::make_engine_for_account`] — so `vike_exec::ExecutionEngine::sizing_equity`, the one
    /// resolver every acting consumer reads, answers `min(resolved equity, this)`.
    ///
    /// `None` — the default, and what a machine with no `policy.toml` gets — is the axis switched
    /// off: `sizing_equity` is bit-identical to `resolved_equity` and every verdict is
    /// byte-identical to before this field existed. Like [`Self::max_account_exposure`] and unlike
    /// [`Self::venues`], this default does NOT bite;
    /// [`vike_config::Policy::max_sizing_equity`] argues why an upgrade may not start shrinking a
    /// live account's position sizes from a file nobody wrote.
    ///
    /// ⚠ **It is carried, but it must never reach the margin-CALL sweep**, and that asymmetry is
    /// not visible from this struct: a lower equity figure is conservative for sizing and admission
    /// and DESTRUCTIVE for a liquidation decision. The separation is made where the number is read,
    /// by there being two resolvers — `vike_exec::ExecutionEngine::sizing_equity` (capped) and
    /// `resolved_equity` (the truth, which the watchdog and every report surface keep) — and that
    /// method's doc is the authority for which consumer is on which side.
    pub max_sizing_equity: Option<f64>,

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

    /// **The settings database's `account` table, as the composition root read it** — one of the
    /// two fields of this struct that are not ceilings (the other is [`Self::venue_settings`]).
    ///
    /// It is carried HERE because this struct is already what every mount seam is handed about the
    /// deployment: `Option<&MountPolicy>` reaches [`crate::make_engine_for_account`]'s venue match
    /// AND — since the account table landed — [`crate::venue_account_arming`], the projection
    /// `vike_run::refuse_unarmed_mount_accounts` judges a strategy mount against. Those two must
    /// resolve a dukascopy account IDENTICALLY (the dukascopy bridge's mount,
    /// `crates/bridges/dukascopy/src/mount.rs`: the row's credential-key owner prefix decides which
    /// LEGAL ENTITY an order reaches), and one snapshot on the object they both
    /// already receive is what makes disagreeing impossible. A second parameter beside the policy
    /// would let a caller hand the two sides different tables.
    ///
    /// ⚠ [`From<&Policy>`](MountPolicy::from) cannot fill it and does not pretend to: it is not in
    /// `policy.toml`. The root SETS it after reading the store — `crates/vike-tradehub/src/
    /// tradehub_cli.rs`'s `live_mount_with` — and every other caller leaves it
    /// [`AccountDirectory::unread`], which refuses every labelled dukascopy account by name and
    /// leaves the DEFAULT account byte-identical. That is the same answer a `Backend::Files` box
    /// gets, so a root that never reads the store degrades exactly as an unmigrated box does.
    pub accounts: AccountDirectory,

    /// **Every venue's `venue_setting` rows, as the composition root read them** — the second field
    /// that is not a ceiling, carried here for `accounts`' reason: the arming projection and the
    /// mount are handed this struct and nothing else in common, so one snapshot reaches both. Each
    /// bridge receives its own venue's view as `MountInputs::settings`; a venue with no rows gets
    /// the empty view. [`From<&Policy>`](MountPolicy::from) cannot fill it (it is not in the policy
    /// file): the root sets it after reading the store.
    pub venue_settings:
        std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
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
            //
            // ⚠ THIS COMMENT IS NO LONGER THE ONLY RECORD OF THAT, and it must not become one
            // again. A run profile's `[risk]` table carries a ceiling of the same NAME which DOES
            // reach `RiskLimits` — so the drop here is what makes the two different gates, and it
            // was written down in a `# mirrors settings/policy.toml` comment on a live box and
            // nowhere else. `vike_config::ceilings::PRE_TRADE_CEILINGS` now carries both rows with
            // the act each judges, `vike-cli config show` renders them, and
            // `crates/vike-config/tests/ceilings_are_distinct.rs` fails if the two ever claim the
            // same enforcement site — i.e. if somebody folds this field in after all without
            // saying so.
            max_notional_per_order: _,
            // CARRIED. The ACCOUNT-aggregate exposure ceiling, folded onto
            // `vike_exec::RiskLimits::max_account_exposure` in `make_engine_for_account` — the
            // first `Policy` ceiling to reach the pre-trade GATE itself rather than an order
            // surface or a venue arm. It could where the sibling directly above could not, because
            // no existing refusal is keyed on it: absent is uncapped, and `require_live_risk_budget`
            // does not demand it.
            max_account_exposure,
            // CARRIED. The ceiling on the equity FIGURE the sizing and admission lanes may see —
            // the second `Policy` ceiling to reach a live DECISION rather than an order surface,
            // and it could land for the same reason the one above it could: no existing refusal is
            // keyed on it, absent is uncapped, and `require_live_risk_budget` does not demand it.
            // ⚠ It reaches the acting consumers ONLY. Where it must NOT reach — the margin-call
            // sweep, which liquidates and therefore has to judge against the real figure — is
            // enforced by there being two resolvers on the engine, not by anything on this struct.
            max_sizing_equity,
            // CARRIED. The one field with a venue seam waiting for it (#1051).
            market_slippage,
            // CARRIED. Reaches cTrader's mount (`crates/bridges/ctrader/src/mount.rs`'s
            // `CtraderVenueMount`, the only adapter with a position book at its halt boundary) and is
            // REPORTED for every other venue when an operator asked for `verify` and the venue cannot
            // honour it.
            halt_admit,
            // Not carried, and NOT a gap — the wrong crate. The dead-man switch is a property of
            // the CORE (`vike_core::CoreConfig::deadman`: it watches the whole core's ingest and
            // trips through the core's own order-write path), not of any one venue engine this
            // projection feeds `make_engine`. It is constructed by the composition root that
            // builds the `CoreConfig` — `crates/vike-tradehub/src/tradehub_cli.rs`'s
            // `deadman_config_from_policy`, called from `live_mount_with` — and
            // `crates/vike-config/tests/policy_is_consumed.rs` names that site. Carrying the
            // numbers here would put the same fact on two structs with nothing reading the second.
            deadman_timeout_ms: _,
            deadman_action: _,
            // Not carried either, and the same argument one switch over: the LINK dead-man
            // (`vike_core::CoreConfig::link_deadman`) watches the FeedStatus the core receives and
            // trips through the core's own order-write path, so it belongs to the `CoreConfig` this
            // projection does not build. Its per-venue half is a TABLE
            // (`vike_model::link_deadman_default`), not a policy value, and the composition root
            // folds the two — `crates/vike-tradehub/src/tradehub_cli.rs`'s
            // `link_deadman_config_from_policy`.
            link_deadman_grace_ms: _,
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
            max_account_exposure: *max_account_exposure,
            max_sizing_equity: *max_sizing_equity,
            venues: venues.clone(),
            // NOT from the policy file — there is nothing in `Policy` to project. The composition
            // root fills it after reading the store; see [`MountPolicy::accounts`].
            accounts: AccountDirectory::unread(),
            // NOT from the policy file — the root fills it.
            venue_settings: std::collections::BTreeMap::new(),
        }
    }
}

#[path = "policy_tests.rs"]
#[cfg(test)]
mod policy_tests;
