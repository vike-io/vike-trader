//! **THE VENUE ADAPTER CONTRACT** — the one trait each venue bridge implements, so that `vike-tradehub-core`
//! names no venue (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`).
//!
//! # Two phases, one source of truth
//!
//! [`VenueAdapter::resolve`] is PURE — no network, no thread, no side effect beyond a `tracing`
//! event — and it IS the arming probe: `vike-tradehub-core`'s arming projection calls it.
//! [`VenueAdapter::start`] acts on the SAME answer (it calls `resolve`, or the one private function
//! `resolve` is built on), so the probe and the arm cannot disagree: they are one function. What
//! "pure" permits, and its one declared exception, are on [`VenueAdapter::resolve`] itself.
//!
//! # The rules every implementation keeps
//!
//! 1. **Inputs are data.** A bridge reads no process environment, no process-global path and no
//!    `vike-config` type: the started ACCOUNT's credentials arrive in [`AdapterInputs::credentials`]
//!    (by FIELD — decision 0156 item 6: a bridge never composes a credential name), the deployment's
//!    named values in [`AdapterInputs::secrets`], venue settings and the venue's addresses (its
//!    `venue_url` rows, `VenueSettings::urls`) in [`AdapterInputs::settings`], the
//!    state and bin directories and the HALT sentinel's path in [`AdapterInputs::process`], the FX login gate's
//!    clock in [`AdapterInputs::fx_clock`].
//! 2. **The account's tier crosses as [`AdapterInputs::tier`], a `vike_model::AccountTier`** — the
//!    one tier vocabulary, an account FACT, never a policy type: this crate does not depend on
//!    `vike-config`, so `AdapterPolicy` cannot be named here
//!    (`docs/decisions/0129-one-tier-vocabulary-in-vike-model.md`). The tiers a bridge answers in
//!    ([`ArmingResolution::Armed`], [`VenueAdapterExecutionRunning::bound_tier`], [`BookConfirmed::tier`]) are the same
//!    type. A `live`-tier account whose arm binds a lower tier is `vike-tradehub-core`'s to hold at paper
//!    (a live account never trades demo); a bridge reports what it would bind and decides nothing
//!    about it.
//! 3. **Only `vike-tradehub-core` builds a paper engine.** A bridge that declines returns
//!    [`VenueAdapterExecutionOutcome::NotRunning`]; `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs` holds it.
//! 4. **There is no fallback grid** (owner, 2026-11-13: "remove all hardcoded symbols"). An exec thread
//!    whose mounted symbol's properties have not loaded refuses its new orders and amends in words, still
//!    sends its cancels, and fetches again ([`await_properties`]); a start whose pre-fetch failed reports no grid (`None`, the permissive default).
//! 5. **This module is a SHARED HOME.** It is extended by ONE coordinated change informed by every
//!    consumer. A venue port that finds a missing knob stops and reports; it never widens a type
//!    here in passing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use vike_execution::recon::{ReconcileClient, ReconcileFetchError};
use vike_execution::{EventSender, ExecutionClient};
use vike_model::AccountTier;
use vike_model::ProfileRisk;
use vike_model::VenueNetwork;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{HaltAdmit, MarginMode, SymbolProperties};
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::venue_setting::VenueSettings;

use crate::exec_actor::{
    ExecutionClientActorCancelOutcome, ExecutionClientActorCommand, cancel_event,
};
use crate::rest::{VenueRest, VenueRestRefusing};
use crate::transport::RestTransport;
use vike_secrets::account_directory::AccountDirectory;

/// Process facts a start may need, resolved ONCE by `vike-tradehub-core`, so a bridge reads no process
/// global. `Default` is "this process declared no project" — a test, a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessPaths {
    /// `<project>/settings/state`, as the boot declared it.
    pub state_dir: Option<PathBuf>,
    /// `<project>/bin`, derived from `state_dir`.
    pub bin_dir: Option<PathBuf>,
    /// **The operator HALT sentinel** — the one file the process's kill switch is, as
    /// `vike-tradehub-core` resolved it (`vike_boot::halt::halt_path_from_env`; `<state_dir>/HALT` whenever the
    /// boot declared a project). Every live exec client the start builds is handed THIS path with
    /// its `with_halt_path` builder, and checks `vike_execution::halt::sentinel_engaged` against it at its
    /// submit seam; a bridge resolves nothing itself (decision 0099).
    ///
    /// ⚠ The `Default` is EMPTY, and an empty path watches nothing — a test or a tool that built
    /// facts by hand. `vike-tradehub-core` always fills it, so a real start never sees the empty one, and
    /// a client handed it logs `HALT KILL SWITCH IS NOT WIRED` rather than looking armed.
    pub halt_path: PathBuf,
}

/// **The resolved `flags.*` rows a bridge's start reads, as plain bools** — the composition root resolved
/// each from the settings database (decision 0095; `crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s
/// `adapter_flags`), so a bridge names no settings key and links no `vike-config` (decision 0095's amendment of
/// 2026-11-22). `Default` is every flag OFF, the guarded state of each.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdapterFlags {
    /// `flags.poly_exec`: Polymarket's exec gate (`vike_polymarket::polymarket_execution_enabled`).
    pub poly_exec: bool,
    /// `flags.poly_reconcile`: Polymarket's reconcile gate (`vike_polymarket::polymarket_reconcile_enabled`).
    pub poly_reconcile: bool,
    /// `flags.hyperliquid_hip3`: Hyperliquid's HIP-3 builder-dex universe.
    pub hyperliquid_hip3: bool,
    /// `flags.allow_withdraw_keys`: start an account whose key can WITHDRAW (the override of the live-start
    /// withdraw-key gate, `vike_secrets::key_permissions::withdraw_gate_before_start`). Off refuses such a key,
    /// its guarded state.
    pub allow_withdraw_keys: bool,
}

