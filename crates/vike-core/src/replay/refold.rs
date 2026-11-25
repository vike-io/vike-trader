//! The shared re-fold engine (`replay_from` / `refold_records`) and the state a restore hands back.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use vike_execution::{Engine, EngineSnapshot, Ingest};
use vike_journal::{
    Journal, JournalFileConfig, JournalSnapshotContingency, JournalSnapshotOrderEmulationArm,
    OrderHeldRelease,
};
use vike_model::accounts::account_keys::label_of_route_key;

use super::client::JournalReplayInertStrategy;
use super::{
    ExecutionClientJournalReplay, JournalRecord, JournalReplayError, JournalReplayQueueClock,
    fold_conditionals, fold_order_client_id_mounts,
};
use crate::{CoreConfig, JournalConfig, Mount, spawn_core_multi};

/// Which `Snapshot` in the journal supplies the restore base (Task 6 factoring).
pub(crate) enum JournalReplayBaseSelection {
    /// The FIRST `Snapshot` — re-folds the WHOLE post-checkpoint tail. `replay_offline`'s determinism
    /// fence needs a non-trivial tail to reproduce, so it restores from the earliest checkpoint.
    First,
    /// The LATEST `Snapshot` — re-folds only the crash tail after it (empty on a clean shutdown, where
    /// the exit Snapshot already reflects everything). The fast restart path (`restore_from_journal`).
    Latest,
}

/// What a re-fold accepts beyond the verifying replay's v1 scope — the two journal shapes every
/// production journal has (decision 0127): several engines (the live node mounts one per wired
/// market) and `Ingest::Watchdog` waker records (the live node arms the stuck-order watchdog).
///
/// The verifying entry points (`replay_offline`, `restore_from_journal`) keep [`Self::VERIFY`]: a
/// determinism fence must refuse what it cannot reproduce. The boot restore
/// (`crate::replay::restore_for_boot`) takes [`Self::restore`]: it fences nothing, and falls back to
/// the pure folds of the latest `Snapshot` when even this cannot run.
pub(crate) struct JournalReplayRefoldOptions<'a> {
    /// Re-fold a base of several engines, including one with a labelled account or two engines of one
    /// venue: each order write of the tail is then lowered onto the engine its record names
    /// ([`keyed_base`], [`replayed_order`]), or the refold is refused when a record cannot say.
    pub(crate) multi_engine: bool,
    /// Drop the tail's `Ingest::Watchdog` records (counted on [`JournalReplayEndState::skipped_wakers`]) instead
    /// of refusing the tail. What is then NOT reproduced: the stuck-order sweep's synthetic reject
    /// and the dead-man's cancels, both wall-clock decisions the journal does not record.
    pub(crate) skip_wakers: bool,
    /// The parent directory of the replay core's scratch journal. `None` is the system temp
    /// directory, the offline tools' known debt (`unique_temp_dir`).
    pub(crate) scratch_root: Option<&'a Path>,
    /// This install's order client id sessions the journal itself may not name (`vike.db`'s
    /// `order_client_id_session` rows, `(session, last_sequence)`): every replay core holds them as past sessions
    /// beside the ones the journal's `Snapshot`s name, so a late fill on an id a session OLDER than the readable
    /// journal minted regenerates as OURS, as the live core that held them read it.
    pub(crate) past_sessions: &'a [(String, u64)],
}

impl JournalReplayRefoldOptions<'static> {
    /// The verifying replay's scope: one engine, no waker record, scratch in the system temp.
    pub(crate) const VERIFY: Self = JournalReplayRefoldOptions {
        multi_engine: false,
        skip_wakers: false,
        scratch_root: None,
        past_sessions: &[],
    };
}

impl<'a> JournalReplayRefoldOptions<'a> {
    /// The boot restore's scope: several engines, wakers skipped, scratch under `scratch_root`
    /// (`<settings>/state/restore-scratch` in production), and this install's `past_sessions`.
    pub(crate) fn restore(scratch_root: &'a Path, past_sessions: &'a [(String, u64)]) -> Self {
        JournalReplayRefoldOptions {
            multi_engine: true,
            skip_wakers: true,
            scratch_root: Some(scratch_root),
            past_sessions,
        }
    }
}

/// The reproduced state a `replay_from` run ends on — the shared payoff of `replay_offline` (which
/// then fences `final_hash` against the source) and `restore_from_journal` (which just returns the
/// engines/order_client_id). NO fence is applied here; that stays with the callers.
pub(crate) struct JournalReplayEndState {
    pub(crate) engines: Vec<EngineSnapshot>,
    pub(crate) order_client_id_session: String,
    pub(crate) order_client_id_sequence: u64,
    /// the replay core's final emulated-conditional arm-id counter (its exit Snapshot's stamped
    /// `arm_sequence`, floored at `base + tail-armed-count` — see the module doc's undercount note) —
    /// the arm-side twin of `order_client_id_sequence`, seeded from the BASE Snapshot (or the pre-v7 fallback bound)
    /// and advanced by any tail arms, exactly as `order_client_id_sequence` is
    pub(crate) arm_sequence: u64,
    /// the resting conditional books at the journal's end — the base `Snapshot`'s books folded
    /// forward through the tail's conditional records ([`fold_conditionals`]), in fire order
    pub(crate) order_emulation_arms: Vec<JournalSnapshotOrderEmulationArm>,
    /// the resting OTO/OCO contingency book at the journal's end — read straight from the replay
    /// core's own exit `Snapshot` (the mark-less replay core reproduces it faithfully, so no record
    /// fold is needed — see the `contingencies` seed comment in `replay_from`)
    pub(crate) contingencies: Vec<JournalSnapshotContingency>,
    /// the orders held for their release at the journal's end — the replay core's own exit `Snapshot`,
    /// as for the contingency book: its table moves only through journaled records
    pub(crate) held_releases: Vec<OrderHeldRelease>,
    /// the EMULATED parents at the journal's end (decision 0275) — the replay core's own exit `Snapshot`,
    /// as for the held orders: its book moves only through journaled records
    pub(crate) emulated: Vec<vike_journal::JournalSnapshotOrderEmulation>,
    /// the per-mount attribution ledgers captured on the BASE `Snapshot` (gap D)
    pub(crate) mount_attribution: Vec<vike_journal::JournalSnapshotMountAttribution>,
    /// `(order client id, mount_id)` for every order a mount minted, folded from the WHOLE readable record
    /// set ([`fold_order_client_id_mounts`]) — not just the tail, so an order resting since before the base
    /// `Snapshot` still restores its origin
    pub(crate) order_client_id_mounts: Vec<(String, String)>,
    /// source journal record count (for `JournalReplayOutcome::records`)
    pub(crate) records: u64,
    /// hash of the replayed exit Snapshot (the reproduced final state)
    pub(crate) final_hash: u64,
    /// `Ingest::Watchdog` records the tail dropped under [`JournalReplayRefoldOptions::skip_wakers`] (0 otherwise:
    /// the verifying scope refuses such a tail instead)
    pub(crate) skipped_wakers: u64,
    /// What the replay cores' OMS stated about the tail they refolded, every core of a session split in order
    /// ([`replayed_reports`]): the boot restore's regenerated execution reports (package 2's C.1b).
    pub(crate) execution_reports: JournalReplayReports,
}

/// The execution reports the replay cores stated, each under its cause's sequence in the SOURCE journal.
#[derive(Debug, Clone, Default)]
pub(crate) struct JournalReplayReports {
    /// `(now_ms, report)` in replay order, `caused_by_sequence` the source journal's.
    pub(crate) placed: Vec<(i64, vike_model::orders::ExecutionReport)>,
    /// The order client ids of reports whose cause no source record could be matched to: the refold did not
    /// reproduce the journal's records one for one, so these are not regenerated.
    pub(crate) unplaced: Vec<String>,
    /// `(caused_by_sequence, order client id)` of every report a replay core withheld because the order's pre-trade
    /// decision read market data the journal does not hold (its `ExecutionReportsUnverified` marker, placed like a
    /// report, `crate::replay::MARKET_DATA_UNVERIFIED`).
    pub(crate) market_dependent: Vec<(u64, String)>,
}

/// What [`replayed_reports`] places a replay core's report by: the source sequence of every tail mint by its order
/// client id (a mint is re-derived, never re-applied, and mints the same id), and every order client id session
/// the journal's `Snapshot`s name with the highest sequence they state, beside the install's own past sessions
/// ([`JournalReplayRefoldOptions::past_sessions`]) (handed to each replay core as its
/// `CoreConfig::order_client_id_past_sessions`, so a late fill on an id of a session it switched from is OURS
/// there as it was live). It also carries what every replay core is handed of the whole journal beside them: the order
/// client id ranges reserved for counted denials (`reserved`, its `CoreConfig::order_client_id_reserved`), which the live
/// core never minted in, so a replay core mints past them the ids it did.
struct JournalReplayReportPlaces {
    minted: std::collections::HashMap<String, u64>,
    sessions: Vec<(String, u64)>,
    reserved: Vec<(String, u64, u64)>,
}

impl JournalReplayReportPlaces {
    fn of(records: &[JournalRecord], base_index: usize, past_sessions: &[(String, u64)]) -> Self {
        let reserved = super::fold::fold_reserved_order_client_ids(records);
        let mut minted = std::collections::HashMap::new();
        let mut sessions: Vec<(String, u64)> = past_sessions.to_vec();
        for (i, rec) in records.iter().enumerate() {
            match rec {
                JournalRecord::MintedSubmit { sequence, request, .. }
                    if i > base_index && !request.order_client_id.is_empty() =>
                {
                    minted.insert(request.order_client_id.clone(), *sequence);
                }
                // A `Snapshot` names its own session and the past ones its core held (decision 0304: the boot reads from
                // the newest `Snapshot` on, so a session only an older one named comes from that list).
                JournalRecord::Snapshot {
                    order_client_id_session,
                    order_client_id_sequence,
                    order_client_id_past_sessions,
                    ..
                } => {
                    let named =
                        std::iter::once((order_client_id_session, *order_client_id_sequence))
                            .chain(order_client_id_past_sessions.iter().map(|(s, n)| (s, *n)));
                    for (session, sequence) in named {
                        match sessions.iter_mut().find(|(s, _)| s == session) {
                            Some((_, last)) => *last = (*last).max(sequence),
                            None => sessions.push((session.clone(), sequence)),
                        }
                    }
                }
                // every other record: not this lookup's business
                _ => {}
            }
        }
        JournalReplayReportPlaces { minted, sessions, reserved }
    }
}

