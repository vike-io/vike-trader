//! `Command::SetAccountTradingState` on the test thread: ONE engine halted, reduced or resumed while
//! the others keep their state, the precedence against what the node holds (the drawdown latch's
//! `Reducing`, a node-wide word, a fault), the link dead-man's hold, and the refusals. The same
//! command through a spawned core with a journal, and its replay, is
//! `tests/journal/account_trading_state.rs`.

use super::*;
use vike_exec::testing::RecordingClient;
use vike_model::TradingState::{Active, Halted, Reducing};

use crate::runtime::test_support::{core_of, engine_on};

const ALT: &str = "other";
const THIRD: &str = "third";

/// Three accounts on three venues (`sim`, `other`, `third`; the route key of each is its venue), an
/// injected clock (`dispatch` re-stamps every message from it), and the given link dead-man config.
fn three(link_deadman: Option<LinkDeadManConfig>) -> CoreThread<RecordingClient> {
    three_with(CoreConfig { link_deadman, ..Default::default() })
}

/// [`three`] over a caller's config (its clock is replaced by the injected one).
fn three_with(mut cfg: CoreConfig) -> CoreThread<RecordingClient> {
    cfg.clock = Box::new(|| 0);
    core_of(
        engine_on("sim", "BTCUSDT", RecordingClient::default()),
        vec![
            (1.0, engine_on(ALT, "BTCUSDT", RecordingClient::default())),
            (1.0, engine_on(THIRD, "BTCUSDT", RecordingClient::default())),
        ],
        cfg,
    )
}

fn account(c: &mut CoreThread<RecordingClient>, route_key: &str, state: TradingState) {
    c.dispatch(Ingest::Command(Command::SetAccountTradingState {
        route_key: route_key.to_string(),
        state,
    }));
}

/// The three engines' states, in roster order.
fn states(c: &CoreThread<RecordingClient>) -> [TradingState; 3] {
    [c.engine.trading_state, c.extra_engines[0].1.trading_state, c.extra_engines[1].1.trading_state]
}

fn said(c: &CoreThread<RecordingClient>, needle: &str) -> bool {
    c.recent.iter().any(|l| l.contains(needle))
}

fn link_armed() -> Option<LinkDeadManConfig> {
    Some(LinkDeadManConfig {
        grace: std::time::Duration::from_millis(1000),
        action: DeadManAction::CancelAllAndHalt,
        venues: [ALT.to_string()].into_iter().collect(),
    })
}

/// **The headline.** One account is halted and the other two keep trading; resuming it leaves them be.
#[test]
fn one_engine_is_halted_while_the_others_stay_active() {
    let mut c = three(None);
    account(&mut c, ALT, Halted);
    assert_eq!(states(&c), [Active, Halted, Active]);
    account(&mut c, THIRD, Reducing);
    assert_eq!(states(&c), [Active, Halted, Reducing], "each account has its own state");
    account(&mut c, ALT, Active);
    assert_eq!(states(&c), [Active, Active, Reducing], "resuming one leaves the rest alone");
}

/// The node-wide word is exactly what it was: it reaches every engine, whatever an account was told.
#[test]
fn a_node_wide_state_still_reaches_every_engine() {
    let mut c = three(None);
    account(&mut c, ALT, Halted);
    c.dispatch(Ingest::Command(Command::SetTradingState(Reducing)));
    assert_eq!(states(&c), [Reducing, Reducing, Reducing]);
    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    assert_eq!(states(&c), [Active, Active, Active]);
}

/// **The latch is the node's.** `raise_trading_state` is the call the drawdown latch makes
/// (`sweep_drawdown_latch`): an account-scoped `Active` cannot lift it, a tighter state still lands,
/// a looser-than-account one lands at the floor, and only a node-wide `Active` clears it.
#[test]
fn an_account_scoped_active_does_not_clear_a_latch() {
    let mut c = three(None);
    c.raise_trading_state(Reducing);
    assert_eq!(states(&c), [Reducing, Reducing, Reducing]);

    account(&mut c, ALT, Active);
    assert_eq!(states(&c), [Reducing, Reducing, Reducing], "the latch holds");
    assert!(said(&c, "CLAMPED"), "and says so: {:?}", c.recent);

    account(&mut c, ALT, Halted);
    assert_eq!(states(&c), [Reducing, Halted, Reducing], "a tighter state lands");
    account(&mut c, ALT, Active);
    assert_eq!(states(&c)[1], Reducing, "back to the floor, never past it");

    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    assert_eq!(states(&c), [Active, Active, Active], "the node-wide word clears the latch");
    account(&mut c, ALT, Halted);
    account(&mut c, ALT, Active);
    assert_eq!(states(&c), [Active, Active, Active], "and the account is free again");
}

