//! **`RemoteBackend` — the venue port over the gatewayhub** (wave 2, W11; decision 0164).
//!
//! The tradehub opens no venue connection of its own: the daemon that owns them all
//! (`vike-gatewayhub`) answers the fold's questions over the `trade` wire, and this is the
//! [`VenueBackend`] that asks. It is the in-process `LocalBackend`'s counterpart behind the same
//! port, and it is what [`crate::tradehub_cli`]'s live mount composes when `flags.tradehub_live` is
//! on. What the fold sees is unchanged: the same [`ArmingTable`], the same `AdapterBind`
//! (an exec client and a reconcile client where a bridge's used to be), the same refusal words.
//!
//! # What a verb is on the wire
//!
//! | port verb | wire |
//! |---|---|
//! | `arming_table` | `ArmingQuery`, ONCE per backend ([`RemoteBackend::connect`]), then answered from memory |
//! | `preflight_plan` | `PreflightQuery` step `Plan`: the venue lists and each wired venue's clock risk |
//! | `server_time_ms`, `authed_read`, `net_reading` | ONE `PreflightQuery` per READ (steps `Clock`, `Credential`, `Network`): the keep half's `run_preflight` owns the schedule (resample, interleaved budgets), the gatewayhub only measures ([`legs`]) |
//! | `start_adapter` | `TradeOpen` (the lease), THEN `AdapterStart` over it |
//! | `record_identity` | `IdentityRecord` over the account's open lease |
//! | `report_not_running` | nothing: the gatewayhub says it in ITS journal |
//!
//! ⚠ **`TradeOpen` is answered before `AdapterStart` is sent** (drift register R2): the gatewayhub
//! claims the account's process-wide lock and lease at the open, so a second trading process on the
//! account is refused BEFORE a venue client exists. `tests/remote_backend_wire.rs`'s
//! `the_lease_opens_before_the_adapter_starts` holds the order on the wire.
//!
//! # The gatewayhub is not there (decision 0164, owner ruling D7)
//!
//! [`RemoteBackend::connect`] asks for the table for a bounded `config.gatewayhub_connect_wait_ms`
//! ([`contact`]), **only when some account is active at a non-paper tier** ([`needs_gateway`]): a
//! paper-only box never waits and never dials. When the wait ends without an answer the backend
//! answers the table itself, from the `account` rows alone ([`wire::unavailable_table`]): every
//! `paper` account keeps the row a paper-only box shows, every `demo`/`live` account is NOT RUNNING
//! with `ArmingBlock::GatewayUnavailable`. **Never a paper client built in place of a demo/live
//! account** (decision 0156): the fold gets no engine for it and an order addressed to it is refused
//! by name. The tradehub arms those accounts itself once the gatewayhub answers
//! ([`crate::late_arm`]).
//!
//! # A venue that is not there (the boot is serial per account)
//!
//! The gatewayhub answering does not mean every venue behind it does (ibkr's IB Gateway can have lost its
//! broker link and answer nothing). `start_adapter` therefore waits for the adapter's answer at most the
//! same `config.gatewayhub_connect_wait_ms` (never more than the lease's `request_reply`), and a venue
//! that stays silent is `NotRunning(AdapterFailed)` with the lease detached, not `Unavailable`: the
//! gatewayhub is up, so the late arm must not restart the tradehub into the same silent venue.
//!
//! # What a restart and a gatewayhub restart add (wave 2, W13)
//!
//! * **The journal cursor** rides `TradeOpen.resume` ([`restore::BootHints::cursor_for`], from
//!   `BootRestore::gateway_cursors()`), so the gatewayhub replays what this tradehub missed while it was
//!   down; an account the restore holds no cursor for opens fresh and reconciles.
//! * **The boot wire-id prefetch**: right after the adapter answers `Live` and before any core exists,
//!   the `WireIds` requests (paged) for every coid the restore will ask about ([`restore::BootHints::coids_for`]).
//! * **A gatewayhub restart under a live lease** (a new epoch): [`reteach`] starts the adapter again,
//!   re-teaches every resting order and reconciles at once.
//!
//! # Credentials
//!
//! This type holds the Trade KEY inside the [`GatewayLink`] it was handed and nothing else secret:
//! the `account` snapshot it keeps to derive the unavailable table is the policy's own (rows; once
//! the tradehub's boot takes `NoVenueKeys` it holds no credential value at all). `Debug` shows the
//! address and which scopes have a key, never a key.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use vike_config::account_arming::{account_tier, known_accounts};
use vike_config::{ArmingBlock, VenueArming};
use vike_exec::EventSender;
use vike_exec::recon::ReconClient;
use vike_gatewayhub_client::client::{GatewayLink, TradeOpenError};
use vike_gatewayhub_client::proto;
use vike_gatewayhub_client::trade::{
    CoreLinkSignal, MountedKeys, TradeAccount, TradeError, TradeLink, TradeSession, TradeTimings,
    rpc,
};
use vike_hub_proto::auth::Scope;
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub_core::preflight::{CredentialGap, ServerTimeGap};
use vike_tradehub_core::{
    AdapterBind, AdapterStartQuery, ArmingTable, BackendError, IdentityRecord, NetReading,
    PreflightPlan, PreflightQuery, VenueBackend, Withhold,
};

mod balance_gate;
mod contact;
pub mod facts;
mod legs;
pub mod quoted;
pub mod restore;
pub mod reteach;
pub(crate) mod wire;

use crate::server::starting::{AccountStart, StartProgress};
use crate::server::symbol_admission::{
    SymbolFacts, account_converts, anchor_is_unconfirmed, anchor_with_journal_or_usd,
    quote_lease_converts, quote_lease_reads_currency,
};
use contact::{SystemPacer, wait_for};
use legs::Asking;

/// **Does any account of this box run at a non-paper tier?** — the only reason to ask the
/// gatewayhub anything at start. Read off the `account` rows alone: an account the table itself
/// does not run (two active tiers, none active, an unreadable table) needs no venue either.
#[must_use]
pub fn needs_gateway(accounts: &AccountDirectory) -> bool {
    vike_model::VENUES.iter().any(|venue| {
        known_accounts(venue, Some(accounts)).iter().any(|label| {
            matches!(
                account_tier(Some(accounts), venue, label),
                Ok(AccountTier::Demo | AccountTier::Live)
            )
        })
    })
}

/// The fold's arming table for the one the gatewayhub answered: the conversion `RemoteBackend`
/// reads its own table through, public for the one reader that asks without a backend (the
/// `venues` command, `crate::venues_cli`). An unknown word in the answer is `adapter failed`, never
/// armed; a row this build cannot name is skipped and logged.
#[must_use]
pub fn arming_table_of_wire(table: &proto::ArmingTable) -> ArmingTable {
    wire::table_of_wire(table)
}

/// What the start learned about the gatewayhub, shared by every view of one connection.
#[derive(Clone)]
enum Contact {
    /// No account needs a venue: nothing was asked and nothing was dialled.
    NotNeeded,
    /// The gatewayhub answered the arming query.
    Up(Arc<ArmingTable>),
    /// It did not within the bounded wait (or this tradehub holds no key to ask with); the text is
    /// the last failure's.
    Down(Arc<str>),
}

/// The venues whose bridge can report a `ContractValue` that is not `Linear` 1.0: deribit (inverse
/// perpetuals and futures) and ibkr (an exchange multiplier). A gatewayhub that does not advertise
/// `gw_trade_contract_value` reads `None` for them whatever the bridge knew, so they are NOT RUNNING
/// on it (decision 0170, item 6). The other twelve report none, and `None` is right for them.
const CONTRACT_VALUE_VENUES: [&str; 2] = ["deribit", "ibkr"];

