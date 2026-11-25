use std::path::Path;

use super::*;
use crate::{ConfigError, RowPlanError};

/// A value refusal reads `error [<id>]: <file>: <key> = <text>`, and after the id the bytes are the
/// ones the loader always printed; its `Display` stays bare. A failure with no refusal of its own
/// (an unreadable source) still has an id.
#[test]
fn a_value_refusal_shows_its_id_before_the_unchanged_sentence() {
    let e = ConfigError::value(
        Path::new("policy.toml"),
        "market_slippage",
        ConfigRefusal::MarketSlippageAboveMaximum { value: 0.9, max: 0.05 },
    );
    let old = "policy.toml: market_slippage = 0.9 exceeds the allowed maximum 0.05 (a band is a \
               FRACTION: 0.002 = 0.2%). The maximum is the widest band already compiled in, so this \
               key can only tighten a venue, never widen one";
    assert_eq!(e.to_string(), old);
    assert_eq!(e.id(), ConfigRefusalId::MarketSlippageAboveMaximum);
    assert_eq!(e.rendered(), format!("error [config.market_slippage.above_maximum]: {old}"));

    let unreadable =
        ConfigError::Read { file: "settings.db".into(), source: std::io::Error::other("denied") };
    assert_eq!(
        unreadable.rendered(),
        "error [config.load.unreadable]: settings.db: cannot be read: denied"
    );
}

/// A key the grammar refuses reads `error [<id>]: bad settings key ...`. Built here, not through the
/// planner: a file that calls the writer is pinned by the settings-row writer gate, and the planner's
/// own path to this refusal is `write_tests.rs`'s.
#[test]
fn a_bad_key_shows_its_id_before_the_sentence() {
    let err = RowPlanError::BadKey(SettingsWriteRefusal::EmptySegment { key: "config..x".into() });
    assert_eq!(
        err.to_string(),
        "error [config.write.empty_segment]: bad settings key \"config..x\": empty key segment"
    );
}
