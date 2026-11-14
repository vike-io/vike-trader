//! What a symbol IS and the grid it trades on.

/// ⚠ FEEDS side, deliberately: `data`'s route reads it, and a keyless market-data build
/// (`scripts/ci_feature_suite.sh`'s `bridges-feeds` arm) must be able to reach the COIN-M book.
pub mod listing;
#[cfg(feature = "execution")]
pub mod record;
