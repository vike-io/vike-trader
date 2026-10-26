//! **`BridgeTrade` — the real [`TradeLane`]: the venue half of the fold over the registry of bridges**
//! (wave 2, W9a's `LocalBackend` moved whole into the gatewayhub by W9b).
//!
//! Every verb is the body the fold ran inline before the seam, behind the port with its order of
//! operations kept: the account's tier BEFORE any credential, the resolve-half tier interlock, the
//! no-downgrade rule, the one-sidecar refusal, the process-exclusive claim (kept only when the
//! resource runs, dropped otherwise), the bridge's `start`, the start-half interlock, and the order
//! hold wrapped around the bound client. What changed is the argument shape: `&AdapterPolicy` became the
//! three things the venue half reads of it (the account directory, the venue settings and the
//! deployment map, [`TradeSnapshot`] and `vars`), and the answers are the wire's mirror DTOs
//! (`vike_gatewayhub_client::proto`) instead of the fold's own types.
//!
//! It owns what the venue half reads: the registry, the credential/deployment map (`vars`) and the
//! `account`/`venue_setting` snapshot, whose `preflight_failed` is ALWAYS empty: the startup preflight's
//! stop is the tradehub's overlay over the table, and it reaches a start as [`Withhold`], never as
//! lane state — a stop baked into the table could not be told apart from the venue's own answer.
//!
//! # Two surfaces
//!
//! * [`TradeLane`] is what the sessions drive (`crate::server::trade`): batch verbs over wire values.
//! * The per-read verbs ([`BridgeTrade::preflight_plan`], [`BridgeTrade::server_time_ms`],
//!   [`BridgeTrade::authed_read`], [`BridgeTrade::net_reading`]) are the units `TradeLane::preflight`
//!   is made of, public because the in-process roster tests drive the tradehub's fold through them.
//!   `run_preflight` re-reads a clock up to `DEFAULT_CLOCK_SAMPLES` times and interleaves two legs'
//!   budgets between reads, so the daemon answers one question at a time and the caller owns the
//!   schedule: see [`BridgeTrade::preflight`] (the `TradeLane` verb).
//!
//! # `confirm` (the lookup-by-client-order-id table)
//!
//! [`TradeLane::resolves_by_coid`] is answered from [`COID_LOOKUP`], the coverage table of
//! the wave-2 plan's B.7 table (the gatewayhub's lookup-by-client-order-id coverage), one row per roster venue.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use vike_bridge_core::venue_adapter::{ClockDecl, ClockRisk};
use vike_config::{ArmingBlock, VenueArming};
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_gatewayhub_client::proto::{
    AdapterStart, ArmingTable, ClockRiskClass, ClockRiskInfo, LegResult, PreflightAnswer,
    PreflightLists, PreflightStep, WireSymbolFacts, Withhold,
};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account};
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::venue_setting::VenueSettings;

use super::assumed_ccy::{CCY_SAID, CcyRead, lister_currency, start_currency};
use super::probe::{
    CredentialGap, NetReading, ServerTimeGap, clock_result, credential_result, network_result,
};
use super::registry::{VenueRow, row_of};
use super::snapshot::TradeSnapshot;
use super::started_keys::StartedKeys;
use super::startup::{
    DEFAULT_NET_PROBE_WAIT, authed_read_probe, authed_read_probes, bounded_server_time_ms,
    clock_risks, net_reading_of, spawn_net_probe,
};
use super::{arming, book_identity, contract, server_time};
use crate::trade::ibkr::{IbkrGateway, IbkrWatch, TierNow};
use crate::trade::lane::{
    AdapterRefusal, LinkWatch, StartedAdapter, TradeLane, TradeTarget, VenueLinkReport,
};

/// A fresh read of the `account` table (rows only): what the supervised link's predicate asks before every
/// reconnect attempt. The daemon's root builds it over its credential store handle.
pub type AccountRows = Arc<dyn Fn() -> AccountDirectory + Send + Sync>;

/// One credential read, as [`authed_read_probe`] builds it from the plan's probe map.
type CredentialReader = Box<dyn Fn(&str) -> Result<(), CredentialGap> + Send + Sync>;

/// **Which venues can look an order up by CLIENT order id behind `ExecutionClient::confirm`** — the
/// coverage table of the plan's B.7, one row per `vike_model::VENUES` id, WRITTEN NEXT TO the lane
/// that answers `ResolveOrders` from it.
///
/// `true` only where the bridge builds its `ExecActor` `with_confirm(..)` over a venue query by client
/// order id: **bybit** (`crates/bridges/bybit/src/exec.rs`: order-link-id query, realtime then
/// history), **okx** (`crates/bridges/okx/src/exec.rs`: `clOrdId`), **binance** (`crates/bridges/binance/src/exec.rs`: `GET /fapi/v1/order` or `/api/v3/order` by `origClientOrderId`, with the broker link prefix), **aster** (`crates/bridges/aster/src/exec.rs`: the same query on `/fapi/v3/order` or `/api/v3/order`, bare id), **oanda** (`crates/bridges/oanda/src/exec.rs`: `GET /orders/@{coid}`), **polymarket** (`crates/bridges/polymarket/src/exec_plane/client.rs`: the derived order hash, same-epoch only), **ig**
/// (`crates/bridges/ig/src/exec.rs`: dealRef to confirms) and **fxcm** (`crates/bridges/fxcm/src/exec.rs`: the RequestID of a placement whose reply was lost, in-process only). Every other venue's `confirm` is the
/// `ExecutionClient` default or an `ExecActor` built WITHOUT `with_confirm`, a clean no-op:
///
/// | venue | `confirm` | UNKNOWN submit after a gatewayhub epoch change |
/// |---|---|---|
/// | bybit, okx, binance, aster, oanda, ig | re-queries by coid | resolved by the query (ig needs its in-process deal reference: answers nothing after a daemon restart; oanda found -> `OrderAccepted` for an order in any state, not found or an error -> silence: its `confirm` never rules an order absent, the oanda bridge page, `crates/bridges/oanda/CLAUDE.md`, has the rest) |
/// | polymarket | re-queries `GET /data/order/<derived order hash>` for the submits THIS process left UNKNOWN | same-epoch only: the unknown set is in-memory, so after a daemon restart it answers nothing; found -> `OrderAccepted`, not found -> silence |
/// | deribit | no-op: the wrapper delegates to an actor with no `with_confirm`; `resolve_ambiguous_submit` exists but runs only inside the adapter's own submit-timeout path | reconcile and the stuck-order watchdog |
/// | hyperliquid | no (the cancel key is `cloid = keccak(coid)`, a cancel path, not a lookup) | same |
/// | fxcm | re-queries the Orders table by the RequestID of a submit THIS process left UNKNOWN (a lost reply), through the exec thread | same-epoch only: the unknown set is in-memory; found -> `OrderAccepted`, not found -> silence (a lost MARKET reply fills and leaves the table: reconcile books it) |
/// | ctrader, alpaca, ibkr, dukascopy | no | same |
///
/// `tests/trade_coid_lookup.rs` pins every row against the bridge sources (`with_confirm(` present in
/// a venue's crate if and only if the row says `true`), so wiring a `confirm` into a bridge without
/// updating this table, or the reverse, fails a test. Each no-op venue's residual is in its bridge's
/// `CLAUDE.md`.
#[rustfmt::skip]
pub const COID_LOOKUP: &[(&str, bool)] = &[
    ("binance", true),
    ("bybit", true),
    ("okx", true),
    ("deribit", false),
    ("oanda", true),
    ("ig", true),
    ("fxcm", true),
    ("dukascopy", false),
    ("polymarket", true),
    ("ibkr", false),
    ("ctrader", false),
    ("alpaca", false),
    ("aster", true),
    ("hyperliquid", false),
    // vike:new-venue:row ("{venue}", false), // TODO(new-venue: {venue}): does its bridge build its ExecActor `with_confirm(..)` over a venue query by client order id? Say true only if so; `tests/trade_coid_lookup.rs` pins the answer to the bridge source
];

/// What [`BridgeTrade::preflight_plan`] answers: the venue lists of the preflight.
#[derive(Debug, Clone, Default)]
pub struct PreflightPlan {
    /// The declared [`ClockRisk`] of each WIRED venue among the clock venues: the fact the tradehub's
    /// preflight turns into thresholds and a remedy.
    pub clock_risks: HashMap<String, ClockRisk>,
    /// How many clock venues have a WIRED endpoint: only wired reads buy time budget.
    pub wired_clock_reads: usize,
    /// The venues a credential read was built for, sorted: the credential leg's list IS this set.
    pub credential_venues: Vec<String>,
}

/// The books venues confirmed, shared by every generation of a refreshed lane.
type ConfirmedBooks = Arc<Mutex<HashMap<(String, Option<String>), contract::ConfirmedBook>>>;

/// See the module doc.
pub struct BridgeTrade {
    registry: &'static [VenueRow],
    vars: Arc<HashMap<String, String>>,
    /// Always `Some` and always without a `preflight_failed` venue: the shape
    /// `bounded_server_time_ms` takes, because its probe closure must own its captures.
    snapshot: Arc<Option<TradeSnapshot>>,
    /// Built by [`BridgeTrade::preflight_plan`], answered from by [`BridgeTrade::authed_read`].
    credential_reader: Mutex<Option<CredentialReader>>,
    /// The books venues confirmed themselves at a start, waiting for `record_identity` (which the
    /// tradehub makes after `resolve_fee_schedule`), keyed `(venue, account label text)`.
    confirmed: ConfirmedBooks,
    /// The key set each started adapter was built from, so the arming table can name an account whose key in
    /// the store has changed since (`StartedKeys`). Shared by every generation of the lane.
    started_keys: Arc<StartedKeys>,
    /// Whether the bridges have spoken for the accounts the fan-out never starts (once per process).
    not_running_said: Arc<AtomicBool>,
    /// A FRESH read of the account rows, for the supervised link's predicate (`with_account_rows`); `None` =
    /// the snapshot the lane started with answers.
    rows_now: Option<AccountRows>,
    /// The data half's view of a venue's price feed, for the reconcile health gate ([`TradeLane::recon_degraded`]);
    /// `None` reports every venue healthy.
    health: Option<Arc<dyn crate::health::VenueHealth>>,
    /// The public instrument list behind `TradeLane::symbol_facts` (`with_symbol_facts`); `None` = the lane answers the
    /// default. Shared by every generation, so its listing cache survives a refresh.
    symbol_facts: Option<Arc<super::symbol_facts::SymbolFacts>>,
    /// alpaca's facts: its instrument list needs the asking account's credentials, so the lane lists it
    /// itself ([`BridgeTrade::alpaca_symbol_facts`]). Shared by every generation, so its listing survives a
    /// refresh; only consulted when `symbol_facts` is set (the composition root says the lane answers facts).
    alpaca_facts: Arc<super::symbol_facts::AccountFacts>,
    /// Hyperliquid's API-wallet approval watch (`with_agent_watch`); `None` = nothing is watched. Shared by every
    /// generation, so the watcher's memory of what it told the operator survives a refresh.
    agent_watch: Option<Arc<super::agent_watch::AgentWatch>>,
    /// ctrader's facts, ONE per account (its symbol list is one account's protobuf session and its deposit
    /// currency is the account's own), keyed by the wire account text (`""` = the default account) AND by
    /// `CtraderCatalog::account_fingerprint`, so credentials swapped under one label never answer from the old
    /// account's listing: [`BridgeTrade::ctrader_symbol_facts`]. Shared by every generation, like `alpaca_facts`.
    ctrader_facts: Arc<super::symbol_facts::FactsByAccount<super::symbol_facts::CtraderFacts>>,
    /// ctrader's rate memory, ONE per account and credential fingerprint like `ctrader_facts`: the rate each
    /// currency's last ask gave (reused briefly: an order's look and its commit are one read), a failed ask
    /// (not dialled again for a while) and the account's single ask in flight. Shared by every generation.
    ctrader_rates: Arc<super::symbol_facts::FactsByAccount<super::ctrader_rate::RateMemory>>,
    /// How many cTrader rate asks run now in the whole hub (at most `ctrader_rate::HUB_RATE_ASKS`), so a slow
    /// venue cannot hold the hub's four lookup threads. Shared by every generation.
    ctrader_rate_asks: Arc<super::ctrader_rate::HubRateAsks>,
    /// oanda's facts, ONE per account (its instrument list and its home currency are the account's own),
    /// keyed by the wire account text (`""` = the default account) AND by a fingerprint of the account id
    /// the credentials name, so a credential swap under one label never answers from the old account's
    /// list: [`BridgeTrade::oanda_symbol_facts`]. Shared by every generation, like `alpaca_facts`.
    oanda_facts: Arc<super::symbol_facts::FactsByAccount<super::symbol_facts::OandaFacts>>,
    /// ig's facts, ONE per account, keyed by the wire account text (`""` = the default account) like
    /// ctrader's ([`super::symbol_facts::FactsByAccount`]): an epic's dealing rules and the account's
    /// currency are both read with that account's own login ([`BridgeTrade::ig_symbol_facts`]). Shared by
    /// every generation, like `alpaca_facts`.
    ig_accounts: Arc<super::symbol_facts::FactsByAccount<IgAccount>>,
    /// ibkr's facts, ONE per account, keyed by the wire account text (`""` = the default account) like ig's:
    /// a stock name is resolved by asking that account's logged-in gateway
    /// ([`BridgeTrade::ibkr_symbol_facts`]). Shared by every generation, like `alpaca_facts`.
    #[cfg(feature = "ibkr")]
    ibkr_accounts: Arc<super::symbol_facts::FactsByAccount<IbkrAccount>>,
    /// Hyperliquid's confirmed leverage caps, kept until the account's adapter is started again
    /// ([`BridgeTrade::leverage_cap`]). Shared by every generation, so a refresh does not make the next
    /// `AdapterStart` re-read what the venue already confirmed.
    leverage_caps: Arc<super::leverage_caps::LeverageCaps>,
    /// How the lane READS a venue that posts a leverage (bybit, okx, binance; decision 0182): the bridges'
    /// own readers in production. Shared by every generation of the lane.
    posted_reads: super::posted_leverage::PostedReads,
    /// What each account's last start did about a posting venue's leverage (handed the bridge the figure, or
    /// refused), so the read-back confirms only what the start wrote. Shared by every generation.
    posted_handovers: Arc<super::posted_leverage::Handovers>,
    /// The symbol the process's ONE dukascopy engine was started on (`AdapterStart::symbol` of its last SUCCESSFUL
    /// start; a refused one records nothing): the money the ledger books at par, which a labelled account may
    /// choose, so it is the start's and not the wiring table's ([`Self::dukascopy_mount`]). Shared by every
    /// generation of the lane.
    dukascopy_mount: Arc<Mutex<Option<String>>>,
}

