//! The MIRROR DTOs of the trade wire: what the gatewayhub answers about arming, preflight and an
//! adapter start, as plain serde data with no bridge, no `vike-bridge-core`, no `vike-config` and no
//! `vike-secrets` behind it.
//!
//! The tradehub's venue seam (`VenueBackend`, wave 2's W9a/W11) is written against these. The wire
//! carries WORDS, grids, opaque handles and static facts - **never** a `Resolution`, an
//! `AdapterInputs`, a credential or a setting value (decision 0164, owner ruling D1): an `ArmingBlock`
//! (`vike_model::ArmingBlock`) crosses as its `word()` and the consumer maps it back with
//! `ArmingBlock::from_word`, sending an UNKNOWN word to "adapter failed", never to armed.
//!
//! Every mirror of an existing type is converted by EXHAUSTIVE `match`es, so a new variant of the
//! original fails to COMPILE there until the wire decides what to call it. An EXACT twin of a
//! `vike-model` type is not mirrored: `vike_model::FeeSchedule`, `vike_model::GridSource`,
//! `vike_model::ClockRisk`, `vike_model::Withhold` and a venue's static facts,
//! `vike_model::VenueAdapterDeclarationView` (with its `ClockDeclarationKind` word), cross as they are,
//! and the fixture gate pins their variants (`tests/wire_tag_fixtures.rs`'s `MODEL_FEES_SOURCE`, `GRID_SOURCE`
//! and `DECLARATION_SOURCE`). Later fields are `#[serde(default)]`: a reply may grow one and an older
//! client still reads it (no `deny_unknown_fields` on a reply).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vike_model::{
    AccountTier, ClockRisk, ContractValue, HaltAdmit, MarginMode, SymbolProperties,
    VenueAdapterDeclarationView, Withhold,
};

// ---- Arming --------------------------------------------------------------------------------------

/// The answer to [`Request::ArmingQuery`](super::Request::ArmingQuery): the static facts of every
/// venue the daemon carries and one row per `(venue, account)` it would run. Fetched ONCE per
/// assembly; the tradehub's overlay (`PreflightFailed` above `SidecarHeldElsewhere`) is a pure
/// function over it and stays tradehub-side.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ArmingTableWire {
    /// Static properties per roster venue slug. The clock word's first golden line
    /// (`fixtures/gatewayhub_wire/v1_responses.jsonl`) carries words no daemon writes; it stays, and
    /// a later line carries the three a daemon writes
    /// (`crates/vike-gatewayhub/src/trade/adapters/arming.rs`'s `facts_of`).
    pub properties: BTreeMap<String, VenueAdapterDeclarationView>,
    /// One row per account, in the daemon's order.
    pub rows: Vec<ArmingRowWire>,
    /// The ROUTE KEYS (`venue`, or `venue#LABEL`) of the accounts whose adapter this daemon started with a
    /// key set that differs from the one the store holds now: the session is still signing with the old
    /// key, and only a restart of that account changes it. Words only, never a fingerprint or a value.
    /// Absent (an older daemon, or none changed) writes no key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_changed: Vec<String>,
    /// Route key -> **a COUNTER that moves when something a start of that account reads changed in the
    /// store**: its key set (a value replaced in place keeps its row, so the rows cannot show it), its venue's
    /// settings, the deployment rows and the flags. A number that counts, never a fingerprint or a value: the
    /// same key written back does not bring the old number back, and a daemon restart starts elsewhere.
    /// Absent (an older daemon) writes no key, and then nothing is asked again on its word.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub start_inputs: BTreeMap<String, u64>,
}

/// One account's arming, the mirror of `vike_config::ArmingAccount` plus the book name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmingRowWire {
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
pub enum NetReadingWire {
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
pub enum ServerTimeGapWire {
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
pub enum CredentialGapWire {
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
    /// No measurement: the venue lists the caller's plan needs ([`PreflightPlanWire`]).
    Plan,
    /// ONE bounded clock read of each venue named: a [`PreflightLegWire::Clock`] each.
    Clock,
    /// ONE bounded credential read of each venue named: a [`PreflightLegWire::Credential`] each.
    Credential,
    /// The daemon's network reading (the venues are ignored): one [`PreflightLegWire::Network`].
    Network,
}

/// A wired venue's declared clock risk: the class and the remedy sentence for a skew past the warn
/// line, byte for byte the bridge's (`ClockRisk::remedy`). The thresholds are the TRADEHUB's judgement
/// over the class, never the daemon's: only a signed-timestamp venue may FAIL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockRiskInfoWire {
    /// The class.
    pub class: ClockRisk,
    /// What to DO about a measured skew at this venue, in its own terms.
    pub remedy: String,
}

/// The venue lists of the preflight plan, answered to [`PreflightStep::Plan`]: what the caller needs
/// before it reads anything. No I/O beyond building the credential probes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreflightPlanWire {
    /// The declared clock risk of each venue asked that has a WIRED clock endpoint.
    #[serde(default)]
    pub clock_risks: BTreeMap<String, ClockRiskInfoWire>,
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
    pub legs: Vec<PreflightLegWire>,
    /// The venue lists, for a `Plan` question; `None` for every other.
    pub plan: Option<PreflightPlanWire>,
}

