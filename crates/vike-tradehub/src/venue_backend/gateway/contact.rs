//! **The bounded wait for the gatewayhub at start** (decision 0164, owner ruling D7): a tradehub
//! that starts while the daemon is down keeps asking for `config.gatewayhub_connect_wait_ms`, then
//! gives up — and giving up is NOT a refusal to start: the accounts that need the gatewayhub are
//! NOT RUNNING with `ArmingBlock::GatewayUnavailable` and the ones that need no venue run as usual.
//!
//! The wait is a function of an `ask` closure and a [`Pacer`] (the clock and the sleep), so a test
//! drives it on a virtual clock in microseconds: the production pacer is [`SystemPacer`].
//!
//! Shape: ask at once; after a failure sleep `FIRST_RETRY`, doubling to `MAX_RETRY`, never past the
//! deadline; the LAST ask is made AT the deadline, so a gatewayhub that comes up in the final second
//! is found. Progress is `warn!` once at the first failure and every [`REPORT_EVERY`] after it, each
//! line naming the address, the reason and the time left — an operator reading a journal during a
//! deploy sees a wait, not a hang.

use std::io;
use std::time::{Duration, Instant};

/// The first pause after a failed ask.
pub(super) const FIRST_RETRY: Duration = Duration::from_millis(250);
/// The longest pause between asks.
pub(super) const MAX_RETRY: Duration = Duration::from_secs(2);
/// How often a failing wait says so.
pub(super) const REPORT_EVERY: Duration = Duration::from_secs(10);

/// The clock and the sleep the wait runs on.
pub(super) trait Pacer {
    /// Now, on this pacer's clock.
    fn now(&self) -> Instant;
    /// Sleep for `d` on this pacer's clock.
    fn sleep(&self, d: Duration);
}

/// The real clock and `std::thread::sleep`.
pub(super) struct SystemPacer;

impl Pacer for SystemPacer {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Ask until `ask` answers or `wait` has passed. `Err` is the LAST failure's text (never a
/// credential: the client's errors name the address and the reason).
pub(super) fn wait_for<T>(
    ask: &mut dyn FnMut() -> io::Result<T>,
    wait: Duration,
    pacer: &dyn Pacer,
    what: &str,
) -> Result<T, String> {
    let started = pacer.now();
    let deadline = started + wait;
    let mut delay = FIRST_RETRY;
    let mut next_report = started;
    loop {
        let why = match ask() {
            Ok(answer) => {
                let waited = pacer.now().saturating_duration_since(started);
                if waited >= FIRST_RETRY {
                    tracing::info!(
                        waited_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX),
                        "{what}: the gatewayhub answered"
                    );
                }
                return Ok(answer);
            }
            Err(e) => e.to_string(),
        };
        let now = pacer.now();
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            return Err(why);
        }
        if now >= next_report {
            tracing::warn!(
                remaining_s = remaining.as_secs(),
                "{what}: waiting for the gatewayhub ({why}); {} s left before its demo/live \
                 accounts are stopped with `gatewayhub unavailable`",
                remaining.as_secs()
            );
            next_report = now + REPORT_EVERY;
        }
        pacer.sleep(delay.min(remaining));
        delay = (delay * 2).min(MAX_RETRY);
    }
}

#[path = "contact_tests.rs"]
#[cfg(test)]
mod contact_tests;
