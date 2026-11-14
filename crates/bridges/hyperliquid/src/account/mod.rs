//! The account itself: who the key trades, its modes and leverage, its link and quote rate.

// Whether the account's API-wallet approval is still alive (`extraAgents` + `userRole`, both public):
// the gatewayhub's hourly watcher asks it. `exec`-gated because it derives the signer address from the key.
#[cfg(feature = "execution")]
pub mod agent_expiry;
// The agent → master lookup. ⚠ NOT behind `exec`: it is an unauthenticated `/info` read that signs
// nothing and needs no key material, so a feeds-only build can ask it too — and the question it
// answers (whose book is this address) is not an execution question.
pub mod identity;
// What the venue lets an account margin a coin at (decision 0181): two public `/info` reads and a pure
// fold. `exec`-gated for the same reason `adapter` is: it resolves the key's account through `adapter`.
#[cfg(feature = "execution")]
pub mod leverage;
#[cfg(feature = "execution")]
pub(crate) mod modes;
#[cfg(feature = "execution")]
pub mod user_signed;