/// One preflight observation. The report text is built TRADEHUB-side from these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreflightLegWire {
    /// A venue's clock.
    Clock {
        /// The venue.
        venue: String,
        /// The reading or its absence.
        leg: ServerTimeGapWire,
        /// Why a DECLARED gap has no reading: the row's own sentence, on [`ServerTimeGapWire::NotChecked`] and
        /// [`ServerTimeGapWire::UnmeasuredRisk`] (the report prints it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// What a drifted clock costs at this venue, on [`ServerTimeGapWire::UnmeasuredRisk`].
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
        leg: CredentialGapWire,
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
        reading: NetReadingWire,
        /// Completed probe rounds, on [`NetReadingWire::Alive`] (the report prints it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rounds: Option<u64>,
        /// Epoch ms the probe saw the internet go down, on [`NetReadingWire::Dead`].
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
    pub reconcile_enabled: bool,
    /// Whether the tradehub hands in a reconcile trigger (a
    /// `vike_model::VenueAdapterDeclarationView::takes_reconcile_trigger` venue).
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
    /// **The node asks the daemon to READ the account's currency from the venue at this start, at the cost of
    /// the venue's own budget** (decision 0183): `true` only for a venue whose statement of the currency
    /// costs a connection of a scarce budget (IBKR's data lane: four dials an hour per gateway; IG's login and
    /// `GET /accounts` out of the account's request allowance), and only for
    /// an account the node will convert a quote currency for (the venue and tier the rule names, an exposure
    /// ceiling armed). `false` is every other start, which then reads and costs exactly what it did before the
    /// field existed: the account that never converts spends no dial on a currency it would not use. A venue
    /// that states the currency for free (OANDA's account summary) ignores the field. A sender that predates
    /// the field is `false`. The field takes part in the daemon's "is this the same start" comparison. `false`
    /// is not written at all, so every start that asks nothing is byte-identical on the wire to the start
    /// before the field existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_account_currency: bool,
    /// The account's route key as the core spells an account's engine (`route_key_of`: the bare
    /// venue id for a default account). The daemon stamps its events with it; the client names the
    /// same spelling on every cursor it hands the core.
    pub route_key: String,
}

