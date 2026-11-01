//! The PURE functions a restore and its fence are computed from: the record folds, the id floor.

use std::collections::{HashMap, HashSet};

use vike_exec::{EngineSnapshot, GatewayCursor, Ingest};
use vike_journal::{SnapConditional, SnapContingency};
use vike_model::OrderRequest;
use vike_model::accounts::account_keys::label_of_route_key;
use vike_model::events::Event;

use super::{JournalOrder, JournalRecord, ReplayError};

/// The PURE coid -> mount-id fold behind attribution durability (gap D): which mount MINTED each
/// order, read back out of the journal's existing provenance records.
///
/// Nothing new is written to make this work. `apply_strategy_intent` already journals every mounted
/// strategy's intent as [`JournalRecord::StrategySubmit`] carrying its `mount_id`, write-ahead of the
/// `apply_intent` that mints the coid — and that mint writes its own [`JournalRecord::MintedSubmit`]
/// with the RESOLVED request immediately after, before any other record can be appended (the fold is
/// single-writer and single-threaded). So the pairing rule is positional and exact:
///
/// - a `StrategySubmit` OPENS ownership by its `mount_id` (and, for the rare intent that already
///   carries a client-supplied coid, maps that coid directly — no `MintedSubmit` follows one);
/// - every following `MintedSubmit` maps its minted coid to the open owner (a `Bracket` intent mints
///   three legs and writes three records — all three belong to the mount that armed them);
/// - a record that begins a NON-strategy write closes ownership: a `Cmd` (an operator/GUI command),
///   an UNOWNED `MarginCallLiquidate` (the account-wide margin-call sweep), a `Snap`, or a
///   `ConditionalFire` whose arm no mount armed (an operator's). Their `MintedSubmit`s are therefore
///   correctly left UNATTRIBUTED.
/// - a `ConditionalFire` of an arm that a mount ARMED (a `ConditionalArmed` journaled while that
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
/// attributed its coid in memory but journaled it indistinguishably from an account-wide
/// liquidation, so on restore that one order came back unattributed and its fill landed in the
/// RESIDUAL row instead of the mount's ledger — under-reporting exactly the realized loss the budget
/// latch exists to bound. A PRE-v14 journal's latch flatten still restores unattributed (the field
/// reads back `None`), which is that journal's own recorded truth rather than a new loss.
pub(crate) fn fold_coid_mounts(records: &[JournalRecord]) -> Vec<(String, String)> {
    fold_coid_origins(records).0
}

