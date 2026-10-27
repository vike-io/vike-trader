//! [`AdapterPolicy`]: the projection of [`vike_config::Policy`] that the venue mount actually
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
//! [`AdapterPolicy::from`] destructures [`vike_config::Policy`] EXHAUSTIVELY (no `..`), so a new
//! `Policy` field breaks that line and forces a carried/not-carried decision with a written reason,
//! as `vike_model::VENUES` forces a row in every per-venue capability table. A runtime test cannot
//! do this (there is nothing to iterate), so the pattern IS the gate.
//!
//! ## What is NOT carried, and why
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
//! [`AdapterPolicy::max_leverage`]: a CEILING on the run profile's leverage, narrowing it (a `min`,
//! never a raise, and never a leverage of its own) at the top of [`crate::make_engine_for_account`].
//! It was "not carried" while its default was `1.0` — a binding `1.0` would have clamped every
//! deployment with no `policy` rows to 1× — and is carried now that absence is `None`.
//!
//! [`AdapterPolicy::halt_admit`]: how much evidence the HALT sentinel demands before a submit leaves.
//! `admit` (default) is the old rule; `verify` is REAL only on cTrader (the one adapter holding a
//! position book at its halt boundary). ⚠ An operator who sets `verify` and mounts binance must be
//! TOLD at mount that it degraded and why: `vike_model::effective_halt_admit` answers both from one
//! call, `vike_model::halt_verify_support` carries the per-venue reason.
//!
//! [`AdapterPolicy::accounts`]: **the account table, whose tier and `active` columns decide what each
//! account trades** (`vike_config::account_arming`'s `account_tier`). No row or an inactive row does NOT RUN and a
//! `paper` tier runs the paper simulator, so a box with credentials and no account row arms nothing:
//! arming on credential PRESENCE alone was the defect (MEASURED on the CI box: a one-venue run profile
//! holding NINE live authenticated exec sessions). It is not a `policy` setting: the root reads it
//! from the store.
//!
//! [`AdapterPolicy::preflight_failed`]: the venues the startup preflight hard-FAILED this session —
//! a STOP for their `demo`/`live` accounts (`ArmingBlock::PreflightFailed`). Not a setting either:
//! `crate::assemble_runtime` fills it from the report it ran.
//!
//! [`AdapterPolicy::market_slippage`]: the band a venue with **no native market order** prices its
//! emulated market/stop-market orders at. Hyperliquid is the only such venue
//! (`vike_model::market_slippage`'s module doc), so one venue arm is COMPLETE coverage. Bound:
//! `vike_bridge_core::market_slippage::resolve_market_slippage`; seam:
//! `crates/bridges/hyperliquid/src/exec/client.rs`'s `spawn_with_market_slippage` (#1051).
//!
//! [`AdapterPolicy::paper_slippage`] and [`AdapterPolicy::preflight_disk`]: the two `config.*` knobs the
//! mount itself applies (the trading-mount batch of settings rows) — the slippage a `paper`-tier
//! account's simulated exchange fills at, and the startup preflight's disk floors. Neither is a
//! `policy` ceiling and [`From<&Policy>`](AdapterPolicy::from) leaves both at their defaults, which
//! are the figures the mount hard-coded before the rows existed; the root sets them, so this crate
//! reads no setting.

use crate::preflight::DiskThresholds;
use vike_config::Policy;
use vike_model::AccountTier;
use vike_model::HaltAdmit;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;

/// The slippage, as a fraction of price (`0.0002` is 2 basis points), a `paper`-tier account's
/// simulated exchange fills at when [`AdapterPolicy::paper_slippage`] is `None` — the figure the
/// paper engine's book hard-coded before the `config.paper_slippage` row existed.
pub const DEFAULT_PAPER_SLIPPAGE: f64 = 0.0002;

