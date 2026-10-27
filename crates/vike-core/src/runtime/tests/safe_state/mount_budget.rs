//! Per-mount fill attribution and the optional per-mount loss/notional budget latch, and (at the
//! bottom) the attribution ledger of a mount trading a symbol QUOTED IN ANOTHER CURRENCY than the
//! account's: it must book what the account booked, at the rate in force when each fill folded.

use super::*;
use crate::runtime::test_support::coid_of;

// ---- steal/core-per-mount-budget: per-mount fill ATTRIBUTION + optional loss/notional BUDGET ----

/// A mount that submits ONE market order per hook call. Under [`RecordingClient`] the order is
/// RECORDED but never fills, so it RESTS — a mount whose resting order the budget latch can cancel
/// and whose post-latch intents `drain_broker` discards. The `symbol` arg is ignored (the mount
/// pins its own symbol), so this one strategy serves every mount.
struct BudgetSubmit;
impl Strategy<LiveBroker> for BudgetSubmit {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        broker.submit_market("x", 1, 1.0);
    }
}

fn budget_mount(symbol: &str) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        strategy: Box::new(BudgetSubmit),
    }
}

/// One `mount_id -> budget` entry — the [`CoreConfig::mount_budgets`] shape. These mounts carry no
/// `controller_id`, so their id is the legacy `{venue}__{symbol}__{interval}` derivation.
fn one_budget(symbol: &str, b: MountBudget) -> std::collections::HashMap<String, MountBudget> {
    let mut m = std::collections::HashMap::new();
    m.insert(crate::strategy_state::mount_id_with(None, "sim", symbol, "1m"), b);
    m
}

/// A fill for `coid` on (sim, symbol). Distinct `trade_id` per (coid, side, px) so the account's
/// per-trade dedup never collapses two injected fills. `mark_price: None` on purpose — a fill with a
/// mark writes the `PriceBoard` mark slot (engine `on_event`), which would then dominate the crash
/// price the sweep tests set via `set_mark`; leaving it `None` keeps the board reflecting only what
/// each test explicitly sets.
fn coid_fill(coid: &str, symbol: &str, side: i32, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        // No static prefix to hoist, so this is `new` + `expect` rather than `prefixed`: keeping the
        // id byte-identical matters more than the shape, and `coid` is a caller literal here.
        trade_id: TradeId::new(format!("{coid}-{side}-{px}")).expect("coid is never empty"),
        client_order_id: coid.to_string(),
        venue: "sim".into(),
        symbol: symbol.into(),
        side,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        ts: 0,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }
}

/// A scratch journal directory for the margin-budget lane, removed when the guard drops. The
/// journal's own `open` calls `create_dir_all`, so this reserves the path without creating it.
fn budget_journal_dir() -> crate::scratch::Scratch {
    crate::scratch::Scratch::reserved("mb")
}

