//! `SimBroker` — the backtest fill engine plus its per-symbol state and cost/params types.
//! `engine.rs` re-exports the public types, and the crate root names them as
//! `vike_sim::{SimBroker, EngineParams, …}` — their one public spelling, since `engine` is sealed
//! `pub(crate)` (docs/decisions/0087).
//!
//! ⚠ Two panics in this file's neighbourhood are deliberately NOT addressed by the `bars_for`
//! change: `SimBroker::idx` panics on an unknown symbol, and
//! `crates/vike-sim/src/bar_buffer.rs`'s `BarSeriesBuffer` carries its own copy of the same
//! parse-and-lookup pair. `BarSeriesBuffer` is `pub use`d from the crate root and has ZERO
//! constructors in this repository, so it is a public divergent copy rather than a live path.

mod fills;
mod intake;
mod knobs;
mod lookups;
mod params;
mod sizing_gate;
mod slippage;
mod state;
mod traits;
mod waits;

pub use knobs::{DEFAULT_IMPACT_WINDOW, DecideMode, EquitySampling, FillModelKind};
pub use params::{EngineParams, OptionExpirySource, ResolutionSource};
pub(super) use state::SymbolState;
pub use state::{MirrorFill, MirrorFunding, SettlementFill, SimBroker, VariationSettlement};

/// `bars_for`/`forming_for` have zero callers anywhere in this tree (`SimBroker` implements only
/// `Broker`/`HftBroker`, neither of which declares them, and no Rhai surface binds them either) —
/// this module carries no other test double for `SimBroker`, so these two build one through the
/// real constructor path (`StrategyEngine::new(..).core`) with a no-op strategy, matching the
/// benches' `NoopStrategy` idiom.
#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::Strategy;

    struct NoopStrategy;
    impl Strategy<SimBroker> for NoopStrategy {}

    fn broker(p: EngineParams) -> SimBroker {
        crate::StrategyEngine::new(vec![("BTCUSDT".to_string(), Vec::new())], NoopStrategy, p).core
    }

    #[test]
    fn an_unregistered_timeframe_is_none_not_a_panic() {
        let p = EngineParams { timeframes: vec!["4h".to_string()], ..Default::default() };
        let broker = broker(p);
        assert!(broker.bars_for("BTCUSDT", "1d").is_none());
        assert!(broker.forming_for("BTCUSDT", "1d").is_none());
    }

    #[test]
    fn a_registered_timeframe_still_answers() {
        let p = EngineParams { timeframes: vec!["4h".to_string()], ..Default::default() };
        let broker = broker(p);
        assert!(broker.bars_for("BTCUSDT", "4h").is_some());
    }

    fn universe() -> Vec<String> {
        ["ETHUSDT", "BTCUSDT", "SOLUSDT"].iter().map(|s| (*s).to_string()).collect()
    }

    /// The DEFAULT mode's order is the IDENTITY, and that is not a detail: it is the whole reason
    /// routing every cross-sectional fold through `SimBroker::cross_section` cannot move a
    /// parity fixture. Same indices, same sequence, same f64 bits.
    #[test]
    fn sequential_is_the_identity_permutation() {
        let syms = universe();
        assert_eq!(DecideMode::Sequential.cross_section_order(&syms), vec![0, 1, 2]);
        assert_eq!(DecideMode::default(), DecideMode::Sequential);
    }

    /// The simultaneous order is derived from the symbols' own NAMES, so it is a function of the
    /// instrument SET — which is what makes every consumer of it (the folds, the allocator's
    /// tie-break, the `on_bar` fan-out) blind to how the universe was typed.
    #[test]
    fn simultaneous_orders_by_name_not_by_list_position() {
        let syms = universe();
        // BTCUSDT(1) < ETHUSDT(0) < SOLUSDT(2)
        assert_eq!(DecideMode::Simultaneous.cross_section_order(&syms), vec![1, 0, 2]);
    }

    /// THE PROPERTY: two permutations of one universe induce the same VISITING SEQUENCE of
    /// symbols. Asserted on the names rather than on the index vectors, because the indices are
    /// necessarily different — that difference is exactly what the mode is absorbing.
    #[test]
    fn a_permuted_universe_is_visited_in_the_same_sequence() {
        let forward: Vec<String> = ["AAA", "BBB", "CCC"].iter().map(|s| (*s).to_string()).collect();
        let reversed: Vec<String> =
            ["CCC", "BBB", "AAA"].iter().map(|s| (*s).to_string()).collect();
        let seq = |syms: &[String]| -> Vec<String> {
            DecideMode::Simultaneous
                .cross_section_order(syms)
                .iter()
                .map(|&i| syms[i].clone())
                .collect()
        };
        assert_eq!(seq(&forward), seq(&reversed));
        assert_eq!(seq(&forward), vec!["AAA", "BBB", "CCC"]);
        // ...and the SEQUENTIAL order is not invariant — the defect, so the assertion above is
        // known to be testing something.
        let seq_seq = |syms: &[String]| -> Vec<String> {
            DecideMode::Sequential
                .cross_section_order(syms)
                .iter()
                .map(|&i| syms[i].clone())
                .collect()
        };
        assert_ne!(seq_seq(&forward), seq_seq(&reversed));
    }

    /// Whatever the mode, the answer is a PERMUTATION of `0..n` — every symbol visited exactly
    /// once. `StrategyEngine::fill_step_gated` inverts it into a rank table on that assumption
    /// (`r[si] = pos` over every entry, with no fill value left standing), and a duplicated or
    /// missing index there would silently give two symbols the same allocation priority.
    #[test]
    fn every_mode_yields_a_permutation_even_with_a_duplicated_symbol() {
        let dup: Vec<String> =
            ["BTCUSDT", "AAA", "BTCUSDT"].iter().map(|s| (*s).to_string()).collect();
        for mode in [DecideMode::Sequential, DecideMode::Simultaneous] {
            let mut order = mode.cross_section_order(&dup);
            assert_eq!(order.len(), dup.len());
            order.sort_unstable();
            assert_eq!(order, vec![0, 1, 2], "{mode:?} did not yield a permutation");
        }
        // The duplicate keeps LIST position as its tie-break, so the pair stays in the order it
        // was given — a degenerate universe is still ordered deterministically.
        assert_eq!(DecideMode::Simultaneous.cross_section_order(&dup), vec![1, 0, 2]);
    }
}