/// Every floor the same way: the account is first halted (a tighter state always lands, which is what
/// shows the command is live), then asked `Active`, and lands at the floor, never past it.
fn assert_the_floor_holds(c: &mut CoreThread<RecordingClient>, floor: TradingState, what: &str) {
    account(c, ALT, Halted);
    assert_eq!(c.extra_engines[0].1.trading_state, Halted, "{what}: a tighter state lands");
    account(c, ALT, Active);
    assert_eq!(c.extra_engines[0].1.trading_state, floor, "{what}: Active lands at the floor");
    assert_eq!(
        c.engine.trading_state, floor,
        "{what}: the other accounts are as the node left them"
    );
    assert!(said(c, "CLAMPED"), "{what}: and the ring says so: {:?}", c.recent);
}

/// A node-wide `reduce`/`halt` is a floor, the silence dead-man's halt is, and a faulted core is:
/// none of them is lifted by an account, and only the node-wide `Active` clears them.
#[test]
fn a_node_wide_word_a_dead_man_halt_and_a_fault_are_floors_too() {
    let mut c = three(None);
    c.dispatch(Ingest::Command(Command::SetTradingState(Halted)));
    assert_the_floor_holds(&mut c, Halted, "a node-wide halt");

    let mut c = three(None);
    c.dispatch(Ingest::Command(Command::SetTradingState(Reducing)));
    assert_the_floor_holds(&mut c, Reducing, "a node-wide reduce");

    let mut c = three_with(CoreConfig {
        deadman: Some(DeadManConfig {
            timeout: std::time::Duration::from_millis(1000),
            action: DeadManAction::CancelAllAndHalt,
            halt_file: None,
        }),
        ..Default::default()
    });
    c.deadman.as_mut().expect("armed").observe(0);
    c.sweep_deadman(5_000);
    assert_eq!(states(&c), [Halted, Halted, Halted], "the silence dead-man halted the node");
    assert_the_floor_holds(&mut c, Halted, "the silence dead-man");

    let mut c = three(None);
    c.enter_safe_state("test fault".to_string());
    assert_the_floor_holds(&mut c, Halted, "a faulted core");
    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    account(&mut c, ALT, Halted);
    account(&mut c, ALT, Active);
    assert_eq!(
        c.extra_engines[0].1.trading_state, Active,
        "the operator's node-wide word reopens it"
    );
}

/// A restart restores an engine `Halted` with no node rule behind it (the refused-snapshot path): the
/// account-scoped `Active` is the way out for THAT engine, and the other restored engine stays put.
#[test]
fn a_restored_halt_with_no_node_rule_behind_it_is_lifted_for_that_account_alone() {
    let mut c = three(None);
    c.engine.trading_state = Halted; // as `boot_restore` leaves them
    c.extra_engines[0].1.trading_state = Halted;
    account(&mut c, ALT, Active);
    assert_eq!(states(&c), [Halted, Active, Active]);
    assert!(!said(&c, "CLAMPED"), "nothing held it: {:?}", c.recent);
}

/// **A link-held engine is skipped.** The dead-man holds `other` `Halted`: an account-scoped `Active`
/// does not reopen it (`resume-venue` does), a tighter state is remembered for the release, and the
/// engines of other venues are untouched either way.
#[test]
fn a_link_held_engine_is_skipped() {
    let mut c = three(link_armed());
    c.link_deadman.as_mut().expect("armed").record_halt(ALT, 1, Active);
    c.extra_engines[0].1.trading_state = Halted;

    account(&mut c, ALT, Active);
    assert_eq!(c.extra_engines[0].1.trading_state, Halted, "the dead venue stays shut");
    assert_eq!(
        c.link_deadman.as_ref().expect("armed").prior_of(1),
        Some(Active),
        "prior untouched"
    );
    assert!(said(&c, "SKIPPED"), "and says so: {:?}", c.recent);

    account(&mut c, ALT, Reducing);
    assert_eq!(c.extra_engines[0].1.trading_state, Halted);
    assert_eq!(
        c.link_deadman.as_ref().expect("armed").prior_of(1),
        Some(Reducing),
        "a tighter word is remembered for the release"
    );

    account(&mut c, THIRD, Halted);
    assert_eq!(
        states(&c),
        [Active, Halted, Halted],
        "another venue's account is still addressable"
    );
}

