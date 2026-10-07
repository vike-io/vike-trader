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
//!    [`ExecOutcome::Paper`]; `crates/vike-ops/tests/wiring/paper_mount_arming_gate.rs` holds it.
//! 4. **The fallback grid is the bridge's own business**, internal to its `mount`.
//! 5. **This module is a SHARED HOME.** It is extended by ONE coordinated change informed by every
//!    consumer. A venue port that finds a missing knob stops and reports; it never widens a type
//!    here in passing.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient, ProfileRisk};
use vike_model::accounts::account_keys::AccountLabel;
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

impl Tier {
    /// The one spelling of a tier in a structured log field and in prose — `demo` or `live`, the
    /// words `vike_config::VenueMode::as_str` uses for the same two tiers. Every bridge's
    /// live-mount line carries it as `tier = Tier::Live.as_str()` or `tier = Tier::Demo.as_str()`
    /// (`crates/vike-ops/tests/wiring/live_mount_line_gate.rs` holds the convention), so no bridge can
    /// misspell a tier, and a log pipeline filters on two values.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Tier::Demo => "demo",
            Tier::Live => "live",
        }
    }
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
    /// **A LIVE-tier key set for this account is in the store, and this arm mounts only its demo,
    /// practice or sandbox tier** — it has no live arm, so a live key set is never selected and the
    /// venue stays paper, whatever the ceiling says. Maps to
    /// `vike_config::ArmingBlock::LiveTierNotWired`.
    ///
    /// It is the cause for a credential that is PRESENT and UNUSABLE, which the credential doctrine
    /// says is an error and not an absence (the store holds a key; nothing will ever read it), and
    /// it is deliberately neither of its neighbours: [`Self::NoCredentials`] sends an operator to
    /// WRITE keys, and [`Self::LiveCredentialsAbsent`] is the opposite fact (the ceiling is `live`,
    /// the arm COULD use a live set and none is stored). A bridge that answers it also reports it
    /// loudly at `mount`, through [`report_unused_live_tier`] (which speaks
    /// [`report_live_tier_not_wired`] for this cause), so the log and the arming screen say the same
    /// thing; for a LABELLED account, which is never mounted, `vike-mount` asks the bridge to speak
    /// for it through [`VenueMount::report_unmounted_account`].
    ///
    /// What counts as "a live-tier key set" is the arm's own loader at the live tier: a COMPLETE set,
    /// with one exception that predates the cause — oanda refuses on ANY live-named variable, half a
    /// pair included, and ALSO when a practice set is stored beside it (a store that says `live` must
    /// never trade the practice account in silence). The other arms that mount only a demo tier and
    /// find a live set beside a demo one still mount the demo tier, byte for byte as before — and say,
    /// once per process per account, that the live set is unused. A HALF-written live set is not this
    /// cause (the loader calls it absent, so it stays [`Self::NoCredentials`], as okx's half-written
    /// trio does); the mount names the keys it lacks in the log instead.
    LiveTierNotWired,
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
    /// **Say what this arm knows about an account it will NOT mount.** `vike-mount` calls it, once
    /// per start, for each LABELLED account of the venue that resolved paper for want of a usable
    /// key set — a `Paper(LiveTierNotWired)` or `Paper(NoCredentials)` answer under a ceiling
    /// above `paper`, or `Paper(SdkAbsent)` for the arm whose box can lack the SDK a key set needs
    /// — because such an account is never mounted ([`VenueMount::mount`] is reached for the default
    /// account and for a labelled account that armed, and for no other). The arm works out for
    /// itself whether its demo tier loaded, so it can be asked in any of those cells. Without
    /// this the DEFAULT account's `mount` would say "a live-tier key set is stored and unused" and
    /// the same fact on a labelled account would be silent, visible only on `vike-backend venues`.
    ///
    /// **Logging only.** It has no return value, so it cannot change what mounts, and it must not
    /// block, touch the network or read anything beyond `inputs`. The default says nothing, which
    /// is right for every arm that has no live-tier finding to make; an arm that makes one
    /// implements this with the SAME function its `mount` calls, so the two surfaces cannot word
    /// one fact two ways.
    fn report_unmounted_account(&self, _inputs: &MountInputs<'_>) {}
}

/// **What an arm that mounts only its demo tier found of the LIVE tier in one account's store** —
/// the three states its mount can say something different about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveTierSet {
    /// No live-tier key at all: the ordinary unconfigured state, and silent.
    Absent,
    /// A complete live-tier set, as the arm's own loader reads one.
    Complete,
    /// Some live-tier keys exist and the set is incomplete: the REQUIRED names that are missing,
    /// label-composed, in the loader's order. Names only — never a value.
    Incomplete { missing: Vec<String> },
}

