//! **When a multi-symbol step is decided** — [`DecideMode`], the profile's `engine.decide` words
//! (`sequential`, `simultaneous`) and the cross-section order each one walks.
//!
//! Here, below the CLI (which forwards `--decide`), the profile resolver
//! (`crates/vike-backtest/src/run/profile.rs`'s `decide_mode`) and the simulator that walks the
//! order, so the three read one roster instead of spelling the words each. What a mode does to a
//! fill stays the simulator's (`vike_simulator::StrategyEngineConfig::decide`).

vike_utility::word_enum! {
    /// WHEN a multi-symbol step is decided: one symbol at a time in the order the caller listed
    /// them, or every symbol against one shared account state
    /// (`vike_simulator::StrategyEngineConfig::decide`).
    ///
    /// # The defect this type names: the symbol list is a hidden strategy parameter
    ///
    /// `vike_simulator::StrategyEngine::run` fans `Strategy::on_bar` out over `symbols` in the
    /// order the caller handed them in, and three of the ENGINE's own reads are a function of that
    /// order rather than of the SET of instruments. A run's answer is supposed to be a function of
    /// the universe, the tape and the strategy; each of these makes it a function of a fourth input
    /// nobody chose, no profile records and no report shows:
    ///
    /// 1. **The shared-cash allocator's tie-break.** `StrategyEngine::fill_step_gated` already does
    ///    the hard half: it collects the whole step's triggered orders across every symbol, fills
    ///    the reductions first (freeing cash), and only then ranks the opens against the shared
    ///    pool. But it ranks on [`crate::OrderResting::weight`] and breaks a tie on COLLECTION
    ///    order, which is symbol-list order — and **nothing in this tree ever sets `weight`**: an
    ///    untagged [`crate::OrderBroker::place`] passes `0.0`, and so does
    ///    `OrderBrokerSimulator::strategy_order_target_percent`. So every order ties, and the
    ///    allocator's entire verdict — who gets the last of the cash and the last open slot — is
    ///    the argv order. [`Self::Simultaneous`] replaces that tie-break with the symbol's own
    ///    name.
    /// 2. **Every cross-sectional fold.** `OrderBrokerSimulator::equity_now`, `open_risk`,
    ///    `margin_in_use_pending_aware` and `cap_to_leverage` all sum with
    ///    `vike_math::summation::compensated_sum` over `0..symbols.len()`. f64 addition is not
    ///    associative and Neumaier compensation does not make it so, so permuting the list moves
    ///    `equity_now` by an ULP — which moves [`crate::ContractValue::units_worth`], which can
    ///    move a fill by a whole lot or a whole tick. **This is the one that bites the DEFAULT
    ///    configuration**: with no `[risk]` table, no `leverage` and no position caps, the
    ///    non-gated fill lane gates nothing at all, so the fold order is the *only* order
    ///    dependency a plain portfolio run has.
    /// 3. **The pre-trade gate's pending-aware budget.**
    ///    `OrderBrokerSimulator::margin_in_use_pending_aware` and the exposure fold in
    ///    `OrderBrokerSimulator::gate_order` sum the `pending` set, which ACCUMULATES during the
    ///    `on_bar` fan-out: the symbol judged first faces the step's full budget and every later
    ///    symbol faces what is left of it. That term is not a bug — its own doc argues why a
    ///    pre-trade cap must model the local view — but it means the DECISION, not merely the
    ///    allocation, is taken in list order. Closing it needs a step-boundary budget snapshot,
    ///    which is **not built**; [`Self::Simultaneous`] is therefore REFUSED at load beside an
    ///    armed `[risk]` table or `engine.leverage` rather than silently claiming a cross-section
    ///    it did not compute (`ProfileBacktest::refusals`).
    ///
    /// # Why the cure is a canonical ORDER rather than a joint solve
    ///
    /// A genuinely joint allocation — pro-rata scaling every open by one factor so the step's
    /// demand fits the pool — is a different and larger design, and it cannot express an
    /// indivisible resource: `max_open_positions` is a slot count, and half a slot is not a thing.
    /// What every order dependency above has in common is narrower and fixable: the walk is indexed
    /// by LIST POSITION, an accident of how the universe was typed. Ordering the cross-section by
    /// the instrument's own identity makes each of them a property of the SET, so permuting
    /// `data.symbols` cannot move the answer — which is exactly the property a portfolio run is
    /// supposed to have.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub enum DecideMode {
        /// One symbol at a time, in the order the caller listed them — the frozen behaviour, and
        /// **byte-identical to the engine before this type existed**: `cross_section_order`
        /// returns the identity permutation, so every fold visits the same indices in the same
        /// sequence and sums to the same bits, and the allocator keeps its collection-order
        /// tie-break.
        ///
        /// It is named rather than left implicit because an unnamed default cannot be reported.
        /// The list-order dependency was invisible in every profile and every report; a run that
        /// carries `decide = "sequential"` says out loud which of the two answers it computed.
        #[default]
        Sequential => "sequential",
        /// One decision over the whole cross-section: every fold and the shared-cash allocator walk
        /// the symbols in a canonical order derived from their own names, so no permutation of
        /// `data.symbols` can change the result.
        ///
        /// ⚠ It **implies the shared-cash admission phase**
        /// (`vike_simulator::StrategyEngineConfig::cash_gate`) and therefore inherits both of that
        /// mode's consequences: the step routes through `StrategyEngine::fill_step_gated` instead
        /// of the per-symbol `fill_pending` loop, and the granular sub-bar lane is unconditionally
        /// disabled. That is not a coupling of convenience — without an allocator there is no
        /// cross-section to order. The per-symbol lane consumes the position caps in list order
        /// inside `StrategyEngine::dispatch_fill` and has no place where a ranking could be applied
        /// at all, so a "simultaneous" mode on that lane would be a name with nothing behind it.
        Simultaneous => "simultaneous",
    }
}

