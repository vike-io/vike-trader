//! The TRADE SEAM: what the sessions of `crate::trade` ask of the venue half of the daemon.
//!
//! The twin of `crate::data::DataLane` and `crate::rest::RestLane`, for orders. `crate::trade` (and
//! `crate::server::trade`, its wire adapter) own the lease, the per-account event ring, the dedup set, the
//! HALT gate, the heartbeats and the back-pressure; a `TradeLane` owns the VENUE LOGIC and nothing else:
//! which accounts arm, what a venue clock and a credential probe say, and the start of one venue adapter.
//! The split is the one `docs/decisions/0165-vike-mount-splits-and-no-other-hub-gets-a-core-crate.md` draws, and it is the seam wave 2 moves the venue half of
//! `vike-tradehub-core` across: the real lane (`TradeLaneBridge`, W9b) is that crate's `LocalBackend` moved
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
//!   (`vike_model::VenueAdapterDeclarationView::takes_reconcile_trigger`); firing it forwards a
//!   `ReconcileNow` to the client.
//! * **A refusal is words, never a paper stand-in.** `AdapterRefusal` is the account NOT RUNNING with
//!   the bridge's own refusal text byte for byte (decision 0156).
//! * **The returned `ExecutionClient` obeys the trait's own contract**: `submit`/`cancel`/`modify`
//!   return at once, a dead venue path synthesizes a terminal `OrderRejected` through `events` (no order
//!   may silently vanish), and `begin_detach`/`detach` are idempotent with each other. The sessions call
//!   them from ONE thread per account.
//! * **No credential value crosses**: not in an argument, an answer, a `Debug` or a log line.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use vike_execution::recon::ReconcileClient;
use vike_execution::{EventSender, ExecutionClient};
use vike_gatewayhub_client::proto::{
    AdapterStart, ArmingTableWire, PreflightAnswer, PreflightStep, SymbolPropertiesRefusal,
    SymbolPropertiesRefusalCode, SymbolPropertiesWire, VenueLinkWire,
};
use vike_model::ArmingBlock;
use vike_model::{AccountTier, MarginMode, SymbolProperties};

use crate::session::reconnect::VenueLinkReconnectSchedule;

/// Which account a lane call is about: the venue, the account label's text (`None` is the unlabelled
/// default account, the wire's own spelling) and the tier the lease was opened at (never `paper`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeLaneTarget {
    /// A `vike_model::VENUES` slug.
    pub venue: String,
    /// The account label's text; `None` is the default account.
    pub account: Option<String>,
    /// The tier the account runs at (the lease's, already checked against `account_tier`).
    pub tier: AccountTier,
}

/// A venue adapter that came up: the facts the fold builds an engine from, and the two opaque handles
/// the session drives. The wire's `AdapterReply::Running` is made from this.
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
    pub reconcile: Option<Box<dyn ReconcileClient>>,
}

impl std::fmt::Debug for StartedAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartedAdapter")
            .field("bound_tier", &self.bound_tier)
            .field("has_recon", &self.reconcile.is_some())
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

/// **[`TradeLane::account_currency_stated`]'s answer as the wire writes it**: `AdapterReply::Running`'s
/// `account_currency` (the code as the venue wrote it, `USD` for an assumption, absent for none) and
/// `account_currency_assumed`. The one place the in-process answer meets those two fields.
#[must_use]
pub(crate) fn account_currency_wire(
    told: Option<&vike_model::AccountCurrency>,
) -> (Option<String>, bool) {
    (told.map(|c| c.code().to_string()), told.is_some_and(vike_model::AccountCurrency::is_assumed))
}

/// The venue half of the trade lane. See the module doc.
pub trait TradeLane: Send + Sync {
    /// The venues whose adapter this build carries, in advertisement order (a `vike_model::VENUES` slug
    /// each). A build fact.
    fn venues(&self) -> Vec<String>;

    /// The tier the active `account` rows state for this account, or the reason there is none. The
    /// answer of `vike_config::account_arming::account_tier` over the lane's directory.
    fn account_tier(&self, venue: &str, account: Option<&str>) -> Result<AccountTier, ArmingBlock>;

    /// The id of the active `account` row this account runs on at `tier` (the row whose keys an adapter of it
    /// signs with), or `None`: no such row, a `paper` tier, or a derived lane twin (whose row carries its parent's
    /// id). **DEFAULT** `None`: a lane that knows no rows never follows a rename.
    fn row_of(&self, venue: &str, account: Option<&str>, tier: AccountTier) -> Option<i64> {
        let _ = (venue, account, tier);
        None
    }