/// A route key no engine carries is refused in the ring and changes nothing, and so is a stopped account.
#[test]
fn an_unknown_route_key_and_a_stopped_account_are_refused() {
    let mut c = three(None);
    account(&mut c, "sim#NOPE", Halted);
    assert_eq!(states(&c), [Active, Active, Active]);
    assert!(said(&c, "REFUSED") && said(&c, "sim#NOPE"), "{:?}", c.recent);

    c.retired_engines.push(2);
    account(&mut c, THIRD, Halted);
    assert_eq!(c.extra_engines[1].1.trading_state, Active, "a stopped account keeps its state");
    assert!(said(&c, "STOPPED"), "{:?}", c.recent);
}

// ───────────────────── the node's state is a NODE fact, never an engine's (review F2, F3) ─────────────────────

fn hold(c: &mut CoreThread<RecordingClient>, route_key: &str, on: bool) {
    c.dispatch(Ingest::Command(Command::HoldAccount {
        route_key: route_key.to_string(),
        hold: on,
    }));
}

/// `core` with a loss on ALT's book armed to trip the drawdown latch at 1%: the first sweep seeds the
/// high-water mark, the price drop and the second sweep trip it.
fn core_with_a_latch() -> CoreThread<RecordingClient> {
    let mut c = three_with(CoreConfig {
        seed_cash: 1000.0,
        max_drawdown: Some(0.01),
        ..Default::default()
    });
    c.extra_engines[0].1.account.positions.insert(
        (ALT.into(), "BTCUSDT".into(), vike_model::events::PositionSide::Both),
        vike_exec::PositionEntry { size: 100.0, avg_px: 100.0, ..Default::default() },
    );
    c.extra_engines[0].1.price_board.set_quote(ALT, "BTCUSDT", 100.0, 100.5, 1);
    c
}

fn trip_the_latch(c: &mut CoreThread<RecordingClient>) {
    c.sweep_drawdown_latch(0.01);
    c.extra_engines[0].1.price_board.set_quote(ALT, "BTCUSDT", 50.0, 50.5, 2);
    c.sweep_drawdown_latch(0.01);
}

/// **F2: an account-scoped state on the PRIMARY is not the node's.** `trade account-state <primary> reducing`
/// used to read as the node being latched (the guard read engine 0's own state), so a later drawdown breach
/// never called `raise_trading_state`, the floor never rose and the other accounts stayed `Active`. The guard
/// and the hot start read `node_floor`, so the latch still trips for the node, and a hot-started account
/// inherits the NODE's state and not the primary's.
#[test]
fn an_account_scoped_state_on_the_primary_does_not_switch_the_latch_off() {
    for primary_state in [Reducing, Halted] {
        let mut c = core_with_a_latch();
        c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
        account(&mut c, "sim", primary_state);
        assert_eq!(c.node_floor, Active, "{primary_state:?}: one account's word is not the node's");

        // A hot start now does not inherit the primary's account-scoped state.
        c.apply_account_change(
            AccountChange::add_engine(
                100.0,
                engine_on("fourth", "BTCUSDT", RecordingClient::default()),
            ),
            5,
        );
        assert_eq!(
            c.eng(3).trading_state,
            Active,
            "{primary_state:?}: not inherited by a new account"
        );

        trip_the_latch(&mut c);
        assert_eq!(c.node_floor, Reducing, "{primary_state:?}: the latch tripped for the NODE");
        assert_eq!(c.extra_engines[0].1.trading_state, Reducing, "{primary_state:?}");
        assert_eq!(c.extra_engines[1].1.trading_state, Reducing, "{primary_state:?}: and the rest");
        assert_eq!(c.engine.trading_state, primary_state, "the primary keeps the stricter word");
        assert_eq!(
            c.eng(3).trading_state,
            Reducing,
            "{primary_state:?}: the latch reaches the new one"
        );

        // Now the node IS latched: a hot start inherits it, an account-scoped Active cannot lift it.
        c.apply_account_change(
            AccountChange::add_engine(
                100.0,
                engine_on("fifth", "BTCUSDT", RecordingClient::default()),
            ),
            6,
        );
        assert_eq!(c.eng(4).trading_state, Reducing);
        account(&mut c, ALT, Active);
        assert_eq!(c.extra_engines[0].1.trading_state, Reducing);
    }
}

