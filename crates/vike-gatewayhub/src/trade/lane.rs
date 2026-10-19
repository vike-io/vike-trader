//! The TRADE SEAM: what the sessions of `crate::trade` ask of the venue half of the daemon.
//!
//! The twin of `crate::data::DataLane` and `crate::rest::RestLane`, for orders. `crate::trade` (and
//! `crate::server::trade`, its wire adapter) own the lease, the per-account event ring, the dedup set, the
//! HALT gate, the heartbeats and the back-pressure; a `TradeLane` owns the VENUE LOGIC and nothing else:
//! which accounts arm, what a venue clock and a credential probe say, and the start of one venue adapter.
//! The split is the one `docs/decisions/0165-vike-mount-splits-and-no-other-hub-gets-a-core-crate.md` draws, and it is the seam wave 2 moves the venue half of
//! `vike-tradehub-core` across: the real lane (`BridgeTrade`, W9b) is that crate's `LocalBackend` moved
//! whole, so every method below is the in-process call it makes today with the wire types for its
//! arguments and answers. Nothing here names a bridge, `vike-tradehub-core` or a credential value.
//!
//! # What a lane promises
//!
//! * **`venues` is a BUILD fact**, like `DataLane::venues`: the venues whose adapter this build carries
//!   (`trade_venue=` in `Welcome.features`), computed once at start. It never reads a socket, a venue's
//!   health or a credential.
//! * **`account_tier` is the account table's answer** (`vike_config::account_arming::account_tier` over
//!   the lane's own `AccountDirectory`), the ONE function that turns the rows into a tier. The sessions
//!   re-derive the tier a client claims at `TradeOpen` from it and refuse a mismatch; they never read a
//!   row themselves.
//! * **`arming_table` and `preflight` are PURE queries.** Nothing is started, nothing is claimed, and
//!   the only I/O is the preflight's own bounded probes. Both are called from a connection thread, so
//!   they may take as long as a probe takes and no longer (the rpc reply wait is the client's bound).
//! * **`start_adapter` starts ONE account's venue client**, the work `make_engine_for_account` does
//!   between the pre-connect budget refusal and the engine: the resolve-half tier interlock, the
//!   no-downgrade rule, the process-exclusive claim, the bridge's own `start`, the start-half interlock
//!   and the order hold. `events` is the lane the adapter's pumps push into, already scoped to the
//!   account's route key: **a lane keeps it and hands it to the bridge, and delivers nothing the
//!   bridge did not emit**. `trigger` is the reconcile-trigger channel for a venue that takes one
//!   (`VenueFacts::takes_recon_trigger`); firing it forwards a `ReconcileNow` to the client.
//! * **A refusal is words, never a paper stand-in.** `AdapterRefusal` is the account NOT RUNNING with
//!   the bridge's own refusal text byte for byte (decision 0156).
//! * **The returned `ExecutionClient` obeys the trait's own contract**: `submit`/`cancel`/`modify`
//!   return at once, a dead venue path synthesizes a terminal `OrderRejected` through `events` (no order
//!   may silently vanish), and `begin_detach`/`detach` are idempotent with each other. The sessions call
//!   them from ONE thread per account.
//! * **No credential value crosses**: not in an argument, an answer, a `Debug` or a log line.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use vike_config::ArmingBlock;
use vike_exec::recon::ReconClient;
use vike_exec::{EventSender, ExecutionClient};
use vike_gatewayhub_client::proto::{
    AdapterStart, ArmingTable, PreflightAnswer, PreflightStep, WireSymbolFacts,
};
use vike_model::{AccountTier, MarginMode, SymbolProperties};

use crate::session::reconnect::Schedule;

/// Which account a lane call is about: the venue, the account label's text (`None` is the unlabelled
/// default account, the wire's own spelling) and the tier the lease was opened at (never `paper`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeTarget {
    /// A `vike_model::VENUES` slug.
    pub venue: String,
    /// The account label's text; `None` is the default account.
    pub account: Option<String>,
    /// The tier the account runs at (the lease's, already checked against `account_tier`).
    pub tier: AccountTier,
}

/// A venue adapter that came up: the facts the fold builds an engine from, and the two opaque handles
/// the session drives. The wire's `AdapterReply::Live` is made from this.
pub struct StartedAdapter {
    /// The tier the client BOUND. The start's own interlock has already held it equal to the account's;
    /// the session checks it once more.
    pub bound_tier: AccountTier,
    /// The venue-fetched grid of the engine symbol.
    pub grid: Option<SymbolProperties>,
    /// What one unit of the engine symbol's quantity is worth (kind plus per-unit factor, decision
    /// 0170), when the venue reported one; `None` is `Linear`, 1.0.
    pub contract_value: Option<vike_model::ContractValue>,
    /// The ruling margin mode, when the venue reported one.
    pub margin_mode: Option<MarginMode>,
    /// The grids of the declared legs the venue could answer, in declaration order.
    pub leg_grids: Vec<(String, SymbolProperties)>,
    /// The bound exec client. Driven from one thread; `Send` only.
    pub client: Box<dyn ExecutionClient + Send>,
    /// The account's reconcile client, when the venue built one.
    pub recon: Option<Box<dyn ReconClient>>,
}

