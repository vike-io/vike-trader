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
//!    named values in [`AdapterInputs::secrets`], venue settings in [`AdapterInputs::settings`], the
//!    state and bin directories and the HALT sentinel's path in [`AdapterInputs::process`].
//! 2. **The account's tier crosses as [`AdapterInputs::tier`], a `vike_model::AccountTier`** — the
//!    one tier vocabulary, an account FACT, never a policy type: this crate does not depend on
//!    `vike-config`, so `AdapterPolicy` cannot be named here
//!    (`docs/decisions/0129-one-tier-vocabulary-in-vike-model.md`). The tiers a bridge answers in
//!    ([`Resolution::Armed`], [`LiveExec::bound_tier`], [`IdentityReport::tier`]) are the same
//!    type. A `live`-tier account whose arm binds a lower tier is `vike-tradehub-core`'s to hold at paper
//!    (a live account never trades demo); a bridge reports what it would bind and decides nothing
//!    about it.
//! 3. **Only `vike-tradehub-core` builds a paper engine.** A bridge that declines returns
//!    [`ExecOutcome::Paper`]; `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs` holds it.
//! 4. **The fallback grid is the bridge's own business**, internal to its `start`.
//! 5. **This module is a SHARED HOME.** It is extended by ONE coordinated change informed by every
//!    consumer. A venue port that finds a missing knob stops and reports; it never widens a type
//!    here in passing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_model::AccountTier;
use vike_model::ProfileRisk;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{HaltAdmit, MarginMode, SymbolProperties};
use vike_secrets::account_credentials::AccountCredentials;
use vike_secrets::venue_setting::VenueSettings;

use crate::transport::RestTransport;
use vike_secrets::account_directory::AccountDirectory;

/// Process facts a start may need, resolved ONCE by `vike-tradehub-core`, so a bridge reads no process
/// global. `Default` is "this process declared no project" — a test, a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessFacts {
    /// `<project>/settings/state`, as the boot declared it.
    pub state_dir: Option<PathBuf>,
    /// `<project>/bin`, derived from `state_dir`.
    pub bin_dir: Option<PathBuf>,
    /// **The operator HALT sentinel** — the one file the process's kill switch is, as
    /// `vike-tradehub-core` resolved it (`vike_boot::halt::halt_path_from_env`; `<state_dir>/HALT` whenever the
    /// boot declared a project). Every live exec client the start builds is handed THIS path with
    /// its `with_halt_path` builder, and checks `vike_exec::halt::sentinel_engaged` against it at its
    /// submit seam; a bridge resolves nothing itself (decision 0099).
    ///
    /// ⚠ The `Default` is EMPTY, and an empty path watches nothing — a test or a tool that built
    /// facts by hand. `vike-tradehub-core` always fills it, so a real start never sees the empty one, and
    /// a client handed it logs `HALT KILL SWITCH IS NOT WIRED` rather than looking armed.
    pub halt_path: PathBuf,
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
    /// **The deployment's NAMED values** — the composition root's name-keyed map: the resolved
    /// flags it folds in (`VIKE_ALLOW_WITHDRAW_KEYS`, the polymarket gates), the attribution codes,
    /// and the store's tier-less deployment rows (a builder-fee knob, the JForex tool paths). No
    /// account's credential is read out of it: those are [`Self::credentials`].
    pub secrets: &'a HashMap<String, String>,
    /// This venue's `venue_setting` rows.
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
    pub process: &'a ProcessFacts,
}

/// Why a venue stays paper — one variant per `vike_config::ArmingBlock` a BRIDGE can produce. The
/// others (`PaperTier`, `AccountInactive`, `NoAccountRow`, `TierConflict`, `FeatureAbsent`,
/// `NoAccountSupport`, `SidecarHeldElsewhere`) are `vike-tradehub-core`'s own and are never a bridge's
/// answer; `OrdersHeld` is not a paper cause at all (the row starts at its tier), and a bridge
/// says it with [`HeldBelowLive::OrdersHeld`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperCause {
    /// The account holds no usable key set — the original live gate.
    NoCredentials,
    /// The account's tier is `live`, the arm could reach it, and the account holds no usable key
    /// set (decision 0095: for the venues whose network is the tier, `live` means mainnet). Maps to
    /// `vike_config::ArmingBlock::LiveCredentialsAbsent`.
    LiveCredentialsAbsent,
    /// **The account's tier is `live`, it holds a key set, and this arm does not trade live** — it
    /// starts only its demo, practice or sandbox tier (it has no live arm), or, for an arm that
    /// HAS one (ibkr), its live tier refused this account under one of its own rules, which its
    /// `start` log names. Maps to `vike_config::ArmingBlock::LiveTierNotWired`.
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
pub enum HeldBelowLive {
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
    /// The bridge's own client refuses, AND `vike-tradehub-core` refuses in front of it: it maps this to
    /// `vike_config::ArmingBlock::OrdersHeld` and wraps the bound client so no order reaches it
    /// (`crates/vike-gatewayhub/src/trade/adapters/orders_held.rs`'s `OrdersHeldClient`). A cancel still passes, because
    /// a hold must never trap an operator in an order.
    OrdersHeld,
}