/// The parts of the `base`-selected `Snapshot` that [`replay_from`] restores from.
struct BaseSnap {
    engines: Vec<EngineSnapshot>,
    order_client_id_session: String,
    order_client_id_sequence: u64,
    arm_sequence: Option<u64>,
    order_emulation_arms: Vec<JournalSnapshotOrderEmulationArm>,
    contingencies: Vec<JournalSnapshotContingency>,
    held_releases: Vec<OrderHeldRelease>,
    emulated: Vec<vike_journal::JournalSnapshotOrderEmulation>,
    mount_attribution: Vec<vike_journal::JournalSnapshotMountAttribution>,
    recent_order_client_ids: Vec<(String, i64)>,
    /// Who owns each order it holds (decision 0304's `Snapshot.order_client_id_mounts`).
    order_client_id_mounts: Vec<(String, String)>,
}

/// The parts of the replay core's exit `Snapshot` that [`replay_from`] hands back ([`read_exit_snap`]).
struct ExitSnap {
    final_hash: u64,
    engines: Vec<EngineSnapshot>,
    order_client_id_session: String,
    order_client_id_sequence: u64,
    arm_sequence: u64,
    order_emulation_arms: Vec<JournalSnapshotOrderEmulationArm>,
    contingencies: Vec<JournalSnapshotContingency>,
    held_releases: Vec<OrderHeldRelease>,
    emulated: Vec<vike_journal::JournalSnapshotOrderEmulation>,
    /// The reports this replay core's OMS stated, placed in the source journal ([`replayed_reports`]).
    execution_reports: JournalReplayReports,
}

/// What one replay core starts from ([`run_replay_core`]): the base `Snapshot`'s, or, past a crossed `Snapshot`
/// that names another order client id session or trade-id window, the previous replay core's exit state under that
/// `Snapshot`'s session and window.
struct JournalReplayStart {
    engines: Vec<EngineSnapshot>,
    order_client_id_session: (String, u64),
    arm_sequence: u64,
    order_emulation_arms: Vec<JournalSnapshotOrderEmulationArm>,
    contingencies: Vec<JournalSnapshotContingency>,
    held_releases: Vec<OrderHeldRelease>,
    emulated: Vec<vike_journal::JournalSnapshotOrderEmulation>,
    /// The late-retry memory the live core that wrote the tail held (its `Snapshot`'s `recent_order_client_ids`), so a
    /// re-applied explicit id that core refused is refused again.
    recent_order_client_ids: Vec<(String, i64)>,
}

/// One entry of the tail [`replay_tail`] extracts: a record to re-apply (its sequence in the source journal, its
/// `now_ms`, its message), or a crossed `Snapshot` whose order client id session differs from the one minting so far
/// (decision 0270's D1: a later process switched sessions, so its ids are minted under this `Snapshot`'s
/// `(order_client_id_session, order_client_id_sequence)`) or whose trade-id window does (a later process was told
/// another reconcile read-back: `vike_execution::Engine::size_fill_window`, so its `Snapshot`s cut by that window).
enum TailItem {
    Msg(u64, i64, Ingest),
    Session {
        order_client_id_session: (String, u64),
        recent_order_client_ids: Vec<(String, i64)>,
        fill_window_days: Option<u32>,
    },
}

/// The trade-id window a core's `Snapshot` states: every engine of one core is sized alike (`CoreConfig::
/// reconcile_lookback_ms`), so the primary's.
fn core_fill_window(engines: &[EngineSnapshot]) -> Option<u32> {
    engines.first().and_then(|e| e.fill_window_days)
}

/// A live mount a v2 replay core holds as an [`JournalReplayInertStrategy`], so the records that mount wrote are
/// lowered on it and mint its labels ([`replay_mounts`]).
struct JournalReplayMount {
    /// The live mount's id as a mount id reads it back (`crate::strategy_state::mount_id_with`), so the
    /// replay mount's own id is this text.
    mount_id: String,
    venue: String,
    /// The account its records name (`None`: the venue's default account).
    account: Option<vike_model::accounts::account_keys::AccountLabel>,
    symbol: String,
    /// Its strategy code as the journal says it minted.
    order_client_id_code: Option<vike_model::orders::client_id::OrderClientIdStrategyCode>,
}

impl JournalReplayMount {
    fn mount(&self) -> Mount {
        Mount {
            order_client_id_code: self.order_client_id_code,
            account: self.account.clone(),
            symbols: Vec::new(),
            controller_id: Some(self.mount_id.clone()),
            venue: self.venue.clone(),
            symbol: self.symbol.clone(),
            interval: String::new(),
            strategy: Box::new(JournalReplayInertStrategy),
            underlying_symbol: None,
        }
    }
}

/// Whether `session` mints v2 ids (a `W`-marked session string).
fn session_is_v2(session: &str) -> bool {
    vike_model::orders::client_id::OrderClientIdGenerator::resume(session.to_string(), 0).is_v2()
}

/// The owner of each order the base `Snapshot`'s `engines` hold, as `(order client id, mount id)`, for a mount that `Snapshot`
/// names (`mount_attribution`): the journal's own provenance (`order_client_id_mounts`), else, for an order whose provenance a
/// pruned prefix took, a v2 strategy id whose code exactly ONE of those mounts carries. Pure. The boot restore asks it
/// with no mints too (`crate::replay::restore_for_boot`): it reads from the `Snapshot` on, so every base order's mint
/// is below it.
pub(super) fn base_owners(
    mount_attribute: &[vike_journal::JournalSnapshotMountAttribution],
    engines: &[EngineSnapshot],
    order_client_id_mounts: &[(String, String)],
) -> Vec<(String, String)> {
    let named = |m: &str| mount_attribute.iter().any(|r| r.mount_id == m);
    let by_code = |order_client_id: &str| mount_by_code(mount_attribute, order_client_id);
    // Each registry order's FIRST mapping, as a scan of the mints from the front finds it: ONE pass over the
    // journal's mints, not one per order.
    let mut minted_by: std::collections::HashMap<&str, Option<&str>> = engines
        .iter()
        .flat_map(|e| {
            e.registry.iter().map(|(order_client_id, _)| (order_client_id.as_str(), None))
        })
        .collect();
    for (c, m) in order_client_id_mounts {
        if let Some(first) = minted_by.get_mut(c.as_str()) {
            first.get_or_insert(m.as_str());
        }
    }
    engines
        .iter()
        .flat_map(|e| e.registry.iter().map(|(order_client_id, _)| order_client_id))
        .filter_map(|order_client_id| {
            let owner =
                minted_by.get(order_client_id.as_str()).copied().flatten().map(str::to_string);
            owner
                .or_else(|| by_code(order_client_id))
                .filter(|m| named(m))
                .map(|m| (order_client_id.clone(), m))
        })
        .collect()
}

/// The mount a v2 STRATEGY id's code names among a `Snapshot`'s mount rows (`mount_attribute`), when exactly one
/// mount's own row carries that code; `None` for any other id. Pure.
pub(super) fn mount_by_code(
    mount_attribute: &[vike_journal::JournalSnapshotMountAttribution],
    order_client_id: &str,
) -> Option<String> {
    use vike_model::orders::client_id::{
        OrderClientIdReading, OrderClientIdSource, parse_order_client_id,
    };
    let Some(OrderClientIdReading::V2(p)) = parse_order_client_id(order_client_id) else {
        return None;
    };
    let mut rows = mount_attribute.iter().filter(|r| {
        r.venue.is_none()
            && p.src == OrderClientIdSource::Strategy
            && r.order_client_id_code == Some(p.strat)
    });
    rows.next().filter(|_| rows.next().is_none()).map(|r| r.mount_id.clone())
}

/// The route row of each order a tail EVENT names that no base registry holds and no record minted
/// (decision 0304): an order ended and left out of the base `Snapshot` (an ended order rides one for
/// `vike_execution::ENDED_ORDER_KEEP_MS` only), whose mint the boot no longer reads, sits on the base engine
/// its v2 id's ACCOUNT code names (`EngineSnapshot::account_code`) when exactly one base engine carries that
/// code, as the live core's submit-time map placed it. Pure.
fn account_code_routes(
    base: &[EngineSnapshot],
    tail: &[JournalRecord],
    minted: &[(String, String)],
) -> Vec<(String, String)> {
    use vike_model::orders::client_id::{OrderClientIdReading, parse_order_client_id};
    let mut known: std::collections::HashSet<&str> = base
        .iter()
        .flat_map(|e| e.registry.iter().map(|(id, _)| id.as_str()))
        .chain(minted.iter().map(|(id, _)| id.as_str()))
        .collect();
    let mut rows: Vec<(String, String)> = Vec::new();
    for rec in tail {
        let JournalRecord::Command { message, .. } = rec else { continue };
        let Some(id) = message.venue_event().and_then(vike_model::events::Event::order_client_id)
        else {
            continue;
        };
        if id.is_empty() || !known.insert(id) {
            continue;
        }
        let Some(OrderClientIdReading::V2(parts)) = parse_order_client_id(id) else { continue };
        let Some(code) = parts.account else { continue };
        let mut carriers = base.iter().filter(|e| e.account_code == Some(code));
        if let (Some(e), None) = (carriers.next(), carriers.next()) {
            rows.push((id.to_string(), e.route_key.clone().unwrap_or_else(|| e.venue.clone())));
        }
    }
    rows
}

