//! The PURE functions a restore and its fence are computed from: the record folds, the id floor.

use std::collections::{HashMap, HashSet};

use vike_execution::{EngineSnapshot, Ingest};
use vike_journal::{JournalSnapshotContingency, JournalSnapshotOrderEmulationArm};
use vike_model::EventRingCursor;
use vike_model::OrderRequest;
use vike_model::accounts::account_keys::label_of_route_key;
use vike_model::events::Event;

use super::{JournalRecord, JournalReplayError, OrderOwnerCandidate};

/// The PURE order client id -> mount-id fold behind attribution durability (gap D): which mount MINTED each
/// order, read back out of the journal's existing provenance records.
///
/// Nothing new is written to make this work. `apply_strategy_intent` already journals every mounted
/// strategy's intent as [`JournalRecord::StrategySubmit`] carrying its `mount_id`, write-ahead of the
/// `apply_intent` that mints the order client id — and that mint writes its own [`JournalRecord::MintedSubmit`]
/// with the RESOLVED request immediately after, before any other record can be appended (the fold is
/// single-writer and single-threaded). So the pairing rule is positional and exact:
///
/// - a `StrategySubmit` OPENS ownership by its `mount_id` (and, for the rare intent that already
///   carries a client-supplied order client id, maps that order client id directly — no `MintedSubmit` follows one);
/// - every following `MintedSubmit` maps its minted order client id to the open owner (a `Bracket` intent mints
///   three legs and writes three records — all three belong to the mount that armed them);
/// - a record that begins a NON-strategy write closes ownership: a `Command` (an operator/GUI command),
///   an UNOWNED `MarginCallLiquidate` (the account-wide margin-call sweep), a `Snapshot`, or a
///   `OrderEmulationFire` whose arm no mount armed (an operator's). Their `MintedSubmit`s are therefore
///   correctly left UNATTRIBUTED.
/// - a `OrderEmulationFire` of an arm that a mount ARMED (a `OrderEmulationArmed` journaled while that
///   mount's `StrategySubmit` was open) OPENS ownership by the arming mount: the stop is the strategy's
///   own order, and `CoreThread::submit_fired` attributes it live. A disarm forgets the arm.
/// - a `MarginCallLiquidate` that DOES name a `mount_id` (journal v14 — the per-mount budget latch's
///   flatten, `CoreThread::latch_mount`) OPENS ownership by that id, exactly like a `StrategySubmit`:
///   that order is minted by, and closes the attributed position of, one specific mount.
///
/// Kept a pure function over records (no replay core, no engine) for the same reason
/// [`fold_conditionals`] is: it must be able to reconstruct history the mark-less replay core cannot.
///
/// The `mount_id` arm closes what used to be a documented residual here. The budget latch's flatten
/// attributed its order client id in memory but journaled it indistinguishably from an account-wide
/// liquidation, so on restore that one order came back unattributed and its fill landed in the
/// RESIDUAL row instead of the mount's ledger — under-reporting exactly the realized loss the budget
/// latch exists to bound. A PRE-v14 journal's latch flatten still restores unattributed (the field
/// reads back `None`), which is that journal's own recorded truth rather than a new loss.
pub(crate) fn fold_order_client_id_mounts(records: &[JournalRecord]) -> Vec<(String, String)> {
    fold_order_client_id_origins(records).0
}

/// [`fold_order_client_id_mounts`] plus the arms a MOUNT still owns at the end of the fold: `arm_id ->
/// mount_id` for every `OrderEmulationArmed` journaled under an open `StrategySubmit` and not fired or
/// disarmed since. The boot restore hands that map to the core (`CoreConfig::conditional_owners`)
/// so a restored stop's fire books into the mount that armed it, exactly as `submit_fired` does
/// for a live arm. An entry for an arm a mass-cancel cleared is harmless: only arms that are still
/// in the restored books are looked up in it.
pub(crate) fn fold_order_client_id_origins(
    records: &[JournalRecord],
) -> JournalReplayOrderClientIdOrigins {
    let (origins, _) = fold_order_client_id_origins_with_fires(records);
    origins
}

/// [`fold_order_client_id_origins`]' answer: `(order client id, mount_id)` per minted order, and `arm_id -> mount_id`.
pub(crate) type JournalReplayOrderClientIdOrigins =
    (Vec<(String, String)>, HashMap<String, String>);

