//! One row of an arming table, built by hand: ONE copy for this crate's hot-account tests
//! (`src/node/hot_tests.rs`) and the tradehub's feed-gate tests (`crates/vike-tradehub/src/feed_gate_tests.rs`),
//! which each carried their own until 2026-10-31.

use vike_config::{ArmingBlock, VenueArming};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;

use crate::ArmingRow;

/// The row of account `label` of `venue` (account id 1), held at `tier`, running at `effective` (or not,
/// with `block`), with no book.
pub fn arming_row(
    venue: &'static str,
    label: AccountLabel,
    tier: AccountTier,
    effective: Option<AccountTier>,
    block: ArmingBlock,
) -> ArmingRow {
    ArmingRow {
        arming: VenueArming {
            venue,
            label,
            tier: Some(tier),
            effective,
            block,
            account_ids: vec![1],
        },
        book: None,
    }
}