/// The mounts whose records the tail after `base_index` holds (a `StrategySubmit` naming a venue, a budget
/// latch's `MarginCallLiquidate`), each on the engine its record names (its route key, else the venue's
/// default account), with the strategy code the journal says it minted: the code its `Snapshot` rows carry,
/// and over it the one its own minted v2 ids carry (`order_client_id_mounts`, the order client id -> mount fold). Then every
/// other mount that owns an order the base `Snapshot` holds (`owners`, [`base_owners`]), on that order's engine:
/// its tail may hold only a Modify or a Cancel, which name an order client id and no venue, and an amend's replacement
/// past the `R` cap still mints that mount's labels. Pure.
fn replay_mounts(
    records: &[JournalRecord],
    base_index: usize,
    engines: &[EngineSnapshot],
    order_client_id_mounts: &[(String, String)],
    fire_owners: &std::collections::HashMap<usize, String>,
    owners: &[(String, String)],
) -> Vec<JournalReplayMount> {
    use vike_model::orders::client_id::{
        OrderClientIdReading, OrderClientIdSource, parse_order_client_id,
    };
    let mut codes = std::collections::HashMap::new();
    for rec in records {
        if let JournalRecord::Snapshot { mount_attribution: mount_attribute, .. } = rec {
            for row in mount_attribute.iter().filter(|r| r.venue.is_none()) {
                if let Some(code) = row.order_client_id_code {
                    codes.insert(row.mount_id.clone(), code);
                }
            }
        }
    }
    for (order_client_id, mount_id) in order_client_id_mounts {
        if let Some(OrderClientIdReading::V2(p)) = parse_order_client_id(order_client_id)
            && p.src == OrderClientIdSource::Strategy
        {
            codes.insert(mount_id.clone(), p.strat);
        }
    }
    let mut mounts: Vec<JournalReplayMount> = Vec::new();
    for (i, rec) in records.iter().enumerate().skip(base_index + 1) {
        let (mount_id, venue, route_key) = match rec {
            JournalRecord::StrategySubmit { mount_id, intent, route_key, .. } => {
                let Some(venue) = intent_venues(intent).first().map(|v| v.to_string()) else {
                    continue;
                };
                (mount_id, venue, route_key)
            }
            JournalRecord::MarginCallLiquidate {
                mount_id: Some(mount_id),
                request: req,
                route_key,
                ..
            } => (mount_id, req.venue.clone(), route_key),
            JournalRecord::OrderEmulationFire { request: req, route_key, .. } => {
                let Some(mount_id) = fire_owners.get(&i) else { continue };
                (mount_id, req.venue.clone(), route_key)
            }
            // A mount's working emulated order places its children as that mount's (decision 0275).
            JournalRecord::Command {
                message: Ingest::Command(vike_execution::Command::Emulated(step)),
                ..
            } => {
                static NO_ROUTE_KEY: Option<String> = None;
                let (Some(mount_id), vike_execution::EmulatedVerb::Child(intent)) =
                    (&step.mount_id, &step.verb)
                else {
                    continue;
                };
                let Some(venue) = intent_venues(intent).first().map(|v| v.to_string()) else {
                    continue;
                };
                (mount_id, venue, &NO_ROUTE_KEY)
            }
            // every other record: not this lookup's business
            _ => continue,
        };
        let id = crate::strategy_state::mount_id_with(Some(mount_id), "", "", "");
        if mounts.iter().any(|m| m.mount_id == id) {
            continue;
        }
        let key = route_key.clone().unwrap_or_else(|| venue.clone());
        let symbol = engines
            .iter()
            .find(|e| e.route_key.as_deref().unwrap_or(e.venue.as_str()) == key)
            .map(|e| e.symbol.clone())
            .unwrap_or_default();
        mounts.push(JournalReplayMount {
            account: label_of_route_key(&venue, &key).filter(|l| !l.is_default()),
            order_client_id_code: codes.get(mount_id).copied(),
            mount_id: id,
            venue,
            symbol,
        });
    }
    // The FIRST engine holding each order, as a scan in engine order finds it.
    let mut engine_of: std::collections::HashMap<&str, &EngineSnapshot> =
        std::collections::HashMap::new();
    for e in engines {
        for (c, _) in &e.registry {
            engine_of.entry(c.as_str()).or_insert(e);
        }
    }
    for (order_client_id, mount_id) in owners {
        let id = crate::strategy_state::mount_id_with(Some(mount_id), "", "", "");
        let Some(e) = engine_of.get(order_client_id.as_str()) else {
            continue;
        };
        if mounts.iter().any(|m| m.mount_id == id) {
            continue;
        }
        let key = e.route_key.as_deref().unwrap_or(e.venue.as_str());
        mounts.push(JournalReplayMount {
            account: label_of_route_key(&e.venue, key).filter(|l| !l.is_default()),
            order_client_id_code: codes.get(mount_id).copied(),
            mount_id: id,
            venue: e.venue.clone(),
            symbol: e.symbol.clone(),
        });
    }
    mounts
}

/// The shared machinery of `replay_offline` + `restore_from_journal` (Task 6): restore the engine
/// from the `base`-selected `Snapshot`, spawn a real core (pure no-op [`ExecutionClientJournalReplay`],
/// [`JournalReplayQueueClock`] replaying the tail's `now_ms`, temp journal with `snapshot_every = u64::MAX`), pump the post-base
/// `Command` tail IN ORDER, and read back the replayed exit Snapshot. Returns [`JournalReplayError::Empty`] when
/// the journal is empty or carries no `Snapshot`, [`JournalReplayError::Unsupported`] for a multi-engine base
/// or a watchdog-tick tail (v1 scope). NO fence — the caller adds it (`replay_offline`) or omits it
/// (`restore_from_journal`).
pub(crate) fn replay_from(
    dir: &Path,
    base: JournalReplayBaseSelection,
) -> Result<JournalReplayEndState, JournalReplayError> {
    refold_records(Journal::read_all(dir)?, base, &JournalReplayRefoldOptions::VERIFY)
}

