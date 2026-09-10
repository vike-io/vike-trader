//! **`policy.toml`'s `max_sizing_equity` — the ceiling on the equity FIGURE, driven through the
//! REAL runtime, in BOTH directions.**
//!
//! The axis exists because on a live venue `Account::balance` is the venue's attested wallet for
//! the WHOLE account the credentials open, every reconcile pass adopts it, and under
//! `vike_exec::BalanceMode::Authoritative` resolved equity is `balance + unrealized` — so a third
//! party's deposit or withdrawal on a shared account moves what this daemon sizes and admits
//! against, with nothing this process did. `vike_config::Policy::max_sizing_equity` bounds the
//! figure the SPENDING lanes see; `docs/decisions/0048-the-equity-a-strategy-sizes-against-is-capped-not-disputed.md`
//! is the verdict and carries why disputing the venue's number was tried and abandoned instead.
//!
//! ⚠ **The direction this file exists for is the SECOND one.** A ceiling that reaches the
//! margin-CALL sweep is not a conservative setting, it is a liquidation trigger: that sweep reads
//! equity as the collateral behind open positions, so a capped figure makes a healthy account look
//! under-margined and it starts closing. `crates/vike-config/tests/policy_is_consumed.rs` can prove
//! the key is READ — a text needle in the mount — and cannot prove WHERE it lands. These tests can,
//! because they run the sweep.
//!
//! Kill proofs, all measured by hand before the test they name was committed:
//! * point every `equity:` field in `crates/vike-core/src/runtime/strategy_drive.rs`'s
//!   `sizing_equity` calls back at `resolved_equity` ⇒
//!   `a_capped_engine_sizes_against_the_ceiling_not_the_wallet` fails (500 units instead of 100);
//! * point `crates/vike-core/src/runtime/watchdog.rs`'s `sweep_margin_call_engine` at
//!   `sizing_equity` ⇒ `the_ceiling_never_reaches_the_margin_call_sweep` fails (a healthy account
//!   is liquidated);
//! * drop the `cap_sizing_equity(...)` wrapper from `AppliedFill::equity_after` in
//!   `crates/vike-exec/src/execution_engine/mod.rs`'s `fold` — i.e. restore the pre-#1677 line ⇒
//!   `the_per_fill_equity_a_strategy_sizes_from_is_capped` fails (100_000 instead of 20_000).
//!
//! ⚠ **That third one is why this file grew a fill lane.** `AppliedFill::equity_after` is the
//! `ctx.equity` a `Strategy::on_fill` handler sizes its next order from, it is computed from
//! `Account::equity_all` rather than from `resolved_equity`, and three consecutive review rounds on
//! the abandoned #1671 each missed it for exactly that reason — a grep for the resolver does not
//! find it. It shipped capped in #1677 with nothing pinning that it stays capped, which is the same
//! shape of hole one review layer down. The two tests below close it end-to-end: the ceiling is
//! armed on `RiskLimits`, the fill is folded by the real engine, and the figure is read back from
//! inside a real strategy's `on_fill` rather than off the struct.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use vike_core::{CoreConfig, LiveBroker, StrategyMount, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, BarUpdate, ExecutionEngine, MarginCallConfig, RiskGate, RiskLimits,
};
use vike_model::events::{Event, FillEvent};
use vike_model::{Bar, Broker, Fill, Strategy};

/// The venue wallet these tests are about: a figure this daemon did not choose and cannot control,
/// which on a shared account a third party can move at any moment.
const WALLET: f64 = 100_000.0;
/// What the operator wrote in `policy.toml` — the slice of that wallet this deployment is allowed
/// to grow into.
const CEILING: f64 = 20_000.0;

fn bar(ts: i64, px: f64) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// An engine holding an AUTHORITATIVE venue wallet — the live shape, where `resolved_equity` is
/// `balance + unrealized` and the balance is the venue's number for the whole account.
fn engine(limits: RiskLimits, wallet: f64) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Authoritative),
        RiskGate::new(limits),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    e.account.balance = wallet;
    e
}

fn config(strategy: Box<dyn Strategy<LiveBroker> + Send>) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash: 1_000.0, // ignored in Authoritative mode; present so the mount is ordinary
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy,
        }),
        ..CoreConfig::default()
    }
}

fn close_bar(handle: &vike_core::CoreHandle, b: Bar) {
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bar: b,
        })
        .unwrap();
}

/// Targets half of whatever `ctx.equity()` reports, once — `LiveBroker::order_target_percent`,
/// which folds through `vike_model::units_from_percent`, the ONE sizing law.
struct TargetHalf {
    done: bool,
}
impl Strategy<LiveBroker> for TargetHalf {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if !self.done {
            self.done = true;
            broker.order_target_percent(0.5);
        }
    }
}

