//! The kind of an exit bound to an open position: a protective STOP or a take-profit LIMIT. One copy
//! for the bridges that bind exits to a position (cTrader's `PositionsById`, FXCM's trade-bound
//! orders); each venue's wire code for it stays in its bridge.

/// Which side of the market a bound exit sits on: a protective STOP or a take-profit LIMIT. A bracket
/// arms one of each on the same position, so a venue counts capacity PER KIND.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    /// The stop-loss.
    Stop,
    /// The take-profit.
    Limit,
}

impl ExitKind {
    /// The words an operator reads for it.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Limit => "take-profit limit",
        }
    }
}
