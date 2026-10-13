//! The EXEC plane of the Polymarket bridge: order signing and submission, fills, the user
//! channel, reconcile and settlement. Compiled only under the crate's `polymarket` feature, as ONE
//! gate on this directory rather than one `#[cfg]` per module; the keyless `feeds` plane lives
//! beside it at the crate root and never names it.
//!
//! Visibility is exactly what it was at the crate root: a module that was private there (visible
//! crate-wide) is `pub(crate)` here, and a `pub` one stays `pub`.

pub(crate) mod auth;
pub(crate) mod client;
pub mod convert_arb;
pub(crate) mod exec;
pub mod fill_tracker;
pub mod heartbeat;
pub(crate) mod history;
pub(crate) mod l1;
pub mod mount;
pub(crate) mod order;
// The bounded, expiring staging area for a user-channel event whose CLOB order id `registry` cannot
// re-key YET (the ack race) — the fix for an executed fill silently vanishing at `user_ws`'s three
// re-key sites. Public because `PendingStats` is the observable half of "a silent drop must not
// survive in any form": it is read off the shared registry (`PolymarketRegistry::pending_stats`).
pub mod pending_events;
pub mod rate_budget;
// `polymarket`-gated DELIBERATELY, unlike hl/aster's feed-plane `ratelimit` modules: this one is
// the USER-channel WS-send gate, and its only consumer is the exec-plane user-data pump.
pub(crate) mod ratelimit;
pub mod recon_client;
pub(crate) mod registry;
pub mod scoring;
// The 9-module opt-in settlement cluster — chain / resolve / auto_redeem / redeem / redeem_confirm
// / redeem_ledger / redeem_relayer / split_merge / positions — lives under settlement/; its module
// doc states the cluster contract ONCE (opt-in env gates, default OFF, no composition-root wiring
// today). Its modules are named by ONE path, `settlement::<module>` (the owner's 2026-09-18
// no-alias ruling): the module aliases that kept the pre-move root paths resolving were deleted
// 2026-09-27. The flat ITEM re-exports further down are crate-root vocabulary and stay.
pub mod settlement;
pub(crate) mod user_data;
pub(crate) mod user_ws;