/// `resume-venue`'s fallback reads the NODE's state, not a sibling account's own `halt`/`reduce`: with no
/// overlay for the venue (a halt restored from a Snap), its `Halted` engine comes back to the node's state
/// (`Active`), where the old fallback read the strictest state any OTHER engine showed (`Reducing`).
#[test]
fn a_resume_venue_fallback_does_not_read_another_accounts_own_state() {
    let mut c = three(link_armed());
    c.engine.trading_state = Halted; // `sim`'s engine, as restored from a Snap with no overlay
    account(&mut c, THIRD, Reducing); // a sibling account's OWN word
    assert_eq!(c.node_floor, Active);
    c.dispatch(Ingest::Command(Command::ResumeVenue { venue: "sim".to_string(), force: false }));
    assert_eq!(c.engine.trading_state, Active, "the node's state, not another account's own word");
    assert_eq!(c.extra_engines[1].1.trading_state, Reducing, "the sibling keeps its own word");
}

/// **F3: the node's state is REBUILT from the restored engines.** Not engine 0's and not the maximum: the
/// minimum strictness, so a uniform halt/reduce/latch keeps its floor, and a lone halted engine does not read
/// as the node's.
#[test]
fn the_node_floor_is_rebuilt_from_the_restored_engines() {
    let build = |states: [TradingState; 3]| {
        let mk = |venue: &str, s| {
            let mut e = engine_on(venue, "BTCUSDT", RecordingClient::default());
            e.trading_state = s;
            e
        };
        three_from(mk("sim", states[0]), mk(ALT, states[1]), mk(THIRD, states[2]))
    };
    // A uniform halt or latch restores EVERY engine at that state: the floor is kept...
    assert_eq!(build([Halted, Halted, Halted]).node_floor, Halted);
    assert_eq!(build([Reducing, Reducing, Reducing]).node_floor, Reducing);
    // …a lone halted engine (a snapshot refused for its tier, an account-scoped halt) is not the node's…
    assert_eq!(build([Active, Halted, Active]).node_floor, Active);
    assert_eq!(build([Halted, Active, Active]).node_floor, Active, "even when it is the primary");
    assert_eq!(build([Reducing, Active, Active]).node_floor, Active);
    // …and the loosest of a mixed restore is the node's.
    assert_eq!(build([Halted, Reducing, Reducing]).node_floor, Reducing);

    // A restored uniform latch keeps its guard and its clamp: nothing re-latches, nothing lifts it.
    let mut c = build([Reducing, Reducing, Reducing]);
    account(&mut c, ALT, Active);
    assert_eq!(c.extra_engines[0].1.trading_state, Reducing, "the restored latch holds");
    assert!(said(&c, "CLAMPED"), "{:?}", c.recent);

    // A lone restored halt is lifted for that account, and the latch is still armed for the node.
    let mut c = build([Halted, Active, Active]);
    account(&mut c, "sim", Active);
    assert_eq!(states(&c), [Active, Active, Active]);
    assert!(!said(&c, "CLAMPED"), "{:?}", c.recent);
}

/// Three engines in the given states, assembled (the floor is rebuilt there).
fn three_from(
    primary: ExecutionEngine<RecordingClient>,
    alt: ExecutionEngine<RecordingClient>,
    third: ExecutionEngine<RecordingClient>,
) -> CoreThread<RecordingClient> {
    let cfg = CoreConfig { clock: Box::new(|| 0), ..Default::default() };
    core_of(primary, vec![(1.0, alt), (1.0, third)], cfg)
}

// ───────────────────────── the account HOLD (review F1, F4) ─────────────────────────

/// **F1: a node-wide word cannot loosen a hold.** `trade halt` then `trade resume` (or the profile's
/// `initial_trading_state`) used to leave a kept-up twin `Active`: the watcher's one `Reducing` was edge-
/// triggered and wiped by the next node-wide set. The hold is the core's floor now.
#[test]
fn a_node_wide_word_cannot_loosen_a_hold() {
    let mut c = three(None);
    hold(&mut c, ALT, true);
    assert_eq!(states(&c), [Active, Reducing, Active], "held closing-only, the others untouched");

    c.dispatch(Ingest::Command(Command::SetTradingState(Halted)));
    assert_eq!(states(&c), [Halted, Halted, Halted]);
    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    assert_eq!(
        states(&c),
        [Active, Reducing, Active],
        "⚠ the resume leaves the held twin closing-only"
    );

    // An account-scoped Active cannot open it either; only the release does.
    account(&mut c, ALT, Active);
    assert_eq!(c.extra_engines[0].1.trading_state, Reducing);
    hold(&mut c, ALT, false);
    assert_eq!(states(&c), [Active, Active, Active], "released: back to what the node said last");
}

