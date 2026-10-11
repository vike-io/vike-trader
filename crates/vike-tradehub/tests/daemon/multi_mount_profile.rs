//! The `[[mounts]]` daemon profile (split-plane I10, the Pattern-A centerpiece): N strategies /
//! N venues in ONE daemon process — the STATIC, profile-declared half (runtime mount/unmount is
//! B5, a separate concurrent workstream).
//!
//! What is proven, and against which seam:
//!
//! - **Profile shape** (`DaemonProfile::from_toml_str`): the single-mount spelling parses
//!   byte-compatibly beside the new array; both-spellings-set is refused naming the offending
//!   keys; a row that fails the existing per-mount refusals fails naming its ROW; two rows
//!   deriving one mount id are refused at LOAD naming both rows (never `assemble_core`'s
//!   duplicate-id panic).
//! - **Which ACCOUNT a row trades on** (`account = "ALT"`): the headline SPREAD — two rows, one
//!   venue, one symbol, one strategy, two accounts — LOADS, each row keeps its own label through
//!   `mount_rows`/`to_mount_spec`, and the two derive DIFFERENT mount ids while the default
//!   account's id stays byte-identical. This is the operator's only entry point to the account
//!   field, and it sits above `vike-core`/`vike-mount`, so their account tests cannot reach it.
//! - **The mount itself** (`vike_mount::build_paper_multi_strategy_core_with`, the exact call
//!   `main.rs`'s multi paper arm composes): a two-venue profile mounts BOTH strategies on one
//!   core and each trades into its OWN venue's paper book (the attribution a shared process must
//!   keep straight); a one-venue two-symbol profile routes through
//!   `MultiPaperExecutionClient` — each book holds exactly its own mount's fill.
//! - **Bounded teardown**: the two-mount core shuts down `Graceful` inside the profile deadline
//!   through the same `run_with_deadline` primitive `main.rs` uses.
//! - **`StrategyStatus` over the wire** (split-plane B4's `mounts: Vec<WireMountRow>` — designed
//!   for exactly this): a publisher spawned with N mount rows answers N rows; the mount-less
//!   `publish::spawn` keeps the identity-derived single row byte-identically
//!   (`observe_roundtrip.rs` pins that half).
//!
//! No network, no creds, no feature flags: paper books + hand-fed bars + a loopback observe
//! server, all in the default CI lane.

use std::sync::{Arc, Mutex};

use vike_mount::{MultiStrategyMount, PaperFill};

// The HALT pinning, the resolve and the bar drive the children use, from the binary's one copy
// (`own_sentinel` documents why a paper mount needs its sentinel pinned at all).
use crate::support::{drive_two_bars, opts_pinned_to, own_sentinel, resolve_mounts, wait_until};

#[cfg(test)]
#[path = "multi_mount_profile/mounts_trade.rs"]
mod mounts_trade;
#[cfg(test)]
#[path = "multi_mount_profile/profile_shape.rs"]
mod profile_shape;
#[cfg(test)]
#[path = "multi_mount_profile/strategy_status.rs"]
mod strategy_status;

fn book<'a>(
    mount: &'a MultiStrategyMount,
    venue: &str,
    symbol: &str,
) -> &'a Arc<Mutex<Vec<PaperFill>>> {
    mount.fills.iter().find(|((v, s), _)| v == venue && s == symbol).map(|(_, f)| f).unwrap_or_else(
        || {
            panic!(
                "no paper book for ({venue}, {symbol}); books: {:?}",
                mount.fills.iter().map(|(k, _)| k).collect::<Vec<_>>()
            )
        },
    )
}
