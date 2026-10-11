//! `registry`'s tests, one file per table, over `crate::test_support`'s broker double and the resolve
//! helpers below.

use super::*;
use crate::test_support::RecordingBroker;

#[cfg(test)]
mod buy_hold;
#[cfg(test)]
mod gates;
#[cfg(test)]
mod keys_routes;
#[cfg(test)]
mod params;
#[cfg(test)]
mod roster;

/// A bar the RUNTIME would deliver: symbol-stamped with the MOUNT's own instrument
/// (`crates/vike-core/src/runtime/dispatch.rs`'s `BarClose` arm sets `bar.symbol = Some(key.1)`).
fn mounted_bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("MOUNTED".to_string()),
    }
}

fn resolve(
    name: &str,
    params: &Value,
) -> Result<Box<dyn Strategy<RecordingBroker> + Send>, RegistryError> {
    strategy_by_name::<RecordingBroker>(name, params)
}

fn empty() -> Value {
    Value::Table(Default::default())
}
