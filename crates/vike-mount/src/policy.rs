//! [`MountPolicy`]: the projection of [`vike_config::Policy`] that the venue mount actually
//! CONSUMES, the edge where the typed settings system reaches a venue mount.
//!
//! ## Why a projection and not `Option<&Policy>` straight through
//! "Wired" must be readable at the seam: a reader seeing `policy.max_leverage` inside
//! [`crate::make_engine`] would assume it is enforced there, and it is not. The struct names
//! exactly what the mount applies; the `From` impl names, with its reason, every field it drops.
//! **No count is written here** (one once was, and was wrong within one change): the `From` impl
//! IS the list.
//!
//! ## The completeness gate is the compiler
//! [`MountPolicy::from`] destructures [`vike_config::Policy`] EXHAUSTIVELY (no `..`), so a new
//! `Policy` field breaks that line and forces a carried/not-carried decision with a written reason,
//! as `vike_model::VENUES` forces a row in every per-venue capability table. A runtime test cannot
//! do this (there is nothing to iterate), so the pattern IS the gate.
//!
//! ## What is NOT carried, and why
//! - **`max_leverage`**: the mount's live leverage arrives through
//!   `vike_model::ProfileRisk::max_leverage` → the `im_requirement` rescue (absent = the
//!   buying-power gate's 1× default). The policy default `1.0` as a binding ceiling would clamp
//!   every deployment with no `policy` rows to 1× on upgrade, from a setting nobody wrote.
//!   Reconciling the two authorities is its own phase.
//! - **`max_notional_per_order`**: consumed at the ORDER surfaces (`vike-desktop`'s order-entry
//!   preview, `vike-tradehub`'s control-server limits, `vike-cli trade`/`mcp`'s advisory guardrail),
//!   so it reaches no `vike_model::RiskLimits`. Folding it in would move
//!   `require_live_risk_budget`'s PRE-CONNECT refusal (it reads the run profile alone; its
//!   operator text names the profile's `[risk]` table): a decision, not plumbing, not made here.
//!
//!   ⚠ `Policy::max_total_exposure` was claimed consumed here and was read by NOTHING (validated,
//!   no complaint, no ceiling). It is now a refusing tombstone
//!   (`vike_config::PolicyPatch::max_total_exposure`) naming the run profile's `[risk]` table, and
//!   `crates/vike-config/tests/policy_is_consumed.rs` verifies each claimed consumer by opening the
//!   file and looking for the read.
//!
//!   ⚠ `rate.max_utilization` was claimed to bind bridge pacing, but the bridges use the compiled-in
//!   `vike_model::rate_limits::DEFAULT_UTILIZATION` and nothing read the preference: now a refused
//!   tombstone (`vike_config::PolicyPatch::rate`), and `policy_is_consumed.rs` no longer counts a
//!   claimed consumer inside `crates/vike-config/`.
//!
//! ## What IS carried
//! [`MountPolicy::halt_admit`]: how much evidence the HALT sentinel demands before a submit leaves.
//! `admit` (default) is the old rule; `verify` is REAL only on cTrader (the one adapter holding a
//! position book at its halt boundary). ⚠ An operator who sets `verify` and mounts binance must be
//! TOLD at mount that it degraded and why: `vike_model::effective_halt_admit` answers both from one
//! call, `vike_model::halt_verify_support` carries the per-venue reason.
//!
//! [`MountPolicy::venues`]: **the one ceiling whose DEFAULT changes behaviour** (`paper` for every
//! venue): the defect it closes is arming on credential PRESENCE alone (MEASURED on the CI box: a
//! one-venue run profile holding NINE live authenticated exec sessions, aster among them, whose
//! `ASTER_LIVE_*` keys have no `{VENUE}_MAINNET` flag to refuse). The upgrade cost (a credentialled
//! box going all-paper on first start) is paid by [`crate::venue_arming_migration`], which warns
//! rather than refuses (its doc says why).
//!
//! [`MountPolicy::market_slippage`]: the band a venue with **no native market order** prices its
//! emulated market/stop-market orders at. Hyperliquid is the only such venue
//! (`vike_model::market_slippage`'s module doc), so one venue arm is COMPLETE coverage. Bound:
//! `vike_bridge_core::market_slippage::resolve_market_slippage`; seam:
//! `crates/bridges/hyperliquid/src/exec.rs`'s `spawn_with_market_slippage` (#1051).

use vike_bridge_core::account_directory::AccountDirectory;
use vike_config::{Policy, VenueMode, VenuePolicy};
use vike_model::HaltAdmit;