/// Everything a bridge may know about the deployment — DATA ONLY.
#[derive(Clone, Copy)]
pub struct AdapterInputs<'a> {
    /// The account being resolved or started; the default account on a single-account box.
    pub account: &'a AccountLabel,
    /// **The started account's credentials, by FIELD** — the store's `credential.account_id` +
    /// `field` for the ONE account row this start is (`vike-tradehub-core` resolves it from `(venue,
    /// label, tier)`), plus the venue's application rows (`venue_id` + `field`). A bridge asks it
    /// for `API_KEY` and never composes a name (decision 0156 item 6); empty is the live gate.
    pub credentials: AccountCredentials<'a>,
    /// **The deployment's NAMED values** — the composition root's name-keyed map: the attribution codes
    /// and the store's tier-less deployment rows (a builder-fee knob, the JForex tool paths). No
    /// account's credential is read out of it: those are [`Self::credentials`]; the venue gates are
    /// [`Self::flags`].
    pub secrets: &'a HashMap<String, String>,
    /// The resolved `flags.*` rows a start reads ([`AdapterFlags`]).
    pub flags: AdapterFlags,
    /// This venue's `venue_setting` rows, and its `venue_url` ADDRESSES: a bridge dials none but
    /// `settings.urls().url(UrlType::of_tier(tier), channel, protocol, product)`
    /// (`vike_venues::venue_urls::VenueUrls::url`, the one lookup).
    pub settings: &'a VenueSettings,
    /// **The starting account's tier**, as its `account` row states it (`vike-tradehub-core` computes it
    /// from the account table: `crates/vike-config/src/account_arming.rs`'s `account_tier`).
    ///
    /// ⚠ `vike-tradehub-core` never hands a bridge [`AccountTier::Paper`]: a paper tier returns before any
    /// bridge is consulted (`crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s `contract_arming`). A bridge
    /// still matches it EXHAUSTIVELY and reads it the way it reads [`AccountTier::Demo`] — the
    /// not-live branch, exactly what `live_permitted == false` meant before decision 0129 — so the
    /// one tier that may reach real money is [`AccountTier::Live`] and only it.
    pub tier: AccountTier,
    /// The settings database's `account` table, as the composition root read it.
    pub accounts: &'a AccountDirectory,
    /// The state and bin directories, and the HALT sentinel's path.
    pub process: &'a ProcessPaths,
    /// **The FX login gate's clock** (owner ruling 2026-11-13: no FX venue logs in, reconnects or retries a login
    /// between Friday 22:00 and Sunday 21:00 UTC, `vike_venues::session::fx_login_closed_until`). The gatewayhub hands
    /// its wall clock to every input it builds for a venue session; an FX bridge puts it on the config its sessions
    /// are built from. `None` is no gate: the session logs in on any day (a tool, a smoke, a test that does not care).
    pub fx_clock: Option<&'a Arc<dyn vike_model::Clock + Send + Sync>>,
}

/// Why an account is NOT RUNNING — one variant per `vike_model::ArmingBlock` a BRIDGE can produce. The
/// others (`PaperTier`, `AccountInactive`, `NoAccountRow`, `TierConflict`, `FeatureAbsent`,
/// `NoAccountSupport`, `SidecarHeldElsewhere`) are `vike-tradehub-core`'s own and are never a bridge's
/// answer; `OrdersHeld` is not a paper cause at all (the row starts at its tier), and a bridge
/// says it with [`ArmingHeldBelowLive::OrdersHeld`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmingNotRunningCause {
    /// The account holds no usable key set — the original live gate.
    NoCredentials,
    /// The account's tier is `live`, the arm could reach it, and the account holds no usable key
    /// set (decision 0095: for the venues whose network is the tier, `live` means mainnet). Maps to
    /// `vike_model::ArmingBlock::LiveCredentialsAbsent`.
    LiveCredentialsAbsent,
    /// **The account's tier is `live`, it holds a key set, and this arm does not trade live** — it
    /// starts only its demo, practice or sandbox tier (it has no live arm), or, for an arm that
    /// HAS one (ibkr), its live tier refused this account under one of its own rules, which its
    /// `start` log names. Maps to `vike_model::ArmingBlock::LiveTierNotWired`.
    ///
    /// The cause for a credential that is PRESENT and UNUSABLE (an error, not an absence), and
    /// deliberately neither neighbour: [`Self::NoCredentials`] sends an operator to WRITE keys, and
    /// [`Self::LiveCredentialsAbsent`] is the opposite fact. A bridge that answers it also reports it
    /// at `start` through [`report_live_tier_not_wired`], so the log and the arming screen agree; a
    /// LABELLED account, never started, is spoken for by [`VenueAdapter::report_not_running_account`].
    /// A HALF-written set is not this cause (the loader calls it absent, so it stays
    /// [`Self::NoCredentials`]); the start names the missing fields.
    LiveTierNotWired,
    /// Polymarket's exec switch is not set.
    ExecFlagUnset,
    /// The venue has no demo tier and the account's tier is below `live`.
    LiveOnlyArm,
    /// A native SDK this venue needs did not load on this box.
    SdkAbsent,
    /// The settings database cannot say which account this is.
    AccountNotInStore,
    /// The bridge has no live arm at all — the scaffold's conservative answer.
    NoLiveArm,
}

/// What holds an armed resolution below LIVE TRADING. Two kinds, and `vike-tradehub-core`
/// (`crates/vike-gatewayhub/src/trade/adapters/contract.rs`'s `resolution_to_arming`) folds them differently:
///
/// * **The demo-arm reasons** ([`Self::DemoOnlyArm`], [`Self::LiveCredentialsAbsent`]) ride a
///   DEMO arming. `vike-tradehub-core` reads them only when the account's tier is `live`, and then holds the
///   account at PAPER, because a live account never trades demo; below that the demo tier IS the
///   account's tier and nothing is being refused.
/// * **[`Self::OrdersHeld`]** rides an arming at the account's OWN tier: the arm binds that tier and
///   sends no order (`docs/decisions/0140-ibkr-rides-one-session-one-product-one-tier.md`, stage A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmingHeldBelowLive {
    /// The arm hardcodes its demo endpoint.
    DemoOnlyArm,
    /// The arm can reach live, but only demo-tier credentials exist. ⚠ No bridge answers it since
    /// decision 0156 item 6: an account has ONE tier and its credentials are that tier's, so there is
    /// no demo set beside a live one to fall back to.
    LiveCredentialsAbsent,
    /// **Connect, serve data, send no orders.** The arm BINDS the tier it is armed at (a live
    /// session against the real account, its data and its reconcile), and its exec client REFUSES
    /// every order: each submit gets a synthesized terminal `OrderRejected` (no order may silently
    /// vanish), loudly, and never reaches the venue. For an arm whose live order path is not proven,
    /// e.g. a fill path nobody has observed: a fill nobody can see would let a real position build
    /// up unseen.
    ///
    /// The bridge's own client refuses, AND the gatewayhub refuses in front of it: it maps this to
    /// `vike_model::ArmingBlock::OrdersHeld` and wraps the bound client so no order reaches it
    /// (`crates/vike-gatewayhub/src/trade/adapters/orders_held.rs`'s `hold_orders`, in the shared
    /// `crate::execution_client::orders_held::ExecutionClientOrdersHeld`). A cancel still passes, because
    /// a hold must never trap an operator in an order.
    OrdersHeld,
}

/// The pure arming answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmingResolution {
    /// Does not run, and why.
    NotRunning(ArmingNotRunningCause),
    /// Arms at `tier`; `held_below_live` says why the arming does not trade live: a demo arming's
    /// reason, or [`ArmingHeldBelowLive::OrdersHeld`] for an arm that binds `tier` and sends no order.
    Armed { tier: AccountTier, held_below_live: Option<ArmingHeldBelowLive> },
}

/// The contract's tier for `network` on a venue whose tier IS the network (decision 0095 D1):
/// `Live` on mainnet, `Demo` on the demo or test network. A venue hands in
/// `VenueNetwork::for_tier` of the account's tier, or the network its handshake ran on.
#[must_use]
pub fn bound_tier(network: VenueNetwork) -> AccountTier {
    match network {
        VenueNetwork::Mainnet => AccountTier::Live,
        VenueNetwork::Testnet => AccountTier::Demo,
    }
}