impl std::fmt::Debug for StartedAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartedAdapter")
            .field("bound_tier", &self.bound_tier)
            .field("has_recon", &self.recon.is_some())
            .field("contract_value", &self.contract_value)
            .field("margin_mode", &self.margin_mode)
            .field("legs", &self.leg_grids.len())
            .finish_non_exhaustive()
    }
}

/// An adapter that did not come up: the account is NOT RUNNING (decision 0156).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRefusal {
    /// `ArmingBlock::word()` of the cause (the word the wire carries as `block_word`).
    pub block_word: String,
    /// The refusal text, byte for byte the bridge's own.
    pub refusal_text: String,
}

/// The venue half of the trade lane. See the module doc.
pub trait TradeLane: Send + Sync {
    /// The venues whose adapter this build carries, in advertisement order (a `vike_model::VENUES` slug
    /// each). A build fact.
    fn venues(&self) -> Vec<String>;

    /// The tier the active `account` rows state for this account, or the reason there is none. The
    /// answer of `vike_config::account_arming::account_tier` over the lane's directory.
    fn account_tier(&self, venue: &str, account: Option<&str>) -> Result<AccountTier, ArmingBlock>;

    /// Every venue's static facts and every account's arming row, from the lane's own account table and
    /// credential store: words, never a credential.
    fn arming_table(&self) -> ArmingTable;

    /// The preflight question `step` asks of `venues`, measured now. The CALLER owns the schedule (a clock
    /// read repeated while its band straddles a threshold, two budgets interleaved between reads), so a
    /// lane answers ONE measurement per question and never loops: `Clock` is one bounded read per venue
    /// named, `Credential` one per venue named, `Network` the one network reading, `Plan` no measurement at
    /// all (the venue lists the caller's plan needs), `Batch` each of those once for the venues named.
    fn preflight(&self, venues: &[String], step: PreflightStep) -> PreflightAnswer;

