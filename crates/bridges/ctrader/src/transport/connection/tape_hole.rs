//! What a data socket owes the recorder when it dies: the tape of every spot it carries has a hole.
//!
//! The word is `vike_model::TAPE_HOLE_WORD`, never `quotes`: `crates/bridges/ctrader/CLAUDE.md`'s
//! "A data-socket outage is marked for the recorder alone" says what `quotes` would do.

use std::collections::HashSet;

use vike_data::DataClientSink;
use vike_model::{StreamStatus, TAPE_HOLE_WORD};

use super::VENUE;
use crate::instrument::symbols::SymbolMap;

/// Open a hole from `down_at` on every spot of this socket and return the symbols it was said of: the ones
/// [`close_tape_holes`] closes, since a key subscribed during the outage had no tape to lose. `down_at` is where the
/// socket last proved its tape whole (`super::actor::hole_starts_at`), never the later moment it was called dead.
pub(super) fn open_tape_holes(
    sink: &dyn DataClientSink,
    symbols: &SymbolMap,
    spot_subs: &HashSet<i64>,
    down_at: i64,
) -> Vec<String> {
    let holed: Vec<String> =
        spot_subs.iter().filter_map(|id| symbols.name_of(*id)).map(str::to_string).collect();
    let status = StreamStatus::GapStart { at_timestamp_ms: down_at };
    for symbol in &holed {
        sink.stream_status(VENUE, symbol, TAPE_HOLE_WORD, status);
    }
    holed
}

/// The socket is back and `holed` is subscribed again.
pub(super) fn close_tape_holes(sink: &dyn DataClientSink, holed: &[String], down_at: i64) {
    let status = StreamStatus::Live { gap_started_timestamp_ms: Some(down_at) };
    for symbol in holed {
        sink.stream_status(VENUE, symbol, TAPE_HOLE_WORD, status);
    }
}
