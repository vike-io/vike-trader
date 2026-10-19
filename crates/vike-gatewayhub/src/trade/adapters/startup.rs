//! **The venue half of the startup preflight: the REAL probes** (wave 2, W9a). The pure go/no-go
//! gate is `crates/vike-tradehub-core/src/preflight.rs`, the orchestrator that walks the legs is
//! `crates/vike-tradehub-core/src/startup.rs` (its module doc is the authority for what each leg
//! measures, what it costs and why it is bounded); this module holds what needs a bridge — the
//! clock reads over each row's declared clock, the credential reads over each row's
//! `credential_probe`, and the network probe — and `LocalBackend` answers the orchestrator's
//! verbs from it.
//!
//! The bounds live here because the blocking calls do: [`CREDENTIAL_PROBE_TIMEOUT`] per credential
//! attempt, [`CLOCK_PROBE_TIMEOUT`] per clock read (the transport's own timeout plus a second, so
//! a bounded read reports itself first), [`CREDENTIAL_PROBE_ATTEMPTS`] tries, and the abandon seam
//! (`bounded_probe`) that walks away from a wedged name resolution instead of parking the start.

use std::time::{Duration, Instant};

use vike_bridge_core::{NetProbe, NetProbeHandle, NetProbeThread};

mod clock;
mod credentials;

pub use clock::{bounded_server_time_ms, clock_risks, clock_venues, venue_server_time_ms};
pub(crate) use credentials::authed_read_probe;
pub use credentials::authed_read_probes;

#[cfg(doc)]
use credentials::bounded_probe;

/// How long [`spawn_net_probe`] waits for the [`NetProbe`]'s FIRST round. A warm resolver
/// answers in single-digit ms; a wedged one cannot be interrupted
/// ([`vike_bridge_core::net_probe`]'s caveat), hence this bound and the probe's own thread: past
/// it the leg WARNs "has not completed a round yet" and the mount carries on.
pub const DEFAULT_NET_PROBE_WAIT: Duration = Duration::from_millis(500);

/// Poll granularity while waiting out [`DEFAULT_NET_PROBE_WAIT`].
const NET_PROBE_POLL: Duration = Duration::from_millis(10);

/// The PER-ATTEMPT ceiling on one venue's authed read: the seam between "this venue refused us" and
/// "we never heard back".
///
/// **5 s**: one signed `fetch_balance` round trip answers in tens to low hundreds of ms when
/// healthy, so ~20x headroom never mistakes a slow link for a dead one, while an unreachable host
/// costs 5 s instead of its agent's 30 s (measured: ~20 s on one geo-blocked alpaca TCP connect
/// from the Windows dev box, 2026-08-22).
///
/// ⚠ Not the `ReconClient`'s timeout (its 30 s agent is untouched): this is how long the MOUNT
/// waits before abandoning the attempt.
pub const CREDENTIAL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The PER-ATTEMPT ceiling on one venue's CLOCK read — [`CREDENTIAL_PROBE_TIMEOUT`]'s twin.
///
/// **`crate::trade::adapters::server_time::CLOCK_READ_TIMEOUT` + 1 s, DERIVED**: the transport timeout
/// bounds every read ureq CAN bound; this caps only the case it CANNOT, a wedged name resolution.
///
/// ⚠ The one-second gap is the point: both deadlines start together (this one marginally
/// EARLIER), so equal values would abandon every ordinary timeout a hair before the transport
/// reported it, trading the venue's own error text ("connection timed out to <host>") for our
/// generic "did not answer". One second lets a bounded read report itself first, and abandons an
/// unbounded one a second later instead of never.
///
/// ⚠ **"Reports itself first" holds for six of the seven wired clock reads; the seventh is a
/// declared residual.** hyperliquid's (`crates/bridges/hyperliquid/src/adapter.rs`'s
/// `server_time_ms`) goes through `crates/bridges/hyperliquid/src/base/transport.rs`'s `info`, a RETRY
/// LOOP above ureq (`INFO_MAX_ATTEMPTS` attempts on the 3 s agent, 200 ms then 400 ms backoff), so
/// slow `429`s/`5xx`s can outlast this ceiling and be abandoned with our generic text. Accepted:
/// the retry fires only on a fast-answering throttle or server error, the worst outcome is ② (a
/// WARN), and widening the ceiling would cost every other venue's wedged-resolver case the same
/// seconds on every mount.
pub const CLOCK_PROBE_TIMEOUT: Duration =
    crate::trade::adapters::server_time::CLOCK_READ_TIMEOUT.saturating_add(Duration::from_secs(1));

