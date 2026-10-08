//! [`PolicyPatch`], the all-`Option` file shape a layer patches in, and the `[rate]` tombstone.

use serde::Deserialize;
use std::collections::BTreeMap;
use vike_model::HaltAdmit;

#[cfg(doc)]
use super::{DEFAULT_LINK_DEADMAN_GRACE_MS, Policy};
use super::{DeadManActionSetting, VenueMode};

/// The FILE shape of [`Policy`]: every field optional, so a layer patches only what it names.
///
/// Deserialization goes through this rather than through `Policy` itself, because the loader is
/// LAYERED: `#[serde(default)]` on the effective struct would reset a key the home file set back
/// to the code default whenever a later file omitted it. An all-`Option` patch expresses
/// "unmentioned = inherit", which is what a precedence chain means.
///
/// `deny_unknown_fields` is the other half of the contract: a mistyped key is rejected by NAME.
/// A typo'd setting that silently does nothing is worse than an error — the operator sets a
/// ceiling, sees no complaint, and does not have it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPatch {
    /// See [`Policy::max_leverage`].
    pub max_leverage: Option<f64>,
    /// **TOMBSTONE — removed, and REFUSED rather than ignored.** Not a [`Policy`] field.
    ///
    /// `max_total_exposure` was a `Policy` ceiling that nothing in the workspace ever read (see
    /// this module's doc). Deleting it outright would let `deny_unknown_fields` reject the key
    /// with serde's generic "unknown field" — technically safe, since the load fails and nobody
    /// trades on a limit they do not have, but it tells an operator only that the key is wrong,
    /// not that the protection they wanted lives somewhere else and is already mandatory there.
    ///
    /// So the key is still PARSED, purely so [`Policy::apply`] can refuse it by name and point at
    /// the exposure cap that IS enforced: `max_total_exposure` in a **run profile's `[risk]`
    /// table** (`vike_exec::ProfileRisk` → `vike_exec::RiskLimits`), which `RiskGate::check_inner`
    /// evaluates on every order and which `vike_mount::require_live_risk_budget` already refuses
    /// to mount a live venue without.
    ///
    /// ⚠ The redirect must NOT be read as an equivalence, and the refusal message says so: that
    /// run-profile cap is scoped to **ONE symbol at ONE venue**, not the account — see
    /// `vike_exec::RiskLimits::max_total_exposure`'s doc and the scope pin
    /// `crates/vike-exec/tests/risk/risk_lane_pricing.rs`'s
    /// `max_total_exposure_is_scoped_to_one_venue_and_one_symbol`. Sending an operator who wanted a
    /// book-wide limit to a per-symbol key while calling it "aggregate" would re-create this very
    /// field's defect one file over: a number that looks like the protection asked for and is N×
    /// weaker.
    ///
    /// ⚠ **The refusal offers BOTH, because the account-aggregate ceiling exists:**
    /// [`Policy::max_account_exposure`] is the aggregate this key's NAME reads as, in this
    /// same file, and the message offers it as the rename beside the per-instrument redirect. An
    /// operator who wrote this key was, on the evidence of the name they chose, usually asking for
    /// that one.
    ///
    /// Same argument as [`crate::refuse_removed_env`]: a setting that silently does nothing is the
    /// failure mode worth engineering against, and the fix belongs in the error message.
    pub max_total_exposure: Option<f64>,
    /// See [`Policy::max_notional_per_order`].
    pub max_notional_per_order: Option<f64>,
    /// See [`Policy::max_account_exposure`] — the ACCOUNT-aggregate ceiling, and the key
    /// [`PolicyPatch::max_total_exposure`]'s tombstone now redirects to. Bounds are checked in
    /// [`Policy::apply`], where the file can be named.
    pub max_account_exposure: Option<f64>,
    /// See [`Policy::max_sizing_equity`] — the ceiling on the equity FIGURE the sizing and
    /// admission lanes may see, as distinct from the ceiling on what may be HELD directly above.
    /// Bounds are checked in [`Policy::apply`], where the file can be named.
    pub max_sizing_equity: Option<f64>,
    /// See [`Policy::market_slippage`].
    pub market_slippage: Option<f64>,
    /// See [`Policy::halt_admit`]. An unknown spelling fails HERE, at deserialization, naming the
    /// legal variants — so `halt_admit = "refuse"` is a load error rather than a mode an operator
    /// believes they configured.
    pub halt_admit: Option<HaltAdmit>,
    /// See [`Policy::deadman_timeout_ms`]. A `u64` rather than a float: the value is a count of
    /// milliseconds, and a fractional millisecond in a dead-man timeout is a typo, not a setting.
    /// Bounds are checked in [`Policy::apply`], where the file can be named.
    pub deadman_timeout_ms: Option<u64>,
    /// See [`Policy::deadman_action`]. An unknown spelling fails HERE, at deserialization, naming
    /// the legal variants — the [`PolicyPatch::halt_admit`] idiom.
    pub deadman_action: Option<DeadManActionSetting>,
    /// See [`Policy::link_deadman_grace_ms`]. A `u64` for the same reason as
    /// [`PolicyPatch::deadman_timeout_ms`]: the value is a count of milliseconds, and a fractional
    /// one is a typo rather than a setting. Bounds are checked in [`Policy::apply`], where the file
    /// can be named. ⚠ Absent here means "inherit", and the last layer's absence means ARMED at
    /// [`DEFAULT_LINK_DEADMAN_GRACE_MS`] — the opposite of the sibling above, which is off when
    /// absent.
    pub link_deadman_grace_ms: Option<u64>,
    /// **TOMBSTONE — removed, and REFUSED rather than ignored.** Not a [`Policy`] field.
    ///
    /// `[rate] max_utilization` was a ceiling on `Preferences::rate_utilization`, and that
    /// preference was read by NOTHING: the target utilization every pacer actually spends comes
    /// from `vike_binance::family::klines::KlineSpec::utilization`, which every construction site
    /// fills with the compiled-in [`vike_model::rate_limits::DEFAULT_UTILIZATION`]. So the ceiling
    /// bounded a value no code consumed — the `max_total_exposure` shape one field over, with the
    /// extra twist that the clamp emitted a warning naming both halves and neither did anything.
    ///
    /// The concept did not disappear; it has a better home. [`vike_model::rate_limits`] owns the
    /// number, its bounds and its per-VENUE overrides ([`vike_model::RateLimitConfig`]) — which is
    /// the shape a pacing knob has to have, since one global float cannot say "spend 40 % on
    /// binance and 20 % on aster". The settings pair was a venue-blind duplicate of it, sitting in
    /// a crate no pacer can reach. It comes back when the value can genuinely arrive at
    /// `KlineSpec::utilization` — which needs the fetchers' uniform 4-arg shape to carry it and a
    /// settings-loading binary somewhere in `vike-backfill`, neither of which exists today.
    ///
    /// Same argument as [`crate::refuse_removed_env`] and as
    /// [`PolicyPatch::max_total_exposure`]: a setting that silently does nothing is the failure
    /// mode worth engineering against, and the fix belongs in the error message.
    pub rate: Option<RatePolicyPatch>,
    /// See [`Policy::venues`]. The `[venues]` table, as a raw `venue -> mode` map.
    ///
    /// ⚠ **`deny_unknown_fields` does NOT reach inside this map, and cannot.** It governs the
    /// FIELD names of this struct — `venues` is one field — while serde treats a map's KEYS as
    /// free-form data by construction. So the two halves of validation are split, deliberately and
    /// visibly:
    ///
    /// * the **MODE** is refused by serde HERE, at deserialization, with the legal spellings in the
    ///   message — the [`Policy::halt_admit`] idiom, and free because the value type is an enum;
    /// * the **VENUE** is refused by [`Policy::apply`], by hand, against
    ///   [`vike_model::VENUES`] — nothing else can, and a ceiling written against a venue that does
    ///   not exist applies to nothing while looking exactly like a ceiling that applies to
    ///   something.
    ///
    /// A `BTreeMap` rather than a `HashMap` so the refusal names the alphabetically-first offender
    /// deterministically: a load error that changes text between runs is one an operator cannot
    /// grep a log for.
    pub venues: Option<BTreeMap<String, VenueMode>>,
    /// **Per-ACCOUNT arming ceilings** — the `policy.accounts.<venue>.<LABEL>` rows, assembled into
    /// this `accounts` map as `venue -> LABEL -> mode`:
    ///
    /// ```text
    /// # the venue ceiling — caps every account below it
    /// vike-cli config set policy.venues.hyperliquid live
    /// # a SECOND hyperliquid account, armed
    /// vike-cli config set policy.accounts.hyperliquid.ALT live
    /// vike-cli config set policy.accounts.hyperliquid.TEST demo
    /// ```
    ///
    /// The label is the one from the credential key's
    /// [`vike_model::accounts::account_keys::ACCOUNT_SEPARATOR`] suffix — `HYPERLIQUID_LIVE_API_KEY__ALT` is
    /// account `ALT`. `vike_model::accounts::account_keys::accounts_in_store` is what enumerates them, from
    /// the STORE rather than from any list here: this table states ceilings for accounts, it does
    /// not declare that they exist.
    ///
    /// ⚠ **Three validations, in three different places, for the same structural reason
    /// [`PolicyPatch::venues`] gives:** `deny_unknown_fields` governs this struct's FIELD names and
    /// reaches inside no map. So the MODE is refused by serde (the value type is an enum), the
    /// VENUE by [`Policy::apply`] against [`vike_model::VENUES`], and the LABEL by
    /// [`Policy::apply`] against [`vike_model::accounts::account_keys::AccountLabel::parse`] — which is also
    /// what refuses the reserved `DEFAULT` spelling, whose ceiling is the venue's own line.
    ///
    /// ⚠ **STEP 1: this table is PARSED, VALIDATED, STORED — and folded by nothing.** It is
    /// accepted now, ahead of its consumer, deliberately: an unknown key is refused BY NAME and
    /// takes the whole `policy` section down with it, so a binary that does not yet understand
    /// `policy.accounts` would turn an operator's forward-looking rows into a total settings
    /// failure — every ceiling in it lost, not just the new one. Landing the parse first is what
    /// makes the step-2 rollout an ordinary upgrade. Because an inert settings key is the exact
    /// failure this workspace engineers against, [`fn@crate::load`] emits a WARNING naming this
    /// table whenever one is present, so nobody can believe an account ceiling is armed while it
    /// is not.
    pub accounts: Option<BTreeMap<String, BTreeMap<String, VenueMode>>>,
    /// **Per-ACCOUNT exposure ceilings** — the `policy.account_exposure.<venue>.<LABEL>` rows,
    /// assembled into this `account_exposure` map as `venue -> LABEL -> figure`:
    ///
    /// ```text
    /// # the BOX-WIDE ceiling, every engine carries it
    /// vike-cli config set policy.max_account_exposure 50000
    /// # the unlabelled account's own figure
    /// vike-cli config set policy.account_exposure.binance.DEFAULT 50000
    /// # …and a tighter one for the second account
    /// vike-cli config set policy.account_exposure.binance.ALT 5000
    /// ```
    ///
    /// ⚠ **Why it is not a field inside [`PolicyPatch::accounts`], which is the obvious place.**
    /// That table refuses the reserved `DEFAULT` spelling by design — an account's MODE ceiling is
    /// the venue's own line, so a second place to write it would be two answers to one question. An
    /// exposure FIGURE has no venue line to inherit: `max_account_exposure` is box-wide. Folding
    /// this into `[accounts]` would therefore have left the unlabelled account — the only account
    /// most boxes have — with no way to state its own number, forever. A separate table has no such
    /// constraint, and it also keeps the value type a plain `f64` rather than forcing
    /// `[accounts]`'s value into an untagged mode-or-table enum, whose parse failure is the
    /// undifferentiated "data did not match any variant" this file's refusals exist to avoid.
    ///
    /// ⚠ **Three validations, the same split [`PolicyPatch::accounts`] carries**, with one
    /// difference: the LABEL goes through [`vike_model::accounts::account_keys::parse_wire_account`] rather
    /// than `AccountLabel::parse`, because that is the reader which ADMITS `DEFAULT`. The FIGURE is
    /// refused here too — serde will take any `f64`, including a negative one, and since the fold
    /// is a `min` a negative ceiling would become the binding one and refuse every order.
    ///
    /// ⚠ **It NARROWS, never raises.** `vike_mount::make_engine_for_account` folds
    /// `max_account_exposure` and then this, both through
    /// `vike_exec::RiskLimits::narrow_account_exposure` — a `min` — so a line here can only ever
    /// make one account's ceiling tighter than the box's. Either may be absent; absent on both
    /// leaves the lane switched off exactly as before this table existed.
    pub account_exposure: Option<BTreeMap<String, BTreeMap<String, f64>>>,
}

/// The file shape of the removed `[rate]` table — kept ONLY so [`Policy::apply`] can refuse the key
/// by name. See [`PolicyPatch::rate`].
///
/// `max_utilization` stays an `Option<f64>` rather than becoming a catch-all so `[rate]` with any
/// OTHER key inside it still fails through `deny_unknown_fields` on the inner table, and the one
/// key an operator plausibly wrote gets the explanatory refusal.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatePolicyPatch {
    /// See [`PolicyPatch::rate`] — a tombstone, refused at apply time.
    pub max_utilization: Option<f64>,
}
