//! The `[[mounts]]` array: a PORTFOLIO backtest, several strategy files over the run's ONE simulated
//! account, each a mount dispatched in list order as the live drain dispatches a node's mounts.
//! `ProfileMount` is one entry; the accessors every door reads a profile's strategies through; the
//! load-time rules (`ProfileBacktest::mount_refusals`, which `validate` calls once) and the run-time
//! ones over the files a door read and the strategies a run built (`ProfileBacktest::mount_laws`,
//! `ProfileBacktest::mount_costs`, `ProfileBacktest::mount_inputs`).
//!
//! A profile names exactly one of `[strategy]` and `[[mounts]]`. Several mounts may name one slot of
//! the data slice: each mount's position in it is the fills attributed to that mount, and the account
//! nets them, as live does.

use serde::Deserialize;

use super::{ProfileBacktest, ProfileStrategy, decide_mode};
use crate::HarnessError;
use crate::run::refusals::ProfileRefusal;
use crate::run::tick_replay::ProfileDataSeries;
use vike_model::run_request::replay::ReplayKind;
use vike_simulator::OrderBrokerSimulator;
use vike_strategy::file::scan::StrategyFileLanguage;

/// One `[[mounts]]` entry: a strategy file trading `symbols`, slots of the run's one data slice, under
/// the live mount's two budget arms (`vike_execution::MountBudget`).
///
/// ```toml
/// [[mounts]]
/// symbols = ["BTCUSDT.P", "ETHUSDT.P"]
/// max_leverage = 2.0     # narrows engine.max_mount_leverage for this mount only
/// max_loss = 1500.0      # the loss latch (decision 0249), in the run's ledger currency
/// [mounts.strategy]      # the [strategy] table's own keys: file, params, sdk
/// file = "trend/lube.rs"
/// ```
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "profile-schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProfileMount {
    /// The slots of the data slice this mount trades, each as the slice names it: a symbol, or a
    /// series' leg `SYMBOL@account` (decision 0293). The first is the mount's own symbol. Several
    /// mounts may name one slot.
    pub symbols: Vec<String>,
    /// This mount's leverage cap: its gross notional over the account's sizing equity, at least 1.
    /// Absent: `engine.max_mount_leverage`, every mount's default; stated, it may only narrow that
    /// figure (a higher one is refused, never clamped).
    #[serde(default)]
    pub max_leverage: Option<f64>,
    /// This mount's loss budget (decision 0249's latch), in the run's ledger currency: once its own
    /// PnL loses this much on a closed bar, its orders are cancelled and its position flattened.
    /// Absent: no budget.
    #[serde(default)]
    pub max_loss: Option<f64>,
    /// The strategy this mount runs: the `[strategy]` table's own keys.
    pub strategy: ProfileStrategy,
}

impl ProfileBacktest {
    /// Whether this profile is a portfolio: `[[mounts]]` names at least one mount.
    pub fn is_portfolio(&self) -> bool {
        !self.mounts.is_empty()
    }

    /// **The run's slots in the order its engine holds them**: `ProfileData::resolved_series`', and a portfolio's
    /// by first appearance in mount order (a stable sort: every slot is some mount's). The ONE order a portfolio's
    /// bars (`crate::run::dispatch`'s `mounts::in_mount_order`), its equity log and the rebuild of its curve
    /// (`crate::run::equity_log`) read the slots in, so a saved curve rebuilds whatever order `[data]` lists them in.
    pub(crate) fn slot_series(&self) -> Vec<ProfileDataSeries> {
        let mut series = self.data.resolved_series();
        let named = || self.mounts.iter().flat_map(|m| &m.symbols);
        series.sort_by_cached_key(|s| {
            let slot = s.leg_name();
            named().position(|n| *n == slot).unwrap_or(usize::MAX)
        });
        series
    }

    /// Every strategy table the profile runs, in mount order: `[strategy]`, or each mount's. A door
    /// that reads or builds every file a run needs walks these.
    pub fn strategies(&self) -> impl Iterator<Item = &ProfileStrategy> {
        self.strategy.iter().chain(self.mounts.iter().map(|m| &m.strategy))
    }

    /// [`Self::strategies`], to set what a door owns on each (its read text, its builder).
    pub fn strategies_mut(&mut self) -> impl Iterator<Item = &mut ProfileStrategy> {
        self.strategy.iter_mut().chain(self.mounts.iter_mut().map(|m| &mut m.strategy))
    }

