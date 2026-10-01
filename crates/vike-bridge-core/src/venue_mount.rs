//! **THE VENUE MOUNT CONTRACT** — the one trait each venue bridge implements, so that `vike-mount`
//! names no venue (`docs/decisions/0096-each-bridge-mounts-itself-behind-one-contract.md`).
//!
//! # Two phases, one source of truth
//!
//! [`VenueMount::resolve`] is PURE — no network, no thread, no side effect beyond a `tracing`
//! event — and it IS the arming probe: `vike-mount`'s arming projection calls it.
//! [`VenueMount::mount`] acts on the SAME answer (it calls `resolve`, or the one private function
//! `resolve` is built on), so the probe and the arm cannot disagree: they are one function. What
//! "pure" permits, and its one declared exception, are on [`VenueMount::resolve`] itself.
//!
//! # The rules every implementation keeps
//!
//! 1. **Inputs are data.** A bridge reads no process environment, no process-global path and no
//!    `vike-config` type: secrets arrive in [`MountInputs::secrets`], venue settings in
//!    [`MountInputs::settings`], the state and bin directories and the HALT sentinel's path in
//!    [`MountInputs::process`].
//! 2. **The arming ceiling crosses only as [`MountInputs::live_permitted`].** This crate does not
//!    depend on `vike-config`, so `VenueMode` and `MountPolicy` cannot be named here.
//! 3. **Only `vike-mount` builds a paper engine.** A bridge that declines returns
//!    [`ExecOutcome::Paper`]; `crates/vike-ops/tests/paper_mount_arming_gate.rs` holds it.
//! 4. **The fallback grid is the bridge's own business**, internal to its `mount`.
//! 5. **This module is a SHARED HOME.** It is extended by ONE coordinated change informed by every
//!    consumer. A venue port that finds a missing knob stops and reports; it never widens a type
//!    here in passing.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient, ProfileRisk};
use vike_model::account_keys::AccountLabel;
use vike_model::{HaltAdmit, MarginMode, SymbolProperties};
use vike_secrets::venue_setting::VenueSettings;

use crate::account_directory::AccountDirectory;
use crate::transport::RestTransport;

/// The tier a bridge's credentials authenticate — bridge-facing, and deliberately NOT
/// `vike_config::VenueMode` (0088's rule: a ceiling never crosses into a bridge as a policy type).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// The venue's demo, testnet, sandbox or practice account.
    Demo,
    /// The venue's real-money account.
    Live,
}

/// Process facts a mount may need, resolved ONCE by `vike-mount`, so a bridge reads no process
/// global. `Default` is "this process declared no project" — a test, a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessFacts {
    /// `<project>/settings/state`, as the boot declared it.
    pub state_dir: Option<PathBuf>,
    /// `<project>/bin`, derived from `state_dir`.
    pub bin_dir: Option<PathBuf>,
    /// **The operator HALT sentinel** — the one file the process's kill switch is, as
    /// `vike-mount` resolved it (`crate::halt::halt_path_from_env`; `<state_dir>/HALT` whenever the
    /// boot declared a project). Every live exec client the mount builds is handed THIS path with
    /// its `with_halt_path` builder, and checks `crate::halt::sentinel_engaged` against it at its
    /// submit seam; a bridge resolves nothing itself (decision 0099).
    ///
    /// ⚠ The `Default` is EMPTY, and an empty path watches nothing — a test or a tool that built
    /// facts by hand. `vike-mount` always fills it, so a real mount never sees the empty one, and
    /// a client handed it logs `HALT KILL SWITCH IS NOT WIRED` rather than looking armed.
    pub halt_path: PathBuf,
}

/// Everything a bridge may know about the deployment — DATA ONLY.
#[derive(Clone, Copy)]
pub struct MountInputs<'a> {
    /// The account being resolved or mounted; the default account on a single-account box.
    pub account: &'a AccountLabel,
    /// The credential store's view — the map every legacy arm called `vars`.
    pub secrets: &'a HashMap<String, String>,
    /// This venue's `venue_setting` rows. No bridge reads it yet; the companion plan moves readers
    /// onto it one field at a time.
    pub settings: &'a VenueSettings,
    /// The arming ceiling as a bool: `true` exactly when the ceiling is `live`.
    pub live_permitted: bool,
    /// The settings database's `account` table, as the composition root read it.
    pub accounts: &'a AccountDirectory,
    /// The state and bin directories, and the HALT sentinel's path.
    pub process: &'a ProcessFacts,
}