/// [`fold_order_client_id_origins`] plus, for each `OrderEmulationFire` of an arm a mount armed, that mount: `record
/// index -> mount_id`. A strategy's fire is the strategy's order in its id too (decision 0270), so a
/// replay lowers it as that mount's (`crates/vike-core/src/replay/refold.rs`'s `replay_tail`).
pub(crate) fn fold_order_client_id_origins_with_fires(
    records: &[JournalRecord],
) -> (JournalReplayOrderClientIdOrigins, HashMap<usize, String>) {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut fire_owner: HashMap<usize, String> = HashMap::new();
    let mut owner: Option<&str> = None;
    // arm id -> the mount that armed it, for an arm journaled while a mount's `StrategySubmit` was
    // open: the fire of that arm is THAT mount's own order (see below).
    let mut arm_owner: HashMap<&str, &str> = HashMap::new();
    // The last arm that fired, and its owner.
    let mut fired: Option<(&str, &str)> = None;
    for (i, rec) in records.iter().enumerate() {
        match rec {
            JournalRecord::StrategySubmit { mount_id, intent, .. } => {
                owner = Some(mount_id.as_str());
                // A client-supplied (non-empty) order client id never reaches the minting path, so it has no
                // `MintedSubmit` to pair with — map it here or lose it.
                if let Some(order_client_id) = client_supplied_order_client_id(intent) {
                    out.push((order_client_id, mount_id.clone()));
                }
            }
            JournalRecord::MintedSubmit { request: req, replaces, .. } => {
                let order_client_id = req.order_client_id.as_str();
                // an amend's replacement is its predecessor's, whatever record the mint follows
                let m = match replaces {
                    Some(old) => out.iter().rev().find(|(c, _)| c == old).map(|(_, m)| m.clone()),
                    None => owner.map(str::to_string),
                };
                if let Some(m) = m.filter(|_| !order_client_id.is_empty()) {
                    out.push((order_client_id.to_string(), m));
                }
            }
            // The per-mount budget latch's flatten NAMES its owner (v14): it OPENS ownership, so the
            // `MintedSubmit` that follows — the order client id `apply_intent` mints for it — books into that
            // mount's ledger, identically to the live runtime's own `order_client_id_mount` insert.
            JournalRecord::MarginCallLiquidate { mount_id: Some(mid), .. } => {
                owner = Some(mid.as_str());
            }
            // A conditional's FIRE reopens ownership with the mount that ARMED it, so the
            // `MintedSubmit` the release writes restores into that mount's ledger, identically to
            // the live runtime's `submit_fired` write. An arm nobody owned (an operator's, under a
            // `Command`; or one whose `OrderEmulationArmed` is no longer in view because
            // `prune_before_latest_snap` deleted the early segment holding it: this fold reads the
            // whole READABLE record set, so a pre-`Snapshot` arm is normally seen) has no entry
            // and the release CLOSES ownership: unattributed, as before.
            JournalRecord::OrderEmulationArmed { arm_id, .. } => {
                if let Some(m) = owner {
                    arm_owner.insert(arm_id.as_str(), m);
                }
            }
            JournalRecord::OrderEmulationDisarmed { arm_id, .. } => {
                arm_owner.remove(arm_id.as_str());
            }
            // A stop larger than its position fires as two records of one arm (`submit_fired`): the second is its too.
            JournalRecord::OrderEmulationFire { arm_id, .. } => {
                owner = match arm_owner.remove(arm_id.as_str()) {
                    Some(mount) => {
                        fired = Some((arm_id.as_str(), mount));
                        Some(mount)
                    }
                    None => fired.filter(|(arm, _)| *arm == arm_id.as_str()).map(|(_, m)| m),
                };
                if let Some(m) = owner {
                    fire_owner.insert(i, m.to_string());
                }
            }
            JournalRecord::Command { .. }
            | JournalRecord::Snapshot { .. }
            // ...whereas an UNOWNED `MarginCallLiquidate` is the account-wide margin-call sweep
            // (and every pre-v14 frame, which defaults to `None`) — a release no mount originated,
            // so it CLOSES.
            | JournalRecord::MarginCallLiquidate { mount_id: None, .. } => owner = None,

            // ── EXHAUSTIVE by design: NO `_` arm (see [`JournalRecord`]'s "Adding a variant"
            // contract). These three are observations/markers that MINT NOTHING, so they neither
            // open nor close ownership — an interleaved one must leave a `StrategySubmit` ->
            // `MintedSubmit` pair intact, which is why they are ignored rather than closing:
            //   `PortfolioSnapshot`       — a periodic portfolio observation.
            //   `GtdExpire`           — an expiry DECISION; issues a cancel, mints no order client id.
            //   `ScheduleFire`        — an on_schedule DECISION; the orders it produces arrive as
            //                           their own `StrategySubmit` records, which open ownership.
            //   `SharedCloseTimeout`  — a shared close's DECISION marker; the step's orders arrive
            //                           as their own `StrategySubmit` records.
            //   `StrategyFileMounted` — a mount opened on a strategy file; an observation, mints nothing.
            // A new variant that DOES mint an order client id must be classified into one of the arms above,
            // not added here — the compiler will demand the decision.
            JournalRecord::PortfolioSnapshot { .. }
            | JournalRecord::GtdExpire { .. }
            | JournalRecord::ScheduleFire { .. }
            | JournalRecord::SharedCloseTimeout { .. }
            | JournalRecord::StrategyFileMounted { .. } => {}
        }
    }
    let owners = arm_owner.into_iter().map(|(a, m)| (a.to_string(), m.to_string())).collect();
    ((out, owners), fire_owner)
}

