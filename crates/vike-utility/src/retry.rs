//! **Asking again after a pause** (decision 0141): [`retry_on_pauses`], the ONE ladder every caller
//! that asks a remote side again shares — the venue bridges (oanda's submit lookup, ig's `/confirms`
//! and login, hyperliquid's `/info` and spot symbology, deribit's fresh-combo wait) and the datahub's
//! archive download (`crates/vike-datahub/src/import/s3.rs`'s `S3Bucket`).
//! It moved here from `vike-bridge-core`'s `retry` module (the owner's "yes" of 2026-11-13), because
//! the datahub may not link that crate (`DATAHUB_FENCE`) and kept a copy of its own.
//!
//! The schedule, the "settled" rule and the per-miss line are the CALLER's: this function owns only
//! the order of asks and waits, so it names no transport, no status and no `tracing` target.

use std::time::Duration;

/// **Ask, and while the answer is not `settled`, wait the next of `pauses` and ask again.** At most
/// `pauses.len() + 1` asks, no wait before the first or after the last; the first settled answer wins,
/// otherwise the LAST answer stands. `ask` is handed the attempt's number (1 is the first), so a venue
/// says its own per-miss line in its own `tracing` target and words; `sleep` is injected so a schedule
/// is assertable without a clock.
pub fn retry_on_pauses<T>(
    pauses: &[Duration],
    mut ask: impl FnMut(u32) -> T,
    settled: impl Fn(&T) -> bool,
    mut sleep: impl FnMut(Duration),
) -> T {
    let mut answer = ask(1);
    for (asked, pause) in (1u32..).zip(pauses) {
        if settled(&answer) {
            break;
        }
        sleep(*pause);
        answer = ask(asked + 1);
    }
    answer
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