/// [`replay_from`] over records already read, under `opts` — the boot restore reads the journal
/// once (with its halt located) and hands the records here.
pub(crate) fn refold_records(
    records: Vec<JournalRecord>,
    base: JournalReplayBaseSelection,
    opts: &JournalReplayRefoldOptions<'_>,
) -> Result<JournalReplayEndState, JournalReplayError> {
    if records.is_empty() {
        return Err(JournalReplayError::Empty);
    }
    let records_len = records.len() as u64;

    // Select the restore-base Snapshot per `base`: First ⇒ earliest checkpoint, Latest ⇒ newest.
    let base_index = match base {
        JournalReplayBaseSelection::First => {
            records.iter().position(|r| matches!(r, JournalRecord::Snapshot { .. }))
        }
        JournalReplayBaseSelection::Latest => {
            records.iter().rposition(|r| matches!(r, JournalRecord::Snapshot { .. }))
        }
    };
    let Some(base_index) = base_index else {
        return Err(JournalReplayError::Empty); // no Snapshot ⇒ no engine config to restore
    };
    let BaseSnap {
        engines: base_engines,
        order_client_id_session: base_session,
        order_client_id_sequence: base_seq,
        arm_sequence: base_arm_seq,
        order_emulation_arms: base_conditionals,
        contingencies: base_contingencies,
        held_releases: base_held_releases,
        emulated: base_emulated,
        mount_attribution: base_mount_attr,
        recent_order_client_ids: base_recent_order_client_ids,
        order_client_id_mounts: base_order_mounts,
    } = match &records[base_index] {
        JournalRecord::Snapshot {
            engines,
            order_client_id_session,
            order_client_id_sequence,
            arm_sequence: arm_sequence_value,
            order_emulation_arms: conditionals,
            contingencies,
            held_releases,
            mount_attribution: mount_attribute,
            recent_order_client_ids,
            emulated,
            order_client_id_mounts,
            ..
        } => BaseSnap {
            order_client_id_mounts: order_client_id_mounts.clone(),
            engines: engines.clone(),
            order_client_id_session: order_client_id_session.clone(),
            order_client_id_sequence: *order_client_id_sequence,
            arm_sequence: *arm_sequence_value,
            order_emulation_arms: conditionals.clone(),
            contingencies: contingencies.clone(),
            held_releases: held_releases.clone(),
            emulated: emulated.clone(),
            mount_attribution: mount_attribute.clone(),
            recent_order_client_ids: recent_order_client_ids.clone(),
        },
        // NOT a dispatch — `base_index` came from a `Snapshot`-matching `position`/`rposition` two
        // statements up, so this arm is unreachable by construction and left as a catch-all
        // deliberately: it can only PANIC, never silently drop a future variant. (Contrast the
        // exhaustive folds below, where a missing arm WOULD be a silent drop.)
        _ => unreachable!("indexed a Snapshot"),
    };
    // Multi-mount durability (gap D): rebuild the order client id -> mount ORIGIN map from the journal's own
    // `StrategySubmit` provenance. Folded over every record handed in, not just the post-base tail: an
    // order minted long before the base `Snapshot` can still be RESTING, and it is exactly that order
    // whose post-restart fill used to land unattributed. The boot restore hands in the base `Snapshot`
    // and its tail only (decision 0304): such an order's owner is then the base `Snapshot`'s own row (first,
    // so it is each base order's first mapping), else its v2 id's code (`base_owners`).
    let order_client_id_mounts: Vec<(String, String)> = {
        let folded = fold_order_client_id_mounts(&records);
        let minted: std::collections::HashSet<&str> =
            folded.iter().map(|(c, _)| c.as_str()).collect();
        // a row the records' own mints repeat once (a whole read sees both)
        let carried: Vec<(String, String)> =
            base_order_mounts.into_iter().filter(|(c, _)| !minted.contains(c.as_str())).collect();
        drop(minted);
        carried.into_iter().chain(folded).collect()
    };
    // Emulator PR-3: reconstruct the resting books at the journal's END — the base Snapshot's books
    // folded forward through the tail's conditional records. A PURE fold of records, deliberately
    // independent of the replay core's own book state (see the module doc's design point 1: the
    // replay core neither fires nor arms trailing stops faithfully, the records do). Also counts
    // the tail's `OrderEmulationArmed` records for the arm_sequence undercount patch below.
    let (restored_conditionals, tail_armed_count) =
        fold_conditionals(base_conditionals.clone(), &records[base_index + 1..]);
    // The emulated-conditional arm-id counter to seed the replay core with — the arm-side twin of
    // `base_seq`, and resumed for the same reason (see `RestoredState::arm_sequence`). A v7+ Snapshot
    // STAMPS it (`Some`), so the replay core re-mints tail arms under the IDENTICAL ids the live
    // core minted — which is what lets a tail `DisarmConditional` (keyed by arm id) find its arm.
    // A pre-v7 Snapshot lacks the field (`None`): fall back to max-global-record-seq + 1, a
    // PRUNE-SAFE upper bound on the ids already spent (sequences are globally monotonic across
    // segments and survive pruning in the remaining records; every arm appended at least one
    // record, so ids-spent <= records-ever-written <= max-seq + 1). NEVER the readable record
    // COUNT — `prune_before_latest_snap` moves whole early segments away, so a count restarts a
    // restored session's counter BELOW ids already spent and re-mints duplicates. The overshoot
    // is harmless — but NOT because a pre-v7 journal cannot carry a `DisarmConditional`
    // (versions are per SEGMENT, so a mixed directory absolutely can: a v7+ binary resuming a v6
    // journal appends arms AND disarms into its tail before its first stamping Snapshot). It is
    // harmless because (a) a tail disarm the replay core misses — its tail arms re-mint under
    // the overshot ids, so a recorded id may not match — is a LOUD replay-neutral no-op inside
    // that core, whose book state is outside both the fence (books are not hashed) and the
    // restore surface, and (b) restore MEMBERSHIP comes from `fold_conditionals` above, which
    // resolves arms by their RECORDED ids and therefore consumes the disarm correctly anyway.
    // A too-HIGH counter only wastes ids; only a too-low one re-mints a spent (disarm-key) id.
    let arm_sequence_seed = base_arm_seq.unwrap_or_else(|| {
        records.iter().map(vike_journal::record_sequence).max().map_or(0, |m| m + 1)
    });

    // v1 (the verifying scope): single engine only. The boot restore's scope accepts several
    // engines, each restored from its own snapshot with its own `equity_seed` (the per-engine seed
    // `spawn_core_multi` would otherwise impose from configuration IS in the snapshot). A base whose
    // engines an order's venue cannot tell apart (`keyed_base`) is refolded too, with the route each
    // re-applied record carries (`replayed_order`): `keyed` is the one switch for that.
    if base_engines.is_empty() || (base_engines.len() != 1 && !opts.multi_engine) {
        return Err(JournalReplayError::Unsupported(
            "multi-engine replay (v1: single-engine only)".into(),
        ));
    }
    let keyed = opts.multi_engine && keyed_base(&base_engines);
    // The rows the replay core's submit-time map would hold for the orders the journal minted onto a
    // keyed engine (held exits included), and of an order a tail event names that ended below the base (by its
    // id's account code); each replay core adds its base registries' own.
    let minted_routes = if keyed {
        let mut rows = fold_minted_routes(&records);
        let by_code = account_code_routes(&base_engines, &records[base_index + 1..], &rows);
        rows.extend(by_code);
        rows
    } else {
        Vec::new()
    };
    // The mounts whose records a v2 session's replay core lowers as theirs (`replay_tail`), and the mount
    // that armed each conditional fire's arm.
    let fire_owners = super::fold::fold_order_client_id_origins_with_fires(&records).1;
    let owners = base_owners(&base_mount_attr, &base_engines, &order_client_id_mounts);
    let mounts = replay_mounts(
        &records,
        base_index,
        &base_engines,
        &order_client_id_mounts,
        &fire_owners,
        &owners,
    );
    // The ownership a v2 replay core is seeded with (`CoreConfig::order_client_id_owners`), under the ids it mounts.
    let order_client_id_owners: Vec<(String, String)> = order_client_id_mounts
        .iter()
        .chain(&owners)
        .map(|(c, m)| (c.clone(), crate::strategy_state::mount_id_with(Some(m), "", "", "")))
        .collect();
    // What the replay cores' execution reports are placed by in the source journal (`replayed_reports`): the
    // sequence of each tail mint by its order client id, and the sessions the journal names, so a replay core
    // reads an id one of them minted as OURS, as the live core that held them as its past sessions did.
    let report_places = JournalReplayReportPlaces::of(&records, base_index, opts.past_sessions);

    let mut tail = replay_tail(
        records,
        base_index,
        (&base_session, core_fill_window(&base_engines)),
        keyed.then_some(&base_engines[..]),
        (&mounts, &fire_owners),
    )?;
    let mut skipped_wakers = 0u64;
    let is_waker = |t: &TailItem| matches!(t, TailItem::Msg(_, _, Ingest::Watchdog));
    if tail.iter().any(is_waker) {
        if !opts.skip_wakers {
            return Err(JournalReplayError::Unsupported(
                "watchdog-enabled journal not replayable in v1".into(),
            ));
        }
        let before = tail.len();
        tail.retain(|t| !is_waker(t));
        skipped_wakers = (before - tail.len()) as u64;
    }

    // Pump the tail through a replay core. A crossed `Snapshot` that names ANOTHER order client id session (a later
    // process switched, decision 0270's D1) ends that core: the rest replays through a new one over the
    // engines and books the first reached, resuming that `Snapshot`'s `(order_client_id_session, order_client_id_sequence)` and memory,
    // as the process that wrote it minted. One that states another trade-id window (a later process was told another
    // read-back) ends it too, and the new core's engines cut by that window, as that process's `Snapshot`s did. A
    // crossed `Snapshot` of the same session and window changes nothing.
    let mut start = JournalReplayStart {
        engines: base_engines,
        order_client_id_session: (base_session, base_seq),
        arm_sequence: arm_sequence_seed,
        order_emulation_arms: base_conditionals,
        contingencies: base_contingencies,
        held_releases: base_held_releases,
        emulated: base_emulated,
        recent_order_client_ids: base_recent_order_client_ids,
    };
    let mut items = tail.into_iter();
    let mut execution_reports = JournalReplayReports::default();
    let exit = loop {
        let mut messages: Vec<(u64, i64, Ingest)> = Vec::new();
        let mut crossed = None;
        for item in items.by_ref() {
            match item {
                TailItem::Msg(sequence, now_ms, msg) => messages.push((sequence, now_ms, msg)),
                TailItem::Session {
                    order_client_id_session,
                    recent_order_client_ids,
                    fill_window_days,
                } => {
                    crossed =
                        Some((order_client_id_session, recent_order_client_ids, fill_window_days));
                    break;
                }
            }
        }
        let mut exit = run_replay_core(
            start,
            messages,
            keyed,
            &minted_routes,
            (&mounts, &order_client_id_owners),
            &report_places,
            opts,
        )?;
        execution_reports.placed.append(&mut exit.execution_reports.placed);
        execution_reports.unplaced.append(&mut exit.execution_reports.unplaced);
        execution_reports.market_dependent.append(&mut exit.execution_reports.market_dependent);
        let Some((order_client_id_session, recent_order_client_ids, fill_window_days)) = crossed
        else {
            break exit;
        };
        let mut engines = exit.engines;
        for e in &mut engines {
            e.fill_window_days = fill_window_days;
        }
        start = JournalReplayStart {
            engines,
            order_client_id_session,
            arm_sequence: exit.arm_sequence,
            order_emulation_arms: exit.order_emulation_arms,
            contingencies: exit.contingencies,
            held_releases: exit.held_releases,
            emulated: exit.emulated,
            recent_order_client_ids,
        };
    };
    let ExitSnap {
        final_hash,
        engines,
        order_client_id_session,
        order_client_id_sequence,
        arm_sequence: arm_sequence_value,
        contingencies: restored_contingencies,
        held_releases: restored_held_releases,
        emulated: restored_emulated,
        ..
    } = exit;

    // The arm-counter undercount patch (module doc, design point 1): a tail TRAILING arm is
    // refused by the mark-less replay core, skipping its mint — so the replayed exit `arm_sequence`
    // can land BELOW the ids the live session actually spent. Every live mint since the base
    // wrote exactly one `OrderEmulationArmed` record, so `seed + tail-armed-count` is the true
    // spent bound; take the max so a restored session can never re-mint a spent id (the id is
    // the disarm key). For a pre-v7 base the seed is itself an overshoot bound — still sound.
    let arm_sequence_value = arm_sequence_value.max(arm_sequence_seed + tail_armed_count);

    Ok(JournalReplayEndState {
        engines,
        order_client_id_session,
        order_client_id_sequence,
        arm_sequence: arm_sequence_value,
        order_emulation_arms: restored_conditionals,
        contingencies: restored_contingencies,
        held_releases: restored_held_releases,
        emulated: restored_emulated,
        mount_attribution: base_mount_attr,
        order_client_id_mounts,
        records: records_len,
        final_hash,
        skipped_wakers,
        execution_reports,
    })
}

