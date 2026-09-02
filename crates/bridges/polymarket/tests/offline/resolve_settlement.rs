//! End-to-end proof that a `resolve` settlement fill actually FLATTENS a real local position and
//! books the right realized PnL — driven through a genuine `vike_exec::ExecutionEngine`, not a
//! collecting `Vec<Event>` sink.
//!
//! `resolve.rs`'s own unit tests assert what the module EMITS (one bare `Event::Fill`, right payout,
//! right qty/side, at-most-once). They deliberately stop at the lane boundary, so on their own they
//! prove nothing about the module's central bet: that a bare `Event::Fill` — carrying no registered
//! `client_order_id`, because a settlement has no order behind it — is folded by the engine into
//! position + `closed_pnls` rather than dropped as an unknown-coid lifecycle event. That claim is
//! what makes the whole feature work, and it is a claim about vike-exec, not about this crate. So it
//! gets executed here instead of reasoned about: build the real engine, open a real position, run
//! the real `settle_once`, and assert the resulting account state.
//!
//! Each test folds an ENTRY fill the way the live user-data pump would (`user_ws::fill_pair`'s bare
//! `Event::Fill`), then settles it, then asserts `position == 0` and the exact realized PnL.
//!
//! **Wiring note this test pins:** `ExecutionEngine::accepts_symbol` symbol-filters every fill, so a
//! settlement lands only on an engine whose `symbol` (or `extra_symbols`) is that outcome
//! `token_id`. A future app-root mount that runs one engine per market must therefore register each
//! watched token, or its settlements will be silently filtered out — asserted directly in
//! `settlement_for_an_unwatched_symbol_is_filtered_out`.

use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate, RiskLimits,
};
use vike_model::events::{Event, FillEvent, TradeId};
use vike_polymarket::{
    settle_once, PayoutSource, Position, ResolveDeps, ResolveWatchlist, SettlementLedger,
    WINNER_PAYOUT,
};

const VENUE: &str = "polymarket";

/// Scripted discovery: one canned `/positions` snapshot, no network.
///
/// These tests are about the ENGINE fold — that a bare `Event::Fill` flattens a real position — not
/// about where its price came from, so they name the network-free
/// [`PayoutSource::RedeemableFlag`] source and hand-pick snapshots whose flag-derived payout is the
/// one under test. Payout DERIVATION (chain-priced, fail-closed) is proven exhaustively in
/// `resolve.rs`'s own unit tests, which is where the production `PayoutSource::Chain` fold lives.
struct StubDeps(Vec<Position>);
impl ResolveDeps for StubDeps {
    fn list_positions(&self, _proxy: &str) -> Result<Vec<Position>, String> {
        Ok(self.0.clone())
    }
    fn payout_source(&self) -> PayoutSource {
        PayoutSource::RedeemableFlag
    }
}

fn pos(cid: &str, asset: &str, size: f64, redeemable: bool) -> Position {
    Position {
        condition_id: cid.into(),
        asset: asset.into(),
        size,
        redeemable,
        neg_risk: false,
        outcome_index: Some(0),
        title: "t".into(),
        cur_price: None,
    }
}

/// A real engine for one outcome token, exactly as a per-market mount would build it.
fn engine(symbol: &str) -> ExecutionEngine<RecordingClient> {
    let account = Account::new(1.0, VENUE, None, BalanceMode::Delta);
    let gate = RiskGate::new(RiskLimits::default());
    ExecutionEngine::new(account, gate, RecordingClient::default(), VENUE, symbol)
}

/// The bare entry fill the live user-data pump emits for a trade (`user_ws::fill_pair`).
fn entry_fill(symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        // A test fixture's synthetic id — minted, so the infallible `prefixed` constructor applies.
        trade_id: TradeId::prefixed("entry:", symbol),
        client_order_id: format!("coid:{symbol}"),
        venue: VENUE.to_string().into(),
        symbol: symbol.to_string().into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        ts: 1,
        mark_price: None,
        position_side: "BOTH".to_string().into(),
    }
}