/// One IBKR account's facts: the bounded per-name answers and the source that asks the gateway.
#[cfg(feature = "ibkr")]
#[derive(Default)]
struct IbkrAccount {
    facts: super::ibkr_facts::IbkrFacts,
    /// The answers of a node that CONVERTS a quote currency (decision 0183): kept apart from `facts`, because
    /// the same name is refused there for its currency and admitted here.
    converting: super::ibkr_facts::IbkrFacts,
    /// Replaced when the account's connection changes (`IbkrFactsSource::serves`).
    source: Mutex<Option<Arc<vike_ibkr::any_symbol_socket::IbkrFactsSource>>>,
}

/// The currency of an ibkr engine's money for a node that does NOT convert. The ledger is one currency and
/// nothing converts (decision 0170), and the balance seed reads the CASH of the mounted contract's currency; the
/// door admits a name into an engine only when that engine's own symbol is in this currency too. The account's
/// base currency is NOT read on this path (its `$LEDGER` vocabulary is unobserved), so this is a constant, not a
/// lookup. A node that converts (`ibkr_symbol_facts_quoted`, demo with an exposure ceiling) reads the base
/// currency of the account summary instead and never consults this constant.
#[cfg(feature = "ibkr")]
const IBKR_LEDGER_CURRENCY: &str = "USD";

/// One IG account's facts: the bounded per-epic answers and the kept login that fetches them.
#[derive(Default)]
struct IgAccount {
    facts: super::ig_facts::IgFacts,
    /// Replaced when the account's credentials change (`IgFactsSource::serves`).
    source: Mutex<Option<Arc<vike_ig::IgFactsSource>>>,
}

/// The currency of a dukascopy engine's money for a node that does NOT convert a quote currency. The ledger is
/// one currency and nothing converts there (decision 0170), and this lane answers the unconverting node from a
/// constant, not a lookup: a demo account opened in another currency is UNMEASURED here, and the door admits an
/// instrument only when it is quoted in the money of the engine's own symbol as well. A node that DOES convert
/// (decision 0183) is answered from the currency the sidecar's `ready` line states
/// ([`super::dukascopy_rate`]), and falls back to this constant when the jar states none.
const DUKASCOPY_LEDGER_CURRENCY: &str = "USD";

/// How long the start path waits for ONE read of the account's currency, whichever venue states it: ig's own
/// bound (`GET /accounts` after a login), and oanda's `GET /summary`, which a same-start re-ask now makes on the
/// lease reader thread under the session's adapter lock (a reconnect after a failed first read, or onto an
/// adapter a restore installed) and which used to wait for the shared agent's 30 s.
const CURRENCY_READ_WAIT: std::time::Duration = vike_ig::ACCOUNT_CURRENCY_WAIT;

/// `read` on a thread of its own, waited on for `limit` at most: its answer, or an error that names the wait. The
/// read is not cancelled when the wait ends (its answer is dropped).
pub(super) fn read_within<T: Send + 'static>(
    limit: std::time::Duration,
    what: &str,
    read: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name(format!("gwh-{what}").replace(' ', "-"))
        .spawn(move || {
            let _ = tx.send(read());
        })
        .map_err(|e| format!("could not start the {what}: {e}"))?;
    rx.recv_timeout(limit)
        .unwrap_or_else(|_| Err(format!("the {what} did not answer within {limit:?}")))
}

/// **What a start tells the node about the account's currency** (`TradeLane::account_currency`): the venue's
/// statement, or nothing. `read` is the venue's answer (oanda's summary, ig's `GET /accounts`).
///
/// ⚠ An IG account the wire cannot deal for is told AS WHAT IT IS, and the node refuses to run it
/// (`symbol_admission::ig_account_refusal`). The facts fence an account whose currency is not USD
/// (`vike_ig`'s `wire_account`), so on a GBP account they refuse every epic but the mounted one (EUR/USD, quoted
/// in USD), and the mounted one is never judged by them: the
/// engine's ledger cannot be anchored to "GBP" (it would book a USD-quoted mount's money, ~35% off, under a pound
/// label), and left unlabelled it seeds pound cash into a ledger the ceiling and the margin read as dollars. Telling
/// nothing (this function's old answer) hid that from the node; the node, which knows whether the account asked for
/// the conversion (an exposure ceiling armed: the only start that reads the currency at all), is the one to fail
/// closed. A failed read is logged at `warn` and tells nothing.
pub(super) fn told_currency(
    venue: &str,
    account: &str,
    read: Result<String, String>,
) -> Option<String> {
    match read {
        Ok(currency) if venue == "ig" && currency != vike_ig::WIRE_CURRENCY => {
            tracing::error!(
                venue, account, currency = %currency,
                "ig: this account's currency is {currency} and the mounted epic is quoted in {wire}, so a \
                 ledger in {currency} cannot be tied to the epics it books in {wire}: \
                 only a {wire} account is supported. The node is told {currency} and does NOT RUN an ig \
                 account that armed the conversion with an exposure ceiling",
                wire = vike_ig::WIRE_CURRENCY
            );
            Some(currency)
        }
        Ok(currency) => Some(currency),
        Err(why) => {
            tracing::warn!(
                venue,
                account,
                "{venue}: the account's currency could not be read ({why}); the engine keeps the \
                 ledger label it has and the node refuses a symbol quoted in another currency"
            );
            None
        }
    }
}

/// The wire spelling of the account a request names, as the "said once" memory of the owner's USD default keys it
/// (`""` and an absent label are both the default account).
pub(super) fn account_key(target: &TradeTarget) -> &str {
    target.account.as_deref().unwrap_or("default")
}

/// The line that says why the lane answers a pass `None` for the balance of an account whose currency is only
/// ASSUMED ([`TradeLane::balance_withheld`]).
fn balance_withheld_words(venue: &str) -> String {
    format!(
        "{venue}: the account's currency is only ASSUMED ({}), and the venue's balance carries no currency, so \
         it is not handed to the engine (a figure in the account's real currency would be labelled {}); it \
         lands once a start reads the account's currency from the venue",
        super::assumed_ccy::ASSUMED_CCY,
        super::assumed_ccy::ASSUMED_CCY
    )
}

impl BridgeTrade {
    /// A lane over `registry`, the credential/deployment map and the settings snapshot the daemon read.
    #[must_use]
    pub fn new(
        registry: &'static [VenueRow],
        vars: HashMap<String, String>,
        accounts: &AccountDirectory,
        venue_settings: &std::collections::BTreeMap<String, VenueSettings>,
    ) -> Self {
        Self::with_snapshot(
            registry,
            vars,
            TradeSnapshot {
                accounts: accounts.clone(),
                venue_settings: venue_settings.clone(),
                venue_settings_unread: None,
                preflight_failed: std::collections::BTreeSet::new(),
            },
        )
    }

    /// [`BridgeTrade::new`] over a snapshot already built (the tests'). The snapshot's
    /// `preflight_failed` is dropped (see the module doc).
    #[must_use]
    pub fn with_snapshot(
        registry: &'static [VenueRow],
        vars: HashMap<String, String>,
        mut snapshot: TradeSnapshot,
    ) -> Self {
        snapshot.preflight_failed.clear();
        BridgeTrade {
            registry,
            vars: Arc::new(vars),
            snapshot: Arc::new(Some(snapshot)),
            credential_reader: Mutex::new(None),
            confirmed: Arc::new(Mutex::new(HashMap::new())),
            started_keys: Arc::new(StartedKeys::default()),
            not_running_said: Arc::new(AtomicBool::new(false)),
            rows_now: None,
            health: None,
            symbol_facts: None,
            alpaca_facts: Arc::default(),
            ctrader_facts: Arc::default(),
            ctrader_rates: Arc::default(),
            ctrader_rate_asks: Arc::default(),
            oanda_facts: Arc::default(),
            ig_accounts: Arc::default(),
            #[cfg(feature = "ibkr")]
            ibkr_accounts: Arc::default(),
            leverage_caps: Arc::default(),
            posted_reads: super::posted_leverage::PostedReads::bridges(),
            posted_handovers: Arc::default(),
            dukascopy_mount: Arc::default(),
            agent_watch: None,
        }
    }