/// The hard ceilings [`crate::make_engine`] applies, plus the facts that are not ceilings,
/// [`Self::accounts`], [`Self::venue_settings`] and [`Self::preflight_failed`], and the `config`
/// knobs the mount applies, [`Self::paper_slippage`] and [`Self::preflight_disk`].
///
/// ⚠ Those ride HERE because the mount and the arming projection are handed this struct and
/// nothing else in common, and must resolve a dukascopy ACCOUNT (and a venue setting, and a stopped
/// venue) identically; a second parameter beside it would let a caller give the two sides different
/// tables.
///
/// [`Default`] is exactly "no `policy` rows on this machine and no account table read" (every
/// scalar `None`, NO account running, every venue keeping its compiled-in literal): passing `None`,
/// passing `&AdapterPolicy::default()` and having no rows are the same thing, gated by
/// `an_absent_policy_file_is_exactly_the_start_default`.
///
/// ⚠ **Not `Copy`**: [`Self::accounts`] and [`Self::venue_settings`] own allocations. Call sites
/// pass `Option<&AdapterPolicy>` (`vike_tradehub_core::RuntimeConfig` hands out `Some(&cfg.policy)`); a site
/// wanting an owned one clones.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AdapterPolicy {
    /// Aggression band for an EMULATED market order, as a fraction (`0.002` = 0.2 %); `None` keeps
    /// the venue's own literal. The operator's REQUEST, not the applied band: hyperliquid's mount
    /// (`AdapterRequest::market_slippage` → `crates/bridges/hyperliquid/src/exec.rs`'s
    /// `market_slippage_for`) clamps it, warns, and never widens past the venue default.
    /// See [`vike_config::Policy::market_slippage`].
    pub market_slippage: Option<f64>,

    /// How much evidence the HALT sentinel demands before letting a submit out.
    /// [`HaltAdmit::Admit`] (default, no `policy.halt_admit` row) is the flag-trusting rule;
    /// [`HaltAdmit::Verify`] is applied by cTrader's mount and degrades elsewhere
    /// (`vike_model::halt_verify_support`). The operator's REQUEST, so [`crate::make_engine`] calls
    /// `vike_model::effective_halt_admit` to REPORT the degrade at mount.
    pub halt_admit: HaltAdmit,

    /// **The box-wide LEVERAGE CEILING**, `>= 1.0`; `None` (no `policy.max_leverage` row) is NO
    /// ceiling and changes nothing ([`vike_config::Policy::max_leverage`] argues why the default is
    /// absence, not `1.0`). It does not become a `RiskLimits` field: it NARROWS the run profile's
    /// `[risk] max_leverage` once, at the top of [`crate::make_engine_for_account`]
    /// (`vike_model::ProfileRisk::narrowed_to_leverage_ceiling`), so the buying-power gate and the
    /// leverage each perp bridge posts to its venue account read the SAME narrowed number — one
    /// enforcement site for live and `paper`-tier engines alike.
    pub max_leverage: Option<f64>,

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

    /// **The venues whose engines FAIL CLOSED on symbols** (`vike_model::RiskLimits::deny_unadmitted_symbols`,
    /// decision 0173 item 7 made per venue): an order on a symbol the engine was never told about is
    /// denied instead of rounded onto the mounted symbol's grid and signed. The root fills it from the
    /// venues its control door admits symbols for (`vike_tradehub::server::symbol_admission`); this crate
    /// names no venue. Empty (the default, and every other caller) leaves every engine as it was.
    pub fail_closed_symbol_venues: Vec<String>,

    /// **The venues whose engines may hold a symbol quoted in a currency other than the account's**
    /// (decision 0183, phase 1): the ones whose gatewayhub states a conversion rate for it. The root fills
    /// it from the same place it knows the door's other per-venue facts
    /// (`vike_tradehub::server::symbol_admission::quote_converting_venues`); this crate names no venue.
    /// An engine of a venue in the list is anchored to the account's own currency
    /// ([`crate::quote_conversion_tier_refusal`] says for which accounts). Empty (the default, and every
    /// other caller) leaves every engine as it was.
    pub quote_converting_venues: Vec<String>,

    /// **The venues whose gatewayhub rates the engine's OWN MOUNTED symbol too** (decision 0183, the
    /// mounted-symbol amendment): a subset of [`Self::quote_converting_venues`] whose lane states the quote
    /// currency and the rate of the symbol the engine was started on, so a ledger that is not in the mount's
    /// money (a EUR account on `EURUSD`) is told its currency and the mount is DECLARED foreign-quoted
    /// (`vike_exec::ExecutionEngine::declare_mount_quote_ccy`), its rate arriving by the node's first
    /// admission of it, instead of the anchor being declined. The root fills it
    /// (`vike_tradehub::server::symbol_admission::mount_converting_venues`); this crate names no venue. A venue
    /// NOT in the list keeps the mount fence (`unanchorable_mount`) exactly as it was. Empty (the default, and
    /// every other caller) leaves every engine as it was.
    pub mount_converting_venues: Vec<String>,

    /// **The settings database's `account` table, as the composition root read it** — the answer to
    /// "what tier does this account trade at": an account trades at its row's `tier` while the row
    /// is `active` (`vike_config::account_arming`'s `account_tier`, read at the top of
    /// [`crate::make_engine_for_account`] before any credential), and its row's `max_exposure`
    /// narrows that account's exposure ceiling. It also reaches the bridges: the arming projection
    /// ([`crate::venue_account_arming`], which `crates/vike-tradehub-core/src/node/accounts.rs`'s
    /// `refuse_unarmed_mount_accounts` judges a strategy mount against) and the mount must resolve a
    /// dukascopy account IDENTICALLY (in `crates/bridges/dukascopy/src/adapter.rs` the row's
    /// credential-key owner prefix picks which LEGAL ENTITY an order reaches).
    ///
    /// ⚠ [`From<&Policy>`](AdapterPolicy::from) cannot fill it (not a `policy` setting). The root SETS
    /// it after reading the store (`crates/vike-tradehub/src/tradehub_cli/live_runtime.rs`'s
    /// `live_runtime_with`); every other caller leaves [`AccountDirectory::unread`], under which NO
    /// account runs (no row states a tier, decision 0156). A test plants rows with
    /// [`Self::with_account`] or [`Self::with_account_row`].
    pub accounts: AccountDirectory,

    /// **Every venue's `venue_setting` rows — an IN-PROCESS TEST SEAM since wave 2; no production code
    /// fills it.** The tradehub reads no `venue_setting` row and opens no bridge, so nothing in a
    /// shipped tradehub sets this, and setting it there would reach no bridge. The gatewayhub reads
    /// its own rows at each step a tradehub restart drives (`crates/vike-gatewayhub/src/gatewayhub_cli/vars.rs`'s
    /// `trade_source`) and hands each bridge its venue's view as `AdapterInputs::settings`. What
    /// still reads THIS field are the in-process `VenueBackend`s that run the real bridges over an
    /// `AdapterPolicy` in tests (`crates/vike-gatewayhub/tests/trade_roster/support/bridge_backend.rs`,
    /// `crates/vike-gatewayhub/src/trade/adapters/testutil.rs`) and the venues-screen golden
    /// (`crates/vike-tradehub/src/venues_cli.rs`'s `arming_rows`), which hand it to `BridgeTrade`.
    /// [`From<&Policy>`](AdapterPolicy::from) cannot fill it (not a `policy` setting).
    pub venue_settings:
        std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,

    /// **The venues the startup preflight hard-FAILED this session** (`crate::preflight`'s
    /// `VenueDisposition::Stopped`) — a STOP for every `demo`/`live` account of each
    /// (`ArmingBlock::PreflightFailed`, `crate::arming`'s `account_arming_under`); a `paper` account
    /// still runs. `crate::assemble_runtime` fills it from the report it ran, before the first mount;
    /// [`From<&Policy>`](AdapterPolicy::from) and every other caller leave it empty.
    pub preflight_failed: std::collections::BTreeSet<String>,

    /// **The slippage a `paper`-tier account's simulated exchange fills at**, as a fraction of price
    /// (`config.paper_slippage`). `None` — no row, and every caller that sets nothing — keeps
    /// [`DEFAULT_PAPER_SLIPPAGE`], so a box without the row builds the book it always built. Fees are
    /// not part of it: a paper book keeps the venue's own fee schedule.
    ///
    /// Not a `policy` setting and not a ceiling: [`From<&Policy>`](AdapterPolicy::from) cannot fill it
    /// and the root sets it from its `config` section (`crates/vike-tradehub/src/tradehub_cli.rs`'s
    /// `MountRows`). Read by the paper tier's engine only; no `demo`/`live` account has a paper book.
    pub paper_slippage: Option<f64>,

    /// **The startup preflight's two disk floors** (`config.preflight_disk_warn_mib` and
    /// `config.preflight_disk_fail_mib`), which `crate::startup::run_startup_preflight` reads into
    /// the disk leg. [`Default`](DiskThresholds::default) — no rows — is the compiled pair, so a box
    /// without the rows runs the preflight it always ran. The leg watches the journal directory
    /// alone and never stops a venue.
    ///
    /// Not a `policy` setting: [`From<&Policy>`](AdapterPolicy::from) cannot fill it and the root sets
    /// it from its `config` section, as it does [`Self::paper_slippage`].
    pub preflight_disk: DiskThresholds,
}