/// The order client id a journaled intent ALREADY carries, if any — the one case [`fold_order_client_id_mounts`] cannot
/// pair with a following `MintedSubmit`, because a non-empty `order_client_id` skips the mint
/// entirely (and therefore writes no such record).
fn client_supplied_order_client_id(intent: &vike_execution::OrderIntent) -> Option<String> {
    match intent {
        vike_execution::OrderIntent::Submit(req) if !req.order_client_id.is_empty() => {
            Some(req.order_client_id.clone())
        }
        vike_execution::OrderIntent::Submit(_)
        | vike_execution::OrderIntent::SubmitBatch(_)
        | vike_execution::OrderIntent::Cancel(_)
        | vike_execution::OrderIntent::CancelBatch(_)
        | vike_execution::OrderIntent::Modify { .. }
        | vike_execution::OrderIntent::Confirm(_)
        | vike_execution::OrderIntent::MassCancel { .. }
        | vike_execution::OrderIntent::Flatten { .. }
        | vike_execution::OrderIntent::MarketExit { .. }
        | vike_execution::OrderIntent::Bracket(_)
        | vike_execution::OrderIntent::ArmConditional(_)
        | vike_execution::OrderIntent::DisarmConditional { .. }
        | vike_execution::OrderIntent::Combo(_)
        | vike_execution::OrderIntent::OrderList(_)
        | vike_execution::OrderIntent::Exercise(_) => None,
    }
}

/// The PURE membership fold behind re-arm-on-restore (emulator PR-3): the resting conditional
/// books at the end of a record sequence, derived from the base `Snapshot`'s captured books plus the
/// tail's conditional records — never from re-evaluating a trigger, and never from the replay
/// core's book state (see the module doc's design point 1 for why that state is not trusted).
///
/// Fold rules, mirroring the live `apply_intent`/`submit_fired` semantics record-for-record:
/// - [`JournalRecord::OrderEmulationArmed`] ADDS the arm (its RESOLVED terms — a trailing arm's
///   mark-seeded extreme included). A refusal that happens BEFORE the arm id is spent writes
///   nothing; the duplicate-id backstop is the exception: it journals this record and spends the id
///   before the book refuses, and the `!books.iter().any(..)` guard below is what keeps the first arm.
///   Appended in record order, which IS live insertion (fire) order.
/// - [`JournalRecord::OrderEmulationFire`] / [`JournalRecord::OrderEmulationDisarmed`] REMOVE the arm
///   they name (both are written only when the live book actually changed).
/// - A `Command`/`StrategySubmit`-carried [`vike_execution::OrderIntent::MassCancel`] clears its scope
///   exactly as the live arm does (all books / one venue / one (venue, symbol); a symbol without
///   a venue is ignored, the live no-op), and [`vike_execution::OrderIntent::MarketExit`] clears its
///   venue scope (its lowering's mass-cancel leg is venue-scoped, symbol-`None`).
/// - The `Command`-carried `ArmConditional`/`DisarmConditional` intents themselves are IGNORED: the
///   `OrderEmulationArmed`/`OrderEmulationDisarmed` records above are their authoritative outcomes
///   (an intent can be refused; the record is only written for an applied mutation).
///
/// Returns the folded books plus the count of `OrderEmulationArmed` records seen (the arm-counter
/// bound `replay_from` patches with).
pub(crate) fn fold_conditionals(
    mut books: Vec<JournalSnapshotOrderEmulationArm>,
    tail: &[JournalRecord],
) -> (Vec<JournalSnapshotOrderEmulationArm>, u64) {
    let mut armed_count = 0u64;
    for rec in tail {
        match rec {
            JournalRecord::OrderEmulationArmed { arm_id, resolved, .. } => {
                armed_count += 1;
                if !books.iter().any(|c| c.arm_id == *arm_id) {
                    // A crash-tail arm's record carries no account: `route_key` stays `None`.
                    books.push(JournalSnapshotOrderEmulationArm {
                        arm_id: arm_id.clone(),
                        terms: resolved.clone(),
                        route_key: None,
                    });
                }
            }
            JournalRecord::OrderEmulationFire { arm_id, .. }
            | JournalRecord::OrderEmulationDisarmed { arm_id, .. } => {
                books.retain(|c| c.arm_id != *arm_id);
            }
            JournalRecord::Command {
                message: Ingest::Command(vike_execution::Command::Order(intent)),
                ..
            } => fold_intent_scope(&mut books, intent),
            JournalRecord::StrategySubmit { intent, .. } => fold_intent_scope(&mut books, intent),

            // ── EXHAUSTIVE by design: NO `_` arm (see [`JournalRecord`]'s "Adding a variant"
            // contract). None of these can change book MEMBERSHIP:
            //   `Command` carrying anything but an order intent — a venue `Event`, a `Watchdog`, or a
            //     non-`Order` command. (The refined `Command` arm above catches the order-intent case;
            //     this one is its complement, and is what keeps the match exhaustive over `Command`.)
            //   `Snapshot`                — the fold's own STARTING books; never re-applied here.
            //   `MintedSubmit`        — a resolved order request; arms are not orders.
            //   `PortfolioSnapshot`       — a portfolio observation.
            //   `MarginCallLiquidate` — releases a reduce-only MARKET, arms/disarms nothing.
            //   `GtdExpire`           — expires a RESTING ORDER, not an arm (a mass-cancel-cleared
            //                           arm folds through its own intent, above).
            //   `ScheduleFire`        — an on_schedule DECISION; any arm it produced wrote its own
            //                           `OrderEmulationArmed`.
            //   `SharedCloseTimeout`  — a shared close's DECISION marker; any arm the step produced
            //                           wrote its own `OrderEmulationArmed`.
            //   `StrategyFileMounted` — a mount opened on a strategy file; arms nothing.
            JournalRecord::Command { .. }
            | JournalRecord::Snapshot { .. }
            | JournalRecord::MintedSubmit { .. }
            | JournalRecord::PortfolioSnapshot { .. }
            | JournalRecord::MarginCallLiquidate { .. }
            | JournalRecord::GtdExpire { .. }
            | JournalRecord::ScheduleFire { .. }
            | JournalRecord::SharedCloseTimeout { .. }
            | JournalRecord::StrategyFileMounted { .. } => {}
        }
    }
    (books, armed_count)
}