    /// **The next generation of this lane**: the same registry over a freshly read `vars` and snapshot,
    /// carrying what must survive a refresh (the books venues confirmed at a start and not yet recorded, and
    /// the once-per-process not-running report). The credential reader a preflight plan built is
    /// the old generation's own.
    #[must_use]
    pub(crate) fn successor(
        &self,
        vars: HashMap<String, String>,
        mut snapshot: TradeSnapshot,
    ) -> Self {
        snapshot.preflight_failed.clear();
        BridgeTrade {
            registry: self.registry,
            vars: Arc::new(vars),
            snapshot: Arc::new(Some(snapshot)),
            credential_reader: Mutex::new(None),
            confirmed: Arc::clone(&self.confirmed),
            started_keys: Arc::clone(&self.started_keys),
            not_running_said: Arc::clone(&self.not_running_said),
            rows_now: self.rows_now.clone(),
            health: self.health.clone(),
            symbol_facts: self.symbol_facts.clone(),
            alpaca_facts: Arc::clone(&self.alpaca_facts),
            ctrader_facts: Arc::clone(&self.ctrader_facts),
            ctrader_rates: Arc::clone(&self.ctrader_rates),
            ctrader_rate_asks: Arc::clone(&self.ctrader_rate_asks),
            oanda_facts: Arc::clone(&self.oanda_facts),
            ig_accounts: Arc::clone(&self.ig_accounts),
            #[cfg(feature = "ibkr")]
            ibkr_accounts: Arc::clone(&self.ibkr_accounts),
            leverage_caps: Arc::clone(&self.leverage_caps),
            posted_reads: self.posted_reads.clone(),
            posted_handovers: Arc::clone(&self.posted_handovers),
            dukascopy_mount: Arc::clone(&self.dukascopy_mount),
            agent_watch: self.agent_watch.clone(),
        }
    }

    /// Record what a start handed the bridge for `target`'s account (the read-back tests' stand-in for a
    /// start, which would also consume the scripted venue's reads).
    #[cfg(test)]
    pub(crate) fn handed_over_for_test(&self, target: &TradeTarget, outcome: Result<f64, String>) {
        self.posted_handovers.record(target, outcome);
    }