/// **The arming probe of a one-key-set venue whose tier is the network** (decision 0156 item 6):
/// the account's complete set (`has_keys`) arms its own tier — [`bound_tier`] of `network` — with
/// nothing held below live; with none, a mainnet (`live`) account answers
/// [`ArmingNotRunningCause::LiveCredentialsAbsent`] and a testnet one [`ArmingNotRunningCause::NoCredentials`]. Each venue
/// hands in `VenueNetwork::for_tier` of the account's tier.
#[must_use]
pub fn resolve_one_key_set(has_keys: bool, network: VenueNetwork) -> ArmingResolution {
    match (has_keys, network) {
        (true, _) => ArmingResolution::Armed { tier: bound_tier(network), held_below_live: None },
        (false, VenueNetwork::Mainnet) => {
            ArmingResolution::NotRunning(ArmingNotRunningCause::LiveCredentialsAbsent)
        }
        (false, VenueNetwork::Testnet) => {
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
        }
    }
}

/// What a `live` account with no key set is told by a one-key-set arm ([`report_live_credentials_absent`]):
/// the line `vike-tradehub-core`'s legacy prefix logged at its `MainnetNoCreds` site, after `{venue}: `.
pub const LIVE_CREDENTIALS_ABSENT: &str = "the account's tier is `live` (MAINNET) but no LIVE \
     credentials are present → this account does NOT RUN (a mainnet host is never signed with demo keys; \
     absent credentials are the live gate)";

/// **The loud half of [`ArmingNotRunningCause::LiveCredentialsAbsent`]** for a one-key-set arm whose tier is the
/// network: a start that found no key set calls it with the account's `tier`, and only `Live` (the
/// mainnet account [`resolve_one_key_set`] answers that cause for) is said, at `warn!`, with `venue`
/// and `account`. A `demo` account with no keys is the ordinary unconfigured state and stays silent.
pub fn report_live_credentials_absent(
    venue: &'static str,
    account: &AccountLabel,
    tier: AccountTier,
) {
    match tier {
        AccountTier::Live => {
            tracing::warn!(venue, account = %account, "{venue}: {LIVE_CREDENTIALS_ABSENT}");
        }
        AccountTier::Demo | AccountTier::Paper => {}
    }
}

/// **The tier decision of a DEMO-ONLY arm** (one that hardcodes its demo, practice or sandbox
/// endpoint): `true` for `Demo` and `Paper` (the start contract reads `Paper` as `Demo`), `false` for
/// `Live`, which such an arm never selects. Exhaustive on purpose, and the one place the decision is
/// spelled: a bridge's `adapter.rs` asks it rather than naming a tier
/// (`crates/vike-ops/tests/venues/live_mount_line_gate.rs` reads a tier named in a bridge's start as
/// an arm that can bind it).
#[must_use]
pub fn demo_arm(tier: AccountTier) -> bool {
    match tier {
        AccountTier::Demo | AccountTier::Paper => true,
        AccountTier::Live => false,
    }
}

/// **What a DEMO-ONLY arm does with ONE account** — the one answer its `resolve`, `start`,
/// `report_not_running_account` and startup probe all act on, so they cannot disagree. The account
/// has ONE tier (decision 0156 item 6) and its credentials are that tier's: the arm starts only its
/// demo tier, so a complete set on a `live` account is one it will not use. `C` is the venue's demo
/// config.
pub enum ArmingDemoOnly<C> {
    /// The account's tier is below `live` and it holds the whole set: the DEMO config.
    Demo(C),
    /// The account's tier is `live` and it holds the whole set. The arm never selects the live
    /// hosts, so the venue does not run and says so ([`ArmingNotRunningCause::LiveTierNotWired`]).
    LiveTierNotWired,
    /// The account holds no complete set — the ordinary, silent unconfigured state.
    Unconfigured,
}

impl<C> ArmingDemoOnly<C> {
    /// The arm for an account at `tier` whose demo config loaded as `config` (`None`: no complete set,
    /// whatever the tier). A `Paper` tier reads as `Demo` ([`demo_arm`]); only `Live` is refused.
    pub fn of(tier: AccountTier, config: Option<C>) -> Self {
        match config {
            None => ArmingDemoOnly::Unconfigured,
            Some(config) if demo_arm(tier) => ArmingDemoOnly::Demo(config),
            Some(_) => ArmingDemoOnly::LiveTierNotWired,
        }
    }

    /// The arming answer: DEMO held below live as [`ArmingHeldBelowLive::DemoOnlyArm`], a `live` account's
    /// whole set [`ArmingNotRunningCause::LiveTierNotWired`], anything less [`ArmingNotRunningCause::NoCredentials`].
    #[must_use]
    pub fn resolution(&self) -> ArmingResolution {
        match self {
            ArmingDemoOnly::Demo(_) => ArmingResolution::Armed {
                tier: AccountTier::Demo,
                held_below_live: Some(ArmingHeldBelowLive::DemoOnlyArm),
            },
            ArmingDemoOnly::LiveTierNotWired => {
                ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
            }
            ArmingDemoOnly::Unconfigured => {
                ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
            }
        }
    }

    /// **Everything this arm says about the LIVE tier**: [`report_live_tier_not_wired`] exactly when
    /// [`Self::resolution`] answers [`ArmingNotRunningCause::LiveTierNotWired`], in the venue's own word for its
    /// demo tier (`vike_venues::venue_demo_word`); every other arm is silent. A start calls it for
    /// the account it starts and [`VenueAdapter::report_not_running_account`] for a labelled account
    /// that never starts, so the two cannot word one fact two ways.
    pub fn report_live_tier(&self, venue: &'static str, account: &AccountLabel) {
        if matches!(self, ArmingDemoOnly::LiveTierNotWired) {
            // shortcut: a venue the table does not name says "demo"; every demo-only arm is a roster row there.
            let word = vike_venues::venue_demo_word(venue).unwrap_or("demo");
            report_live_tier_not_wired(venue, account, word);
        }
    }
}

impl<C, E: std::fmt::Display> ArmingDemoOnly<Result<C, E>> {
    /// **The demo-only start's prefix**: the demo config to start, or `None` for an account that does
    /// not run (the start returns [`AdapterOutcome::paper`]). It says the live tier
    /// ([`Self::report_live_tier`]) and a refused demo address ([`report_address_refused`]); an
    /// unconfigured account is silent. Each start announces its own mount after it, because
    /// `crates/vike-ops/tests/venues/live_mount_line_gate.rs` reads that line in each bridge.
    pub fn config_for_start(self, venue: &'static str, account: &AccountLabel) -> Option<C> {
        self.report_live_tier(venue, account);
        match self {
            ArmingDemoOnly::Demo(Ok(config)) => Some(config),
            ArmingDemoOnly::Demo(Err(refusal)) => {
                report_address_refused(venue, account, refusal);
                None
            }
            ArmingDemoOnly::LiveTierNotWired | ArmingDemoOnly::Unconfigured => None,
        }
    }
}