/// The book-clearing half of [`fold_conditionals`]: apply one journaled intent's effect on book
/// MEMBERSHIP (`MassCancel`/`MarketExit` scoping — the live `apply_intent` arms' exact clear
/// semantics). Every other intent leaves the books untouched (arms/disarms fold from their own
/// records instead — see the caller's doc).
///
/// ⚠ **The reducing verbs' `account`, mirrored as far as a record can carry it.** An account
/// named with NO venue clears NOTHING: the live core refuses that intent rather than reading it as
/// the global clear (`CoreThread::reduce_route_for_account`), so folding it as `(None, None)` would
/// restore books the live session never lost — every protective stop gone after a restart. An
/// account named ON a venue clears that venue's scope, because this fold is single-engine by
/// construction (`replay_from` refuses a multi-engine base) and on one engine a held account's
/// narrowed clear IS the venue scope.
///
/// **Residual, declared:** a labelled intent the live core REFUSED as unheld still clears here.
/// Telling the two apart needs the base engine's route key, and `EngineSnapshot` carries none —
/// the same missing fact `CoreConfig::order_emulation_arms`' residual names. It is reachable only by a
/// frame that passed the node's edge while it could not check (before the core's first publish)
/// or by a caller that bypasses the edge; `vike_tradehub::server::refusal::account_refusal` refuses an
/// unheld account before the Ack otherwise, so no write-ahead record of it is ever made.
fn fold_intent_scope(
    books: &mut Vec<JournalSnapshotOrderEmulationArm>,
    intent: &vike_execution::OrderIntent,
) {
    let clear = |books: &mut Vec<JournalSnapshotOrderEmulationArm>,
                 venue: Option<&str>,
                 symbol: Option<&str>| {
        match (venue, symbol) {
            (None, None) => books.clear(),
            (Some(v), None) => books.retain(|c| c.terms.venue != v),
            (Some(v), Some(s)) => books.retain(|c| !(c.terms.venue == v && c.terms.symbol == s)),
            (None, Some(_)) => {} // the live arm ignores symbol-without-venue
        }
    };
    match intent {
        // An account with no venue: refused live, so no clear.
        vike_execution::OrderIntent::MassCancel { venue: None, account: Some(_), .. }
        | vike_execution::OrderIntent::MarketExit { venue: None, account: Some(_) } => {}
        vike_execution::OrderIntent::MassCancel { venue, symbol, .. } => {
            clear(books, venue.as_deref(), symbol.as_deref());
        }
        vike_execution::OrderIntent::MarketExit { venue, .. } => {
            clear(books, venue.as_deref(), None)
        }
        vike_execution::OrderIntent::Submit(_)
        | vike_execution::OrderIntent::SubmitBatch(_)
        | vike_execution::OrderIntent::Cancel(_)
        | vike_execution::OrderIntent::CancelBatch(_)
        | vike_execution::OrderIntent::Modify { .. }
        | vike_execution::OrderIntent::Confirm(_)
        | vike_execution::OrderIntent::Flatten { .. }
        | vike_execution::OrderIntent::Bracket(_)
        | vike_execution::OrderIntent::ArmConditional(_)
        | vike_execution::OrderIntent::DisarmConditional { .. }
        | vike_execution::OrderIntent::Combo(_)
        | vike_execution::OrderIntent::OrderList(_)
        | vike_execution::OrderIntent::Exercise(_) => {}
    }
}

