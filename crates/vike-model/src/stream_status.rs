//! `StreamStatus` — the stream-health disclosure for one live stream (net-hardening §B), the ONE
//! vocabulary every layer that speaks it shares: the shared health state machine that produces it
//! (`vike_bridge_core::stream_health`'s `StreamHealth`), the data seam that carries it
//! (`vike_data::LiveDataSink::stream_status`), and the datahub's market-data wire that ships it to a
//! remote session (`vike_datahub_client::market`'s `MdFrame`).
//!
//! # Why it lives HERE
//!
//! It used to be four copies: a data-layer enum in `vike-data` (layer 20) that "did not grow serde",
//! a field-for-field `HealthEvent` mirror in `vike-bridge-core` (whose pure half keeps `vike-data`
//! behind a feature), a `WireStreamStatus` serde mirror with a `From` pair in
//! `vike-datahub-client`, and the timestamp-free [`crate::FeedStatus`]. The first three were the same
//! three cases with the same fields, held together by hand-written 1:1 maps — the shape the
//! workspace's rule refuses (*when two sides must not disagree, the cure is a shared crate BELOW
//! both*). vike-model is below every one of them and already carries serde, so the enum is defined
//! once, with serde, and each layer names it.
//!
//! # ⚠ The serde shape IS the datahub market-data wire
//!
//! The derive is the default externally tagged form — `{"GapStart":{"at_ts_ms":…}}`,
//! `{"Live":{"gap_started_ts_ms":…}}`, `{"Stale":{"newest_data_ts_ms":…,"now_ms":…}}` — byte-identical
//! to the retired `WireStreamStatus`. Renaming a variant or a field changes what a deployed datahub
//! sends a desktop; `crates/vike-model/src/stream_status_tests.rs` pins the bytes.
//!
//! # What [`crate::FeedStatus`] is, and why it stays
//!
//! [`crate::FeedStatus`] is the strategy-facing STATE (`Disconnected`/`Stale`/`Live`) a mounted
//! strategy and the connection-state dead-man react to; the trip timestamps here are diagnostic and
//! a strategy never reads them. [`StreamStatus::feed_status`] is the one projection from this
//! transition onto that state.

use serde::{Deserialize, Serialize};

use crate::FeedStatus;

/// One stream-health transition. A `stream` names the lane the way the data methods do: an
/// interval (`"1m"`) for bar streams, `"quotes"`/`"trades"`/`"book"`/`"depth"` for tick lanes.
/// Reconnect *handling* (backoff, re-open, re-seed) stays entirely the feed's concern; what the
/// seam gains is gap *disclosure*, because for tick lanes the data lost in a gap is unrecoverable
/// and only the consumer can decide what staleness means (flatten, re-arm, widen).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamStatus {
    /// The stream can no longer be trusted from `at_ts_ms`: transport error, server close, or
    /// idle-watchdog trip. Data between this and the next `Live` is MISSING (tick lanes) or STALE
    /// (bar forming lane); the lossless bar lane is repaired by the re-seed before `Live` fires.
    GapStart {
        /// When the gap opened (epoch-ms).
        at_ts_ms: i64,
    },
    /// The stream is live again. For bar streams this fires AFTER the re-seed, so the lossless lane
    /// is already repaired when consumers see it; for tick lanes it marks where the unrecoverable
    /// hole ends. `gap_started_ts_ms` echoes the matching `GapStart` when one was emitted.
    ///
    /// `Live` ALSO closes a data-freshness staleness episode (a preceding `Stale`) — in that case NO
    /// re-seed occurred at all: the socket never dropped, so there was nothing to repair; fresh
    /// data simply resumed flowing on the same still-live connection. `gap_started_ts_ms` then
    /// echoes the `Stale` episode's trip time rather than a real gap start.
    Live {
        /// The episode this recovery closes, if any.
        gap_started_ts_ms: Option<i64>,
    },
    /// The transport is still alive (recent frames — data OR keepalives) but no fresh DATA has
    /// arrived: `now_ms − newest_data_ts_ms` exceeded the subscription's freshness threshold. This
    /// is the silently-failed re-subscribe a socket-liveness watchdog can't see. Distinct from
    /// `GapStart` (transport loss) precisely because both can be true at once — a consumer may need
    /// "transport OK but data stale". Recovery is the shared `Live`.
    Stale {
        /// The newest data stamp seen (epoch-ms).
        newest_data_ts_ms: i64,
        /// When the staleness was judged (epoch-ms).
        now_ms: i64,
    },
}

impl StreamStatus {
    /// The [`FeedStatus`] this transition discloses — the ONE map from a stream-health transition
    /// onto the state a strategy (`Strategy::on_feed_status`) and the connection-state dead-man
    /// act on. Every consumer that needs the state asks this rather than re-spelling the match: the
    /// core's sink boundary (`vike_core::CoreLaneSink`) and the HFT tick-track pumps that push a
    /// `FeedStatus` straight onto the core tick lane (`crates/bridges/binance/src/family/depth.rs`'s
    /// `disclose_link` and its bybit/okx twins). A second spelling would be a second answer to "is a
    /// `GapStart` a `Disconnected`".
    ///
    /// `GapStart` → `Disconnected` and `Stale` → `Stale` are the two that matter to the dead-man
    /// (`docs/decisions/0038-the-dead-man-observes-the-connection-not-silence.md`): the first opens
    /// its grace window, the second moves nothing at all. The timestamps are dropped: they are
    /// diagnostic, and a strategy reacts to the state, not the stamp.
    #[must_use]
    pub const fn feed_status(self) -> FeedStatus {
        match self {
            StreamStatus::GapStart { .. } => FeedStatus::Disconnected,
            StreamStatus::Live { .. } => FeedStatus::Live,
            StreamStatus::Stale { .. } => FeedStatus::Stale,
        }
    }
}

#[path = "stream_status_tests.rs"]
#[cfg(test)]
mod stream_status_tests;