/// A hold only RAISES, and its release lifts exactly what it set.
#[test]
fn a_hold_only_raises_and_its_release_lifts_only_what_it_set() {
    // An operator's account-scoped halt is never loosened by the hold, nor by its release.
    let mut c = three(None);
    account(&mut c, ALT, Halted);
    hold(&mut c, ALT, true);
    assert_eq!(c.extra_engines[0].1.trading_state, Halted, "the hold does not loosen a halt");
    hold(&mut c, ALT, false);
    assert_eq!(c.extra_engines[0].1.trading_state, Halted, "…and its release does not lift it");

    // A Reducing that was there before the hold stays after it.
    let mut c = three(None);
    account(&mut c, ALT, Reducing);
    hold(&mut c, ALT, true);
    hold(&mut c, ALT, false);
    assert_eq!(c.extra_engines[0].1.trading_state, Reducing, "the operator's reduce survives");

    // An operator halt placed DURING the hold survives the release.
    let mut c = three(None);
    hold(&mut c, ALT, true);
    account(&mut c, ALT, Halted);
    hold(&mut c, ALT, false);
    assert_eq!(c.extra_engines[0].1.trading_state, Halted);

    // The hold's own raise IS lifted.
    let mut c = three(None);
    hold(&mut c, ALT, true);
    hold(&mut c, ALT, false);
    assert_eq!(c.extra_engines[0].1.trading_state, Active);

    // A hold on an engine under a node-wide halt: the halt outlives the hold and a later node-wide Active
    // brings it to the hold's floor, not to Active.
    let mut c = three(None);
    c.dispatch(Ingest::Command(Command::SetTradingState(Halted)));
    hold(&mut c, ALT, true);
    hold(&mut c, ALT, false);
    assert_eq!(states(&c), [Halted, Halted, Halted], "the node-wide halt stands");
}

/// A release never lowers an engine below the NODE's state: the drawdown latch rose while it was held.
#[test]
fn a_release_never_goes_below_the_nodes_state() {
    let mut c = three(None);
    hold(&mut c, ALT, true);
    c.raise_trading_state(Reducing); // the latch
    hold(&mut c, ALT, false);
    assert_eq!(states(&c), [Reducing, Reducing, Reducing], "the latch outlives the hold");
    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    assert_eq!(states(&c), [Active, Active, Active]);
}

/// **F4: a RESTARTED account is a new engine and is never touched.** The hold is keyed to the engine: the
/// stop drops it with the engine, and a release (or a second hold) addressed to the route key meets the NEW
/// engine, which has no hold: it does nothing, and a hot-started engine's inherited state is not undone.
#[test]
fn a_restarted_account_carries_no_hold_and_a_release_does_not_touch_it() {
    let mut c = three(None);
    c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    hold(&mut c, ALT, true);
    assert_eq!(c.extra_engines[0].1.trading_state, Reducing);
    c.apply_account_change(
        AccountChange::StopEngine {
            not_running: vike_exec::NotRunningAccount {
                venue: ALT.to_string(),
                account: None,
                route_key: ALT.to_string(),
                tier: None,
                reason: "account inactive".to_string(),
            },
        },
        10,
    );
    assert!(c.account_holds.is_empty(), "the stopped engine's hold went with it");
    // The account starts again, under a node that is latched: the new engine inherits the latch.
    c.raise_trading_state(Reducing);
    c.apply_account_change(
        AccountChange::add_engine(100.0, engine_on(ALT, "BTCUSDT", RecordingClient::default())),
        20,
    );
    let fresh = c.extra_engines.len();
    assert_eq!(c.eng(fresh).trading_state, Reducing, "inherited from the node");
    hold(&mut c, ALT, false);
    assert_eq!(
        c.eng(fresh).trading_state,
        Reducing,
        "a release for an engine with no hold does nothing"
    );
    assert!(c.account_holds.is_empty());
}

/// A hold on a LINK-HELD engine raises the state it returns to, and its release lowers exactly that.
#[test]
fn a_hold_on_a_link_held_engine_raises_the_state_it_returns_to() {
    let mut c = three(link_armed());
    c.link_deadman.as_mut().expect("armed").record_halt(ALT, 1, Active);
    c.extra_engines[0].1.trading_state = Halted;
    hold(&mut c, ALT, true);
    assert_eq!(c.extra_engines[0].1.trading_state, Halted, "the dead venue stays shut");
    assert_eq!(c.link_deadman.as_ref().expect("armed").prior_of(1), Some(Reducing));
    hold(&mut c, ALT, false);
    assert_eq!(c.link_deadman.as_ref().expect("armed").prior_of(1), Some(Active));
}