/// One replay core of [`refold_records`]: restore `start`, spawn a real core over it, pump `tail` IN
/// ORDER, shut it down and read its exit `Snapshot` back, with the execution reports it stated placed by `places`.
fn run_replay_core(
    start: JournalReplayStart,
    tail: Vec<(u64, i64, Ingest)>,
    keyed: bool,
    minted_routes: &[(String, String)],
    (mounts, order_client_id_owners): (&[JournalReplayMount], &[(String, String)]),
    places: &JournalReplayReportPlaces,
    opts: &JournalReplayRefoldOptions<'_>,
) -> Result<ExitSnap, JournalReplayError> {
    // A v2 session's replay core holds the mounts its strategy records are lowered on (`replay_tail`) and
    // owns their orders as the live core did; a v1 one holds none, as before (its ids carry no label).
    let v2 = session_is_v2(&start.order_client_id_session.0);
    let mut replay_mounts: Vec<Mount> =
        if v2 { mounts.iter().map(JournalReplayMount::mount).collect() } else { Vec::new() };
    let order_client_id_owners = if v2 { order_client_id_owners.to_vec() } else { Vec::new() };
    let JournalReplayStart {
        engines: base_engines,
        order_client_id_session,
        arm_sequence: arm_sequence_value,
        order_emulation_arms: conditionals,
        contingencies,
        held_releases,
        emulated,
        recent_order_client_ids,
    } = start;
    let base = &base_engines[0];
    let order_client_id_routes = if keyed {
        fold_order_client_id_routes(&base_engines, &contingencies, minted_routes)
    } else {
        Vec::new()
    };
    let stamps: Vec<i64> = tail.iter().map(|(_, now_ms, _)| *now_ms).collect();

    // Restore the engine and spawn a real core writing to a fresh temp journal. `snapshot_every =
    // u64::MAX` ⇒ no cadence Snapshots fire — only the always-on exit Snapshot, which carries the replayed
    // final hash. `seed_cash` is restored from the base snapshot's `equity_seed` because
    // `spawn_core` OVERWRITES `engine.equity_seed` from `config.seed_cash`, and `equity_seed` is
    // part of `state_hash` — a default (0.0) would diverge for any non-zero-seed source. Each extra
    // engine is handed `spawn_core_multi` with ITS snapshot's `equity_seed`, for the same reason.
    let engine = Engine::from_snapshot(base, ExecutionClientJournalReplay);
    let extras: Vec<(f64, Engine<ExecutionClientJournalReplay>)> = base_engines[1..]
        .iter()
        .map(|s| (s.equity_seed, Engine::from_snapshot(s, ExecutionClientJournalReplay)))
        .collect();
    let temp = unique_temp_dir(opts.scratch_root).map_err(|e| {
        JournalReplayError::Unsupported(format!(
            "the replay core's scratch directory cannot be created under {}: {e}",
            opts.scratch_root.map_or_else(
                || "the system temp directory".to_string(),
                |r| r.display().to_string()
            )
        ))
    })?;
    // `spawn_core` ALSO overwrites `engine.collect_applied_fills` from `config.strategy.is_some()`
    // (`spawn_core_multi`'s mount-presence derivation), clobbering whatever `from_snapshot` just
    // restored from `base`. Unlike `now_ms`/`marks`, `collect_applied_fills` is NOT excluded from
    // `state_hash` — so a mounted-strategy source session (`true`) would otherwise ALWAYS mismatch
    // a mount-free replay (`false`), regardless of how faithfully the tail folds (PR-5 T3's
    // `StrategySubmit` arm above is unrelated to this — it fixes the ORDER flow; this fixes a
    // second, orthogonal config-derived field). Mount an [`JournalReplayInertStrategy`] iff the base had one:
    // that flips the derivation back to `true`, matching `base`, with no other observable effect.
    // ⚠ Single-engine only: on a multi-engine base the flag of an EXTRA engine is re-derived from
    // the mounts too and cannot be mirrored without a mount per venue, so the restore scope mounts
    // nothing — the flag is only `state_hash` input, and the restore fences no hash.
    // ⚠ Not on a keyed base either: the mount names the venue's DEFAULT account, which a lone
    // labelled engine (`sim#ALT`) is not, and the core would refuse the mount at assembly.
    // A core holding replay mounts needs none: they set the flag on their own venues, as the live mounts did.
    let strategy = (replay_mounts.is_empty()
        && !keyed
        && base_engines.len() == 1
        && base.collect_applied_fills)
        .then(|| Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            venue: base.venue.clone(),
            symbol: base.symbol.clone(),
            interval: String::new(),
            strategy: Box::new(JournalReplayInertStrategy),
            underlying_symbol: None,
        });
    let strategy = if replay_mounts.is_empty() { strategy } else { Some(replay_mounts.remove(0)) };
    let config = CoreConfig {
        extra_mounts: replay_mounts,
        seed_cash: base.equity_seed,
        clock: Box::new(JournalReplayQueueClock::new(stamps)),
        order_client_id_session: Some(order_client_id_session),
        arm_sequence: arm_sequence_value,
        recent_order_client_ids,
        // Seed the replay core's books from the base Snapshot (emulator PR-3), exactly as a live
        // restart does — so a tail `DisarmConditional` targeting an arm minted BEFORE the base
        // finds it (pre-PR-3 that disarm missed and refused loudly), and the replay core's own
        // exit Snapshot carries a meaningful book. Membership for the RESTORE still comes from the
        // record fold above, not from this core (module doc, design point 1).
        order_emulation_arms: conditionals,
        // Seed the replay core's OTO/OCO book from the base Snapshot (live-runtime OCO/OTO), exactly as
        // a live restart does — so a tail fill that RELEASES a held child / cancels an OCO sibling
        // of a bracket submitted BEFORE the base is reproduced (its held request came off the Snapshot,
        // not the tail), and the replay core's exit Snapshot carries the true contingency state.
        // UNLIKE conditionals, the RESTORE membership below reads straight from this core's exit
        // Snapshot (not a record fold): a contingency leg has no mark-seeded field the mark-less replay
        // core mishandles — it is driven purely by journaled fills — so the replay core reproduces
        // it faithfully.
        contingencies,
        // The base `Snapshot`'s held orders (`held_release.rs`), held again so a tail release finds its
        // entry; NO release timer: the tail's journaled `OrderOn` records ARE the releases, and a timer
        // would read a clock stamp per boundary and shift every stamp of the tail.
        held_releases,
        // The base `Snapshot`'s emulated parents (decision 0275), so a tail step finds its parent; the replay
        // core re-applies the journaled steps and runs no law (`CoreConfig::replay_core`).
        emulated,
        release_timer: false,
        // The one core that lowers a mount's replayed record as the mount's (`apply_routed_order`).
        replay_core: true,
        // The sessions the journal names: an id one of them minted is OURS in this core's reports, as it was live.
        order_client_id_past_sessions: places.sessions.clone(),
        // the ranges the live core reserved for counted denials: never minted in, so skipped here too
        order_client_id_reserved: places.reserved.clone(),
        order_client_id_routes,
        order_client_id_owners,
        strategy,
        journal: Some(JournalConfig {
            dir: temp.to_path_buf(),
            file: JournalFileConfig::default(),
            snapshot_every: u64::MAX,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core_multi(engine, extras, config);

    // Pump the tail IN ORDER: events onto the lossless event lane, commands onto the same lane via
    // send_command. Both feed the ONE FIFO ingest queue, so dispatch order == journal order. `sent` is the
    // source sequence of each message sent, in order: the replay core journals each as one `Command` record.
    let events = handle.event_sender();
    let mut sent: Vec<u64> = Vec::with_capacity(tail.len());
    for (sequence, _, msg) in tail {
        if matches!(msg, Ingest::Event(_) | Ingest::GatewayEvent(_) | Ingest::Command(_)) {
            sent.push(sequence);
        }
        match msg {
            Ingest::Event(e) => {
                // `CoreGone` = the replay core died mid-tail. The replayed state then diverges from
                // the source journal and the caller's hash fence (`HashMismatch`) refuses it.
                let _ = events.blocking_send(e);
            }
            // A gateway event replays as the plain event it carries: the cursor is the resume point of a
            // trade session, not fold input, and the replay core folds (and journals into its scratch
            // directory) only what moves the fenced state. ⚠ On a base whose engines an order's venue
            // cannot tell apart it replays as the gateway event itself: the ring names the account a
            // FOREIGN fill belongs to (`route_event_via`), the one handle a shared symbol leaves.
            Ingest::GatewayEvent(g) => {
                if keyed {
                    let _ = events.blocking_send_gateway(*g);
                } else {
                    let _ = events.blocking_send(g.event);
                }
            }
            Ingest::Command(c) => handle.send_command(c),
            Ingest::Watchdog => unreachable!("a watchdog record is refused or dropped above"),
            // A trade-link transition is never journaled (`CoreThread::dispatch`'s `journaled`), and
            // it moves no fenced state (it only defers the wall-clock watchdog, which a replay does
            // not run), so a record of one in a foreign journal is skipped, not replayed.
            Ingest::TradeLink(_) => {}
            // An account change carries a live engine and is never journaled; a record of one in a foreign
            // journal cannot exist (it does not serialize), so it is skipped like the other topology.
            Ingest::Accounts(_) => {}
            // A sandboxed strategy's reply is never journaled either (its orders are, as `StrategySubmit`).
            Ingest::MountReply(_) => {}
            // Market/Bar*/Quote/Trade/Book are never journaled (Task 4 journals only the exec lane),
            // so they cannot appear here; skip defensively rather than panic on a future format.
            Ingest::Market
            | Ingest::BarSeed(_)
            | Ingest::BarClose(_)
            | Ingest::Quote(_)
            | Ingest::Trade(_)
            | Ingest::Book(_)
            | Ingest::StreamStatus(_)
            | Ingest::Flow(_)
            | Ingest::DataDelay(_)
            | Ingest::FundingRate(_) => {}
        }
    }
    handle.shutdown_and_join(); // delivers Shutdown losslessly after the tail, then joins

    // (the replay journal directory is removed by `temp`'s guard when this function returns --
    // including on every `?` above and on a panic, which the old explicit removal here did not)
    let replayed = Journal::read_all(&temp)?;
    let mut exit = read_exit_snap(&replayed, arm_sequence_value)?;
    exit.execution_reports = replayed_reports(&replayed, &sent, places);
    Ok(exit)
}

/// **The execution reports a replay core stated, placed in the SOURCE journal** (package 2's C.1b). The replay core
/// writes its reports into its scratch journal like a live core, each naming the scratch record it states the fold
/// of. Each scratch `Command` is the next message the pump `sent` (its source sequence, in order), and each scratch
/// `MintedSubmit` re-mints a tail mint's very id, so the source sequence is that mint's ([`JournalReplayReportPlaces`]).
/// A report whose cause is neither, or every report when the scratch `Command`s do not count one per message sent
/// (the refold did not reproduce the journal's records), is UNPLACED: never guessed.
fn replayed_reports(
    replayed: &[JournalRecord],
    sent: &[u64],
    places: &JournalReplayReportPlaces,
) -> JournalReplayReports {
    let mut source: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let mut commands = 0usize;
    for rec in replayed {
        match rec {
            JournalRecord::Command { sequence, .. } => {
                if let Some(&s) = sent.get(commands) {
                    source.insert(*sequence, s);
                }
                commands += 1;
            }
            JournalRecord::MintedSubmit { sequence, request, .. } => {
                if let Some(&s) = places.minted.get(&request.order_client_id) {
                    source.insert(*sequence, s);
                }
            }
            // every other record: no cause this lookup places
            _ => {}
        }
    }
    let aligned = commands == sent.len();
    let place = |cause: &u64| source.get(cause).copied().filter(|_| aligned);
    let mut reports = JournalReplayReports::default();
    for rec in replayed {
        match rec {
            JournalRecord::ExecutionReport { now_ms, report, .. } => {
                match place(&report.caused_by_sequence) {
                    Some(caused_by_sequence) => reports.placed.push((
                        *now_ms,
                        vike_model::orders::ExecutionReport {
                            caused_by_sequence,
                            ..report.clone()
                        },
                    )),
                    None => reports.unplaced.push(report.order_client_id.clone()),
                }
            }
            // a replay core's marker of an order decided on market data (no other unverified record is written there)
            JournalRecord::ExecutionReportsUnverified {
                through_sequence,
                order_client_ids,
                ..
            } => match place(through_sequence) {
                Some(cause) => reports
                    .market_dependent
                    .extend(order_client_ids.iter().map(|id| (cause, id.clone()))),
                None => reports.unplaced.extend(order_client_ids.iter().cloned()),
            },
            // every other record: no report
            _ => {}
        }
    }
    reports
}

/// Whether this base's engines cannot be told apart by the venue an order names: some engine is a
/// LABELLED account (its snapshot carries a route key) or two engines share a venue. Only under
/// [`JournalReplayRefoldOptions::multi_engine`]: the verifying scope keeps its v1 behaviour.
///
/// The reason is the route a re-applied tail record replays through. A strategy's `StrategySubmit`
/// used to replay as a `Command::Order`, which lowers by the PAYLOAD's venue — the live core
/// lowered it onto the MOUNT's declared engine, and a conditional fire and a margin-call release
/// onto the engine they were read from. On a core with one engine per venue those are the same
/// engine; on a venue with two accounts the payload route reaches the default account or is refused
/// as ambiguous, and on a venue whose accounts are all labelled it reaches no engine of that venue
/// at all. Such a base was therefore restored from its latest `Snapshot` alone. Now each of those
/// records carries the route key of its engine (`StrategySubmit` and `OrderEmulationFire`: written by a
/// core of this shape; `MarginCallLiquidate`: since v16) and [`replayed_order`] lowers it with
/// `Command::OrderOn`, so such a base refolds like any other.
fn keyed_base(engines: &[EngineSnapshot]) -> bool {
    let mut venues: Vec<&str> = engines.iter().map(|e| e.venue.as_str()).collect();
    venues.sort_unstable();
    engines.iter().any(|e| e.route_key.is_some()) || venues.windows(2).any(|w| w[0] == w[1])
}

/// The venues an intent names, for [`replayed_order`]'s rule about a record with no route key: empty
/// for an intent that is routed by an order client id or an arm id, or that names every venue. EXHAUSTIVE on
/// purpose (no catch-all): a new [`vike_execution::OrderIntent`] must decide here how it is routed.
fn intent_venues(intent: &vike_execution::OrderIntent) -> Vec<&str> {
    use vike_execution::OrderIntent as I;
    match intent {
        I::Submit(req) => vec![req.venue.as_str()],
        I::SubmitBatch(reqs) => reqs.iter().map(|r| r.venue.as_str()).collect(),
        I::Bracket(spec) => vec![spec.venue.as_str()],
        I::Combo(spec) => vec![spec.venue.as_str()],
        I::OrderList(spec) => vec![spec.venue.as_str()],
        I::Exercise(spec) => vec![spec.venue.as_str()],
        I::ArmConditional(c) => vec![c.venue.as_str()],
        I::Flatten { venue, .. } => vec![venue.as_str()],
        I::MassCancel { venue, .. } | I::MarketExit { venue, .. } => {
            venue.iter().map(String::as_str).collect()
        }
        I::Cancel(_)
        | I::CancelBatch(_)
        | I::Modify { .. }
        | I::Confirm(_)
        | I::DisarmConditional { .. } => Vec::new(),
    }
}

/// How one re-applied order write is lowered in the replay core of a base [`keyed_base`] says needs
/// routes (`keyed`: that base's engines; `None` for every other base, which lowers by the payload's
/// venue as it always did and ignores the key).
///
/// * **A record that carries a route key** lowers onto that engine (`Command::OrderOn`).
/// * **A record with none** is lowered by the payload's venue ONLY where that names the engine
///   exactly: the venue has one engine in the base and it is its default account (a core of this shape
///   writes no key for such an engine's mount, or for a fire with no recorded engine). Any other venue
///   means the record predates the key (a crash tail the previous build wrote) or was written by a core
///   of a different shape, and its engine cannot be told: the refold REFUSES, and the restore reads the
///   latest `Snapshot` alone as it always did for such a base. For a margin-call release a missing key
///   means "that venue's default account" (`journal_route_key`'s own convention), so it is lowered
///   onto the default engine of its venue by name when the base has one (`none_is_default`).
fn replayed_order(
    keyed: Option<&[EngineSnapshot]>,
    route_key: Option<String>,
    none_is_default: bool,
    intent: vike_execution::OrderIntent,
) -> Result<Ingest, JournalReplayError> {
    use vike_execution::{Command, RoutedOrder};
    let Some(base) = keyed else { return Ok(Ingest::Command(Command::Order(intent))) };
    if let Some(route_key) = route_key {
        return Ok(Ingest::Command(Command::OrderOn(Box::new(RoutedOrder { route_key, intent }))));
    }
    let venues: Vec<String> = intent_venues(&intent).into_iter().map(str::to_string).collect();
    let mut by_name = None;
    for v in &venues {
        let of_venue: Vec<&EngineSnapshot> = base.iter().filter(|e| e.venue == *v).collect();
        match of_venue.as_slice() {
            // One engine, and it is the default account: the payload names it.
            [only] if only.route_key.is_none() => {}
            _ if none_is_default && of_venue.iter().any(|e| e.route_key.is_none()) => {
                by_name = Some(v.clone());
            }
            _ => {
                return Err(JournalReplayError::Unsupported(format!(
                    "multi-engine refold: a re-applied record on venue `{v}` carries no route key, \
                     and the base holds {} engine(s) of that venue that the venue alone cannot \
                     tell apart (the journal was written before the key, or by another shape of \
                     core)",
                    of_venue.len()
                )));
            }
        }
    }
    Ok(match by_name {
        Some(route_key) => {
            Ingest::Command(Command::OrderOn(Box::new(RoutedOrder { route_key, intent })))
        }
        None => Ingest::Command(Command::Order(intent)),
    })
}

/// [`replay_from`]'s tail extraction: the records after `base_index` that replay re-applies, in file
/// order, each paired with its recorded `now_ms`, and every crossed `Snapshot` whose order client id session is not
/// the one minting so far (`base_session` until the first such `Snapshot`), or whose trade-id window is not the one
/// cutting so far (`base_window`), as a [`TailItem::Session`].
/// `keyed` is the base's engines when [`keyed_base`] holds (see [`replayed_order`]), else `None`.
fn replay_tail(
    records: Vec<JournalRecord>,
    base_index: usize,
    (base_session, base_window): (&str, Option<u32>),
    keyed: Option<&[EngineSnapshot]>,
    (mounts, fire_owners): (&[JournalReplayMount], &std::collections::HashMap<usize, String>),
) -> Result<Vec<TailItem>, JournalReplayError> {
    let mut minting = base_session.to_string();
    let mut cutting = base_window;
    let mut v2 = session_is_v2(base_session);
    let msg = |sequence: u64, now_ms: i64, m: Ingest| TailItem::Msg(sequence, now_ms, m);
    // On a v2 session a MOUNT's record (its submit, its budget latch's flatten, the fire of an arm it armed)
    // is lowered as that mount's (`apply_routed_order` takes a mount id as the route key): its id carries
    // the mount's source and strategy code as the live core minted it (decision 0270). A fire lowers on its
    // arm's engine live and on its mount here, the same engine (a mount arms on its own; a foreign leg's
    // payload route is the same either way). A v1 session renders no label, so its records replay exactly
    // as before.
    let on_mount = |v2: bool, mount_id: &str| {
        let id = crate::strategy_state::mount_id_with(Some(mount_id), "", "", "");
        (v2 && mounts.iter().any(|m| m.mount_id == id)).then_some(id)
    };
    let order_on = |route_key: String, intent: vike_execution::OrderIntent| {
        Ingest::Command(vike_execution::Command::OrderOn(Box::new(vike_execution::RoutedOrder {
            route_key,
            intent,
        })))
    };
    // tail = every Command/StrategySubmit positioned AFTER the base Snapshot, consumed by value (Ingest
    // moves out — no clone), keeping each record's recorded now_ms for the JournalReplayQueueClock seed (same
    // order). StrategySubmit (PR-5 T2/T3) carries a mounted strategy's ALREADY-RESOLVED OrderIntent
    // (mint/tag resolution already happened at the `drain_broker` boundary before it was journaled,
    // same as `apply_strategy_intent`'s doc describes) — replaying it as `Command::Order(intent)`
    // folds it through the SAME `apply_intent` site a `Command`-carried `Command::Order` would, so a
    // mounted-strategy session now replays deterministically instead of the tail silently dropping
    // its order flow (the module doc's old "out of v1 scope" note, now stale). `filter_map`
    // preserves file order, so a StrategySubmit interleaves with surrounding Command records correctly
    // without any extra merge step.
    //
    // ⚠ On a base whose engines an order's venue cannot tell apart (`keyed`), the three order writes
    // below are lowered by `replayed_order`: onto the engine their record names, or the whole refold
    // is refused when a record cannot say (`JournalReplayError::Unsupported`, which the restore reads as
    // "restore the latest `Snapshot` alone"). Every other base lowers by the payload's venue, as always.
    records
        .into_iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            JournalRecord::Command { sequence, now_ms, message: m } if i > base_index => {
                Some(Ok(msg(sequence, now_ms, m)))
            }
            JournalRecord::StrategySubmit {
                sequence, now_ms, mount_id, intent, route_key, ..
            } if i > base_index => Some(match on_mount(v2, &mount_id) {
                Some(id) => Ok(msg(sequence, now_ms, order_on(id, intent))),
                None => replayed_order(keyed, route_key, false, intent)
                    .map(|m| msg(sequence, now_ms, m)),
            }),
            // Emulator-journal PR-1: a conditional FIRE replays as the RELEASE it decided —
            // `Submit(req)` with the order client id still empty, through the SAME `apply_intent` site the
            // live fire used, so the replay core re-mints the identical order client id from the restored
            // generator. The trigger itself is NEVER re-evaluated (the bar/tick that crossed it is
            // not journaled and never will be): the DECISION is the record. `trigger_price` is
            // diagnostic only and does not participate.
            JournalRecord::OrderEmulationFire {
                sequence, now_ms, request: req, route_key, ..
            } if i > base_index => {
                let intent = vike_execution::OrderIntent::Submit(Box::new(req));
                Some(match fire_owners.get(&i).and_then(|mid| on_mount(v2, mid)) {
                    Some(id) => Ok(msg(sequence, now_ms, order_on(id, intent))),
                    None => replayed_order(keyed, route_key, false, intent)
                        .map(|m| msg(sequence, now_ms, m)),
                })
            }
            // Emulator PR-4: a margin-call auto-liquidation replays as the RELEASE it decided —
            // `Submit(req)` with the order client id still empty, through the SAME `apply_intent` site the live
            // sweep used, so the replay core re-mints the identical order client id. The breach itself is NEVER
            // re-evaluated (the closed bar + equity that triggered it are not journaled and never
            // will be): the DECISION is the record, exactly as for `OrderEmulationFire`.
            // A mount's budget-latch flatten (the one that names its mount) is that mount's order.
            JournalRecord::MarginCallLiquidate {
                sequence,
                now_ms,
                request: req,
                route_key,
                mount_id,
                ..
            } if i > base_index => {
                let intent = vike_execution::OrderIntent::Submit(Box::new(req));
                Some(match mount_id.as_deref().and_then(|mid| on_mount(v2, mid)) {
                    Some(id) => Ok(msg(sequence, now_ms, order_on(id, intent))),
                    None => replayed_order(keyed, route_key, true, intent)
                        .map(|m| msg(sequence, now_ms, m)),
                })
            }
            // An amend's REPLACEMENT (its mint names the order it `replaces`): the live core placed a
            // cancel-first one at a drain-loop boundary the journal does not record, so the replay core places
            // it HERE, at this record's stamp, keyed by the old order's order client id (`apply_routed_order`'s
            // `place_journaled_replacement`); its own boundaries place none. A new-first one was placed inside
            // the Modify re-applied above it, and this changes nothing.
            JournalRecord::MintedSubmit {
                sequence,
                now_ms,
                request: req,
                replaces: Some(old),
                ..
            } if i > base_index => Some(Ok(msg(
                sequence,
                now_ms,
                order_on(old, vike_execution::OrderIntent::Submit(Box::new(req))),
            ))),
            // A crossed `Snapshot` naming another order client id session (decision 0270's D1: the process that wrote
            // it switched sessions at its boot, before its first mint): the records after it mint
            // under ITS `(order_client_id_session, order_client_id_sequence)`, with the late-retry memory that process held.
            // One stating another trade-id window (a process told another read-back, most often at a restart that
            // kept its session) cuts the `Snapshot`s after it by that window.
            JournalRecord::Snapshot {
                order_client_id_session,
                order_client_id_sequence,
                recent_order_client_ids,
                engines,
                ..
            } if i > base_index
                && (order_client_id_session != minting
                    || core_fill_window(&engines) != cutting) =>
            {
                minting.clone_from(&order_client_id_session);
                v2 = session_is_v2(&order_client_id_session);
                cutting = core_fill_window(&engines);
                Some(Ok(TailItem::Session {
                    order_client_id_session: (order_client_id_session, order_client_id_sequence),
                    recent_order_client_ids,
                    fill_window_days: cutting,
                }))
            }

            // ── EXHAUSTIVE by design: NO `_` arm. A new `JournalRecord` variant must fail to
            // compile HERE until someone decides whether replay re-applies it — see the "Adding a
            // variant" contract on [`JournalRecord`]. Silently dropping one is the bug class this
            // shape exists to make impossible (PR #915's `MarginCallLiquidate` mount_id gap).
            //
            // (a) The four REPLAYABLE records above, at a PRE-BASE position (`i <= base_index`):
            // already folded into the base `Snapshot`, so re-applying them would DOUBLE the state.
            // (The guarded arms above cannot cover a variant for exhaustiveness purposes, which is
            // exactly why this arm is spelled out rather than implied.)
            JournalRecord::Command { .. }
            | JournalRecord::StrategySubmit { .. }
            | JournalRecord::OrderEmulationFire { .. }
            | JournalRecord::MarginCallLiquidate { .. } => None,

            // (b) REPLAY-NEUTRAL at ANY position — checkpoints, observations and decision markers
            // that move no fenced state. Each variant's own doc carries the full argument:
            //   `Snapshot`                — the restore BASE itself, not a command to re-apply (one that
            //                           switched sessions is the guarded arm above).
            //   `MintedSubmit`        — the mint is re-derived by re-applying the empty-order-client-id submit
            //                           through the SAME `apply_intent` site; replaying the record
            //                           too would submit the order twice. (An amend's replacement,
            //                           at a post-base position, is the guarded arm above.)
            //   `PortfolioSnapshot`       — a periodic observation; carries no engine state at all.
            //   `OrderEmulationArmed`    — the arm replays from its own write-ahead `Command`/
            //                           `StrategySubmit`; this record only RESOLVES its terms (and
            //                           `fold_conditionals` reads it for book membership).
            //   `OrderEmulationDisarmed` — ditto; the disarm intent already carries the whole `arm_id`.
            //   `GtdExpire`           — a DECISION MARKER: the sweep's `cancel_order` publishes
            //                           nothing locally, and the venue's authoritative
            //                           `OrderCanceled` replays as its own `Command`.
            //   `ScheduleFire`        — a DECISION MARKER: `on_schedule`'s orders each ride their
            //                           own `StrategySubmit`, so re-firing would double them.
            //   `SharedCloseTimeout`  — a DECISION MARKER: the step it released wrote its orders as
            //                           their own `StrategySubmit`, so they replay in journal order.
            //   `StrategyFileMounted` — an OBSERVATION: a mount is not journaled, and a replay core
            //                           mounts only inert strategies.
            //   `Forecast`            — an OBSERVATION (book W35): a forecast moves no order, and a replay core
            //                           never re-decides.
            //   `ExecutionReport`, `ExecutionReportsUnverified` — OBSERVATIONS derived from a fold the tail
            //                           re-applies through its cause.
            //   `PositionSnapshot`    — an OBSERVATION of the books at a boot; moves nothing.
            //   `DeniedRepeat`        — an OBSERVATION: denied orders move nothing; the ids they spent are a
            //                           range every replay core skips (`JournalReplayReportPlaces::reserved`).
            JournalRecord::Snapshot { .. }
            | JournalRecord::MintedSubmit { .. }
            | JournalRecord::PortfolioSnapshot { .. }
            | JournalRecord::OrderEmulationArmed { .. }
            | JournalRecord::OrderEmulationDisarmed { .. }
            | JournalRecord::GtdExpire { .. }
            | JournalRecord::ScheduleFire { .. }
            | JournalRecord::SharedCloseTimeout { .. }
            | JournalRecord::StrategyFileMounted { .. }
            | JournalRecord::Forecast { .. }
            | JournalRecord::ExecutionReport { .. }
            | JournalRecord::ExecutionReportsUnverified { .. }
            | JournalRecord::PositionSnapshot { .. }
            | JournalRecord::DeniedRepeat { .. } => None,
        })
        .collect()
}

