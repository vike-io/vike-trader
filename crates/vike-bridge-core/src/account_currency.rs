//! **What a venue said about an account's currency, and the currency assumed when it said none** (decision 0183, amended
//! 2026-10-19: "the account currency is what the venue API states; if the API does not state it, or the read fails,
//! USD"). The vocabulary a bridge reads a currency in ([`CcyRead`]) and the one currency the owner's ruling assumes
//! ([`ASSUMED_CURRENCY`]); the memory of what was said, and the warning, are the gatewayhub's (`assumed_currency`).

/// The currency the owner's ruling assumes when the venue states none.
pub const ASSUMED_CURRENCY: &str = "USD";

/// What one read of an account's currency came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CcyRead {
    /// The venue stated it.
    Stated(String),
    /// The venue stated none, or the read failed (`why`): the ruling's USD fills in.
    Silent(String),
    /// The venue stated a currency the lane cannot hand the node (the engine's mounted pair counts its money in
    /// another one), so there is nothing to assume over: the old answer stands.
    Declined,
}