/// The venue port over the gatewayhub. See the module doc.
pub struct RemoteBackend {
    link: GatewayLink,
    client_id: String,
    timings: TradeTimings,
    /// How long the boot waits for ONE account's `AdapterStart` answer (`config.gatewayhub_connect_wait_ms`,
    /// the `wait` of [`RemoteBackend::connect`]); it only ever shortens `timings.request_reply`.
    /// `None` (a backend built without `connect`) waits `request_reply`.
    adapter_start_bound: Option<Duration>,
    /// The `account` snapshot: the unavailable table is derived from its rows.
    accounts: AccountDirectory,
    /// Venues declared `data_only`: their exec credentials are withheld at the start.
    data_only: HashSet<String>,
    contact: Contact,
    /// Whether the preflight's reads are asked of the gatewayhub, set by `preflight_plan`.
    asking: Mutex<Option<Asking>>,
    /// The open leases between `start_adapter` and `record_identity`, by (venue, label text).
    sessions: Mutex<HashMap<(String, Option<String>), TradeSession>>,
    /// The open leases by route key, kept for the whole run so the control door can ask a lease about
    /// a symbol ([`facts::LeaseFacts`]); shared by every view of this connection.
    facts: Arc<facts::LeaseFacts>,
    /// Leases another trade client holds: the composition root refuses the live mount for them,
    /// as the live-account lock's `AddrInUse` always did.
    refusals: Mutex<Vec<String>>,
    /// What the boot restore tells the leases (resume cursors, the coids to prefetch wire ids for).
    boot: Arc<restore::BootHints>,
    /// The core's published snapshot, late-bound: the epoch-change re-teaching reads the resting orders
    /// off it ([`reteach`]).
    cell: reteach::SnapshotCell,
    /// Whether the address this backend dials is on THIS host (loopback). The preflight's clock skew is
    /// measured here against a stamp the gatewayhub read, which is the signer's skew only when the two share
    /// one host clock ([`legs`] module doc); see [`RemoteBackend::server_time_ms`].
    dial_local: bool,
    /// Whether the gatewayhub's project token is compared with this tradehub's project ([`ProjectCheck`]).
    project: ProjectCheck,
    /// The route keys the gatewayhub's last arming table named as running on a key set that differs from
    /// the store's now ([`VenueBackend::key_changed`]); `None` = it was not asked, or did not answer.
    key_changed: Option<std::collections::BTreeSet<String>>,
    /// Where each account's start is reported while the node's door answers `starting`
    /// ([`crate::server::starting`]). `None` (the default) reports nowhere. Shared by every view.
    progress: Option<StartProgress>,
}

impl std::fmt::Debug for RemoteBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteBackend")
            .field("link", &self.link)
            .field("client_id", &self.client_id)
            .field("data_only", &self.data_only)
            .field("gateway", &self.gateway_down().map_or("up", |_| "down"))
            .finish_non_exhaustive()
    }
}

impl RemoteBackend {
    /// **Connect**: ask the gatewayhub for its arming table, for up to `wait`, when some account
    /// needs it ([`needs_gateway`]); never otherwise. `client_id` is this process's id for its
    /// leases (stable across reconnects, unique per process start). Never fails: an unanswered
    /// wait is the [`Contact::Down`] state this type documents.
    #[must_use]
    pub fn connect(
        link: GatewayLink,
        accounts: AccountDirectory,
        client_id: String,
        wait: Duration,
    ) -> Self {
        let contact = if !needs_gateway(&accounts) {
            Contact::NotNeeded
        } else if !link.can_sign(Scope::Trade) {
            // No key is not a thing to wait for: nothing will mint one in a minute.
            let why = format!(
                "this tradehub holds no `{}` (the gatewayhub's order-scope key) in its hub-key \
                 store, so it cannot ask the gatewayhub at {} for anything — mint it on the \
                 gatewayhub's box with `vike-cli gatewayhub setup`",
                vike_venues::credential_keys::gatewayhub_trade_key(),
                link.addr()
            );
            tracing::error!("{why}");
            Contact::Down(why.into())
        } else {
            let mut ask = || rpc::arming_table(&link);
            match wait_for(&mut ask, wait, &SystemPacer, "tradehub start") {
                Ok(table) => Contact::Up(Arc::new(wire::table_of_wire(&table))),
                Err(why) => {
                    tracing::error!(
                        "the gatewayhub did not answer within {} ms ({why}): every demo/live \
                         account is NOT RUNNING with `gatewayhub unavailable` (paper accounts are \
                         unaffected, and no paper client stands in for the others); the tradehub \
                         arms them itself once the gatewayhub answers",
                        wait.as_millis()
                    );
                    Contact::Down(why.into())
                }
            }
        };
        let mut backend = RemoteBackend::from_parts(link, accounts, client_id, contact);
        backend.adapter_start_bound = Some(wait);
        // The boot re-admission's facts looks wait the same bound ([`facts::LeaseFacts::begin_boot`]).
        backend.facts.set_boot_wait(wait);
        backend
    }

    fn from_parts(
        link: GatewayLink,
        accounts: AccountDirectory,
        client_id: String,
        contact: Contact,
    ) -> Self {
        let dial_local = dial_is_local(link.addr());
        RemoteBackend {
            link,
            client_id,
            timings: TradeTimings::default(),
            adapter_start_bound: None,
            accounts,
            data_only: HashSet::new(),
            contact,
            asking: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            facts: Arc::new(facts::LeaseFacts::default()),
            refusals: Mutex::new(Vec::new()),
            boot: Arc::new(restore::BootHints::none()),
            cell: reteach::SnapshotCell::default(),
            dial_local,
            project: ProjectCheck::NotAsked,
            key_changed: None,
            progress: None,
        }
    }

    /// Report each account's start to `progress` ([`StartProgress`]); `None` reports nowhere. The
    /// composition root says it while the node's door answers `starting`.
    #[must_use]
    pub fn with_start_progress(mut self, progress: Option<StartProgress>) -> Self {
        self.progress = progress;
        self
    }

    /// Compare the gatewayhub's project token (`TradeOpened.project_token`) with the project at
    /// `state_dir` (this tradehub's declared project state directory; `None` = it resolved no project)
    /// at every lease open, and refuse the account when they differ ([`project_refusal`]). The
    /// production root says it ([`crate::tradehub_cli`]'s `RemoteCtor`); a backend built without it asks
    /// nothing (the wire tests' scripted peers carry a token of their own).
    #[must_use]
    pub fn with_project_state_dir(mut self, state_dir: Option<std::path::PathBuf>) -> Self {
        self.project = ProjectCheck::Against(state_dir);
        self
    }

    /// Another view of the SAME connection (the table fetched once, the same ids) over a different
    /// `account` snapshot and `data_only` set: the plan loop reads the full-credential rows, the
    /// mount reads them after the `data_only` withhold, and neither asks the gatewayhub twice.
    #[must_use]
    pub fn view(&self, accounts: AccountDirectory, data_only: HashSet<String>) -> Self {
        RemoteBackend {
            data_only,
            timings: self.timings,
            adapter_start_bound: self.adapter_start_bound,
            boot: Arc::clone(&self.boot),
            facts: Arc::clone(&self.facts),
            cell: Arc::clone(&self.cell),
            project: self.project.clone(),
            key_changed: self.key_changed.clone(),
            progress: self.progress.clone(),
            ..RemoteBackend::from_parts(
                self.link.clone(),
                accounts,
                self.client_id.clone(),
                self.contact.clone(),
            )
        }
    }

