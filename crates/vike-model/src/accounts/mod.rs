//! Account identity: the credential-key grammar that lets one venue hold many accounts, and the tier
//! vocabulary every account row states (`account_tier`, re-exported at the crate root as `AccountTier`).
//! What a venue's own handshake said about the account it authenticated as is parked by
//! `vike_secrets::account_confirmation`, which writes `<project>/settings/state`.

pub mod account_keys;
pub(crate) mod account_tier;
