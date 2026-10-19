//! The MIRROR DTOs of the trade wire: what the gatewayhub answers about arming, preflight and an
//! adapter start, as plain serde data with no bridge, no `vike-bridge-core`, no `vike-config` and no
//! `vike-secrets` behind it.
//!
//! The tradehub's venue seam (`VenueBackend`, wave 2's W9a/W11) is written against these. The wire
//! carries WORDS, grids, opaque handles and static facts - **never** a `Resolution`, an
//! `AdapterInputs`, a credential or a setting value (decision 0164, owner ruling D1): `ArmingBlock` is
//! `vike-config`'s and a hub client may not name it, so it crosses as its `word()` and the consumer maps
//! it back with an exhaustive `match` that sends an UNKNOWN word to "adapter failed", never to armed.
//!
//! Every mirror of an existing type that has no serde ([`WireFeeSchedule`], [`GridSource`]) has two
//! EXHAUSTIVE conversions in `crate::trade::mirror`, so a new variant of the original fails to COMPILE
//! there until the wire decides what to call it. Later fields are `#[serde(default)]`: a reply may
//! grow one and an older client still reads it (no `deny_unknown_fields` on a reply).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vike_model::{AccountTier, ContractValue, HaltAdmit, MarginMode, SymbolProperties};

// ---- Arming --------------------------------------------------------------------------------------

/// The answer to [`Request::ArmingQuery`](super::Request::ArmingQuery): the static facts of every
/// venue the daemon carries and one row per `(venue, account)` it would run. Fetched ONCE per
/// assembly; the tradehub's overlay (`PreflightFailed` above `SidecarHeldElsewhere`) is a pure
/// function over it and stays tradehub-side.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ArmingTable {
    /// Static facts per roster venue slug.
    pub facts: BTreeMap<String, VenueFacts>,
    /// One row per account, in the daemon's order.
    pub rows: Vec<ArmingRow>,
    /// The ROUTE KEYS (`venue`, or `venue#LABEL`) of the accounts whose adapter this daemon started with a
    /// key set that differs from the one the store holds now: the session is still signing with the old
    /// key, and only a restart of that account changes it. Words only, never a fingerprint or a value.
    /// Absent (an older daemon, or none changed) writes no key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_changed: Vec<String>,
}

/// Where a venue arm gets a grid for a symbol other than the started one (the mirror of
/// `vike_bridge_core::DeclaredGridSource`, which this crate may not name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridSource {
    /// The arm already holds a table covering every symbol of the venue.
    InHand,
    /// The only per-symbol source is a blocking, symbol-scoped round trip.
    PerSymbolFetch,
    /// The arm fetches no instrument grid at all.
    NoGrid,
}

/// The static, credential-free facts of one venue's arm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VenueFacts {
    /// `false` = the daemon build carries no adapter for the venue (`ArmingBlock::FeatureAbsent`).
    pub adapter_present: bool,
    /// Whether the arm addresses more than one account (labelled accounts).
    pub addresses_accounts: bool,
    /// Whether the arm takes the reconcile trigger channel (`AdapterStart.takes_trigger`).
    pub takes_recon_trigger: bool,
    /// Where a declared leg's grid comes from.
    pub grid_source: GridSource,
    /// The process-exclusive resource the arm claims (a JForex sidecar), if any.
    #[serde(default)]
    pub process_exclusive: Option<String>,
    /// The venue's clock declaration word (the preflight's four-outcome clock leg reads it).
    #[serde(default)]
    pub clock_decl_word: String,
}

/// One account's arming, the mirror of `vike_config::VenueArming` plus the book name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmingRow {
    /// The roster id.
    pub venue: String,
    /// The account label's text; `None` is the unlabelled default account.
    #[serde(default)]
    pub label: Option<String>,
    /// The tier the account table states; `None` when no active row states exactly one.
    #[serde(default)]
    pub tier: Option<AccountTier>,
    /// What the account would actually run at; `None` = NOT RUNNING (decision 0156).
    #[serde(default)]
    pub effective: Option<AccountTier>,
    /// `ArmingBlock::word()`. An UNKNOWN word from a newer daemon is "adapter failed" at the consumer.
    pub block_word: String,
    /// The active `account.id`s that stated `tier`.
    #[serde(default)]
    pub account_ids: Vec<i64>,
    /// The trading book the account reaches, computed at the daemon (`book_of_account`).
    #[serde(default)]
    pub book: Option<String>,
}