/// Why a venue stays paper — one variant per `vike_config::ArmingBlock` a BRIDGE can produce. The
/// others (`Disarmed`, `FeatureAbsent`, `NoAccountSupport`, `AccountNotNamed`,
/// `SidecarHeldElsewhere`) are `vike-mount`'s own and are never a bridge's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperCause {
    /// No usable key set for the tier the arm would reach — the original live gate.
    NoCredentials,
    /// A `live` ceiling permits the live tier, the arm could reach it, and the store holds no
    /// LIVE-tier key set — and this arm does NOT fall back to its demo tier, so it stays paper
    /// (decision 0095: for the venues whose network is the ceiling, `live` means mainnet and a
    /// mainnet host is never signed with demo keys). Maps to
    /// `vike_config::ArmingBlock::LiveCredentialsAbsent`. The arm that DOES fall back arms its demo
    /// tier instead and answers [`HeldBelowLive::LiveCredentialsAbsent`].
    LiveCredentialsAbsent,
    /// Polymarket's exec switch is not set.
    ExecFlagUnset,
    /// The venue has no demo tier and the ceiling is below `live`.
    LiveOnlyArm,
    /// A native SDK this venue needs did not load on this box.
    SdkAbsent,
    /// The settings database cannot say which account this is.
    AccountNotInStore,
    /// The bridge has no live arm at all — the scaffold's conservative answer.
    NoLiveArm,
}

/// What holds an armed DEMO resolution below a `live` ceiling. `vike-mount` reports it only when
/// the ceiling is `live`; below that the demo tier IS the ceiling and nothing is being refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldBelowLive {
    /// The arm hardcodes its demo endpoint.
    DemoOnlyArm,
    /// The arm can reach live, but only demo-tier credentials exist.
    LiveCredentialsAbsent,
}

/// The pure arming answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Stays paper, and why.
    Paper(PaperCause),
    /// Arms at `tier`; `held_below_live` says why a demo arming is not live.
    Armed { tier: Tier, held_below_live: Option<HeldBelowLive> },
}

/// Everything a mount is handed.
pub struct MountRequest<'a> {
    pub inputs: MountInputs<'a>,
    pub symbol: &'a str,
    pub declared_legs: &'a [String],
    /// Already scoped to this account's route key by `vike-mount`.
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

/// A live exec client and what `vike-mount` folds from it.
pub struct LiveExec {
    pub client: Box<dyn ExecutionClient + Send>,
    /// The tier the credentials actually authenticate — what the identity record is keyed on.
    pub bound_tier: Tier,
    /// The mounted symbol's grid in BASE units; `None` keeps the permissive default.
    pub grid: Option<SymbolProperties>,
    /// The mounted symbol's contract size, where the arm sets one.
    pub contract_size: Option<f64>,
    /// The mounted symbol's ruling margin mode, where the arm resolves one.
    pub margin_mode: Option<MarginMode>,
    /// Declared legs' grids the arm already holds, in declaration order.
    pub leg_grids: Vec<(String, SymbolProperties)>,
}

/// A mount's exec half.
// One value per mount, built once and moved once into the fold: boxing `LiveExec` would buy
// nothing and respell every construction a venue port writes.
#[allow(clippy::large_enum_variant)]
pub enum ExecOutcome {
    Live(LiveExec),
    /// `vike-mount` builds the paper client; the bridge has already logged why.
    Paper,
}

/// The venue's own answer about which BOOK this account trades, for `vike-mount` to record.
pub struct IdentityReport {
    pub book: String,
    /// What the venue answered with, for the log line (e.g. "`userRole`").
    pub evidence: &'static str,
    pub tier: Tier,
}

/// What a mount produced.
pub struct MountOutcome {
    pub exec: ExecOutcome,
    /// May be `Some` on `Paper` (polymarket's recon-only shape).
    pub recon: Option<Box<dyn ReconClient>>,
    /// May be `Some` on `Paper` (hyperliquid answered its identity probe, then failed `meta`).
    pub identity: Option<IdentityReport>,
}

impl MountOutcome {
    /// Paper, with nothing to reconcile and nothing to record.
    #[must_use]
    pub fn paper() -> Self {
        MountOutcome { exec: ExecOutcome::Paper, recon: None, identity: None }
    }
}

// ---- the declaration types MOVED from vike-mount land here (decision 0096) ----------------------