    /// **Another view of the SAME connection over a FRESH arming table** (decision 0169): the gatewayhub is
    /// asked again, once, now, because an account became active while this process runs and the table it
    /// answered at start does not know it. `accounts` are the rows read now and `data_only` the venues
    /// the mount withholds, as for [`Self::view`]. Never fails, like [`Self::connect`]: a gatewayhub that
    /// does not answer is the [`Contact::Down`] state, and the account the caller then starts is NOT
    /// RUNNING `gatewayhub unavailable` (this one dial, no wait loop: the caller is a watcher thread,
    /// not a start).
    #[must_use]
    pub fn refreshed(&self, accounts: AccountDirectory, data_only: HashSet<String>) -> Self {
        let mut key_changed = None;
        let contact = if !needs_gateway(&accounts) {
            Contact::NotNeeded
        } else if !self.link.can_sign(Scope::Trade) {
            Contact::Down(
                "this tradehub holds no gatewayhub order-scope key, so it cannot ask the gatewayhub \
                 for anything"
                    .into(),
            )
        } else {
            match rpc::arming_table(&self.link) {
                Ok(table) => {
                    key_changed = Some(table.key_changed.iter().cloned().collect());
                    Contact::Up(Arc::new(wire::table_of_wire(&table)))
                }
                Err(e) => Contact::Down(e.to_string().into()),
            }
        };
        RemoteBackend {
            data_only,
            timings: self.timings,
            // Same as `view`: a hot-started account waits the boot bound, and its lease lands in the
            // SAME `LeaseFacts` the control door reads (a throwaway one hides it from any-symbol).
            adapter_start_bound: self.adapter_start_bound,
            boot: Arc::clone(&self.boot),
            facts: Arc::clone(&self.facts),
            cell: Arc::clone(&self.cell),
            project: self.project.clone(),
            key_changed,
            ..RemoteBackend::from_parts(
                self.link.clone(),
                accounts,
                self.client_id.clone(),
                contact,
            )
        }
    }

    /// What the boot restore tells the leases: the resume cursor each account opens with and the restored
    /// client order ids a wire-id prefetch is owed for ([`restore::BootHints`]). Default: none (a fresh
    /// start: every lease opens with no cursor and nothing is prefetched).
    #[must_use]
    pub fn with_boot(mut self, boot: restore::BootHints) -> Self {
        self.boot = Arc::new(boot);
        self
    }

    /// The cell the epoch-change re-teaching reads the core's resting orders from; the composition root fills
    /// it with the core's snapshot once the core is up. Shared by every view of this connection.
    #[must_use]
    pub fn with_snapshot_cell(mut self, cell: reteach::SnapshotCell) -> Self {
        self.cell = cell;
        self
    }

    /// Other waits of the lease; see [`TradeTimings`] (raise-only in production).
    #[must_use]
    pub fn with_timings(mut self, timings: TradeTimings) -> Self {
        self.timings = timings;
        self
    }

    /// The link the late-arm poll and the one-shots dial from.
    #[must_use]
    pub fn link(&self) -> &GatewayLink {
        &self.link
    }

    /// The source the control door looks a new symbol's facts up in: the open leases of this
    /// connection, by route key ([`facts::LeaseFacts`]). Shared by every view; empty until an
    /// adapter has started.
    #[must_use]
    pub fn symbol_facts(&self) -> Arc<facts::LeaseFacts> {
        Arc::clone(&self.facts)
    }

    /// Why the gatewayhub is not there, when it is not (the last failure's text).
    #[must_use]
    pub fn gateway_down(&self) -> Option<&str> {
        match &self.contact {
            Contact::Down(why) => Some(why),
            Contact::NotNeeded | Contact::Up(_) => None,
        }
    }

