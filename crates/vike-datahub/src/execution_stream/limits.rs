//! The execution stream's POLICY numbers: how often the datahub flushes a window and retries a failed one. The
//! numbers both ends need are `vike_datahub_client::proto`'s `EXECUTION_STREAM_*`.

use std::time::Duration;

use vike_datahub_client::proto::EXECUTION_STREAM_HEARTBEAT;

/// A window is flushed this long after its first buffered record at the latest, so a quiet stream is
/// acknowledged within a second of its last execution.
pub const FLUSH_EVERY: Duration = Duration::from_secs(1);

/// A window is flushed as soon as it holds this many records: four full frames of a catch-up.
pub const FLUSH_RECORDS: usize = 1024;

/// A window whose write failed is retried, the SAME window under the same keys, this often. Meanwhile no frame is
/// read, so TCP pushes back on the tradehub's publisher thread alone.
pub const RETRY_EVERY: Duration = Duration::from_secs(1);

/// The longest a write of a down frame may block on a tradehub that stopped reading.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a second open of one journal waits for the first stream of it to end before it is refused: the two
/// must never write one cursor at once.
pub const SUPERSEDE_WAIT: Duration = Duration::from_secs(5);

/// A failing flush, at the same stage, is logged again at most this often.
pub const FAILURE_REWARN_EVERY: Duration = Duration::from_secs(15 * 60);

const _: () = assert!(
    FLUSH_EVERY.as_millis() < EXECUTION_STREAM_HEARTBEAT.as_millis()
        && RETRY_EVERY.as_millis() < EXECUTION_STREAM_HEARTBEAT.as_millis(),
    "a window must be flushed, and a failed one retried, well within one heartbeat of the tradehub"
);
const _: () = assert!(
    FLUSH_RECORDS >= vike_datahub_client::proto::EXECUTION_STREAM_RECORDS_PER_FRAME,
    "a window holds at least one full frame"
);
