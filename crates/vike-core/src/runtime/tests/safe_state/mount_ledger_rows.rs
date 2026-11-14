//! A mount with LEGS keeps one ledger row per `(venue, symbol)` (`MountAttribution`). The account folds by
//! instrument; a single `(size, average_price)` per mount folded a sell of ETH as a close of a long of BTC, so the budget
//! latch (`sweep_mount_budgets_in`, `policy.max_mount_loss`) could latch and flatten a healthy mount, or miss a real
//! loss, and the published mount row read the same wrong numbers.

use super::mount_budget::{
    budget_journal_dir, budget_mount, one_budget, order_client_id_fill, venue_rejects,
};
use super::*;
use crate::runtime::test_support::core_of;
use vike_model::{OrderBroker, OrderPlacement};

/// Sends its orders once, on the first tick. The symbol it names is authoritative: the mount declares its legs.
struct Legs {
    orders: Vec<(&'static str, i32, f64)>,
}
impl Strategy<LiveBroker> for Legs {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &TickQuote) {
        for (symbol, side, qty) in std::mem::take(&mut self.orders) {
            broker.place(symbol, "", side, qty, OrderPlacement::Market, false);
        }
    }
}

/// A mount on `(sim, BTCUSDT)` declaring `ETHUSDT` as a leg: long 1 BTC, short 15 ETH.
fn two_leg_mount() -> Mount {
    Mount {
        symbols: vec![MountLeg::same_venue("ETHUSDT")],
        strategy: Box::new(Legs { orders: vec![("BTCUSDT", 1, 1.0), ("ETHUSDT", -1, 15.0)] }),
        ..budget_mount("BTCUSDT")
    }
}

fn clock() -> Option<PriceOnlyConfig> {
    Some(PriceOnlyConfig {
        venues: std::collections::BTreeSet::new(),
        sweep_interval: std::time::Duration::from_secs(30),
        max_price_age: std::time::Duration::from_secs(60),
        price_source: std::collections::BTreeMap::new(),
    })
}

/// The config of a two-leg mount with a flattening loss budget of `max_loss`, on an armed risk clock.
fn two_leg_config(max_loss: f64) -> CoreConfig {
    CoreConfig {
        seed_cash: 1_000_000.0,
        strategy: Some(two_leg_mount()),
        mount_budgets: one_budget(
            "BTCUSDT",
            MountBudget { max_loss: Some(max_loss), max_notional: None, flatten_on_breach: true },
        ),
        price_only: clock(),
        ..CoreConfig::default()
    }
}

/// The non-reduce-only order the mount sent on `symbol` in engine 0.
fn entry_of(core: &CoreThread<RecordingClient>, symbol: &str) -> String {
    core.engine
        .client
        .submissions
        .iter()
        .find(|r| r.symbol == symbol && !r.reduce_only)
        .map(|r| r.order_client_id.clone())
        .unwrap_or_else(|| panic!("the mount sent an order on {symbol}"))
}

/// The live reduce-only close of `symbol` in engine 0.
fn close_of(core: &CoreThread<RecordingClient>, symbol: &str) -> String {
    core.engine
        .client
        .submissions
        .iter()
        .find(|r| r.symbol == symbol && r.reduce_only)
        .map(|r| r.order_client_id.clone())
        .unwrap_or_else(|| panic!("a close went out on {symbol}"))
}

fn fill(
    core: &mut CoreThread<RecordingClient>,
    order_client_id: &str,
    symbol: &str,
    side: i32,
    q: f64,
    px: f64,
) {
    core.bus.publish(
        Event::Fill(order_client_id_fill(order_client_id, symbol, side, q, px)),
        &mut core.engine,
    );
    core.dispatch_applied_fills();
}