/// One order, sized off the strategy's `ctx.equity`, with the ceiling `limits` carries.
fn sized_qty(limits: RiskLimits) -> f64 {
    let handle = spawn_core(engine(limits, WALLET), config(Box::new(TargetHalf { done: false })));
    let cell = handle.snapshot_cell();
    close_bar(&handle, bar(60_000, 100.0));
    handle.shutdown_and_join();
    let snap = cell.load_full();
    assert_eq!(snap.orders.len(), 1, "one rebalance order expected: {:?}", snap.orders);
    assert_eq!(snap.orders[0].side, 1);
    snap.orders[0].qty
}

/// **THE SIZING HALF.** A percent-of-equity sizer on a 100k venue wallet, capped at 20k, buys the
/// units 20k pays for and not the ones the wallet would.
///
/// `units_from_percent(0.5, 20_000, 100, 1) = 100` against `(0.5, 100_000, 100, 1) = 500` — a 5×
/// difference that arrives entirely through `ExecutionEngine::sizing_equity`, with no call site in
/// `strategy_drive.rs` knowing anything about a ceiling.
#[test]
fn a_capped_engine_sizes_against_the_ceiling_not_the_wallet() {
    let qty = sized_qty(RiskLimits { max_sizing_equity: Some(CEILING), ..RiskLimits::new() });
    assert!(
        (qty - 100.0).abs() < 1e-12,
        "0.5 of a 20k ceiling at price 100 is 100 units, not {qty} — the strategy read the wallet"
    );
}

/// **THE NO-CHANGE CLAIM.** With no ceiling armed — every deployment that wrote no `policy.toml`
/// line — the same strategy on the same wallet sizes EXACTLY as it did before this axis existed,
/// bit for bit. `sizing_equity` with a `None` ceiling returns the `f64` verbatim rather than
/// `min`-ing it against an infinity, so this is an identity and not merely an approximation.
#[test]
fn an_unarmed_ceiling_sizes_bit_identically_to_the_wallet() {
    let qty = sized_qty(RiskLimits::new());
    assert_eq!(
        qty.to_bits(),
        500.0_f64.to_bits(),
        "0.5 of the uncapped 100k wallet at price 100 is 500 units, got {qty}"
    );
}

/// **THE REPORT SIDE.** The ceiling bounds what may be SPENT against the wallet; it does not
/// restate the wallet. `CoreSnapshot`'s per-venue equity — the number the GUI, `vike-cli` and an
/// incident review read — must still be the venue's own figure, or an operator would see their own
/// configuration reflected back at them instead of what the account holds.
#[test]
fn the_ceiling_does_not_rewrite_the_published_equity() {
    let limits = RiskLimits { max_sizing_equity: Some(CEILING), ..RiskLimits::new() };
    let handle = spawn_core(engine(limits, WALLET), config(Box::new(TargetHalf { done: true })));
    let cell = handle.snapshot_cell();
    close_bar(&handle, bar(60_000, 100.0));
    handle.shutdown_and_join();

    let snap = cell.load_full();
    let vb = snap.portfolio.venues.first().expect("one venue block");
    assert!(
        (vb.equity - WALLET).abs() < 1e-9,
        "the published equity is the ACCOUNT's, not the ceiling: got {}",
        vb.equity
    );
    assert!((vb.balance - WALLET).abs() < 1e-9, "and so is the balance: got {}", vb.balance);
}

// ---------------------------------------------------------------------------------------------
// The asymmetry
// ---------------------------------------------------------------------------------------------

/// A bare external LONG fill (empty coid) — folds a position with no client involvement, so the
/// pre-trade gate is not in this picture at all and the only thing under test is the sweep.
fn long_fill(tid: &'static str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: tid.into(),
        client_order_id: String::new(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 1,
        mark_price: Some(px),
        position_side: "BOTH".into(),
    })
}

/// A no-op strategy: this lane is about the WATCHDOG, and a strategy that traded would put the
/// pre-trade gate back into a test whose subject is the sweep.
struct Idle;
impl Strategy<LiveBroker> for Idle {
    fn on_bar(&mut self, _broker: &mut LiveBroker, _bar: &Bar) {}
}

/// Run the per-bar margin sweep on an account holding `position` units at 100, with `wallet` cash
/// and `limits`, and answer whether it liquidated anything.
fn liquidated(limits: RiskLimits, wallet: f64, position: f64) -> bool {
    let mut cfg = config(Box::new(Idle));
    cfg.margin_call = Some(MarginCallConfig::default());
    let handle = spawn_core(engine(limits, wallet), cfg);
    let cell = handle.snapshot_cell();
    handle.event_sender().blocking_send(long_fill("t0", position, 100.0)).unwrap();
    close_bar(&handle, bar(60_000, 100.0)); // marks the position ⇒ the sweep runs
    handle.shutdown_and_join();
    !cell.load_full().orders.is_empty()
}