/// The answer to [`AdapterStart`]. A start that did not come up at its tier is NOT RUNNING with the
/// bridge's own refusal words; there is no paper stand-in (decision 0156).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AdapterReply {
    /// The adapter is up.
    Running {
        /// The tier the credentials actually authenticate.
        bound_tier: AccountTier,
        /// The engine symbol's grid in base units.
        #[serde(default)]
        grid: Option<SymbolProperties>,
        /// What one unit of the engine symbol's quantity is worth: its notional kind plus a per-unit
        /// factor (decision 0170). Absent (an older daemon, or a venue that reports none) reads `None`,
        /// which is `Linear` and 1.0; a client that must not run a venue at the wrong kind checks the
        /// daemon advertised [`FEATURE_GATEWAY_TRADE_CONTRACT_VALUE`](super::FEATURE_GATEWAY_TRADE_CONTRACT_VALUE).
        #[serde(default)]
        contract_value: Option<ContractValue>,
        /// The engine symbol's ruling margin mode.
        #[serde(default)]
        margin_mode: Option<MarginMode>,
        /// Declared legs' grids the arm already holds, in declaration order.
        #[serde(default)]
        leg_grids: Vec<(String, SymbolProperties)>,
        /// Whether a reconcile handle exists on this lease ([`TradeUp::Reconcile`](super::TradeUp)).
        #[serde(default)]
        reconcile: bool,
        /// **The leverage the VENUE confirmed for the engine symbol** (decision 0181): present only when
        /// the daemon read an `account.leverage` on this account's row at start. `Ok(x)` = the most the
        /// venue lets this account margin the symbol at right now (hyperliquid: the lower of `meta`'s
        /// `maxLeverage` and the account's own cross leverage from `activeAssetData`); `Err(why)` = the
        /// venue could not confirm, in the operator's words. `None` = no leverage was stated, the
        /// venue has no confirmation, or an older daemon: the node then margins at 1x, as it always did.
        #[serde(default)]
        leverage_cap: Option<Result<f64, String>>,
        /// **The currency the account's money is in, as the VENUE states it** (decision 0183, item 1:
        /// OANDA's account summary, cTrader's deposit asset, IBKR's base currency, the Dukascopy jar's
        /// `ready` line, IG's account list, FXCM's `BASE_CRNCY` property): the label the node anchors the
        /// engine's ledger to, instead of the constructor's `"USDT"` that nothing sets. `None` = the venue
        /// states none, the daemon could not read it, or it predates the field: the engine keeps the label it
        /// has. Never written when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_currency: Option<String>,
        /// **`true` when `account_currency` is an ASSUMPTION and not the venue's word** (FXCM: its API
        /// carries the account's currency in a system property, and when that cannot be read the owner's
        /// ruling of 2026-10-19 fills in USD): the node uses it exactly like a stated currency, for the
        /// accounts that may convert (a `demo` tier with an exposure ceiling), and says it is assumed, at
        /// `warn`, wherever it anchors a ledger to it. `false` (every venue that states its currency, and a
        /// sender that predates the field) is not written at all, so a reply that assumes nothing is
        /// byte-identical on the wire to the reply before the field existed.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        account_currency_assumed: bool,
        /// **`true` when this account's VENUE holds a TIME-only release itself** (decision 0267's 2.6: IBKR's
        /// `goodAfterTime` on its socket backend), so the node may send such an order at once instead of
        /// holding it to its time. `false` (every other venue, and a sender that predates the field: the core
        /// holds every release, as before) is not written at all, so the reply is byte-identical on the wire
        /// to the reply before the field existed.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        native_time_release: bool,
        /// **The LENGTH of the broker code the account's bridge stamps INTO an order client id** (decision 0270:
        /// Binance's Link code, `x-<code>-<id>`; `TradeLane::broker_code_len`), so the node fits the engine's id
        /// shape to it. The length only: the code itself never crosses the hop. `0` (no code, a venue whose
        /// attribution is not an order client id prefix, and a sender that predates the field: the node fits for no code) is
        /// not written at all, so the reply is byte-identical on the wire to the reply before the field existed.
        #[serde(default, skip_serializing_if = "is_zero")]
        broker_code_len: u8,
    },
    /// The account does not run.
    NotRunning {
        /// `ArmingBlock::word()`; an UNKNOWN word is "adapter failed" at the consumer.
        block_word: String,
        /// The refusal text, byte for byte the bridge's.
        refusal_text: String,
    },
}