/// What the crash TAIL after a base `Snapshot` says about orders, for the boot restore's SNAP-ONLY
/// reading (`crate::replay::restore_for_boot` when the refold cannot run): the orders it MINTED and
/// the order client ids a journaled venue event ENDED.
pub(crate) struct JournalReplayTailFacts {
    /// Every `MintedSubmit` of the tail, in order: the resolved request and the route key it was
    /// lowered onto (`None` for the venue's sole/default account, and on a pre-v16 frame).
    pub(crate) minted: Vec<(OrderRequest, Option<String>)>,
    /// Every order client id a journaled event ended ([`Event::ended_order`]).
    pub(crate) terminal: HashSet<String>,
    /// Every order client id the tail USED, with its record's `now_ms`: each `MintedSubmit`'s, and each
    /// one a `Command` or `StrategySubmit` named itself ([`named_order_client_ids`]), answered or not: the tail's half of
    /// the late-retry memory (decision 0270's D7).
    pub(crate) used: Vec<(String, i64)>,
}

impl JournalReplayTailFacts {
    /// Whether the tail ended `order_client_id`.
    pub(crate) fn ended(&self, order_client_id: &str) -> bool {
        self.terminal.contains(order_client_id)
    }
}

/// [`JournalReplayTailFacts`] of `tail`. Three single-variant LOOKUPS, so the catch-all is the
/// "Adding a variant" contract's item 3 on `JournalRecord` (a new variant that mints or ends an
/// order still does it through a `MintedSubmit` or a journaled venue event, and one that names an id
/// carries an `OrderIntent`).
pub(crate) fn tail_facts(tail: &[JournalRecord]) -> JournalReplayTailFacts {
    let mut facts =
        JournalReplayTailFacts { minted: Vec::new(), terminal: HashSet::new(), used: Vec::new() };
    for rec in tail {
        match rec {
            JournalRecord::MintedSubmit { request: req, route_key, now_ms, .. } => {
                if !req.order_client_id.is_empty() {
                    facts.used.push((req.order_client_id.clone(), *now_ms));
                }
                facts.minted.push((req.clone(), route_key.clone()));
            }
            // `venue_event`, not an `Ingest::Event` pattern: a terminal that came over a gateway's trade
            // link is journaled as an `Ingest::GatewayEvent` (v18) and ended its order all the same.
            // Read as `Event` only, a closed order would restore as open until the first pass.
            JournalRecord::Command { message: msg, now_ms, .. } => {
                if let Some(order_client_id) = msg.venue_event().and_then(Event::ended_order) {
                    facts.terminal.insert(order_client_id.to_string());
                }
                let intent = match msg {
                    Ingest::Command(vike_execution::Command::Order(intent)) => Some(intent),
                    Ingest::Command(vike_execution::Command::OrderOn(routed)) => {
                        Some(&routed.intent)
                    }
                    _ => None,
                };
                for order_client_id in intent.map(named_order_client_ids).unwrap_or_default() {
                    facts.used.push((order_client_id.to_string(), *now_ms));
                }
            }
            JournalRecord::StrategySubmit { intent, now_ms, .. } => {
                facts.used.extend(
                    named_order_client_ids(intent).into_iter().map(|c| (c.to_string(), *now_ms)),
                );
            }
            _ => {}
        }
    }
    facts
}