impl AdapterPolicy {
    /// **Plant one ACTIVE account row** `(venue, tier, label)` in [`Self::accounts`] — the
    /// test/tool builder for "this account trades at `tier`". `label` is the row's `label` column
    /// (`None` for [`AccountLabel::Default`]); the row's `id` is one above the highest planted so
    /// far. An UNREAD directory becomes a KNOWN one holding this row (credential key names kept
    /// where the directory carried them, else none).
    #[must_use]
    pub fn with_account(self, venue: &str, label: &AccountLabel, tier: AccountTier) -> Self {
        let id = self.planted_rows().iter().map(|a| a.id).max().unwrap_or(0) + 1;
        self.with_account_row(vike_secrets::Account {
            id,
            venue: venue.to_string(),
            tier,
            label: label.text().map(str::to_string),
            venue_account_id: None,
            parent_id: None,
            active: true,
            last_verified_at: None,
            max_exposure: None,
            leverage: None,
        })
    }

    /// [`Self::with_account`] with a caller-built row: an inactive one, one carrying a
    /// `max_exposure` or a `venue_account_id` (a dukascopy book). The rows stay `id`-ordered, as
    /// `vike_secrets::read_accounts` returns them.
    #[must_use]
    pub fn with_account_row(mut self, row: vike_secrets::Account) -> Self {
        let mut rows = self.planted_rows();
        rows.push(row);
        rows.sort_by_key(|a| a.id);
        let keys = match self.accounts.keys() {
            Some(Ok(Some(keys))) => Some(keys.clone()),
            _ => None,
        };
        let book = self.planted_book();
        self.accounts = AccountDirectory::from_rows(vike_secrets::Accounts::Known(rows), keys)
            .with_credentials(book);
        self
    }

