//! One `ExecutionEngine<RecordingClient>` builder shared by the `engine`/`recon`/`parity`/`risk` members.

use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, ExecutionEngine, RiskGate, RiskLimits};

/// The fields the members' engines differ on, DEFAULTED to the most common value: multiplier `1.0`,
/// venue `"sim"`, symbol `"BTCUSDT"`, `RiskLimits::new()` (no limits), no extra symbols. A member
/// overrides with struct-update syntax (`EngineBuilder { venue: .., ..Default::default() }`), so
/// every departure stays visible at its own site. Plain fields, not setters: each binary sets a
/// different subset, and a setter one binary never calls is dead code there.
///
/// PINNED: no per-symbol multiplier map, `BalanceMode::Delta`, a fresh `RecordingClient`, and the
/// account labelled with the engine's own venue.
pub struct EngineBuilder {
    /// `Account::new`'s first argument: the DEFAULT contract multiplier, not an equity seed.
    pub multiplier: f64,
    pub venue: String,
    pub symbol: String,
    pub limits: RiskLimits,
    /// Assigned after construction (`ExecutionEngine::new` leaves it empty).
    pub extra_symbols: Vec<String>,
}

impl Default for EngineBuilder {
    fn default() -> Self {
        EngineBuilder {
            multiplier: 1.0,
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            limits: RiskLimits::new(),
            extra_symbols: Vec::new(),
        }
    }
}

impl EngineBuilder {
    pub fn build(self) -> ExecutionEngine<RecordingClient> {
        let mut e = ExecutionEngine::new(
            Account::new(self.multiplier, &self.venue, None, BalanceMode::Delta),
            RiskGate::new(self.limits),
            RecordingClient::default(),
            &self.venue,
            &self.symbol,
        );
        e.extra_symbols = self.extra_symbols;
        e
    }
}