/// The pure arming answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Stays paper, and why.
    Paper(PaperCause),
    /// Arms at `tier`; `held_below_live` says why the arming does not trade live: a demo arming's
    /// reason, or [`HeldBelowLive::OrdersHeld`] for an arm that binds `tier` and sends no order.
    Armed { tier: AccountTier, held_below_live: Option<HeldBelowLive> },
}

/// Everything a start is handed.
pub struct AdapterRequest<'a> {
    pub inputs: AdapterInputs<'a>,
    pub symbol: &'a str,
    pub declared_legs: &'a [String],
    /// Already scoped to this account's route key by `vike-tradehub-core`.
    pub events: &'a EventSender,
    /// The global reconcile gate.
    pub recon_enabled: bool,
    /// `Some` only for a venue whose declaration sets `takes_recon_trigger`.
    pub recon_trigger: Option<Sender<()>>,
    pub properties_rec: Option<Arc<vike_data::PropertiesRecorder>>,
    /// The operator's `[risk]` budget; bridges read it for startup leverage only.
    pub risk_profile: Option<&'a ProfileRisk>,
    /// The emulated-market band (`policy.market_slippage`).
    pub market_slippage: Option<f64>,
    /// The halt-admit mode already degraded by `vike_model::effective_halt_admit`.
    pub halt_admit: HaltAdmit,
}

/// A live exec client and what `vike-tradehub-core` folds from it.
pub struct LiveExec {
    pub client: Box<dyn ExecutionClient + Send>,
    /// The tier the credentials actually authenticate — what the identity record is keyed on.
    pub bound_tier: AccountTier,
    /// The engine symbol's grid in BASE units; `None` keeps the permissive default.
    pub grid: Option<SymbolProperties>,
    /// The engine symbol's contract size, where the arm sets one.
    pub contract_size: Option<f64>,
    /// The engine symbol's ruling margin mode, where the arm resolves one.
    pub margin_mode: Option<MarginMode>,
    /// Declared legs' grids the arm already holds, in declaration order.
    pub leg_grids: Vec<(String, SymbolProperties)>,
}

/// A start's exec half.
// One value per start, built once and moved once into the fold: boxing `LiveExec` would buy
// nothing and respell every construction a venue port writes.
#[expect(clippy::large_enum_variant)]
pub enum ExecOutcome {
    Live(LiveExec),
    /// `vike-tradehub-core` builds the paper client; the bridge has already logged why.
    Paper,
}

/// The venue's own answer about which BOOK this account trades, for `vike-tradehub-core` to record.
pub struct IdentityReport {
    pub book: String,
    /// What the venue answered with, for the log line (e.g. "`userRole`").
    pub evidence: &'static str,
    pub tier: AccountTier,
}

/// What a start produced.
pub struct AdapterOutcome {
    pub exec: ExecOutcome,
    /// May be `Some` on `Paper` (polymarket's recon-only shape).
    pub recon: Option<Box<dyn ReconClient>>,
    /// May be `Some` on `Paper` (hyperliquid answered its identity probe, then failed `meta`).
    pub identity: Option<IdentityReport>,
}

impl AdapterOutcome {
    /// Paper, with nothing to reconcile and nothing to record.
    #[must_use]
    pub fn paper() -> Self {
        AdapterOutcome { exec: ExecOutcome::Paper, recon: None, identity: None }
    }
}

// ---- the declaration types (decision 0096) ------------------------------------------------------

