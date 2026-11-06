//! Binance's spot executionReport → vike event mapper: the venue face of the shared
//! [`crate::family::event_mapper`].
//!
//! Aster's spot user-data stream is Binance-verbatim, so the mapping logic itself lives once in
//! `family` and both venues re-export it under their own names (F0, dedup rung 1). The contract,
//! the golden fixtures (`tests/offline/r6_binance_parity.rs`, `tests/offline/r6_binance_userdata.rs`), and the
//! public paths here are unchanged — see the family module for the dual-publish/commission/
//! trade_id notes.

pub use crate::family::event_mapper::{map_execution_report, map_private as map_binance_private};