/// The `(order client id, route key)` rows a keyed base's replay core is seeded with
/// (`CoreConfig::order_client_id_routes`): every order the base's engines already hold, under their engine's
/// key, every HELD exit of the base's contingency book under its parent's engine key (a bracket's held exits sit in no
/// registry, and the boot reads from the `Snapshot` on, decision 0304, so their mint is below it), and every order the
/// records minted onto an engine with a key of its own (`MintedSubmit`'s `route_key`: [`fold_minted_routes`]). A pure
/// function over the base and those rows.
fn fold_order_client_id_routes(
    base: &[EngineSnapshot],
    contingencies: &[JournalSnapshotContingency],
    minted: &[(String, String)],
) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for e in base {
        let key = e.route_key.as_deref().unwrap_or(e.venue.as_str());
        rows.extend(
            e.registry
                .iter()
                .map(|(order_client_id, _)| (order_client_id.clone(), key.to_string())),
        );
    }
    let parents: Vec<(String, String)> = contingencies
        .iter()
        .filter(|c| c.held_request.is_some())
        .filter_map(|c| {
            let parent = c.parent.as_deref()?;
            let (_, key) = rows.iter().find(|(id, _)| id == parent)?;
            Some((c.order_client_id.clone(), key.clone()))
        })
        .collect();
    rows.extend(parents);
    rows.extend_from_slice(minted);
    rows
}