    /// The one `[strategy]` of a route that runs one strategy (a walk-forward, a search, a permutation
    /// test, a Studio slice), or that route's refusal of a portfolio in words.
    pub fn one_strategy(&self) -> Result<&ProfileStrategy, HarnessError> {
        self.strategy.as_ref().ok_or(HarnessError::from(ProfileRefusal::MountsRouteRunsOneStrategy))
    }

    /// The table whose file's header laws (`fill`, `optimistic_quotes`) and costs the run takes:
    /// `[strategy]`, else the first mount's, which every other mount must agree with
    /// ([`Self::mount_laws`], [`Self::mount_costs`]). `None` only for a profile that names neither, which
    /// `validate` refuses.
    pub fn law_strategy(&self) -> Option<&ProfileStrategy> {
        self.strategies().next()
    }

    /// **The strategy code a result records** as `(file, sha256, built SDK)`: `[strategy]`'s own, and for
    /// a portfolio the mounts' files joined `" + "`, the sha256 over each mount's `file\tsha256\n` in
    /// mount order, and no SDK (each mount's row in the report carries its own). Empty where no door read
    /// a file.
    pub fn strategy_identity(&self) -> (String, String, Option<u32>) {
        if let Some(strategy) = &self.strategy {
            return (strategy.file.clone(), strategy.sha256().to_string(), strategy.built_sdk());
        }
        let files: Vec<&str> = self.mounts.iter().map(|m| m.strategy.file.as_str()).collect();
        let listed: String =
            self.strategies().map(|s| format!("{}\t{}\n", s.file, s.sha256())).collect();
        let read = self.strategies().all(|s| !s.sha256().is_empty()) && !self.mounts.is_empty();
        let sha256 = if read {
            vike_hub_protocol::hash::sha256_hex(listed.as_bytes())
        } else {
            String::new()
        };
        (files.join(" + "), sha256, None)
    }