/// Everything a start is handed.
pub struct AdapterRequest<'a> {
    pub inputs: AdapterInputs<'a>,
    pub symbol: &'a str,
    pub declared_legs: &'a [String],
    /// Already scoped to this account's route key by `vike-tradehub-core`.
    pub events: &'a EventSender,
    /// The global reconcile gate.
    pub reconcile_enabled: bool,
    /// `Some` only for a venue whose connection row nudges reconcile (`vike_venues::venue_connection`'s
    /// `nudges_reconcile`).
    pub reconcile_trigger: Option<Sender<()>>,
    pub properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    /// The operator's `[risk]` budget; bridges read it for startup leverage only.
    pub risk_profile: Option<&'a ProfileRisk>,
    /// The emulated-market band (`policy.market_slippage`).
    pub market_slippage: Option<f64>,
    /// The halt-admit mode already degraded by `vike_model::effective_halt_admit`.
    pub halt_admit: HaltAdmit,
}

/// A live exec client and what `vike-tradehub-core` folds from it.
pub struct VenueAdapterExecutionRunning {
    pub client: Box<dyn ExecutionClient + Send>,
    /// The tier the credentials actually authenticate — what the identity record is keyed on.
    pub bound_tier: AccountTier,
    /// The engine symbol's grid in BASE units; `None` keeps the permissive default.
    pub grid: Option<SymbolProperties>,
    /// What one unit of the engine symbol's quantity is worth: its notional KIND plus a per-unit
    /// factor (decision 0170). `None` is [`vike_model::ContractValue::default`], `Linear` and 1.0, which
    /// is what every arm but deribit (inverse perpetuals and futures) and IBKR (an exchange multiplier)
    /// reports. It replaced `contract_size`, a bare multiplier that could not express an inverse
    /// contract; the venue's lot (`SymbolProperties::contract_size`) stays on the grid and is no money.
    pub contract_value: Option<vike_model::ContractValue>,
    /// The engine symbol's ruling margin mode, where the arm resolves one.
    pub margin_mode: Option<MarginMode>,
    /// Declared legs' grids the arm already holds, in declaration order.
    pub leg_grids: Vec<(String, SymbolProperties)>,
}

/// A start's exec half.
// One value per start, built once and moved once into the fold: boxing `VenueAdapterExecutionRunning` would buy
// nothing and respell every construction a venue port writes.
#[expect(clippy::large_enum_variant)]
pub enum VenueAdapterExecutionOutcome {
    Running(VenueAdapterExecutionRunning),
    /// The account does NOT RUN, with no paper client in its place (decision 0156; the name is older
    /// than that); the bridge has already logged why. A REFUSAL (a key the venue declined, a tier it
    /// bound wrong, a failed start it cannot tell apart): the host reports `ArmingBlock::AdapterFailed`,
    /// which is not asked again by itself.
    NotRunning,
    /// The account does NOT RUN YET: the bridge's CONNECT to the venue failed before the venue said
    /// anything (its gateway not running, a socket refused, a network failure), so nothing was
    /// refused. TRANSIENT: the host reports `ArmingBlock::VenueUnreachable`, and the tradehub asks
    /// again until it connects. Answered only by a bridge that tells its connect failure apart
    /// ([`AdapterOutcome::unreachable`]).
    Unreachable,
}

/// **A venue-CONFIRMED book**: the venue's own answer about which BOOK this account trades
/// (hyperliquid's `userRole`, IBKR's `managedAccounts`), for the gatewayhub to record against the
/// book the store names. The ONE such type: the start hands it up in [`AdapterOutcome::identity`].
///
/// `book` stays the venue's RAW answer: the recorder builds the one `vike_model::BookId` from it
/// when it compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookConfirmed {
    /// The book the venue named, as it named it.
    pub book: String,
    /// What the venue answered with, for the log line (e.g. "`userRole`").
    pub evidence: &'static str,
    /// The tier the confirmation was made at.
    pub tier: AccountTier,
}

/// What a start produced.
pub struct AdapterOutcome {
    pub exec: VenueAdapterExecutionOutcome,
    /// May be `Some` on `Paper` (polymarket's recon-only shape).
    pub reconcile: Option<Box<dyn ReconcileClient>>,
    /// May be `Some` on `Paper` (hyperliquid answered its identity probe, then failed `meta`).
    pub identity: Option<BookConfirmed>,
}

impl AdapterOutcome {
    /// [`VenueAdapterExecutionOutcome::NotRunning`], with nothing to reconcile and nothing to record.
    #[must_use]
    pub fn paper() -> Self {
        AdapterOutcome {
            exec: VenueAdapterExecutionOutcome::NotRunning,
            reconcile: None,
            identity: None,
        }
    }

    /// [`VenueAdapterExecutionOutcome::Unreachable`], with nothing to reconcile and nothing to record: a start
    /// whose connect to the venue failed (the bridge has logged why).
    #[must_use]
    pub fn unreachable() -> Self {
        AdapterOutcome {
            exec: VenueAdapterExecutionOutcome::Unreachable,
            reconcile: None,
            identity: None,
        }
    }

    /// **A start that FAILED on a venue call of kind `kind`** (`crate::transport::ErrorKind`, the one classification
    /// of a venue failure; owner 2026-11-16: a network failure is transient on every venue). A kind a later try can
    /// pass ([`crate::transport::ErrorKind::is_terminal`] false: a network failure, an ambiguous timeout, a venue
    /// 5xx, a rate limit, a maintenance window) is [`Self::unreachable`], asked again until it starts; every other
    /// (a declined key, a refused request, an unknown failure) is [`Self::paper`], which parks.
    #[must_use]
    pub fn failed(kind: crate::transport::ErrorKind) -> Self {
        if kind.is_terminal() { Self::paper() } else { Self::unreachable() }
    }
}

// ---- the declaration types (decision 0096) ------------------------------------------------------
//
// Where a venue arm gets another symbol's grid is `vike_model::GridSource` and what a drifted clock
// costs is `vike_model::ClockRisk`: both are lease-wire words the hubs name, so they live below
// every daemon (`crates/vike-model/src/risk/grid.rs`, `crates/vike-model/src/venues/declaration.rs`).

/// **How one venue's effective trading book is identified from the credential store, offline.**
///
/// The two variants are the two honest answers, and there is no third: either the account's
/// credentials NAME the book (directly, or through a field from which the name is derivable), or
/// they do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookIdentityRule {
    /// **The account's credentials name the book.** [`Self::Named::name_fields`] are the account's
    /// FIELDS tried in order; the first non-blank value IS the book.
    /// [`Self::Named::evm_key_fields`] is the FALLBACK for a venue whose identifier may be left
    /// implicit in an EVM private key — the address is then derived locally
    /// (`vike_bridge_core::eip712::eth_address_from_private_key`), which is the same derivation
    /// that venue's own signer performs.
    Named {
        /// Fields whose VALUE is the book, tried in order. Must be safe to log — see
        /// `crates/vike-gatewayhub/src/trade/adapters/book_identity.rs`'s module doc.
        name_fields: &'static [&'static str],
        /// Fields holding an EVM private key the address is derived from when no
        /// [`Self::Named::name_fields`] field is present. Empty for every non-EVM venue.
        evm_key_fields: &'static [&'static str],
    },
    /// **Nothing in the store names the book**, so only a live call could answer. No warning is
    /// possible and none is emitted; the reason is carried so the row is a CLASSIFICATION rather
    /// than a gap.
    Undeterminable {
        /// Why — cited to the credential shape it was read from.
        why: &'static str,
    },
}