    /// Start one account's venue adapter. See the module doc for what `events` and `trigger` are.
    fn start_adapter(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        events: EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<StartedAdapter, AdapterRefusal>;

    /// Record which venue ACCOUNT the bound session authenticated as (a signed `fetch_account_identity`
    /// read on `recon`, when the lane built one) and fold the venue-confirmed book. Bookkeeping: an
    /// `Err` is a warning the session reports back, never a refusal.
    fn record_identity(
        &self,
        target: &TradeTarget,
        bound_tier: AccountTier,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), String>;

    /// Whether `venue` can look an order up by CLIENT order id (`ExecutionClient::confirm` re-queries the
    /// venue). The answer to `ResolveOrders` for an order the session does not track: `true` launches the
    /// re-query, `false` is `ResolveState::NoResolver` and the order stays UNKNOWN until reconcile.
    ///
    fn resolves_by_coid(&self, venue: &str) -> bool;

    /// **The venue facts of ANY symbol of the target's exchange** (`TradeUp::SymbolFacts`), from PUBLIC
    /// data: no credential is read, and no adapter need be started. `symbol` is in the node's engine
    /// spelling. May block on a venue call (the session runs it on its own short-lived thread, never the
    /// lease reader's). The `Err` is the operator's sentence, verbatim. **DEFAULT** `Err`: a lane or a
    /// venue that does not answer (bybit, binance and aster do, in `BridgeTrade`).
    fn symbol_facts(&self, target: &TradeTarget, symbol: &str) -> Result<WireSymbolFacts, String> {
        let _ = (target, symbol);
        Err("this venue's adapter does not answer symbol facts".to_string())
    }

    /// **[`Self::symbol_facts`] for a node that CONVERTS a quote currency** (`TradeUp::SymbolFacts::quoted`,
    /// decision 0183, phase 1): a symbol quoted in a currency other than the account's is not refused
    /// for that alone but answered with its quote currency and the rate that converts it
    /// (`WireSymbolFacts::quote_ccy`, `quote_rate`, `rate_as_of_ms`), asked of the venue NOW (a rate is
    /// never served from the facts' keep time). A venue with no rate source answers exactly what
    /// [`Self::symbol_facts`] does, so its own fence stands: the **DEFAULT**, so nothing but a venue that
    /// overrides this ever hands a node a foreign-quoted symbol. May block on the venue, like
    /// [`Self::symbol_facts`].
    fn symbol_facts_quoted(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        self.symbol_facts(target, symbol)
    }

    /// **The currency the account's money is in, as the VENUE states it** (decision 0183, item 1), read
    /// when an adapter comes up and handed to the node in `AdapterReply::Live::account_ccy`, which anchors
    /// the engine's ledger to it. `None` (the **DEFAULT**) when the venue states none, the read failed, or
    /// the lane has no way to ask: the engine keeps the label it has. May block on the venue (the start
    /// already does); a failure is logged by the lane, never an error of the start.
    fn account_currency(&self, target: &TradeTarget) -> Option<String> {
        let _ = target;
        None
    }

    /// **What the VENUE confirms of the account's own stated leverage** (decision 0181), asked once per
    /// successful, non-restored start: `None` when the account row states none or the venue has no
    /// confirmation to give (the **DEFAULT**, so every other venue's start is exactly what it was, with
    /// no extra venue call); `Some(Ok(x))` the most the venue lets this account margin the engine
    /// symbol at; `Some(Err(why))` the venue could not confirm, in the operator's words (never a
    /// credential). May block on the venue (the start already does). The tradehub applies the
    /// figure only when this answers `Ok` and covers it.
    ///
    /// For a venue that POSTS a leverage (bybit, okx, binance; decision 0182) the answer is a READ-BACK
    /// instead of a cap: it is `Some` only when `start.account_leverage` asks a figure, and `Ok(x)` is the
    /// figure the venue was SEEN to hold (the tradehub applies it only when `x` equals its ask). The
    /// lane makes no write: the bridge's exec thread posts, once, at its start.
    fn leverage_cap(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
    ) -> Option<Result<f64, String>> {
        let _ = (target, start);
        None
    }

    /// **What the VENUE confirms of the account's stated leverage for ONE symbol the node admitted on
    /// demand** (`TradeUp::SymbolFacts` as a commit): the per-symbol twin of [`Self::leverage_cap`]'s
    /// read-back for a venue that POSTS a leverage. `start` is the running adapter's own start, whose
    /// `account_leverage` is the figure the node asked; `symbol` is in the node's engine spelling and is
    /// never the mounted one (that symbol's write and read-back belong to the start).
    ///
    /// `None` when nothing is asked: the start asked no figure, the venue/tier is not a posting row of
    /// `vike_secrets::LEVERAGE_VENUES`, the account's own row no longer states it, the start was
    /// withheld, or the symbol is spot (the **DEFAULT**, so every other venue is exactly what it was).
    /// `Some(Ok(x))` the venue was SEEN holding `x`, the asked figure, after a read-only probe and (when it
    /// held another number) ONE write through the bridge's own verb and a bounded poll. `Some(Err(why))`
    /// a refusal or failure, in the operator's words (never a credential): the symbol stays admitted
    /// exactly as before and the daemon says why in its log. May block on the venue (up to the probe's
    /// reads, one write and the poll); the session runs it on its facts thread, never the lease reader's.
    fn symbol_leverage(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        symbol: &str,
    ) -> Option<Result<f64, String>> {
        let _ = (target, start, symbol);
        None
    }

    /// The venue session of this account's adapter that the daemon SUPERVISES, when the venue has one
    /// (IBKR's gateway socket, decision 0164 verdict 7(b)): `Some` makes the session probe it, take the
    /// adapter down when it drops and bring it back while the rows say the account runs. Called once per
    /// SUCCESSFUL start. **DEFAULT `None`**: every other venue's bridge owns its reconnect, and the daemon
    /// adds none (a refactor changes no working behaviour).
    fn link_watch(&self, target: &TradeTarget, start: &AdapterStart) -> Option<Arc<dyn LinkWatch>> {
        let _ = (target, start);
        None
    }

    /// **The reconcile health gate's evidence** (wave 2, W13, R31): `Some(reason)` while `venue`'s price
    /// feed is mid-gap, so a `Recon` request is answered `ReconNotReady{reason}` (the tradehub's manager
    /// skips that pass) instead of fetching reports the venue has not caught up on. `None` is healthy and
    /// the default: a lane with no feed health to report never suppresses a pass (the fail-soft direction;
    /// this gate can only suppress). Cheap and non-blocking: the session asks it on the lease reader's
    /// thread, once per `Recon` request.
    fn recon_degraded(&self, venue: &str) -> Option<String> {
        let _ = venue;
        None
    }
}

/// What the daemon asks of a supervised venue session (see [`TradeLane::link_watch`]). Both questions are
/// answered from the world as it is NOW, never from a value read at start.
pub trait LinkWatch: Send + Sync {
    /// Whether the venue session is reachable (one bounded probe); `Err` says why not (never a credential).
    fn reachable(&self) -> Result<(), String>;
    /// The tier the account rows state RIGHT NOW: a fresh read of the store on every call, through
    /// `vike_config::account_arming::account_tier`. `Err` is the block that stops the account running
    /// (inactive, no row, the venue-scoped two-tier conflict of decision 0140).
    fn tier_now(&self) -> Result<AccountTier, ArmingBlock>;
    /// The cadence and the backoff of the supervision.
    fn schedule(&self) -> Schedule;
}