/// **THE ASYMMETRY, and the direction a text gate cannot see.** The margin-call sweep judges
/// SOLVENCY, so it must read the account as it really is — a capped equity would make a healthy
/// account look under-margined and LIQUIDATE it, which is the opposite of the conservative effect
/// the same cap has on sizing.
///
/// The account here is arranged so the two answers DIFFER, which is the only arrangement that
/// proves anything: 10 units at 100 under the LEAN default 5% maintenance is 50 of margin used.
/// Against the real 1_000 wallet that is comfortably healthy; against a 40 ceiling it is a breach
/// (50 > 40·1.10 = 44) and the sweep would release a reduce-only MARKET. Nothing may be submitted.
#[test]
fn the_ceiling_never_reaches_the_margin_call_sweep() {
    let capped = RiskLimits { max_sizing_equity: Some(40.0), ..RiskLimits::new() };
    assert!(
        !liquidated(capped, 1_000.0, 10.0),
        "a solvent account was liquidated because the sweep read the operator's sizing ceiling \
         instead of its collateral — `resolved_equity` is the figure that lane must judge against"
    );
}

/// The other half of the pair, so the test above cannot pass by the sweep being inert: with the
/// REAL equity actually below maintenance, the same sweep DOES fire. Without this a broken
/// margin-call wiring would read as a green asymmetry proof.
#[test]
fn a_genuinely_under_margined_account_is_still_liquidated() {
    assert!(
        liquidated(RiskLimits { max_sizing_equity: Some(40.0), ..RiskLimits::new() }, 40.0, 10.0),
        "the sweep must still fire on a REAL breach — the ceiling changes nothing about it"
    );
}

// ---------------------------------------------------------------------------------------------
// The per-FILL equity — `AppliedFill::equity_after`, read as `ctx.equity` inside `on_fill`
// ---------------------------------------------------------------------------------------------

/// Records `ctx.equity()` every time a fill is delivered, and trades nothing — the subject is the
/// FIGURE the handler is handed, so an order placed from here would only put the pre-trade gate
/// back into a test that is not about it.
struct RecordEquityOnFill(Arc<Mutex<Vec<f64>>>);
impl Strategy<LiveBroker> for RecordEquityOnFill {
    fn on_bar(&mut self, _broker: &mut LiveBroker, _bar: &Bar) {}
    fn on_fill(&mut self, broker: &mut LiveBroker, _fill: &Fill) {
        // Through the `Broker` trait, not the field: this is the accessor a strategy actually has.
        self.0.lock().expect("equity log").push(broker.equity());
    }
}

/// Fold ONE external fill on a `wallet`-cash engine carrying `limits`, and answer with the
/// `ctx.equity` its `on_fill` was handed. The fill is 1 unit at the 100 mark, so it moves no
/// unrealized PnL and pays no commission — under `BalanceMode::Authoritative`, `equity_all` is
/// `balance + unrealized`, and the figure under test is the wallet itself, capped or not.
fn equity_seen_on_fill(limits: RiskLimits, wallet: f64) -> f64 {
    let log = Arc::new(Mutex::new(Vec::new()));
    let handle =
        spawn_core(engine(limits, wallet), config(Box::new(RecordEquityOnFill(Arc::clone(&log)))));
    handle.event_sender().blocking_send(long_fill("f0", 1.0, 100.0)).unwrap();
    // A closed bar drives the dispatch that delivers the buffered fill, then the teardown drain
    // catches anything still queued — so this cannot pass by the delivery simply not happening.
    close_bar(&handle, bar(60_000, 100.0));
    handle.shutdown_and_join();
    let seen = log.lock().expect("equity log").clone();
    assert_eq!(seen.len(), 1, "exactly one on_fill delivery expected, got {seen:?}");
    seen[0]
}

/// **THE HOLE THIS FILE EXISTS TO CLOSE.** A `Strategy::on_fill` handler re-sizing inside the fill
/// callback must see the CEILING, not the wallet — otherwise the one entry point that reads a
/// different equity source is the hole in a cap every other site honours, and a sizer that trades
/// on fills is uncapped while one that trades on bars is not.
#[test]
fn the_per_fill_equity_a_strategy_sizes_from_is_capped() {
    let limits = RiskLimits { max_sizing_equity: Some(CEILING), ..RiskLimits::new() };
    let seen = equity_seen_on_fill(limits, WALLET);
    assert!(
        (seen - CEILING).abs() < 1e-9,
        "on_fill was handed {seen}, not the {CEILING} ceiling — `AppliedFill::equity_after` is \
         reading `Account::equity_all` without `ExecutionEngine::cap_sizing_equity`"
    );
}

/// Its no-change twin, and the reason the test above cannot pass by the figure being wrong in some
/// other way: with no ceiling armed the same delivery hands back the wallet, BIT for bit. The cap
/// returns its input untouched rather than `min`-ing against an infinity, so this is an identity.
#[test]
fn an_unarmed_ceiling_leaves_the_per_fill_equity_bit_identical() {
    let seen = equity_seen_on_fill(RiskLimits::new(), WALLET);
    assert_eq!(
        seen.to_bits(),
        WALLET.to_bits(),
        "an unarmed ceiling must leave the per-fill equity the untouched wallet, got {seen}"
    );
}