// ---- Preflight -----------------------------------------------------------------------------------

/// A bridge-free network reading (the mirror of `vike_bridge_core::NetProbeHandle`'s answer): the
/// tradehub's `check_network` takes this instead of a handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetReading {
    /// The probe has not completed a round yet (or is not wired).
    Unknown,
    /// The resolver and the egress answer.
    Alive,
    /// They do not, and why (never a URL or a credential).
    Dead(String),
}

/// The clock leg of one venue: a reading, or the reason there is none (the preflight's outcomes 1, 2
/// and the two declared ones).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockLeg {
    /// The venue answered: its clock, the daemon's clock at the read and the round trip. The consumer
    /// judges `server_ms` itself (a [`PreflightStep::Clock`] question is ONE read, asked inside the
    /// consumer's own bracket on the same host); the other two are for a reader of the batch.
    ServerTime {
        /// The venue's server time, epoch ms.
        server_ms: i64,
        /// The daemon's clock at the read, epoch ms.
        local_ms: i64,
        /// The round trip of the read, ms.
        rtt_ms: u64,
    },
    /// The venue publishes a clock and this attempt got none (the venue's error text).
    Unreachable(String),
    /// Declared, nothing at stake.
    NotChecked,
    /// Declared, but the venue's auth binds the clock into the order path.
    UnmeasuredRisk,
}

/// The credential leg of one account: a cheap authenticated read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialLeg {
    /// The credentials signed and were accepted.
    Accepted,
    /// The venue ANSWERED and refused (or a definite transport error): a FAIL. Venue error text,
    /// never a key.
    Rejected(String),
    /// No answer inside the bound: not evidence about the credentials.
    Unanswered {
        /// How long the daemon waited, ms.
        waited_ms: u64,
        /// What the wait was on, in operator language.
        detail: String,
    },
}

/// **Which question a [`Request::PreflightQuery`](super::Request::PreflightQuery) asks.**
///
/// The tradehub's `run_preflight` owns the schedule: it re-reads a venue clock while the reading's
/// ±rtt/2 band straddles a threshold (the tightest round trip wins) and interleaves the clock and
/// credential budgets between reads, so one batch measured at the daemon cannot stand in for it. The
/// daemon therefore only MEASURES, one read per question; the caller decides how many and when. The
/// daemon and the tradehub share one host (the gatewayhub binds loopback only), so a venue stamp the
/// daemon read inside the caller's own bracket is comparable with the caller's clock as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PreflightStep {
    /// Every leg of the venues once (a clock, a credential read per probed venue, the network): the
    /// meaning of a frame that names no step.
    #[default]
    Batch,
    /// No measurement: the venue lists the caller's plan needs ([`PreflightLists`]).
    Plan,
    /// ONE bounded clock read of each venue named: a [`LegResult::Clock`] each.
    Clock,
    /// ONE bounded credential read of each venue named: a [`LegResult::Credential`] each.
    Credential,
    /// The daemon's network reading (the venues are ignored): one [`LegResult::Network`].
    Network,
}

/// What a drifted clock costs at a venue (the mirror of `vike_bridge_core::venue_adapter::ClockRisk`,
/// which this crate may not name). The thresholds are the TRADEHUB's judgement over this fact, never
/// the daemon's: only a signed-timestamp venue may FAIL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockRiskClass {
    /// Every private request is signed with a timestamp and rejected outside a recv window.
    SignedTimestamp,
    /// The clock is bound into the order nonce; the cliff is a day away.
    NonceWindow,
    /// Auth carries no per-request timestamp; the leg is a host-health canary.
    NoTimestamp,
}

