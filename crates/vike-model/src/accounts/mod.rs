//! Account identity: the credential-key grammar that lets one venue hold many accounts, the tier
//! vocabulary every account row states (`tier`, re-exported at the crate root as `AccountTier`),
//! and the venue network a tier reaches (`venue_network`, at the crate root as `VenueNetwork`).
//! What a venue's own handshake said about the account it authenticated as is parked by
//! `vike_secrets::account_confirmation`, which writes `<project>/settings/state`. One recorded venue
//! session of an account's socket is `link_session`'s row (`LinkSessionRow` at the crate root).
//!
//! Two more names are spelled from the crate root: `BookId` (`book_id.rs`, which venue BOOK an account
//! trades, normalised once) and `ArmingBlock` (`arming.rs`, why an account does not run at its tier).
//! So is `CredentialPresence` (`credential_presence.rs`, whether the credential a credentialed lane
//! reads is stored: a word, never a value).

pub mod account_keys;
pub(crate) mod arming;
pub(crate) mod book_id;
pub(crate) mod credential_presence;
pub(crate) mod link_session;
pub(crate) mod tier;
pub(crate) mod venue_network;
