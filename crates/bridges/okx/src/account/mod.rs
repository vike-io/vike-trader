//! The account itself: who the key trades, its modes and leverage, its link and quote rate.

// The account-identity read. `exec`-gated like every other signing module here: it is a SIGNED
// request, so an ungated declaration would redden the `bridges-feeds` lane
// (`cargo check -p vike-okx --no-default-features`), which compiles the keyless market-data half.
pub mod identity;
// The READ of an account's leverage (decision 0182): the gatewayhub's lane asks it before and after the
// one write this crate's exec thread makes. Signed GETs through `transport`, so exec-plane.
pub mod leverage;
// The account's position mode, read at every start and switched to net when the account is flat; a
// signed request, so `exec`-gated like `account_config`.
pub mod modes;
// Whether a spot order can BORROW on the account (decision 0232, amended 2026-11-23): a LIVE start's refusal, judged
// on the withdraw gate's own `account/config` answer, so it lives on the exec plane with that read.
pub mod spot_borrow;