/// A wired venue's declared clock risk: the class and the remedy sentence for a skew past the warn
/// line, byte for byte the bridge's (`ClockRisk::remedy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockRiskInfo {
    /// The class.
    pub class: ClockRiskClass,
    /// What to DO about a measured skew at this venue, in its own terms.
    pub remedy: String,
}

/// The venue lists of the preflight plan, answered to [`PreflightStep::Plan`]: what the caller needs
/// before it reads anything. No I/O beyond building the credential probes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreflightLists {
    /// The declared clock risk of each venue asked that has a WIRED clock endpoint.
    #[serde(default)]
    pub clock_risks: BTreeMap<String, ClockRiskInfo>,
    /// How many of the venues asked have a wired clock endpoint: only wired reads buy time budget.
    #[serde(default)]
    pub wired_clock_reads: u32,
    /// The venues a credential read was built for, sorted: the credential leg's list IS this set.
    #[serde(default)]
    pub credential_venues: Vec<String>,
}

/// What a [`PreflightStep`] question is answered with: the legs measured and, for
/// [`PreflightStep::Plan`], the venue lists (the two fields of [`Response::PreflightLegs`](super::Response)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreflightAnswer {
    /// The legs measured, in the daemon's order.
    pub legs: Vec<LegResult>,
    /// The venue lists, for a `Plan` question; `None` for every other.
    pub plan: Option<PreflightLists>,
}

/// One preflight observation. The report text is built TRADEHUB-side from these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegResult {
    /// A venue's clock.
    Clock {
        /// The venue.
        venue: String,
        /// The reading or its absence.
        leg: ClockLeg,
        /// Why a DECLARED gap has no reading: the row's own sentence, on [`ClockLeg::NotChecked`] and
        /// [`ClockLeg::UnmeasuredRisk`] (the report prints it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// What a drifted clock costs at this venue, on [`ClockLeg::UnmeasuredRisk`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_stake: Option<String>,
    },
    /// An account's credential probe.
    Credential {
        /// The venue.
        venue: String,
        /// The account label's text; `None` is the default account.
        #[serde(default)]
        label: Option<String>,
        /// The probe's outcome.
        leg: CredentialLeg,
    },
    /// The book a venue-confirmed identity read named (`None` = the venue could not say).
    Identity {
        /// The venue.
        venue: String,
        /// The account label's text; `None` is the default account.
        #[serde(default)]
        label: Option<String>,
        /// The venue-confirmed book.
        book: Option<String>,
    },
    /// The daemon's network liveness (the daemon owns the probe).
    Network {
        /// The reading.
        reading: NetReading,
        /// Completed probe rounds, on [`NetReading::Alive`] (the report prints it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rounds: Option<u64>,
        /// Epoch ms the probe saw the internet go down, on [`NetReading::Dead`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_ms: Option<i64>,
    },
}

// ---- Adapter start -------------------------------------------------------------------------------

/// What a bridge reads from the operator's `[risk]` budget: the startup leverage and nothing else
/// (`vike_bridge_core::leverage::resolve_startup_leverage`). `vike_model::ProfileRisk` derives
/// `Deserialize` only and `vike-model` is a measured crate, so the wire carries this mirror and the
/// daemon rebuilds the profile from it; a text gate holds the field list to what the bridges read.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct AdapterRisk {
    /// `[risk] max_leverage`; `None` = the venue's own default.
    #[serde(default)]
    pub max_leverage: Option<f64>,
}

/// Why a credential-bearing start is withheld: the tradehub's two reasons to start an adapter as
/// DATA-ONLY, applied at the daemon with `AccountDirectory::without_venue_credentials`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Withhold {
    /// The venue's preflight failed.
    PreflightFailed,
    /// The venue's credentials serve a data feed only.
    DataOnly,
}