/// Where a venue arm would have to get the instrument grid for a symbol OTHER than the mounted
/// one — the declaration behind which arms of `vike_mount::make_engine_with_legs` populate
/// `vike_exec::RiskLimits::grid_by_symbol` (see `vike_mount::symbol_grid`'s module doc, its ⚠
/// section).
///
/// Declaring TODAY'S REALITY, per the capability-map playbook: the rows are read off the arms, no
/// arm's behaviour is inferred from a row, and `every_roster_venue_declares_a_grid_source` iterates
/// `vike_model::VENUES` so a new bridge crate cannot join the roster without classifying itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclaredGridSource {
    /// The arm ALREADY holds a resolved instrument table covering every symbol of the venue, so a
    /// declared leg's grid costs no network at all. These are the arms wired today.
    InHand,
    /// The arm's only per-symbol source is a blocking, symbol-SCOPED network round trip — the very
    /// call it already makes once for the mounted symbol. A declared leg would cost one more, at
    /// mount time, before any feed is up. NOT wired: see `vike_mount::symbol_grid`'s module doc,
    /// its ⚠ section.
    PerSymbolFetch,
    /// The arm fetches no instrument grid at all (or the venue publishes none of that shape), so
    /// there is nothing to read per symbol and nothing to wire. Its mounted symbol has no grid
    /// either, which is why a leg here inherits nothing harmful.
    NoGrid,
}

/// **How one venue's effective trading book is identified from the credential store, offline.**
///
/// The two variants are the two honest answers, and there is no third: either the store NAMES the
/// account (directly, or through a key from which the name is derivable), or it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookIdentity {
    /// **The store names the book.** [`Self::Named::name_suffixes`] are the key suffixes tried in
    /// order; the first non-blank value IS the book. [`Self::Named::evm_key_suffixes`] is the
    /// FALLBACK for a venue whose identifier may be left implicit in an EVM private key — the
    /// address is then derived locally (`vike_bridge_core::eth_address_from_private_key`), which is
    /// the same derivation that venue's own signer performs.
    Named {
        /// The `{PREFIX}` of every key this row composes — `"OANDA"`, `"POLY"`, `"HYPERLIQUID"`.
        prefix: &'static str,
        /// The tier tokens tried, IN ORDER, for an account that resolved to `VenueMode::Demo`.
        /// Empty means the venue has no demo tier at all (polymarket runs no testnet), in which
        /// case a demo-resolved account of it has no book — which is correct, since its arm cannot
        /// mount one either.
        demo_tiers: &'static [&'static str],
        /// The tier tokens tried, IN ORDER, for an account that resolved to `VenueMode::Live`.
        /// More than one entry is a LEGACY spelling the venue's own loader still accepts
        /// (`vike_bridge_core::credentials::Environment::legacy_str`'s `MAINNET`).
        live_tiers: &'static [&'static str],
        /// Key suffixes whose VALUE is the book, tried in order. Must be safe to log — see
        /// `vike_mount::book_identity`'s module doc.
        name_suffixes: &'static [&'static str],
        /// Key suffixes holding an EVM private key the address is derived from when no
        /// [`Self::Named::name_suffixes`] key is present. Empty for every non-EVM venue.
        evm_key_suffixes: &'static [&'static str],
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
    /// nonce inside `(T - 2 days, T + 1 day)` (`docs/research/2026-07-16-hyperliquid-adapters/README.md`
    /// §9, implemented by that crate's `NonceManager`), so the rejection cliff is a DAY away, not
    /// five seconds.
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
/// It is a declared field rather than something inferred from the endpoint text, because the live
/// smoke branches on it: a keyless endpoint that fails from a box with egress is the ② this whole
/// lane exists to surface, while a credentialed one may simply have no key here. Sniffing the
/// human-facing `endpoint` string for "(public)" got deribit's row — whose label reads
/// `"(public, testnet host)"` — WRONG, reporting a genuine failure as a skipped credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockAuth {
    /// Keyless: any box with egress can read it, so a failure is always a real failure.
    Public,
    /// Needs a credential from the vars map (never a signature — these are all cheap reads).
    Credentialed,
}

/// A resource only ONE account of a venue may hold per process — dukascopy's JForex sidecar. The
/// two functions render the refusals in the venue's own words; the rule itself is `vike-mount`'s.
///
/// ⚠ `resource` keys the process-level CLAIM, so two venues that declare one resource do exclude
/// each other there. The PROJECTION does not model that: which account holds the resource is
/// decided per VENUE, so each of two venues sharing a resource projects its own holder as armed,
/// and the second to mount comes up PAPER through the claim, for a reason the projection never
/// showed. No two declarations share a resource today; a venue that needs to would first have to
/// teach `vike-mount`'s holder rule to look across venues.
#[derive(Debug, Clone, Copy)]
pub struct ProcessExclusive {
    pub resource: &'static str,
    pub held_by_another: fn(label: &str, holder: &str) -> String,
    pub already_claimed: fn(label: &str) -> String,
}