    /// **Hold `fields` (`(FIELD, value)` pairs) as the credentials of the active `(venue, label,
    /// tier)` account** — the row is planted first when the directory holds none. A test's door to
    /// "this account holds these keys" (decision 0156 item 6): the mount reads an account's
    /// credentials by FIELD out of the directory, never from a name map.
    #[must_use]
    pub fn with_credentials(
        self,
        venue: &str,
        label: &AccountLabel,
        tier: AccountTier,
        fields: &[(&str, &str)],
    ) -> Self {
        let mut me = if self.accounts.account_row(venue, label, tier).is_some() {
            self
        } else {
            self.with_account(venue, label, tier)
        };
        let id = me.accounts.account_row(venue, label, tier).map(|a| a.id).unwrap_or_else(|| {
            panic!(
                "no single active account row for ({venue}, {label}, {tier}) to hold credentials"
            )
        });
        let mut book = me.planted_book();
        for (field, value) in fields {
            book.insert_account(id, *field, *value);
        }
        let rows = me.planted_rows();
        let keys = match me.accounts.keys() {
            Some(Ok(Some(keys))) => Some(keys.clone()),
            _ => None,
        };
        me.accounts = AccountDirectory::from_rows(vike_secrets::Accounts::Known(rows), keys)
            .with_credentials(book);
        me
    }

    /// **Hold `fields` as `venue`'s APPLICATION credentials** (cTrader's OAuth client), shared by every
    /// account of the venue — a test's door, like [`Self::with_credentials`].
    #[must_use]
    pub fn with_venue_credentials(mut self, venue: &str, fields: &[(&str, &str)]) -> Self {
        let mut book = self.planted_book();
        for (field, value) in fields {
            book.insert_venue(venue, *field, *value);
        }
        let rows = self.planted_rows();
        let keys = match self.accounts.keys() {
            Some(Ok(Some(keys))) => Some(keys.clone()),
            _ => None,
        };
        self.accounts = AccountDirectory::from_rows(vike_secrets::Accounts::Known(rows), keys)
            .with_credentials(book);
        self
    }

