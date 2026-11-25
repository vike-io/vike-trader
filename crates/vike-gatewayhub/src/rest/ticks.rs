//! **The tick lane's engine**: `Fetch { Ticks }` answered by a venue's `TicksSpec` — the engine half of
//! `crates/vike-bridge-core/src/ticks.rs`'s contract. The venue's refusals, the requests it cuts a window
//! into and the points it can stop at are the spec's, in the venue's bridge (dukascopy's `.bi5` hour walk
//! is `crates/bridges/dukascopy/src/ticks.rs`'s `TicksSpecDukascopy`); this file holds no venue fact.
//!
//! The engine hands the spec a stop probe that reads the request's cancel flag (`FetchContext::is_cancelled`)
//! and maps its answer onto the wire's, words unchanged: a refusal is `SourceError::Refused`, a venue
//! failure `SourceError::Fetch`, and a stop the engine's cancel words naming the spec's stage.

use vike_bridge_core::ticks::{TicksError, TicksSpec};
use vike_data::source::SourceError;
use vike_marketdata::TickQuote;

use super::{FetchContext, cancelled};

/// `Fetch { Ticks }` over `spec`: the module doc.
pub fn fetch_ticks(
    spec: &dyn TicksSpec,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
    ctx: &FetchContext,
) -> Result<Vec<TickQuote>, SourceError> {
    spec.fetch_until(symbol, start_ms, end_ms, &|| ctx.is_cancelled()).map_err(
        |error| match error {
            TicksError::Refused(words) => SourceError::Refused(words.into()),
            TicksError::Fetch(words) => SourceError::Fetch(words.into()),
            TicksError::Stopped(stage) => cancelled(stage),
        },
    )
}

#[cfg(test)]
#[path = "ticks_tests.rs"]
mod ticks_tests;