    /// The label the `account` row `row` of `venue` carries NOW (`Some(None)`: the default account; the operator may
    /// have renamed it since an adapter started on it), or `None` when no row of `venue` has that id (a derived lane
    /// twin is never the row). A fresh read, as `account_tier`'s. **DEFAULT** `None`.
    fn row_label(&self, venue: &str, row: i64) -> Option<Option<String>> {
        let _ = (venue, row);
        None
    }

    /// **Whether account `account` of `venue` signs with the credentials of `account` row `row` NOW** (a fresh read):
    /// the row itself under the label it carries now, or an account that INHERITS the row's keys (a derived lane twin,
    /// binance's `PERP`, carries its parent's id and keys). What a credential change of `row` reconnects (decision
    /// 0287). **DEFAULT**: the row itself, by [`Self::row_label`].
    fn signs_with_row(&self, venue: &str, account: Option<&str>, row: i64) -> bool {
        self.row_label(venue, row).is_some_and(|label| label.as_deref() == account)
    }

    /// Every venue's static facts and every account's arming row, from the lane's own account table and
    /// credential store: words, never a credential.
    fn arming_table(&self) -> ArmingTableWire;

    /// The preflight question `step` asks of `venues`, measured now. The CALLER owns the schedule (a clock
    /// read repeated while its band straddles a threshold, two budgets interleaved between reads), so a
    /// lane answers ONE measurement per question and never loops: `Clock` is one bounded read per venue
    /// named, `Credential` one per venue named, `Network` the one network reading, `Plan` no measurement at
    /// all (the venue lists the caller's plan needs), `Batch` each of those once for the venues named.
    fn preflight(&self, venues: &[String], step: PreflightStep) -> PreflightAnswer;

    /// Start one account's venue adapter. See the module doc for what `events` and `trigger` are.
    fn start_adapter(
        &self,
        target: &TradeLaneTarget,
        start: &AdapterStart,
        events: EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<StartedAdapter, AdapterRefusal>;

    /// Record which venue ACCOUNT the bound session authenticated as (a signed `fetch_account_identity`
    /// read on `recon`, when the lane built one) and fold the venue-confirmed book. Bookkeeping: an
    /// `Err` is a warning the session reports back, never a refusal.
    fn record_identity(
        &self,
        target: &TradeLaneTarget,
        bound_tier: AccountTier,
        recon: Option<&dyn ReconcileClient>,
    ) -> Result<(), String>;

    /// Whether `venue` can look an order up by ORDER CLIENT id (`ExecutionClient::confirm` re-queries the
    /// venue). The answer to `ResolveOrders` for an order the session does not track: `true` launches the
    /// re-query, `false` is `ResolveState::NoResolver` and the order stays UNKNOWN until reconcile.
    ///
    fn resolves_by_order_client_id(&self, venue: &str) -> bool;

    /// **The venue facts of ANY symbol of the target's exchange** (`TradeUp::SymbolProperties`), from PUBLIC
    /// data: no credential is read, and no adapter need be started. `symbol` is in the node's engine
    /// spelling. May block on a venue call (the session runs it on its own short-lived thread, never the
    /// lease reader's). The `Err` is the refusal as it is sent: its code, set where it was built, and the operator's
    /// sentence, verbatim. **DEFAULT** `Err`: a lane or a venue that does not answer (bybit, binance and aster do, in
    /// `TradeLaneBridge`).
    fn symbol_facts(
        &self,
        target: &TradeLaneTarget,
        symbol: &str,
    ) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
        let _ = (target, symbol);
        Err(SymbolPropertiesRefusal {
            code: SymbolPropertiesRefusalCode::Verdict,
            reason: "this venue's adapter does not answer symbol facts".to_string(),
            refusal_id: None,
        })
    }

    /// **[`Self::symbol_facts`] for a node that CONVERTS a quote currency** (`TradeUp::SymbolProperties::quoted`,
    /// decision 0183, phase 1): a symbol quoted in a currency other than the account's is not refused
    /// for that alone but answered with its quote currency and the rate that converts it
    /// (`SymbolPropertiesWire::quote_currency`, `quote_rate`, `rate_as_of_ms`), asked of the venue NOW (a rate is
    /// never served from the facts' keep time). A venue with no rate source answers exactly what
    /// [`Self::symbol_facts`] does, so its own fence stands: the **DEFAULT**, so nothing but a venue that
    /// overrides this ever hands a node a foreign-quoted symbol. May block on the venue, like
    /// [`Self::symbol_facts`].
    fn symbol_facts_quoted(
        &self,
        target: &TradeLaneTarget,
        symbol: &str,
    ) -> Result<SymbolPropertiesWire, SymbolPropertiesRefusal> {
        self.symbol_facts(target, symbol)
    }