    /// The leases another trade client holds, as the sentence the refusal prints. Empty on a clean
    /// start. The live mount is refused when it is not (the old lock's behaviour, kept).
    #[must_use]
    pub fn lease_refusals(&self) -> Vec<String> {
        self.refusals.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The two things a started adapter is owed before the core can be spawned over it.
    ///
    /// * **The wire-id prefetch** for every coid the restore will ask about, ON THIS (the boot) thread:
    ///   `wire_id_for` runs on the fold thread, which may not wait for the gatewayhub. A failure is
    ///   said at `error` and is NOT fatal: the prefetch stays owed, the reconcile client answers NOT
    ///   READY until it has been paid (it retries it itself), and a miss at fold time is logged and
    ///   answers `None`.
    /// * **The epoch-change worker** ([`reteach`]) for the lease's lifetime.
    fn prefetch_and_watch(
        &self,
        session: &TradeSession,
        start: proto::AdapterStart,
        trigger: Option<Sender<()>>,
        epoch_rx: std::sync::mpsc::Receiver<(u64, u64)>,
        coids: &[String],
        venue: String,
        label: Option<String>,
        tier: AccountTier,
    ) {
        if !coids.is_empty()
            && let Err(e) = session.prefetch_wire_ids(coids)
        {
            tracing::error!(
                venue = %venue, coids = coids.len(),
                "the boot wire-id prefetch failed ({e}): this account's reconcile is NOT READY until \
                 it is paid, so a restore never plans with an empty translation map"
            );
        }
        let worker = reteach::Reteach {
            session: session.clone(),
            start,
            venue,
            label,
            tier,
            trigger,
            cell: Arc::clone(&self.cell),
        };
        if let Err(e) = worker.spawn(epoch_rx) {
            tracing::error!(
                "the gatewayhub-restart worker could not start ({e}): after a gatewayhub restart \
                 this account's adapter would not be re-started by the tradehub"
            );
        }
    }

    fn refuse(&self, why: String) {
        self.refusals.lock().unwrap_or_else(PoisonError::into_inner).push(why);
    }

    /// The table this backend answers, before the `data_only` overlay.
    fn base_table(&self) -> ArmingTable {
        match &self.contact {
            Contact::Up(table) => (**table).clone(),
            Contact::NotNeeded | Contact::Down(_) => wire::unavailable_table(&self.accounts),
        }
    }

    /// The venues with an account the table runs at a non-paper tier: the credential leg's list when
    /// the preflight query itself got no answer.
    fn armed_venues(&self) -> Vec<String> {
        let table = wire::with_data_only(&self.base_table(), &self.data_only);
        let mut venues: Vec<String> = table
            .rows()
            .iter()
            .filter(|row| {
                row.arming.effective.is_some_and(|tier| tier != AccountTier::Paper)
                    && row.arming.block.runs()
            })
            .map(|row| row.arming.venue.to_string())
            .collect();
        venues.sort();
        venues.dedup();
        venues
    }

    fn asking(&self) -> Option<Asking> {
        self.asking.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// ONE preflight question of `venues`, asked of the gatewayhub now. The fold's reads are blocking and
    /// sequential, so each is one dial and one answer.
    fn ask(
        &self,
        venues: &[String],
        step: proto::PreflightStep,
    ) -> std::io::Result<proto::PreflightAnswer> {
        rpc::preflight(&self.link, venues, step)
    }

    /// A failed open or start, as the port's two answers. Logged here, in the words the daemon
    /// used, because an operator reads THIS journal first.
    fn open_failed(&self, q: &AdapterStartQuery<'_>, e: TradeOpenError) -> BackendError {
        let (venue, account) = (q.venue, q.account);
        match e {
            // A refusal no retry can change (the daemon predates the verb or carries no adapter for
            // this venue: version skew; the daemon refuses the key) is NOT "the gatewayhub is down": it
            // must not wait for the late arm, whose restart would meet the same refusal every cycle.
            // The account is NOT RUNNING `adapter failed` and an operator fixes the deployment. Said
            // once, here.
            TradeOpenError::Link(why) if link_refusal_is_fixed(&why) => {
                tracing::error!(
                    venue, account = %account, kind = ?why.kind(),
                    "{venue}: the gatewayhub cannot serve this account's trade lease and retrying will \
                     not change that (a deploy version skew, or a key it refuses): {why}. The account \
                     is NOT RUNNING (`adapter failed`) and nothing retries it by itself: fix the \
                     deployment (upgrade the gatewayhub, or correct the key it refuses), then set the \
                     account's `account.active` off and on again to start it"
                );
                BackendError::NotRunning(ArmingBlock::AdapterFailed)
            }
            TradeOpenError::Link(why) => {
                BackendError::Unavailable(format!("{venue}: could not open the trade lease: {why}"))
            }
            TradeOpenError::LeaseHeld { holder_key_id, since_ms } => {
                let why = format!(
                    "{}: the gatewayhub's trade lease for this account is held by {holder_key_id} \
                     since epoch ms {since_ms} — another tradehub (or a tool) is trading it, and \
                     two live processes on one venue account is the accident the lease exists to \
                     refuse. Stop the other process, or take the lease over explicitly",
                    q.route_key
                );
                tracing::error!(venue, account = %account, "{why}");
                self.refuse(why);
                BackendError::NotRunning(ArmingBlock::AdapterFailed)
            }
            TradeOpenError::Refused { block_word, text } => {
                let block = wire::refusal_block(&block_word, venue);
                tracing::warn!(
                    venue, account = %account, block_word = %block_word,
                    "{venue}: the gatewayhub refused the trade lease: {text}"
                );
                BackendError::NotRunning(block)
            }
            TradeOpenError::Daemon(text) => {
                tracing::error!(
                    venue, account = %account,
                    "{venue}: the gatewayhub answered the trade lease with an error: {text}"
                );
                BackendError::NotRunning(ArmingBlock::AdapterFailed)
            }
        }
    }

    /// A failed request over an OPEN lease. A link that is down, lost or silent is "the gatewayhub is
    /// unavailable"; a daemon that lacks the verb (version skew, fixed until it is upgraded) or spoke
    /// nonsense is a failed adapter.
    fn request_failed(
        venue: &str,
        account: &impl std::fmt::Display,
        e: TradeError,
    ) -> BackendError {
        match e {
            TradeError::Unsupported(text) => {
                // The link lacks a verb the daemon does not advertise: version skew, fixed until the
                // daemon is upgraded (see `link_refusal_is_fixed`), so not a late-arm wait.
                tracing::error!(
                    venue, account = %account,
                    "{venue}: {text}. The account is NOT RUNNING (`adapter failed`) and nothing retries \
                     it by itself: upgrade the gatewayhub, then set the account's `account.active` off \
                     and on again to start it"
                );
                BackendError::NotRunning(ArmingBlock::AdapterFailed)
            }
            TradeError::LinkDown(text) | TradeError::LinkLost(text) | TradeError::Timeout(text) => {
                BackendError::Unavailable(text)
            }
            TradeError::Protocol(text) => {
                tracing::error!(venue, account = %account, "{venue}: {text}");
                BackendError::NotRunning(ArmingBlock::AdapterFailed)
            }
        }
    }
}

/// One account's start, reported to the node's `starting` door: `starting` when it is made, `up`
/// when [`AccountReport::up`] is called, and NOT RUNNING when it is dropped without that (every
/// refusal and every early return of `start_adapter` ends that way, a panic included).
struct AccountReport {
    progress: Option<StartProgress>,
    route_key: String,
    up: bool,
}

impl AccountReport {
    fn begin(progress: Option<&StartProgress>, route_key: &str) -> Self {
        if let Some(progress) = progress {
            progress.account(route_key, AccountStart::Starting);
        }
        AccountReport { progress: progress.cloned(), route_key: route_key.to_string(), up: false }
    }

    fn up(&mut self) {
        self.up = true;
        if let Some(progress) = &self.progress {
            progress.account(&self.route_key, AccountStart::Up);
        }
    }
}

impl Drop for AccountReport {
    fn drop(&mut self) {
        if let (false, Some(progress)) = (self.up, &self.progress) {
            progress.account(
                &self.route_key,
                AccountStart::NotRunning("did not start, see the journal".to_string()),
            );
        }
    }
}

/// Whether a failed lease OPEN is a refusal that trying again cannot change: a verb or venue the daemon
/// does not advertise ([`std::io::ErrorKind::Unsupported`], the client's local refusal), a key the daemon
/// will not accept ([`std::io::ErrorKind::PermissionDenied`]) or a peer that is not this protocol
/// ([`std::io::ErrorKind::InvalidData`]). Every other link error (a refused or timed-out dial, a reset) is
/// the gatewayhub being away, which the late arm waits out.
fn link_refusal_is_fixed(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        e.kind(),
        ErrorKind::Unsupported | ErrorKind::PermissionDenied | ErrorKind::InvalidData
    )
}

/// Whether every address `addr` (`host:port`) resolves to is loopback, i.e. the gatewayhub dialled is on
/// this host. An address that resolves to nothing, or to anything off-host, is not local.
fn dial_is_local(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    addr.to_socket_addrs().is_ok_and(|resolved| {
        let all: Vec<_> = resolved.collect();
        !all.is_empty() && all.iter().all(|a| a.ip().is_loopback())
    })
}

/// Why the preflight's clock leg reads no skew when the gatewayhub is not on this host.
const REMOTE_GATEWAYHUB_REASON: &str = "not checked (remote gatewayhub): the gatewayhub's address is \
     not loopback, so a skew measured against this host's clock is not the clock that signs";
/// What that costs.
const REMOTE_GATEWAYHUB_AT_STAKE: &str = "orders are signed with the gatewayhub's clock, whose skew \
     against the venue was not measured here";

/// The file under the project state directory that holds the project token the gatewayhub made
/// (`vike_gatewayhub::trade::state_dir::PROJECT_TOKEN_FILE`; a test holds the two spellings equal).
const PROJECT_TOKEN_FILE: &str = "gateway-id";

/// Whether this backend compares the gatewayhub's project token with its own project.
#[derive(Debug, Clone)]
enum ProjectCheck {
    /// Nothing was said: no comparison.
    NotAsked,
    /// Compare against the token file under this project state directory (`None` = no project resolved).
    Against(Option<std::path::PathBuf>),
}

/// **Do the tradehub and the gatewayhub name one project?** The HALT sentinel, the journals and the
/// live-account locks all live under the project's state directory, so two processes on different projects
/// would each believe the other's safety files were theirs. The gatewayhub proves its project by the token
/// in `<state_dir>/gateway-id` (it made the file before it answered the open), which this tradehub reads
/// from ITS state directory. `Some(words)` is the refusal; `None` is "the same project" or "nothing to
/// compare" (the gatewayhub states no token: it has no project state directory). No token is ever printed.
fn project_refusal(state_dir: Option<&std::path::Path>, theirs: &str) -> Option<String> {
    if theirs.is_empty() {
        return None;
    }
    let Some(dir) = state_dir else {
        return Some(
            "this tradehub resolved no project state directory, but the gatewayhub works in one \
             (it states a project token): they are not the same project"
                .to_string(),
        );
    };
    let file = dir.join(PROJECT_TOKEN_FILE);
    match std::fs::read_to_string(&file) {
        Ok(text) if text.trim() == theirs => None,
        Ok(_) => Some(format!(
            "the project token in {} differs from the gatewayhub's: the two processes were started \
             from different projects",
            file.display()
        )),
        Err(e) => Some(format!(
            "this tradehub cannot read the project token at {} ({e}), so it cannot show that the \
             gatewayhub works in the same project",
            file.display()
        )),
    }
}

/// The wire's reason to withhold credentials for the fold's. Exhaustive.
fn withhold_to_wire(withhold: Withhold) -> proto::Withhold {
    match withhold {
        Withhold::PreflightFailed => proto::Withhold::PreflightFailed,
        Withhold::DataOnly => proto::Withhold::DataOnly,
    }
}

impl VenueBackend for RemoteBackend {
    fn arming_table(&self) -> Result<ArmingTable, BackendError> {
        Ok(wire::with_data_only(&self.base_table(), &self.data_only))
    }