/// Where a venue arm would have to get the instrument grid for a symbol OTHER than the started
/// one — the declaration behind which arms of `vike_tradehub_core::make_engine_with_legs` populate
/// `vike_model::RiskLimits::grid_by_symbol` (see `vike_tradehub_core::symbol_grid`'s module doc, its ⚠
/// section).
///
/// Declaring TODAY'S REALITY, per the capability-map playbook: the rows are read off the arms, no
/// arm's behaviour is inferred from a row, and
/// `crates/vike-gatewayhub/tests/trade_roster/mount_roster/symbol_grid.rs`'s
/// `every_roster_venue_declares_a_grid_source` iterates `vike_model::VENUES`, so a new bridge crate
/// cannot join the roster without classifying itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclaredGridSource {
    /// The arm ALREADY holds a resolved instrument table covering every symbol of the venue, so a
    /// declared leg's grid costs no network at all. These are the arms wired today.
    InHand,
    /// The arm's only per-symbol source is a blocking, symbol-SCOPED network round trip — the very
    /// call it already makes once for the engine symbol. A declared leg would cost one more, at
    /// start time, before any feed is up. NOT wired: see `vike_tradehub_core::symbol_grid`'s module doc,
    /// its ⚠ section.
    PerSymbolFetch,
    /// The arm fetches no instrument grid at all (or the venue publishes none of that shape), so
    /// there is nothing to read per symbol and nothing to wire. Its engine symbol has no grid
    /// either, which is why a leg here inherits nothing harmful.
    NoGrid,
}

/// **How one venue's effective trading book is identified from the credential store, offline.**
///
/// The two variants are the two honest answers, and there is no third: either the account's
/// credentials NAME the book (directly, or through a field from which the name is derivable), or
/// they do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookIdentity {
    /// **The account's credentials name the book.** [`Self::Named::name_fields`] are the account's
    /// FIELDS tried in order; the first non-blank value IS the book.
    /// [`Self::Named::evm_key_fields`] is the FALLBACK for a venue whose identifier may be left
    /// implicit in an EVM private key — the address is then derived locally
    /// (`vike_bridge_core::eip712::eth_address_from_private_key`), which is the same derivation
    /// that venue's own signer performs.
    Named {
        /// Fields whose VALUE is the book, tried in order. Must be safe to log — see
        /// `vike_tradehub_core::book_identity`'s module doc.
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

/// What a drifted host clock actually COSTS at a venue — the fact each wired row must state, so the
/// report's remedy can never assert a mechanism the venue does not have, and so only the venues
/// that can lose an order are judged against the recv-window thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockRisk {
    /// Every private request is SIGNED with a timestamp and rejected outside a recv window (the
    /// signers hard-code `recv_window: 5000`). Drift far enough and EVERY order is rejected:
    /// binance `-1021`, bybit `10002`, okx `50102`.
    SignedTimestamp,
    /// The clock is bound into the order NONCE rather than a recv window. Hyperliquid accepts a
    /// nonce inside `(T - 2 days, T + 1 day)`
    /// (`crates/bridges/hyperliquid/src/signing/hash.rs`'s `NonceManager`), so the rejection cliff
    /// is a DAY away, not five seconds.
    NonceWindow,
    /// Auth carries no per-request timestamp at all (deribit's `client_credentials`, ig's session
    /// token pair), so no order can be rejected for clock drift. The leg is a HOST-HEALTH canary:
    /// it still proves the box's NTP is broken, and a wrong clock still misdates every local
    /// record of the session.
    NoTimestamp,
}

impl ClockRisk {
    /// The remediation line for a measured out-of-band skew at a venue with this risk. Every one
    /// starts with the same action — the host clock is what is wrong — and differs in what it
    /// costs, which is the part an operator prioritises on.
    #[must_use]
    pub fn remedy(self) -> &'static str {
        match self {
            ClockRisk::SignedTimestamp => {
                "sync the host clock (NTP / w32tm) — this venue stamps a timestamp on every signed \
                 request and rejects one outside a 5000 ms recv window, so drift rejects ORDERS"
            }
            ClockRisk::NonceWindow => {
                "sync the host clock (NTP / w32tm) — this venue binds the clock into the order \
                 nonce (valid within roughly a day), so orders survive this drift but the host's \
                 NTP is broken"
            }
            ClockRisk::NoTimestamp => {
                "sync the host clock (NTP / w32tm) — this venue's auth stamps no timestamp, so \
                 orders are NOT at risk here; the drift misdates every local record and is evidence \
                 the host's NTP is broken"
            }
        }
    }
}

/// Whether reading this venue's clock needs a credential — the fact that decides whether a box
/// WITHOUT that credential may legitimately skip the read, or has just found a real fault.
///
/// A declared field, never inferred from the human-facing `endpoint` text: the live smoke branches
/// on it (a keyless endpoint that fails from a box with egress is a real fault, a credentialed one
/// may simply have no key here), and sniffing labels like deribit's `"(public, testnet host)"`
/// reports a genuine failure as a skipped credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockAuth {
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

