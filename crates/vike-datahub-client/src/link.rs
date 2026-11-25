//! `link` — **the held datahub connection a poll keeps, and how a poll rides out a broken LINK**.
//!
//! # The held connection
//!
//! [`DatahubLink`] is the connection both polls of [`crate::tool_feeds::poll`] (the News and
//! Calendar half and the Options half) hold between their passes, with the dial it was made for:
//! a keyed handshake is a nonce round trip, and paying it on every pass for the life of a window
//! would be most of the traffic. [`held_or_dial`] hands a pass the held one when it was made for
//! the same address and key name, else a fresh `Scope::Read` dial.
//!
//! # A broken link
//!
//! A transport failure (a refused, reset or aborted socket, a timeout, a tunnel blip, a datahub
//! restart) is TRANSIENT: the poll keeps the last good rows, asks again on its own cadence (no timer
//! of its own here) and shows only [`RECONNECTING_LINE`]. The cause is shown, plainly, once the link
//! has failed more than [`QUIET_FAILURES`] passes in a row, and a pass that reaches the datahub
//! again ends the streak. Which failures are transient is the protocol's verdict
//! ([`DatahubLinkError`]); a refusal the datahub ANSWERED (a refused key, an unknown verb, an old
//! server) is never one, and its caller shows it at once.
//!
//! The first failure of a streak is logged ONCE at WARN with its cause, and the recovery once at
//! INFO; nothing is logged per pass (the options poll asks every second).

use std::time::Duration;

use vike_hub_protocol::auth::Scope;
use vike_utility::errors::FailureOutcome;

use crate::DatahubClient;
use crate::dial::DatahubDial;

/// How many link failures in a row a poll shows only [`RECONNECTING_LINE`] for: the next one names
/// the cause. A slow, lossy ssh tunnel drops one held connection now and then, and a restarted
/// datahub refuses for a few seconds; neither is a fault worth a red line.
pub(crate) const QUIET_FAILURES: u32 = 2;

/// What a poll shows while its link is down for at most [`QUIET_FAILURES`] passes.
pub(crate) const RECONNECTING_LINE: &str = "reconnecting to the datahub…";

/// **A dial or a request that came back without an answer**, and which side failed.
///
/// `transient` is the LINK failing: a refused, reset or aborted socket, a timeout, a peer gone
/// mid-frame — a later pass may get through, so the poll rides it out quietly
/// ([`DatahubLinkStreak`]). It is never set when the datahub SPOKE: a dial it refused is read by
/// `vike_hub_protocol::call::HubCallError::failure_outcome` (`Refused`: an older datahub a device key
/// cannot answer included, so a version skew is named at once, never "reconnecting"), and a request it
/// answered with an error leaves the client usable, where any transport failure marks it unusable
/// (`vike_hub_protocol::conn::HubConnection::is_usable`). Both verdicts are the protocol's own; none
/// is recovered from the words.
#[derive(Debug)]
pub(crate) struct DatahubLinkError {
    /// The client's own sentence: the cause a status line names.
    pub(crate) cause: String,
    /// The link failed, not the datahub's answer.
    pub(crate) transient: bool,
}

impl DatahubLinkError {
    /// A request's `Err` on `client`, judged by whether the client is still usable after it.
    pub(crate) fn after_request(cause: String, client: &DatahubClient) -> Self {
        Self { cause, transient: !client.is_usable() }
    }
}

/// The held connection, with the dial it was made for. Both polls hold one, through
/// [`held_or_dial`].
pub(crate) struct DatahubLink {
    address: String,
    epoch: u64,
    pub(crate) client: DatahubClient,
}

/// The connection a pass asks on: `held` when it was made for this `dial`'s address and device-key epoch,
/// otherwise a fresh `Scope::Read` dial whose replies are bounded by `reply_timeout` (a `held` for
/// another dial is dropped after it). `Err` is the dial's failure: nothing was sent.
pub(crate) fn held_or_dial(
    held: Option<DatahubLink>,
    dial: &DatahubDial,
    reply_timeout: Duration,
) -> Result<DatahubLink, DatahubLinkError> {
    match held {
        Some(h) if h.address == dial.address && h.epoch == dial.epoch => Ok(h),
        _ => {
            let mut client =
                DatahubClient::connect_with_keys(&dial.address, dial.keys.as_ref(), Scope::Read)
                    .map_err(|e| DatahubLinkError {
                        transient: e.failure_outcome() != FailureOutcome::Refused,
                        cause: e.to_string(),
                    })?;
            client.set_reply_timeout(Some(reply_timeout));
            Ok(DatahubLink { address: dial.address.clone(), epoch: dial.epoch, client })
        }
    }
}

/// One poll's run of link failures (the News and Calendar poll holds one, the options poll
/// another). A switched datahub starts a new one: each poll's `observe_dial` resets its state.
#[derive(Debug, Default)]
pub(crate) struct DatahubLinkStreak {
    failures: u32,
}

impl DatahubLinkStreak {
    /// One pass of `poll` whose link to the datahub at `addr` failed with `cause`. `None` while the
    /// streak is short (show [`RECONNECTING_LINE`]); the sentence that names the cause once it is
    /// not. The first failure of a streak is logged here, once.
    pub(crate) fn failed(&mut self, poll: &str, addr: &str, cause: &str) -> Option<String> {
        self.failures = self.failures.saturating_add(1);
        if self.failures == 1 {
            tracing::warn!(
                "{poll}: lost the link to the datahub at {addr} ({cause}); keeping the last good \
                 rows and retrying on the poll's own cadence"
            );
        }
        (self.failures > QUIET_FAILURES).then(|| {
            format!(
                "the link to the datahub at {addr} failed {} times in a row (still retrying): \
                 {cause}",
                self.failures
            )
        })
    }

    /// One pass of `poll` that reached the datahub at `addr` (any answer, a refusal included): ends
    /// a streak, logging the recovery once.
    pub(crate) fn reached(&mut self, poll: &str, addr: &str) {
        if self.failures > 0 {
            tracing::info!(
                "{poll}: the link to the datahub at {addr} is back after {} failed passes",
                self.failures
            );
        }
        self.failures = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short streak stays quiet, the next failure names the cause and the count, and a pass that
    /// reaches the datahub starts the count again.
    #[test]
    fn a_short_streak_is_quiet_a_long_one_names_its_cause_and_a_success_resets_it() {
        let mut streak = DatahubLinkStreak::default();
        for _ in 0..QUIET_FAILURES {
            assert_eq!(streak.failed("news", "127.0.0.1:17878", "os error 10053"), None);
        }
        let loud = streak.failed("news", "127.0.0.1:17878", "os error 10053").expect("loud");
        assert!(loud.contains("127.0.0.1:17878") && loud.contains("os error 10053"), "{loud}");
        assert!(loud.contains("3 times in a row"), "{loud}");
        assert!(loud.contains("failed"), "the providers' dot reads it as an error: {loud}");
        streak.reached("news", "127.0.0.1:17878");
        assert_eq!(streak.failed("news", "127.0.0.1:17878", "reset"), None, "a new streak");
    }
}