/// Attempts per credential probe: TWO, i.e. one retry, only for an attempt that ANSWERED with an
/// error. The retry makes a FAIL mean "confirmed": a stopped venue is a real consequence, so one
/// blip (a `429`, a reset) must not cause it. A TIMED-OUT probe is never retried: it spent its
/// bound learning nothing, and twice doubles the worst case.
pub const CREDENTIAL_PROBE_ATTEMPTS: usize = 2;

/// The pause before the retry: inside the leg's budget, past an instantaneous transport blip.
const CREDENTIAL_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// The longest thread name Linux keeps: `TASK_COMM_LEN` is 16 bytes INCLUDING the NUL, and `std`
/// TRUNCATES a longer name silently. ⚠ The credential name SHIPPED over it (`vike-preflight-cred`,
/// 19 bytes) and a first draft of the clock name copied it, so a the CI box stack dump would show BOTH
/// legs' workers as `vike-preflight-`. The `const` assertion below makes an overrun a build error;
/// `probe_thread_names_survive_linux_truncation` also names the overrun and checks the two differ.
const THREAD_NAME_MAX_BYTES: usize = 15;

/// Thread name of a credential probe's worker: an ABANDONED worker outlives its call
/// ([`bounded_probe`]'s residual), so this is what a stack dump shows — within
/// [`THREAD_NAME_MAX_BYTES`].
const CREDENTIAL_PROBE_THREAD: &str = "vike-pf-cred";

/// Thread name of a clock probe's worker ([`CREDENTIAL_PROBE_THREAD`]'s twin) — the one most likely
/// still parked when an operator looks (the wedged-resolver case).
const CLOCK_PROBE_THREAD: &str = "vike-pf-clock";

// Both names fit what Linux keeps (`THREAD_NAME_MAX_BYTES`): a build error, not a truncated dump.
const _: () = {
    assert!(CREDENTIAL_PROBE_THREAD.len() <= THREAD_NAME_MAX_BYTES);
    assert!(CLOCK_PROBE_THREAD.len() <= THREAD_NAME_MAX_BYTES);
};

/// Spawn the [`NetProbe`] and wait at most `wait` for its first round, so the leg reads a
/// MEASUREMENT, not the handle's optimistic initial value. Keep the [`NetProbeThread`] alive while
/// its handle is read; dropping it stops WITHOUT joining (a wedged DNS resolve never parks us).
pub fn spawn_net_probe(wait: Duration) -> NetProbeThread {
    let thread = NetProbe::with_defaults().spawn();
    let handle = thread.handle();
    let deadline = Instant::now() + wait;
    while !handle.has_probed() && Instant::now() < deadline {
        std::thread::sleep(NET_PROBE_POLL);
    }
    thread
}

/// The network leg's reading of a probe handle, as the fold reads it: three states and their
/// numbers ([`crate::trade::adapters::probe::NetReading`]), so the preflight's report names no bridge type.
#[must_use]
pub fn net_reading_of(handle: &NetProbeHandle) -> crate::trade::adapters::probe::NetReading {
    if !handle.has_probed() {
        crate::trade::adapters::probe::NetReading::Unknown
    } else if handle.internet_up() {
        crate::trade::adapters::probe::NetReading::Alive { rounds: handle.checks() }
    } else {
        crate::trade::adapters::probe::NetReading::Dead { since_ms: handle.last_down_ms() }
    }
}

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::{Arc, Mutex};

#[cfg(test)]
use vike_exec::recon::ReconClient;

#[cfg(test)]
use crate::trade::adapters::probe::{CredentialGap, ServerTimeGap};

#[cfg(test)]
use clock::abandoned_clock_gap;
#[cfg(test)]
use credentials::authed_read_probes_with;
#[cfg(test)]
use credentials::{BoundedProbe, CredentialProbe, CredentialProbes};
#[cfg(test)]
use credentials::{bounded_probe, identity_recording_probe};

#[path = "startup_tests/mod.rs"]
#[cfg(test)]
mod startup_tests;
