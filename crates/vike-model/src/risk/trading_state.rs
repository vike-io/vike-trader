//! `TradingState`: the three-way switch the pre-trade gate and the operator's kill switch share.
//!
//! Pure data, plus the one fact about it every consumer that COMPARES two of them needs: how strict
//! each is ([`TradingState::strictness`]). The GATE that judges an order against it is vike-exec's
//! `RiskGate`, and the verbs that move it (`SetTradingState`, the halt sentinel) are the exec and
//! core planes'. It sits here because the engine snapshot, the published read model, the journaled
//! command lane and the GUI all name it, and none of them may link the gate to do so.

/// Whether the engine admits new orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TradingState {
    Active,
    /// only position-reducing orders allowed
    Reducing,
    /// no new orders (kill switch)
    Halted,
}

impl TradingState {
    /// How strict the state is: `Active` 0 < `Reducing` 1 < `Halted` 2. A larger number admits
    /// less. Used wherever one state must not LOOSEN another (a run profile's
    /// `guards.initial_trading_state` is never sent over a stricter state the journal restored) —
    /// compare these numbers, never the variants' declaration order.
    #[must_use]
    pub const fn strictness(self) -> u8 {
        match self {
            TradingState::Active => 0,
            TradingState::Reducing => 1,
            TradingState::Halted => 2,
        }
    }
}

#[path = "trading_state_tests.rs"]
#[cfg(test)]
mod trading_state_tests;
