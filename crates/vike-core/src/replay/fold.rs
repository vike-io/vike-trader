//! The PURE functions a restore and its fence are computed from: the record folds, the id floor.

use vike_exec::Ingest;
use vike_journal::SnapConditional;

use super::{JournalRecord, ReplayError};

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
    let mut out: Vec<(String, String)> = Vec::new();
    let mut owner: Option<&str> = None;
    // arm id -> the mount that armed it, for an arm journaled while a mount's `StrategySubmit` was
    // open: the fire of that arm is THAT mount's own order (see below).
    let mut arm_owner: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
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
    out
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
                    books.push(SnapConditional { arm_id: arm_id.clone(), terms: resolved.clone() });
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
