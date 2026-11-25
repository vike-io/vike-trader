//! A PORTFOLIO run: several strategy files (MOUNTS) over ONE [`OrderBrokerSimulator`], one account, in
//! the live drain's order (`vike_core`'s `drive_strategy`), so a backtest of what runs live is possible.
//!
//! [`StrategyEngineMounts`] is itself a `Strategy<OrderBrokerSimulator>`: the engine runs it as its one
//! strategy and it hands each hook to the mounts it belongs to, the broker's current mount set first
//! (`crates/vike-simulator/src/engine/sim_broker/mounts.rs`'s module doc has what the broker keys by it).
//!
//! - **A bar** goes to every mount that trades its slot, in mount order, each gated by its own
//!   `warmup()` (the composite's is the least of them, so the engine's gate opens with the first). The
//!   fan-out is symbol-major as the run's (`cross_section` order), live's per-bar-event rule.
//! - **A fill and an order end** go to the mount that placed the order alone, its key stripped: the mount
//!   rides the order's `qid` and the tag is keyed by it. A fill no mount minted (`tag` `None`: a
//!   liquidation, a settlement, a protective stop, a combo leg) reaches no mount (decision 0116).
//! - **Every order is judged when it is placed**, through the one gate, so the first mount to place faces
//!   the whole budget, as live's first-come drain does. No simultaneity is claimed.
//! - `on_start`, `on_stop` and `on_schedule` go to every mount in mount order (shortcut: a strategy file
//!   cannot register a schedule, so none is a mount's own), and `failures` answers every mount's together.
//!   The tick hooks are not routed: `run_ticks` refuses a mounted engine. Each mount's reads by NAME are
//!   account reads, as live's are (decision 0048): a strategy sizing on `equity()` sizes on the account's.
//!
//! This module sits at the crate root, NOT under `src/engine/`: `vike-rust-strategy`'s equivalence probe
//! derives the engine's hook roster from the call sites in those files, and the composite's calls are a
//! mount's, not the engine's.

use vike_marketdata::Bar;
use vike_model::{Fill, OrderEvent, Strategy};

use crate::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig};

/// One mount of a portfolio run: its strategy, the run symbols it trades (its own, which `""` names,
/// first) and the budget the live mount states (resolved caps).
pub struct StrategyEngineMount {
    pub strategy: Box<dyn Strategy<OrderBrokerSimulator>>,
    pub symbols: Vec<String>,
    pub budget: Option<vike_execution::MountBudget>,
}

/// One mount's figures at the end of a portfolio run ([`StrategyEngine::mount_reports`]), from its own
/// book: the fills its own orders made, folded by the live ledger's law. The account's record is `run`'s;
/// what it holds beyond the mounts' sum (a liquidation, a settlement, fills no mount minted) is the
/// residual.
#[derive(Debug, Clone)]
pub struct StrategyEngineMountReport {
    /// Each run symbol the mount trades (its own first) with its attributed row there.
    pub rows: Vec<(String, vike_execution::MountAttribution)>,
    /// Its realized net plus unrealized PnL at each of the record's points
    /// (`vike_analytics::TradingRecord::equity_timestamp`).
    pub pnl_curve: Vec<f64>,
    /// Its closed-trade statistics (trades, win percent, ...) when the run armed the report
    /// (`StrategyEngineConfig::closed_trades`), its open lots at their last price.
    pub closed_trades: Option<vike_trade_ledger::TradeClosedMetrics>,
    /// When its loss budget latched it, if it did.
    pub latched_at: Option<i64>,
    /// How many opening orders its leverage cap refused.
    pub leverage_refusals: usize,
}

/// The mounts of a portfolio run as the engine's one strategy (this module's doc).
pub struct StrategyEngineMounts {
    strategies: Vec<Box<dyn Strategy<OrderBrokerSimulator>>>,
    /// Each mount's own `warmup()`, read once.
    warmups: Vec<usize>,
}

