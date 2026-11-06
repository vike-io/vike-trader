//! The keyless data surface, ungated by `exec`: the live `DataClient` feed (`market_feed`) and its
//! frame normalizers (`market_data`), candle backfill (`history`), instrument load + cache
//! (`instruments`), the universal catalog (`catalog`), realized-funding history (`funding`) and the
//! agent → master lookup (`user_role`). A `default-features = false` build links this and `base` only.

pub mod catalog;
pub mod funding;
pub mod history;
pub mod instruments;
pub mod market_data;
pub mod market_feed;
// The agent → master lookup. ⚠ NOT behind `exec`: it is an unauthenticated `/info` read that signs
// nothing and needs no key material, so a feeds-only build can ask it too — and the question it
// answers (whose book is this address) is not an execution question.
pub mod user_role;