impl LiveTierSet {
    /// The state of one account's live tier. `complete` is the arm's own loader's verdict at the
    /// live tier (so "complete" can never mean something the loader would refuse), and
    /// `spellings` are the names that loader reads (`credentials::TierKeys`), consulted only when
    /// the set is not complete: the first spelling that has a tier-named key and lacks a required
    /// one is the half-written set, and its missing names are the answer.
    #[must_use]
    pub fn read(
        complete: bool,
        spellings: &[crate::credentials::TierKeys],
        secrets: &HashMap<String, String>,
    ) -> Self {
        if complete {
            return LiveTierSet::Complete;
        }
        spellings
            .iter()
            .find_map(|keys| keys.missing_in(secrets))
            .map_or(LiveTierSet::Absent, |missing| LiveTierSet::Incomplete { missing })
    }
}

/// **What a demo-pinned arm says about the LIVE tier of the account it is mounting** — one call from
/// `mount` (and, through [`VenueMount::report_unmounted_account`], for a labelled account that is
/// never mounted), so the six arms word it once. `demo_mounts` is whether the arm's demo tier
/// loaded for this account.
///
/// | demo tier | live tier | what is said |
/// |---|---|---|
/// | mounts | complete | one `warn!`: the live set is UNUSED, said once per process per `(venue, account)` |
/// | absent | complete | one `error!`: [`report_live_tier_not_wired`] |
/// | absent | incomplete | one `error!` naming the MISSING key names, and only names |
/// | any other cell | | nothing |
///
/// Every line is a DIAGNOSTIC: this function returns nothing and is reached only after the arm has
/// decided what to mount, so it cannot move a venue between paper and live. Each carries `venue`,
/// `account` and `found_tier` (never `tier`: see [`report_live_tier_not_wired`]), and no value.
pub fn report_unused_live_tier(
    venue: &'static str,
    account: &AccountLabel,
    demo_word: &'static str,
    demo_mounts: bool,
    live: &LiveTierSet,
) {
    match (demo_mounts, live) {
        (true, LiveTierSet::Complete) => report_live_tier_unused(venue, account, demo_word),
        (false, LiveTierSet::Complete) => report_live_tier_not_wired(venue, account, demo_word),
        (false, LiveTierSet::Incomplete { missing }) => {
            report_live_tier_incomplete(venue, account, demo_word, missing);
        }
        // A half-written live set beside a complete demo set changes nothing: the demo tier mounts
        // and no live key was going to be read. An absent live tier is the ordinary state.
        (true, LiveTierSet::Incomplete { .. }) | (_, LiveTierSet::Absent) => {}
    }
}

/// How an account is named in a sentence: `the default account` or `account ALT`.
fn whose(account: &AccountLabel) -> String {
    account
        .text()
        .map_or_else(|| "the default account".to_string(), |label| format!("account {label}"))
}