/// A venue's static facts — no I/O.
#[derive(Debug, Clone, Copy)]
pub struct VenueDeclaration {
    /// `resolve` and `mount` read the NAMED account's keys (was `arm_addresses_accounts`).
    pub addresses_accounts: bool,
    pub process_exclusive: Option<ProcessExclusive>,
    /// The venue's resync supervisor takes the reconnect trigger.
    pub takes_recon_trigger: bool,
    pub grid_source: DeclaredGridSource,
    pub book_identity: BookIdentity,
    pub clock: ClockDecl,
}

/// A venue's server-clock row; [`VenueMount::server_time_ms`] is the read for a `Wired` one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockDecl {
    Wired { endpoint: &'static str, auth: ClockAuth, risk: ClockRisk },
    NotWired { reason: &'static str, unmeasured_risk: Option<&'static str> },
}

/// The startup credential probe a venue offers.
pub enum CredentialProbe {
    /// `vike-mount` reads the balance, then records which account the key is at `bound_tier`.
    RecordsIdentity { client: Box<dyn ReconClient>, bound_tier: Tier },
    /// The bridge's own read; nothing is recorded.
    ReadOnly(Arc<dyn Fn() -> Result<(), String> + Send + Sync>),
}

/// THE CONTRACT. Object-safe: a registry holds `&'static dyn VenueMount` rows.
pub trait VenueMount: Send + Sync {
    /// A `vike_model::VENUES` id.
    fn venue(&self) -> &'static str;
    fn declaration(&self) -> VenueDeclaration;
    /// PURE: no side effect on the world outside the process — no network, no thread, no process,
    /// no file, no claim — other than `tracing` events. A `resolve` may LOG what it found: an
    /// `error!` when the credential store will not open, for instance, as dukascopy's does
    /// (`crates/bridges/dukascopy/src/mount.rs`'s `resolve_in`).
    ///
    /// ⚠ One declared exception: fxcm's answer depends on whether the ForexConnect SDK is staged on
    /// this box, and `vike_fxcm::sdk_available()` finds out by OPENING a shared library. That is
    /// local I/O with no network and no venue contact, and it is the only I/O a `resolve` may do.
    fn resolve(&self, inputs: &MountInputs<'_>) -> Resolution;
    /// Acts on [`Self::resolve`]'s answer.
    fn mount(&self, req: MountRequest<'_>) -> MountOutcome;
    /// The clock read for a `ClockDecl::Wired` venue, bounded by `timeout`.
    fn server_time_ms(&self, _inputs: &MountInputs<'_>, _timeout: Duration) -> Result<i64, String> {
        Err("no clock leg".into())
    }
    /// The startup credential probe, or `None` when the venue is not probed.
    ///
    /// ⚠ `vike-mount` calls this on the startup thread, OUTSIDE the bound its probes run under; only
    /// the probe it returns is bounded. So build a client eagerly only when construction is PURE —
    /// a signer and a transport, no network: [`CredentialProbe::RecordsIdentity`]'s shape. A client
    /// whose construction CONNECTS is built inside a [`CredentialProbe::ReadOnly`] closure
    /// instead, as ctrader's is. Connecting here would block the startup thread with no bound, and a
    /// connect that failed here would return no probe at all — no row, and silence where the
    /// credential leg exists to FAIL.
    fn credential_probe(&self, _inputs: &MountInputs<'_>) -> Option<CredentialProbe> {
        None
    }
}

/// Build a reconcile client only when reconciliation is on — MOVED from `vike-mount`, where it was
/// private (which is why ctrader's bridge restated it). `false` never calls `build`: the property
/// is the LAZINESS, which no assertion on the returned `Option` can see.
pub fn recon_if_enabled<F>(recon_enabled: bool, build: F) -> Option<Box<dyn ReconClient>>
where
    F: FnOnce() -> Option<Box<dyn ReconClient>>,
{
    if recon_enabled { build() } else { None }
}

/// `GET {base}{path}` on an agent bounded by `timeout`, with the venue's own error text — the
/// shared helper for a bridge's clock read. `vike-mount`'s legacy clock fetchers kept their own
/// copy (`public_time_body`) until okx's, its last caller, moved into okx's bridge.
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
/// server-time parser. (It began as the bridges' copy of a `missing` helper vike-mount's own
/// clock fetchers used; those fetchers, and that helper, left vike-mount when the last venues
/// moved into their bridges — #2356.)
#[must_use]
pub fn missing_time_field(field: &str) -> String {
    format!("{field} missing from the server-time response")
}

#[path = "venue_mount_tests.rs"]
#[cfg(test)]
mod venue_mount_tests;