/// The order client ids an intent NAMES itself (a sender's own, never minted, so no `MintedSubmit`
/// records them): a submit's, a batch's, a combo's and an order list's legs'. A bracket's stem is no
/// order id; the legs it derives are minted. EXHAUSTIVE on purpose (no catch-all): a new
/// [`vike_execution::OrderIntent`] must decide here whether it carries a sender's id.
fn named_order_client_ids(intent: &vike_execution::OrderIntent) -> Vec<&str> {
    use vike_execution::OrderIntent as I;
    let ids: Vec<&str> = match intent {
        I::Submit(req) => vec![req.order_client_id.as_str()],
        I::SubmitBatch(reqs) => reqs.iter().map(|r| r.order_client_id.as_str()).collect(),
        I::Combo(spec) => spec.order_client_id.iter().map(String::as_str).collect(),
        I::OrderList(spec) => {
            spec.legs.iter().filter_map(|l| l.order_client_id.as_deref()).collect()
        }
        I::Bracket(_)
        | I::Cancel(_)
        | I::CancelBatch(_)
        | I::Modify { .. }
        | I::Confirm(_)
        | I::MassCancel { .. }
        | I::Flatten { .. }
        | I::MarketExit { .. }
        | I::ArmConditional(_)
        | I::DisarmConditional { .. }
        | I::Exercise(_) => Vec::new(),
    };
    ids.into_iter().filter(|id| !id.is_empty()).collect()
}

/// The gateway ring cursors a REFOLDED boot restore hands back (journal v18): per route key, where its
/// account's ring stood at the END of the readable journal. `base` is the latest `Snapshot`'s
/// `gateway_cursors` (an account quiet since the checkpoint has no record left in `tail` and keeps
/// the checkpoint's); each `GatewayEvent` of the `tail`, in journal order, then advances its
/// account's cursor by [`EventRingCursor::after`] -- the very rule the live fold applies, so the
/// cursor a restart presents is the one the core would have held had it never stopped.
///
/// Pure over records, like the folds around it. Only the Full refold folds the tail's events into an
/// engine, so only that path may return this: a snap-only restore keeps `base` unadvanced. A single-variant LOOKUP on purpose
/// (`if let`, the item 3 of [`vike_journal::JournalRecord`]'s "Adding a variant" contract): it asks
/// "is this THE gateway event?", not "what is this?".
///
/// **Only an account whose ENGINE is in `engines` advances.** The refold builds its replay core from the
/// base `Snapshot`'s engines alone, so a tail event of an account the `Snapshot` does not know (armed after the
/// last checkpoint, then traded, then a crash) reaches no engine: a cursor past it would tell the
/// gateway the event was applied and it would never send it again. Such an account keeps the `Snapshot`'s
/// cursor, or has none and opens its session fresh (a reconcile then recovers what it can). An engine
/// answers to its `route_key`, the venue id for the venue's default account.
pub(crate) fn fold_gateway_cursors(
    base: &[(String, EventRingCursor)],
    engines: &[EngineSnapshot],
    tail: &[JournalRecord],
) -> Vec<(String, EventRingCursor)> {
    let has_engine = |key: &str| {
        engines.iter().any(|e| e.route_key.as_deref().unwrap_or(e.venue.as_str()) == key)
    };
    let mut held: Vec<(String, EventRingCursor)> = base.to_vec();
    for rec in tail {
        if let JournalRecord::Command { message: Ingest::GatewayEvent(g), .. } = rec
            && has_engine(g.route_key.as_str())
        {
            match held.iter_mut().find(|(key, _)| key.as_str() == g.route_key.as_str()) {
                Some((_, cursor)) => *cursor = cursor.after(g.cursor),
                None => held.push((g.route_key.to_string(), g.cursor)),
            }
        }
    }
    held
}

/// **What a mount's closed-trade ledger books from a journal's TAIL** (decision 0271 item 8,
/// `crates/vike-core/src/runtime/closed_trades.rs`'s "A restart"): the venue events after the latest `Snapshot` that
/// the live core booked into a ledger, in journal order. They are journaled as events already and nothing of the
/// ledger is (the `Snapshot` carries its state), so a restart resumes each mount's ledger from the `Snapshot` and books
/// each of these on it through the same call: a fill the engine folded cannot be missing from the ledger.
#[derive(Debug, Clone, PartialEq)]
pub enum TradeClosedTail {
    /// A venue fill the engine folded. `owner` is the mount the journal says minted its order (a
    /// `StrategySubmit` and the `MintedSubmit` after it), `None` where the journal does not say (the core asks
    /// its own ownership table then); `margin_close` when the account-wide margin call released its order (a
    /// `MarginCallLiquidate` no mount owns, and the `MintedSubmit` after it): a liquidation.
    Fill { fill: vike_model::events::FillEvent, owner: Option<String>, margin_close: bool },
    /// The venue's own liquidation of a position.
    Liquidated(vike_model::events::PositionLiquidated),
}

