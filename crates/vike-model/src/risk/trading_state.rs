//! `TradingState`: the three-way switch the pre-trade gate and the operator's kill switch share.
//!
//! Pure data, with no behaviour: the GATE that judges an order against it is vike-exec's
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
