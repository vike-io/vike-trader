//! **A PAPER position pays its venue's public funding rate, by the simulator's one formula**, end to end:
//! the live core (`spawn_core`) over the real paper exchange (`ExecutionClientPaper`), the funding law the
//! production roots hand the core (`vike_model::ContractValue::funding`, the backtest's own), and the
//! venue's rate arriving on the core's tick lane as the tradehub's datahub sink sends it
//! (`TickSender::funding_rate`). The entry fills on the paper book at the next bar's open; the venue then
//! states its rate for a settlement and rolls past it, and the engine pays `rate x notional` once.
//!
//! A linear USDT perpetual pays in dollars; an inverse (coin-margined) one pays in its settle coin, which the
//! published block shows valued at the contract's price (decision 0258), so `rate x dollars` as well.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, CoreHandle, LiveBroker, Mount, spawn_core};
use vike_execution::{Account, BalanceMode, Engine, FundingRateUpdate, MarketTick, RiskGate};
use vike_marketdata::{Bar, FundingRate};
use vike_model::{
    AccountTier, ContractValue, Fill, OrderBroker, OrderPlacement, RiskLimits, Strategy,
};
use vike_paper::{ExecutionClientPaper, replay_bars};

const INTERVAL: &str = "1m";
const PX: f64 = 60_000.0;
/// A settlement far ahead of the wall clock (2100-01-01), so only the venue's roll books it, and the next.
const T1: i64 = 4_102_444_800_000;
const T2: i64 = T1 + 8 * 3_600_000;

/// Buys `qty` at market on the first bar, and nothing after.
struct BuyOnce {
    qty: f64,
    sent: bool,
}

impl Strategy<LiveBroker> for BuyOnce {
    fn on_bar(&mut self, ctx: &mut LiveBroker, _bar: &Bar) {
        if !self.sent {
            self.sent = true;
            ctx.place("", "", 1, self.qty, OrderPlacement::Market, false);
        }
    }
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A paper engine on `venue`/`symbol` (inverse when `inverse`) that buys `qty` at `px`, is marked there,
/// and is then told `rate` for [`T1`] and the venue's roll to [`T2`]: the funding the engine's block
/// publishes (dollars) once the settlement is booked, and the paper fills.
fn paid_funding(venue: &str, symbol: &str, inverse: bool, qty: f64, px: f64, rate: f64) -> f64 {
    let client = ExecutionClientPaper::new(venue, symbol, 0.0, 0.0, 0.0);
    let fills: Arc<Mutex<Vec<Fill>>> = Arc::clone(&client.fills);
    let grid = inverse.then(|| {
        let mut g = indexmap::IndexMap::new();
        g.insert(symbol.to_string(), ContractValue::inverse(1.0));
        g
    });
    let mut engine = Engine::new(
        Account::new(1.0, venue, grid, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        venue,
        symbol,
    );
    engine.set_tier(AccountTier::Paper); // what the mount does for a paper account
    let handle: CoreHandle = spawn_core(
        engine,
        CoreConfig {
            seed_cash: 10_000.0,
            funding_law: Some(ContractValue::funding),
            strategy: Some(Mount {
                order_client_id_code: None,
                account: None,
                symbols: Vec::new(),
                controller_id: None,
                underlying_symbol: None,
                venue: venue.to_string(),
                symbol: symbol.to_string(),
                interval: INTERVAL.to_string(),
                strategy: Box::new(BuyOnce { qty, sent: false }),
            }),
            ..CoreConfig::default()
        },
    );
    let cell = handle.snapshot_cell();
    // The venue's mark first (an inverse engine values its coin at the contract's price), then the bars.
    handle.market_sender().publish(MarketTick {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        price: px,
        timestamp: 1,
    });
    let b = |i: i64| vike_simulator::test_support::bar(60_000 * i, px, px, px, px, 10.0);
    replay_bars(&handle.bar_sender(), venue, symbol, INTERVAL, &[b(1), b(2), b(3)]);
    wait_for("the paper entry", || fills.lock().unwrap().len() == 1);
    wait_for("the position", || {
        cell.load_full().positions.iter().any(|p| p.symbol == symbol && p.size == qty)
    });
    let funding = |next_ts: i64| FundingRateUpdate {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        funding: FundingRate {
            timestamp: 1,
            symbol: symbol.to_string(),
            rate,
            interval_ms: Some(28_800_000),
            next_timestamp: Some(next_ts),
            predicted: None,
            price: None,
        },
    };
    let ticks = handle.tick_sender();
    ticks.funding_rate(funding(T1)).expect("core up");
    ticks.funding_rate(funding(T2)).expect("core up"); // the venue rolled past T1
    ticks.funding_rate(funding(T2)).expect("core up"); // and says so again: booked once
    let block = |c: &vike_execution::CoreSnapshot| {
        c.portfolio.venues.iter().find(|v| v.venue == venue).map(|v| v.funding_paid)
    };
    wait_for("the settlement", || block(&cell.load_full()).is_some_and(|f| f != 0.0));
    std::thread::sleep(Duration::from_millis(200)); // a second booking would land by now
    let paid = block(&cell.load_full()).expect("the venue block");
    handle.shutdown_and_join();
    assert_eq!(fills.lock().unwrap().len(), 1, "the funding sent no order");
    paid
}

/// **A paper long BTC perpetual crossing one settlement at rate r pays r x notional ONCE** (1 contract at
/// 60,000, r = 0.01%: 6 dollars), by the simulator's formula.
#[test]
fn a_paper_long_usdt_perpetual_pays_rate_times_notional_once() {
    let paid = paid_funding("binance", "BTCUSDT.P", false, 1.0, PX, 0.0001);
    let want = ContractValue::linear(1.0).funding(1.0, PX, 0.0001);
    assert!((paid + want).abs() < 1e-9, "paid {paid}, want -{want}");
    assert!((want - 6.0).abs() < 1e-9, "rate x notional: {want}");
}

/// **An inverse (coin-margined) paper perpetual pays in its settle coin**: 1,000 one-dollar contracts at r =
/// 0.01% owe 0.1 dollars, booked as 0.1 / 60,000 BTC and published valued at the contract's price.
#[test]
fn a_paper_inverse_perpetual_pays_in_its_settle_coin() {
    let paid = paid_funding("deribit", "BTC-PERPETUAL", true, 1_000.0, PX, 0.0001);
    let coin = ContractValue::inverse(1.0).funding(1_000.0, PX, 0.0001);
    assert!((coin - 0.1 / PX).abs() < 1e-15, "the formula's coin amount: {coin}");
    assert!((paid + coin * PX).abs() < 1e-9, "published {paid}, the coin {coin} at {PX}");
}

/// **The paper charge equals a REAL settlement**: the binance demo account's `FUNDING_FEE` row of
/// 2026-10-24 16:00 UTC (`time` 1792857600000, `income` -0.00797482 USDT, recorded by the sibling funding
/// work as `funding_income_demo.json`) was a long of 0.001 BTCUSDT at that settlement's demo `fundingRate`
/// 0.0001 and `markPrice` 79,748.2 (`GET demo-fapi.binance.com/fapi/v1/fundingRate`, public). The same
/// position on a paper engine, told the same rate and mark, pays the same amount to the venue's 8 decimals.
#[test]
fn a_paper_long_pays_what_the_binance_demo_charged_for_the_same_settlement() {
    let paid = paid_funding("binance", "BTCUSDT.P", false, 0.001, 79_748.2, 0.0001);
    assert!((paid - -0.00797482).abs() < 5e-9, "paid {paid}, the venue charged -0.00797482");
}