/// The hard ceilings [`crate::make_engine`] applies, plus TWO facts that are not ceilings,
/// [`Self::accounts`] and [`Self::venue_settings`].
///
/// ⚠ Those two ride HERE because the mount and the arming projection are handed this struct and
/// nothing else in common, and must resolve a dukascopy ACCOUNT (and a venue setting) identically;
/// a second parameter beside it would let a caller give the two sides different tables.
///
/// [`Default`] is exactly "no `policy` rows on this machine" (every scalar `None`, every venue
/// [`VenueMode::Paper`] and keeping its compiled-in literal): passing `None`, passing
/// `&MountPolicy::default()` and having no rows are the same thing, gated by
/// `an_absent_policy_file_is_exactly_the_mount_default`.
///
/// ⚠ **Not `Copy`**: [`Self::venues`] owns a `BTreeMap`. Call sites pass `Option<&MountPolicy>`
/// (`vike_mount::NodeConfig` hands out `Some(&cfg.policy)`); a site wanting an owned one clones.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MountPolicy {
    /// Aggression band for an EMULATED market order, as a fraction (`0.002` = 0.2 %); `None` keeps
    /// the venue's own literal. The operator's REQUEST, not the applied band: hyperliquid's mount
    /// (`MountRequest::market_slippage` → `crates/bridges/hyperliquid/src/exec.rs`'s
    /// `market_slippage_for`) clamps it, warns, and never widens past the venue default.
    /// See [`vike_config::Policy::market_slippage`].
    pub market_slippage: Option<f64>,

    /// How much evidence the HALT sentinel demands before letting a submit out.
    /// [`HaltAdmit::Admit`] (default, no `policy.halt_admit` row) is the flag-trusting rule;
    /// [`HaltAdmit::Verify`] is applied by cTrader's mount and degrades elsewhere
    /// (`vike_model::halt_verify_support`). The operator's REQUEST, so [`crate::make_engine`] calls
    /// `vike_model::effective_halt_admit` to REPORT the degrade at mount.
    pub halt_admit: HaltAdmit,

    /// **The ACCOUNT-aggregate open-notional ceiling**, folded onto
    /// `vike_model::RiskLimits::max_account_exposure` at the end of
    /// [`crate::make_engine_for_account`], so `vike_exec::RiskGate::check_inner`'s
    /// `over-account-exposure` lane judges every order the account mints. `None` (no
    /// `policy.max_account_exposure` row) switches the axis off and does NOT bite, deliberately
    /// ([`vike_config::Policy::max_account_exposure`] argues why).
    ///
    /// ⚠ Carried where [`vike_config::Policy::max_notional_per_order`] is NOT because this axis is
    /// not in `crate::require_live_risk_budget`'s refusal
    /// (`vike_model::RiskLimits::max_account_exposure`'s doc says why): it moves no live gate.
    pub max_account_exposure: Option<f64>,

    /// **The ceiling on the EQUITY FIGURE the sizing and admission lanes may see**, folded onto
    /// `vike_model::RiskLimits::max_sizing_equity` at the end of
    /// [`crate::make_engine_for_account`]: `vike_exec::ExecutionEngine::sizing_equity` answers
    /// `min(resolved equity, this)`. `None` (no `policy.max_sizing_equity` row) switches it off and
    /// does NOT bite ([`vike_config::Policy::max_sizing_equity`] argues why).
    ///
    /// ⚠ **It must never reach the margin-CALL sweep**: a lower equity figure is conservative for
    /// sizing and DESTRUCTIVE for a liquidation. The split is made where the number is read, by two
    /// resolvers: `vike_exec::ExecutionEngine::sizing_equity` (capped) and `resolved_equity` (the
    /// truth, kept by the watchdog and every report); that method's doc is the authority.
    pub max_sizing_equity: Option<f64>,

    /// **How far each venue may arm**: the per-venue CEILING, read at the top of
    /// [`crate::make_engine`] through [`Self::venue_mode`] and folded as [`VenueMode::cap`] (`min`,
    /// never `max`), so it can only REFUSE an arming the credentials would have produced.
    ///
    /// [`VenuePolicy::default()`] (every venue [`VenueMode::Paper`], no `policy.venues` rows) is the
    /// one default that BITES: credential presence stops being the only gate. The one upgrade
    /// hazard in the settings system, announced by [`crate::venue_arming_migration`].
    pub venues: VenuePolicy,

    /// **The settings database's `account` table, as the composition root read it** (not a ceiling).
    /// It reaches [`crate::make_engine_for_account`]'s venue match AND
    /// [`crate::venue_account_arming`], the projection `crates/vike-mount/src/node/accounts.rs`'s `refuse_unarmed_mount_accounts`
    /// judges a strategy mount against; both must resolve a dukascopy account IDENTICALLY (in
    /// `crates/bridges/dukascopy/src/mount.rs` the row's credential-key owner prefix picks which
    /// LEGAL ENTITY an order reaches).
    ///
    /// ⚠ [`From<&Policy>`](MountPolicy::from) cannot fill it (not a `policy` setting). The root SETS
    /// it after reading the store (`crates/vike-tradehub/src/tradehub_cli/live_mount.rs`'s
    /// `live_mount_with`); every other caller leaves [`AccountDirectory::unread`], which refuses
    /// every labelled dukascopy account by name and leaves the DEFAULT account unchanged, the same
    /// answer an unmigrated `Backend::Absent` box gets.
    pub accounts: AccountDirectory,

    /// **Every venue's `venue_setting` rows, as the composition root read them** (not a ceiling;
    /// here for `accounts`' reason). Each bridge receives its venue's view as
    /// `MountInputs::settings`, empty for a venue with no rows. [`From<&Policy>`](MountPolicy::from)
    /// cannot fill it: the root sets it after reading the store.
    pub venue_settings:
        std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
}