    /// **The currency the account's money is in, as the VENUE states it** (decision 0183, item 1), read
    /// when an adapter comes up and handed to the node in `AdapterReply::Running::account_currency`, which anchors
    /// the engine's ledger to it. `None` (the **DEFAULT**) when the venue states none, the read failed, or
    /// the lane has no way to ask: the engine keeps the label it has. May block on the venue (the start
    /// already does); a failure is logged by the lane, never an error of the start. `start` is the start
    /// being answered: `start.read_account_currency` is the node asking for a currency whose statement costs the
    /// venue a scarce budget (IBKR's data lane), so a venue that pays for the read reads it only then.
    ///
    /// A read that came back EMPTY is not kept for the adapter's life: the session asks again at the next
    /// start of the same running adapter (`TradeConfig::account_currency_retry_after`).
    fn account_currency(&self, target: &TradeLaneTarget, start: &AdapterStart) -> Option<String> {
        let _ = (target, start);
        None
    }

    /// Whether this venue's statement of the account's currency is paid for out of a venue budget (ibkr's
    /// data lane, ig's allowance) and so is read only for a start that asks (`start.read_account_currency`). The
    /// session's re-ask of a currency that came back empty (`TradeConfig::account_currency_retry_after`) honours
    /// the same rule. `false` (the **DEFAULT**) for a venue whose read is free.
    fn currency_read_is_budgeted(&self, target: &TradeLaneTarget) -> bool {
        let _ = target;
        false
    }

    /// [`Self::account_currency`] together with HOW the lane knows it (`None` = it states none). It is
    /// `vike_model::AccountCurrency::Assumed` only when the venue's API could not state the currency and the lane
    /// filled one in under the owner's ruling (FXCM: USD, flagged); that travels to the node as
    /// `AdapterReply::Running::account_currency_assumed` and is never silent. The default is the plain answer,
    /// stated, so every other venue's start is exactly what it was. ⚠ A wrapper lane must forward this verb like
    /// every other (`RefreshedTrade` does, and holds it). `start` is the start being answered, the REAL one (its
    /// `symbol` is the symbol the engine is mounted on, and a venue that pays for the read gates it on the start's
    /// own asks): never a default built beside the call.
    fn account_currency_stated(
        &self,
        target: &TradeLaneTarget,
        start: &AdapterStart,
    ) -> Option<vike_model::AccountCurrency> {
        self.account_currency(target, start).map(vike_model::AccountCurrency::Stated)
    }

    /// **Whether this account's VENUE holds a TIME-only release itself** (decision 0267's 2.6: IBKR's
    /// `goodAfterTime` on its socket backend), asked at every non-restored start (a running adapter's replay too,
    /// so it is the lane's own fact, never a venue call) and handed to the node as
    /// `AdapterReply::Running::native_time_release`. `false` (the **DEFAULT**): the core holds every release, as
    /// before. ⚠ A wrapper lane must forward this verb like every other (`RefreshedTrade` does).
    fn native_time_release(&self, target: &TradeLaneTarget, start: &AdapterStart) -> bool {
        let _ = (target, start);
        false
    }

    /// **The LENGTH of the broker code this account's bridge stamps INTO an order client id** (decision 0270:
    /// Binance's Link code, `x-<code>-<id>`), read off the deployment rows the bridge's own start reads, so the
    /// node fits the engine's id shape to it; handed over as `AdapterReply::Running::broker_code_len`. The length
    /// only: the code never leaves this process and is never logged. `0` (the **DEFAULT**): no code to fit. ⚠ A
    /// wrapper lane must forward this verb like every other (`RefreshedTrade` does).
    fn broker_code_len(&self, target: &TradeLaneTarget, start: &AdapterStart) -> u8 {
        let _ = (target, start);
        0
    }