/// [`fold_coid_mounts`] plus the arms a MOUNT still owns at the end of the fold: `arm_id ->
/// mount_id` for every `ConditionalArmed` journaled under an open `StrategySubmit` and not fired or
/// disarmed since. The boot restore hands that map to the core (`CoreConfig::conditional_owners`)
/// so a restored stop's fire books into the mount that armed it, exactly as `submit_fired` does
/// for a live arm. An entry for an arm a mass-cancel cleared is harmless: only arms that are still
/// in the restored books are looked up in it.
pub(crate) fn fold_coid_origins(
    records: &[JournalRecord],
) -> (Vec<(String, String)>, HashMap<String, String>) {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut owner: Option<&str> = None;
    // arm id -> the mount that armed it, for an arm journaled while a mount's `StrategySubmit` was
    // open: the fire of that arm is THAT mount's own order (see below).
    let mut arm_owner: HashMap<&str, &str> = HashMap::new();
    for rec in records {
        match rec {
            JournalRecord::StrategySubmit { mount_id, intent, .. } => {
                owner = Some(mount_id.as_str());
                // A client-supplied (non-empty) coid never reaches the minting path, so it has no
                // `MintedSubmit` to pair with — map it here or lose it.
                if let Some(coid) = client_supplied_coid(intent) {
                    out.push((coid, mount_id.clone()));
                }
            }
            JournalRecord::MintedSubmit { req, .. } => {
                let coid = req.client_order_id.as_str();
                if let Some(m) = owner.filter(|_| !coid.is_empty()) {
                    out.push((coid.to_string(), m.to_string()));
                }
            }
            // The per-mount budget latch's flatten NAMES its owner (v14): it OPENS ownership, so the
            // `MintedSubmit` that follows — the coid `apply_intent` mints for it — books into that
            // mount's ledger, identically to the live runtime's own `coid_mount` insert.
            JournalRecord::MarginCallLiquidate { mount_id: Some(mid), .. } => {
                owner = Some(mid.as_str());
            }
            // A conditional's FIRE reopens ownership with the mount that ARMED it, so the
            // `MintedSubmit` the release writes restores into that mount's ledger, identically to
            // the live runtime's `submit_fired` write. An arm nobody owned (an operator's, under a
            // `Cmd`; or one whose `ConditionalArmed` is no longer in view because
            // `prune_before_latest_snap` deleted the early segment holding it: this fold reads the
            // whole READABLE record set, so a pre-`Snap` arm is normally seen) has no entry
            // and the release CLOSES ownership: unattributed, as before.
            JournalRecord::ConditionalArmed { arm_id, .. } => {
                if let Some(m) = owner {
                    arm_owner.insert(arm_id.as_str(), m);
                }
            }
            JournalRecord::ConditionalDisarmed { arm_id, .. } => {
                arm_owner.remove(arm_id.as_str());
            }
            JournalRecord::ConditionalFire { arm_id, .. } => {
                owner = arm_owner.remove(arm_id.as_str());
            }
            JournalRecord::Cmd { .. }
            | JournalRecord::Snap { .. }
            // ...whereas an UNOWNED `MarginCallLiquidate` is the account-wide margin-call sweep
            // (and every pre-v14 frame, which defaults to `None`) — a release no mount originated,
            // so it CLOSES.
            | JournalRecord::MarginCallLiquidate { mount_id: None, .. } => owner = None,

            // ── EXHAUSTIVE by design: NO `_` arm (see [`JournalRecord`]'s "Adding a variant"
            // contract). These three are observations/markers that MINT NOTHING, so they neither
            // open nor close ownership — an interleaved one must leave a `StrategySubmit` ->
            // `MintedSubmit` pair intact, which is why they are ignored rather than closing:
            //   `PortfolioSnap`       — a periodic portfolio observation.
            //   `GtdExpire`           — an expiry DECISION; issues a cancel, mints no coid.
            //   `ScheduleFire`        — an on_schedule DECISION; the orders it produces arrive as
            //                           their own `StrategySubmit` records, which open ownership.
            // A new variant that DOES mint a coid must be classified into one of the arms above,
            // not added here — the compiler will demand the decision.
            JournalRecord::PortfolioSnap { .. }
            | JournalRecord::GtdExpire { .. }
            | JournalRecord::ScheduleFire { .. } => {}
        }
    }
    let owners = arm_owner.into_iter().map(|(a, m)| (a.to_string(), m.to_string())).collect();
    (out, owners)
}

/// The coid a journaled intent ALREADY carries, if any — the one case [`fold_coid_mounts`] cannot
/// pair with a following `MintedSubmit`, because a non-empty `client_order_id` skips the mint
/// entirely (and therefore writes no such record).
fn client_supplied_coid(intent: &vike_exec::OrderIntent) -> Option<String> {
    match intent {
        vike_exec::OrderIntent::Submit(req) if !req.client_order_id.is_empty() => {
            Some(req.client_order_id.clone())
        }
        vike_exec::OrderIntent::Submit(_)
        | vike_exec::OrderIntent::SubmitBatch(_)
        | vike_exec::OrderIntent::Cancel(_)
        | vike_exec::OrderIntent::CancelBatch(_)
        | vike_exec::OrderIntent::Modify { .. }
        | vike_exec::OrderIntent::Confirm(_)
        | vike_exec::OrderIntent::MassCancel { .. }
        | vike_exec::OrderIntent::Flatten { .. }
        | vike_exec::OrderIntent::MarketExit { .. }
        | vike_exec::OrderIntent::Bracket(_)
        | vike_exec::OrderIntent::ArmConditional(_)
        | vike_exec::OrderIntent::DisarmConditional { .. }
        | vike_exec::OrderIntent::Combo(_) => None,
    }
}