    /// The load-time rules of `[strategy]` and `[[mounts]]` that read more than one table, in the order
    /// [`Self::validate`] reports them (its `refusals` calls this once). `slice_ok` is whether the data
    /// slice itself passed, so the slot rules are asked only of a slice whose slots are known.
    pub(super) fn mount_refusals(&self, slice_ok: bool) -> Vec<(&'static str, HarnessError)> {
        let mut out: Vec<(&'static str, HarnessError)> = Vec::new();
        match (&self.strategy, self.is_portfolio()) {
            (None, false) => {
                out.push(("strategy", HarnessError::from(ProfileRefusal::StrategyOrMountsMissing)))
            }
            (Some(_), true) => {
                out.push(("strategy", HarnessError::from(ProfileRefusal::StrategyBesideMounts)))
            }
            _ => {}
        }
        if !self.is_portfolio() {
            return out;
        }
        for (i, mount) in self.mounts.iter().enumerate() {
            if let Err(e) = mount.strategy.language() {
                // The carrier forwards the bare sentence (`mounts[i].<sentence>`): the id shown is the carrier's.
                let refusal = match e {
                    HarnessError::Validation(refusal) => refusal.to_string(),
                    other => other.message(),
                };
                let e =
                    HarnessError::from(ProfileRefusal::MountsStrategyFileRefused { i, refusal });
                out.push(("mounts.strategy.file", e));
            }
            if let Some(sdk) = mount.strategy.sdk
                && matches!(mount.strategy.language(), Ok(StrategyFileLanguage::Rhai))
            {
                let e = HarnessError::from(ProfileRefusal::MountsStrategySdkBesideRhaiFile {
                    i,
                    sdk,
                    file: mount.strategy.file.clone(),
                });
                out.push(("mounts.strategy.sdk", e));
            }
            if mount.symbols.is_empty() {
                let e = HarnessError::from(ProfileRefusal::MountsSymbolsEmpty { i });
                out.push(("mounts.symbols", e));
            }
            for (at, symbol) in mount.symbols.iter().enumerate() {
                if mount.symbols[..at].contains(symbol) {
                    let e = HarnessError::from(ProfileRefusal::MountsSymbolsRepeated {
                        i,
                        symbol: symbol.clone(),
                    });
                    out.push(("mounts.symbols", e));
                }
            }
            // The judge reads anything else as no cap (`vike_execution::risk::mount_leverage_refusal`).
            if let Some(cap) = mount.max_leverage {
                if !(cap.is_finite() && cap >= 1.0) {
                    let e =
                        HarnessError::from(ProfileRefusal::MountsMaxLeverageBelowOneOrNotFinite {
                            i,
                            cap,
                        });
                    out.push(("mounts.max_leverage", e));
                } else if let Some(engine) = self.engine.max_mount_leverage
                    && cap > engine
                {
                    // A mount figure only narrows the run's (decision 0181): refused, never clamped.
                    let e = HarnessError::from(ProfileRefusal::MountsMaxLeverageAboveEngine {
                        i,
                        cap,
                        engine,
                    });
                    out.push(("mounts.max_leverage", e));
                }
            }
            // `MountBudget::is_active` reads anything else as no budget.
            if let Some(loss) = mount.max_loss
                && !(loss.is_finite() && loss > 0.0)
            {
                let e = HarnessError::from(ProfileRefusal::MountsMaxLossNotPositiveOrNotFinite {
                    i,
                    loss,
                });
                out.push(("mounts.max_loss", e));
            }
        }
        // The slot rules read the slice's slots, so they are asked only of a slice that passed.
        if slice_ok {
            let slots: Vec<String> =
                self.data.resolved_series().iter().map(|s| s.leg_name()).collect();
            for (i, mount) in self.mounts.iter().enumerate() {
                for symbol in mount.symbols.iter().filter(|s| !slots.contains(s)) {
                    let e = HarnessError::from(ProfileRefusal::MountsSymbolsNotInDataSlice {
                        i,
                        symbol: symbol.clone(),
                        slots: slots.join(", "),
                    });
                    out.push(("mounts.symbols", e));
                }
            }
            for slot in &slots {
                if !self.mounts.iter().any(|m| m.symbols.contains(slot)) {
                    let e = HarnessError::from(ProfileRefusal::MountsSlotTradedByNoMount {
                        slot: slot.clone(),
                    });
                    out.push(("mounts", e));
                }
            }
        }
        // Each route below runs ONE strategy: accepting mounts there would run something else.
        if self.walkforward.is_some() {
            let e = HarnessError::from(ProfileRefusal::MountsBesideWalkforward);
            out.push(("mounts", e));
        }
        if self.is_paramscan() {
            let e = HarnessError::from(ProfileRefusal::MountsBesideParamscan);
            out.push(("mounts", e));
        }
        if self.permutation.is_some() {
            let e = HarnessError::from(ProfileRefusal::MountsBesidePermutation);
            out.push(("mounts", e));
        }
        if decide_mode(self.engine.decide.as_deref()).unwrap_or_default().is_simultaneous() {
            let e = HarnessError::from(ProfileRefusal::MountsBesideSimultaneousDecide);
            out.push(("mounts", e));
        }
        if self.data.kind == ReplayKind::Tick {
            let e = HarnessError::from(ProfileRefusal::MountsOnTickLane);
            out.push(("mounts", e));
        }
        if self.mounts.len() > 1 && self.engine.in_bar.is_some_and(|r| r.fill_at_close) {
            let e = HarnessError::from(ProfileRefusal::EngineInBarFillAtCloseWithMounts);
            out.push(("engine.in_bar", e));
        }
        if self.mounts.len() > 1 && self.engine.in_bar.is_some_and(|r| r.committed_stops) {
            let e = HarnessError::from(ProfileRefusal::EngineInBarCommittedStopsWithMounts);
            out.push(("engine.in_bar", e));
        }
        if !self.engine.closed_trades {
            let e = HarnessError::from(ProfileRefusal::EngineClosedTradesOffForPortfolio);
            out.push(("engine.closed_trades", e));
        }
        out
    }

    /// **The header laws every mount must agree on** (one run has one): the `fill` and
    /// `optimistic_quotes` of each mount's FILE, read by the door. A `fill = "close"` file beside
    /// another mount is refused for the reason `[engine.in_bar] fill_at_close` is ([`Self::mount_refusals`]).
    /// `Ok` for a profile that is no portfolio.
    pub(crate) fn mount_laws(&self) -> Result<(), HarnessError> {
        let law = |s: &ProfileStrategy| {
            s.source.as_ref().map_or((false, false), |s| (s.optimistic_quotes, s.fill_at_close))
        };
        let mut laws = self.mounts.iter().map(|m| (m, law(&m.strategy)));
        let Some((first, (first_quotes, _))) = laws.next() else { return Ok(()) };
        if self.mounts.len() > 1
            && let Some(close) = self.mounts.iter().find(|m| law(&m.strategy).1)
        {
            return Err(HarnessError::from(ProfileRefusal::MountsFileFillsAtClose {
                file: close.strategy.file.clone(),
            }));
        }
        // A stated `[engine.quotes]` is every mount's law, whatever the headers say (`optimistic_quotes`).
        if self.engine.quotes.is_none()
            && let Some((other, _)) = laws.find(|(_, (quotes, _))| *quotes != first_quotes)
        {
            return Err(HarnessError::from(ProfileRefusal::MountsOptimisticQuotesDiffer {
                first: first.strategy.file.clone(),
                other: other.strategy.file.clone(),
            }));
        }
        Ok(())
    }

    /// **The one commission and slippage every mount must charge** (R1): each mount's
    /// (`crate::run::costs::costs_of_built`, off the strategy the run built for it, in mount order),
    /// which must be the same amounts, or the run is refused naming two mounts that differ.
    pub(crate) fn mount_costs(
        &self,
        built: &[Box<dyn vike_model::Strategy<OrderBrokerSimulator>>],
    ) -> Result<vike_model::Costs, HarnessError> {
        let mut costs = self.mounts.iter().zip(built).map(|(mount, strategy)| {
            crate::run::costs::costs_of_table(&mount.strategy, strategy.as_ref())
                .map(|c| (mount, c))
        });
        let Some(first) = costs.next() else {
            return Err(HarnessError::from(ProfileRefusal::MountsCostsAskedWithoutMount));
        };
        let (first_mount, first) = first?;
        for next in costs {
            let (mount, cost) = next?;
            if cost.commission != first.commission || cost.slippage != first.slippage {
                return Err(HarnessError::from(ProfileRefusal::MountsCostsDiffer {
                    first: first_mount.strategy.file.clone(),
                    other: mount.strategy.file.clone(),
                }));
            }
        }
        Ok(first)
    }

    /// **The series a portfolio reads beside its bars** (R3, decision 0271): every mount's declared
    /// inputs in mount order, each once, and whether they are read WHOLE (`input_whole`: only when every
    /// mount's file is `runs_on = "backtest"`). Mounts that differ there, with any input declared, are
    /// refused: one run reads its inputs one way.
    pub(crate) fn mount_inputs(
        &self,
        built: &[Box<dyn vike_model::Strategy<OrderBrokerSimulator>>],
    ) -> Result<(Vec<String>, bool), HarnessError> {
        let declared = each_once(built.iter().flat_map(|s| s.inputs()));
        let whole = |m: &&ProfileMount| m.strategy.source.as_ref().is_some_and(|s| s.backtest_only);
        let every = self.mounts.iter().all(|m| whole(&m));
        if !every
            && !declared.is_empty()
            && let Some(backtest_only) = self.mounts.iter().find(whole)
        {
            return Err(HarnessError::from(ProfileRefusal::MountsInputReadsDiffer {
                file: backtest_only.strategy.file.clone(),
            }));
        }
        Ok((declared, every))
    }

    /// Every input the profile's strategies declare (decision 0271), in mount order, each once: what a
    /// door that plans without running asks, building each strategy to ask it.
    ///
    /// # Errors
    /// The first strategy's own build refusal.
    pub fn declared_input_names(&self) -> Result<Vec<String>, HarnessError> {
        let mut names = Vec::new();
        for strategy in self.strategies() {
            names.extend(strategy.build()?.inputs());
        }
        Ok(each_once(names))
    }
}

/// `names` in their order, each kept once.
fn each_once(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// **`[strategy]` beside a non-empty `[[mounts]]`, refused before the typed read** (so a `[strategy.params]`
/// table alone does not meet serde's "missing field `file`"): the CLI's `--preset`, `--script`, `--param`
/// and `--strategy` flags, `--set strategy.…` and the MCP `script` argument all write `[strategy]`, so a
/// portfolio's strategy is edited in its mount's own table.
pub(super) fn refuse_strategy_beside_mounts(doc: &toml::Table) -> Result<(), HarnessError> {
    let mounts = doc.get("mounts").and_then(toml::Value::as_array).is_some_and(|m| !m.is_empty());
    if mounts && doc.contains_key("strategy") {
        return Err(HarnessError::from(ProfileRefusal::MountsProfileNamesNoStrategyTable));
    }
    Ok(())
}