    fn key_changed(&self) -> Option<std::collections::BTreeSet<String>> {
        self.key_changed.clone()
    }

    fn preflight_plan(&self, q: &PreflightQuery<'_>) -> Result<PreflightPlan, BackendError> {
        let answered = match &self.contact {
            Contact::Up(_) => match self.ask(q.clock_venues, proto::PreflightStep::Plan) {
                Ok(answer) => answer.plan.ok_or_else(|| {
                    "the gatewayhub answered the preflight plan with no venue lists: it predates the \
                     per-step preflight (a gatewayhub older than this tradehub: deploy the \
                     gatewayhub first)"
                        .to_string()
                }),
                Err(e) => Err(e.to_string()),
            },
            Contact::NotNeeded => Err("no account needs a venue".to_string()),
            Contact::Down(why) => Err(why.to_string()),
        };
        let plan = match answered {
            Ok(lists) => {
                *self.asking.lock().unwrap_or_else(PoisonError::into_inner) = Some(Asking::Up);
                PreflightPlan {
                    clock_policies: legs::policies_of(&lists),
                    wired_clock_reads: lists.wired_clock_reads as usize,
                    credential_venues: lists.credential_venues,
                }
            }
            Err(why) => {
                // Not an `Err` to the fold: it would drop the credential leg altogether, and a venue
                // whose legs were never measured must show a WARN row, not no row.
                if matches!(self.contact, Contact::Up(_)) {
                    tracing::error!(
                        "the startup preflight could not ask the gatewayhub for its plan ({why}): \
                         every venue leg reads `did not answer`, none reads green"
                    );
                }
                let table = self.arming_table()?;
                let wired = q
                    .clock_venues
                    .iter()
                    .filter(|v| table.facts_of(v).is_some_and(|f| f.clock_decl_word == "wired"))
                    .count();
                *self.asking.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(Asking::Failed(why));
                PreflightPlan {
                    clock_policies: HashMap::new(),
                    wired_clock_reads: wired,
                    credential_venues: self.armed_venues(),
                }
            }
        };
        Ok(plan)
    }

    fn server_time_ms(&self, venue: &str, _tier: AccountTier) -> Result<i64, ServerTimeGap> {
        // The skew the fold computes is the venue stamp against THIS host's clock, and it is the signer's
        // skew only when the gatewayhub shares the host. A gatewayhub dialled off-host (a tunnel or forward
        // to a remote box's loopback passes its bind guard) would make the reading the clock OFFSET between
        // two boxes: not asked, and never green (an unmeasured hazard, rendered WARN).
        if !self.dial_local {
            return Err(ServerTimeGap::UnmeasuredRisk {
                reason: REMOTE_GATEWAYHUB_REASON,
                at_stake: REMOTE_GATEWAYHUB_AT_STAKE,
            });
        }
        match self.asking() {
            None => Err(ServerTimeGap::Unreachable("no preflight plan was asked".to_string())),
            Some(Asking::Failed(why)) => Err(legs::clock_unanswered(&why)),
            Some(Asking::Up) => match self.ask(&[venue.to_string()], proto::PreflightStep::Clock) {
                Ok(answer) => legs::server_time_of(venue, &answer.legs),
                Err(e) => Err(legs::clock_unanswered(&e.to_string())),
            },
        }
    }

    fn authed_read(&self, venue: &str) -> Result<(), CredentialGap> {
        match self.asking() {
            None => {
                Err(CredentialGap::Rejected(format!("no credential probe was planned for {venue}")))
            }
            Some(Asking::Failed(why)) => Err(legs::credential_unanswered(&why)),
            Some(Asking::Up) => {
                match self.ask(&[venue.to_string()], proto::PreflightStep::Credential) {
                    Ok(answer) => legs::authed_read_of(venue, &answer.legs),
                    Err(e) => Err(legs::credential_unanswered(&e.to_string())),
                }
            }
        }
    }

    fn net_reading(&self) -> NetReading {
        match self.asking() {
            Some(Asking::Up) => match self.ask(&[], proto::PreflightStep::Network) {
                Ok(answer) => legs::net_reading_of(&answer.legs, vike_model::now_ms()),
                Err(e) => {
                    tracing::warn!("the gatewayhub did not answer the network reading ({e})");
                    NetReading::Unknown
                }
            },
            None | Some(Asking::Failed(_)) => NetReading::Unknown,
        }
    }

    fn report_not_running(&self, _venue: &str, _rows: &[VenueArming]) {
        // The bridge speaks for the accounts the fan-out never starts (a stored key set that is
        // unused or incomplete) in the gatewayhub's own journal, where the credentials are read.
    }