    /// The credential book [`Self::accounts`] already carried, or an empty one.
    fn planted_book(&self) -> vike_secrets::account_credentials::CredentialBook {
        match self.accounts.credentials() {
            Some(Ok(book)) => book.clone(),
            _ => vike_secrets::account_credentials::CredentialBook::new(),
        }
    }

    /// The rows [`Self::accounts`] already answered, or none (unread, unanswerable or unreadable).
    fn planted_rows(&self) -> Vec<vike_secrets::Account> {
        match self.accounts.rows() {
            Some(Ok(accounts)) => accounts.known().map(<[_]>::to_vec).unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

impl From<&Policy> for AdapterPolicy {
    /// Project the loaded policy onto what the mount applies.
    ///
    /// ⚠ The destructure is EXHAUSTIVE on purpose (module doc). Do not add `..`: a new `Policy`
    /// field must break this line and get a home or a written reason, never be silently dropped.
    fn from(policy: &Policy) -> Self {
        let Policy {
            // CARRIED, as a CEILING on the run profile's leverage (never a leverage of its own):
            // `make_engine_for_account` narrows the profile's `[risk] max_leverage` with it, so the
            // gate's `im_requirement` and the bridges' account leverage follow one number. Absent
            // is `None` — no ceiling — which is what keeps a policy-row-less deployment exactly as
            // it was (the old `1.0` default is gone; it would have clamped everyone to 1×).
            max_leverage,
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
            // CARRIED to cTrader's mount (`crates/bridges/ctrader/src/adapter.rs`'s
            // `CtraderVenueAdapter`) and REPORTED elsewhere when `verify` cannot be honoured.
            halt_admit,
            // Not carried, and NOT a gap: the dead-man switch belongs to the CORE
            // (`vike_core::CoreConfig::deadman` watches all ingest and trips through the core's
            // order path), built by the root: `crates/vike-tradehub/src/venue_arming/deadman.rs`'s
            // `deadman_config_from_policy`, called from `live_runtime_with` (named by
            // `crates/vike-config/tests/policy_is_consumed.rs`). A copy here is read by nothing.
            deadman_timeout_ms: _,
            deadman_action: _,
            // Not carried, same argument: the LINK dead-man (`vike_core::CoreConfig::link_deadman`)
            // is the core's; the root folds it with the per-venue TABLE
            // (`vike_venues::link_deadman_default`) in
            // `crates/vike-tradehub/src/venue_arming/deadman.rs`'s `link_deadman_config_from_policy`.
            link_deadman_grace_ms: _,
        } = policy;
        AdapterPolicy {
            market_slippage: *market_slippage,
            halt_admit: *halt_admit,
            max_leverage: *max_leverage,
            max_account_exposure: *max_account_exposure,
            max_sizing_equity: *max_sizing_equity,
            // NOT from the policy file: the root fills it (`fail_closed_symbol_venues`).
            fail_closed_symbol_venues: Vec::new(),
            // NOT from the policy file: the root fills it (`quote_converting_venues`).
            quote_converting_venues: Vec::new(),
            // NOT from the policy file: the root fills it (`mount_converting_venues`).
            mount_converting_venues: Vec::new(),
            // NOT from the policy file: the root fills it ([`AdapterPolicy::accounts`]).
            accounts: AccountDirectory::unread(),
            // NOT from the policy file: the root fills it.
            venue_settings: std::collections::BTreeMap::new(),
            // NOT from the policy file: `crate::assemble_runtime` fills it from its preflight report.
            preflight_failed: std::collections::BTreeSet::new(),
            // NOT from the policy file: it is the `config.paper_slippage` row, set by the root.
            paper_slippage: None,
            // NOT from the policy file: the two `config.preflight_disk_*_mib` rows, set by the root.
            preflight_disk: DiskThresholds::default(),
        }
    }
}

#[path = "policy_tests.rs"]
#[cfg(test)]
mod policy_tests;