/// The start of one venue adapter over an open lease ([`TradeUp::AdapterStart`](super::TradeUp)).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterStart {
    /// The request id the [`AdapterReply`] echoes.
    pub rid: u64,
    /// The engine symbol.
    pub symbol: String,
    /// Declared leg symbols, in declaration order.
    #[serde(default)]
    pub declared_legs: Vec<String>,
    /// The global reconcile gate.
    pub recon_enabled: bool,
    /// Whether the tradehub hands in a reconcile trigger (a `VenueFacts::takes_recon_trigger` venue).
    #[serde(default)]
    pub takes_trigger: bool,
    /// The `[risk]` fields a bridge reads; `None` = no profile.
    #[serde(default)]
    pub risk: Option<AdapterRisk>,
    /// The emulated-market band (`policy.market_slippage`).
    #[serde(default)]
    pub market_slippage: Option<f64>,
    /// The halt-admit mode already degraded by `vike_model::effective_halt_admit`.
    #[serde(default)]
    pub halt_admit: HaltAdmit,
    /// Start as data-only, and why.
    #[serde(default)]
    pub withhold: Option<Withhold>,
    /// **The leverage the NODE asks a venue that POSTS one (bybit, okx, binance) to hold for this
    /// account** (decision 0182): `floor(min(account.leverage, policy.max_leverage, [risk]
    /// max_leverage))`, a whole number, computed by the node because the daemon knows neither
    /// ceiling. `None` = the account states none, or its venue/tier is not in
    /// `vike_secrets::LEVERAGE_VENUES`, or the sender predates the field: the start is then exactly
    /// what it always was. An older daemon ignores the field and answers no `leverage_cap`, which the
    /// node reads as "nothing confirmed" (1x). The field takes part in the daemon's "is this the same
    /// start" comparison, so a changed figure tears the running adapter down and starts a fresh one.
    /// `None` is not written at all, so every start that asks nothing is byte-identical on the wire to
    /// the start before the field existed (the frozen `fixtures/gatewayhub_wire/v1_trade_up.jsonl`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_leverage: Option<f64>,
    /// The account's route key as the core spells an account's engine (`account_route_key`: the bare
    /// venue id for a default account). The daemon stamps its events with it; the client names the
    /// same spelling on every cursor it hands the core.
    pub route_key: String,
}

/// The answer to [`AdapterStart`]. A start that did not come up at its tier is NOT RUNNING with the
/// bridge's own refusal words; there is no paper stand-in (decision 0156).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AdapterReply {
    /// The adapter is up.
    Live {
        /// The tier the credentials actually authenticate.
        bound_tier: AccountTier,
        /// The engine symbol's grid in base units.
        #[serde(default)]
        grid: Option<SymbolProperties>,
        /// What one unit of the engine symbol's quantity is worth: its notional kind plus a per-unit
        /// factor (decision 0170). Absent (an older daemon, or a venue that reports none) reads `None`,
        /// which is `Linear` and 1.0; a client that must not run a venue at the wrong kind checks the
        /// daemon advertised [`FEATURE_GW_TRADE_CONTRACT_VALUE`](super::FEATURE_GW_TRADE_CONTRACT_VALUE).
        #[serde(default)]
        contract_value: Option<ContractValue>,
        /// The engine symbol's ruling margin mode.
        #[serde(default)]
        margin_mode: Option<MarginMode>,
        /// Declared legs' grids the arm already holds, in declaration order.
        #[serde(default)]
        leg_grids: Vec<(String, SymbolProperties)>,
        /// Whether a reconcile handle exists on this lease ([`TradeUp::Recon`](super::TradeUp)).
        #[serde(default)]
        recon: bool,
        /// **The leverage the VENUE confirmed for the engine symbol** (decision 0181): present only when
        /// the daemon read an `account.leverage` on this account's row at start. `Ok(x)` = the most the
        /// venue lets this account margin the symbol at right now (hyperliquid: the lower of `meta`'s
        /// `maxLeverage` and the account's own cross leverage from `activeAssetData`); `Err(why)` = the
        /// venue could not confirm, in the operator's words. `None` = no leverage was stated, the
        /// venue has no confirmation, or an older daemon: the node then margins at 1x, as it always did.
        #[serde(default)]
        leverage_cap: Option<Result<f64, String>>,
        /// **The currency the account's money is in, as the VENUE states it** (decision 0183, item 1:
        /// OANDA's account summary): the label the node anchors the engine's ledger to, instead of the
        /// constructor's `"USDT"` that nothing sets. `None` = the venue states none, the daemon could not
        /// read it, or it predates the field: the engine keeps the label it has. Never written when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_ccy: Option<String>,
    },
    /// The account does not run.
    NotRunning {
        /// `ArmingBlock::word()`; an UNKNOWN word is "adapter failed" at the consumer.
        block_word: String,
        /// The refusal text, byte for byte the bridge's.
        refusal_text: String,
    },
}