    fn start_adapter(
        &self,
        q: AdapterStartQuery<'_>,
        events: &EventSender,
        trigger: Option<Sender<()>>,
    ) -> Result<AdapterBind, BackendError> {
        let Contact::Up(table) = &self.contact else {
            return Err(BackendError::Unavailable(
                self.gateway_down()
                    .map_or_else(|| "no gatewayhub was asked".to_string(), str::to_string),
            ));
        };
        let mut report = AccountReport::begin(self.progress.as_ref(), q.route_key);
        let takes_trigger =
            trigger.is_some() && table.facts_of(q.venue).is_some_and(|f| f.takes_recon_trigger);
        let account = TradeAccount {
            client_id: self.client_id.clone(),
            venue: q.venue.to_string(),
            label: q.account.text().map(str::to_string),
            tier: q.tier,
            route_key: q.route_key.to_string(),
        };
        // The keys the link's loss is disclosed on: the engine's own, and the declared legs'.
        let keys = MountedKeys::new();
        keys.add(q.venue, q.symbol);
        for leg in q.declared_legs {
            keys.add(q.venue, leg);
        }
        // The hooks W13 hangs on the lease live HERE, in this one builder: the resume cursor (the journal's,
        // for this account's route key; none = open fresh and reconcile), and the signal that also
        // carries a gatewayhub restart (a new epoch) to the re-teaching worker ([`reteach`]).
        let resume = self.boot.cursor_for(q.route_key);
        let (epoch_tx, epoch_rx) = std::sync::mpsc::channel();
        let signal = reteach::ReteachSignal::new(
            CoreLinkSignal::new(events, &account, keys.clone()),
            epoch_tx,
        );
        let reconcile_now = trigger.clone();
        let (venue, label, tier) = (account.venue.clone(), account.label.clone(), account.tier);
        let coids = self.boot.coids_for(q.venue, label.as_deref());
        let mut link = TradeLink::new(self.link.clone(), account, events.clone())
            .with_timings(self.timings)
            .with_halt_path(vike_boot::halt::halt_path_from_env())
            .with_resume(resume)
            .with_signal(Arc::new(signal));
        // The link's own gap paths (a ring replay the daemon could not serve, a lost frame, a
        // `ReconcileNow`) poke the manager for EVERY venue: `takes_recon_trigger` says whether the BRIDGE's
        // own resync supervisor takes the trigger (what `AdapterStart.takes_trigger` tells the daemon),
        // not whether this client may ask for a pass when the gatewayhub's event stream has a hole.
        if let Some(trigger) = trigger {
            link = link.with_trigger(trigger);
        }
        // ⚠ THE LEASE FIRST (R2): the open is answered, claiming the account's lock at the gatewayhub,
        // before the adapter start below is sent.
        let session = link.open().map_err(|e| self.open_failed(&q, e))?;
        if let ProjectCheck::Against(state_dir) = &self.project {
            let theirs = session.opened().map(|o| o.project_token).unwrap_or_default();
            if theirs.is_empty() {
                tracing::warn!(
                    venue = q.venue, account = %q.account,
                    "{}: the gatewayhub states no project token (it works in no project state \
                     directory), so the two processes cannot be shown to share one project",
                    q.venue
                );
            }
            if let Some(why) = project_refusal(state_dir.as_deref(), &theirs) {
                tracing::error!(
                    venue = q.venue, account = %q.account,
                    "{}: REFUSED — {why}. The HALT sentinel, the journals and the live-account locks \
                     live under the project, so this account is NOT RUNNING (`adapter failed`) until \
                     both processes run from one project. Nothing retries it by itself: start the \
                     process that is in the wrong project from the right one (if that is this \
                     tradehub, in your next batched restart), then set the account's \
                     `account.active` off and on again to start it",
                    q.venue
                );
                session.detach();
                return Err(BackendError::NotRunning(ArmingBlock::AdapterFailed));
            }
        }

        let withhold = q
            .withhold
            .map(withhold_to_wire)
            .or_else(|| self.data_only.contains(q.venue).then_some(proto::Withhold::DataOnly));
        // Whether the account has an exposure ceiling armed: the opt-in for converting a quote currency. For a
        // hot start the registry was told the engine's own answer before this start; for the boot, the root bound
        // the rows the boot built the engines from before the core was assembled.
        let ceiling = SymbolFacts::exposure_cap_armed(self.facts.as_ref(), q.route_key);
        let reads_currency = quote_lease_reads_currency(q.venue, q.tier, ceiling);
        let converts = account_converts(q.venue, q.tier, ceiling);
        let mut start = proto::AdapterStart {
            rid: 0,
            symbol: q.symbol.to_string(),
            declared_legs: q.declared_legs.to_vec(),
            recon_enabled: q.recon_enabled,
            takes_trigger,
            risk: q.risk_profile.map(|p| proto::AdapterRisk { max_leverage: p.max_leverage }),
            market_slippage: q.market_slippage,
            halt_admit: q.halt_admit,
            withhold,
            account_leverage: q.account_leverage,
            // An ibkr or ig account that will convert asks its start to read the currency (a dial of a scarce
            // budget, a login out of an allowance); every other start asks nothing, as it always did.
            read_account_ccy: reads_currency,
            route_key: String::new(),
        };
        // ONE silent venue (ibkr's gateway without its broker link answers nothing for minutes) may not
        // hold the serial boot: the wait is bounded, and the account stops with its own word.
        let bound = self
            .adapter_start_bound
            .map_or(self.timings.request_reply, |b| b.min(self.timings.request_reply));
        // THE MOUNT the session is started on: the symbol asked for, or - once, and only for the one account
        // `moved_mount` names - the pair the venue's own statement of the account's money asks for.
        let mut mount = q.symbol.to_string();
        let mut moved = false;
        let reply = loop {
            // The SECOND start has to fit the teardown of the first adapter in front of its own login (the
            // gatewayhub joins the first one's exec thread, still inside its login, for up to its full join
            // wait), so it gets twice the room: a slow tail must not leave the account NOT RUNNING where the
            // wired mount would have run it.
            let this_bound = if moved { bound.saturating_mul(2) } else { bound };
            let reply = match session.start_adapter_within(start.clone(), this_bound) {
                Ok(reply) => reply,
                // The gatewayhub IS there (the lease opened); the venue did not answer. Not `Unavailable`:
                // that is `gatewayhub unavailable`, whose late restart would meet the same silent venue
                // again at every start.
                Err(TradeError::Timeout(text)) => {
                    session.detach();
                    tracing::error!(
                        venue = q.venue, account = %q.account,
                        "{}: the venue did not answer the adapter start within {this_bound:?} ({text}); this \
                         account is NOT RUNNING (`adapter failed`) and the other accounts start without \
                         it. Fix the venue's side (for ibkr: IB Gateway's link to the broker), then set \
                         the account's `account.active` off and on again to retry it; nothing retries \
                         it by itself",
                        q.venue
                    );
                    return Err(BackendError::NotRunning(ArmingBlock::AdapterFailed));
                }
                Err(e) => {
                    session.detach();
                    return Err(Self::request_failed(q.venue, q.account, e));
                }
            };
            match (!moved).then(|| moved_mount(&q, converts, &mount, &reply)).flatten() {
                Some(pair) => {
                    let money = match &reply {
                        proto::AdapterReply::Live { account_ccy: Some(money), .. } => {
                            money.as_str()
                        }
                        _ => "",
                    };
                    tracing::warn!(
                        venue = q.venue, account = %q.account,
                        "{}: the venue states the account's money is {money}, which the wired mount `{mount}` \
                         does not count in, so its ledger could not be anchored to it. Starting the session \
                         again on `{pair}`, a pair quoted in {money}: the same lease, the gatewayhub replaces \
                         the adapter",
                        q.venue
                    );
                    keys.add(q.venue, pair);
                    start.symbol = pair.to_string();
                    mount = pair.to_string();
                    moved = true;
                }
                None => break reply,
            }
        };
        match reply {
            proto::AdapterReply::NotRunning { block_word, refusal_text } => {
                let block = wire::refusal_block(&block_word, q.venue);
                // The bridge's own refusal words, as the gatewayhub produced them.
                tracing::warn!(
                    venue = q.venue, account = %q.account, block_word = %block_word,
                    "{}: the gatewayhub did not start the adapter: {refusal_text}", q.venue
                );
                session.detach();
                Err(BackendError::NotRunning(block))
            }
            proto::AdapterReply::Live {
                bound_tier,
                grid,
                contract_value,
                margin_mode,
                leg_grids,
                recon,
                leverage_cap,
                account_ccy,
                account_ccy_assumed,
            } => {
                // Defence in depth (the gatewayhub's own interlock held it too): the client the
                // daemon bound must be at exactly the account's tier. Above it is a live order path
                // the operator did not ask for; below it is a downgrade the account never agreed to.
                if bound_tier != q.tier {
                    tracing::error!(
                        venue = q.venue, account = %q.account,
                        "{}: the gatewayhub bound a client at tier {bound_tier}, but the account's \
                         row states {} — refused, the account does NOT RUN",
                        q.venue, q.tier
                    );
                    session.detach();
                    return Err(BackendError::NotRunning(ArmingBlock::AdapterFailed));
                }
                // DECISION 0170, item 6: a daemon that predates `contract_value` answers every start with
                // the field ABSENT, which reads `None` (Linear, 1.0). Right for a venue whose bridge
                // reports none; WRONG for one that can report an inverse contract or an exchange
                // multiplier, so those are NOT RUNNING with the reason rather than quietly sized at
                // the wrong kind.
                if CONTRACT_VALUE_VENUES.contains(&q.venue) && !session.reports_contract_value() {
                    tracing::error!(
                        venue = q.venue, account = %q.account,
                        "{}: REFUSED — the gatewayhub predates contract_value (it does not advertise \
                         \"{}\"), so this venue's contract kind and factor cannot be known and a \
                         guess would size every order at the wrong notional. The account is NOT \
                         RUNNING until the gatewayhub is upgraded",
                        q.venue, vike_gatewayhub_client::proto::FEATURE_GW_TRADE_CONTRACT_VALUE
                    );
                    session.detach();
                    return Err(BackendError::NotRunning(ArmingBlock::AdapterFailed));
                }
                // FAIL CLOSED FOR MONEY: an ig account that asked for the conversion (an exposure ceiling armed,
                // the only start that reads its currency) and whose currency is not the one the adapter books
                // orders in has a ledger no label can make right (`ig_account_refusal`). It does NOT RUN; an
                // account with no ceiling reads nothing and is not judged.
                if reads_currency
                    && let Some(why) = crate::server::symbol_admission::ig_account_refusal(
                        q.venue,
                        account_ccy.as_deref(),
                    )
                {
                    tracing::error!(
                        venue = q.venue, account = %q.account,
                        "{}: REFUSED — {why}. This account is NOT RUNNING (`adapter failed`) because its \
                         exposure ceiling asked the node to convert quote currencies. Nothing retries it by \
                         itself: remove the account's exposure ceiling (an ig account with none runs as it \
                         always did, its ledger unlabelled), or use a USD ig account, then set \
                         `account.active` off and on again",
                        q.venue
                    );
                    session.detach();
                    return Err(BackendError::NotRunning(ArmingBlock::AdapterFailed));
                }
                self.prefetch_and_watch(
                    &session,
                    start,
                    reconcile_now,
                    epoch_rx,
                    &coids,
                    venue,
                    label,
                    tier,
                );
                let recon_client: Option<Box<dyn ReconClient>> =
                    recon.then(|| Box::new(session.recon_client()) as Box<dyn ReconClient>);
                let client = Box::new(session.exec_client());
                // THE CURRENCY THE LEDGER IS ANCHORED TO: the venue's word, or - for an account that
                // converts, whose gatewayhub could not read it right now (a data lane that refused the dial, a
                // slow summary: a restart is when that is most likely) - the one the previous run's ledger was
                // anchored to, which the journal carries when the engine held a symbol in another currency
                // (`anchor_with_journal` states the rule), or, with neither, USD ASSUMED (the owner's ruling,
                // `anchor_with_journal_or_usd`). Without an anchor the restored engine would be assembled
                // unanchored: its held foreign positions would keep the rate they were restored with, and the
                // core would skip a default account's journaled engine (its ledger label differs from the
                // journal's), flat while the venue holds the position. An assumed currency is flagged on the
                // bind and the engine's assembly says it at `warn`, as for fxcm's lane-assumed one.
                let journaled = self.boot.fx_for(q.route_key);
                let stated = account_ccy.filter(|ccy| !ccy.trim().is_empty());
                // What the VENUE itself said: a currency the gatewayhub filled in for a silent venue
                // (`account_ccy_assumed` on the reply) is its silence, not its word.
                let venue_stated = stated.is_some() && !account_ccy_assumed;
                // ...kept for the balance gate below (`stated` is consumed by the anchor).
                let venue_ccy = stated.clone().filter(|_| venue_stated);
                let (account_ccy, account_ccy_assumed) = anchor_with_journal_or_usd(
                    q.venue,
                    converts,
                    stated,
                    account_ccy_assumed,
                    journaled.map(|fx| fx.account_ccy.as_str()),
                )
                .map_or((None, false), |(ccy, assumed)| (Some(ccy), assumed));
                // The currency may anchor the engine's ledger only if the engine's own mounted contract
                // counts money in it (ibkr: the balance it reads is that contract's cash; every other venue: the
                // ENGINE's own fence, the same decision its assembly takes, so a lease the engine is not told
                // a currency for is never anchored here either). The words are kept for the door's refusal.
                // (Judged only for an account that converts, as the engine's assembly does: an account with no
                // ceiling anchors nothing either way and its start says nothing about it.)
                let fence = account_ccy.as_deref().filter(|_| converts).and_then(|ccy| {
                    let why =
                        crate::server::symbol_admission::unanchorable_mount(q.venue, &mount, ccy)?;
                    tracing::warn!(
                        venue = q.venue, account = %q.account,
                        "{}: the account's currency ({ccy}) is not the ledger's: {why}. A symbol \
                         quoted in another currency stays refused", q.venue
                    );
                    Some(why)
                });
                // ...the currency the fence declined, kept for the balance gate's words below.
                let fenced_ccy = account_ccy.clone().filter(|_| fence.is_some());
                let account_ccy = account_ccy.filter(|_| fence.is_none());
                // An assumption nobody anchored the ledger to is no assumption (the fence's words are logged).
                let account_ccy_assumed = account_ccy_assumed && account_ccy.is_some();
                // An anchor nobody confirmed is checkable only where the gatewayhub's rates state their ledger
                // (ibkr's); anywhere else the door admits no symbol quoted in another currency on it.
                let unconfirmed = anchor_is_unconfirmed(
                    q.venue,
                    account_ccy.is_some(),
                    account_ccy_assumed,
                    venue_stated,
                );
                if !venue_stated && let Some(ccy) = &account_ccy {
                    let consequence = if unconfirmed {
                        "A rate that does not state its ledger cannot confirm it, so NO symbol quoted in \
                         another currency is admitted on one until the account is started again and the \
                         venue states its own (`account.active` off and on); a rate that does state its \
                         ledger is held to it"
                    } else {
                        "Every rate the gatewayhub states is checked against it"
                    };
                    if account_ccy_assumed {
                        // The USD default: the engine's assembly says the assumption at `warn` (the one
                        // line, as for fxcm's); here is only what it means for the door.
                        tracing::info!(
                            venue = q.venue, account = %q.account,
                            "{}: the venue stated no currency for the account (a journaled engine, if any, \
                             only records an earlier assumption of it); the ledger is anchored to the assumed \
                             {ccy}, and a symbol quoted in another currency is admitted only at a rate the \
                             gatewayhub states is expressed in {ccy} too, so a venue that states another \
                             currency later is refused until the account is started again (inside the \
                             conversion's own gates: a demo account with an exposure ceiling)",
                            q.venue
                        );
                    } else {
                        tracing::warn!(
                            venue = q.venue, account = %q.account,
                            "{}: the gatewayhub could not state the account's currency now; the ledger is \
                             anchored to {ccy}, the currency the journaled engine's rates are expressed in. {consequence}",
                            q.venue
                        );
                    }
                }
                // A node converts a quote currency for the one venue and tier the rule names (decision
                // 0183, phase 1; the ceiling half is judged where the facts land, `LeaseFacts`): every
                // facts request of this lease then says so, and the daemon answers a symbol quoted in
                // another currency with its rate instead of refusing it for that alone. For ibkr the
                // account's ceiling is the opt-in here too (`quote_lease_converts`); an UNCONFIRMED anchor
                // asks for no rate, which the door would refuse (a budgeted venue pays for each ask).
                session.convert_quotes(quote_lease_converts(
                    q.venue,
                    q.tier,
                    account_ccy.is_some() && !unconfirmed,
                    ceiling,
                ));
                self.facts.register(q.route_key, session.clone());
                // ...and the currency the venue says the account is in, which the engine's ledger is anchored
                // to for an account that converts: a symbol quoted in another currency is refused without one.
                if unconfirmed {
                    self.facts.anchor_unconfirmed(q.route_key, account_ccy.as_deref());
                } else {
                    self.facts.anchor_currency(q.route_key, account_ccy.as_deref());
                }
                // THE BALANCE GATE IS THE NODE'S (`balance_gate`): what the engine was BOUND with decides whether
                // the venue's cash, which carries no currency, is adopted into its ledger. An assumed USD, a
                // stated currency the fence declined to anchor and a stated non-USD currency on an account that
                // does not convert hold it back; the gatewayhub's own gate follows the last start IT saw, which
                // a re-teach after its restart rewrites under an engine that is not rebuilt.
                let binding = if account_ccy_assumed {
                    balance_gate::Binding::Assumed
                } else if let (Some(why), Some(ccy)) = (fence.as_deref(), fenced_ccy.as_deref()) {
                    balance_gate::Binding::Declined { ccy, stated: venue_stated, why }
                } else if !converts
                    && let Some(ccy) =
                        venue_ccy.as_deref().filter(|ccy| !ccy.eq_ignore_ascii_case("USD"))
                {
                    balance_gate::Binding::Unanchored { ccy }
                } else {
                    balance_gate::Binding::Anchored
                };
                let hold = balance_gate::hold(q.venue, &binding);
                if let Some(why) = fence {
                    self.facts.note_unanchorable_mount(q.route_key, why);
                }
                // The foreign-quoted symbols the journal restores for an ibkr account: the rate timer keeps
                // their rates fresh whether or not the boot's re-admission reaches them, and says so when it
                // cannot (oanda's restored positions are left to the boot's pass as they always were). Its
                // balance (the mounted contract's cash) is not asked until the daemon's reconcile client was
                // told each of them: the new client's list is empty until the boot's re-admission lands.
                let mut untold: Option<balance_gate::Untold> = None;
                if q.venue == "ibkr" {
                    let symbols = journaled.map(|fx| fx.symbols.as_slice()).unwrap_or_default();
                    self.facts.note_journaled_foreign(q.route_key, symbols);
                    let names: Vec<String> = symbols.iter().map(|(name, _)| name.clone()).collect();
                    if !names.is_empty() {
                        let told = session.clone();
                        untold = Some(Box::new(move || {
                            names.iter().filter(|name| !told.has_told(name)).cloned().collect()
                        }));
                    }
                }
                // The gate says what it holds to the lease registry the publisher stamps every frame from, so the
                // operator sees `balance held: <reason>` beside the account instead of a seed-based equity.
                let recon_client = recon_client.map(|inner| {
                    let facts = Arc::clone(&self.facts);
                    let route_key = q.route_key.to_string();
                    let shown: balance_gate::Shown = Box::new(move |reason: Option<&str>| {
                        facts.set_balance_held(&route_key, reason)
                    });
                    let spec = balance_gate::GateSpec {
                        venue: q.venue.to_string(),
                        account: q.account.to_string(),
                        hold,
                        untold,
                        shown,
                    };
                    balance_gate::gate(inner, spec)
                });
                self.sessions
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert((q.venue.to_string(), q.account.text().map(str::to_string)), session);
                report.up();
                Ok(AdapterBind {
                    client,
                    recon: recon_client,
                    bound_tier,
                    grid,
                    contract_value,
                    margin_mode,
                    leg_grids,
                    // The gatewayhub records the venue-confirmed identity inside its own start.
                    confirmed_book: None,
                    // The venue's answer to the account's stated leverage (decision 0181).
                    leverage_cap,
                    // The currency the venue says the account's money is in (decision 0183, item 1), and
                    // whether it is only ASSUMED (FXCM's ruling): the shared tail anchors the ledger to it
                    // only for an account that converts, and says so when it is a guess.
                    account_ccy,
                    account_ccy_assumed,
                    // Where the session now stands, when this start moved it off the symbol asked for
                    // (`moved_mount`): the engine is mounted there.
                    mount_symbol: moved.then_some(mount),
                })
            }
        }
    }