/// The `(order client id, route key)` of every order the records minted onto an engine with a key of its own
/// ([`fold_order_client_id_routes`]'s last part).
fn fold_minted_routes(records: &[JournalRecord]) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for rec in records {
        if let JournalRecord::MintedSubmit { request: req, route_key: Some(key), .. } = rec
            && !req.order_client_id.is_empty()
        {
            rows.push((req.order_client_id.clone(), key.clone()));
        }
    }
    rows
}

/// The replay core's exit `Snapshot` among the records of its temp journal (for [`replay_from`]).
fn read_exit_snap(
    replayed: &[JournalRecord],
    arm_sequence_seed: u64,
) -> Result<ExitSnap, JournalReplayError> {
    // The replay journal's final Snapshot: it carries the reproduced hash / engines / order client id state
    // + the replayed arm counter (this build always stamps `arm_sequence`, so the `unwrap_or` below is
    // for shape only — the replay core's exit Snapshot is v7 by construction).
    replayed
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snapshot {
                hash,
                engines,
                order_client_id_session,
                order_client_id_sequence,
                arm_sequence: arm_sequence_value,
                order_emulation_arms: conditionals,
                contingencies,
                held_releases,
                emulated,
                ..
            } => Some(ExitSnap {
                final_hash: *hash,
                engines: engines.clone(),
                order_client_id_session: order_client_id_session.clone(),
                order_client_id_sequence: *order_client_id_sequence,
                arm_sequence: arm_sequence_value.unwrap_or(arm_sequence_seed),
                order_emulation_arms: conditionals.clone(),
                contingencies: contingencies.clone(),
                held_releases: held_releases.clone(),
                emulated: emulated.clone(),
                execution_reports: JournalReplayReports::default(),
            }),
            // every other record: not this tap's business (audited 2026-10, I-21)
            _ => None,
        })
        .ok_or_else(|| JournalReplayError::Unsupported("replay produced no snapshot".into()))
}

