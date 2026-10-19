//! **THE VENUE PORT — the one seam between the generic fold and every venue connection**
//! (wave 2, W9a; `docs/decisions/0165-*`).
//!
//! The fold in this crate (`make_engine*`, `assemble_runtime`, the startup preflight) decides WHICH
//! account runs at WHICH tier and folds what a venue hands back into an engine. Everything that
//! needs a bridge's `VenueAdapter` trait object or a credential VALUE sits behind [`VenueBackend`]:
//! the keep half of this crate names no bridge type and holds no registry, and
//! `tests/keep_half_names_no_bridge.rs` holds that line.
//!
//! # The implementations
//!
//! The venue code itself is the gatewayhub's `BridgeTrade`
//! (`crates/vike-gatewayhub/src/trade/adapters/bridge_trade.rs`, `LocalBackend` moved whole by W9b,
//! the real `TradeLane`); the tradehub reaches it over the trade wire through a remote implementation of
//! this port (wave 2, W11). In this crate:
//!
//! * [`PaperBackend`] — no venue at all: the rows a paper-only or switched-off box shows
//!   (`flags.tradehub_live` off), nothing dialled, every `demo`/`live` account NOT RUNNING with
//!   [`vike_config::ArmingBlock::LiveSwitchOff`].
//! * [`ScriptedBackend`] — a double that answers what a test scripts and records what it was asked.
//!
//! # What crosses, and what never does
//!
//! Words ([`vike_config::ArmingBlock`]), grids, static facts ([`VenueFacts`]) and opaque handles
//! (the bound exec client, the reconcile client). Never a `Resolution`, an `AdapterInputs` or an
//! `AccountCredentials`: the table is computed where the credentials are, and the fold reads it.
//!
//! # The arming table replaces fifteen calls to `resolve`
//!
//! `resolve` is pure and cheap, and fifteen functions of the old `arming.rs` called it. The fold now
//! fetches ONE [`ArmingTable`] per assembly ([`VenueBackend::arming_table`]) and every reader —
//! `vike-backend venues`, the live-account lock set, the preflight's venue lists, the account
//! fan-out — reads that one snapshot, so "one snapshot, two consumers" holds by construction. The
//! preflight stop (a venue the startup preflight hard-FAILED) is applied over the table by the
//! fold, not by the backend: see [`crate::venue_account_arming`].
//!
//! # ⚠ Where this port departs from the plan's first sketch (each is a DOUBT in the W9a notes)
//!
//! * The preflight is the plan's batch verb split at the unit `PreflightProbes` already has:
//!   [`VenueBackend::preflight_plan`] (the venue lists and clock policies) plus one verb per READ
//!   ([`VenueBackend::server_time_ms`], [`VenueBackend::authed_read`]) and the network reading.
//!   `run_preflight` re-reads a clock up to `DEFAULT_CLOCK_SAMPLES` times and interleaves the two
//!   legs' budgets between reads, so a batch verb would change the schedule.
//! * [`VenueBackend::record_identity`] carries the bound tier and the bind's reconcile client, and
//!   the venue-CONFIRMED identity rides on [`AdapterBind::confirmed_book`], recorded in the same
//!   call after the fee schedule: moving it into the start would move a log line and a parked
//!   record before `resolve_fee_schedule`.
//! * [`VenueBackend::report_not_running`] is a fifth verb: the bridge speaks for the accounts the
//!   fan-out never starts (`VenueAdapter::report_not_running_account`).
//! * [`AdapterStartQuery::risk_profile`] is the narrowed `ProfileRisk`, not a mirror DTO: the wire
//!   mirror (`AdapterRisk`) belongs with the wire, W11.

use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::Sender;

use vike_config::{ArmingBlock, VenueArming};
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{AccountTier, HaltAdmit, MarginMode, ProfileRisk, SymbolProperties};

use crate::WiredMarket;
use crate::preflight::{ClockPolicy, CredentialGap, ServerTimeGap};
use crate::symbol_grid::GridSource;

mod paper;
#[cfg(any(test, feature = "test-support"))]
mod scripted;

pub use paper::PaperBackend;
#[cfg(any(test, feature = "test-support"))]
pub use scripted::{ScriptedBackend, ScriptedCall};

/// The port. Object-safe on purpose: the fold holds a `&dyn VenueBackend` (an `Arc` in
/// [`crate::RuntimeConfig`], because the preflight's probe closures are `'static`).
pub trait VenueBackend: Send + Sync {
    /// **The arming rows of every roster venue's accounts, and the static facts the fold reads**,
    /// computed once per assembly. The rows are the backend's own verdict WITHOUT the startup
    /// preflight's stop (that is the fold's overlay) and WITH the one-sidecar override.
    fn arming_table(&self) -> Result<ArmingTable, BackendError>;