    /// The lane reading a posting venue through `reads` (the tests' scripted readers and clock).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_posted_reads(mut self, reads: super::posted_leverage::PostedReads) -> Self {
        self.posted_reads = reads;
        self
    }

    /// The deployment map this generation reads (the refresh tests' window).
    #[cfg(all(test, feature = "data"))]
    pub(crate) fn vars_for_test(&self) -> HashMap<String, String> {
        self.vars.as_ref().clone()
    }

    /// The kept leverage caps (the start tests' window).
    #[cfg(test)]
    pub(crate) fn leverage_caps_for_test(&self) -> &super::leverage_caps::LeverageCaps {
        &self.leverage_caps
    }

    /// oanda's kept per-account facts (the quoted-facts tests prime a listing through it, so no request is made).
    #[cfg(test)]
    pub(crate) fn oanda_facts_for_test(
        &self,
    ) -> &super::symbol_facts::FactsByAccount<super::symbol_facts::OandaFacts> {
        &self.oanda_facts
    }

    /// How many venue-confirmed books wait for `record_identity` (shared by every generation).
    #[cfg(test)]
    pub(crate) fn confirmed_books_for_test(&self) -> usize {
        self.confirmed.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// The venue settings this generation reads (the refresh tests' window).
    #[cfg(all(test, feature = "data"))]
    pub(crate) fn venue_settings_for_test(
        &self,
    ) -> std::collections::BTreeMap<String, VenueSettings> {
        self.snapshot().venue_settings.clone()
    }

    /// The lane with a FRESH read of the account rows behind the supervised link's predicate
    /// (`TradeLane::link_watch`): every call of `rows` re-reads the store, so an `account.active` flip made
    /// since the start is seen at the next reconnect attempt. Without it the predicate answers from the
    /// snapshot the lane started with, which never changes.
    #[must_use]
    pub fn with_account_rows(mut self, rows: AccountRows) -> Self {
        self.rows_now = Some(rows);
        self
    }

    /// This lane answering the reconcile health gate from `health` (the tick pumps' view, `crate::health`).
    #[must_use]
    pub fn with_health(mut self, health: Arc<dyn crate::health::VenueHealth>) -> Self {
        self.health = Some(health);
        self
    }

    /// This lane answering `TradeLane::symbol_facts` from `facts` (carried by every generation).
    #[must_use]
    pub fn with_symbol_facts(mut self, facts: Arc<super::symbol_facts::SymbolFacts>) -> Self {
        self.symbol_facts = Some(facts);
        self
    }

    /// This lane watching hyperliquid accounts' API-wallet approval (carried by every generation).
    #[must_use]
    pub fn with_agent_watch(mut self, watch: Arc<super::agent_watch::AgentWatch>) -> Self {
        self.agent_watch = Some(watch);
        self
    }

    fn snapshot(&self) -> &TradeSnapshot {
        self.snapshot.as_ref().as_ref().expect("a BridgeTrade always holds a snapshot")
    }

    /// **Put a started hyperliquid account under the approval watch** (`agent_watch`): only the signer's
    /// public ADDRESS is derived from the account's key (the key is not kept), and the account is dropped
    /// from the watch once its row no longer says it runs at this tier (the same fresh read the supervised
    /// link asks). ⚠ The predicate lives as long as the watcher thread, so it keeps the snapshot's ROWS
    /// only (`AccountDirectory::without_credentials`): a book of every account's keys held there would
    /// outlive a rotation. Nothing here can fail a start: no watch, no key, or no usable key is a quiet
    /// return.
    pub(crate) fn watch_agent(&self, target: &TradeTarget, account: &AccountLabel) {
        let Some(watch) = &self.agent_watch else { return };
        let accounts = &self.snapshot().accounts;
        let creds = accounts.credentials_for(&target.venue, account, target.tier);
        let Some(subject) = vike_hyperliquid::agent_expiry::subject_of(target.tier, &creds) else {
            return;
        };
        let (snapshot_rows, rows_now) = (accounts.without_credentials(), self.rows_now.clone());
        let (venue, label, tier) = (target.venue.clone(), account.clone(), target.tier);
        let name = account.text().unwrap_or("default").to_string();
        watch.watch(super::agent_watch::Watched {
            key: format!("{}/{name}/{}", target.venue, tier.as_str()),
            account: name,
            subject,
            still_runs: Box::new(move || {
                let directory = match &rows_now {
                    Some(read) => read(),
                    None => snapshot_rows.clone(),
                };
                matches!(
                    vike_config::account_arming::account_tier(Some(&directory), &venue, &label),
                    Ok(now) if now == tier
                )
            }),
        });
    }

    /// The snapshot a START reads: this lane's, with the venue's credentials withheld (and, for a
    /// preflight stop, the venue named in `preflight_failed`) when the tradehub asked.
    fn snapshot_for(&self, venue: &str, withhold: Option<Withhold>) -> TradeSnapshot {
        let mut snapshot = self.snapshot().clone();
        if let Some(withhold) = withhold {
            let (kept, _withheld) = snapshot.accounts.without_venue_credentials(venue);
            snapshot.accounts = kept;
            if withhold == Withhold::PreflightFailed {
                snapshot.preflight_failed.insert(venue.to_string());
            }
        }
        snapshot
    }

    /// **alpaca's symbol facts**: its instrument list (`GET /v1/assets`) is behind the account's own
    /// Bearer, so unlike the six public lists it is read with THIS account's sandbox credentials,
    /// resolved here from the lane's snapshot exactly as a start resolves them. It reads ONE asset
    /// (`GET /v1/assets/{symbol}`, never the several-MB active list), bounded as a public lookup is
    /// (`AccountFacts`: an asset kept ten minutes, single-flight per account and symbol, a failure
    /// remembered), and the credential never leaves `AlpacaCatalog`. A `live` account is refused: the
    /// adapter runs the sandbox only (`vike_alpaca::config::SandboxArm::LiveTierNotWired`), so such an
    /// account has no lease to ask from.
    fn alpaca_symbol_facts(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        if target.tier == AccountTier::Live {
            return Err("alpaca: this adapter trades its sandbox only, so a live account runs \
                        nothing and no instrument is looked up for it"
                .to_string());
        }
        let Some(VenueRow::Adapter(row)) = row_of(self.registry, "alpaca") else {
            return Err(super::symbol_facts::NOT_ANSWERED.to_string());
        };
        let label = Self::label_of(target.account.as_deref())
            .map_err(|block| format!("alpaca: no account to look up for: {}", block.as_str()))?;
        let process = contract::process_facts();
        let inputs = contract::inputs_for(
            *row,
            &label,
            &self.vars,
            target.tier,
            Some(self.snapshot()),
            &process,
        );
        let catalog = vike_alpaca::AlpacaCatalog::new(&inputs.credentials);
        // The slot and the lister are this account's (label and tier), never another asker's.
        self.alpaca_facts.facts(
            &format!("{label}/{}", target.tier.as_str()),
            "alpaca",
            symbol,
            // The facts wrap a failure as "alpaca: the venue's instrument list could not be fetched: <why>".
            Arc::new(move |symbol| catalog.asset(symbol).map_err(|e| e.0)),
        )
    }

    /// **ctrader's symbol facts**: there is no keyless list; the account's symbols are a protobuf
    /// session's answer, so, like alpaca's, they are read with THIS account's own demo grant (resolved from
    /// the lane's snapshot as a start resolves it) under `AccountFacts`' bounds (one listing per ten
    /// minutes, single-flight, a failure remembered), and the grant never leaves `CtraderCatalog`. The
    /// listing is ONE one-shot session (`CtraderCatalog::account_listing`: the shared grant is followed,
    /// never spent) beside the exec, data and reconcile sockets, and it brings the account's deposit
    /// currency with it: `vike_ctrader::catalog::any_symbol_refusal` admits a forex pair or a CFD of a
    /// measured class quoted in that currency and refuses everything else by name (the engine converts no
    /// currency here, and what a share, bond or dated-futures CFD's unit means is unmeasured). Only a `demo` account is looked up for: the adapter dials the demo host alone
    /// (`vike_ctrader::config::demo_arm`), so any other tier has no lease to ask from.
    fn ctrader_symbol_facts(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        self.ctrader_facts_for(target, symbol, false)
    }

    /// **The same, for a node that CONVERTS a quote currency** (`TradeUp::SymbolFacts::quoted`, decision 0183,
    /// phase 1): a forex pair quoted in another currency than the deposit's is answered with its quote
    /// currency and the rate the venue's own quote of the conversion pair gives (a one-shot data session,
    /// asked NOW and never kept: `vike_ctrader::CtraderCatalog::quote_rate`), where
    /// [`Self::ctrader_symbol_facts`] refuses it. A rate the venue cannot give is a refusal worded as a
    /// failure of the rate (`FACTS_RATE_UNAVAILABLE`, `ctrader_rate::quoted_rate`).
    fn ctrader_symbol_facts_quoted(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        self.ctrader_facts_for(target, symbol, true)
    }

    /// The account's own cTrader catalog (its demo grant, resolved from the lane's snapshot as a start resolves
    /// it): the one place every cTrader read of this module builds it. Only a `demo` account has one, because
    /// the adapter dials the demo host alone (`vike_ctrader::config::demo_arm`).
    fn ctrader_catalog(
        &self,
        target: &TradeTarget,
    ) -> Result<vike_ctrader::CtraderCatalog, String> {
        if target.tier != AccountTier::Demo {
            return Err("ctrader: this adapter trades its demo account only (a `demo` account), \
                        so no instrument is looked up for any other tier"
                .to_string());
        }
        let Some(VenueRow::Adapter(row)) = row_of(self.registry, "ctrader") else {
            return Err(super::symbol_facts::NOT_ANSWERED.to_string());
        };
        let label = Self::label_of(target.account.as_deref())
            .map_err(|block| format!("ctrader: no account to look up for: {}", block.as_str()))?;
        let process = contract::process_facts();
        let inputs = contract::inputs_for(
            *row,
            &label,
            &self.vars,
            target.tier,
            Some(self.snapshot()),
            &process,
        );
        Ok(vike_ctrader::CtraderCatalog::new(&inputs.credentials))
    }

    fn ctrader_facts_for(
        &self,
        target: &TradeTarget,
        symbol: &str,
        convert: bool,
    ) -> Result<WireSymbolFacts, String> {
        let catalog = Arc::new(self.ctrader_catalog(target)?);
        // The facts and the rate memory are this account's AND these credentials': a swap under one label
        // replaces both (`FactsByAccount::of`), so the old account's listing and rates never answer.
        let wire_account = target.account.as_deref().unwrap_or_default();
        let fingerprint = catalog.account_fingerprint();
        let account = self.ctrader_facts.of(wire_account, fingerprint);
        let listing = Arc::clone(&catalog);
        let said = account_key(target).to_string();
        // The facts wrap a failure as "ctrader: the venue's instrument list could not be fetched: <why>". A deposit
        // currency the broker's asset list does not name is the owner's USD, ASSUMED (`listing_or_assumed`), where
        // the strict listing refused the whole account; a named one is passed through untouched.
        let lister: super::symbol_facts::CurrencyLister = Arc::new(move || {
            super::ctrader_rate::listing_or_assumed(&CCY_SAID, &said, || {
                listing.account_listing_lenient().map_err(|e| e.0)
            })
        });
        if !convert {
            return account.facts(symbol, lister, vike_ctrader::catalog::any_symbol_refusal);
        }
        // The parameter types are spelled out, as OANDA's are: a closure coerced to a `dyn Fn` over references
        // is only general in their lifetimes when it says they are references.
        // Each ask is a whole authenticated session: the memory hands out a rate read a moment ago (the order's
        // look and its commit), does not redial a failed ask for a while and caps the asks in flight.
        let memory = self.ctrader_rates.of(wire_account, fingerprint);
        let hub = Arc::clone(&self.ctrader_rate_asks);
        let rate: super::symbol_facts::RateSource = Arc::new(move |quote: &str, deposit: &str| {
            super::ctrader_rate::quoted_rate(quote, deposit, |q| {
                // The same assumption as the listing's, so the two cannot disagree about the deposit: only a
                // deposit the asset list does NOT name is converted into USD.
                memory.read(&hub, q, || {
                    catalog.quote_rate_assuming(q, super::assumed_ccy::ASSUMED_CCY)
                })
            })
        });
        account.facts_quoted(symbol, lister, vike_ctrader::catalog::quoted_symbol_refusal, &rate)
    }

    /// **oanda's symbol facts**: its instrument list (`GET /v3/accounts/{id}/instruments`) is behind the
    /// account's own Bearer, so, like alpaca's and ctrader's, it is read with THIS account's practice
    /// credentials, resolved from the lane's snapshot as a start resolves them, under `AccountFacts`'
    /// bounds (one listing per ten minutes, single-flight, a failure remembered). The listing is paired
    /// with the account's home currency (`GET /summary`) and `OandaFacts` refuses an instrument not quoted
    /// in it (the plain answer's currency fence, for a node that converts nothing: a node that does is
    /// answered by [`Self::oanda_symbol_facts_quoted`], below). Only a `demo`
    /// account is looked up for: the adapter trades the practice account alone
    /// (`vike_oanda::adapter_tier`), so any other tier has no lease to ask from.
    fn oanda_symbol_facts(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        self.oanda_facts_for(target, symbol, false)
    }

    /// **The same, for a node that CONVERTS a quote currency** (`TradeUp::SymbolFacts::quoted`, decision
    /// 0183, phase 1): an instrument quoted in another currency than the account's is answered with its
    /// quote currency and the rate OANDA states for it (`GET /pricing?includeHomeConversions=true`, asked
    /// NOW and never kept: `vike_oanda::OandaCatalog::home_conversions`), where [`Self::oanda_symbol_facts`]
    /// refuses it. A rate the venue cannot give is a refusal worded as a failure of the rate
    /// (`FACTS_RATE_UNAVAILABLE`, `oanda_rate::quoted_rate`), so the node does not remember it as a verdict
    /// on the symbol.
    fn oanda_symbol_facts_quoted(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        self.oanda_facts_for(target, symbol, true)
    }

    /// The account's own OANDA catalog (its practice credentials, resolved from the lane's snapshot as a
    /// start resolves them): the one place every OANDA read of this module builds it. Only a `demo` account
    /// has one, because the adapter trades the practice account alone (`vike_oanda::adapter_tier`).
    fn oanda_catalog(&self, target: &TradeTarget) -> Result<vike_oanda::OandaCatalog, String> {
        if target.tier != AccountTier::Demo {
            return Err(
                "oanda: this adapter trades its practice account only (a `demo` account), \
                        so no instrument is looked up for any other tier"
                    .to_string(),
            );
        }
        let Some(VenueRow::Adapter(row)) = row_of(self.registry, "oanda") else {
            return Err(super::symbol_facts::NOT_ANSWERED.to_string());
        };
        let label = Self::label_of(target.account.as_deref())
            .map_err(|block| format!("oanda: no account to look up for: {}", block.as_str()))?;
        let process = contract::process_facts();
        let inputs = contract::inputs_for(
            *row,
            &label,
            &self.vars,
            target.tier,
            Some(self.snapshot()),
            &process,
        );
        Ok(vike_oanda::OandaCatalog::new(&inputs.credentials))
    }

    fn oanda_facts_for(
        &self,
        target: &TradeTarget,
        symbol: &str,
        convert: bool,
    ) -> Result<WireSymbolFacts, String> {
        use vike_catalog::CatalogProvider;
        let catalog = Arc::new(self.oanda_catalog(target)?);
        let account = self
            .oanda_facts
            .of(target.account.as_deref().unwrap_or_default(), catalog.account_fingerprint());
        let listing = Arc::clone(&catalog);
        let said = account_key(target).to_string();
        // A failure is wrapped by the facts as "oanda: the venue's instrument list could not be fetched: <why>". A
        // summary that fails or states no home currency is the owner's USD, ASSUMED (`source_currency`); a stated
        // one is passed through untouched.
        let lister: super::symbol_facts::CurrencyLister =
            Arc::new(move || -> Result<(String, Vec<vike_model::Instrument>), String> {
                let currency = lister_currency(
                    &CCY_SAID,
                    "oanda",
                    &said,
                    listing.account_fingerprint().is_some(),
                    listing.home_currency().map_err(|e| e.0),
                )?;
                let rows = listing.list_instruments().map_err(|e| e.0)?;
                Ok((currency, rows))
            });
        if !convert {
            return account.facts(symbol, lister);
        }
        // The parameter types are spelled out: a closure coerced to a `dyn Fn` over references is only
        // general in their lifetimes when it says they are references. The pricing read converts into the
        // home currency the summary states, else the assumed one; OANDA's own entry for the home currency
        // (must convert at 1) refuses an assumption that is wrong.
        let rate: super::symbol_facts::RateSource =
            Arc::new(move |instrument: &str, home: &str| {
                super::oanda_rate::quoted_rate(instrument, home, |i| {
                    catalog.home_conversions_assuming(&[i], Some(super::assumed_ccy::ASSUMED_CCY))
                })
            });
        account.facts_quoted(symbol, lister, &rate)
    }

    /// **ig's symbol facts**: an epic's dealing rules (`GET /markets/{epic}`) and the account's currency
    /// (`GET /accounts`) are behind the account's own login, so, like alpaca's and oanda's, they are read with
    /// THIS account's demo credentials, resolved from the lane's snapshot as a start resolves them. IG has no
    /// list to keep, so `IgFacts` bounds the lookups per epic (a keep time, a single flight, a budget of
    /// upstream fetches per minute and a bound on the epics tracked), and `vike_ig::IgFactsSource` keeps ONE
    /// login for the account however many epics are asked. The fences (CURRENCIES only, a USD account and
    /// quote, the pip grid) are `vike_ig::market_facts`'. Only a `demo` account is looked up for: the adapter
    /// reaches the demo gateway alone (`vike_ig::adapter`), so any other tier has no lease to ask from.
    fn ig_symbol_facts(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        let (account, source) = self.ig_account_source(target)?;
        let epic = symbol.to_string();
        // ONE overall deadline per lookup: the daemon admits four lookups at once for ALL venues (a hung IG one
        // is a slot every other venue's lookup is then refused `busy` for, up to four serial 30 s calls long).
        account.facts.facts(symbol, move || source.facts_within(&epic, vike_ig::FACTS_LOOKUP_WAIT))
    }

    /// **The same, for a node that CONVERTS a quote currency** (`TradeUp::SymbolFacts::quoted`, decision 0183):
    /// an epic quoted in another currency than the account's is answered with its quote currency and the rate
    /// IG states for it, checked against a price (`vike_ig::IgFactsSource::facts_quoted`: the market details
    /// of the epic, and of a conversion pair for a cross, read over the account's one kept login), where
    /// [`Self::ig_symbol_facts`] refuses it. The rate is read NOW and never served from the listing's keep
    /// time (`IgFacts::facts_quoted`); one IG gives no price to confirm, or that disagrees, is a refusal
    /// worded as a failure of the rate (`FACTS_RATE_UNAVAILABLE`), so the node does not remember it as a verdict.
    fn ig_symbol_facts_quoted(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        let (account, source) = self.ig_account_source(target)?;
        let epic = symbol.to_string();
        // The same overall deadline as the plain ask ([`Self::ig_symbol_facts`]): a cross is up to four serial calls.
        account.facts.facts_quoted(symbol, move || {
            source.facts_quoted_within(&epic, vike_ig::FACTS_LOOKUP_WAIT)
        })
    }

    /// The account's IG facts and its kept login (`IgFactsSource`: one session however many epics are
    /// asked), resolved from the lane's snapshot as a start resolves them. The one place every IG facts read
    /// of this module builds them; only a `demo` account has one.
    fn ig_account_source(
        &self,
        target: &TradeTarget,
    ) -> Result<(Arc<IgAccount>, Arc<vike_ig::IgFactsSource>), String> {
        if target.tier != AccountTier::Demo {
            return Err(
                "ig: this adapter reaches its demo gateway only (a `demo` account), so no \
                        epic is looked up for any other tier"
                    .to_string(),
            );
        }
        let Some(VenueRow::Adapter(row)) = row_of(self.registry, "ig") else {
            return Err(super::symbol_facts::NOT_ANSWERED.to_string());
        };
        let label = Self::label_of(target.account.as_deref())
            .map_err(|block| format!("ig: no account to look up for: {}", block.as_str()))?;
        let process = contract::process_facts();
        let inputs = contract::inputs_for(
            *row,
            &label,
            &self.vars,
            target.tier,
            Some(self.snapshot()),
            &process,
        );
        // The row's book too (`venue_account_id`): the currency this source reads must be the account's the
        // orders go to (`vike_ig::IgConfig::account_pin`).
        let Some(config) = vike_ig::load_ig_config_for(&inputs) else {
            return Err("ig: this account holds no usable credentials (API_KEY, IDENTIFIER and \
                        PASSWORD), so no epic is looked up for it"
                .to_string());
        };
        let account = self.ig_accounts.of(target.account.as_deref().unwrap_or_default(), None);
        let source = {
            let mut held = account.source.lock().unwrap_or_else(PoisonError::into_inner);
            match held.as_ref() {
                Some(source) if source.serves(&config) => Arc::clone(source),
                _ => {
                    let source = Arc::new(vike_ig::IgFactsSource::new(config));
                    *held = Some(Arc::clone(&source));
                    source
                }
            }
        };
        Ok((account, source))
    }

    /// **ibkr's symbol facts**: IBKR publishes no instrument list, so a stock name (`AAPL.SMART.USD`) is
    /// resolved by asking THIS account's logged-in IB Gateway (a symbol search, the contract details and the
    /// market rules on one data connection through the gate's data lane, which never delays an order-lane
    /// dial: `vike_ibkr::any_symbol_socket`). `IbkrFacts` bounds the lookups per name (a keep time, a single
    /// flight, a budget per minute, a bound on the names tracked). The fences (a US stock by name, the
    /// ledger's currency, one conId, the grid) are `vike_ibkr::any_symbol`'s. Only a `demo` account is looked
    /// up for (the first safe step: IBKR is exercised on its paper account), and only on the socket backend.
    #[cfg(feature = "ibkr")]
    fn ibkr_symbol_facts(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        let (account, source) = self.ibkr_account(target)?;
        let name = symbol.to_ascii_uppercase();
        let asked = name.clone();
        account.facts.facts(&name, move || source.facts(&asked, IBKR_LEDGER_CURRENCY))
    }

    /// **The same, for a node that CONVERTS a quote currency** (`TradeUp::SymbolFacts::quoted`, decision 0183):
    /// the stock's grid is resolved with no currency fence and kept like every IBKR answer, and a stock quoted in
    /// another currency than the account's BASE currency (the currency of the account summary's `NetLiquidation`
    /// row, not the constant [`IBKR_LEDGER_CURRENCY`]) is answered with its quote currency and the rate of a
    /// live forex quote, read on every ask (`ibkr_rate::quoted_facts`). A rate the gateway cannot give is a
    /// refusal worded as a failure of the rate. Forex pairs are refused as CASH positions by the stock-name
    /// grammar, before any dial.
    #[cfg(feature = "ibkr")]
    fn ibkr_symbol_facts_quoted(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        let (account, source) = self.ibkr_account(target)?;
        let name = symbol.to_ascii_uppercase();
        // ONE ask, ONE connection: the grid and the rate ride the same scope, so a new foreign stock costs the
        // data lane one dial, and a stock whose grid is kept and whose rate is kept (or needs none) costs none.
        let ask = source.scope();
        super::ibkr_rate::quoted_facts(
            &account.converting,
            &name,
            || source.facts_any_currency_in(&ask, &name),
            |currency| source.rate_for_in(&ask, currency),
        )
    }

    /// **The currency the account's money is in, as IBKR states it** (decision 0183, item 1): the base currency of
    /// the account summary, read on the gateway session's data connection when the adapter has come up, for an
    /// account whose node asked for it (`start.read_account_ccy`: demo, an exposure ceiling armed, the venue
    /// converting). An account that never converts spends no dial on a currency it would not use. `None` for an
    /// account that is not `demo`, a cpapi account, and a gateway that states none within
    /// [`super::ibkr_rate::BASE_BOUND`] and was never read in this process; the engine then keeps its ledger label
    /// and a stock quoted in another currency stays refused. The read is asked a few times inside that bound,
    /// because the data lane of the gateway's gate refuses a dial at once while another dial holds its turn; a
    /// read that does not come back falls back to the currency this process read before (the one the engine's
    /// ledger was anchored to), said at `warn`.
    #[cfg(feature = "ibkr")]
    fn ibkr_account_currency(&self, target: &TradeTarget, start: &AdapterStart) -> Option<String> {
        if target.tier != AccountTier::Demo || !start.read_account_ccy {
            return None;
        }
        let (_, source) = match self.ibkr_account(target) {
            Ok(held) => held,
            Err(why) => {
                tracing::debug!(venue = %target.venue, "ibkr: the account's base currency is not read: {why}");
                return None;
            }
        };
        let read = super::ibkr_rate::read_base_currency(
            |left| source.account_base_currency_within(left),
            super::ibkr_rate::BASE_BOUND,
            super::ibkr_rate::BASE_PAUSE,
        );
        read.or_else(|| {
            let remembered = source.remembered_base()?;
            tracing::warn!(
                venue = %target.venue,
                "ibkr: the account's base currency could not be read now; the account is anchored to the \
                 currency this gatewayhub read before ({remembered})"
            );
            Some(remembered)
        })
    }

    /// The account's kept IBKR facts and the source that asks its gateway: the one place both facts verbs build
    /// them. Refused by name for a tier other than `demo` and for an account that holds no usable `ACCOUNT` field.
    #[cfg(feature = "ibkr")]
    fn ibkr_account(
        &self,
        target: &TradeTarget,
    ) -> Result<(Arc<IbkrAccount>, Arc<vike_ibkr::any_symbol_socket::IbkrFactsSource>), String>
    {
        if target.tier != AccountTier::Demo {
            return Err(
                "ibkr: any-symbol admission is wired for a `demo` account only, so no stock \
                        is looked up for any other tier"
                    .to_string(),
            );
        }
        let Some(VenueRow::Adapter(row)) = row_of(self.registry, "ibkr") else {
            return Err(super::symbol_facts::NOT_ANSWERED.to_string());
        };
        let label = Self::label_of(target.account.as_deref())
            .map_err(|block| format!("ibkr: no account to look up for: {}", block.as_str()))?;
        let process = contract::process_facts();
        let inputs = contract::inputs_for(
            *row,
            &label,
            &self.vars,
            target.tier,
            Some(self.snapshot()),
            &process,
        );
        let Some(config) = vike_ibkr::config::load_ibkr_config_from(
            target.tier,
            &inputs.credentials,
            inputs.settings,
        ) else {
            return Err(
                "ibkr: this account holds no usable ACCOUNT field (or its gateway rows do not \
                        parse), so no stock is looked up for it"
                    .to_string(),
            );
        };
        let account = self.ibkr_accounts.of(target.account.as_deref().unwrap_or_default(), None);
        let source = {
            let mut held = account.source.lock().unwrap_or_else(PoisonError::into_inner);
            match held.as_ref() {
                Some(source) if source.serves(&config) => Arc::clone(source),
                _ => {
                    let source =
                        Arc::new(vike_ibkr::any_symbol_socket::IbkrFactsSource::new(config));
                    *held = Some(Arc::clone(&source));
                    source
                }
            }
        };
        Ok((account, source))
    }

    /// **dukascopy's symbol facts**: STATIC and offline. The sidecar publishes no instrument list and its
    /// protocol has no query, so the grid is the committed JForex SDK table's
    /// (`vike_dukascopy::any_symbol::instrument_facts`: currency pairs quoted in the ledger's money,
    /// every other row, the metals included, refused by name). No credential is read and nothing is dialled. Only a `demo`
    /// account is answered (the sidecar logs into a demo server only; a `live` account never runs).
    fn dukascopy_symbol_facts(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        if target.tier != AccountTier::Demo {
            return Err("dukascopy: any-symbol admission is wired for a `demo` account only (the \
                        JForex sidecar logs into a demo server only), so no instrument is admitted \
                        for any other tier"
                .to_string());
        }
        let Some(VenueRow::Adapter(_)) = row_of(self.registry, "dukascopy") else {
            return Err(super::symbol_facts::NOT_ANSWERED.to_string());
        };
        let properties =
            vike_dukascopy::any_symbol::instrument_facts(symbol, DUKASCOPY_LEDGER_CURRENCY)?;
        Ok(WireSymbolFacts { properties, ..WireSymbolFacts::default() })
    }

    /// **dukascopy's symbol facts for a node that CONVERTS a quote currency** (decision 0183, phase 1): the same
    /// static grid, but an instrument quoted in another currency than the account's is answered with its quote
    /// currency and the rate of the mid of the conversion pair, read from the SIDECAR'S OWN TICKS
    /// ([`super::dukascopy_rate::quoted_facts`]). The account's currency is what the sidecar's `ready` line states.
    /// Everything that keeps the lane from converting (a `live` tier, a jar that states no currency, a mount in
    /// another money, no sidecar) answers what [`Self::dukascopy_symbol_facts`] does, so its own fence stands.
    /// May block for a few seconds on the first tick of a conversion pair (the facts thread, never the reader's).
    fn dukascopy_symbol_facts_quoted(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        let Some(VenueRow::Adapter(_)) = row_of(self.registry, "dukascopy") else {
            return Err(super::symbol_facts::NOT_ANSWERED.to_string());
        };
        if target.tier != AccountTier::Demo {
            return self.dukascopy_symbol_facts(target, symbol);
        }
        super::dukascopy_rate::quoted_facts(
            &vike_dukascopy::quotes::process_bus(),
            vike_dukascopy::quotes::RATE_WAIT,
            &self.dukascopy_mount(),
            symbol,
            || self.dukascopy_symbol_facts(target, symbol),
        )
    }

    /// The symbol the process's dukascopy engine was started on: the last SUCCESSFUL start's `AdapterStart::symbol`
    /// this lane saw for the venue, else the wiring table's (`EURUSD`; a lookup before any start). A labelled
    /// account may be mounted on another pair than the table's, and the money its ledger books at par is that
    /// pair's.
    pub(super) fn dukascopy_mount(&self) -> String {
        self.dukascopy_mount.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_else(
            || vike_venues::venue_wiring::wired_symbol("dukascopy").unwrap_or_default().to_string(),
        )
    }

    /// The currency the dukascopy sidecar STATES for the account (whether or not the mounted pair is quoted in
    /// it: the node's mount fence judges that, [`super::dukascopy_rate::ccy_read`]), else `None` and the engine
    /// keeps the label it has. Only a `demo` account has a sidecar. Said at `info` for an old jar: an ordinary
    /// state, and the node's refusal of a foreign-quoted symbol names it again.
    fn dukascopy_account_currency(&self, target: &TradeTarget) -> Option<String> {
        match self.dukascopy_ccy_read(target)? {
            CcyRead::Stated(currency) => Some(currency),
            CcyRead::Silent(why) => {
                tracing::info!(
                    venue = %target.venue, account = account_key(target),
                    "{why}; the engine keeps the ledger label it has and the node refuses a symbol quoted in \
                     another currency"
                );
                None
            }
            CcyRead::Declined => None,
        }
    }

    /// The venue's own answer to "what currency is this account in?" for oanda, ig and ctrader, before any policy:
    /// `None` when this venue/ask reads nothing here, else the read (`Err` is the failure's words). ig reads only
    /// for a start that asks ([`Self::currency_read_is_budgeted`]); an ig row that states no readable currency (the
    /// facts then read it as USD, `IgFactsSource::currency_is_assumed`) is a silence, not a statement.
    fn account_currency_raw(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
    ) -> Option<Result<String, String>> {
        Some(match target.venue.as_str() {
            // ⚠ Bounded like ig's: the start (and a same-start re-ask) holds the session's adapter lock while this
            // asks, and `GET /summary` waits up to the shared agent's 30 s. Nothing is kept of a read that was
            // given up on (the catalog holds no row), so the next ask reads again.
            "oanda" => self.oanda_catalog(target).and_then(|catalog| {
                read_within(CURRENCY_READ_WAIT, "account-currency read", move || {
                    catalog.home_currency().map_err(|e| e.0)
                })
            }),
            // ⚠ The start holds the session's adapter lock while this asks, and the login and `GET /accounts`
            // each wait up to 30 s: bounded to `ACCOUNT_CURRENCY_WAIT`. The read is not abandoned (the kept
            // login and row serve the next ask), and only the FIRST start of a source pays for it.
            "ig" if start.read_account_ccy => {
                self.ig_account_source(target).and_then(|(_, source)| {
                    let read = source
                        .account_currency_within(vike_ig::ACCOUNT_CURRENCY_WAIT)
                        .map_err(|e| e.text().to_string())?;
                    if source.currency_is_assumed() {
                        return Err("ig: the account's row states no readable currency".to_string());
                    }
                    Ok(read)
                })
            }
            "ctrader" => self
                .ctrader_catalog(target)
                .and_then(|catalog| catalog.account_currency().map_err(|e| e.0)),
            _ => return None,
        })
    }

    /// **What the lane's read of the account's currency came to, for the owner's USD default**
    /// ([`assumed_ccy`](super::assumed_ccy)): `None` where nothing is ever assumed (a tier other than `demo`, a venue with no
    /// currency source, ibkr while the node did not ask for the budgeted read), else the read as stated, silent
    /// (failed or empty: USD fills in) or declined (nothing to say, nothing to assume over).
    fn account_currency_read(&self, target: &TradeTarget, start: &AdapterStart) -> Option<CcyRead> {
        if target.tier != AccountTier::Demo {
            return None;
        }
        #[cfg(feature = "ibkr")]
        if target.venue == "ibkr" {
            if !start.read_account_ccy {
                return None;
            }
            return Some(match self.ibkr_account_currency(target, start) {
                Some(ccy) => CcyRead::Stated(ccy),
                None => CcyRead::Silent(
                    "the gateway's account summary stated no base currency (the data lane refused the dial, \
                     the gateway was silent, or the summary carries none)"
                        .to_string(),
                ),
            });
        }
        if target.venue == "dukascopy" {
            return self.dukascopy_ccy_read(target);
        }
        Some(match self.account_currency_raw(target, start)? {
            Ok(ccy) => {
                // The log an ig account in another currency than the wire's has always had.
                let _ = told_currency(&target.venue, account_key(target), Ok(ccy.clone()));
                CcyRead::Stated(ccy)
            }
            Err(why) => CcyRead::Silent(why),
        })
    }

    /// What the dukascopy sidecar says about the account's currency, for a `demo` account only
    /// ([`super::dukascopy_rate::ccy_read`]): the currency it STATES (whether or not the mounted pair is quoted in
    /// it: the node's mount fence declines an anchor the mount cannot carry, in its own words, as it does for
    /// every other venue), silence from an attached jar that predates the field, and nothing to say while no
    /// sidecar is attached.
    fn dukascopy_ccy_read(&self, target: &TradeTarget) -> Option<CcyRead> {
        (target.tier == AccountTier::Demo).then(|| {
            super::dukascopy_rate::ccy_read(
                &vike_dukascopy::quotes::process_bus(),
                &self.dukascopy_mount(),
            )
        })
    }

    /// The label a wire account names; `None` is the default account. A label the wire spelling does
    /// not admit is an account with no row.
    fn label_of(account: Option<&str>) -> Result<AccountLabel, ArmingBlock> {
        match account {
            None => Ok(AccountLabel::Default),
            Some(text) => parse_wire_account(text).map_err(|_| ArmingBlock::NoAccountRow),
        }
    }

    /// **The preflight's venue lists**: the declared clock risk of each venue in `clock_venues` that
    /// has a wired clock endpoint, how many of them there are (they buy time budget), and the venues
    /// a credential read was built for. Builds the credential probes [`BridgeTrade::authed_read`] then
    /// answers from.
    pub fn preflight_plan(&self, clock_venues: &[String]) -> PreflightPlan {
        // The credential probes are built FIRST, as the preflight always did: a ctrader probe is
        // lazy, an alpaca/binance one eager, and the tier interlock logs here.
        let probes = authed_read_probes(self.registry, &self.vars, Some(self.snapshot()));
        // The CREDENTIAL leg's list IS the authed-readable set (the startup module doc's INVARIANT).
        let mut credential_venues: Vec<String> = probes.keys().cloned().collect();
        credential_venues.sort();
        let clock_risks = clock_risks(self.registry, clock_venues);
        // The budget is DERIVED from how many venues are actually READ (only wired ones buy time).
        let wired_clock_reads = clock_venues
            .iter()
            .filter(|v| {
                matches!(server_time::clock_decl(self.registry, v), Some(ClockDecl::Wired { .. }))
            })
            .count();
        *self.credential_reader.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Box::new(authed_read_probe(probes)));
        PreflightPlan { clock_risks, wired_clock_reads, credential_venues }
    }

    /// ONE venue clock read as an ABSOLUTE epoch-ms stamp, bounded (a wedged name resolution is
    /// abandoned at `CLOCK_PROBE_TIMEOUT`); the gaps are the preflight's four outcomes.
    pub fn server_time_ms(&self, venue: &str, tier: AccountTier) -> Result<i64, ServerTimeGap> {
        bounded_server_time_ms(self.registry, venue, &self.vars, tier, &self.snapshot)
    }

    /// ONE bounded credential read (a cheap authenticated balance fetch, retried once after an
    /// ANSWERED error, never after a silence) for a venue [`BridgeTrade::preflight_plan`] listed.
    pub fn authed_read(&self, venue: &str) -> Result<(), CredentialGap> {
        let guard = self.credential_reader.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.as_ref() {
            Some(read) => read(venue),
            None => {
                Err(CredentialGap::Rejected(format!("no credential probe was planned for {venue}")))
            }
        }
    }

    /// ONE clock read of `venue` as the wire's leg: the reading with the daemon's clock at its end and the
    /// round trip, or the reason there is none. Only a venue whose default account would start a venue
    /// client is read (its row states a tier); one that states none is `NotChecked`.
    fn clock_leg_of(&self, venue: &str) -> LegResult {
        let read_started = vike_model::now_ms();
        let read =
            match vike_config::account_arming::venue_tier(Some(&self.snapshot().accounts), venue) {
                Some(tier) => self.server_time_ms(venue, tier),
                None => Err(ServerTimeGap::NotChecked(
                    "the account does not run: no account row states its tier",
                )),
            };
        let read_ended = vike_model::now_ms();
        let rtt_ms = u64::try_from(read_ended.saturating_sub(read_started)).unwrap_or(0);
        clock_result(venue, read, read_ended, rtt_ms)
    }

    /// ONE credential read of `venue` as the wire's leg (the default account's: the probes are built for
    /// it alone). The probes are built first when no plan was asked since this lane started, so a read
    /// never answers "no probe was planned" for a venue that has one.
    fn credential_leg_of(&self, venue: &str) -> LegResult {
        let planned =
            self.credential_reader.lock().unwrap_or_else(PoisonError::into_inner).is_some();
        if !planned {
            let _ = self.preflight_plan(&[]);
        }
        credential_result(venue, None, self.authed_read(venue))
    }

    /// The network liveness reading the preflight's network leg shows: a probe is spawned, waited on
    /// for a bounded first round, read and dropped.
    pub fn net_reading(&self) -> NetReading {
        let thread = spawn_net_probe(DEFAULT_NET_PROBE_WAIT);
        let reading = net_reading_of(&thread.handle());
        // Signals stop without joining — see `NetProbeThread`'s `Drop` doc.
        drop(thread);
        reading
    }

    /// **Let each bridge speak for the accounts of `venue` the fan-out will never start** (a stored
    /// key set that is unused or incomplete), once over rows already computed; it cannot change which
    /// account starts. `TradeLane` has no verb for it: [`TradeLane::arming_table`] says it, once per
    /// process, for every roster venue.
    pub fn report_not_running(&self, venue: &str, rows: &[VenueArming]) {
        contract::report_not_running_accounts(
            self.registry,
            venue,
            rows,
            &self.vars,
            Some(self.snapshot()),
        );
    }

    /// **The arming table without the not-running report**: what [`TradeLane::arming_table`] serves,
    /// minus the once-per-process lines the bridges say about accounts that will not start. For a
    /// caller that says those itself at its own place in the sequence (the in-process roster tests).
    #[must_use]
    pub fn arming_table_quiet(&self) -> ArmingTable {
        let mut table = arming::arming_table(self.registry, &self.vars, self.snapshot());
        table.key_changed = self.started_keys.changed(&self.snapshot().accounts);
        table
    }

    /// The arming rows (`VenueArming` plus the book) of every roster venue, preflight stop NOT applied.
    #[must_use]
    pub fn arming_rows(&self) -> Vec<(VenueArming, Option<String>)> {
        arming::arming_rows(self.registry, &self.vars, self.snapshot())
    }

    /// Say, once per process, what each bridge has to say about accounts that will not start. The
    /// tradehub's fan-out said it once per assembly, from the same rows; with the venue half in the
    /// daemon, the first arming table it serves is the nearest equivalent.
    fn say_not_running_once(&self) {
        if self.not_running_said.swap(true, Ordering::SeqCst) {
            return;
        }
        let rows = self.arming_rows();
        for venue in vike_model::VENUES {
            let venue_rows: Vec<VenueArming> =
                rows.iter().filter(|(r, _)| r.venue == *venue).map(|(r, _)| r.clone()).collect();
            self.report_not_running(venue, &venue_rows);
        }
    }

    /// **The start itself**: [`TradeLane::start_adapter`]'s body over the venue half's own types, so
    /// the in-process roster tests can read what a refusal said.
    ///
    /// # Errors
    /// [`contract::Refusal`] when the account does not run at its tier (decision 0156).
    pub(crate) fn start(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        events: &EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<StartedAdapter, contract::Refusal> {
        let account = Self::label_of(target.account.as_deref()).map_err(contract::Refusal::of)?;
        let row = match row_of(self.registry, &target.venue) {
            Some(VenueRow::Adapter(row)) => *row,
            // The tradehub reads the table's facts before it asks, so these are defensive: the same
            // two answers `make_engine_for_account` always gave a venue with no bridge here.
            Some(VenueRow::FeatureAbsent { .. }) => {
                return Err(contract::Refusal::of(ArmingBlock::FeatureAbsent));
            }
            None => return Err(contract::Refusal::of(ArmingBlock::NoLiveArm)),
        };
        // FAIL CLOSED on a `venue_setting` read that failed: the empty map it left is not "no rows", and
        // a bridge started on its built-in defaults (ibkr's gateway, the polymarket market list, the
        // order-path opt-ins) is not the account the operator configured. The reason is named; the next
        // start re-reads the table.
        if let Some(why) = &self.snapshot().venue_settings_unread {
            tracing::error!(
                venue = %target.venue, error = %why,
                "the venue_setting table could not be read: this venue's adapter is NOT started on \
                 built-in defaults"
            );
            return Err(contract::Refusal {
                block: ArmingBlock::AdapterFailed,
                text: Some(format!(
                    "the venue_setting rows could not be read ({why}); {} does not start on built-in \
                     defaults — fix the settings database, the next start reads it again",
                    target.venue
                )),
            });
        }
        let snapshot = self.snapshot_for(&target.venue, start.withhold);
        // `ProfileRisk` is `Deserialize`-only and `vike-model` is a measured crate, so the wire carries
        // the one field a bridge reads of it and the daemon rebuilds the profile.
        let risk_profile = start.risk.map(|r| vike_model::ProfileRisk {
            max_leverage: r.max_leverage,
            ..Default::default()
        });
        // A venue that POSTS a leverage and a start that asks for the account's own figure
        // (decision 0182): the bridge is handed the figure as its profile's number, so its ONE
        // existing start-time write carries it, only when a read-only probe said the venue may be
        // asked; otherwise the profile goes through untouched (today's start).
        let risk_profile = self.posted_profile(target, start, &account, risk_profile)?;
        let bound = contract::contract_parts(
            row,
            contract::ContractCall {
                registry: self.registry,
                venue: &target.venue,
                symbol: &start.symbol,
                account: &account,
                declared_legs: &start.declared_legs,
                vars: &self.vars,
                events,
                recon_enabled: start.recon_enabled,
                recon_trigger: trigger,
                risk_profile: risk_profile.as_ref(),
                policy: Some(&snapshot),
                tier: target.tier,
                halt_admit: start.halt_admit,
                market_slippage: start.market_slippage,
            },
        )?;
        if let Some(confirmed) = bound.confirmed_book {
            self.confirmed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert((target.venue.clone(), target.account.clone()), confirmed);
        }
        // The key set the adapter was just built from (the lane's own snapshot: `snapshot_for` only ever
        // withholds, and a withheld start would not have come up), for the key-changed warning.
        self.started_keys.record(&self.snapshot().accounts, &target.venue, &account, target.tier);
        if target.venue == "hyperliquid" {
            self.watch_agent(target, &account);
        }
        Ok(bound.started)
    }

    /// **The profile the bridge is started with** (decision 0182): `profile` itself unless this start
    /// asks a posting venue to hold the account's figure (`posted_leverage::asked_figure`) AND the venue's
    /// read-only probe says it may be asked, in which case the figure becomes the profile's
    /// `max_leverage`. A refused probe is said at WARN and leaves `profile` untouched, so the start is
    /// today's, including the default write; `leverage_cap` then answers the refusal to the tradehub.
    ///
    /// ⚠ **The one exception is a MAINNET LANE TWIN** (`posted_leverage::live_twin_must_hand_over`): its
    /// start would write the default (or the run profile's number) to the REAL account, so with no figure
    /// to ask, or a probe that refuses, the start is REFUSED here (`AdapterFailed`, in words) and the
    /// bridge is never started: no write, no session.
    ///
    /// # Errors
    /// [`contract::Refusal`] for a mainnet lane twin whose figure cannot be handed to the venue.
    fn posted_profile(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        account: &AccountLabel,
        profile: Option<vike_model::ProfileRisk>,
    ) -> Result<Option<vike_model::ProfileRisk>, contract::Refusal> {
        let accounts = &self.snapshot().accounts;
        // Only THIS start may say what was handed over: whatever an earlier start of THIS venue's account
        // recorded is gone (the record is keyed by venue too: another venue's start touches none of it).
        self.posted_handovers.forget(target);
        // A withheld start is data-only: its credentials are not used, so nothing is written to the venue.
        let strict = start.withhold.is_none()
            && super::posted_leverage::live_twin_must_hand_over(accounts, target, account);
        let refuse = |why: &str| {
            tracing::error!(
                venue = %target.venue, account = %account,
                "{}: the LIVE perp lane twin is NOT started: {why}. Nothing was written to the venue \
                 and no bridge was started (a start that cannot hand over the owner's leverage would \
                 write the default to the real account)",
                target.venue
            );
            contract::Refusal {
                block: ArmingBlock::AdapterFailed,
                text: Some(format!(
                    "the live perp lane twin does not start: {why}. Nothing was written to the venue"
                )),
            }
        };
        let Some(candidate) =
            super::posted_leverage::asked_figure(accounts, target, account, start)
        else {
            if strict {
                return Err(refuse(
                    "the start asks the venue for no leverage (the account row no longer states \
                     one, the table refuses the figure, or the node sent none)",
                ));
            }
            return Ok(profile);
        };
        match super::posted_leverage::probe(
            &self.posted_reads,
            accounts,
            target,
            account,
            &start.symbol,
            candidate,
        ) {
            Ok(()) => {
                tracing::info!(
                    venue = %target.venue, account = %account,
                    "{}: the account's leverage ({candidate}x) is what this start writes to the venue",
                    target.venue
                );
                self.posted_handovers.record(target, Ok(candidate));
                Ok(Some(super::posted_leverage::profile_with(profile, candidate)))
            }
            Err(why) => {
                if strict {
                    self.posted_handovers.record(target, Err(why.clone()));
                    return Err(refuse(&format!(
                        "the venue may not be asked to hold {candidate}x: {why}"
                    )));
                }
                tracing::warn!(
                    venue = %target.venue, account = %account,
                    "{}: the account's leverage ({candidate}x) is NOT written to the venue: {why}. The \
                     start is the one it always was, and the engine will stay at 1x",
                    target.venue
                );
                self.posted_handovers.record(target, Err(why));
                Ok(profile)
            }
        }
    }

    /// **What a posting venue confirms of the figure this start asked** (decision 0182): `None` when the
    /// start asks nothing, else the venue SEEN holding it (`Ok`, equal to the ask) or why not. Never
    /// kept: a re-ask on a running adapter reads the venue again, so a leverage changed on the venue
    /// since is seen, not replayed.
    fn posted_leverage_cap(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
    ) -> Option<Result<f64, String>> {
        let label = Self::label_of(target.account.as_deref()).ok()?;
        let accounts = &self.snapshot().accounts;
        let candidate = super::posted_leverage::asked_figure(accounts, target, &label, start)?;
        // The venue may hold the figure from an EARLIER run: that confirms nothing unless THIS start
        // handed the bridge the figure to write.
        if let Err(why) =
            super::posted_leverage::written_by_the_start(&self.posted_handovers, target, candidate)
        {
            return Some(Err(why));
        }
        Some(super::posted_leverage::confirm(
            &self.posted_reads,
            accounts,
            target,
            &label,
            &start.symbol,
            candidate,
        ))
    }

    /// **What a posting venue confirms of the account's figure for a symbol admitted on demand**
    /// (decision 0182's per-symbol twin): `None` when nothing is asked of the venue (see
    /// [`TradeLane::symbol_leverage`]), else the venue SEEN holding the figure or why not.
    ///
    /// The figure is the running start's own (`posted_leverage::asked_figure`, the one gate), so a
    /// symbol is never asked for a number the mounted symbol would not have been. The mounted symbol is
    /// the start's (its write and read-back are `posted_leverage_cap`), and a spot symbol has no
    /// leverage. Everything else is the venue's: `posted_leverage::settle_symbol` reads, writes through
    /// the registry bridge's own `set_symbol_leverage` at most once, and reads back.
    fn posted_symbol_leverage(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        symbol: &str,
    ) -> Option<Result<f64, String>> {
        if symbol.eq_ignore_ascii_case(&start.symbol) {
            return None;
        }
        let label = Self::label_of(target.account.as_deref()).ok()?;
        let accounts = &self.snapshot().accounts;
        let candidate = super::posted_leverage::asked_figure(accounts, target, &label, start)?;
        if super::posted_leverage::is_spot_symbol(&target.venue, symbol) {
            return None;
        }
        let Some(VenueRow::Adapter(row)) = row_of(self.registry, &target.venue) else {
            return Some(Err(format!(
                "{}: this build carries no adapter to write {symbol}'s leverage",
                target.venue
            )));
        };
        let process = contract::process_facts();
        let inputs = contract::inputs_for(
            *row,
            &label,
            &self.vars,
            target.tier,
            Some(self.snapshot()),
            &process,
        );
        Some(super::posted_leverage::settle_symbol(
            &self.posted_reads,
            accounts,
            target,
            &label,
            symbol,
            candidate,
            |figure| row.set_symbol_leverage(&inputs, symbol, figure),
        ))
    }
}

