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