    /// **The route keys of the accounts that run on a key set the store no longer holds** (the
    /// session started with one key; the database now says another): `Some(set)` when the backend that
    /// holds the keys answered, `None` when it cannot say (an in-process backend, a gatewayhub that did not
    /// answer, one that predates the question). Words only: no fingerprint and no value crosses.
    /// **DEFAULT `None`**: only the backend that owns the venue sessions can know.
    fn key_changed(&self) -> Option<std::collections::BTreeSet<String>> {
        None
    }

    /// The preflight's venue lists: the clock policy of each venue in `q.clock_venues`, how many of
    /// them have a WIRED clock endpoint (they buy time budget), and the venues a credential read
    /// was built for. Builds the credential probes [`VenueBackend::authed_read`] then answers from.
    fn preflight_plan(&self, q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError>;

    /// ONE venue clock read as an ABSOLUTE epoch-ms stamp, bounded (a wedged name resolution is
    /// abandoned at `CLOCK_PROBE_TIMEOUT`); the gaps are the preflight's four outcomes.
    fn server_time_ms(&self, venue: &str, tier: AccountTier) -> Result<i64, ServerTimeGap>;

    /// ONE bounded credential read (a cheap authenticated balance fetch, retried once after an
    /// ANSWERED error, never after a silence) for a venue [`VenueBackend::preflight_plan`] listed.
    fn authed_read(&self, venue: &str) -> Result<(), CredentialGap>;

    /// The network liveness reading the preflight's network leg shows: a probe is spawned, waited on
    /// for a bounded first round, read and dropped. The fold asks only when some venue would go
    /// live; a backend with nothing to dial answers [`NetReading::Unknown`].
    fn net_reading(&self) -> NetReading;

    /// Let each bridge speak for the accounts of `venue` the fan-out will never start (a stored
    /// key set that is unused or incomplete). Called once per start over rows already computed;
    /// it cannot change which account starts.
    fn report_not_running(&self, venue: &str, rows: &[VenueArming]);

    /// **Start one account's venue client** at `q.tier`: the resolve-half tier interlock, the
    /// no-downgrade rule, the one-sidecar refusal, the process-exclusive claim, the bridge's own
    /// `start`, the start-half interlock and the order hold — everything between the fold's
    /// pre-connect budget refusal and the engine. `Err` is the account NOT RUNNING, with the
    /// reason as a word.
    fn start_adapter(
        &self,
        q: AdapterStartQuery<'_>,
        events: &EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<AdapterBind, BackendError>;

    /// Record which venue ACCOUNT the bound session authenticated as (a signed
    /// `fetch_account_identity` read on `recon`, a failure only a warning) and, when the venue
    /// named the book itself, fold that confirmation. Called AFTER the fee schedule is resolved:
    /// on binance spot the account id and the commission rates ride ONE body, and asking first costs
    /// a second signed read.
    fn record_identity(
        &self,
        q: &IdentityRecord<'_>,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), BackendError>;
}

/// Why a backend could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// The account does NOT RUN, for this reason (decision 0156). Never a paper stand-in.
    NotRunning(ArmingBlock),
    /// The backend could not be reached (a gatewayhub that is down). A start that meets it leaves
    /// the account NOT RUNNING with [`ArmingBlock::GatewayUnavailable`] (decision 0164, D7), never
    /// on a paper stand-in; a backend that meets it at the arming table answers every `demo`/`live`
    /// row with that block itself (`vike-tradehub`'s `RemoteBackend`).
    Unavailable(String),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendError::NotRunning(block) => {
                write!(f, "the account does not run: {}", block.as_str())
            }
            BackendError::Unavailable(why) => write!(f, "the venue backend is unavailable: {why}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// **Static facts about one venue's adapter**, read once with the arming table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VenueFacts {
    /// `false` = the venue's bridge is not compiled into this backend (`FeatureAbsent`): a `paper`
    /// account of it runs the simulator, a `demo`/`live` one does not run.
    pub adapter_present: bool,
    /// Whether the start can address a NAMED (labelled) account. Unread when `adapter_present` is
    /// `false`.
    pub addresses_accounts: bool,
    /// Whether the bridge takes the reconnect-resync trigger.
    pub takes_recon_trigger: bool,
    /// Where the venue's per-symbol grid could come from.
    pub grid_source: GridSource,
    /// The process-exclusive resource the venue's start claims, if it declares one.
    pub process_exclusive: Option<String>,
    /// How the venue declares its server clock: `"wired"`, `"not-wired"` or `"absent"`.
    pub clock_decl_word: &'static str,
}

impl VenueFacts {
    /// The facts of a venue whose bridge this backend does not carry.
    #[must_use]
    pub fn absent() -> Self {
        VenueFacts {
            adapter_present: false,
            addresses_accounts: false,
            takes_recon_trigger: false,
            grid_source: GridSource::NoGrid,
            process_exclusive: None,
            clock_decl_word: "absent",
        }
    }
}

/// One account's row of the [`ArmingTable`]: the arming projection plus the venue BOOK the account
/// trades, computed where the credentials are (`None` = not determinable, which warns nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmingRow {
    /// `(venue, label, tier, effective, block, account_ids)` — what `vike-backend venues` renders.
    pub arming: VenueArming,
    /// The offline-resolved book, when the account runs and the store or the credentials name it.
    pub book: Option<String>,
}

/// **The arming snapshot of one assembly**: the static facts of each venue and one
/// [`ArmingRow`] per ACCOUNT, in roster order (a venue's accounts default-first). Read through
/// [`crate::venue_account_arming`], [`crate::venue_arming`] and friends, which lay the startup
/// preflight's stop over it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArmingTable {
    facts: BTreeMap<String, VenueFacts>,
    rows: Vec<ArmingRow>,
}

impl ArmingTable {
    /// A table over `facts` (keyed by venue id) and `rows` (roster order).
    #[must_use]
    pub fn new(facts: BTreeMap<String, VenueFacts>, rows: Vec<ArmingRow>) -> Self {
        ArmingTable { facts, rows }
    }