impl MountPolicy {
    /// This venue's arming ceiling, the cap [`crate::make_engine`] applies. An unknown venue
    /// answers [`VenueMode::Paper`] ([`VenuePolicy::get`]'s rule): an unknown label must not arm.
    #[must_use]
    pub fn venue_mode(&self, venue: &str) -> VenueMode {
        self.venues.get(venue)
    }
}

impl From<&Policy> for MountPolicy {
    /// Project the loaded policy onto what the mount applies.
    ///
    /// ⚠ The destructure is EXHAUSTIVE on purpose (module doc). Do not add `..`: a new `Policy`
    /// field must break this line and get a home or a written reason, never be silently dropped.
    fn from(policy: &Policy) -> Self {
        let Policy {
            // Not carried: the leverage authority is `ProfileRisk`/`im_requirement`, and this
            // field's `1.0` default would clamp every policy-file-less deployment to 1×.
            max_leverage: _,
            // Not carried: consumed at the order surfaces. Folding it into `RiskLimits` moves
            // `require_live_risk_budget`'s refusal, a decision, not plumbing.
            // ⚠ Not the only record: the run profile's `[risk]` ceiling of the same NAME DOES reach
            // `RiskLimits`, so this drop makes them two gates.
            // `vike_config::ceilings::PRE_TRADE_CEILINGS` carries both rows, `vike-cli config show`
            // renders them, `crates/vike-config/tests/ceilings_are_distinct.rs` fails if both
            // claim one site.
            max_notional_per_order: _,
            // CARRIED onto `vike_model::RiskLimits::max_account_exposure` in
            // `make_engine_for_account`, the first `Policy` ceiling to reach the pre-trade GATE: no
            // existing refusal is keyed on it (absent is uncapped; `require_live_risk_budget` does
            // not demand it).
            max_account_exposure,
            // CARRIED, for the same reason: the equity FIGURE the sizing and admission lanes see.
            // ⚠ Acting consumers ONLY. Keeping it off the margin-call sweep (which must judge the
            // real figure) is enforced by the engine's two resolvers, not by this struct.
            max_sizing_equity,
            // CARRIED. The one field with a venue seam waiting for it (#1051).
            market_slippage,
            // CARRIED to cTrader's mount (`crates/bridges/ctrader/src/mount.rs`'s
            // `CtraderVenueMount`) and REPORTED elsewhere when `verify` cannot be honoured.
            halt_admit,
            // Not carried, and NOT a gap: the dead-man switch belongs to the CORE
            // (`vike_core::CoreConfig::deadman` watches all ingest and trips through the core's
            // order path), built by the root: `crates/vike-tradehub/src/venue_arming/deadman.rs`'s
            // `deadman_config_from_policy`, called from `live_mount_with` (named by
            // `crates/vike-config/tests/policy_is_consumed.rs`). A copy here is read by nothing.
            deadman_timeout_ms: _,
            deadman_action: _,
            // Not carried, same argument: the LINK dead-man (`vike_core::CoreConfig::link_deadman`)
            // is the core's; the root folds it with the per-venue TABLE
            // (`vike_model::link_deadman_default`) in
            // `crates/vike-tradehub/src/venue_arming/deadman.rs`'s `link_deadman_config_from_policy`.
            link_deadman_grace_ms: _,
            // CARRIED: the per-venue arming CEILING (`paper < demo < live`), folded at the top of
            // `make_engine_with_legs` as `mode.cap(…)`: `min`, never `max`.
            // ⚠ CLONED: the one `Policy` field owning an allocation (what cost `MountPolicy` its
            // `Copy`); a fourteen-entry `BTreeMap` once per mount, per venue, beside a socket open.
            venues,
        } = policy;
        MountPolicy {
            market_slippage: *market_slippage,
            halt_admit: *halt_admit,
            max_account_exposure: *max_account_exposure,
            max_sizing_equity: *max_sizing_equity,
            venues: venues.clone(),
            // NOT from the policy file: the root fills it ([`MountPolicy::accounts`]).
            accounts: AccountDirectory::unread(),
            // NOT from the policy file: the root fills it.
            venue_settings: std::collections::BTreeMap::new(),
        }
    }
}

#[path = "policy_tests.rs"]
#[cfg(test)]
mod policy_tests;