// ───────────────────── what reaches the journal, and the Snap that pins it (review F6) ─────────────────────

fn journaled_three(
    dir: &std::path::Path,
    link: Option<LinkDeadManConfig>,
) -> CoreThread<RecordingClient> {
    three_with(CoreConfig {
        link_deadman: link,
        journal: Some(crate::runtime::JournalConfig::at(dir.to_path_buf())),
        ..Default::default()
    })
}

/// `(records, snaps)` the core's journal holds now.
fn journal_counts(c: &mut CoreThread<RecordingClient>, dir: &std::path::Path) -> (usize, usize) {
    c.journal.as_mut().expect("journal").flush().expect("flush");
    let records = vike_journal::CommandJournal::read_all(dir).expect("readable");
    let snaps =
        records.iter().filter(|r| matches!(r, vike_journal::JournalRecord::Snap { .. })).count();
    (records.len(), snaps)
}

/// **A command that changes nothing is not journaled** (so it never stamps the segment format 21): an unknown
/// or stopped account, a release with no hold, a hold already held, a state the engine already holds. A real
/// one is.
#[test]
fn a_command_that_changes_nothing_is_not_journaled() {
    let dir = crate::scratch::Scratch::reserved("account-state-noop-not-journaled");
    let mut c = journaled_three(&dir, None);
    let before = c.journaled_since_snap;
    account(&mut c, "sim#NOPE", Halted);
    hold(&mut c, "sim#NOPE", true);
    hold(&mut c, ALT, false); // nothing held
    account(&mut c, ALT, Active); // it is Active already
    c.retired_engines.push(2);
    account(&mut c, THIRD, Halted); // stopped
    hold(&mut c, THIRD, true);
    assert_eq!(c.journaled_since_snap, before, "six no-ops, nothing journaled");
    assert_eq!(
        vike_journal::CommandJournal::latest_segment_version(&dir).unwrap(),
        Some(18),
        "…so the segment is not stamped 21"
    );

    hold(&mut c, ALT, true);
    hold(&mut c, ALT, true); // already held
    assert_eq!(
        c.journaled_since_snap,
        before + 1,
        "the first hold is journaled, the repeat is not"
    );
    account(&mut c, "sim", Halted);
    assert_eq!(c.journaled_since_snap, before + 2);
    c.journal.as_mut().expect("journal").flush().expect("flush");
    assert_eq!(vike_journal::CommandJournal::latest_segment_version(&dir).unwrap(), Some(21));
}

/// **A clamp writes a `Snap`** (a replay core has not got the latch behind the floor and would fold the
/// unclamped word), and a plain set does not.
#[test]
fn a_clamp_writes_a_snap_and_a_plain_set_does_not() {
    let dir = crate::scratch::Scratch::reserved("account-state-clamp-writes-a-snap");
    let mut c = journaled_three(&dir, None);
    let (_, s0) = journal_counts(&mut c, &dir);
    account(&mut c, ALT, Halted);
    let (_, s1) = journal_counts(&mut c, &dir);
    assert_eq!(s1, s0, "a plain account-scoped set writes no Snap");

    c.raise_trading_state(Reducing); // the latch
    account(&mut c, ALT, Active); // Halted -> Reducing, clamped
    assert!(said(&c, "CLAMPED"), "{:?}", c.recent);
    let (_, s2) = journal_counts(&mut c, &dir);
    assert_eq!(s2, s1 + 1, "the clamp is pinned by a Snap holding the engines as they ARE");
}

/// **A skip writes a `Snap`**: the link overlay is the live core's alone, and a replay core would apply the
/// state to the engine this core left halted. A hold on a link-held engine too.
#[test]
fn a_skip_and_a_hold_on_a_link_held_engine_write_a_snap() {
    let dir = crate::scratch::Scratch::reserved("account-state-skip-writes-a-snap");
    let mut c = journaled_three(&dir, link_armed());
    c.link_deadman.as_mut().expect("armed").record_halt(ALT, 1, Active);
    c.extra_engines[0].1.trading_state = Halted;
    let (_, s0) = journal_counts(&mut c, &dir);
    account(&mut c, ALT, Reducing); // tighter than the state the venue returns to: remembered
    assert!(said(&c, "SKIPPED") && said(&c, "remembered"), "{:?}", c.recent);
    let (_, s1) = journal_counts(&mut c, &dir);
    assert_eq!(s1, s0 + 1, "the SKIPPED branch writes a Snap when it remembered something");
    hold(&mut c, ALT, true);
    let (_, s2) = journal_counts(&mut c, &dir);
    assert_eq!(s2, s1 + 1, "…and so does a hold on the link-held engine");
}