/// Run one `settle_once` pass, piping every emitted event straight into the engine.
fn settle_into(
    eng: &mut ExecutionEngine<RecordingClient>,
    deps: &StubDeps,
    watchlist: &mut ResolveWatchlist,
    ledger: &SettlementLedger,
) -> usize {
    let mut outbox = Outbox::default();
    let mut emitted = 0usize;
    let mut emit = |e: Event| {
        emitted += 1;
        eng.on_event(&e, &mut outbox);
        true
    };
    settle_once(deps, "0xproxy", watchlist, ledger, &mut emit);
    emitted
}

/// A WINNING leg: bought 100 @ 0.40, resolves at 1.0 → position flat, realized = (1.0 - 0.40) * 100.
#[test]
fn settlement_flattens_a_winning_position_and_books_the_gain() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("settled.txt"));
    let mut eng = engine("tokWin");
    let mut outbox = Outbox::default();

    // Open the position the way the live fill path does.
    let entry_px = 0.40;
    let qty = 100.0;
    eng.on_event(&Event::Fill(entry_fill("tokWin", 1, qty, entry_px)), &mut outbox);
    assert_eq!(eng.position_size("BOTH"), qty, "entry fill opened the long");
    assert!(eng.account.closed_pnls.is_empty(), "nothing realized yet");

    // The market resolves in our favour and is still held (pre-redeem).
    let deps = StubDeps(vec![pos("0xA", "tokWin", qty, true)]);
    let mut wl = ResolveWatchlist::new();
    let emitted = settle_into(&mut eng, &deps, &mut wl, &ledger);

    assert_eq!(emitted, 1, "exactly one settlement event");
    assert_eq!(eng.position_size("BOTH"), 0.0, "settlement FLATTENED the local position");
    let expected = (WINNER_PAYOUT - entry_px) * qty;
    assert_eq!(
        eng.account.realized_pnl.to_bits(),
        expected.to_bits(),
        "realized the full winning payout spread"
    );
    assert_eq!(eng.account.closed_pnls.len(), 1, "exactly one closed-PnL entry booked");
}

/// A LOSING leg: bought 40 @ 0.25, resolves at 0.0 → position flat, realized = -0.25 * 40.
/// This is the case nothing in the system could ever retire before this module: a loser is never
/// redeemed, so without a settlement it would linger forever.
#[test]
fn settlement_flattens_a_losing_position_and_books_the_full_loss() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("settled.txt"));
    let mut eng = engine("tokLose");
    let mut outbox = Outbox::default();

    let entry_px = 0.25;
    let qty = 40.0;
    eng.on_event(&Event::Fill(entry_fill("tokLose", 1, qty, entry_px)), &mut outbox);
    assert_eq!(eng.position_size("BOTH"), qty);

    // The sibling winning leg names the winner; our leg is the loser.
    let deps = StubDeps(vec![pos("0xA", "tokWin", 10.0, true), pos("0xA", "tokLose", qty, false)]);
    let mut wl = ResolveWatchlist::new();
    settle_into(&mut eng, &deps, &mut wl, &ledger);

    assert_eq!(eng.position_size("BOTH"), 0.0, "the losing position is retired, not stranded");
    let expected = (0.0 - entry_px) * qty;
    assert_eq!(
        eng.account.realized_pnl.to_bits(),
        expected.to_bits(),
        "realized the full cost basis as a loss"
    );
    assert_eq!(eng.account.closed_pnls.len(), 1);
}