impl StrategyEngine<StrategyEngineMounts> {
    /// A portfolio run of `mounts` over `bars_by_symbol` (the run's slots), on one account of `p`.
    /// `run` returns the ACCOUNT's record.
    ///
    /// # Panics
    ///
    /// The last line (the harness refuses each in words first): no mount, a mount with no symbol, a
    /// symbol that is no slot of the run, `p.max_mount_leverage` set (each mount's budget states its
    /// own), `p.decide` simultaneous (the fan-out is the drain's order). `run_ticks` panics on a
    /// mounted engine.
    pub fn new_mounted(
        bars_by_symbol: Vec<(String, Vec<Bar>)>,
        mounts: Vec<StrategyEngineMount>,
        p: StrategyEngineConfig,
    ) -> Self {
        assert!(
            p.max_mount_leverage.is_none(),
            "a portfolio run states each mount's leverage cap in its budget, not the run's"
        );
        assert!(!p.decide.is_simultaneous(), "a portfolio run decides in mount order");
        let mut table = Vec::with_capacity(mounts.len());
        let mut strategies = Vec::with_capacity(mounts.len());
        for m in mounts {
            table.push((m.symbols, m.budget));
            strategies.push(m.strategy);
        }
        let warmups = strategies.iter().map(|s| s.warmup()).collect();
        let composite = StrategyEngineMounts { strategies, warmups };
        let mut engine = StrategyEngine::new(bars_by_symbol, composite, p);
        engine.core.install_mounts(table);
        engine
    }

    /// Each mount's figures so far, in mount order ([`StrategyEngineMountReport`]); after `run`, the
    /// run's.
    pub fn mount_reports(&self) -> Vec<StrategyEngineMountReport> {
        self.core.mount_reports()
    }
}

impl Strategy<OrderBrokerSimulator> for StrategyEngineMounts {
    fn warmup(&self) -> usize {
        self.warmups.iter().copied().min().unwrap_or(0)
    }

    fn on_start(&mut self, broker: &mut OrderBrokerSimulator) {
        for (m, strategy) in self.strategies.iter_mut().enumerate() {
            broker.mount = m;
            strategy.on_start(broker);
        }
    }

    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        let si = broker.dispatching_slot;
        for (m, strategy) in self.strategies.iter_mut().enumerate() {
            if broker.mounts[m].hears[si] && broker.index >= self.warmups[m] {
                broker.mount = m;
                strategy.on_bar(broker, bar);
            }
        }
    }

    fn on_schedule(&mut self, broker: &mut OrderBrokerSimulator, tag: &str) {
        for (m, strategy) in self.strategies.iter_mut().enumerate() {
            if broker.index >= self.warmups[m] {
                broker.mount = m;
                strategy.on_schedule(broker, tag);
            }
        }
    }

    fn on_fill(&mut self, broker: &mut OrderBrokerSimulator, fill: &Fill) {
        let Some((m, tag)) = fill.tag.as_deref().and_then(OrderBrokerSimulator::split_tag_key)
        else {
            return;
        };
        let fill = Fill { tag: (!tag.is_empty()).then(|| tag.to_string()), ..fill.clone() };
        broker.mount = m;
        self.strategies[m].on_fill(broker, &fill);
    }

    fn on_order_event(&mut self, broker: &mut OrderBrokerSimulator, event: &OrderEvent) {
        let Some((m, tag)) = event.tag.as_deref().and_then(OrderBrokerSimulator::split_tag_key)
        else {
            return;
        };
        // an untagged order's end (a release the leverage cap refused) is heard tagless, as its fill is
        let event = OrderEvent { tag: (!tag.is_empty()).then(|| tag.to_string()), ..event.clone() };
        broker.mount = m;
        self.strategies[m].on_order_event(broker, &event);
    }

    fn on_stop(&mut self, broker: &mut OrderBrokerSimulator) {
        for (m, strategy) in self.strategies.iter_mut().enumerate() {
            broker.mount = m;
            strategy.on_stop(broker);
        }
    }

    /// Every mount's failures together. shortcut: the record says how often the account's strategies
    /// failed, not which mount; a per-mount row when a portfolio report needs it.
    fn failures(&self) -> vike_model::StrategyFailures {
        let mut all = vike_model::StrategyFailures::default();
        for strategy in &self.strategies {
            all.merge(&strategy.failures());
        }
        all
    }
}