/// The [`TradeClosedTail`] of `tail`, the records after the base `Snapshot` whose `engines` it holds: each venue fill an
/// engine folded (its trade id new to them: in no base engine's `seen_trade_ids` and not earlier in the tail; an
/// empty id folds every time), and each venue liquidation, in journal order. `owners` is the journal's
/// order client id -> mount fold (`fold_order_client_id_origins`). Pure.
pub(crate) fn closed_tail(
    tail: &[JournalRecord],
    engines: &[EngineSnapshot],
    owners: &HashMap<String, String>,
) -> Vec<TradeClosedTail> {
    let mut seen: HashSet<&str> =
        engines.iter().flat_map(|e| e.seen_trade_ids.iter().map(String::as_str)).collect();
    let mut seen_liquidation: HashSet<&str> =
        engines.iter().flat_map(|e| e.seen_liquidation_ids.iter().map(String::as_str)).collect();
    let mut margin_closes: HashSet<&str> = HashSet::new();
    let mut margin_open = false;
    let mut out = Vec::new();
    for rec in tail {
        // A margin call's release journals its `MintedSubmit` right after it, before any other record.
        let after_margin_call = std::mem::replace(
            &mut margin_open,
            matches!(rec, JournalRecord::MarginCallLiquidate { mount_id: None, .. }),
        );
        match rec {
            JournalRecord::MintedSubmit { request: req, .. } if after_margin_call => {
                margin_closes.insert(req.order_client_id.as_str());
            }
            JournalRecord::Command { message: msg, .. } => match msg.venue_event() {
                Some(Event::Fill(f)) => {
                    let tid = f.trade_id.as_str();
                    if tid.is_empty() || seen.insert(tid) {
                        let order_client_id = f.order_client_id.as_str();
                        out.push(TradeClosedTail::Fill {
                            fill: f.clone(),
                            owner: owners.get(order_client_id).cloned(),
                            margin_close: margin_closes.contains(order_client_id),
                        });
                    }
                }
                Some(Event::PositionLiquidated(p)) => {
                    let tid = p.trade_id.as_str();
                    if tid.is_empty() || seen_liquidation.insert(tid) {
                        out.push(TradeClosedTail::Liquidated(p.clone()));
                    }
                }
                // every other event: no ledger books it
                _ => {}
            },
            // every other record: not this fold's business
            _ => {}
        }
    }
    out
}

/// The account LABEL TEXT an engine's route key names on `venue` — the spelling
/// `crate::order_owners::OrderOwnerEntry::account` is written in (`None` for the venue's default
/// account, and for a key that is not one of `venue`'s).
fn account_text(venue: &str, route_key: Option<&str>) -> Option<String> {
    let label = label_of_route_key(venue, route_key?)?;
    label.text().map(str::to_string)
}