/// The wire's class of a declared clock risk. Exhaustive: a new `ClockRisk` fails to compile here until the
/// wire decides what to call it.
fn risk_class(risk: ClockRisk) -> ClockRiskClass {
    match risk {
        ClockRisk::SignedTimestamp => ClockRiskClass::SignedTimestamp,
        ClockRisk::NonceWindow => ClockRiskClass::NonceWindow,
        ClockRisk::NoTimestamp => ClockRiskClass::NoTimestamp,
    }
}

/// The wire's venue lists for a plan: each wired venue's risk class WITH the bridge's remedy sentence
/// (the thresholds are the tradehub's judgement over the class, never the daemon's).
fn lists_of(plan: &PreflightPlan) -> PreflightLists {
    PreflightLists {
        clock_risks: plan
            .clock_risks
            .iter()
            .map(|(venue, risk)| {
                (
                    venue.clone(),
                    ClockRiskInfo { class: risk_class(*risk), remedy: risk.remedy().to_string() },
                )
            })
            .collect(),
        wired_clock_reads: u32::try_from(plan.wired_clock_reads).unwrap_or(u32::MAX),
        credential_venues: plan.credential_venues.clone(),
    }
}

impl TradeLane for BridgeTrade {
    fn venues(&self) -> Vec<String> {
        self.registry
            .iter()
            .filter(|row| matches!(row, VenueRow::Adapter(_)))
            .map(|row| row.venue().to_string())
            .collect()
    }