/// The venue facts of one symbol, the payload of [`TradeDown::SymbolFactsReply`](super::TradeDown): what
/// the node needs to admit a symbol of the lease's exchange into the account's engine
/// (`Command::AdmitSymbol`, decision 0173). The same two facts `AdapterReply::Live` carries for the engine
/// symbol.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WireSymbolFacts {
    /// The symbol's grid and minimums, in base units.
    pub properties: SymbolProperties,
    /// What one unit of quantity is worth; `None` = `Linear` and 1.0 (spot, and every linear perpetual).
    #[serde(default)]
    pub contract_value: Option<ContractValue>,
    /// **The leverage the VENUE was SEEN holding for this symbol after the daemon made it hold the
    /// account's figure** (a venue that posts one: bybit, okx, binance; decision 0182's per-symbol
    /// twin). Set only on a COMMIT (`TradeUp::SymbolFacts { probe: false }`, the node's "I admitted
    /// it"), never on a look, which must change nothing at the venue; and only when the account's
    /// adapter asked a figure at its start, the symbol is a perpetual of the venue and not the mounted
    /// one, the read-only probe passed, and a bounded poll then read the venue holding exactly that
    /// whole number. `None` = anything else: nothing was asked, a refusal or failure (the daemon's
    /// log has the reason), a spot symbol, an older daemon. The node then leaves the symbol at the
    /// engine's rate, as it always did; an older node ignores the field. `None` is not written at
    /// all, so every reply that confirms nothing is byte-identical on the wire to the reply before
    /// the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leverage: Option<f64>,
    /// **The currency the symbol is QUOTED in, when that is not the account's own** (decision 0183, phase 1:
    /// an OANDA instrument such as `EUR_GBP` on a USD account). Set only for a request that asked for it
    /// (`TradeUp::SymbolFacts::quoted`, a node that converts a quote currency), and only with a rate
    /// beside it: a daemon that cannot state the rate REFUSES the symbol instead (no rate, no admission).
    /// `None` = the symbol is quoted in the account's own currency (or the venue is not one that
    /// converts), which is every answer an older daemon gives, so the node then reads it as it always did.
    /// Never written when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_ccy: Option<String>,
    /// **Account-currency units per ONE unit of [`Self::quote_ccy`]** (EUR_GBP on a USD account: about
    /// `1.35`, USD_JPY: about `1/150`, never `150`), as the venue states it for the account. Present
    /// exactly when [`Self::quote_ccy`] is. The node refuses a symbol whose rate is missing, not finite
    /// and positive, or not fresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_rate: Option<f64>,
    /// When [`Self::quote_rate`] was asked of the venue, epoch ms on the DAEMON's clock (the moment the
    /// request was made: the rate is no newer than that). Present exactly when [`Self::quote_ccy`] is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_as_of_ms: Option<i64>,
}

// ---- Reconcile ------------------------------------------------------------------------------------

/// What a [`TradeUp::Recon`](super::TradeUp) asks for: the six `ReconClient` fetches. (`fetch_mass_status`
/// is the trait's own composition of three of them and crosses as those three, in the trait's order.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReconKind {
    /// `fetch_order_status_reports(since)`.
    Orders {
        /// Window start, epoch ms.
        since: i64,
    },
    /// `fetch_fill_reports(since)`.
    Fills {
        /// Window start, epoch ms.
        since: i64,
    },
    /// `fetch_position_status_reports()`.
    Positions,
    /// `fetch_balance()`.
    Balance,
    /// `fetch_fee_rates()`.
    FeeRates,
    /// `fetch_account_identity()`.
    AccountIdentity,
}