    /// The facts of `venue`; `None` for an id the backend carries no row for at all (a test's
    /// `"sim"`).
    #[must_use]
    pub fn facts_of(&self, venue: &str) -> Option<&VenueFacts> {
        self.facts.get(venue)
    }

    /// Every account row, roster order.
    #[must_use]
    pub fn rows(&self) -> &[ArmingRow] {
        &self.rows
    }

    /// The row of one account.
    #[must_use]
    pub fn row_of_account(&self, venue: &str, label: &AccountLabel) -> Option<&ArmingRow> {
        self.rows.iter().find(|r| r.arming.venue == venue && r.arming.label == *label)
    }

    /// **The markets a build MOUNTS**: [`crate::wired_markets`]' engine rows, kept where this table
    /// says the venue's adapter is present — a `FeatureAbsent` venue (a bridge the backend was
    /// built without) or an id the table does not carry drops out. The live assembly mounts these,
    /// [`crate::armed_live_venues`] claims locks over them, and the xEMM live mount validates
    /// against them.
    #[must_use]
    pub fn wired_markets(&self) -> Vec<WiredMarket> {
        crate::wired_markets()
            .into_iter()
            .filter(|m| self.facts.get(m.venue).is_some_and(|f| f.adapter_present))
            .collect()
    }
}

/// What the preflight's venue lists are asked for.
#[derive(Debug, Clone, Copy)]
pub struct PreflightQuery<'a> {
    /// The venues the clock leg runs for (every venue whose default account would go live at its
    /// tier), roster order.
    pub clock_venues: &'a [String],
}

/// What [`VenueBackend::preflight_plan`] answers.
#[derive(Debug, Clone, Default)]
pub struct PreflightPlan {
    /// The thresholds and remedy for a MEASURED skew, of the wired venues among the clock venues.
    pub clock_policies: HashMap<String, ClockPolicy>,
    /// How many clock venues have a WIRED endpoint: only wired reads buy time budget.
    pub wired_clock_reads: usize,
    /// The venues a credential read was built for, sorted: the credential leg's list IS this set.
    pub credential_venues: Vec<String>,
}

/// The network leg's reading — the bridge-core probe handle's state as three variants and their
/// numbers, so the preflight's report names no bridge type. The report's wording is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetReading {
    /// No round has completed: the handle's `internet_up` is still the optimistic initial value,
    /// which is not a measurement.
    Unknown,
    /// The last round observed the internet up, after `rounds` completed rounds.
    Alive {
        /// Completed probe rounds.
        rounds: u64,
    },
    /// The last round observed the internet down.
    Dead {
        /// Epoch ms the flag last went down.
        since_ms: i64,
    },
}

/// What a start drops from the credentials before the bridge sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Withhold {
    /// The venue's startup preflight hard-FAILED this session: its credentials are withheld and its
    /// `demo`/`live` accounts stop with [`ArmingBlock::PreflightFailed`].
    PreflightFailed,
    /// The venue is declared `data_only`: its exec credentials are withheld while a credentialed
    /// market feed authenticates from the same store.
    DataOnly,
}

