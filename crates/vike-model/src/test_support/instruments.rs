//! Contract builders the instrument and picker tests each spelled.

use crate::{Contract, ContractPerp, Margining};

/// A perpetual margined as `margining` and settled in `settle`, with no value currency, funding
/// interval or settling window.
pub fn perp(margining: Margining, settle: &str) -> Contract {
    Contract::Perp(ContractPerp {
        margining,
        settle: settle.into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    })
}