impl DecideMode {
    /// `true` for [`Self::Simultaneous`] — the one branch every consumer needs, spelled once so a
    /// third variant cannot silently read as sequential at some call site that matched on
    /// `Simultaneous` alone.
    #[must_use]
    pub fn is_simultaneous(self) -> bool {
        matches!(self, DecideMode::Simultaneous)
    }

    /// The order every cross-sectional walk takes over `symbols`, as indices INTO `symbols`.
    ///
    /// * [`Self::Sequential`] ⇒ the identity permutation `0..symbols.len()`. Load-bearing: it is
    ///   what makes the default path byte-identical rather than merely equivalent — the folds
    ///   visit the same indices in the same sequence, so the f64 sums are bit-for-bit the frozen
    ///   ones and no parity fixture moves.
    /// * [`Self::Simultaneous`] ⇒ indices sorted by the symbol's own name, ascending, with the
    ///   list position as the tie-break for a duplicated name.
    ///
    /// ⚠ **Sorting by NAME is arbitrary, and that is the point — it is arbitrary in a way the
    /// caller cannot influence.** Any total order intrinsic to the instrument would do; what
    /// matters is that it is not the list position, because the list position is the one input a
    /// portfolio run's answer must not depend on. A duplicated symbol falls back to list position,
    /// spelled as an EXPLICIT `.then(a.cmp(b))` rather than left to `sort_by`'s stability — a total
    /// comparator is what makes the result a function of the input and not of the sort
    /// implementation. A duplicate universe is already degenerate (two slots of one instrument
    /// sharing one price series), so there is nothing better to order it by, and taking the
    /// earlier slot at least keeps the answer a permutation, which
    /// `StrategyEngine::fill_step_gated`'s rank inversion relies on.
    #[must_use]
    pub fn cross_section_order(self, symbols: &[String]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..symbols.len()).collect();
        if self.is_simultaneous() {
            order.sort_by(|a, b| symbols[*a].cmp(&symbols[*b]).then(a.cmp(b)));
        }
        order
    }
}

#[cfg(test)]
mod tests {
    use super::DecideMode;

    /// Sequential is the identity and the default; simultaneous orders by name, a duplicated name
    /// by list position, and both answer a permutation.
    #[test]
    fn the_cross_section_order_of_each_mode() {
        let symbols: Vec<String> =
            ["ETHUSDT", "BTCUSDT", "SOLUSDT"].iter().map(|s| (*s).to_string()).collect();
        assert_eq!(DecideMode::default(), DecideMode::Sequential);
        assert_eq!(DecideMode::Sequential.cross_section_order(&symbols), vec![0, 1, 2]);
        assert_eq!(DecideMode::Simultaneous.cross_section_order(&symbols), vec![1, 0, 2]);
        let duplicated: Vec<String> =
            ["BTCUSDT", "AAA", "BTCUSDT"].iter().map(|s| (*s).to_string()).collect();
        assert_eq!(DecideMode::Simultaneous.cross_section_order(&duplicated), vec![1, 0, 2]);
    }
}