    /// **Why the venue's reconcile BALANCE must not be handed to the node for this account**, or `None` (the
    /// **DEFAULT**: it may). Asked on every balance fetch; a `Some` answers the pass `None` ("not reported"), so
    /// the node folds no balance and raises no drift, and carries the words. The case is an account whose currency
    /// the lane only ASSUMED (`account_currency_stated`'s flag): the venue's figure carries no currency, and the node
    /// adopts it into a ledger labelled with the account's currency, so a figure in the account's REAL currency
    /// would be labelled with the assumed one. A wrapper lane must forward this verb like every other.
    ///
    /// ⚠ A BELT, not the authority: it follows the last start THIS gatewayhub saw, and the node's re-teach after a
    /// gatewayhub restart re-sends the start under an engine that is not rebuilt. The authority is the node's own
    /// gate over the reconcile client it binds (`vike-tradehub`'s `venue_backend::gateway::balance_gate`), which follows
    /// what the engine was bound with and which nothing the gatewayhub reads later can open.
    fn balance_withheld(&self, target: &TradeLaneTarget) -> Option<String> {
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
        target: &TradeLaneTarget,
        start: &AdapterStart,
    ) -> Option<Result<f64, String>> {
        let _ = (target, start);
        None
    }

    /// **What the VENUE confirms of the account's stated leverage for ONE symbol the node admitted on
    /// demand** (`TradeUp::SymbolProperties` as a commit): the per-symbol twin of [`Self::leverage_cap`]'s
    /// read-back for a venue that POSTS a leverage. `start` is the running adapter's own start, whose
    /// `account_leverage` is the figure the node asked; `symbol` is in the node's engine spelling and is
    /// never the mounted one (that symbol's write and read-back belong to the start).
    ///
    /// `None` when nothing is asked: the start asked no figure, the venue/tier is not a posting row of
    /// `vike_venues::LEVERAGE_VENUES`, the account's own row no longer states it, the start was
    /// withheld, or the symbol is spot (the **DEFAULT**, so every other venue is exactly what it was).
    /// `Some(Ok(x))` the venue was SEEN holding `x`, the asked figure, after a read-only probe and (when it
    /// held another number) ONE write through the bridge's own verb and a bounded poll. `Some(Err(why))`
    /// a refusal or failure, in the operator's words (never a credential): the symbol stays admitted
    /// exactly as before and the daemon says why in its log. May block on the venue (up to the probe's
    /// reads, one write and the poll); the session runs it on its facts thread, never the lease reader's.
    fn symbol_leverage(
        &self,
        target: &TradeLaneTarget,
        start: &AdapterStart,
        symbol: &str,
    ) -> Option<Result<f64, String>> {
        let _ = (target, start, symbol);
        None
    }

    /// **An operator's READ of the target account at its venue** (`TradeUp::VenueRead`): its balance, the
    /// venue's own list of its open orders, or the positions it holds, asked NOW with the account's own
    /// credentials on the host its tier selects. READ-ONLY: it changes nothing at the venue and folds nothing into any book, and it needs
    /// no started adapter (the bridge builds a client of its own per call, as `symbol_leverage`'s write does), except
    /// ig's, which borrows the account's running order session and opens no login of its own.
    /// May block on a venue call (the session runs it on a thread of its own, never the lease reader's, and
    /// bounds how often it is asked). The `Err` is the operator's sentence, never a credential.
    ///
    /// **DEFAULT** `Err(not offered)`: a lane or a venue that implements nothing is never asked to do more
    /// (`TradeLaneBridge` forwards to the registry bridge's `VenueAdapter::venue_read`, which only polymarket,
    /// hyperliquid, deribit, bybit (the balance and the open orders), binance, okx and ig (the open orders) answer).
    /// ⚠ A wrapper lane must forward this verb like every other.
    fn venue_read(
        &self,
        target: &TradeLaneTarget,
        kind: vike_model::VenueReadKind,
    ) -> Result<vike_model::VenueRead, vike_utility::errors::CarriedRefusal> {
        Err(vike_model::not_offered_words(&target.venue, kind).into())
    }

    /// The venue session of this account's adapter that the daemon SUPERVISES, when its bridge declares one
    /// (`vike_bridge_core::venue_adapter::VenueAdapter::link_probe`, decision 0164 verdict 7(b), amended 2026-11-21:
    /// IBKR's gateway socket, FXCM's ForexConnect session): `Some` makes the session probe it, take the adapter down
    /// when it drops and bring it back while the rows say the account runs and the venue's calendar lets it log in.
    /// Called once per SUCCESSFUL start, and once per start the lane refused `venue unreachable`, which `Some` PARKS
    /// until the link answers (owner ruling 2026-11-16: IBKR's gateway not logged in yet; the session's `link` module,
    /// "Parked"). **DEFAULT `None`**: every other venue's bridge owns its reconnect, and the daemon adds none.
    fn link_watch(
        &self,
        target: &TradeLaneTarget,
        start: &AdapterStart,
    ) -> Option<Arc<dyn VenueLinkWatchSource>> {
        let _ = (target, start);
        None
    }