/// Everything the backend needs to start ONE account's venue client. Borrowed: in process nothing
/// is copied, and the wire serialises exactly these fields.
#[derive(Clone)]
pub struct AdapterStartQuery<'a> {
    /// A roster venue id.
    pub venue: &'a str,
    /// Which account of the venue.
    pub account: &'a AccountLabel,
    /// The account's tier (never `paper` here).
    pub tier: AccountTier,
    /// The symbol the engine starts on.
    pub symbol: &'a str,
    /// The EXTRA symbols the strategy mounts declared (per-leg grids).
    pub declared_legs: &'a [String],
    /// The global reconcile verdict (NOT inferred from the trigger: only some venues take one).
    pub recon_enabled: bool,
    /// The run profile's `[risk]` budget, already narrowed by the policy's leverage ceiling.
    pub risk_profile: Option<&'a ProfileRisk>,
    /// The operator's market-slippage request for a venue with no native market order.
    pub market_slippage: Option<f64>,
    /// The halt-admit mode in force.
    pub halt_admit: HaltAdmit,
    /// Credentials to drop before the bridge sees them.
    pub withhold: Option<Withhold>,
    /// **The whole-number leverage this account asks a venue that POSTS one to hold** (decision
    /// 0182): `Some` only for an account whose row states a leverage on a `Posted` venue at a tier
    /// `vike_secrets::LEVERAGE_VENUES` opens (`crate::account_start_leverage`); `None` is every other
    /// start, which then is byte-identical to the start before the field existed.
    pub account_leverage: Option<f64>,
    /// The account's routing key (`venue` for the default account, `venue#LABEL` otherwise).
    pub route_key: &'a str,
    /// The opt-in symbol-properties recorder. IN-PROCESS ONLY: production passes `None` since
    /// #2093 and the wire never carries it.
    pub properties_rec: Option<std::sync::Arc<vike_data::PropertiesRecorder>>,
}

/// A venue-CONFIRMED book (hyperliquid's `userRole`): what the venue itself named about an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmedBook {
    /// The book the venue named.
    pub book: String,
    /// The phrase naming WHAT the venue answered with, for the disagreement log.
    pub evidence: &'static str,
    /// The tier the confirmation was made at.
    pub tier: AccountTier,
}

/// What a successful start hands back: the bound exec client and the facts the fold builds the
/// engine from. `client` and `recon` are opaque handles — in process the bridge's own objects.
pub struct AdapterBind {
    /// The bound exec client.
    pub client: Box<dyn ExecutionClient + Send>,
    /// The account's reconcile client, when the venue built one.
    pub recon: Option<Box<dyn ReconClient>>,
    /// The tier the client BOUND — the start's tier interlock has already held it equal to the
    /// account's.
    pub bound_tier: AccountTier,
    /// The venue-fetched grid of the engine symbol.
    pub grid: Option<SymbolProperties>,
    /// What one unit of the engine symbol's quantity is worth (kind plus per-unit factor, decision
    /// 0170), when the venue reported one; `None` is `Linear`, 1.0.
    pub contract_value: Option<vike_model::ContractValue>,
    /// The ruling margin mode, when the venue reported one.
    pub margin_mode: Option<MarginMode>,
    /// The grids of the declared legs the venue could answer.
    pub leg_grids: Vec<(String, SymbolProperties)>,
    /// The book the venue confirmed itself, to record after the fee schedule.
    pub confirmed_book: Option<ConfirmedBook>,
    /// What the VENUE confirmed of the account's stated leverage (decision 0181): `Ok(x)` is the most
    /// it lets the account margin the engine symbol at; `Err` says why it could not confirm; `None`
    /// is "nothing was asked, or the backend has no venue to ask" (paper, a scripted start, an older
    /// gatewayhub). Read by the shared tail alone, and only when the account row states a leverage.
    pub leverage_cap: Option<Result<f64, String>>,
    /// The currency the account's money is in, as the VENUE states it (decision 0183, item 1: OANDA's
    /// account summary). `None` = the venue states none, the backend could not read it, or there is no
    /// venue to ask (paper, a scripted start, an older gatewayhub). Read by the shared tail alone, which
    /// anchors the engine's ledger to it only for an account that converts a quote currency
    /// ([`crate::quote_conversion_tier_refusal`], and the root's venue list in
    /// `AdapterPolicy::quote_converting_venues`): every other engine keeps the label it has always had.
    pub account_ccy: Option<String>,
}

impl std::fmt::Debug for AdapterBind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterBind")
            .field("bound_tier", &self.bound_tier)
            .field("has_recon", &self.recon.is_some())
            .field("contract_value", &self.contract_value)
            .field("margin_mode", &self.margin_mode)
            .field("legs", &self.leg_grids.len())
            .finish_non_exhaustive()
    }
}

/// [`VenueBackend::record_identity`]'s question.
#[derive(Debug, Clone)]
pub struct IdentityRecord<'a> {
    /// A roster venue id.
    pub venue: &'a str,
    /// Which account.
    pub account: &'a AccountLabel,
    /// The tier the mount BOUND, never the account's (the two agree on every start that arms).
    pub bound_tier: AccountTier,
    /// The book the venue confirmed itself, when it did.
    pub confirmed: Option<&'a ConfirmedBook>,
}

#[path = "backend_tests.rs"]
#[cfg(test)]
mod backend_tests;
