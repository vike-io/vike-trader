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
    PreflightLists, PreflightStep, Withhold,
};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::{AccountLabel, parse_wire_account};
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::venue_setting::VenueSettings;

use super::probe::{
    CredentialGap, NetReading, ServerTimeGap, clock_result, credential_result, network_result,
};
use super::registry::{VenueRow, row_of};
use super::snapshot::TradeSnapshot;
use super::startup::{
    DEFAULT_NET_PROBE_WAIT, authed_read_probe, authed_read_probes, bounded_server_time_ms,
    clock_risks, net_reading_of, spawn_net_probe,
};
use super::{arming, book_identity, contract, server_time};
use crate::trade::ibkr::{IbkrGateway, IbkrWatch, TierNow};
use crate::trade::lane::{AdapterRefusal, LinkWatch, StartedAdapter, TradeLane, TradeTarget};

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
    /// Whether the bridges have spoken for the accounts the fan-out never starts (once per process).
    not_running_said: Arc<AtomicBool>,
    /// A FRESH read of the account rows, for the supervised link's predicate (`with_account_rows`); `None` =
    /// the snapshot the lane started with answers.
    rows_now: Option<AccountRows>,
    /// The data half's view of a venue's price feed, for the reconcile health gate ([`TradeLane::recon_degraded`]);
    /// `None` reports every venue healthy.
    health: Option<Arc<dyn crate::health::VenueHealth>>,
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
            not_running_said: Arc::new(AtomicBool::new(false)),
            rows_now: None,
            health: None,
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
            not_running_said: Arc::clone(&self.not_running_said),
            rows_now: self.rows_now.clone(),
            health: self.health.clone(),
        }
    }

    /// The deployment map this generation reads (the refresh tests' window).
    #[cfg(all(test, feature = "data"))]
    pub(crate) fn vars_for_test(&self) -> HashMap<String, String> {
        self.vars.as_ref().clone()
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

    fn snapshot(&self) -> &TradeSnapshot {
        self.snapshot.as_ref().as_ref().expect("a BridgeTrade always holds a snapshot")
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
        arming::arming_table(self.registry, &self.vars, self.snapshot())
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
        Ok(bound.started)
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
        self.start(target, start, &events, trigger).map_err(|refusal| {
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
        let snapshot_rows = self.snapshot().accounts.clone();
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

    fn recon_degraded(&self, venue: &str) -> Option<String> {
        self.health.as_ref().and_then(|health| health.degraded(venue))
    }
}