/// **The panic branch of `publish_guarded` raises the floor**: a faulted core is not reopened by an
/// account-scoped `Active`, only by a node-wide one.
#[test]
fn a_publish_panic_raises_the_floor() {
    let mut c = three_with(CoreConfig {
        repaint: Some(Box::new(|| panic!("the repaint hook panics (test)"))),
        ..Default::default()
    });
    c.publish_guarded();
    assert!(c.fault.is_some(), "PREMISE: the panic branch ran");
    assert_eq!(c.node_floor, Halted);
    account(&mut c, ALT, Active);
    assert_eq!(c.extra_engines[0].1.trading_state, Halted, "clamped to the faulted core's floor");
    assert!(said(&c, "CLAMPED"), "{:?}", c.recent);
}

// ───────────────────── what DROPS an account's own word, said (re-review P1-P5) ─────────────────────

/// The ring lines that carry `needle`, for a message that must name something.
fn lines_with(c: &CoreThread<RecordingClient>, needle: &str) -> Vec<String> {
    c.recent.iter().filter(|l| l.contains(needle)).map(|l| l.to_string()).collect()
}

/// **P1: a node-wide word NAMES the accounts it reopens.** The node-wide `resume` replaces an account's own
/// halt (that is the verb), but the WARN and the ring note say which route keys it reopened, with the state each
/// held. What a node-wide rule itself set (a node-wide halt, the latch) is the node's and is not named.
#[test]
fn a_node_wide_word_names_the_accounts_it_reopens() {
    let mut c = three(None);
    account(&mut c, ALT, Halted);
    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    assert_eq!(states(&c), [Active, Active, Active], "the node-wide word still reopens it");
    let said_it = lines_with(&c, "REOPENED");
    assert_eq!(said_it.len(), 1, "{:?}", c.recent);
    assert!(said_it[0].contains("other (Halted)"), "{}", said_it[0]);
    assert!(!said_it[0].contains("third") && !said_it[0].contains("sim"), "only that account");

    // A node-wide `reduce` over an account's own `halt` loosens it to Reducing: named too.
    let mut c = three(None);
    account(&mut c, THIRD, Halted);
    c.dispatch(Ingest::Command(Command::SetTradingState(Reducing)));
    assert!(said(&c, "REOPENED") && said(&c, "third (Halted)"), "{:?}", c.recent);

    // A node-wide halt and the latch are the NODE's: resuming them reopens no ACCOUNT's own word.
    let mut c = three(None);
    c.dispatch(Ingest::Command(Command::SetTradingState(Halted)));
    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    assert!(!said(&c, "REOPENED"), "{:?}", c.recent);
    let mut c = three(None);
    c.raise_trading_state(Reducing);
    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    assert!(!said(&c, "REOPENED"), "{:?}", c.recent);

    // …but an account halted on its own UNDER a latch is named, and only it.
    let mut c = three(None);
    c.raise_trading_state(Reducing);
    account(&mut c, ALT, Halted);
    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    let said_it = lines_with(&c, "REOPENED");
    assert_eq!(said_it.len(), 1, "{:?}", c.recent);
    assert!(
        said_it[0].contains("other (Halted)") && !said_it[0].contains("third"),
        "{}",
        said_it[0]
    );
}

/// **P2 (no behaviour change, said):** `resume-venue` with no overlay lifts EVERY halted engine of the venue, an
/// account halted on its own included, and the note NAMES them.
#[test]
fn a_resume_venue_without_an_overlay_names_the_halted_accounts_it_lifts() {
    let mut c = three(link_armed());
    account(&mut c, ALT, Halted); // an operator's own halt of the account
    c.dispatch(Ingest::Command(Command::ResumeVenue { venue: ALT.to_string(), force: false }));
    assert_eq!(
        c.extra_engines[0].1.trading_state, Active,
        "the fallback lifts it (unchanged behaviour)"
    );
    let note = lines_with(&c, "RESUMED");
    assert_eq!(note.len(), 1, "{:?}", c.recent);
    assert!(note[0].contains("no recorded prior") && note[0].contains("other"), "{}", note[0]);
    assert!(note[0].contains("EVERY halted engine"), "{}", note[0]);
}