    fn account_tier(&self, venue: &str, account: Option<&str>) -> Result<AccountTier, ArmingBlock> {
        let label = Self::label_of(account)?;
        vike_config::account_arming::account_tier(Some(&self.snapshot().accounts), venue, &label)
    }

    fn arming_table(&self) -> ArmingTable {
        self.say_not_running_once();
        self.arming_table_quiet()
    }

    /// **One question, one measurement per venue named** (see [`TradeLane::preflight`]): the daemon only
    /// MEASURES. The tradehub's `run_preflight` re-reads a venue clock up to `DEFAULT_CLOCK_SAMPLES` times
    /// and interleaves the clock and credential budgets between reads, so it asks one read at a time and
    /// owns the loop; every answer is exactly what the matching per-read verb
    /// ([`BridgeTrade::server_time_ms`], [`BridgeTrade::authed_read`], [`BridgeTrade::net_reading`]) says,
    /// in the wire's words.
    fn preflight(&self, venues: &[String], step: PreflightStep) -> PreflightAnswer {
        let legs = match step {
            PreflightStep::Plan => {
                return PreflightAnswer {
                    legs: Vec::new(),
                    plan: Some(lists_of(&self.preflight_plan(venues))),
                };
            }
            PreflightStep::Clock => venues.iter().map(|venue| self.clock_leg_of(venue)).collect(),
            PreflightStep::Credential => {
                venues.iter().map(|venue| self.credential_leg_of(venue)).collect()
            }
            PreflightStep::Network => vec![network_result(self.net_reading())],
            PreflightStep::Batch => {
                if venues.is_empty() {
                    return PreflightAnswer::default();
                }
                let plan = self.preflight_plan(venues);
                let mut legs = Vec::with_capacity(venues.len() * 2 + 1);
                for venue in venues {
                    legs.push(self.clock_leg_of(venue));
                    if plan.credential_venues.contains(venue) {
                        legs.push(self.credential_leg_of(venue));
                    }
                }
                legs.push(network_result(self.net_reading()));
                legs
            }
        };
        PreflightAnswer { legs, plan: None }
    }

