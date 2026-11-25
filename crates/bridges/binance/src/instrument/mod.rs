//! What a symbol IS and the grid it trades on.

/// ⚠ FEEDS side, deliberately: `data`'s route reads it, and a keyless market-data build
/// (`scripts/ci_feature_suite.sh`'s `bridges-feeds` arm) must be able to reach the COIN-M book.
pub mod listing;
/// Decision 0290: a user name and its lane name, both ways, off the kept row (the feeds side reads it too).
pub mod names;
#[cfg(feature = "execution")]
pub mod record;