/// Whether reading this venue's clock needs a credential — the fact that decides whether a box
/// WITHOUT that credential may legitimately skip the read, or has just found a real fault.
///
/// A declared field, never inferred from the human-facing `endpoint` text: the live smoke branches
/// on it (a keyless endpoint that fails from a box with egress is a real fault, a credentialed one
/// may simply have no key here), and sniffing labels like deribit's `"(public, testnet host)"`
/// reports a genuine failure as a skipped credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockAuthentication {
    /// Keyless: any box with egress can read it, so a failure is always a real failure.
    Public,
    /// Needs a credential from the vars map (never a signature — these are all cheap reads).
    Credentialed,
}

/// A resource only ONE account of a venue may hold per process — dukascopy's JForex sidecar. The
/// two functions render the refusals in the venue's own words; the rule itself is `vike-tradehub-core`'s.
///
/// ⚠ `resource` keys the process-level CLAIM, so two venues that declare one resource do exclude
/// each other there. The PROJECTION does not model that: which account holds the resource is
/// decided per VENUE, so each of two venues sharing a resource projects its own holder as armed,
/// and the second to start comes up PAPER through the claim, for a reason the projection never
/// showed. No two declarations share a resource today; a venue that needs to would first have to
/// teach `vike-tradehub-core`'s holder rule to look across venues.
#[derive(Debug, Clone, Copy)]
pub struct ProcessExclusive {
    pub resource: &'static str,
    pub held_by_another: fn(label: &str, holder: &str) -> String,
    pub already_claimed: fn(label: &str) -> String,
}

/// A venue's static facts — no I/O. The daemons carry a credential-free summary of it,
/// `vike_model::VenueAdapterDeclarationView`, whose reconnect-trigger fact is the venue's connection row instead
/// (`vike_venues::venue_connection`'s `nudges_reconcile`).
#[derive(Debug, Clone, Copy)]
pub struct VenueAdapterDeclaration {
    /// `resolve` and `start` read the NAMED account's keys.
    pub addresses_accounts: bool,
    pub process_exclusive: Option<ProcessExclusive>,
    pub grid_source: vike_model::GridSource,
    pub book_identity: BookIdentityRule,
    pub clock: ClockDeclaration,
}

/// A venue's server-clock row; [`VenueAdapter::server_time_ms`] is the read for a `Wired` one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockDeclaration {
    Wired { endpoint: &'static str, auth: ClockAuthentication, risk: vike_model::ClockRisk },
    NotWired { reason: &'static str, unmeasured_risk: Option<&'static str> },
}

/// The startup credential probe a venue offers.
pub enum CredentialProbe {
    /// `vike-tradehub-core` reads the balance, then records which account the key is at `bound_tier`.
    RecordsIdentity { client: Box<dyn ReconcileClient>, bound_tier: AccountTier },
    /// The bridge's own read; nothing is recorded. Its failure's `outcome` is the verdict: `Refused` refuses the
    /// keys, any other word gives none.
    ReadOnly(Arc<dyn Fn() -> Result<(), ReconcileFetchError> + Send + Sync>),
}

impl CredentialProbe {
    /// **A probe that FAILS with `why`, asking nothing**: what an account holding its keys is probed
    /// with when the probe cannot be built, e.g. its address row was refused. The credential leg
    /// then refuses the account in these words (`Refused`). A missing probe (`None`) would say nothing at all.
    #[must_use]
    pub fn failing(why: String) -> Self {
        CredentialProbe::ReadOnly(Arc::new(move || {
            Err(ReconcileFetchError::unclassified(why.clone()))
        }))
    }
}

/// **What one account-mode probe found and did** — the outcome of a bridge's mode heal
/// ([`VenueAdapter::heal_modes`] and the start's own switch): the position mode, or a symbol's or
/// the account's margin mode, whatever the venue calls them. Binance and okx answer in it; the STATE the
/// probe read is `vike_model::PositionMode`. The venue's words, its gate (a mainnet account is never read
/// or written for a mode, owner ruling 2026-10-26) and its log lines stay in its bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeHealOutcome {
    /// Already the mode the adapter orders under (or somebody switched it while the bridge asked):
    /// nothing was written, or the venue said "no need".
    Right,
    /// The account was in the other mode, flat, and the venue took the switch.
    Switched,
    /// The account is in the other mode and was NOT switched (the venue refused, or something blocks the
    /// switch): the sentence says what is wrong and what blocks the fix, in the operator's words. The
    /// account is left as it was.
    Refused(String),
    /// The mode could not be read, or reads as a word the bridge does not know: nothing was written, and
    /// the start goes on as it would have without the probe.
    Unread(String),
    /// Not the demo network: nothing was read and nothing was written.
    LeftAlone,
}

