//! The OANDA demo smokes' one wait on an execution client's ingest lane. The `oanda_smokes` binary
//! (`main.rs` beside it) declares it ONCE by a plain `mod ingest_wait;` (the only shape
//! `scripts/refuse_live_credentials.sh` follows into a lane target), and each smoke reads it as
//! `crate::ingest_wait`.

use std::time::Duration;

use vike_exec::Ingest;
use vike_model::events::Event;

/// Drain the ingest channel until `pred` matches an `Event`, or `total` elapses: `Err` on the timeout or
/// a closed channel.
pub fn wait_for(
    rx: &mut tokio::sync::mpsc::Receiver<Ingest>,
    total: Duration,
    mut pred: impl FnMut(&Event) -> bool,
) -> Result<Event, String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async {
        let deadline = tokio::time::Instant::now() + total;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let ingest = tokio::time::timeout(remaining, rx.recv())
                .await
                .map_err(|_| "timed out waiting for a matching event".to_string())?
                .ok_or("ingest channel closed")?;
            if let Ingest::Event(ev) = ingest
                && pred(&ev)
            {
                return Ok(ev);
            }
        }
    })
}
