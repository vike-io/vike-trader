//! The **mount-site wiring** of [`crate::preflight`]: the one place the pure go/no-go gate is
//! handed REAL probes. `vike_mount::build_node` calls [`run_startup_preflight`] once, at the top of
//! the node assembly, before any venue is mounted.
//!
//! `preflight` performs no I/O: every observation arrives through its four-method
//! [`PreflightProbes`](crate::preflight::PreflightProbes) seam, and an unwired leg returns
//! [`NO_PROBE`](crate::preflight::NO_PROBE). The legs:
//!
//! - **network — ARMED.** A real [`NetProbe`] is spawned, waited on for at most
//!   [`DEFAULT_NET_PROBE_WAIT`], read, then dropped (stop without join,
//!   [`vike_bridge_core::NetProbeThread`]'s `Drop`). Spawned ONLY when a venue would mount live
//!   AT ITS ACCOUNT'S TIER ([`crate::would_mount_live_under_policy`] over `vike_model::VENUES`): a
//!   pure-paper mount — no credentials, or every account `paper` — stays offline and the leg WARNs
//!   "no NetProbe wired".
//! - **clock skew — ARMED from the roster declaration table** ([`crate::server_time`]). Every
//!   venue that WOULD MOUNT LIVE AT ITS ACCOUNT'S TIER gets a row: a MEASUREMENT where an endpoint
//!   is
//!   wired (against the demo/mainnet host that venue's arm binds), a "publishes one, did not
//!   answer" WARN where the read fails, and a DECLARED row with its reason where there is no clock
//!   leg — NOT-APPLICABLE where drift costs nothing, a WARN where auth signs the clock into the
//!   order path (④; polymarket). None of the last three is ever a FAIL, so our inability to measure
//!   never degrades a venue, and a MEASURED skew FAILs only a venue that rejects orders over drift
//!   (`crate::server_time::clock_policy_of`).
//! - **credentials — ARMED, through each venue's own `ReconClient`.** [`authed_read_probes`]
//!   builds one PROBE (a cheap `fetch_balance`) per venue whose credentials resolve. ⚠ INVARIANT:
//!   [`PreflightConfig::credential_venues`] is DERIVED from that probe map, never hand-written — an
//!   unwired credential leg FAILs by design, so listing a venue we cannot authed-read would
//!   manufacture a FAIL.
//!
//!   Two probe SHAPES, and the difference is load-bearing:
//!
//!   * **Eagerly-built** (binance/bybit/okx via their bridges' `VenueMount::credential_probe`
//!     (`RecordsIdentity`); alpaca via `crates/bridges/alpaca/src/recon_client.rs`'s
//!     `recon_client`): construction is PURE (signer + transport), so the closure just calls
//!     `fetch_balance`.
//!   * **Lazily-built** (ctrader, `crates/bridges/ctrader/src/mount.rs`'s `CtraderVenueMount`):
//!     `crates/bridges/ctrader/src/recon_client.rs`'s `recon_client` CONNECTS while constructing.
//!     Built eagerly, a refused grant returns `None` and the venue gets NO ROW — an expired token
//!     would read as silence, not a FAIL — so construction is deferred into the closure.
//!
//!   A contract row offers either shape via `VenueMount::credential_probe` (`RecordsIdentity` /
//!   `ReadOnly`); that call runs on this thread, outside the probe's bound (the trait doc's rule).
//!
//!   ⚠ **Every probe's error NAMES THE HOST** it could not reach or that refused it: the rehearsal
//!   this leg comes from spent its whole diagnosis budget finding by hand that every Alpaca host
//!   was TCP-unreachable while the mount announced `exec="LIVE"`.
//!
//!   ⚠ **The leg also RECORDS which account each key is**, right AFTER its balance read (on binance
//!   spot the id rides the same `/api/v3/account` body, so it is free), and never changes the
//!   verdict: a key that answered its balance works.
//!   `crate::book_identity::record_authenticated_account` is the whole of it — the site covering a
//!   venue this box ARMS but never MOUNTS (measured on the CI box: 16 `account` rows against a profile
//!   that mounts one).
//!
//! - **disk headroom — ARMED on unix, DECLARED on Windows.** A full disk silently kills the live
//!   `RecorderSink` writer and the journal WAL, and the CI box was measured at ~70% of one filesystem
//!   consumed while this leg was dark. [`free_space_bytes`] uses `rustix::fs::statvfs` — SAFE, in a
//!   crate `Cargo.lock` already held — so no `unsafe` (the workspace forbids `unsafe_code`), no
//!   lint exemption, no new package. ⚠ `statvfs` is POSIX-only: on Windows the probe returns a
//!   DECLARED reason and [`disk_dirs`] renders one honest row; both shipped daemons run on Linux.
//!
//! # ⚠ The ACCOUNT'S TIER gates every leg, and what the tier is doing in a CLOCK leg
//!
//! Every venue set below is derived through [`crate::would_mount_live_under_policy`] — the SAME
//! predicate `make_engine` arms on, via the same `crate::arming`'s `account_tier` — so preflight
//! and mount cannot disagree about what is armed. `policy: None` reads all-`paper` and contacts
//! nothing. Gated on credential PRESENCE alone (`crate::would_mount_live`), a box whose ig account
//! was `paper` still had IG contacted every start — a CREDENTIALED clock read
//! (`crates/bridges/ig/src/mount.rs`'s `IgVenueMount::server_time_ms` sends `X-IG-API-KEY`) beside
//! a SIGNED `fetch_balance` — on an account the operator had said not to touch.
//!
//! ## Account-scoped versus box-scoped, because the legs genuinely differ
//!
//! - **credentials — ACCOUNT-SCOPED** (a signed read; at ctrader an authenticated socket). Gated.
//! - **disk — BOX-SCOPED** (`statvfs`). Ungated: a full disk kills the journal WAL on an all-paper
//!   box too.
//! - **network — BOX-SCOPED** (it resolves `one.one.one.one` and `dns.google`), gated anyway:
//!   [`any_venue_would_mount_live`] gives the reasons.
//! - **clock skew — a box-scoped QUANTITY**, but the CONTACT is with the venue, and the VERDICT
//!   (`crate::server_time::clock_policy_of`) and CONSEQUENCE (demotion to paper) are per-venue: a
//!   paper-tier venue would buy an inert reading with a forbidden contact. Gated.
//!
//!   ⚠ **No box loses a clock check it had.** `clock_venues` was always derived from live intent,
//!   so a credential-less box already had an EMPTY clock leg
//!   (`no_credentials_means_no_clock_venues`, `an_empty_credentials_map_preflights_offline`); the
//!   tier widens that to where the verdict is inert. A real BOX clock check would be a
//!   venue-independent leg (NTP, a neutral host).
//!
//! ## The tier residual, CLOSED by decision 0095
//!
//! The account's tier also picks WHICH network is signed and, since 0095, which host each clock
//! read uses: a fetcher that chose from `{VENUE}_MAINNET` alone read the MAINNET clock for a `demo`
//! account (`crate::server_time`'s "tier discipline" section). Each fetcher is now its bridge's
//! `VenueMount::server_time_ms` (docs/decisions/0096), handed the `tier_permits_live` fold
//! [`run_startup_preflight`] computes per venue as `MountInputs::live_permitted`.
//!
//! ⚠ **The two venue lists are SEPARATE, and merging them was the second half of the defect.** One
//! `PreflightConfig::venues` from `AUTHED_READ_MARKETS` (the credential leg's CEX table, gone with
//! docs/decisions/0096) fed both legs, so the clock ran for the CEX trio only and wiring deribit or
//! hyperliquid changed nothing. Nor can they unify the other way: a credential entry FAILs
//! (degrading the venue), a clock entry only WARNs. So the clock list is LIVE INTENT
//! ([`crate::would_mount_live_under_policy`]) and the credential list is client-buildability, with
//! the SAME tier as a second conjunct.
//!
//! # Cost, and what runs by default
//!
//! With NO credentials (CI, every paper mount) both lists are empty, no `NetProbe` is spawned and
//! **not one network call is made**. **All-paper accounts (no account row, every row inactive or
//! `paper`), or no policy, reach the same state with a FULL credential store** (the fresh-box
//! default). An armed mount pays per venue it
//! is about to mount live: one clock read per wired venue (keyless but for ig; ZERO for a declared
//! one), ONE signed balance read per credentialed venue, sequential, plus the bounded
//! [`DEFAULT_NET_PROBE_WAIT`] — warm, tens of milliseconds. A clock read repeats (at most
//! [`crate::preflight::DEFAULT_CLOCK_SAMPLES`] times) only while inconclusive.
//!
//! ⚠ **The clock leg is a BLOCKING startup stall, bounded by arithmetic.** Seven wired venues ×
//! [`crate::preflight::DEFAULT_CLOCK_SAMPLES`] on the shared 30 s agent
//! (`vike_bridge_core::http`'s `blocking_agent`) would be **7 × 3 × 30 s = 630 s** for a check
//! whose worst finding is a WARN. Three bounds instead: `crate::server_time::CLOCK_READ_TIMEOUT`
//! per fetcher; [`crate::preflight::clock_budget_for`] for the leg, DERIVED from how many venues
//! are read, one shared pool (not restated here — `DEFAULT_CLOCK_BUDGET_MS` is only its FLOOR,
//! `docs/decisions/0027-clock-budget-derived-from-the-roster.md`); and the abandon seam below.
//! Worst case is **budget + one in-flight read**; the arithmetic lives ONCE in
//! [`crate::preflight`]'s "the clock leg is BOUNDED" section, against a healthy cost the live smoke
//! MEASURED at 1781 ms from the CI box.
//!
//! ⚠ **NEITHER of the first two bounds can preempt a wedged NAME RESOLUTION.** `std` has no
//! resolver timeout (`vike_bridge_core::net_probe`'s caveat), so a wedged resolver parks the FIRST
//! clock read at the top of `vike_mount::build_node`; the budget is checked only BETWEEN reads.
//!
//! So a WIRED clock read goes through the SAME [`bounded_probe`] — [`CLOCK_PROBE_TIMEOUT`] per
//! read — and an abandoned read is outcome ② (a WARN). ⚠ NOT a fan-out or a per-venue slice
//! (`docs/decisions/0027-clock-budget-derived-from-the-roster.md`); a DECLARED venue is never
//! probed ([`bounded_server_time_ms`]). [`bounded_probe`] declares the leaked-thread residual.
//!
//! # The credential leg is BOUNDED — without any `ReconClient` gaining a knob
//!
//! ⚠ Unbounded, each `fetch_balance` rode its `ReconClient`'s 30 s agent: `30 s × credentialed
//! venues`, growing with the roster (on the Windows dev box 2026-08-22 a geo-blocked alpaca probe
//! sat ~20 s on one TCP connect).
//!
//! ⚠ **Not a timeout on `ReconClient`**: it is a SHARED HOME (every venue's reconcile path),
//! changed only by one coordinated PR for all consumers, never a per-venue patch for one caller.
//! The bound sits where the blocking call is ISSUED:
//!
//! - [`CREDENTIAL_PROBE_TIMEOUT`] per attempt, via [`bounded_probe`], which ABANDONS an
//!   unresponsive venue's thread ([`NetProbeThread`]'s `Drop` shape).
//! - [`CREDENTIAL_PROBE_ATTEMPTS`]: ONE retry, only after an ANSWERED error, never a timeout.
//! - [`crate::preflight::credential_budget_for`]: the leg total, derived per credentialed venue.
//!
//! Worst case = `MAX_CREDENTIAL_BUDGET_MS + CREDENTIAL_PROBE_ATTEMPTS × CREDENTIAL_PROBE_TIMEOUT`,
//! fixed as the roster grows. The `flags.preflight_skip` row removes every leg.
//!
//! # The report is ENFORCED — and the bound above is what earns that
//!
//! `vike_mount::build_node_with_preflight` ACTS on
//! [`crate::preflight::PreflightReport::venue_disposition`]: a hard per-venue FAIL mounts that
//! venue PAPER for the session.
//!
//! ⚠ It was advisory while ANY authed-read error — a transient timeout too — raised a FAIL. The
//! bound is the confirmed-vs-transient distinction: [`bounded_probe`] yields
//! [`CredentialGap::Unanswered`] for a silence (WARN, never demotes) and
//! [`CredentialGap::Rejected`] only for an answer (demotes). [`crate::preflight`]'s policy section
//! has the full argument and the one accepted residual.
//!
//! # The skip
//!
//! The `flags.preflight_skip` row, folded into the caller's map as `VIKE_PREFLIGHT_SKIP` = the
//! EXACT string `"1"` (see [`crate::preflight::preflight_skipped`]), returns the empty skipped report
//! WITHOUT building a probe, spawning a thread or resolving a credential.