    fn start_adapter(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        events: EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<StartedAdapter, AdapterRefusal> {
        // A (re)started adapter is a new lifetime: the venue's word on its leverage is asked again.
        self.leverage_caps.forget(target.account.as_deref().unwrap_or_default());
        let started = self.start(target, start, &events, trigger);
        // The pair the dukascopy engine is mounted on is what its ledger books at par: kept for the facts and
        // the account-currency read that follow, and ONLY when the start came up. A start that was refused (the
        // one-sidecar rule, no credentials) mounted nothing, and the account that holds the process's one
        // sidecar must keep being judged on ITS pair, not the decliner's.
        if started.is_ok() && target.venue == "dukascopy" {
            *self.dukascopy_mount.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(start.symbol.clone());
        }
        started.map_err(|refusal| {
            let block_word = super::arming::wire_word(refusal.block).to_string();
            // The bridge's own sentence where it rendered one; otherwise the account row's remedy for
            // the block, which is what `vike-backend venues` prints for it.
            let refusal_text = refusal.text.unwrap_or_else(|| {
                vike_model::VENUES.iter().copied().find(|v| *v == target.venue).map_or_else(
                    || block_word.clone(),
                    |venue| {
                        VenueArming {
                            venue,
                            label: Self::label_of(target.account.as_deref()).unwrap_or_default(),
                            tier: Some(target.tier),
                            effective: None,
                            block: refusal.block,
                            account_ids: Vec::new(),
                        }
                        .why()
                    },
                )
            });
            AdapterRefusal { block_word, refusal_text }
        })
    }

    fn record_identity(
        &self,
        target: &TradeTarget,
        bound_tier: AccountTier,
        recon: Option<&dyn ReconClient>,
    ) -> Result<(), String> {
        let account = Self::label_of(target.account.as_deref())
            .map_err(|block| format!("no account to record: {}", block.as_str()))?;
        let directory = super::snapshot::directory_of(Some(self.snapshot()));
        book_identity::record_authenticated_account(
            &target.venue,
            &account,
            bound_tier,
            recon,
            directory,
        );
        // Hyperliquid's identity confirmation (decision 0088's B3): its `userRole` probe ran in its
        // own start and arrives as the contract's `IdentityReport`. Only a CONFIRMED answer is
        // recorded, as `record_confirmation`'s `Disagrees` arm writes nothing.
        let confirmed = self
            .confirmed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(target.venue.clone(), target.account.clone()));
        if let Some(confirmed) = confirmed {
            book_identity::record_confirmation(
                &target.venue,
                confirmed.evidence,
                &account,
                confirmed.tier,
                &confirmed.book,
                directory,
            );
        }
        Ok(())
    }