    /// **The venue path a bridge OWNS and can lose while its lease stays up** (Dukascopy's JForex sidecar, which
    /// the client restarts by itself; OANDA's transactions stream, which its lane reopens by itself): `Some` hands the session a reader of that path's state, which the status
    /// verb answers with ([`VenueLinkReport`], `TradeStatusReport::venue_link`) so `trade health` can say
    /// "down" instead of "running". Called once per SUCCESSFUL start. **DEFAULT `None`**: the daemon watches
    /// nothing else and reports nothing, which is every other venue's start exactly as it was. The twin of
    /// [`Self::link_watch`] for a path the bridge heals itself: it only REPORTS, it never rebuilds an adapter,
    /// ends a lease or refuses a command. ⚠ A wrapper lane must forward this verb like every other.
    fn venue_link(
        &self,
        target: &TradeLaneTarget,
        start: &AdapterStart,
    ) -> Option<VenueLinkReport> {
        let _ = (target, start);
        None
    }

    /// **The reconcile health gate's evidence** (wave 2, W13, R31): `Some(reason)` while `venue`'s price
    /// feed is mid-gap, so a `Recon` request is answered `ReconcileNotReady{reason}` (the tradehub's manager
    /// skips that pass) instead of fetching reports the venue has not caught up on. `None` is healthy and
    /// the default: a lane with no feed health to report never suppresses a pass (the fail-soft direction;
    /// this gate can only suppress). Cheap and non-blocking: the session asks it on the lease reader's
    /// thread, once per `Recon` request.
    fn reconcile_degraded(&self, venue: &str) -> Option<String> {
        let _ = venue;
        None
    }

    /// **The account's credentials changed and its adapter was just torn down to start on them** (decision 0287): a
    /// lane that remembers what an adapter started with forgets it for `target`, so it names no changed key for an
    /// account that is about to start on the new one. DEFAULT: nothing to forget. ⚠ A wrapper lane must forward it.
    fn credentials_changed(&self, target: &TradeLaneTarget) {
        let _ = target;
    }
}

/// A venue path's state as it is NOW (see [`TradeLane::venue_link`]): read on every status request, never cached.
/// `None` = the bridge cannot vouch for the path either way right now (nothing heard yet, a stale word, a shim that
/// predates the reading): the status carries no `venue_link`, so the account reads not known, never up.
pub type VenueLinkReport = Arc<dyn Fn() -> Option<VenueLinkWire> + Send + Sync>;

/// What the daemon asks of a supervised venue session (see [`TradeLane::link_watch`]). Every question is
/// answered from the world as it is NOW, never from a value read at start.
pub trait VenueLinkWatchSource: Send + Sync {
    /// Whether the venue session is reachable (one bounded probe); `Err` says why not (never a credential).
    fn reachable(&self) -> Result<(), String>;
    /// The venue session's own word that it ENDED for good (the bridge's probe:
    /// `vike_bridge_core::venue_link::VenueLinkProbe::lost`), acted on at once while the link is up. DEFAULT `None`.
    fn lost(&self) -> Option<String> {
        None
    }
    /// The venue session's own word that it is UP now (the bridge's probe:
    /// `vike_bridge_core::venue_link::VenueLinkProbe::up`): a rebuild is healthy only once it said so. DEFAULT `None`:
    /// it cannot tell, and a rebuild whose start returned is healthy at once.
    fn up(&self) -> Option<bool> {
        None
    }
    /// Why the venue's calendar holds every login NOW (an FX venue while the FX week is closed:
    /// `vike_venues::session::fx_login_closed_words` of the open), asked before every rebuild; `None` = it may log in.
    /// DEFAULT `None`.
    fn login_closed(&self) -> Option<String> {
        None
    }
    /// The tier the account rows state RIGHT NOW: a fresh read of the store on every call, through
    /// `vike_config::account_arming::account_tier`. `Err` is the block that stops the account running
    /// (inactive, no row, the venue-scoped two-tier conflict of decision 0140).
    fn tier_now(&self) -> Result<AccountTier, ArmingBlock>;
    /// The cadence and the backoff of the supervision.
    fn schedule(&self) -> VenueLinkReconnectSchedule;
    /// What a start PARKED behind this link says while the venue session does not answer yet (owner ruling
    /// 2026-11-16: `trade health` reads it). DEFAULT [`NOT_READY_WORDS`].
    fn not_ready_words(&self) -> String {
        NOT_READY_WORDS.to_string()
    }
}

/// The default of [`VenueLinkWatchSource::not_ready_words`]: words that name no venue.
pub const NOT_READY_WORDS: &str = "the venue session does not answer yet";