/// A venue's static facts — no I/O.
#[derive(Debug, Clone, Copy)]
pub struct VenueDeclaration {
    /// `resolve` and `start` read the NAMED account's keys.
    pub addresses_accounts: bool,
    pub process_exclusive: Option<ProcessExclusive>,
    /// The venue's resync supervisor takes the reconnect trigger.
    pub takes_recon_trigger: bool,
    pub grid_source: DeclaredGridSource,
    pub book_identity: BookIdentity,
    pub clock: ClockDecl,
}

/// A venue's server-clock row; [`VenueAdapter::server_time_ms`] is the read for a `Wired` one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockDecl {
    Wired { endpoint: &'static str, auth: ClockAuth, risk: ClockRisk },
    NotWired { reason: &'static str, unmeasured_risk: Option<&'static str> },
}

/// The startup credential probe a venue offers.
pub enum CredentialProbe {
    /// `vike-tradehub-core` reads the balance, then records which account the key is at `bound_tier`.
    RecordsIdentity { client: Box<dyn ReconClient>, bound_tier: AccountTier },
    /// The bridge's own read; nothing is recorded.
    ReadOnly(Arc<dyn Fn() -> Result<(), String> + Send + Sync>),
}

/// THE CONTRACT. Object-safe: a registry holds `&'static dyn VenueAdapter` rows.
pub trait VenueAdapter: Send + Sync {
    /// A `vike_model::VENUES` id.
    fn venue(&self) -> &'static str;
    fn declaration(&self) -> VenueDeclaration;
    /// PURE: no side effect on the world outside the process — no network, no thread, no process,
    /// no file, no claim — other than `tracing` events. A `resolve` may LOG what it found.
    ///
    /// ⚠ One declared exception: fxcm's answer depends on whether the ForexConnect SDK is staged on
    /// this box, and `vike_fxcm::sdk_available()` finds out by OPENING a shared library. That is
    /// local I/O with no network and no venue contact, and it is the only I/O a `resolve` may do.
    fn resolve(&self, inputs: &AdapterInputs<'_>) -> Resolution;
    /// Acts on [`Self::resolve`]'s answer.
    fn start(&self, req: AdapterRequest<'_>) -> AdapterOutcome;
    /// The clock read for a `ClockDecl::Wired` venue, bounded by `timeout`.
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
}

/// How an account is named in a sentence: `the default account` or `account ALT`.
fn whose(account: &AccountLabel) -> String {
    account
        .text()
        .map_or_else(|| "the default account".to_string(), |label| format!("account {label}"))
}

/// Build a reconcile client only when reconciliation is on. `false` never CALLS `build`: the
/// property is the LAZINESS, which no assertion on the returned `Option` can see. A bridge that
/// does not call this builds its recon client whenever it starts live; read the venue's `mount.rs`
/// for whether it reconciles.
pub fn recon_if_enabled<F>(recon_enabled: bool, build: F) -> Option<Box<dyn ReconClient>>
where
    F: FnOnce() -> Option<Box<dyn ReconClient>>,
{
    if recon_enabled { build() } else { None }
}

/// **The loud half of [`PaperCause::LiveTierNotWired`]**: say, at `error!`, that a `live` account
/// holds a key set this arm will not use and the venue is therefore staying paper.
///
/// A demo-only arm calls it exactly when the account's tier is `live` and the account holds its
/// key set — the same two facts its `resolve` answers [`PaperCause::LiveTierNotWired`] from — and
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
/// ⚠ It logs NO key name and NO value: `vike-cli secrets list` finds the keys, and this sentence is
/// pasted into chats.
pub fn report_live_tier_not_wired(
    venue: &'static str,
    account: &AccountLabel,
    demo_word: &'static str,
) {
    let whose = whose(account);
    tracing::error!(
        venue,
        account = %account,
        found_tier = AccountTier::Live.as_str(),
        "{venue}: {whose} is a LIVE account (tier `live`) holding its credentials, but this arm \
         mounts only its {demo_word} tier and never selects a live one, so {venue} stays PAPER. \
         Nothing was signed \
         and no order can reach the live account from this build. To trade the {demo_word} \
         account, give it its own account row at tier `demo` with its {demo_word} keys; \
         `vike-cli secrets accounts` names what the store holds"
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
        crate::http::blocking_agent_with_timeout(timeout),
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