    fn symbol_facts(&self, target: &TradeTarget, symbol: &str) -> Result<WireSymbolFacts, String> {
        match &self.symbol_facts {
            Some(_) if target.venue == "alpaca" => self.alpaca_symbol_facts(target, symbol),
            Some(_) if target.venue == "ctrader" => self.ctrader_symbol_facts(target, symbol),
            Some(_) if target.venue == "oanda" => self.oanda_symbol_facts(target, symbol),
            Some(_) if target.venue == "ig" => self.ig_symbol_facts(target, symbol),
            #[cfg(feature = "ibkr")]
            Some(_) if target.venue == "ibkr" => self.ibkr_symbol_facts(target, symbol),
            Some(_) if target.venue == "dukascopy" => self.dukascopy_symbol_facts(target, symbol),
            Some(facts) => facts.facts(&target.venue, symbol),
            None => Err(super::symbol_facts::NOT_ANSWERED.to_string()),
        }
    }

    /// oanda (a venue-stated factor), ctrader (the mid of the conversion pair), ibkr (a live forex quote),
    /// dukascopy (the sidecar's own ticks of the conversion pair) and ig (the market details' `currencies[]`,
    /// checked against a price) have a rate source (decision 0183, phase 1); every other venue answers what
    /// [`Self::symbol_facts`] does, so its own fence stands for a node that asks to convert.
    fn symbol_facts_quoted(
        &self,
        target: &TradeTarget,
        symbol: &str,
    ) -> Result<WireSymbolFacts, String> {
        match &self.symbol_facts {
            Some(_) if target.venue == "oanda" => self.oanda_symbol_facts_quoted(target, symbol),
            Some(_) if target.venue == "ctrader" => {
                self.ctrader_symbol_facts_quoted(target, symbol)
            }
            Some(_) if target.venue == "ig" => self.ig_symbol_facts_quoted(target, symbol),
            #[cfg(feature = "ibkr")]
            Some(_) if target.venue == "ibkr" => self.ibkr_symbol_facts_quoted(target, symbol),
            Some(_) if target.venue == "dukascopy" => {
                self.dukascopy_symbol_facts_quoted(target, symbol)
            }
            // fxcm: the account's own running reconcile session, no second login (`fxcm_rate`).
            Some(_) if target.venue == "fxcm" => {
                super::fxcm_rate::facts_for(target.tier, target.account.as_deref(), symbol)
            }
            _ => self.symbol_facts(target, symbol),
        }
    }

    /// The currency the VENUE states for the account: oanda's account summary (`account.currency`), ctrader's
    /// trader record (its deposit asset, `ProtoOATrader.depositAssetId`, named through the asset list), ibkr's
    /// base currency (the `NetLiquidation` row's), the dukascopy sidecar's `ready` line
    /// (`IAccount.getAccountCurrency()`, read off the price lane's bus: the sidecar is the one this account's
    /// start just brought up) and ig's account list (`GET /accounts`, the row of the account the login is
    /// scoped to; waited on for `vike_ig::ACCOUNT_CURRENCY_WAIT` at most; an IG account in another currency
    /// than the wire's, `USD`, is logged at `error` and told as it is, and the node does not run it:
    /// [`told_currency`]). ibkr and ig read it only when the node asked (`start.read_account_ccy`: demo, an
    /// exposure ceiling armed), because their read costs the venue a connection of a budget
    /// ([`Self::currency_read_is_budgeted`]); the others' is free and ignores the field. No other venue's lane
    /// reads one here.
    fn account_currency(&self, target: &TradeTarget, start: &AdapterStart) -> Option<String> {
        #[cfg(feature = "ibkr")]
        if target.venue == "ibkr" {
            return self.ibkr_account_currency(target, start);
        }
        if target.venue == "dukascopy" {
            return self.dukascopy_account_currency(target);
        }
        let read = self.account_currency_raw(target, start)?;
        told_currency(&target.venue, account_key(target), read)
    }

    /// ibkr (a dial of the data lane's four an hour) and ig (a login and a read out of the account's allowance)
    /// pay for the statement of the account's currency out of a venue budget: they read it only for a start
    /// that asks (`start.read_account_ccy`), and the session asks them again (a repeated start) only for such
    /// a start too. The node sets the field for exactly these two venues (`reads_currency_on_ask`).
    fn currency_read_is_budgeted(&self, target: &TradeTarget) -> bool {
        matches!(target.venue.as_str(), "ibkr" | "ig")
    }

    /// fxcm states its account's currency in a system property of its own session (`BASE_CRNCY`), read off the
    /// account's running reconcile session; when the API cannot give it the owner's ruling fills in USD,
    /// ASSUMED and flagged (`fxcm_rate::currency_for`). **Every other converting venue now does the same**
    /// (`assumed_ccy`, owner ruling 2026-10-19: "the account currency is what the venue API states; if the API
    /// does not state it, or the read fails: USD"): a `demo` account whose read came back empty or failed is
    /// told as USD, ASSUMED, and warned once; a currency the venue STATES is told as it is, untouched, and once
    /// stated (to this start or to a rate source) it is never downgraded to an assumption by a later silent read.
    /// Nothing is assumed for a tier that never converts, for ibkr and ig while the node did not ask for the
    /// budgeted read, or for dukascopy while no sidecar is attached.
    fn account_currency_stated(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
    ) -> (Option<String>, bool) {
        if target.venue == "fxcm" {
            // The session's own decision, as stated (the node's mount fence judges the mounted symbol, as it does
            // for every other venue). FXCM says its own assumption (`vike_fxcm::reads`), so the start only NOTES
            // what the node was told: that, not the session's state later, is what the balance gate follows.
            let told = super::fxcm_rate::currency_for(target.tier, target.account.as_deref());
            if target.tier == AccountTier::Demo {
                CCY_SAID.note_told("fxcm", account_key(target), !matches!(told, (Some(_), false)));
            }
            return told;
        }
        match self.account_currency_read(target, start) {
            Some(read) => start_currency(&CCY_SAID, &target.venue, account_key(target), read),
            None => (None, false),
        }
    }

    /// **Why the venue's reconcile balance must not be handed to the node for this account** (the gatewayhub
    /// answers the pass `None`, "not reported", so the node folds no balance and raises no drift), or `None`.
    /// The figure of oanda, ig and fxcm is the account's home-currency cash with no currency beside it, and
    /// the node adopts it into a ledger LABELLED with the account's currency: while the engine's currency is only
    /// ASSUMED (`assumed_ccy`), a figure in the account's real currency would be labelled USD. ⚠ It is what the
    /// last START told the node that decides, never what a rate source or a session reads later: the engine's
    /// label does not move until it is started again. ⚠ A belt only: the node's own gate over the reconcile client
    /// it binds (`remote_backend::balance_gate`) is the authority, because a re-teach after this process's restart
    /// is a start no engine consumes. ctrader fences
    /// itself (`vike_ctrader::recon_client::parse_balance` answers only a USD deposit it NAMES), ibkr's client
    /// withholds itself while the engine holds a stock in another currency, and dukascopy has no reconcile leg.
    fn balance_withheld(&self, target: &TradeTarget) -> Option<String> {
        let account = account_key(target);
        let assumed = match target.venue.as_str() {
            "oanda" | "ig" => CCY_SAID.is_assumed(&target.venue, account),
            // What the start told, or the session's own say-so when no start was seen by this process.
            "fxcm" => {
                CCY_SAID.is_assumed("fxcm", account)
                    || super::fxcm_rate::currency_is_assumed(target.account.as_deref())
            }
            _ => false,
        };
        assumed.then(|| balance_withheld_words(&target.venue))
    }

    /// **hyperliquid's leverage confirmation** (decision 0181): asked only when this lane's own account
    /// rows state a leverage for the account, so every other start makes no venue call. The venue read
    /// is `vike_hyperliquid::leverage_cap`'s; the credentials are the account's own, resolved from the
    /// lane's snapshot as a start resolves them.
    fn leverage_cap(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
    ) -> Option<Result<f64, String>> {
        if target.venue != "hyperliquid" {
            // bybit, okx and binance (decision 0182): the venue is READ BACK holding the asked figure.
            // Any other venue, and any start that asks nothing, answers `None`, as it always did.
            return self.posted_leverage_cap(target, start);
        }
        let label = Self::label_of(target.account.as_deref()).ok()?;
        let directory = &self.snapshot().accounts;
        let Some(Ok(rows)) = directory.rows() else { return None };
        let stated = rows.leverage_of(&target.venue, label.text())?;
        let creds = directory.credentials_for(&target.venue, &label, target.tier);
        // Kept for the adapter's lifetime (`start_adapter` forgets it): an `AdapterStart` that finds the
        // adapter running asks again, and each ask was up to three venue reads.
        Some(self.leverage_caps.confirmed(
            target.account.as_deref().unwrap_or_default(),
            target.tier,
            &start.symbol,
            stated.get(),
            || {
                vike_hyperliquid::leverage_cap::confirmed_leverage_cap(
                    target.tier,
                    &creds,
                    &label,
                    &start.symbol,
                )
            },
        ))
    }

    /// bybit, okx and binance (decision 0182's per-symbol twin): the venue is read, written once through
    /// the bridge's own verb when it holds another number, and READ BACK holding the account's figure
    /// for a symbol admitted on demand. Every other venue, and every ask that asks nothing, is `None`.
    fn symbol_leverage(
        &self,
        target: &TradeTarget,
        start: &AdapterStart,
        symbol: &str,
    ) -> Option<Result<f64, String>> {
        self.posted_symbol_leverage(target, start, symbol)
    }

    fn resolves_by_coid(&self, venue: &str) -> bool {
        COID_LOOKUP.iter().any(|(v, yes)| *v == venue && *yes)
    }

    /// **IBKR's gateway socket is the one venue link the daemon supervises** (decision 0164 verdict 7(b)):
    /// its bridge latches dead for good when the gateway logs off, so the daemon watches the port the bridge
    /// dials (`crate::trade::ibkr`) and rebuilds the adapter. Every other venue's bridge owns its reconnect
    /// and gets `None`. The predicate re-reads the rows through `with_account_rows` on every call.
    fn link_watch(
        &self,
        target: &TradeTarget,
        _start: &AdapterStart,
    ) -> Option<Arc<dyn LinkWatch>> {
        if target.venue != "ibkr" {
            return None;
        }
        let empty = VenueSettings::default();
        let settings = self.snapshot().venue_settings.get("ibkr").unwrap_or(&empty);
        let gateway = match IbkrGateway::from_settings(target.tier, settings) {
            Ok(Some(gateway)) => gateway,
            Ok(None) => return None,
            Err(why) => {
                tracing::warn!("TRADE-LINK: the ibkr gateway is not supervised: {why}");
                return None;
            }
        };
        let label = Self::label_of(target.account.as_deref()).ok()?;
        // The link's predicate lives as long as the link: rows only, never the credential book.
        let snapshot_rows = self.snapshot().accounts.without_credentials();
        let rows_now = self.rows_now.clone();
        let venue = target.venue.clone();
        let tier_now: TierNow = Box::new(move || {
            let directory = match &rows_now {
                Some(read) => read(),
                None => snapshot_rows.clone(),
            };
            vike_config::account_arming::account_tier(Some(&directory), &venue, &label)
        });
        Some(Arc::new(IbkrWatch::new(gateway, tier_now)))
    }

    /// **Dukascopy's JForex sidecar is the one venue path that reports itself** (it dies and the exec client
    /// restarts it while the lease stays up): the status verb states its state (`super::dukascopy_link`). Every
    /// other venue's bridge owns its reconnect and says nothing here.
    fn venue_link(&self, target: &TradeTarget, _start: &AdapterStart) -> Option<VenueLinkReport> {
        (target.venue == "dukascopy").then(super::dukascopy_link::process_report)
    }

    fn recon_degraded(&self, venue: &str) -> Option<String> {
        self.health.as_ref().and_then(|health| health.degraded(venue))
    }
}