/// THE CONTRACT. Object-safe: a registry holds `&'static dyn VenueAdapter` rows.
pub trait VenueAdapter: Send + Sync {
    /// A `vike_model::VENUES` id.
    fn venue(&self) -> &'static str;
    fn declaration(&self) -> VenueAdapterDeclaration;
    /// PURE: no side effect on the world outside the process — no network, no thread, no process,
    /// no file, no claim — other than `tracing` events. A `resolve` may LOG what it found.
    ///
    /// ⚠ One declared exception: fxcm's answer depends on whether the ForexConnect SDK is staged on
    /// this box, and `vike_fxcm::sdk_available()` finds out by OPENING a shared library. That is
    /// local I/O with no network and no venue contact, and it is the only I/O a `resolve` may do.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> ArmingResolution;
    /// Acts on [`Self::resolve`]'s answer.
    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome;
    /// The clock read for a `ClockDeclaration::Wired` venue, bounded by `timeout`.
    fn server_time_ms(
        &self,
        _inputs: &AdapterInputs<'_>,
        _timeout: Duration,
    ) -> Result<i64, String> {
        Err("no clock leg".into())
    }
    /// The startup credential probe, or `None` when the venue is not probed.
    ///
    /// ⚠ `vike-tradehub-core` calls this on the startup thread, OUTSIDE the bound its probes run under; only
    /// the probe it returns is bounded. So build a client eagerly only when construction is PURE —
    /// a signer and a transport, no network: [`CredentialProbe::RecordsIdentity`]'s shape. A client
    /// whose construction CONNECTS is built inside a [`CredentialProbe::ReadOnly`] closure instead,
    /// as ctrader's is: connecting here would block startup with no bound, and a failed connect
    /// would return no probe at all — silence where the credential leg exists to FAIL.
    fn credential_probe(&self, _inputs: &AdapterInputs<'_>) -> Option<CredentialProbe> {
        None
    }
    /// **Say what this arm knows about an account it will NOT start.** `vike-tradehub-core` calls it, once
    /// per start, for each LABELLED account of the venue that resolved paper for want of a usable
    /// key set — a `Paper(LiveTierNotWired)` or `Paper(NoCredentials)` answer for an account
    /// whose tier is above `paper`, or `Paper(SdkAbsent)` for the arm whose box can lack the SDK
    /// a key set needs — because such an account is never started ([`VenueAdapter::start`] is
    /// reached for the default account and for a labelled account that armed, and for no other).
    /// Without this, the fact the DEFAULT account's `start` logs would be silent for a labelled
    /// account.
    ///
    /// **Logging only.** It has no return value, so it cannot change what starts, and it must not
    /// block, touch the network or read anything beyond `inputs`. The default says nothing; an arm
    /// with a live-tier finding implements this with the SAME function its `start` calls, so the two
    /// surfaces cannot word one fact two ways.
    fn report_not_running_account(&self, _inputs: &AdapterInputs<'_>) {}
    /// **Write the account's leverage for ONE symbol the engine was not mounted on**: the venue's
    /// set-leverage, once, with the figure the gatewayhub's lane already probed
    /// (`crate::leverage_read::settle`; the lane reads the venue back afterwards and never assumes
    /// this landed). The exec thread's start-time write covers the mounted symbol only, and its
    /// foreign-symbol clients post none on purpose, so a symbol admitted later keeps the leverage the
    /// venue holds unless this is called. The bridge's OWN set-leverage verb, idempotent where the
    /// venue is (bybit's "not modified" is swallowed there); no clamp, the probe refused a figure above
    /// the symbol's maximum. `Err` is the venue's words, never a credential, and says HOW it failed
    /// ([`crate::leverage_read::LeverageWriteError`]): a refusal ends the ask, a timeout or a 5xx (the
    /// outcome is unknown, the POST may have landed) is read back by the lane before it is called a failure.
    ///
    /// **A bridge guards this verb ITSELF**: it writes only at the tiers its owner opened for it, whatever
    /// `vike_venues::LEVERAGE_VENUES` says, so opening a tier in that table cannot open a mainnet write
    /// here by accident (bybit and okx: `demo` only; binance: `demo` and the `live` lane twin, decisions
    /// 0182 and 0184). A tier the verb is closed at is `Refused` with no request.
    ///
    /// **DEFAULT `Err`**: a venue that posts no per-account leverage is never asked
    /// (`vike_venues::LEVERAGE_VENUES` names the three that do).
    fn set_symbol_leverage(
        &self,
        _inputs: &AdapterInputs<'_>,
        _symbol: &str,
        _leverage: f64,
    ) -> Result<(), crate::leverage_read::LeverageWriteError> {
        Err(crate::leverage_read::LeverageWriteError::Refused(format!(
            "{}: this adapter writes no per-symbol leverage",
            self.venue()
        )))
    }
    /// **Put the account's modes right for `symbol` BEFORE the gatewayhub's read-only leverage probe
    /// reads them** (decision 0182): the mode switch the bridge's exec thread makes at its own start
    /// (binance's position mode and the symbol's margin type, bybit's position mode of the symbol and the
    /// account's margin mode, okx's net mode), run on a one-shot client of the same tier, host and
    /// credentials. The probe refuses a hedge, isolated or long/short account (the engine models none of
    /// them) and used to run BEFORE the start's switch, so an account that stated a leverage in such a
    /// mode was refused its figure at the very start that then fixed the mode. Each bridge keeps its own
    /// gate: a mainnet account is never read or written for a mode (owner ruling 2026-10-26).
    ///
    /// `Err` is the words of a mode the venue would not let the bridge fix (what blocks it): the lane logs
    /// them and runs the probe anyway, which refuses the figure under its own reason. It is never a reason
    /// to refuse a start.
    ///
    /// **DEFAULT `Ok(())`**: a venue that posts no per-account leverage has no mode to fix here
    /// (`vike_venues::LEVERAGE_VENUES` names the three that do).
    fn heal_modes(&self, _inputs: &AdapterInputs<'_>, _symbol: &str) -> Result<(), String> {
        Ok(())
    }
    /// **Answer an operator's READ of the account** (`TradeUp::VenueRead` at the gatewayhub): the balance
    /// (with the per-exchange allowance where the venue has one), the venue's OWN list of the account's
    /// open orders or the positions it holds, asked of the venue now with THIS account's credentials, on
    /// the host its tier selects.
    /// The twin of [`Self::set_symbol_leverage`] in shape (a one-shot call the bridge builds its client
    /// for, on the daemon's thread) and the opposite in effect: it changes NOTHING at the venue and folds
    /// nothing into any book, which is what separates it from a reconcile pass.
    ///
    /// ⚠ **The answer is ACCOUNT-WIDE or it is not offered.** The reconcile clients most bridges build
    /// read the mounted symbol and the symbols the engine admitted (`vike-gatewayhub-client`'s
    /// `RECONCILE_READS_ADMITTED` lists them: binance, bybit, okx, deribit, oanda, alpaca, ig and the rest); an
    /// open-orders list read through one would call an account with a resting order on another symbol
    /// empty. So a bridge implements this only where the venue's own call lists every open order of the
    /// account, and says in words when it cannot read it. `Err` is the operator's sentence, never a
    /// credential (the venue's own words, or [`vike_model::not_offered_words`]), with the bridge's refusal id
    /// kept where it declares one (`refusal.carried()`; free text `.into()`).
    ///
    /// **DEFAULT `Err(not offered)`**: a venue that implements nothing is never asked to do more.
    fn venue_read(
        &self,
        _inputs: &AdapterInputs<'_>,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, vike_utility::errors::CarriedRefusal> {
        Err(vike_model::not_offered_words(self.venue(), kind).into())
    }
    /// **The venue session the gatewayhub SUPERVISES for this account** (decision 0164 verdict 7(b), amended 2026-11-21):
    /// `Some` hands the daemon's one link watch a [`crate::venue_link::VenueLinkProbe`], and the daemon then takes the
    /// adapter down when the probe says the session is gone and rebuilds it while the account rows say it runs. Asked once
    /// per successful start and once per start refused `venue unreachable` (which `Some` parks until the probe answers),
    /// with the inputs that start read; `route_key` is the account's (`AdapterRequest::events`' route key), for a probe
    /// that reads the cell its exec thread writes. PURE: no venue call, no thread, no claim; `tracing` only.
    ///
    /// **DEFAULT `None`**: the bridge's session heals itself (its own reconnect, a sidecar it restarts), and the daemon
    /// adds no rebuild that would race it. IBKR's socket gateway and FXCM's ForexConnect session answer it.
    fn link_probe(
        &self,
        _inputs: &AdapterInputs<'_>,
        _route_key: &str,
    ) -> Option<Box<dyn crate::venue_link::VenueLinkProbe>> {
        None
    }
}

/// How an account is named in a sentence: `the default account` or `account ALT`.
#[must_use]
pub fn account_phrase(account: &AccountLabel) -> String {
    account
        .text()
        .map_or_else(|| "the default account".to_string(), |label| format!("account {label}"))
}

/// **The account's address row was refused, so it does NOT RUN**: one `error!` line with `venue`,
/// `account` and the refusal's words (a `vike_venues::venue_urls::UrlRefusal` names the row and the
/// command that fixes it). `error!`: the operator wrote a key set this start cannot use.
pub fn report_address_refused(
    venue: &'static str,
    account: &AccountLabel,
    refusal: impl std::fmt::Display,
) {
    tracing::error!(
        venue,
        account = %account,
        "{venue}: REFUSED — {refusal}; the account is NOT RUNNING"
    );
}

/// **The one form of an arming refusal**: how an exec thread that refuses every command answers
/// `cmd`, on `events`. It serves an account whose mount the bridge refused (binance's unreadable
/// COIN-M row, fxcm while no session is open). `false` for `Shutdown`: the loop leaves.
///
/// For a loop that cannot be `run_loop` (fxcm ticks its `confirm` lane between commands). Every answer
/// is [`crate::rest::VenueRestRefusing`]'s, so this and [`crate::exec_actor::run_loop`] over it cannot drift
/// (the test `refuse_command_answers_as_run_loop_over_refusing_rest`): a submit is
/// `[OrderSubmitted, OrderRejected(why)]`; every amend (price, qty or trigger) the non-terminal
/// `OrderModifyRejected(why)`;
/// a cancel (and each id of a batch, answered per id) follows the owner's B-15 as `VenueRestRefusing`
/// does: an order this thread refused gets a terminal `OrderCanceled`, any other (one an earlier
/// process placed and the core restored, which the venue may still hold) the non-terminal
/// `OrderCancelRejected`, so it is never booked cancelled while it may rest.
pub fn refuse_command(events: &EventSender, cmd: ExecutionClientActorCommand, why: &str) -> bool {
    let rest = VenueRestRefusing { why: why.to_string() };
    let cancel = |order_client_id: &String| match rest.cancel_order(order_client_id) {
        // `VenueRestRefusing` answers `Ok` only with `CancelAck::Canceled` (it never reaches a venue).
        Ok(_) => cancel_event(order_client_id, ExecutionClientActorCancelOutcome::Canceled),
        // `run_loop`'s own answer to that `Err` (`crate::exec_actor`'s `answer_cancel`), its catalog id included.
        Err(e) => crate::rest::cancel_rejected(order_client_id, &e),
    };
    let answer = match cmd {
        ExecutionClientActorCommand::Submit(request, _) => rest.submit_order(&request),
        ExecutionClientActorCommand::Cancel { order_client_id, .. } => {
            vec![cancel(&order_client_id)]
        }
        ExecutionClientActorCommand::CancelBatch { order_client_ids, .. } => {
            order_client_ids.iter().map(cancel).collect()
        }
        ExecutionClientActorCommand::Modify {
            order,
            new_qty,
            new_price,
            new_trigger_price: None,
            ..
        } => rest.modify_order(&order, new_qty, new_price),
        ExecutionClientActorCommand::Modify {
            order,
            new_qty,
            new_price,
            new_trigger_price: Some(trigger),
            ..
        } => rest.modify_trigger(&order, new_qty, new_price, trigger),
        // A refusing actor declares neither lane, so these are mis-wirings, answered as `run_loop` answers them.
        ExecutionClientActorCommand::SubmitBatch(requests, _) => {
            crate::exec_actor::submit_batch_undeclared(events, &requests);
            Vec::new()
        }
        ExecutionClientActorCommand::Adopt(order) => {
            crate::exec_actor::adopt_undeclared(&order);
            Vec::new()
        }
        ExecutionClientActorCommand::Shutdown => return false,
    };
    for event in answer {
        // `CoreGone`: the core has exited, so there is nothing left to tell.
        let _ = events.blocking_send(event);
    }
    true
}

/// **An exec thread holds until its MOUNTED symbol's properties load** (owner, 2026-11-13, question 3 of
/// "remove all hardcoded symbols": the fallback grids are gone). `fetch` is asked at once and its answer
/// returned as soon as it has one. While it fails, `fetch` is asked again after each `pause` (the callers
/// pass [`crate::per_symbol::NEGATIVE_TTL`], the pause after which a FOREIGN symbol's failed grid is asked
/// again, so every symbol of an engine retries alike and no timer is new), and the commands that arrive
/// meanwhile are answered by what they need:
///
/// - **A new order or an amend needs the grid**: refused in the one form of an arming refusal
///   ([`refuse_command`]) with [`properties_not_loaded`]'s words, the fetch's own error included.
/// - **A cancel needs none, so it is SENT, and answered exactly as the trading loop answers it**: through
///   `light`, the venue's command-loop client built with no grid (the mounted symbol and every other,
///   with no network until a call), with [`crate::exec_actor::run_loop`]'s own mapping. A cancel the venue
///   calls "already gone" is handed to the `run_loop` this thread runs next, whose gap-sentinel settles it
///   as it settles its own (at a `Shutdown` during the wait it ends `OrderCanceled`, as at that loop's end).
///   A cancel the venue only TOOK has no stream to end it here, so the order is read back by its order client
///   id ([`read_back_cancel`]). Being unable to cancel during a venue outage would be a risk, not a refusal
///   (owner's rule: behaviour is kept except where he ruled). `light` is built at the first failed fetch, never when the first succeeds.
///
/// `None`: `Shutdown`, or every sender gone, before the properties loaded; the caller returns. What queued
/// behind the fetch that succeeded is the caller's to serve, as it always served what queued behind its
/// first fetch: the shared loop refuses a submit too old to send (`crate::exec_actor::too_old_to_send`).
/// The active confirm runs on the actor's own worker with grid-less clients, so status queries never wait
/// here at all.
pub fn await_properties<T, L: VenueRest>(
    venue: &str,
    symbol: &str,
    events: &EventSender,
    rx: &Receiver<ExecutionClientActorCommand>,
    pause: Duration,
    light: impl FnOnce() -> L,
    mut fetch: impl FnMut() -> Result<T, String>,
) -> Option<T> {
    let emit = |ev: Event| {
        // `CoreGone`: the core has exited, so there is nothing left to tell.
        let _ = events.blocking_send(ev);
    };
    let mut light = Some(light);
    let mut grid_less: Option<L> = None;
    let mut gone: Vec<(String, Instant)> = Vec::new();
    let mut failed = 0u32;
    let mut said = String::new();
    loop {
        let why = match fetch() {
            Ok(found) => {
                if failed > 0 {
                    tracing::info!(
                        venue,
                        symbol,
                        failed_attempts = failed,
                        "{venue}: {symbol}'s properties loaded; its orders are served again"
                    );
                }
                crate::exec_actor::hand_over_gone_while_waiting(gone);
                return Some(found);
            }
            Err(why) => why,
        };
        failed += 1;
        let reason = properties_not_loaded(venue, symbol, &why);
        // Loud once, then only when the venue's answer changes: a long outage is one line, not one a pause.
        if failed == 1 {
            tracing::error!(venue, symbol, "{reason}");
        } else if why != said {
            tracing::warn!(venue, symbol, failed_attempts = failed, "{reason}");
        }
        said = why;
        let client = grid_less.get_or_insert_with(|| (light.take().expect("built once"))());
        let until = Instant::now() + pause;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left) {
                Ok(ExecutionClientActorCommand::Cancel { order_client_id, .. }) => {
                    let id = order_client_id.clone();
                    if crate::exec_actor::answer_cancel(&*client, id, &mut gone, &emit) {
                        read_back_cancel(&*client, &order_client_id, &emit);
                    }
                }
                Ok(ExecutionClientActorCommand::CancelBatch { order_client_ids, .. }) => {
                    crate::exec_actor::answer_cancel_batch(&*client, &order_client_ids, &emit);
                }
                Ok(cmd) => {
                    if !refuse_command(events, cmd, &reason) {
                        break_off(gone, &emit);
                        return None;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    break_off(gone, &emit);
                    return None;
                }
            }
        }
    }
}