/// The PURE membership fold behind re-arm-on-restore (emulator PR-3): the resting conditional
/// books at the end of a record sequence, derived from the base `Snap`'s captured books plus the
/// tail's conditional records — never from re-evaluating a trigger, and never from the replay
/// core's book state (see the module doc's design point 1 for why that state is not trusted).
///
/// Fold rules, mirroring the live `apply_intent`/`submit_fired` semantics record-for-record:
/// - [`JournalRecord::ConditionalArmed`] ADDS the arm (its RESOLVED terms — a trailing arm's
///   mark-seeded extreme included). A refusal that happens BEFORE the arm id is spent writes
///   nothing; the duplicate-id backstop is the exception: it journals this record and spends the id
///   before the book refuses, and the `!books.iter().any(..)` guard below is what keeps the first arm.
///   Appended in record order, which IS live insertion (fire) order.
/// - [`JournalRecord::ConditionalFire`] / [`JournalRecord::ConditionalDisarmed`] REMOVE the arm
///   they name (both are written only when the live book actually changed).
/// - A `Cmd`/`StrategySubmit`-carried [`vike_exec::OrderIntent::MassCancel`] clears its scope
///   exactly as the live arm does (all books / one venue / one (venue, symbol); a symbol without
///   a venue is ignored, the live no-op), and [`vike_exec::OrderIntent::MarketExit`] clears its
///   venue scope (its lowering's mass-cancel leg is venue-scoped, symbol-`None`).
/// - The `Cmd`-carried `ArmConditional`/`DisarmConditional` intents themselves are IGNORED: the
///   `ConditionalArmed`/`ConditionalDisarmed` records above are their authoritative outcomes
///   (an intent can be refused; the record is only written for an applied mutation).
///
/// Returns the folded books plus the count of `ConditionalArmed` records seen (the arm-counter
/// bound `replay_from` patches with).
pub(crate) fn fold_conditionals(
    mut books: Vec<SnapConditional>,
    tail: &[JournalRecord],
) -> (Vec<SnapConditional>, u64) {
    let mut armed_count = 0u64;
    for rec in tail {
        match rec {
            JournalRecord::ConditionalArmed { arm_id, resolved, .. } => {
                armed_count += 1;
                if !books.iter().any(|c| c.arm_id == *arm_id) {
                    // A crash-tail arm's record carries no account: `route_key` stays `None`.
                    books.push(SnapConditional {
                        arm_id: arm_id.clone(),
                        terms: resolved.clone(),
                        route_key: None,
                    });
                }
            }
            JournalRecord::ConditionalFire { arm_id, .. }
            | JournalRecord::ConditionalDisarmed { arm_id, .. } => {
                books.retain(|c| c.arm_id != *arm_id);
            }
            JournalRecord::Cmd {
                msg: Ingest::Command(vike_exec::Command::Order(intent)), ..
            } => fold_intent_scope(&mut books, intent),
            JournalRecord::StrategySubmit { intent, .. } => fold_intent_scope(&mut books, intent),

            // ── EXHAUSTIVE by design: NO `_` arm (see [`JournalRecord`]'s "Adding a variant"
            // contract). None of these can change book MEMBERSHIP:
            //   `Cmd` carrying anything but an order intent — a venue `Event`, a `Watchdog`, or a
            //     non-`Order` command. (The refined `Cmd` arm above catches the order-intent case;
            //     this one is its complement, and is what keeps the match exhaustive over `Cmd`.)
            //   `Snap`                — the fold's own STARTING books; never re-applied here.
            //   `MintedSubmit`        — a resolved order request; arms are not orders.
            //   `PortfolioSnap`       — a portfolio observation.
            //   `MarginCallLiquidate` — releases a reduce-only MARKET, arms/disarms nothing.
            //   `GtdExpire`           — expires a RESTING ORDER, not an arm (a mass-cancel-cleared
            //                           arm folds through its own intent, above).
            //   `ScheduleFire`        — an on_schedule DECISION; any arm it produced wrote its own
            //                           `ConditionalArmed`.
            JournalRecord::Cmd { .. }
            | JournalRecord::Snap { .. }
            | JournalRecord::MintedSubmit { .. }
            | JournalRecord::PortfolioSnap { .. }
            | JournalRecord::MarginCallLiquidate { .. }
            | JournalRecord::GtdExpire { .. }
            | JournalRecord::ScheduleFire { .. } => {}
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
/// the same missing fact `CoreConfig::conditionals`' residual names. It is reachable only by a
/// frame that passed the node's edge while it could not check (before the core's first publish)
/// or by a caller that bypasses the edge; `vike_tradehub::server::refusal::account_refusal` refuses an
/// unheld account before the Ack otherwise, so no write-ahead record of it is ever made.
fn fold_intent_scope(books: &mut Vec<SnapConditional>, intent: &vike_exec::OrderIntent) {
    let clear = |books: &mut Vec<SnapConditional>, venue: Option<&str>, symbol: Option<&str>| {
        match (venue, symbol) {
            (None, None) => books.clear(),
            (Some(v), None) => books.retain(|c| c.terms.venue != v),
            (Some(v), Some(s)) => books.retain(|c| !(c.terms.venue == v && c.terms.symbol == s)),
            (None, Some(_)) => {} // the live arm ignores symbol-without-venue
        }
    };
    match intent {
        // An account with no venue: refused live, so no clear.
        vike_exec::OrderIntent::MassCancel { venue: None, account: Some(_), .. }
        | vike_exec::OrderIntent::MarketExit { venue: None, account: Some(_) } => {}
        vike_exec::OrderIntent::MassCancel { venue, symbol, .. } => {
            clear(books, venue.as_deref(), symbol.as_deref());
        }
        vike_exec::OrderIntent::MarketExit { venue, .. } => clear(books, venue.as_deref(), None),
        vike_exec::OrderIntent::Submit(_)
        | vike_exec::OrderIntent::SubmitBatch(_)
        | vike_exec::OrderIntent::Cancel(_)
        | vike_exec::OrderIntent::CancelBatch(_)
        | vike_exec::OrderIntent::Modify { .. }
        | vike_exec::OrderIntent::Confirm(_)
        | vike_exec::OrderIntent::Flatten { .. }
        | vike_exec::OrderIntent::Bracket(_)
        | vike_exec::OrderIntent::ArmConditional(_)
        | vike_exec::OrderIntent::DisarmConditional { .. }
        | vike_exec::OrderIntent::Combo(_) => {}
    }
}

/// What the crash TAIL after a base `Snap` says about orders, for the boot restore's SNAP-ONLY
/// reading (`crate::replay::restore_for_boot` when the refold cannot run): the orders it MINTED and
/// the coids a journaled venue event ENDED.
pub(crate) struct TailFacts {
    /// Every `MintedSubmit` of the tail, in order: the resolved request and the route key it was
    /// lowered onto (`None` for the venue's sole/default account, and on a pre-v16 frame).
    pub(crate) minted: Vec<(OrderRequest, Option<String>)>,
    /// Every coid a journaled `OrderFilled`/`OrderCanceled`/`OrderRejected`/`OrderExpired`/
    /// `OrderDenied` named — the set `vike_exec`'s fold calls terminal (`is_terminal_event`).
    pub(crate) terminal: HashSet<String>,
}

impl TailFacts {
    /// Whether the tail ended `coid`.
    pub(crate) fn ended(&self, coid: &str) -> bool {
        self.terminal.contains(coid)
    }
}

/// [`TailFacts`] of `tail`. Two single-variant LOOKUPS, so the catch-all is the
/// "Adding a variant" contract's item 3 on `JournalRecord` (a new variant that mints or ends an
/// order still does it through a `MintedSubmit` or a journaled venue event).
pub(crate) fn tail_facts(tail: &[JournalRecord]) -> TailFacts {
    let mut facts = TailFacts { minted: Vec::new(), terminal: HashSet::new() };
    for rec in tail {
        match rec {
            JournalRecord::MintedSubmit { req, route_key, .. } => {
                facts.minted.push((req.clone(), route_key.clone()));
            }
            // `venue_event`, not an `Ingest::Event` pattern: a terminal that came over a gateway's trade
            // link is journaled as an `Ingest::GatewayEvent` (v18) and ended its order all the same.
            // Read as `Event` only, a closed order would restore as open until the first pass.
            JournalRecord::Cmd { msg, .. } => {
                if let Some(coid) = msg.venue_event().and_then(terminal_coid) {
                    facts.terminal.insert(coid.to_string());
                }
            }
            _ => {}
        }
    }
    facts
}

/// The gateway ring cursors a REFOLDED boot restore hands back (journal v18): per route key, where its
/// account's ring stood at the END of the readable journal. `base` is the latest `Snap`'s
/// `gateway_cursors` (an account quiet since the checkpoint has no record left in `tail` and keeps
/// the checkpoint's); each `GatewayEvent` of the `tail`, in journal order, then advances its
/// account's cursor by [`GatewayCursor::after`] -- the very rule the live fold applies, so the
/// cursor a restart presents is the one the core would have held had it never stopped.
///
/// Pure over records, like the folds around it. Only the Full refold folds the tail's events into an
/// engine, so only that path may return this: a snap-only restore keeps `base` unadvanced. A single-variant LOOKUP on purpose
/// (`if let`, the item 3 of [`vike_journal::JournalRecord`]'s "Adding a variant" contract): it asks
/// "is this THE gateway event?", not "what is this?".
///
/// **Only an account whose ENGINE is in `engines` advances.** The refold builds its replay core from the
/// base `Snap`'s engines alone, so a tail event of an account the `Snap` does not know (armed after the
/// last checkpoint, then traded, then a crash) reaches no engine: a cursor past it would tell the
/// gateway the event was applied and it would never send it again. Such an account keeps the `Snap`'s
/// cursor, or has none and opens its session fresh (a reconcile then recovers what it can). An engine
/// answers to its `route_key`, the venue id for the venue's default account.
pub(crate) fn fold_gateway_cursors(
    base: &[(String, GatewayCursor)],
    engines: &[EngineSnapshot],
    tail: &[JournalRecord],
) -> Vec<(String, GatewayCursor)> {
    let has_engine = |key: &str| {
        engines.iter().any(|e| e.route_key.as_deref().unwrap_or(e.venue.as_str()) == key)
    };
    let mut held: Vec<(String, GatewayCursor)> = base.to_vec();
    for rec in tail {
        if let JournalRecord::Cmd { msg: Ingest::GatewayEvent(g), .. } = rec
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

/// The coid a TERMINAL lifecycle event names; `None` for every other event.
fn terminal_coid(e: &Event) -> Option<&str> {
    match e {
        Event::OrderFilled(x) => Some(&x.client_order_id),
        Event::OrderCanceled(x) => Some(&x.client_order_id),
        Event::OrderRejected(x) => Some(&x.client_order_id),
        Event::OrderExpired(x) => Some(&x.client_order_id),
        Event::OrderDenied(x) => Some(&x.client_order_id),
        _ => None,
    }
}

/// The account LABEL TEXT an engine's route key names on `venue` — the spelling
/// `crate::order_owners::OwnedOrder::account` is written in (`None` for the venue's default
/// account, and for a key that is not one of `venue`'s).
fn account_text(venue: &str, route_key: Option<&str>) -> Option<String> {
    let label = label_of_route_key(venue, route_key?)?;
    label.text().map(str::to_string)
}

/// The orders the journal says a MOUNT left RESTING, as the ownership candidates the boot restore
/// hands the core (`crate::CoreConfig::journal_orders`) — the fix to [`fold_coid_mounts`]' "every
/// coid ever minted", which is the third reason decision 0113 gave for not restoring from the
/// journal.
///
/// "Resting" is: NON-terminal in a restored engine's registry (and not `Emulated`, a local order no
/// venue holds); when the restore could not refold the tail (`tail` is `Some`), additionally every
/// order the tail MINTED, and minus every coid the tail ended. PLUS every HELD exit of
/// `contingencies` (an OTO child kept off the venue until its parent fills): no venue reports it,
/// but its fill after a release must still find its mount. An order the gate refused before it
/// reached a registry is not one. Only a coid some mount minted ([`fold_coid_mounts`]' `owners`)
/// becomes a candidate — the restore puts orders back under their MOUNT, and an operator's order
/// has none. Each carries what the journal knows: the venue and symbol, the account label of the
/// engine it is on (a held exit takes its parent's), its limit price (decision 0120's verdict
/// 8: the price an adopted order is put back with) and, because a venue report carries none of
/// them, its kind, `reduce_only` flag and trigger price (so an adopted stop stays one), plus the tier
/// of the engine's snapshot (`JournalOrder::tier`, in memory).
///
/// **One exception to "only a coid some mount minted":** a RESTING PROTECTIVE STOP in the registry of
/// an engine that is not a paper book (`crate::order_owners::is_resting_protective_stop`) with no
/// mount owner is an operator's, and becomes a candidate under the reserved pseudo mount
/// (`crate::order_owners::PROTECTIVE_STOP_MOUNT`), so the pass adopts it instead of leaving it an
/// `UnknownOrder`. Only the registry is read for that: a stop minted in an un-refolded tail stays as
/// it was (the final `Snap` of a clean stop has everything). This function only NAMES the candidate;
/// whether it is adopted (a `demo` book, a venue whose restore gate is open) is
/// `runtime/boot_restore.rs`'s `merge_journal_owners`.
pub(crate) fn resting_orders(
    engines: &[EngineSnapshot],
    tail: Option<&TailFacts>,
    contingencies: &[SnapContingency],
    owners: &HashMap<String, String>,
) -> Vec<JournalOrder> {
    let ended = |coid: &str| tail.is_some_and(|t| t.ended(coid));
    let mut out: Vec<JournalOrder> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // coid -> the account label text of the engine holding it, for a held exit's parent.
    let mut account_of: HashMap<&str, Option<String>> = HashMap::new();
    // `(tier of the engine it rests on, operator's stop)` besides the order itself.
    let mut push = |coid: &str,
                    req: &OrderRequest,
                    account: Option<String>,
                    origin: (Option<vike_model::AccountTier>, bool)| {
        let (tier, operator_stop) = origin;
        let owner = owners
            .get(coid)
            .map(String::as_str)
            .or(operator_stop.then_some(crate::order_owners::PROTECTIVE_STOP_MOUNT));
        if let Some(mount_id) = owner
            && seen.insert(coid.to_string())
        {
            out.push(JournalOrder {
                coid: coid.to_string(),
                mount_id: mount_id.to_string(),
                venue: Some(req.venue.clone()),
                symbol: Some(req.symbol.clone()),
                account,
                price: req.price,
                reduce_only: req.reduce_only,
                trigger_price: req.trigger_price,
                order_type: Some(req.order_type.clone()),
                tier,
            });
        }
    };
    for e in engines {
        let account = account_text(&e.venue, e.route_key.as_deref());
        // A paper book is emptied by a restart (`restored_paper_orders_gone_at_boot`): its operator
        // stops are no candidates. An engine that states no tier is an old or paper one.
        let venue_book = e.tier.is_some_and(|t| t != vike_model::AccountTier::Paper);
        for (coid, mo) in &e.registry {
            account_of.insert(coid.as_str(), account.clone());
            let gone = mo.status.is_terminal() || mo.status == vike_model::OrderStatus::Emulated;
            if gone || ended(coid) {
                continue;
            }
            let operator_stop = venue_book && crate::order_owners::is_resting_protective_stop(mo);
            push(coid, &mo.request, account.clone(), (e.tier, operator_stop));
        }
    }
    if let Some(t) = tail {
        for (req, route_key) in &t.minted {
            let account = account_text(&req.venue, route_key.as_deref());
            account_of.insert(req.client_order_id.as_str(), account.clone());
            if req.client_order_id.is_empty() || t.ended(&req.client_order_id) {
                continue;
            }
            push(&req.client_order_id, req, account, (None, false));
        }
    }
    for c in contingencies {
        let Some(req) = &c.held_request else { continue };
        if ended(&c.coid) || c.parent.as_deref().is_some_and(ended) {
            continue;
        }
        let account = c.parent.as_deref().and_then(|p| account_of.get(p).cloned()).flatten();
        push(&c.coid, req, account, (None, false));
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
pub(crate) fn fence_floor(field: &'static str, got: u64, expected: u64) -> Result<(), ReplayError> {
    if got < expected {
        return Err(ReplayError::RestoreMismatch { field, expected, got });
    }
    Ok(())
}