/// **P2 (no behaviour change, said):** stopping an account and starting it again gives a NEW engine at the
/// node's state: an account-scoped halt does not survive it.
#[test]
fn stopping_an_account_forgets_its_own_halt() {
    let mut c = three(None);
    c.engine_door = Some(crate::runtime::account_change::engine_of_parcel::<RecordingClient>);
    account(&mut c, ALT, Halted);
    c.apply_account_change(
        AccountChange::StopEngine {
            not_running: vike_exec::NotRunningAccount {
                venue: ALT.to_string(),
                account: None,
                route_key: ALT.to_string(),
                tier: None,
                reason: "account inactive".to_string(),
            },
        },
        10,
    );
    c.apply_account_change(
        AccountChange::add_engine(100.0, engine_on(ALT, "BTCUSDT", RecordingClient::default())),
        20,
    );
    let fresh = c.extra_engines.len();
    assert_eq!(c.eng(fresh).trading_state, Active, "the new engine starts at the node's state");
}

/// **P3: an operator's `Reducing` on a link-held, HELD engine survives the release.** The link branch used to
/// leave the hold's remembered state alone, so the release lowered the prior the venue returns to from the hold's
/// `Reducing` to `Active`, and the operator's word was lost.
#[test]
fn an_operators_word_on_a_link_held_held_engine_survives_the_release() {
    let mut c = three(link_armed());
    c.link_deadman.as_mut().expect("armed").record_halt(ALT, 1, Active);
    c.extra_engines[0].1.trading_state = Halted;
    hold(&mut c, ALT, true); // the venue returns to Reducing at least
    account(&mut c, ALT, Reducing); // the operator's word, equal to what the hold put there
    assert!(said(&c, "remembered"), "not dropped: {:?}", c.recent);
    hold(&mut c, ALT, false);
    assert_eq!(
        c.link_deadman.as_ref().expect("armed").prior_of(1),
        Some(Reducing),
        "the operator's Reducing outlives the hold"
    );
}

/// **P4: a node-wide word with a hold in force writes a `Snap`** (a replay core has no hold: it would fold the
/// held engine looser than live), and without a hold it does not.
#[test]
fn a_node_wide_word_with_a_hold_in_force_writes_a_snap() {
    let dir = crate::scratch::Scratch::reserved("account-state-node-wide-with-a-hold");
    let mut c = journaled_three(&dir, None);
    let (_, s0) = journal_counts(&mut c, &dir);
    c.dispatch(Ingest::Command(Command::SetTradingState(Halted)));
    let (_, s1) = journal_counts(&mut c, &dir);
    assert_eq!(s1, s0, "no hold, no link hold: no Snap, as before");

    hold(&mut c, ALT, true);
    let (_, s2) = journal_counts(&mut c, &dir);
    c.dispatch(Ingest::Command(Command::SetTradingState(Active)));
    assert_eq!(c.extra_engines[0].1.trading_state, Reducing, "PREMISE: the hold survived the word");
    let (_, s3) = journal_counts(&mut c, &dir);
    assert_eq!(s3, s2 + 1, "a node-wide word with a hold in force writes a Snap");
}

/// **P5: a word dropped for a link-held engine changes nothing, so it is not journaled and stamps nothing**; a
/// tighter one is remembered, and then it is (with the `Snap` that pins it).
#[test]
fn a_dropped_word_for_a_link_held_engine_is_not_journaled() {
    let dir = crate::scratch::Scratch::reserved("account-state-link-held-dropped");
    let mut c = journaled_three(&dir, link_armed());
    c.link_deadman.as_mut().expect("armed").record_halt(ALT, 1, Active);
    c.extra_engines[0].1.trading_state = Halted;
    let (r0, s0) = journal_counts(&mut c, &dir);

    account(&mut c, ALT, Active); // looser than the prior: dropped
    assert!(said(&c, "dropped"), "{:?}", c.recent);
    assert_eq!(journal_counts(&mut c, &dir), (r0, s0), "nothing journaled, no Snap");
    assert_eq!(
        vike_journal::CommandJournal::latest_segment_version(&dir).unwrap(),
        Some(18),
        "…and the segment is not stamped 21"
    );

    account(&mut c, ALT, Reducing); // tighter: remembered
    let (r1, s1) = journal_counts(&mut c, &dir);
    assert_eq!((r1, s1), (r0 + 2, s0 + 1), "the record and the Snap that pins it");
    assert_eq!(vike_journal::CommandJournal::latest_segment_version(&dir).unwrap(), Some(21));
}
