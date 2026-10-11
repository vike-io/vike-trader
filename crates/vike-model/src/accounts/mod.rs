//! Account identity: the credential-key grammar that lets one venue hold many accounts, what a
//! venue's own handshake said about the account it authenticated as, and the tier vocabulary every
//! account row states (`venue_mode`, re-exported at the crate root as `VenueMode`).

pub mod account_confirmation;
pub mod account_keys;
pub(crate) mod venue_mode;