/// The `(venue, account)` pairs [`report_live_tier_unused`] has already said, for the life of the
/// process. Keyed by the account's rendered label (`DEFAULT` for the default account), the key every
/// mount line renders it under.
static UNUSED_LIVE_TIER_SAID: LazyLock<Mutex<HashSet<(&'static str, String)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// **A complete LIVE-tier set BESIDE a demo one, said once per process per `(venue, account)`.** The
/// arm mounts its demo tier, so there is no refusal to report and nothing stays paper — but an
/// operator who stored live keys believing they would trade live read a mount line that named the
/// demo tier and nothing else. `warn!`, not `error!`: the venue mounted what it always mounted.
///
/// The dedup is the point of the `Mutex`: a mount is reached again whenever a venue is mounted again
/// in one process (a retry, a re-plan, a test), and the same sentence at every one is a line nobody
/// reads twice. A new `(venue, account)` is a new finding and is said. The key is the pair, never
/// the store's content, so rotating a key does not re-arm it — a restart of the process does.
fn report_live_tier_unused(venue: &'static str, account: &AccountLabel, demo_word: &'static str) {
    let first = UNUSED_LIVE_TIER_SAID
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert((venue, account.to_string()));
    if !first {
        return;
    }
    let whose = whose(account);
    tracing::warn!(
        venue,
        account = %account,
        found_tier = Tier::Live.as_str(),
        "{venue}: a LIVE-tier credential set is ALSO stored for {whose}, and it is UNUSED: this arm \
         mounts only its {demo_word} tier and never selects a live one, so no order can reach the \
         live account from this build and nothing was signed with that set. `vike-cli secrets \
         list` names what the store holds"
    );
}

/// **A HALF-WRITTEN live-tier set** — some of its keys are stored and the arm's loader calls the set
/// absent, which printed `NoCredentials` under the words of an empty store. The sentence names the
/// REQUIRED keys that are missing (label-composed, so the names an operator has to write for THIS
/// account) the way `missing_required_passphrase_for_account`'s report names okx's passphrase, and
/// nothing else: not the keys that are present, and no value.
///
/// It also says what completing the set would NOT do, because that is the part an operator acts on
/// wrongly: this arm mounts only its demo tier, so a complete live set would be the
/// `LiveTierNotWired` case, not a mount. `error!` for the reason [`report_live_tier_not_wired`]
/// gives; the cause the arming screen prints stays `NoCredentials`, as okx's does.
fn report_live_tier_incomplete(
    venue: &'static str,
    account: &AccountLabel,
    demo_word: &'static str,
    missing: &[String],
) {
    let whose = whose(account);
    let names = missing.iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ");
    tracing::error!(
        venue,
        account = %account,
        found_tier = Tier::Live.as_str(),
        missing_keys = %missing.join(", "),
        "{venue}: a LIVE-tier credential set is stored for {whose} but it is INCOMPLETE — {names} \
         unset or blank. This arm mounts only its {demo_word} tier and never selects a live one, \
         so completing that set would not mount it either: {venue} stays PAPER, and nothing was \
         signed. To trade the {demo_word} account, store its {demo_word}-tier keys \
         (`vike-cli secrets set <NAME>`); `vike-cli secrets list` names what the store holds"
    );
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

/// **The loud half of [`PaperCause::LiveTierNotWired`]**: say, at `error!`, that the store holds a
/// LIVE-tier key set the arm will not use and the venue is therefore staying paper.
///
/// A bridge reaches it through [`report_unused_live_tier`] exactly when its demo-tier config is
/// absent and a live-tier one is present — the same two facts its `resolve` answers
/// [`PaperCause::LiveTierNotWired`] from — and then returns [`MountOutcome::paper`], so the arming
/// screen and the log say the same thing. (fxcm answers the SDK question too, and it is the one arm
/// where that could make the two disagree: its `resolve` and its `mount` now ask their questions in
/// ONE order — a stand-alone live login first, then the shim, then the demo login — so a box without
/// the shim holding only a live login reads this cause on the screen as well as in the log.
/// `crates/bridges/fxcm/src/mount.rs`'s `resolution_for` is the rule.) `error!` and not `warn!`
/// because the credential doctrine is that an ABSENT credential is silent and a PRESENT-and-unusable
/// one is an error: a misconfiguration wearing the "not configured" answer looks exactly like a
/// correct fresh install.
///
/// The event carries `venue`, `account` and **`found_tier`** (`live`, the tier that was FOUND and
/// refused) — and deliberately NOT `tier`. On every live-MOUNT line `tier` is the tier that was
/// MOUNTED (`Tier::as_str`; `crates/vike-ops/tests/wiring/live_mount_line_gate.rs` holds that), so a
/// pipeline filtering on `tier=live` matches exactly the mounts that went live and never this line,
/// which says nothing did. `demo_word` is what this arm calls the tier it DOES mount (`demo`,
/// `sandbox`, `testnet`…), spoken in the sentence.
///
/// One call is one line: a start emits it once per `(venue, account)` whose bridge `mount` is
/// reached — the venue's DEFAULT account whenever its ceiling is above `paper` — and, for a
/// LABELLED account, which is mounted only when it armed above paper (which a live-tier-only
/// labelled account never does: `vike_mount::accounts_to_mount`), once per start through
/// [`VenueMount::report_unmounted_account`], which `vike_mount::make_engine_accounts` calls for
/// exactly the labelled accounts it does not mount. The same sentence either way.
///
/// ⚠ It logs NO key name and NO value: a name is enough to find it with `vike-cli secrets list`, and
/// this sentence is pasted into chats. (oanda's older refusal names the live-named VARIABLES it
/// found — `UnreachableLiveTier` holds names and never a value — and is the one disposition the
/// doctrine allows for a name; it does not use this function. The half-written-set line beside it
/// names the MISSING keys for the same reason a missing passphrase is named: the operator has to
/// write them.)
pub fn report_live_tier_not_wired(
    venue: &'static str,
    account: &AccountLabel,
    demo_word: &'static str,
) {
    let whose = whose(account);
    tracing::error!(
        venue,
        account = %account,
        found_tier = Tier::Live.as_str(),
        "{venue}: a LIVE-tier credential set is stored for {whose}, but this arm mounts only its \
         {demo_word} tier and never selects a live one, so {venue} stays PAPER. Nothing was signed \
         and no order can reach the live account from this build. To trade the {demo_word} \
         account, store its {demo_word}-tier keys; `vike-cli secrets list` names what the store \
         holds"
    );
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