/// `vike_model::FeeSchedule` has no serde: this mirrors it variant for variant.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum WireFeeSchedule {
    /// `FeeSchedule::PercentMakerTaker`.
    PercentMakerTaker {
        /// Maker rate, basis points of quote notional.
        maker_bps: f64,
        /// Taker rate, basis points of quote notional.
        taker_bps: f64,
    },
    /// `FeeSchedule::PerShareWithFloor`.
    PerShareWithFloor {
        /// Fee per share.
        per_share: f64,
        /// Minimum fee.
        min: f64,
        /// Cap as a percent of notional.
        max_pct: f64,
    },
    /// `FeeSchedule::PercentOfUnderlying`.
    PercentOfUnderlying {
        /// Basis points of the underlying notional.
        bps: f64,
        /// Cap as a percent of the premium notional.
        premium_cap_pct: f64,
    },
    /// `FeeSchedule::ProbabilityScaled`.
    ProbabilityScaled {
        /// Taker rate.
        taker_rate: f64,
        /// Maker rate.
        maker_rate: f64,
        /// Maker rebate share.
        maker_rebate_share: f64,
    },
    /// `FeeSchedule::Free`.
    Free,
}

/// One page of a reconcile answer. Rows are the `vike-model` report types (their serde shape is the
/// journal's and pinned there).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReconPayload {
    /// [`ReconKind::Orders`].
    Orders(Vec<vike_model::OrderStatusReport>),
    /// [`ReconKind::Fills`].
    Fills(Vec<vike_model::FillReport>),
    /// [`ReconKind::Positions`].
    Positions(Vec<vike_model::PositionStatusReport>),
    /// [`ReconKind::Balance`]; `None` = the venue surfaces none.
    Balance(Option<f64>),
    /// [`ReconKind::FeeRates`]; `None` = not wired for this venue.
    FeeRates(Option<WireFeeSchedule>),
    /// [`ReconKind::AccountIdentity`]; `None` = not wired for this venue.
    AccountIdentity(Option<String>),
}

impl ReconPayload {
    /// The kind's name, for a mismatch message.
    pub fn kind(&self) -> &'static str {
        match self {
            ReconPayload::Orders(_) => "Orders",
            ReconPayload::Fills(_) => "Fills",
            ReconPayload::Positions(_) => "Positions",
            ReconPayload::Balance(_) => "Balance",
            ReconPayload::FeeRates(_) => "FeeRates",
            ReconPayload::AccountIdentity(_) => "AccountIdentity",
        }
    }

    /// Append `later` to these rows when both are the same LIST kind; `Err` otherwise (a scalar kind
    /// has exactly one page, and a page of another kind is a desync).
    pub fn extend(&mut self, later: ReconPayload) -> Result<(), String> {
        match (self, later) {
            (ReconPayload::Orders(a), ReconPayload::Orders(b)) => a.extend(b),
            (ReconPayload::Fills(a), ReconPayload::Fills(b)) => a.extend(b),
            (ReconPayload::Positions(a), ReconPayload::Positions(b)) => a.extend(b),
            (mine, theirs) => {
                return Err(format!(
                    "a {} page followed a {} page of the same reconcile fetch (only the list kinds page)",
                    theirs.kind(),
                    mine.kind()
                ));
            }
        }
        Ok(())
    }
}

/// The daemon's own account of a lease, for the status verb: names and counters, never values.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TradeStatusReport {
    /// Whether an adapter is up on this lease.
    #[serde(default)]
    pub adapter_up: bool,
    /// The last event `seq` the daemon assigned for the account.
    #[serde(default)]
    pub last_seq: u64,
    /// The oldest `seq` the ring still holds.
    #[serde(default)]
    pub ring_floor: u64,
    /// Commands received through the lease since it opened.
    #[serde(default)]
    pub commands_received: u64,
}