    fn record_identity(
        &self,
        q: &IdentityRecord<'_>,
        _recon: Option<&dyn ReconClient>,
    ) -> Result<(), BackendError> {
        let key = (q.venue.to_string(), q.account.text().map(str::to_string));
        let session = self.sessions.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        let Some(session) = session else {
            return Err(BackendError::Unavailable(format!(
                "{}: no open trade lease to record the account identity over",
                q.venue
            )));
        };
        // Over the lease the account already holds: the gatewayhub reads the venue with the
        // credentials it holds, after the fee schedule was resolved (a failure is only a warning).
        session.record_identity().map_err(|e| Self::request_failed(q.venue, q.account, e))
    }

    /// **A start that came up was then refused by the shared tail** (the fold has already `begin_detach`ed and
    /// `detach`ed the bound client, which ended the lease): forget everything this backend kept for it. The
    /// open-lease table entry ([`RemoteBackend::sessions`], emptied only by `record_identity`, which a refused
    /// start never reaches), the control door's lease facts ([`facts::LeaseFacts::forget_lease`]) and, through
    /// the closed lease, the `gateway-reteach` worker (it ends on a closed lease and never starts an adapter on
    /// one, so a gatewayhub restart cannot send the stored `AdapterStart` of a refused start again). Detached
    /// here as well: `detach` is idempotent, and a client that was dropped without it leaves this the only
    /// thing that can end the lease.
    fn release_refused_start(&self, venue: &str, account: &AccountLabel, route_key: &str) {
        let key = (venue.to_string(), account.text().map(str::to_string));
        let session = self.sessions.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        if let Some(session) = session {
            session.begin_detach();
            session.detach();
        }
        self.facts.forget_lease(route_key);
    }
}

/// **The pair a start is made AGAIN on, or `None`** (the one place a mount follows the account's money): the venue
/// stated the account's currency in its reply (`reply`, a `Live` answer at the account's own tier, with a
/// currency that is its WORD and not the lane's assumption), the account converts a quote currency (`converts`: a
/// `demo` account with an exposure ceiling armed, so the ledger is anchored at all), and
/// `symbol_admission.rs`'s `currency_mount` has a pair for it in place of `mount`, the symbol the session stands on.
///
/// Only the venue's WORD moves a mount. An unstated currency is the node's assumed USD, which the wired mount carries,
/// and a journaled one is the previous run's belief (the account may have been re-pointed since): neither starts a
/// second session. Today that is fxcm's GBP account and no other (`currency_mount`'s table).
fn moved_mount(
    q: &AdapterStartQuery<'_>,
    converts: bool,
    mount: &str,
    reply: &proto::AdapterReply,
) -> Option<&'static str> {
    let proto::AdapterReply::Live {
        bound_tier,
        account_ccy: Some(ccy),
        account_ccy_assumed: false,
        ..
    } = reply
    else {
        return None;
    };
    if !converts || *bound_tier != q.tier {
        return None;
    }
    crate::server::symbol_admission::currency_mount(q.venue, mount, ccy.trim())
}

#[path = "remote_backend_tests.rs"]
#[cfg(test)]
mod remote_backend_tests;