/// A cancel the venue only TOOK ([`crate::rest::OrderCancelAck::Requested`]) during the wait has nothing else to
/// end it: no private stream is open yet, and the venue's row of it is older than the
/// [`crate::exec_actor::run_loop`] that runs next, whose replay drops it (`crate::exec_actor::is_pre_spawn`). So the
/// order is read back at once by its order client id ([`crate::rest::VenueRest::confirm_order`], decision 0303) and
/// the cancel the read shows is forwarded ([`crate::rest::cancel_read_back`]: nothing else the read says is, an
/// accept or an "absent" reject). A read that shows no cancel (the order still rests, filled, or the read failed)
/// says nothing: the order stays live until the venue's word or reconcile ends it, and the warning names it.
fn read_back_cancel<L: VenueRest>(client: &L, order_client_id: &str, emit: &impl Fn(Event)) {
    let read = client.confirm_order(order_client_id, vike_model::now_ms());
    let canceled = crate::rest::cancel_read_back(order_client_id, read);
    if canceled.is_empty() {
        tracing::warn!(
            target: "vike_bridge_core",
            order_client_id,
            "cancel taken while the grid is unknown, and the read by order client id shows no cancel yet: \
             the order stays live until the venue or reconcile ends it"
        );
    }
    canceled.into_iter().for_each(emit);
}