#[cfg(test)]
use std::collections::HashMap;
use std::time::Duration;

#[cfg(doc)]
use vike_bridge_core::{NetProbe, NetProbeThread};

#[cfg(doc)]
use crate::preflight::{CredentialGap, PreflightConfig};

mod clock;
mod credentials;
mod run;

pub use clock::{bounded_server_time_ms, clock_policies, clock_venues, venue_server_time_ms};
pub use credentials::authed_read_probes;
pub use run::{
    any_venue_would_mount_live, free_space_bytes, run_startup_preflight, spawn_net_probe,
    withhold_venue_credentials,
};

#[cfg(doc)]
use credentials::bounded_probe;
#[cfg(doc)]
use run::disk_dirs;

/// How long [`run_startup_preflight`] waits for the [`NetProbe`]'s FIRST round. A warm resolver
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
/// **`crate::server_time::CLOCK_READ_TIMEOUT` + 1 s, DERIVED**: the transport timeout bounds every
/// read ureq CAN bound; this caps only the case it CANNOT, a wedged name resolution (the module
/// doc's abandon paragraph).
///
/// ⚠ The one-second gap is the point: both deadlines start together (this one marginally
/// EARLIER), so equal values would abandon every ordinary timeout a hair before the transport
/// reported it, trading the venue's own error text ("connection timed out to <host>") for our
/// generic "did not answer". One second lets a bounded read report itself first, and abandons an
/// unbounded one a second later instead of never.
///
/// ⚠ **"Reports itself first" holds for six of the seven wired clock reads; the seventh is a
/// declared residual.** hyperliquid's (`crates/bridges/hyperliquid/src/mount.rs`'s
/// `server_time_ms`) goes through `crates/bridges/hyperliquid/src/transport.rs`'s `info`, a RETRY
/// LOOP above ureq (`INFO_MAX_ATTEMPTS` attempts on the 3 s agent, 200 ms then 400 ms backoff), so
/// slow `429`s/`5xx`s can outlast this ceiling and be abandoned with our generic text. Accepted:
/// the retry fires only on a fast-answering throttle or server error, the worst outcome is ② (a
/// WARN), and widening the ceiling would cost every other venue's wedged-resolver case the same
/// seconds on every mount.
pub const CLOCK_PROBE_TIMEOUT: Duration =
    crate::server_time::CLOCK_READ_TIMEOUT.saturating_add(Duration::from_secs(1));

/// Attempts per credential probe: TWO, i.e. one retry, only for an attempt that ANSWERED with an
/// error. The retry makes a FAIL mean "confirmed": a demotion to paper is a real consequence (the
/// module doc's enforcement section), so one blip (a `429`, a reset) must not cause it. A TIMED-OUT
/// probe is never retried: it spent its bound learning nothing, and twice doubles the worst case.
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

#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Instant;

#[cfg(test)]
use vike_exec::recon::ReconClient;

#[cfg(test)]
use crate::preflight::{CredentialGap, ServerTimeGap};

#[cfg(test)]
use clock::abandoned_clock_gap;
#[cfg(test)]
use credentials::{BoundedProbe, CredentialProbe, CredentialProbes};
#[cfg(test)]
use credentials::{authed_read_probe, authed_read_probes_with};
#[cfg(test)]
use credentials::{bounded_probe, identity_recording_probe};
#[cfg(test)]
use run::disk_dirs;

#[path = "startup_tests/mod.rs"]
#[cfg(test)]
mod startup_tests;