/// OFF/default: with an EMPTY `mount_budgets`, no mount is watched — the sweep is never armed, a
/// direct sweep call no-ops, and NOTHING latches or cancels on even a catastrophic loss. The
/// attribution ledger STILL folds (it is additive + read-only and never gates a fold decision), so
/// trading behavior is byte-identical to a budget-free runtime.
#[test]
fn mount_budget_off_is_byte_identical() {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        ..CoreConfig::default() // mount_budgets: empty (the default)
    };
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;
    assert!(!core.any_mount_budget, "no active budget must leave the sweep un-armed");

    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid = coid_of(&core, 0);
    core.bus.publish(Event::Fill(coid_fill(&coid, "BTCUSDT", 1, 10.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();
    // a catastrophic crash on the board, then a DIRECT sweep call: still no latch (no budget).
    core.engine.price_board.set_mark("sim", "BTCUSDT", 1.0, 2);
    core.sweep_mount_budgets(3);

    assert!(!core.mount_latched[0], "no budget must never latch, even on a huge loss");
    assert!(core.engine.client.cancels.is_empty(), "no budget must issue no scoped cancel");
    assert_eq!(core.engine.trading_state, TradingState::Active, "account state untouched");
    assert_eq!(
        core.mount_attr[0].size.to_bits(),
        10.0_f64.to_bits(),
        "attribution still folds (additive, read-only)"
    );
}

/// ATTRIBUTION: fills route to the ORIGINATING mount's ledger by coid (not by (venue, symbol)), and
/// a realized-PnL close is booked to that mount ONLY — a sibling mount's ledger is untouched.
#[test]
fn mount_attribution_folds_fills_to_the_originating_mount() {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        extra_mounts: vec![budget_mount("ETHUSDT")],
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.extra_symbols = vec!["ETHUSDT".into()];
    core.engine.collect_applied_fills = true;

    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    core.drive_strategy_tick("sim", "ETHUSDT", 50.0, 1, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_a = coid_of(&core, 0);
    let coid_b = coid_of(&core, 1);
    assert_ne!(coid_a, coid_b);

    core.bus.publish(Event::Fill(coid_fill(&coid_a, "BTCUSDT", 1, 3.0, 100.0)), &mut core.engine);
    core.bus.publish(Event::Fill(coid_fill(&coid_b, "ETHUSDT", 1, 7.0, 50.0)), &mut core.engine);
    core.dispatch_applied_fills();
    assert_eq!(core.mount_attr[0].size.to_bits(), 3.0_f64.to_bits());
    assert_eq!(core.mount_attr[0].avg_px.to_bits(), 100.0_f64.to_bits());
    assert_eq!(core.mount_attr[1].size.to_bits(), 7.0_f64.to_bits());
    assert_eq!(core.mount_attr[1].avg_px.to_bits(), 50.0_f64.to_bits());

    // a close of A's BTC (sell 3 @ 110) attributes +30 realized to mount 0 only.
    core.coid_mount.insert("cA-close".to_string(), 0);
    core.bus
        .publish(Event::Fill(coid_fill("cA-close", "BTCUSDT", -1, 3.0, 110.0)), &mut core.engine);
    core.dispatch_applied_fills();
    assert_eq!(core.mount_attr[0].size.to_bits(), 0.0_f64.to_bits(), "A's position closed");
    assert!(
        (core.mount_attr[0].realized_pnl - 30.0).abs() < 1e-9,
        "A realized {}",
        core.mount_attr[0].realized_pnl
    );
    assert_eq!(
        core.mount_attr[1].realized_pnl.to_bits(),
        0.0_f64.to_bits(),
        "B's ledger untouched by A's close"
    );
}

/// BUDGET: a mount breaching `max_loss` latches liquidate-only — its OWN resting order is canceled
/// (scoped by coid), its post-latch intents are discarded, and the ACCOUNT trading_state is NOT
/// touched — while a SIBLING mount with no budget is untouched and keeps trading.
#[test]
fn mount_budget_latches_liquidate_only_and_leaves_the_sibling() {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        extra_mounts: vec![budget_mount("ETHUSDT")],
        mount_budgets: one_budget(
            "BTCUSDT",
            MountBudget { max_loss: Some(50.0), max_notional: None, flatten_on_breach: false },
        ),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.extra_symbols = vec!["ETHUSDT".into()];
    core.engine.collect_applied_fills = true;
    assert!(core.any_mount_budget, "an active budget arms the sweep");

    // A rests an order + takes a long 1 @ 100; B rests its own order.
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_a = coid_of(&core, 0);
    core.bus.publish(Event::Fill(coid_fill(&coid_a, "BTCUSDT", 1, 1.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();
    core.drive_strategy_tick("sim", "ETHUSDT", 50.0, 1, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_b = coid_of(&core, 1);

    // BTCUSDT crashes to 40: A's long 1 @ 100 marks -60 loss > 50.
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 2);
    core.sweep_mount_budgets(3);

    assert!(core.mount_latched[0], "A breached max_loss -> latched");
    assert!(!core.mount_latched[1], "B has no budget -> never latched");
    assert!(
        core.engine.client.cancels.contains(&coid_a),
        "A's resting order canceled: {:?}",
        core.engine.client.cancels
    );
    assert!(
        !core.engine.client.cancels.contains(&coid_b),
        "the sibling's resting order must NOT be canceled"
    );
    assert_eq!(
        core.engine.trading_state,
        TradingState::Active,
        "a per-mount latch must NOT flip the account-wide trading_state"
    );

    // A's post-latch intents are discarded; the sibling keeps trading.
    let n = core.engine.registry.len();
    core.drive_strategy_tick("sim", "BTCUSDT", 40.0, 4, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(core.engine.registry.len(), n, "a latched mount's new submit is discarded");
    core.drive_strategy_tick("sim", "ETHUSDT", 50.0, 5, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    assert_eq!(core.engine.registry.len(), n + 1, "the sibling mount keeps trading");

    // the widened per-mount view (published on CoreSnapshot via arc-swap) surfaces the latch, each
    // mount's budget, and the resolver-priced attributed position — read-only.
    let views = core.mount_views();
    assert!(views[0].latched, "the published view surfaces mount 0's latch");
    assert!(!views[1].latched, "the sibling view is not latched");
    assert!(views[0].budget.is_some(), "the view carries mount 0's budget");
    assert!(views[1].budget.is_none(), "the sibling carries no budget");
    assert_eq!(
        views[0].position.to_bits(),
        1.0_f64.to_bits(),
        "the view surfaces mount 0's attributed net position"
    );
}

/// BUDGET (max_notional arm): a mount whose gross notional exceeds `max_notional` latches, even with
/// no loss — proving the notional arm is independent of the loss arm.
#[test]
fn mount_budget_latches_on_notional() {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        mount_budgets: one_budget(
            "BTCUSDT",
            MountBudget { max_loss: None, max_notional: Some(150.0), flatten_on_breach: false },
        ),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;

    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_a = coid_of(&core, 0);
    // long 2 @ 100; mark 100 -> gross notional 200 > 150 (no loss at all).
    core.bus.publish(Event::Fill(coid_fill(&coid_a, "BTCUSDT", 1, 2.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();
    core.engine.price_board.set_mark("sim", "BTCUSDT", 100.0, 2);
    core.sweep_mount_budgets(3);
    assert!(core.mount_latched[0], "notional 200 > 150 must latch");
    assert!(core.engine.client.cancels.contains(&coid_a), "and cancel the mount's resting order");
}

/// BOUNDARY + JOURNAL: the latch fires at the per-CLOSED-bar boundary (via `drive_strategy`, not a
/// direct sweep call), and with `flatten_on_breach` the reduce-only MARKET flatten is journaled
/// write-ahead as a `MarginCallLiquidate` record (same shape + replay contract as a margin call) —
/// carrying, since journal v14, the OWNING mount id, which is what lets a restore book its fill into
/// that mount's ledger rather than the residual row.
#[test]
fn mount_budget_flatten_fires_at_bar_boundary_and_is_journaled() {
    let dir = budget_journal_dir();
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount("BTCUSDT")),
        mount_budgets: one_budget(
            "BTCUSDT",
            MountBudget { max_loss: Some(50.0), max_notional: None, flatten_on_breach: true },
        ),
        journal: Some(JournalConfig::at(dir.to_path_buf())),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;

    // A rests an order + takes a long 1 @ 100.
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid_a = coid_of(&core, 0);
    core.bus.publish(Event::Fill(coid_fill(&coid_a, "BTCUSDT", 1, 1.0, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();

    // crash the board mark to 40 (top of the resolve chain, robust), then let a CLOSED BAR drive
    // drive_strategy so the budget sweep runs AT THE BOUNDARY (no direct sweep call) and latches
    // (A's long 1 @ 100 marks -60 loss > 50) + flattens.
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 2);
    core.dispatch(Ingest::BarClose(Box::new(BarUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bar: flat_bar_unit_volume(60_000, 40.0),
    })));
    assert!(core.mount_latched[0], "the latch must fire at the per-closed-bar boundary");
    assert!(core.engine.client.cancels.contains(&coid_a), "and cancel the resting order");

    // a reduce-only MARKET flatten (sell 1 to close the long) was submitted...
    let flat = core
        .engine
        .client
        .submissions
        .iter()
        .find(|r| r.reduce_only && r.order_type == vike_model::OrderType::Market)
        .expect("a reduce-only flatten market was submitted");
    assert_eq!(flat.symbol, "BTCUSDT");
    assert!((flat.qty - 1.0).abs() < 1e-12);
    assert_eq!(flat.side, vike_model::closing_side(1.0), "sell to close a long");

    // ...and it was journaled WRITE-AHEAD as a MarginCallLiquidate record. Flush + DROP the core
    // (releasing the journal's file handle + single-writer lock) BEFORE reading, so `read_all` never
    // races the live writer's open handle (Windows exclusive-open safety).
    core.journal.as_mut().unwrap().flush().unwrap();
    drop(core);
    let records = vike_journal::CommandJournal::read_all(&dir).unwrap();
    let want_mount = crate::strategy_state::mount_id_with(None, "sim", "BTCUSDT", "1m");
    assert!(
        records.iter().any(|r| matches!(
            r,
            vike_journal::JournalRecord::MarginCallLiquidate { req, mount_id, .. }
                if req.reduce_only && req.symbol == "BTCUSDT" && req.order_type == vike_model::OrderType::Market
                    // v14: the OWNING mount rides the record, so the restore-side
                    // `fold_coid_mounts` credits this order to the mount that released it instead
                    // of leaving it — and its realized loss — in the residual row.
                    && mount_id.as_deref() == Some(want_mount.as_str())
        )),
        "the flatten must be journaled write-ahead, naming mount `{want_mount}`: {records:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the budget latch on the RISK CLOCK (adversarial review, finding 3) --------------------------
//
// «what if the candles are 4h, you will check once per 4h?» holds for a mount's loss/notional budget as
// much as for the margin call: `sweep_mount_budgets` ran from `drive_strategy`'s closed bar alone. The
// clock now runs it too, priced and skipped by the same age bound (a stale mount is judged by its bars
// only), fire-once as ever (`mount_latched`).

/// A budgeted mount on a node whose risk clock is armed (`venues` empty: every venue is mounted).
fn clock_budget_core(flatten: bool, journal: Option<JournalConfig>) -> CoreThread<RecordingClient> {
    clock_budget_core_sized(10_000.0, 1.0, None, flatten, journal)
}

/// [`clock_budget_core`] with the account's `seed`, the mount's long of `qty` at 100 and, when
/// `margin_call` is set, the margin-call watchdog beside the budget.
fn clock_budget_core_sized(
    seed: f64,
    qty: f64,
    margin_call: Option<vike_model::MarginCallConfig>,
    flatten: bool,
    journal: Option<JournalConfig>,
) -> CoreThread<RecordingClient> {
    let config = CoreConfig {
        seed_cash: seed,
        margin_call,
        strategy: Some(budget_mount("BTCUSDT")),
        mount_budgets: one_budget(
            "BTCUSDT",
            MountBudget { max_loss: Some(50.0), max_notional: None, flatten_on_breach: flatten },
        ),
        price_only: Some(PriceOnlyConfig {
            venues: std::collections::BTreeSet::new(),
            sweep_interval: std::time::Duration::from_secs(30),
            max_price_age: std::time::Duration::from_secs(60),
            price_source: std::collections::BTreeMap::new(),
        }),
        journal,
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;
    // the mount rests an order and takes a long `qty` @ 100
    core.drive_strategy_tick("sim", "BTCUSDT", 100.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let coid = coid_of(&core, 0);
    core.bus.publish(Event::Fill(coid_fill(&coid, "BTCUSDT", 1, qty, 100.0)), &mut core.engine);
    core.dispatch_applied_fills();
    core.arm_boundary_timers(0);
    core
}

fn budget_latch_notes(core: &CoreThread<RecordingClient>) -> usize {
    core.recent.iter().filter(|e| e.starts_with("MOUNT BUDGET LATCH")).count()
}

/// ⚠ **A mount's budget is judged by the CLOCK, between bars, once.** The mark crashes to 40 (A's long 1
/// at 100 is 60 down against a `max_loss` of 50), stamped 5 s before the sweep, and NO bar ever closes:
/// the first sweep latches the mount and cancels its own order; the next sweeps add nothing (fire-once).
#[test]
fn the_clock_latches_a_mounts_budget_breach_between_bars_once() {
    let mut core = clock_budget_core(false, None);
    let coid = coid_of(&core, 0);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(29_000);
    assert!(!core.mount_latched[0], "not due before the interval");
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "latched by the clock: {:?}", core.recent);
    assert!(core.engine.client.cancels.contains(&coid), "its own resting order, canceled");
    let canceled = core.engine.client.cancels.len();
    assert_eq!(
        core.engine.trading_state,
        TradingState::Active,
        "a mount latch never touches the account"
    );
    core.drive_due_timers(61_000);
    assert_eq!(budget_latch_notes(&core), 1, "fire-once: {:?}", core.recent);
    assert_eq!(core.engine.client.cancels.len(), canceled, "…no second cancel");
}

/// The SAME staleness rule: a mount whose only price is older than the bound is not judged by the clock
/// (its closed bars still judge it), the skip is said once, and a fresh mark brings it back.
#[test]
fn the_clock_skips_a_stale_mounts_budget_and_judges_it_again_on_a_fresh_mark() {
    let mut core = clock_budget_core(false, None);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 1); // stamped at t=1
    for now in [70_000, 101_000, 132_000] {
        core.drive_due_timers(now);
    }
    assert!(!core.mount_latched[0], "no latch off a stale mark");
    let skips = |c: &CoreThread<RecordingClient>| {
        c.recent.iter().filter(|e| e.contains("mounted-engine clock sweeps SKIP")).count()
    };
    assert_eq!(skips(&core), 1, "said once: {:?}", core.recent);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 162_500);
    core.drive_due_timers(163_000);
    assert!(core.mount_latched[0], "latched on the fresh mark");
}

/// The flatten is released by the clock exactly as by a bar: a reduce-only MARKET close of the mount's
/// attributed position, journaled WRITE-AHEAD as a `MarginCallLiquidate` naming the owning mount (so a
/// replay re-applies it and the restore books its fill into the mount's ledger).
#[test]
fn the_clocks_budget_flatten_is_journaled_with_its_mount() {
    let dir = budget_journal_dir();
    let mut core = clock_budget_core(true, Some(JournalConfig::at(dir.to_path_buf())));
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0]);
    let flat = core
        .engine
        .client
        .submissions
        .iter()
        .find(|r| r.reduce_only && r.order_type == vike_model::OrderType::Market)
        .expect("a reduce-only flatten market was submitted by the clock");
    assert!((flat.qty - 1.0).abs() < 1e-12 && flat.side == vike_model::closing_side(1.0));
    core.journal.as_mut().unwrap().flush().unwrap();
    drop(core);
    let want_mount = crate::strategy_state::mount_id_with(None, "sim", "BTCUSDT", "1m");
    let records = vike_journal::CommandJournal::read_all(&dir).unwrap();
    assert!(
        records.iter().any(|r| matches!(
            r,
            vike_journal::JournalRecord::MarginCallLiquidate { req, mount_id, .. }
                if req.reduce_only && mount_id.as_deref() == Some(want_mount.as_str())
        )),
        "the clock's flatten must be journaled write-ahead, naming mount `{want_mount}`: {records:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- a flatten beside the margin call must not close more than the position (second review, 1) ----
//
// One clock sweep runs the margin call, then the mount budgets. The margin call releases a reduce-only
// close of the engine's excess; the budget latch then flattens the mount's WHOLE attributed size. The gate
// nets in-flight reduce-only orders only against a FLAT book (`cover_position_size`), so on a non-flat
// book both are admitted, an async client has folded neither, and the two add up to more than the
// position. A venue that puts `reduce_only` on the wire trims the second; oanda, ig, fxcm, ibkr, ctrader
// and alpaca do not, and the position flips. `latch_mount` therefore flattens at most what the book still
// holds after the live reduce-only orders on that side (other than the latched mount's own, which it has
// just canceled). `RecordingClient` is the async client with no trim, which is the venue that cannot
// save the sum: it records every order and folds none.

/// `(number of reduce-only orders sent, their total quantity)`.
fn reduce_only_sent(core: &CoreThread<RecordingClient>) -> (usize, f64) {
    let sent: Vec<f64> =
        core.engine.client.submissions.iter().filter(|r| r.reduce_only).map(|r| r.qty).collect();
    (sent.len(), sent.iter().sum())
}

/// ⚠ A long 10 at 100 on a seed of 100, the market at 94: the margin call breaches (equity 40 against a
/// maintenance of 47) and closes its excess, a part of the position; the mount's loss (60) is past its
/// budget (50) on the SAME tick, and the flatten that follows is clamped to what is left, so the two close
/// the position exactly and never more.
#[test]
fn a_margin_breach_and_a_budget_breach_on_one_tick_never_close_more_than_the_position() {
    let margin = Some(vike_model::MarginCallConfig::default());
    let mut core = clock_budget_core_sized(100.0, 10.0, margin, true, None);
    let coid = coid_of(&core, 0);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 94.0, 25_000);
    core.drive_due_timers(30_100);

    assert!(core.mount_latched[0], "the mount is latched: {:?}", core.recent);
    assert!(core.engine.client.cancels.contains(&coid), "…its orders canceled");
    let (n, total) = reduce_only_sent(&core);
    assert_eq!(n, 2, "the margin call's close and the (clamped) flatten: {:?}", core.recent);
    assert!(total <= 10.0 + 1e-9, "the closes must never exceed the 10 held, sent {total}");
    assert!((total - 10.0).abs() < 1e-9, "…and they close it, sent {total}");
}

/// The margin call alone already covers the whole position (the market at 90: equity 0 against a
/// maintenance of 45, every unit goes): the flatten has nothing left to close and is SKIPPED, said, while
/// the mount stays latched and its orders canceled.
#[test]
fn a_flatten_the_margin_call_already_covers_is_skipped_but_the_mount_stays_latched() {
    let margin = Some(vike_model::MarginCallConfig::default());
    let mut core = clock_budget_core_sized(100.0, 10.0, margin, true, None);
    let coid = coid_of(&core, 0);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 90.0, 25_000);
    core.drive_due_timers(30_100);

    assert!(core.mount_latched[0], "latched: {:?}", core.recent);
    assert!(core.engine.client.cancels.contains(&coid), "…its orders canceled");
    let (n, total) = reduce_only_sent(&core);
    assert_eq!(n, 1, "only the margin call's close: {:?}", core.recent);
    assert!((total - 10.0).abs() < 1e-9, "…which closes the 10 held, sent {total}");
    assert!(
        core.recent.iter().any(|e| e.contains("MOUNT BUDGET LATCH") && e.contains("flatten")),
        "the skipped flatten is said: {:?}",
        core.recent
    );
}

/// ⚠ A STOPPED account's mount is not latched, by either pass (second review, 3): the margin call skips a
/// retired engine, but the budget sweep did not, so it spent the mount's fire-once latch on a flatten the
/// core refuses (the account is not running), leaving the position for ever unjudged once the account
/// starts again. The closed-bar pass is covered by the same rule; no test pinned the old behaviour.
#[test]
fn a_stopped_accounts_mount_is_not_latched_by_either_pass() {
    let mut core = clock_budget_core(true, None);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.retired_engines.push(0);
    core.sweep_mount_budgets(3); // the closed-bar pass
    core.drive_due_timers(30_100); // the clock
    assert!(
        !core.mount_latched[0],
        "the latch is not spent on a stopped account: {:?}",
        core.recent
    );
    assert_eq!(reduce_only_sent(&core).0, 0, "no flatten sent to a detaching client");
    assert!(core.engine.client.cancels.is_empty(), "…and no cancel");
}

// ---- what counts as a close "in flight" (third review, 3) ----------------------------------------------
//
// `flatten_qty` summed EVERY live reduce-only order on the exact symbol, so a take-profit resting far from the
// market (which cannot fill now) made the budget latch skip its flatten and leave the position open; it missed
// an order that names no symbol (the engine's own, by the convention of `apply/exit.rs`); and on a hedge-mode
// book it netted the Long and Short buckets against each other. It now counts the MARKET closes of the symbol
// (or of the engine's own, when blank) on the side, and takes each side's headroom from the bucket that side
// closes.

/// Register a live reduce-only order straight into engine 0's registry, as `submit_order` leaves it (live, nothing
/// filled), with the symbol, kind, side and quantity a test picks.
fn rest_close(
    core: &mut CoreThread<RecordingClient>,
    coid: &str,
    symbol: &str,
    order_type: vike_model::OrderType,
    side: i32,
    qty: f64,
) {
    let price = (order_type == vike_model::OrderType::Limit).then_some(500.0);
    core.engine.registry.insert(
        coid.into(),
        vike_model::ManagedOrder::new(vike_model::OrderRequest {
            client_order_id: coid.into(),
            venue: "sim".into(),
            symbol: symbol.into(),
            side,
            qty,
            order_type,
            price,
            reduce_only: true,
            ..Default::default()
        }),
    );
}

/// ⚠ **A TAKE-PROFIT RESTING FAR FROM THE MARKET IS NOT A CLOSE IN FLIGHT.** The mount's long 1 at 100 is down to 40
/// (a loss of 60 against a budget of 50) and the engine has a reduce-only LIMIT sell of 1 at 500 resting: it will not
/// fill, so the flatten still goes out. It used to be skipped ("the book can still take 0") and the mount left open.
#[test]
fn a_far_resting_take_profit_does_not_make_the_latch_skip_its_flatten() {
    let mut core = clock_budget_core(true, None);
    rest_close(&mut core, "tp", "BTCUSDT", vike_model::OrderType::Limit, -1, 1.0);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 40.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "latched: {:?}", core.recent);
    let (n, total) = reduce_only_sent(&core);
    assert_eq!((n, total), (1, 1.0), "the flatten, whole: {:?}", core.recent);
}

/// An order that names NO symbol is the engine's own (`apply/exit.rs`, the adapters read it so): a market close of
/// 4 sent that way is 4 of the 10 the book holds.
#[test]
fn a_blank_symbol_close_in_flight_is_counted_against_the_engines_own_symbol() {
    let mut core = clock_budget_core_sized(100.0, 10.0, None, true, None);
    rest_close(&mut core, "blank", "", vike_model::OrderType::Market, -1, 4.0);
    let q = core.flatten_qty(0, "BTCUSDT", -1, 10.0);
    assert!((q - 6.0).abs() < 1e-9, "10 held, 4 on its way out: {q}");
}

/// ⚠ **HEDGE MODE nets per SIDE.** Long 10 and short 6 in their own buckets: a sell closes the long (room 10), a buy
/// closes the short (room 6), each less the market closes already out on THAT side. The old `Long + Short` read
/// the book as net long 4, so the long flatten was clamped to 4 and the short one (a buy against a "long" book)
/// was skipped.
#[test]
fn hedge_mode_headroom_is_taken_per_side_not_from_the_net() {
    use vike_model::events::PositionSide;
    let mut core = clock_budget_core_sized(100.0, 10.0, None, true, None);
    core.engine.account.positions.clear();
    for (side, size) in [(PositionSide::Long, 10.0), (PositionSide::Short, -6.0)] {
        core.engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), side),
            vike_exec::PositionEntry { size, avg_px: 100.0, ..Default::default() },
        );
    }
    let close_long = core.flatten_qty(0, "BTCUSDT", -1, 10.0);
    let close_short = core.flatten_qty(0, "BTCUSDT", 1, 6.0);
    assert!((close_long - 10.0).abs() < 1e-9, "the whole long, not the net: {close_long}");
    assert!((close_short - 6.0).abs() < 1e-9, "the whole short, not skipped: {close_short}");

    rest_close(&mut core, "sell", "BTCUSDT", vike_model::OrderType::Market, -1, 3.0);
    let close_long = core.flatten_qty(0, "BTCUSDT", -1, 10.0);
    let close_short = core.flatten_qty(0, "BTCUSDT", 1, 6.0);
    assert!(
        (close_long - 7.0).abs() < 1e-9,
        "a sell out of 3 shrinks the long's room: {close_long}"
    );
    assert!((close_short - 6.0).abs() < 1e-9, "…and not the short's: {close_short}");
}

// ---- multi-currency: the mount ledger books what the ACCOUNT booked ------------------------------
//
// The per-mount ledger folds each attributed fill through `compute_fill_value` with the contract
// value and the commission the ACCOUNT folded that fill with (`AppliedFill::fx`), because re-reading
// them could price the fill at a rate that has since moved and the raw commission may be in the
// symbol's currency. So on a USD account trading a yen-quoted symbol, `Σ mounts + residual ==
// account` must stay exact. Every figure below is a dyadic rational (1/128, 65/8192, whole prices
// and quantities), so each product is exact and the comparisons are `to_bits`, never a tolerance.

/// 128 yen to the dollar, then 65/8192 (about 126.03) after the refresh.
const R1: f64 = 1.0 / 128.0;
const R2: f64 = 65.0 / 8192.0;

/// A `coid_fill` with a commission of `commission` stated in `asset`.
fn priced_fill(
    coid: &str,
    symbol: &str,
    side: i32,
    qty: f64,
    px: f64,
    commission: f64,
    asset: &str,
) -> FillEvent {
    FillEvent {
        commission,
        commission_asset: asset.into(),
        ..coid_fill(coid, symbol, side, qty, px)
    }
}

/// `USD_JPY` quoted in yen at `rate`, observed at `as_of_ms`, with a mark to open against.
fn jpy_admission(rate: f64, as_of_ms: i64) -> vike_exec::SymbolAdmission {
    vike_exec::SymbolAdmission {
        route_key: "sim".into(),
        symbol: "USD_JPY".into(),
        grid: vike_model::SymbolGrid {
            tick_size: Some(0.25),
            lot_size: Some(1.0),
            ..vike_model::SymbolGrid::default()
        },
        contract_value: None,
        initial_mark: Some(150.0),
        quote_ccy: Some("JPY".into()),
        quote_rate: Some(rate),
        rate_as_of_ms: Some(as_of_ms),
        im_requirement: None,
    }
}

/// A core whose one mount trades `symbol` and hears the fills the engine applies.
fn ledger_core(symbol: &str) -> CoreThread<RecordingClient> {
    let config = CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(budget_mount(symbol)),
        ..CoreConfig::default()
    };
    let mut core = core_with(config);
    core.engine.collect_applied_fills = true;
    core
}

/// The mount's four-fill life: it rests an order and is filled long 1000 @ 150 (commission 200 in
/// `open_asset`), then `refresh` runs, then it sells 400 @ 151 (0.25 in USD) and 100 @ 152 (3.0 in
/// BNB, a THIRD currency for the yen account).
fn play_ledger(
    core: &mut CoreThread<RecordingClient>,
    symbol: &str,
    open_asset: &str,
    refresh: impl FnOnce(&mut CoreThread<RecordingClient>),
) {
    core.drive_strategy_tick("sim", symbol, 150.0, 0, |s, ctx| s.on_quote_tick(ctx, &quote_tick()));
    assert_eq!(core.engine.registry.len(), 1, "the mount's order passed the gate and rests");
    let open = coid_of(core, 0);
    core.bus.publish(
        Event::Fill(priced_fill(&open, symbol, 1, 1000.0, 150.0, 200.0, open_asset)),
        &mut core.engine,
    );
    core.dispatch_applied_fills();

    refresh(core);

    core.coid_mount.insert("cA-close1".to_string(), 0);
    core.bus.publish(
        Event::Fill(priced_fill("cA-close1", symbol, -1, 400.0, 151.0, 0.25, "USD")),
        &mut core.engine,
    );
    core.dispatch_applied_fills();
    core.coid_mount.insert("cA-close2".to_string(), 0);
    core.bus.publish(
        Event::Fill(priced_fill("cA-close2", symbol, -1, 100.0, 152.0, 3.0, "BNB")),
        &mut core.engine,
    );
    core.dispatch_applied_fills();
}

/// `Σ mount rows + residual == account`, exactly: the mount rows are net of fees, the residual is
/// the account's net realized minus them. Returns the residual's figure.
fn residual_of(core: &CoreThread<RecordingClient>) -> f64 {
    let views = core.mount_views();
    let attributed: f64 = views
        .iter()
        .filter(|v| v.kind == vike_exec::MountRowKind::Mount)
        .map(|v| v.realized_pnl)
        .sum();
    let acct = &core.engine.account;
    assert_eq!(
        attributed.to_bits(),
        (acct.realized_pnl - acct.fees_paid).to_bits(),
        "the mount rows are the whole of the account's net realized PnL"
    );
    views
        .iter()
        .find(|v| v.kind == vike_exec::MountRowKind::Residual)
        .expect("a core with a mount publishes the residual row")
        .realized_pnl
}

/// **The headline.** A USD account, a mount on a yen-quoted symbol: opened at one rate, the rate
/// REFRESHED, then closed in two parts. The ledger of the mount equals the ACCOUNT's booked
/// realized PnL and fees, bit for bit, and each leg of it was booked at the rate in force when its
/// fill folded: the open fee (200 yen) at `R1`, both closes at `R2`. The BNB fee is left out of
/// BOTH (the account cannot convert it and says so once), never booked at par into either.
#[test]
fn a_mount_ledger_on_a_yen_symbol_books_what_the_account_booked_at_each_fills_rate() {
    let mut core = ledger_core("USD_JPY");
    core.engine.quote_asset = "USD".into();
    core.engine.now_ms = 1_000;
    assert_eq!(
        core.engine.admit_symbol(&jpy_admission(R1, 1_000)),
        vike_exec::AdmitOutcome::Admitted
    );

    play_ledger(&mut core, "USD_JPY", "JPY", |core| {
        core.engine.now_ms = 2_000;
        assert_eq!(
            core.engine.admit_symbol(&jpy_admission(R2, 2_000)),
            vike_exec::AdmitOutcome::Refreshed,
            "a repeat admission with a newer rate is the refresh"
        );
    });

    // The oracle, from the formula: qty * (exit - entry) * unit * rate-at-the-fold, and the fees
    // each converted at the rate in force at ITS fold.
    let realized = 400.0 * (151.0 - 150.0) * 1.0 * R2 + 100.0 * (152.0 - 150.0) * 1.0 * R2;
    let fees = 200.0 * R1 + 0.25;
    assert_eq!(realized.to_bits(), 4.760_742_187_5_f64.to_bits(), "the oracle itself");
    assert_eq!(
        fees.to_bits(),
        1.8125_f64.to_bits(),
        "…and its fees: 200 yen at 1/128 plus 0.25 USD"
    );

    let acct = &core.engine.account;
    assert_eq!(acct.realized_pnl.to_bits(), realized.to_bits(), "the account booked the oracle");
    assert_eq!(acct.fees_paid.to_bits(), fees.to_bits());
    assert_eq!(acct.fees_unconverted(), 1, "the BNB fee was counted as left out");

    let mount = core.mount_attr[0];
    assert_eq!(mount.realized_pnl.to_bits(), acct.realized_pnl.to_bits(), "mount == account, PnL");
    assert_eq!(mount.fees_paid.to_bits(), acct.fees_paid.to_bits(), "mount == account, fees");
    assert_eq!(mount.size.to_bits(), 500.0_f64.to_bits(), "1000 opened, 500 closed");
    assert_eq!(mount.avg_px.to_bits(), 150.0_f64.to_bits());
    assert_eq!(
        residual_of(&core).to_bits(),
        0.0_f64.to_bits(),
        "nothing is left to the residual row"
    );

    // What is left of the position is valued at the rate NOW (R2), through the same contract value.
    core.engine.price_board.set_mark("sim", "USD_JPY", 153.0, 5_000);
    let views = core.mount_views();
    let row =
        views.iter().find(|v| v.kind == vike_exec::MountRowKind::Mount).expect("the mount's row");
    assert_eq!(row.position.to_bits(), 500.0_f64.to_bits());
    assert_eq!(row.unrealized_pnl.to_bits(), ((153.0 - 150.0) * 500.0 * R2).to_bits());
    assert_eq!(row.notional.to_bits(), (500.0 * 153.0 * R2).to_bits());
}

/// **The twin.** The same four fills on a symbol quoted in the account's own currency: no fx book,
/// every commission as the venue stated it (the BNB one included, as it always was), the mount
/// ledger equal to the account's, the residual zero. The legacy path is what it was.
#[test]
fn the_single_currency_twin_books_the_same_fills_as_it_always_did() {
    let mut core = ledger_core("ETHUSDT");
    core.engine.extra_symbols = vec!["ETHUSDT".into()];

    play_ledger(&mut core, "ETHUSDT", "USDT", |_| {});

    let acct = &core.engine.account;
    assert!(!acct.has_foreign_quote(), "no foreign-quoted symbol: the fx book never existed");
    assert_eq!(acct.fees_unconverted(), 0);
    let realized: f64 = 400.0 * (151.0 - 150.0) * 1.0 + 100.0 * (152.0 - 150.0) * 1.0;
    assert_eq!(acct.realized_pnl.to_bits(), realized.to_bits(), "600 of price PnL at unit 1.0");
    assert_eq!(
        acct.fees_paid.to_bits(),
        (200.0 + 0.25 + 3.0_f64).to_bits(),
        "every fee as stated, whatever its asset"
    );
    let mount = core.mount_attr[0];
    assert_eq!(mount.realized_pnl.to_bits(), acct.realized_pnl.to_bits());
    assert_eq!(mount.fees_paid.to_bits(), acct.fees_paid.to_bits());
    assert_eq!(residual_of(&core).to_bits(), 0.0_f64.to_bits());
}