/// The state a crash restart restores from the journal (Task 6): the reproduced engine
/// snapshot(s) + the order client id session/seq to resume so post-restart order ids never collide with a
/// still-open prior-session id. Caller pattern: `from_snapshot(&engines[0], real_client)` →
/// re-attach `exec_db` → `spawn_core` with `order_client_id_session: Some((order_client_id_session, order_client_id_sequence))`,
/// `arm_sequence` (resume BOTH or neither — see that field), `conditionals` (re-arm the books — see
/// that field) and
/// `journal: Some(same dir)` (append resumes — seq continues) → venue reconcile repairs any tail
/// the venue saw while down.
#[derive(Debug, Clone)]
pub struct RestoredState {
    pub engines: Vec<EngineSnapshot>,
    pub order_client_id_session: String,
    pub order_client_id_sequence: u64,
    /// The emulated-conditional arm-id counter to resume ([`crate::CoreConfig::arm_sequence`]) — the
    /// arm-side twin of `order_client_id_sequence`, and needed for the SAME reason: an arm id is
    /// `{order_client_id_session}a{arm_sequence}` and the session above is restored, so a counter restarting at 0
    /// would re-emit ids the pre-restart run already wrote into this very journal (resume APPENDS
    /// to the same directory) — and since emulator PR-2 the id is the DISARM key, so a duplicate
    /// would let a `DisarmConditional` target the wrong arm.
    ///
    /// The value is the journal's PERSISTED counter: the base `Snapshot` stamps `arm_sequence` next to
    /// `order_client_id_sequence` (v7), advanced by any tail arms the restore re-folded — exactly the `order_client_id_sequence`
    /// mechanism. The pre-v7 shape (the restored journal's readable RECORD COUNT) was
    /// prune-UNSAFE: [`vike_journal::Journal::prune_before_latest_snap`] moves whole
    /// early segments to the archive, so the count could
    /// land BELOW the ids already spent and a restored session re-minted duplicates. A pre-v7
    /// `Snapshot` (no stamped field) falls back to max-global-record-seq + 1 — still a sound upper
    /// bound under pruning, because record sequences are globally monotonic across segments and every
    /// arm appended at least one record.
    pub arm_sequence: u64,
    /// The resting emulated-conditional books to re-arm ([`crate::CoreConfig::order_emulation_arms`]) —
    /// emulator PR-3's whole point: a live position's protective stop survives the restart. The
    /// latest `Snapshot`'s captured books (a trailing arm's ratcheted extreme included) folded
    /// forward through the crash tail's conditional records (arms added, fires/disarms/
    /// mass-cancels removed — [`fold_conditionals`]). Empty for a pre-v8 journal (its Snapshots
    /// carry no books) and for a session with nothing armed — both restore to empty books,
    /// exactly the pre-PR-3 behavior, never an error. Seed it together with `order_client_id_session` and
    /// `arm_sequence`: the re-armed ids are these entries' `arm_id`s, and the counter must resume
    /// ABOVE them.
    pub order_emulation_arms: Vec<JournalSnapshotOrderEmulationArm>,
    /// The resting OTO/OCO contingency book to re-arm ([`crate::CoreConfig::contingencies`]) — the
    /// live-runtime OCO/OTO twin of `conditionals`: a live position's protective bracket (its held
    /// take-profit / stop-loss) survives the restart. Reproduced by re-folding the crash tail
    /// through the replay core (base `Snapshot`'s book seeded, tail fills re-driving release/cancel) and
    /// read from that core's exit `Snapshot` — no separate record fold, since the contingency book has
    /// no mark-seeded field the mark-less replay core mishandles. Empty for a pre-v11 journal (its
    /// Snapshots carry no contingency state) and for a session with no brackets — both restore empty,
    /// exactly the pre-OCO/OTO behavior, never an error.
    pub contingencies: Vec<JournalSnapshotContingency>,
    /// The orders held for their release to hold again ([`crate::CoreConfig::held_releases`]):
    /// read from the replay core's exit `Snapshot`, like `contingencies`. Empty for a journal written
    /// before `Snapshot.held_releases` and for a core that held none.
    pub held_releases: Vec<OrderHeldRelease>,
    /// The EMULATED parents to work again ([`crate::CoreConfig::emulated`], decision 0275): read from the
    /// replay core's exit `Snapshot`, like `held_releases`.
    pub emulated: Vec<vike_journal::JournalSnapshotOrderEmulation>,
    /// The per-mount fill-ATTRIBUTION ledgers to re-seed ([`crate::CoreConfig::mount_attribution`]) —
    /// multi-mount durability (gap D): without them a restart resumed every mount ledger at zero
    /// while `Account.realized_pnl` kept the same PnL, so `Σ mounts ≠ account` from the first
    /// restart onward. The LATEST `Snapshot`'s captured rows, keyed by `mount_id`. Empty for a pre-v13
    /// journal (its Snapshots carry no ledgers) and for a mount-free session — both restore zeroed,
    /// exactly the pre-feature behavior, never an error.
    ///
    /// DOCUMENTED STALENESS, the same shape (and same safe direction) as `conditionals`: this is the
    /// base `Snapshot`'s value, NOT folded forward through the crash tail. A clean shutdown always writes
    /// an exit `Snapshot`, so the common path is exact; an unclean crash loses attribution for the fills
    /// between the last cadence `Snapshot` and the crash. Those fills are still in `Account` (they
    /// replay as journaled `Command` events), so the discrepancy shows up in the published RESIDUAL row
    /// rather than vanishing — measurable, not silent.
    pub mount_attribution: Vec<vike_journal::JournalSnapshotMountAttribution>,
    /// The order client id -> mount ORIGIN map (the ownership half of [`crate::CoreConfig::journal_orders`];
    /// [`crate::replay::restore_for_boot`] keeps only the order client ids still RESTING) — one
    /// `(order_client_id, mount_id)` per order a mount minted, folded from the journal's existing
    /// `StrategySubmit`/`MintedSubmit` provenance ([`fold_order_client_id_mounts`]) over the WHOLE readable
    /// record set. This is what lets an order still RESTING across the restart book its eventual fill
    /// into the mount that placed it. Empty for a journal with no mounted-strategy submits.
    pub order_client_id_mounts: Vec<(String, String)>,
}

/// A fresh, collision-free working directory for one replay core's journal, OWNED — removed when
/// the returned guard drops, on every path including a panic.
///
/// Uniqueness is the process id + a nanosecond stamp + a per-call counter, so concurrent
/// `replay_offline` calls never share one.
///
/// ⚠ **This is a journal directory, which makes the ownership the expensive half.**
/// `crates/vike-journal/src/segment.rs`'s `reserve_blocks` calls `posix_fallocate` and `JournalConfig::at`
/// defaults to 64 MiB segments, so a segment is FULLY allocated the moment it is created rather
/// than sparse. A leaked replay directory is a real 64 MiB. `replay_from` did remove this at the
/// end — but only on the success path: every `?` above that line, and every panic, left one behind.
/// That is the same shape that put 215 GB of leaked scratch on the CI box, and
/// [`vike_paths::scratch::ScratchDir`] is what closes it here.
///
/// ⚠ **`root: None` is still the system temp directory, and that is a KNOWN debt with a row in
/// `crates/vike-ops/tests/hygiene/system_temp_gate/pin.rs`'s `SYSTEM_TEMP_PIN`, not an oversight.** Production
/// scratch belongs under a root the composition root resolves (a library may not) and passes in
/// as a parameter. The PRODUCTION caller does pass one: the boot
/// restore (`crate::replay::restore_for_boot`, `JournalReplayRefoldOptions::restore`) is handed
/// `<settings>/state/restore-scratch` by
/// `vike-tradehub`. What still reaches `None` are the offline verifiers, [`replay_offline`] and
/// [`restore_from_journal`] (the `replay-journal` tool and ~55 test call sites); giving them the
/// parameter is the mechanical rest of the repair, and the row goes with it.
fn unique_temp_dir(root: Option<&Path>) -> std::io::Result<vike_paths::scratch::ScratchDir> {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tag = format!("vike-replay-{}-{nanos}-{n}", std::process::id());
    let parent = match root {
        Some(root) => root.to_path_buf(),
        None => std::env::temp_dir(),
    };
    vike_paths::scratch::ScratchDir::create_in(&parent, &tag)
}