/// The venue facts of one symbol, the payload of [`TradeDown::SymbolPropertiesReply`](super::TradeDown): what
/// the node needs to admit a symbol of the lease's exchange into the account's engine
/// (`Command::AdmitSymbol`, decision 0173). The same two facts `AdapterReply::Running` carries for the engine
/// symbol.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SymbolPropertiesWire {
    /// The symbol's grid and minimums, in base units.
    pub properties: SymbolProperties,
    /// What one unit of quantity is worth; `None` = `Linear` and 1.0 (spot, and every linear perpetual).
    #[serde(default)]
    pub contract_value: Option<ContractValue>,
    /// **The leverage the VENUE was SEEN holding for this symbol after the daemon made it hold the
    /// account's figure** (a venue that posts one: bybit, okx, binance; decision 0182's per-symbol
    /// twin). Set only on a COMMIT (`TradeUp::SymbolProperties { probe: false }`, the node's "I admitted
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
    /// an OANDA instrument such as `EUR_GBP` on a USD account, a cTrader or Dukascopy pair, an IBKR or IG
    /// market, an FXCM pair). Set only for a request that asked for it
    /// (`TradeUp::SymbolProperties::quoted`, a node that converts a quote currency), and only with a rate
    /// beside it: a daemon that cannot state the rate REFUSES the symbol instead (no rate, no admission).
    /// `None` = the symbol is quoted in the account's own currency (or the venue is not one that
    /// converts), which is every answer an older daemon gives, so the node then reads it as it always did.
    /// Never written when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_currency: Option<String>,
    /// **Account-currency units per ONE unit of [`Self::quote_currency`]** (EUR_GBP on a USD account: about
    /// `1.35`, USD_JPY: about `1/150`, never `150`), as the venue states it for the account. Present
    /// exactly when [`Self::quote_currency`] is. The node refuses a symbol whose rate is missing, not finite
    /// and positive, or not fresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_rate: Option<f64>,
    /// When [`Self::quote_rate`] was asked of the venue, epoch ms on the DAEMON's clock (the moment the
    /// request was made: the rate is no newer than that). Present exactly when [`Self::quote_currency`] is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_as_of_ms: Option<i64>,
    /// **The ledger currency this answer is expressed in** (decision 0183): the node refuses an answer whose
    /// ledger currency is not the one its engine was anchored to, so a rate (or the notional of a symbol in the
    /// account's own money) in a currency the engine does not count in is never booked. IBKR states it beside a
    /// rate only (the account's base currency as the daemon read it for this very rate); OANDA, cTrader and FXCM
    /// state it on every answer a converting node asks for (the home or deposit currency of the listing, or the
    /// currency the session decided, that the answer was read against). Absent is "no statement" (an older
    /// daemon, ig, dukascopy), which the node reads as it always did. Never written when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_currency: Option<String>,
    /// **`true` when the VENUE itself named [`Self::account_currency`]** as the account's currency (oanda's account
    /// summary, ctrader's asset list, fxcm's `BASE_CRNCY`), and not the daemon's USD default
    /// (decision 0189: where the venue states none or the read fails, the lane assumes USD and stamps `USD`
    /// exactly as a stated one is stamped). The stamp alone proves nothing about an ASSUMED engine; this flag is
    /// the proof, so the node can lift the hold it put on that engine's venue balance when the venue confirms
    /// the very currency the engine only assumed, without restarting the account. `false` = not proven (an older
    /// daemon, a venue that states no ledger, an assumed currency): the node then reads the answer as it always
    /// did and lifts nothing. Never written when `false`, so every reply that proves nothing is byte-identical on
    /// the wire to the reply before the field existed; an older node ignores the key.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub account_currency_stated: bool,
    /// **The symbol's own INITIAL-MARGIN rate, as the VENUE states it for the symbol** (`1 / leverage`: 0.0333 is
    /// 30x), for a venue whose margin is a per-instrument fact the account cannot choose (IG's
    /// `instrument.marginFactor`). The node writes it as the admission's `initial_margin_requirement` when no leverage
    /// figure a commit confirmed (`Self::leverage`, a venue that POSTS the account's own leverage) says better.
    /// `None` = the venue states none for this symbol (every venue but IG, and an IG epic whose factor is absent,
    /// not a percentage or not a rate): the engine keeps its own rate, as it always did. Never written when
    /// absent, so every answer that states nothing is byte-identical to the answer before the field existed, and
    /// an older node ignores the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin_rate: Option<f64>,
    /// **The symbol these facts are FOR, when the daemon spelled it other than the node asked** (Polymarket: the
    /// node asked with a market NAME, `some-market:Yes`, and the daemon resolved it to the outcome's token id; the
    /// facts are the token's). The node admits and orders under THIS spelling, so the one position keeps one name.
    /// `None` = the asked symbol is the symbol (every answer of every other venue, and of a token id).
    /// Never written when absent, so every other reply is byte-identical to the reply before the field existed,
    /// and an older node ignores the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,

    /// **The margin mode a position on this symbol opens in, when it is not `Cross`** (decision 0218):
    /// `Isolated` for a perpetual the venue lets an account hold in isolated margin only (Hyperliquid's
    /// `onlyIsolated`, `strictIsolated`, `noCross`), `Cash` for a fully funded spot pair. The node writes it
    /// as the admission's `margin_mode`, so the engine books the symbol's positions in the pool the venue
    /// really holds them in. `None` = cross (every symbol of every other venue, and an older daemon).
    ///
    /// ⚠ **An answer that states one is sent ONLY to a node that said it understands it**
    /// (`TradeUp::SymbolProperties::modes`): a node that predates the field would read the symbol as cross and book an
    /// isolated position wrong, so the daemon REFUSES such a symbol by name for a node that did not ask
    /// (`crates/vike-gatewayhub/src/trade/session.rs`'s `mode_refusal`), exactly as it refuses a foreign-quoted one
    /// for a node that does not convert. Never written when absent, so every answer that states nothing is
    /// byte-identical to the answer before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub margin_mode: Option<MarginMode>,
}