/// The orders the journal says a MOUNT left RESTING, as the ownership candidates the boot restore
/// hands the core (`crate::CoreConfig::journal_orders`) — the fix to [`fold_order_client_id_mounts`]' "every
/// order client id ever minted", which is the third reason decision 0113 gave for not restoring from the
/// journal.
///
/// "Resting" is: NON-terminal in a restored engine's registry (and not `Emulated`, a local order no
/// venue holds); when the restore could not refold the tail (`tail` is `Some`), additionally every
/// order the tail MINTED, and minus every order client id the tail ended. PLUS every HELD exit of
/// `contingencies` (an OTO child kept off the venue until its parent fills): no venue reports it,
/// but its fill after a release must still find its mount. An order the gate refused before it
/// reached a registry is not one. Only an order client id some mount minted ([`fold_order_client_id_mounts`]' `owners`)
/// becomes a candidate under its MOUNT (an operator's order has none: the exception below). Each
/// carries what the journal knows: the venue and symbol, the account label of the
/// engine it is on (a held exit takes its parent's), its limit price (decision 0120's verdict
/// 8: the price an adopted order is put back with) and, because a venue report carries none of
/// them, its kind, `reduce_only` flag and trigger price (so an adopted stop stays one), plus the tier
/// of the engine's snapshot (`OrderOwnerCandidate::tier`, in memory).
///
/// **One exception to "only an order client id some mount minted":** a RESTING PROTECTIVE STOP
/// (`crate::order_owners::is_resting_protective_stop`) or a RESTING LIMIT
/// (`crate::order_owners::is_resting_limit`) with no mount owner, in the registry of an engine that
/// is not a paper book, is an operator's (the control door, the Trade window), and becomes a
/// candidate under the reserved pseudo mount (`crate::order_owners::PROTECTIVE_STOP_MOUNT`), so the
/// pass adopts it instead of leaving our own order an `UnknownOrder`. Only the registry is read for
/// that: an order minted in an un-refolded tail stays as it was (the final `Snapshot` of a clean stop has
/// everything). This function only NAMES the candidate; whether it is adopted (a `demo` or `live` book, a venue
/// whose restore gate is open) is `runtime/boot_restore.rs`'s `merge_journal_owners`.
pub(crate) fn resting_orders(
    engines: &[EngineSnapshot],
    tail: Option<&JournalReplayTailFacts>,
    contingencies: &[JournalSnapshotContingency],
    owners: &HashMap<String, String>,
) -> Vec<OrderOwnerCandidate> {
    let ended = |order_client_id: &str| tail.is_some_and(|t| t.ended(order_client_id));
    let mut out: Vec<OrderOwnerCandidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // order client id -> the account label text of the engine holding it, for a held exit's parent.
    let mut account_of: HashMap<&str, Option<String>> = HashMap::new();
    // `(tier of the engine it rests on, operator's order)` besides the order itself.
    let mut push = |order_client_id: &str,
                    req: &OrderRequest,
                    account: Option<String>,
                    origin: (Option<vike_model::AccountTier>, bool, bool)| {
        let (tier, operator_order, lifetime_known) = origin;
        let owner = owners
            .get(order_client_id)
            .map(String::as_str)
            .or(operator_order.then_some(crate::order_owners::PROTECTIVE_STOP_MOUNT));
        if let Some(mount_id) = owner
            && seen.insert(order_client_id.to_string())
        {
            out.push(OrderOwnerCandidate {
                order_client_id: order_client_id.to_string(),
                mount_id: mount_id.to_string(),
                venue: Some(req.venue.clone()),
                symbol: Some(req.symbol.clone()),
                account,
                price: req.price,
                reduce_only: req.reduce_only,
                trigger_price: req.trigger_price,
                time_in_force: lifetime_known.then_some(req.time_in_force),
                gtd_expiry: req.gtd_expiry,
                order_type: Some(req.order_type.clone()),
                tier,
            });
        }
    };
    for e in engines {
        let account = account_text(&e.venue, e.route_key.as_deref());
        // A paper book is emptied by a restart (`restored_paper_orders_gone_at_boot`): its operator
        // orders are no candidates. An engine that states no tier is an old or paper one.
        let venue_book = e.tier.is_some_and(|t| t != vike_model::AccountTier::Paper);
        for (order_client_id, mo) in &e.registry {
            account_of.insert(order_client_id.as_str(), account.clone());
            let gone = mo.status.is_terminal() || mo.status == vike_model::OrderStatus::Emulated;
            if gone || ended(order_client_id) {
                continue;
            }
            let operator_order = venue_book
                && (crate::order_owners::is_resting_protective_stop(mo)
                    || crate::order_owners::is_resting_limit(mo));
            // Only an order the node placed carries `created_ms`. A report's rebuild has none, and the
            // previous release's `Snapshot` of one states no `lifetime_unknown`, only the type's `GTC`. What
            // this release put back on an adopted order, its ownership line remembers (`merge_journal_owners`).
            let lifetime_known = mo.created_ms.is_some();
            push(
                order_client_id,
                &mo.request,
                account.clone(),
                (e.tier, operator_order, lifetime_known),
            );
        }
    }
    if let Some(t) = tail {
        for (req, route_key) in &t.minted {
            let account = account_text(&req.venue, route_key.as_deref());
            account_of.insert(req.order_client_id.as_str(), account.clone());
            if req.order_client_id.is_empty() || t.ended(&req.order_client_id) {
                continue;
            }
            push(&req.order_client_id, req, account, (None, false, true));
        }
    }
    for c in contingencies {
        let Some(req) = &c.held_request else { continue };
        if ended(&c.order_client_id) || c.parent.as_deref().is_some_and(ended) {
            continue;
        }
        let account = c.parent.as_deref().and_then(|p| account_of.get(p).cloned()).flatten();
        push(&c.order_client_id, req, account, (None, false, true));
    }
    out
}

/// The id-counter half of the widened fence: a reproduced counter may OVERSHOOT what the live
/// session recorded but must never land BELOW it.
///
/// The asymmetry is the whole point, and it is a safety property rather than a determinism one.
/// An overshoot only wastes ids (`replay_from` deliberately produces one for a mark-refused tail
/// trailing arm, and again for a pre-v7 base — both documented there). An UNDERCOUNT means a
/// restored session re-mints an id the pre-crash run already spent into this very journal — and
/// since emulator PR-2 an `arm_id` is the DISARM key, so a duplicate lets a stale
/// `DisarmConditional` silently remove a fresh stop-loss. Fencing the floor generalizes the one
/// hand-written scenario that class of bug had.
pub(crate) fn fence_floor(
    field: &'static str,
    got: u64,
    expected: u64,
) -> Result<(), JournalReplayError> {
    if got < expected {
        return Err(JournalReplayError::RestoreMismatch { field, expected, got });
    }
    Ok(())
}