/// A local SHORT is closed by the settlement's BUY side — the fill is always the position's inverse.
#[test]
fn settlement_closes_a_short_position() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("settled.txt"));
    let mut eng = engine("tokWin");
    let mut outbox = Outbox::default();

    // Sold 50 @ 0.80; it resolves YES (1.0) → a loss of (0.80 - 1.0) * 50 on the short.
    let entry_px = 0.80;
    let qty = 50.0;
    eng.on_event(&Event::Fill(entry_fill("tokWin", -1, qty, entry_px)), &mut outbox);
    assert_eq!(eng.position_size("BOTH"), -qty, "entry fill opened the short");

    // The local book is short, so the local-position hook supplies the signed size.
    struct ShortDeps(Vec<Position>);
    impl ResolveDeps for ShortDeps {
        fn list_positions(&self, _proxy: &str) -> Result<Vec<Position>, String> {
            Ok(self.0.clone())
        }
        fn payout_source(&self) -> PayoutSource {
            PayoutSource::RedeemableFlag
        }
        fn local_position(&self, _token_id: &str) -> Option<f64> {
            Some(-50.0)
        }
    }
    let deps = ShortDeps(vec![pos("0xA", "tokWin", qty, true)]);
    let mut wl = ResolveWatchlist::new();
    let mut emit = |e: Event| {
        eng.on_event(&e, &mut outbox);
        true
    };
    settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut emit);

    assert_eq!(eng.position_size("BOTH"), 0.0, "the short was bought back flat");
    let expected = (WINNER_PAYOUT - entry_px) * -qty;
    assert_eq!(eng.account.realized_pnl.to_bits(), expected.to_bits(), "short loses on a winner");
}

/// The engine's own `seen_trade_ids` dedup is a SECOND, independent guard on top of the ledger:
/// even a settlement fill replayed inside one process cannot double-book. Proves the
/// `resolution:<cid>:<token>` trade_id is doing real work.
#[test]
fn engine_dedups_a_replayed_settlement_fill() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("settled.txt"));
    let mut eng = engine("tokWin");
    let mut outbox = Outbox::default();

    let qty = 100.0;
    eng.on_event(&Event::Fill(entry_fill("tokWin", 1, qty, 0.40)), &mut outbox);

    // Capture the settlement fill, then feed it to the engine TWICE.
    let deps = StubDeps(vec![pos("0xA", "tokWin", qty, true)]);
    let mut wl = ResolveWatchlist::new();
    let mut captured: Vec<Event> = Vec::new();
    settle_once(&deps, "0xproxy", &mut wl, &ledger, &mut |e| {
        captured.push(e);
        true
    });
    assert_eq!(captured.len(), 1);

    eng.on_event(&captured[0], &mut outbox);
    assert_eq!(eng.position_size("BOTH"), 0.0);
    assert_eq!(eng.account.closed_pnls.len(), 1);

    eng.on_event(&captured[0], &mut outbox); // replay
    assert_eq!(eng.position_size("BOTH"), 0.0, "replay must not flip the position negative");
    assert_eq!(eng.account.closed_pnls.len(), 1, "replay deduped on trade_id — no double-book");
}

/// A settlement whose symbol this engine does not own is filtered out by `accepts_symbol`, leaving
/// the account untouched. This pins the mount requirement documented at the top of this file.
#[test]
fn settlement_for_an_unwatched_symbol_is_filtered_out() {
    let mut eng = engine("tokMine");
    let mut outbox = Outbox::default();
    eng.on_event(&Event::Fill(entry_fill("tokMine", 1, 10.0, 0.5)), &mut outbox);
    assert_eq!(eng.position_size("BOTH"), 10.0);

    // A settlement for a DIFFERENT token: same venue, not this engine's symbol.
    let dir = tempfile::tempdir().unwrap();
    let ledger = SettlementLedger::open(dir.path().join("settled.txt"));
    let deps = StubDeps(vec![pos("0xOther", "tokOther", 5.0, true)]);
    let mut wl = ResolveWatchlist::new();
    let emitted = settle_into(&mut eng, &deps, &mut wl, &ledger);

    assert_eq!(emitted, 1, "the module still emitted it");
    assert_eq!(eng.position_size("BOTH"), 10.0, "this engine's position is untouched");
    assert!(eng.account.closed_pnls.is_empty(), "no PnL booked from another symbol's settlement");
}