// ---- Reconcile ------------------------------------------------------------------------------------

/// What a [`TradeUp::Reconcile`](super::TradeUp) asks for: the six `ReconcileClient` fetches. (`fetch_mass_status`
/// is the trait's own composition of three of them and crosses as those three, in the trait's order.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReconcileKind {
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

/// One page of a reconcile answer. Rows are the `vike-model` report types (their serde shape is the
/// journal's and pinned there).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReconcilePayload {
    /// [`ReconcileKind::Orders`].
    Orders(Vec<vike_model::OrderStatusReport>),
    /// [`ReconcileKind::Fills`].
    Fills(Vec<vike_model::FillReport>),
    /// [`ReconcileKind::Positions`].
    Positions(Vec<vike_model::PositionStatusReport>),
    /// [`ReconcileKind::Balance`]; `None` = the venue surfaces none.
    Balance(Option<f64>),
    /// [`ReconcileKind::FeeRates`]; `None` = not wired for this venue.
    FeeRates(Option<vike_model::FeeSchedule>),
    /// [`ReconcileKind::AccountIdentity`]; `None` = not wired for this venue.
    AccountIdentity(Option<String>),
}

impl ReconcilePayload {
    /// The kind's name, for a mismatch message.
    pub fn kind(&self) -> &'static str {
        match self {
            ReconcilePayload::Orders(_) => "Orders",
            ReconcilePayload::Fills(_) => "Fills",
            ReconcilePayload::Positions(_) => "Positions",
            ReconcilePayload::Balance(_) => "Balance",
            ReconcilePayload::FeeRates(_) => "FeeRates",
            ReconcilePayload::AccountIdentity(_) => "AccountIdentity",
        }
    }

    /// Append `later` to these rows when both are the same LIST kind; `Err` otherwise (a scalar kind
    /// has exactly one page, and a page of another kind is a desync).
    pub fn extend(&mut self, later: ReconcilePayload) -> Result<(), String> {
        match (self, later) {
            (ReconcilePayload::Orders(a), ReconcilePayload::Orders(b)) => a.extend(b),
            (ReconcilePayload::Fills(a), ReconcilePayload::Fills(b)) => a.extend(b),
            (ReconcilePayload::Positions(a), ReconcilePayload::Positions(b)) => a.extend(b),
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
    pub last_sequence: u64,
    /// The oldest `seq` the ring still holds.
    #[serde(default)]
    pub ring_floor: u64,
    /// Commands received through the lease since it opened.
    #[serde(default)]
    pub commands_received: u64,
    /// **The venue path's own word about itself**, for a venue whose bridge owns a path to the venue that can die
    /// while the lease stays up (Dukascopy's JForex sidecar, OANDA's transactions stream). `None` = the venue reports none (every other venue),
    /// or a daemon that predates the field: a reader asks no more of this lease, and never reads `None` as "up".
    /// Written only when present, so a report of a venue without one is byte-identical to the one an older daemon
    /// sends and an older reader ignores the key (a pure addition to a REPLY: decision 0112 verdict 3, no feature
    /// string, no version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue_link: Option<VenueLinkWire>,
}

/// One venue path's state, as the daemon's bridge states it ([`TradeStatusReport::venue_link`]). Words and counters
/// only, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VenueLinkWire {
    /// Whether the path works now.
    #[serde(default)]
    pub up: bool,
    /// While down: why, in the bridge's own words (for Dukascopy the sidecar's `fatal` reason, or the last failed
    /// restart's). Empty while up.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// While down: the restarts the bridge tried since the loss that failed or said nothing in time.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub failed_restarts: u32,
    /// While down: the wait before the bridge's next restart, in milliseconds.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retry_in_ms: u64,
    /// While down: what this costs the account's orders, in the bridge's own words. Empty = the path carries the
    /// orders and the bridge refuses them meanwhile (Dukascopy's sidecar). OANDA's transactions stream says its own
    /// (its orders go out on another path), and so does cTrader's account session (an order may wait for the new one).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub effect: String,
}

fn is_zero<T: Default + PartialEq>(n: &T) -> bool {
    *n == T::default()
}

impl VenueLinkWire {
    /// A path that works.
    #[must_use]
    pub fn working() -> Self {
        Self { up: true, ..Self::default() }
    }
}