/// Buy 1 BTC at 60,000 and sell 15 ETH at 3,000: two opens, on two instruments.
fn open_both_legs(core: &mut CoreThread<RecordingClient>) {
    core.engine.collect_applied_fills = true;
    core.engine.extra_symbols = vec!["ETHUSDT".into()];
    core.drive_strategy_tick("sim", "BTCUSDT", 60_000.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let btc = entry_of(core, "BTCUSDT");
    let eth = entry_of(core, "ETHUSDT");
    fill(core, &btc, "BTCUSDT", 1, 1.0, 60_000.0);
    fill(core, &eth, "ETHUSDT", -1, 15.0, 3_000.0);
    core.arm_boundary_timers(0);
}

fn two_leg_core(max_loss: f64) -> CoreThread<RecordingClient> {
    let mut core = core_with(two_leg_config(max_loss));
    open_both_legs(&mut core);
    core
}

fn mark(core: &mut CoreThread<RecordingClient>, btc: f64, eth: f64, ts: i64) {
    core.engine.price_board.set_mark("sim", "BTCUSDT", btc, ts);
    core.engine.price_board.set_mark("sim", "ETHUSDT", eth, ts);
}

/// `(symbol, side, qty)` of every reduce-only order engine 0's client received.
fn closes(core: &CoreThread<RecordingClient>) -> Vec<(String, i32, f64)> {
    core.engine
        .client
        .submissions
        .iter()
        .filter(|r| r.reduce_only)
        .map(|r| (r.symbol.clone(), r.side, r.qty))
        .collect()
}

/// The realized PnL no mount row claims: `Σ mount rows + residual == account realized net`.
fn residual(core: &CoreThread<RecordingClient>) -> f64 {
    core.mount_views()
        .iter()
        .find(|v| v.kind == vike_execution::MountViewKind::Residual)
        .map(|v| v.realized_pnl)
        .expect("a mounted core publishes the residual row")
}

/// ⚠ **The loss that never happened.** Long 1 BTC at 60,000, then short 15 ETH at 3,000, with no price moving.
/// The account books two opens and no PnL; one `(size, average_price)` per mount folded the sell as closing 1 at 3,000,
/// realized -57,000 and left a "short 14" priced at BTC's mark. The mount must read what the account reads, the
/// budget must not latch, and nothing may be sent.
#[test]
fn a_two_leg_mount_books_no_loss_it_never_took() {
    let mut core = two_leg_core(100.0);
    assert_eq!(core.engine.account.realized_pnl, 0.0, "the account booked two opens");
    assert_eq!(core.mount_valuation(0).1, 0.0, "the mount's realized PnL is the account's");
    assert_eq!(residual(&core), 0.0, "and no PnL is left to the residual row");
    let attr = &core.mount_attribution[0];
    assert_eq!((attr.size, attr.average_price), (1.0, 60_000.0), "the own row");
    assert_eq!(attr.legs.len(), 1, "one more row: {attr:?}");
    assert_eq!((attr.legs[0].venue.as_str(), attr.legs[0].symbol.as_str()), ("sim", "ETHUSDT"));
    assert_eq!((attr.legs[0].attr.size, attr.legs[0].attr.average_price), (-15.0, 3_000.0));

    mark(&mut core, 60_000.0, 3_000.0, 25_000);
    let (position, realized, unrealized, notional) = core.mount_valuation(0);
    assert_eq!(position, 1.0, "the published position is the mount's own symbol's");
    assert_eq!((realized, unrealized), (0.0, 0.0), "no price moved");
    assert_eq!(notional, 60_000.0 + 15.0 * 3_000.0, "each leg at its own price");

    core.drive_due_timers(30_100);
    assert!(!core.mount_latched[0], "a healthy mount stays free: {:?}", core.recent);
    assert!(closes(&core).is_empty(), "and nothing is flattened: {:?}", closes(&core));
}

/// The budget sees the SUM of the legs, each at its own price: 600 down on BTC and 600 down on ETH is inside a
/// budget of 1,300, and past one of 1,000, where the latch closes EACH leg on its own symbol and side.
#[test]
fn the_budget_judges_the_sum_of_the_legs_and_the_flatten_closes_each_leg() {
    let mut inside = two_leg_core(1_300.0);
    mark(&mut inside, 59_400.0, 3_040.0, 25_000);
    let (_, realized, unrealized, notional) = inside.mount_valuation(0);
    assert_eq!((realized, unrealized), (0.0, -1_200.0), "600 down on each leg");
    assert_eq!(notional, 59_400.0 + 15.0 * 3_040.0);
    inside.drive_due_timers(30_100);
    assert!(!inside.mount_latched[0], "1,200 is inside 1,300: {:?}", inside.recent);

    let mut past = two_leg_core(1_000.0);
    mark(&mut past, 59_400.0, 3_040.0, 25_000);
    past.drive_due_timers(30_100);
    assert!(past.mount_latched[0], "1,200 is past 1,000: {:?}", past.recent);
    assert_eq!(
        closes(&past),
        vec![("BTCUSDT".to_string(), -1, 1.0), ("ETHUSDT".to_string(), 1, 15.0)],
        "one reduce-only close per leg: {:?}",
        past.recent
    );
}

/// Each row reads ITS instrument's contract value: the fold read the fill symbol's, the valuation the mount
/// symbol's. ETH at a multiplier of 10 (BTC at 1): short 15 at 3,000, 5 bought back at 3,010, 10 left marked at 3,020.
#[test]
fn each_row_reads_its_own_instruments_contract_value() {
    let mut core = core_with(two_leg_config(1e12));
    let mut grid = indexmap::IndexMap::new();
    grid.insert("ETHUSDT".to_string(), vike_model::ContractValue::linear(10.0));
    core.engine.account = Account::new(1.0, "sim", Some(grid), BalanceMode::Delta);
    open_both_legs(&mut core);

    let eth = entry_of(&core, "ETHUSDT");
    fill(&mut core, &eth, "ETHUSDT", 1, 5.0, 3_010.0);
    assert_eq!(core.engine.account.realized_pnl, -500.0, "the account: 5 x -10 x 10");
    assert_eq!(core.mount_valuation(0).1, -500.0, "the mount books what the account booked");

    mark(&mut core, 60_000.0, 3_020.0, 25_000);
    let (_, _, unrealized, notional) = core.mount_valuation(0);
    assert_eq!(unrealized, -2_000.0, "10 short x -20 x 10, and BTC flat");
    assert_eq!(notional, 60_000.0 + 10.0 * 3_020.0 * 10.0);
}

/// The un-latch reads EVERY row: the mount's own symbol flat with a leg still open is not flat, and the base the
/// budget counts from afterwards is the net realized PnL of all of them.
#[test]
fn the_unlatch_waits_for_every_row_and_its_base_is_their_sum() {
    let mut core = two_leg_core(1_000.0);
    mark(&mut core, 59_400.0, 3_040.0, 25_000);
    core.drive_due_timers(30_100);
    assert!(core.mount_latched[0], "{:?}", core.recent);
    let unlatch = |core: &mut CoreThread<RecordingClient>| {
        core.dispatch(Ingest::Command(Command::UnlatchMount {
            mount_id: crate::strategy_state::mount_id_with(None, "sim", "BTCUSDT", "1m"),
        }));
    };

    let btc = close_of(&core, "BTCUSDT");
    fill(&mut core, &btc, "BTCUSDT", -1, 1.0, 59_400.0);
    assert_eq!(core.mount_attribution[0].size, 0.0, "the mount's own symbol is flat");
    unlatch(&mut core);
    assert!(core.mount_latched[0], "a leg is still open: {:?}", core.recent);
    assert!(
        core.recent
            .iter()
            .any(|e| e.contains("MOUNT UNLATCH REFUSED") && e.contains("sim/ETHUSDT")),
        "the refusal names the leg: {:?}",
        core.recent
    );

    let eth = close_of(&core, "ETHUSDT");
    fill(&mut core, &eth, "ETHUSDT", 1, 15.0, 3_040.0);
    let live: Vec<String> = core
        .engine
        .registry
        .iter()
        .filter(|(_, mo)| mo.status.is_live())
        .map(|(order_client_id, _)| order_client_id.clone())
        .collect();
    for order_client_id in &live {
        venue_rejects(&mut core, order_client_id);
    }
    unlatch(&mut core);
    assert!(!core.mount_latched[0], "flat on every row: {:?}", core.recent);
    assert_eq!(core.mount_loss_base[0], -1_200.0, "the base is the sum of the rows");
}

/// The rows ride the journal's `Snapshot` and a restart puts them back: the same numbers before and after.
#[test]
fn the_rows_ride_the_snap_across_a_restart() {
    let dir = budget_journal_dir();
    let mut core = core_with(CoreConfig {
        journal: Some(JournalConfig::at(dir.to_path_buf())),
        ..two_leg_config(1e12)
    });
    open_both_legs(&mut core);
    mark(&mut core, 59_400.0, 3_040.0, 25_000);
    let before = core.mount_valuation(0);
    core.write_snap(31_000);
    core.journal.as_mut().expect("journaling is on").flush().expect("flush");
    drop(core);

    let records = vike_journal::Journal::read_all(&dir).expect("the journal reads back");
    let rows = records
        .iter()
        .rev()
        .find_map(|r| match r {
            vike_journal::JournalRecord::Snapshot {
                mount_attribution: mount_attribute, ..
            } => Some(mount_attribute.clone()),
            _ => None,
        })
        .expect("a Snapshot was written");
    assert_eq!(rows.len(), 2, "one row per instrument: {rows:?}");
    assert_eq!(rows[0].symbol.as_deref(), Some("ETHUSDT"), "the leg's row carries its key");
    assert_eq!(rows[0].venue.as_deref(), Some("sim"));
    assert_eq!((rows[1].venue.as_deref(), rows[1].symbol.as_deref()), (None, None), "own last");

    let mut back = core_with(CoreConfig { mount_attribution: rows, ..two_leg_config(1e12) });
    back.engine.extra_symbols = vec!["ETHUSDT".into()];
    mark(&mut back, 59_400.0, 3_040.0, 25_000);
    assert_eq!(back.mount_attribution[0].size, 1.0, "the own row, back");
    assert_eq!(back.mount_valuation(0), before, "the same numbers");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The rows ride the ownership file too, which outranks the journal's and is written per fill.
#[test]
fn the_rows_ride_the_ownership_file_across_a_restart() {
    let dir = crate::scratch::Scratch::reserved("vco-mount-rows");
    let log = || Some(crate::order_owners::OrderOwnerLog::in_state_dir(&dir));
    let mut core = core_with(CoreConfig { order_owners: log(), ..two_leg_config(1e12) });
    open_both_legs(&mut core);
    mark(&mut core, 59_400.0, 3_040.0, 25_000);
    let before = core.mount_valuation(0);
    drop(core);

    let mut back = core_with(CoreConfig { order_owners: log(), ..two_leg_config(1e12) });
    back.engine.extra_symbols = vec!["ETHUSDT".into()];
    mark(&mut back, 59_400.0, 3_040.0, 25_000);
    assert_eq!(back.mount_attribution[0].size, 1.0, "the own row, back");
    assert_eq!(back.mount_valuation(0), before, "the same numbers");
}

/// A LEG ON ANOTHER VENUE is a row of its own, valued on that venue's engine and closed there. Long 1 on sim at
/// 60,000, short 1 on okx at 60,100: the one fold read the second fill as a close, flat with +100 realized, so a
/// basis that widened against the mount was a loss nobody saw.
#[test]
fn a_leg_on_another_venue_is_valued_and_closed_on_its_own_engine() {
    let okx = Engine::new(
        Account::new(1.0, "okx", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "okx",
        "BTC-USDT-SWAP",
    );
    let config = CoreConfig {
        strategy: Some(Mount {
            symbols: vec![MountLeg::at("BTC-USDT-SWAP", "okx")],
            strategy: Box::new(Legs {
                orders: vec![("BTCUSDT", 1, 1.0), ("BTC-USDT-SWAP", -1, 1.0)],
            }),
            ..budget_mount("BTCUSDT")
        }),
        ..two_leg_config(200.0)
    };
    let mut core = core_of(
        crate::runtime::test_support::sim_engine_with(RecordingClient::default()),
        vec![(1_000_000.0, okx)],
        config,
    );
    core.engine.collect_applied_fills = true;
    core.extra_engines[0].1.collect_applied_fills = true;
    core.drive_strategy_tick("sim", "BTCUSDT", 60_000.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let btc = entry_of(&core, "BTCUSDT");
    fill(&mut core, &btc, "BTCUSDT", 1, 1.0, 60_000.0);
    let swap = core.extra_engines[0]
        .1
        .client
        .submissions
        .iter()
        .find(|r| r.symbol == "BTC-USDT-SWAP")
        .map(|r| r.order_client_id.clone())
        .expect("the leg's order went to okx's engine");
    let okx_fill = FillEvent {
        venue: "okx".into(),
        ..order_client_id_fill(&swap, "BTC-USDT-SWAP", -1, 1.0, 60_100.0)
    };
    let (bus, engine) = (&mut core.bus, &mut core.extra_engines[0].1);
    bus.publish(Event::Fill(okx_fill), engine);
    core.dispatch_applied_fills();
    core.arm_boundary_timers(0);

    assert_eq!(core.mount_attribution[0].size, 1.0, "the own row keeps its long");
    assert_eq!(core.mount_valuation(0).1, 0.0, "two opens, nothing closed");

    // The other engine's price is stale: the clock does not judge the mount off half its prices.
    core.engine.price_board.set_mark("sim", "BTCUSDT", 59_900.0, 69_000);
    core.extra_engines[0].1.price_board.set_mark("okx", "BTC-USDT-SWAP", 60_250.0, 1);
    core.drive_due_timers(70_000);
    assert!(!core.mount_latched[0], "a stale leg is not judged: {:?}", core.recent);

    core.extra_engines[0].1.price_board.set_mark("okx", "BTC-USDT-SWAP", 60_250.0, 100_000);
    core.engine.price_board.set_mark("sim", "BTCUSDT", 59_900.0, 100_000);
    let (_, _, unrealized, _) = core.mount_valuation(0);
    assert_eq!(unrealized, -250.0, "100 down on sim, 150 down on okx");
    core.drive_due_timers(101_000);
    assert!(core.mount_latched[0], "250 is past 200: {:?}", core.recent);
    assert_eq!(closes(&core), vec![("BTCUSDT".to_string(), -1, 1.0)], "sim's close on sim");
    let okx_closes: Vec<(String, i32, f64)> = core.extra_engines[0]
        .1
        .client
        .submissions
        .iter()
        .filter(|r| r.reduce_only)
        .map(|r| (r.symbol.clone(), r.side, r.qty))
        .collect();
    assert_eq!(okx_closes, vec![("BTC-USDT-SWAP".to_string(), 1, 1.0)], "okx's close on okx");
}

/// ⚠ **A mount that declares no legs keeps its one row, whatever a fill says, to the bit.** `resolve_intent_symbol`
/// routes such a mount's orders on its own symbol alone (another is refused), so its fills do not name another; this pins that the
/// row split cannot reach the fold, the view or the bytes of the one shape that runs today, even if one did.
#[test]
fn a_mount_with_no_legs_folds_every_fill_into_its_one_row_as_it_always_did() {
    let mut core =
        core_with(CoreConfig { strategy: Some(budget_mount("BTCUSDT")), ..two_leg_config(1e12) });
    core.engine.collect_applied_fills = true;
    core.engine.extra_symbols = vec!["ETHUSDT".into()];
    core.drive_strategy_tick("sim", "BTCUSDT", 60_000.0, 0, |s, ctx| {
        s.on_quote_tick(ctx, &quote_tick())
    });
    let order_client_id = entry_of(&core, "BTCUSDT");
    fill(&mut core, &order_client_id, "BTCUSDT", 1, 1.0, 60_000.0);
    fill(&mut core, &order_client_id, "ETHUSDT", -1, 15.0, 3_000.0);
    let attr = &core.mount_attribution[0];
    assert_eq!(attr.size.to_bits(), (-14.0_f64).to_bits());
    assert_eq!(attr.average_price.to_bits(), 3_000.0_f64.to_bits());
    assert_eq!(attr.realized_pnl.to_bits(), (-57_000.0_f64).to_bits());
    assert!(attr.legs.is_empty(), "one row: {attr:?}");
}