/// The wait ends with no loop after it: the cancels the venue called "already gone" end as
/// [`crate::exec_actor::run_loop`]'s end leaves its own, `OrderCanceled`, so no order waits for ever.
fn break_off(gone: Vec<(String, Instant)>, emit: &impl Fn(Event)) {
    for (order_client_id, _) in gone {
        emit(cancel_event(&order_client_id, ExecutionClientActorCancelOutcome::Canceled));
    }
}

/// The words an order on a symbol whose properties are unknown is refused with: what is missing, the
/// fetch's own error `why`, and that nothing was sent ([`await_properties`]; polymarket's per-token tick).
pub fn properties_not_loaded(venue: &str, symbol: &str, why: &str) -> String {
    format!(
        "{venue}: {symbol}: properties not loaded ({why}); the order was NOT sent, and the properties \
         are fetched again"
    )
}

/// Build a reconcile client only when reconciliation is on. `false` never CALLS `build`: the
/// property is the LAZINESS, which no assertion on the returned `Option` can see. A bridge that
/// does not call this builds its recon client whenever it starts live; read the venue's `mount.rs`
/// for whether it reconciles.
pub fn reconcile_if_enabled<F>(recon_enabled: bool, build: F) -> Option<Box<dyn ReconcileClient>>
where
    F: FnOnce() -> Option<Box<dyn ReconcileClient>>,
{
    if recon_enabled { build() } else { None }
}

/// **The loud half of [`ArmingNotRunningCause::LiveTierNotWired`]**: say, at `error!`, that a `live` account
/// holds a key set this arm will not use and the account therefore does not run.
///
/// A demo-only arm calls it exactly when the account's tier is `live` and the account holds its
/// key set — the same two facts its `resolve` answers [`ArmingNotRunningCause::LiveTierNotWired`] from — and
/// then returns [`AdapterOutcome::paper`], so the arming screen and the log say the same thing. (fxcm also answers the SDK question; its `resolve` and
/// `start` ask their questions in ONE order so the two cannot disagree —
/// `crates/bridges/fxcm/src/adapter.rs`'s `resolution_for`.) `error!`, not `warn!`: an ABSENT
/// credential is silent and a PRESENT-and-unusable one is an error, because a misconfiguration
/// wearing the "not configured" answer looks exactly like a correct fresh install.
///
/// The event carries `venue`, `account` and **`found_tier`** (`live`, the tier FOUND and refused),
/// deliberately NOT `tier`, which on a live-mount line means the tier STARTED: the convention is
/// `crates/vike-ops/tests/venues/live_mount_line_gate.rs`'s. `demo_word` is what this arm calls the
/// tier it DOES start (`demo`, `sandbox`, `testnet`…), spoken in the sentence.
///
/// One line per `(venue, account)` per start: from `start` for the DEFAULT account, and for a
/// LABELLED account (never started unless it armed: `vike_tradehub_core::accounts_to_run`) through
/// [`VenueAdapter::report_not_running_account`], which `vike_tradehub_core::make_engine_accounts` calls for
/// exactly the labelled accounts it does not start.
///
/// ⚠ It logs NO key name and NO value: `vike-cli secrets ls` finds the keys, and this sentence is
/// pasted into chats.
pub fn report_live_tier_not_wired(
    venue: &'static str,
    account: &AccountLabel,
    demo_word: &'static str,
) {
    let whose = account_phrase(account);
    tracing::error!(
        venue,
        account = %account,
        found_tier = AccountTier::Live.as_str(),
        "{venue}: {whose} is a LIVE account (tier `live`) holding its credentials, but this arm \
         mounts only its {demo_word} tier and never selects a live one, so {venue} is NOT RUNNING. \
         Nothing was signed \
         and no order can reach the live account from this build. To trade the {demo_word} \
         account, give it its own account row at tier `demo` with its {demo_word} keys; \
         `vike-cli account ls` names what the store holds"
    );
}

/// `GET {base}{path}` on an agent bounded by `timeout`, with the venue's own error text — the
/// shared helper for a bridge's clock read.
pub fn bounded_public_get(
    venue: &'static str,
    base: &str,
    path: &str,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    crate::transport::UreqTransport::with_agent(
        venue,
        vike_transport::http::blocking_agent_with_timeout(timeout),
    )
    .public(base, path, &[])
    .map_err(|e| e.to_string())
}

/// The message for a clock body that answered without the stamp, shared by every bridge's
/// server-time parser.
#[must_use]
pub fn missing_time_field(field: &str) -> String {
    format!("{field} missing from the server-time response")
}

#[path = "venue_adapter_tests.rs"]
#[cfg(test)]
mod venue_adapter_tests;
