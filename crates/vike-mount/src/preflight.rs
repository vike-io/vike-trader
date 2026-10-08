//! Startup **preflight** — the pure go/no-go gate that runs ONCE at mount, before the first live
//! order, so four failure modes otherwise found by a rejected order (or silent data loss) become a
//! structured report an operator reads at t=0.
//!
//! # Where this is wired
//!
//! [`crate::startup::run_startup_preflight`] is the ONE production caller: it builds the real
//! probes (venue server-time read, each reconcile client's cheap authed read, a spawned
//! [`vike_bridge_core::NetProbe`]) and `vike_mount::build_node` runs it at the top of the node
//! mount, so the GUI and the headless daemon get the same gate. This module stays PURE.
//!
//! # Why this exists
//!
//! Each leg composes existing machinery that nothing ran at startup:
//!
//! - **clock skew** — a bad host clock otherwise surfaced as a binance `-1021 INVALID_TIMESTAMP`
//!   (or a bybit `10002`) on the FIRST signed order. WHICH venues this leg can measure, and WHY
//!   not for the rest, is [`crate::server_time`]'s roster-gated declaration table, never a
//!   fall-through here.
//! - **credential validity** — each reconciled venue's `ReconClient` does a cheap authed read; a
//!   dead key otherwise surfaced at the first `submit`.
//! - **disk headroom** — [`crate::startup::free_space_bytes`]: a SAFE `rustix::fs::statvfs` on
//!   unix (no `unsafe`, no new package), a DECLARED gap on Windows. A full disk silently kills the
//!   live `RecorderSink` writer and the journal WAL; the CI box was measured at ~70% of one filesystem
//!   consumed while this leg was dark.
//! - **network** — [`vike_bridge_core::NetProbe`] is off by default, so a dead resolver surfaced
//!   only as every venue reconnect failing at the hostname step.
//!
//! On mainnet day the first real order must not be the probe.
//!
//! # Pure core, injected probes
//!
//! No I/O here: every observation arrives through [`PreflightProbes`] (object-safe), so the whole
//! decision surface is unit-testable offline. [`FnProbes`]' unwired legs return [`NO_PROBE`],
//! never a silent PASS: clock and disk WARN ("could not measure" is no evidence of a fault),
//! credentials FAIL.
//! ⚠ An unwired credential leg is [`CredentialGap::Rejected`] deliberately: `NO_PROBE` is a defect
//! in OUR wiring, and "the check you asked for does not exist" gets the strict answer.
//! The network leg reads a [`NetProbeHandle`] (one relaxed atomic load): this module must not grow
//! a second probe.
//!
//! # The clock leg has FOUR outcomes, not two
//!
//! "No endpoint wired" (permanent, our code) and "the venue did not answer" (wrong right now) are
//! OPPOSITE facts that once left as one identical WARN. [`ServerTimeGap`] splits them and
//! [`check_clock_skew`] renders four rows:
//!
//! | outcome | status | when |
//! |---|---|---|
//! | ① measured | `Pass` / `Warn` / `Fail` | a number, against that venue's thresholds below |
//! | ② unreachable | [`CheckStatus::Warn`] | the venue publishes a clock and the read FAILED |
//! | ③ declared, nothing at stake | [`CheckStatus::NotApplicable`] | [`crate::server_time`] declares no leg, with a reason, at a venue a drift cannot cost |
//! | ④ declared, ORDERS at stake | [`CheckStatus::Warn`] | no leg, at a venue whose auth signs the clock into the order path |
//!
//! ③ is deliberately NOT a warning: a row firing on every healthy mount is noise, and noise buried
//! the original defect. ② stays a WARN (policy section), its message saying a venue that publishes
//! a clock did not answer. ④ exists because ③'s "nothing to see here" was printed over the
//! roster's ONE order-affecting gap: Polymarket signs `POLY_TIMESTAMP` into every authenticated
//! CLOB request (`crates/bridges/polymarket/src/exec_plane/auth.rs`'s `l2_auth_headers`), an
//! unmeasured hazard. The split is a field on the declaration row, never an accident.
//!
//! # The clock-skew thresholds are PER VENUE, because one global number was measurably false
//!
//! `vike_bridge_core::signer`'s `BinanceHmacSigner` and `BybitV5Signer` hard-code
//! `recv_window: 5000` (ms): a request stamped outside it relative to the VENUE's clock is
//! rejected (binance `-1021`, mapped in `crates/bridges/binance/src/error_codes.rs`; bybit
//! `10002`). The stamp is our LOCAL clock (only binance's exec spawn applies a
//! `Signer::set_offset_ms` correction, to its own clients), so the 5000 ms are shared by skew,
//! one-way latency and venue queueing. For such a venue:
//!
//! - [`DEFAULT_CLOCK_FAIL_MS`] = `2500` — **half**: past it one latency spike pushes a request out
//!   of the window and orders fail intermittently. A no-go for the venue.
//! - [`DEFAULT_CLOCK_WARN_MS`] = `500` — **10%**: orders go through, but the clock is visibly not
//!   disciplined — the drift that becomes a `-1021` an hour later.
//!
//! ⚠ **NOT every venue works that way, and those numbers were wrong in both directions for the
//! rest.** `crate::server_time`'s `clock_policy_of` is the authority; two rules:
//!
//! 1. **Only a venue that REJECTS an order over drift may FAIL** (degrading to paper via
//!    [`PreflightReport::venue_disposition`]). Deribit's `client_credentials` and ig's session
//!    tokens stamp no timestamp; hyperliquid's nonce is valid about a DAY: `fail_ms: None`.
//! 2. **The canary venues warn later**, at [`CANARY_CLOCK_WARN_MS`] = `1000`: a reading there is
//!    dominated by THEIR clock.
//!
//! **The MEASUREMENT behind rule 2** (the CI box, NTP-disciplined, "System clock synchronized: yes",
//! 2026-08-09, midpoint-corrected as [`check_clock_skew`] does; reproduce with
//! `crates/vike-tradehub/tests/server_time_smoke.rs`): the four recv-window venues agree within
//! **tens of ms** — bybit +9..23 (both hosts), okx +10..37, aster +15..33, binance's demo host
//! +236..247 over a ~730 ms round trip — pinning the host clock as good. Hyperliquid's testnet:
//! **40 samples in three minutes ranged -220 ms to -424 ms** at a steady ~285 ms round trip; its
//! mainnet node read -59..-221. The venue's clock wanders, not ours.
//!
//! ⚠ **This corrected a derivation false when it shipped:** `WORST_HEALTHY_SKEW_MS = 288`,
//! const-asserted to clear warn by half again (500 > 432); the next run measured -424 (500 > 636
//! fails), **76 ms from warning on a healthy box**. The readings are pinned as `(skew, rtt)` PAIRS
//! in `crates/vike-tradehub/tests/mount_roster/preflight.rs`'s `MEASURED_HEALTHY_READINGS`,
//! replayed through the real check under each venue's real policy.
//!
//! # A reading is only as sharp as its round trip: the ±rtt/2 floor
//!
//! The midpoint correction assumes a SYMMETRIC round trip; its residual is bounded by ±rtt/2, a
//! FLOOR set by the network (a venue 300 ms away cannot be measured to 50 ms). Asymmetry is real
//! and measured: bybit's demo host once read **182 ms of apparent skew on a 549 ms round trip**
//! while every other rep read 13-23 ms. So the verdict is the band's LOWER bound, `|skew| - rtt/2`
//! floored at zero — what the reading PROVES. A slow link cannot manufacture a warning (binance
//! demo, +247 ms over 757 ms, proves 0); it resolves less, but the host clock is SHARED and the
//! tightest link resolves it (`bybit`'s 195 ms round trip proves what binance's 750 ms cannot).
//! All of it is [`PreflightConfig`] state, so a venue is tightened without touching this module.
//!
//! # Sampling: the minimum round trip of up to [`DEFAULT_CLOCK_SAMPLES`], and only when it matters
//!
//! The band width IS the error, so the SMALLEST round trip is kept — never an average (an
//! asymmetric outlier is a one-way bias) — and a venue is resampled only while its band straddles a
//! threshold. Ordinary cases cost ONE call: healthy and grossly drifted readings are unambiguous.
//!
//! # The clock leg is BOUNDED — the arithmetic, and what bounds it
//!
//! ⚠ Every clock read is a BLOCKING REST call on the mount thread before the first venue mounts.
//! Unbounded: seven venues × [`DEFAULT_CLOCK_SAMPLES`] × the shared `vike_bridge_core::http`
//! agent's 30 s = **630 s** of stall for a check whose worst finding is a WARN. Three bounds:
//!
//! - `crate::server_time`'s `CLOCK_READ_TIMEOUT` (3 s) — the transport's PER-READ ceiling on a
//!   dedicated agent; a request timer, blind to a name resolution that never returns.
//! - `crate::startup::CLOCK_PROBE_TIMEOUT` — the MOUNT's per-read ceiling, one second above: the
//!   mount abandons the read's thread (`crate::startup`'s abandon section, and its residual); an
//!   abandoned read is ② — a WARN naming the bound, never a demotion.
//! - [`clock_budget_for`] — the TOTAL over every venue and resample,
//!   [`PER_VENUE_CLOCK_ALLOWANCE_MS`] per venue read, clamped to [`DEFAULT_CLOCK_BUDGET_MS`] ..
//!   [`MAX_CLOCK_BUDGET_MS`], checked before each read on the injected clock (exact under test). A
//!   venue it never reached gets its own WARN row.
//!
//! ⚠ **The budget is DERIVED because a fixed one rotted**
//! (`docs/decisions/0027-clock-budget-derived-from-the-roster.md`): `5000` flat, sized on a
//! SIX-read **1781 ms** the CI box measurement (2026-08-09); by 2026-08-22 the pinned healthy readings
//! summed to **2973 ms**, one dead venue needed **5973 ms**, and eight venues went unread (aster
//! among them). Pinned by `the_budget_absorbs_one_dead_venue_and_still_reads_the_rest`.
//!
//! Worst case `budget + one in-flight read`: at most
//! `MAX_CLOCK_BUDGET_MS + crate::startup::CLOCK_PROBE_TIMEOUT` = **19 s**, today **12.4 s**.
//! ⚠ The in-flight read is bounded by the mount's ABANDON ceiling, not by
//! `crate::server_time::CLOCK_READ_TIMEOUT`, which cannot preempt a wedged name resolution.
//! ⚠ The CREDENTIAL leg does NOT spend this budget: [`run_preflight`] pushes the clock deadline
//! out by each credential probe's cost. Charging it was a real defect: on the Windows dev box
//! 2026-08-22 a geo-blocked alpaca probe sat ~20 s on a TCP connect, and alpaca/aster/hyperliquid
//! reported their clock "not read" after the clock leg had spent 3.2 s of its 5 s — blaming "an
//! earlier venue's clock read" over healthy rows.
//!
//! # The CREDENTIAL leg is bounded too — and the bound is what MAKES the FAIL mean something
//!
//! ⚠ It once had NO bound: each `fetch_balance` rode its `ReconClient`'s 30 s agent (ctrader also
//! paid a TCP+TLS handshake), `30 s × credentialed venues`, uncapped; the alpaca probe measured
//! above sat ~20 s on one connect. [`crate::startup`] bounds it where the call is ISSUED:
//!
//! - `crate::startup::CREDENTIAL_PROBE_TIMEOUT` — the PER-ATTEMPT ceiling: the mount
//!   `recv_timeout`s the probe's thread and ABANDONS it (it exits on its own agent's timeout).
//! - `crate::startup::CREDENTIAL_PROBE_ATTEMPTS` — one RETRY, only after an ANSWERED error; a
//!   timeout is never retried.
//! - [`credential_budget_for`] — the TOTAL, derived like [`clock_budget_for`].
//!
//! Worst case `MAX_CREDENTIAL_BUDGET_MS + CREDENTIAL_PROBE_ATTEMPTS × CREDENTIAL_PROBE_TIMEOUT`,
//! fixed as the roster grows. ⚠ The timeout is ALSO what makes the enforcement below defensible:
//! it separates "the venue refused us" from "we never heard back", and only the first demotes.
//!
//! # Policy: a venue FAIL degrades that venue to paper — and it is now ENFORCED
//!
//! Mirrors the live gate ("absent credentials ARE the live gate", root `CLAUDE.md`). A per-venue
//! hard FAIL marks that venue [`VenueDisposition::Paper`] via
//! [`PreflightReport::venue_disposition`]; the app always starts. A GLOBAL hard FAIL (disk,
//! internet) flips [`PreflightReport::go`] to `false`. This module only DECIDES (no panic, log or
//! mutation); `vike_mount::build_node_with_preflight` is the one site that acts.
//!
//! ⚠ **The disposition used to be computed and thrown away:** `build_node` logged a `FAIL` and
//! mounted LIVE, because "a per-venue FAIL is raised by ANY authed-read error, a transient timeout
//! included". [`CredentialGap`] is that confirmed-vs-transient distinction, so it is enforced:
//!
//! | evidence | status | mount |
//! |---|---|---|
//! | the venue ANSWERED and refused the signed read ([`CredentialGap::Rejected`]) | `Fail` | PAPER |
//! | the probe did not answer inside its bound ([`CredentialGap::Unanswered`]) | `Warn` | live, unchanged |
//! | a PROVEN skew past a venue's own `fail_ms` | `Fail` | PAPER |
//! | anything we merely could not measure (②/③/④, a budget miss, an unqueryable disk) | `Warn`/`N/A` | live, unchanged |
//!
//! Refused and ABSENT keys are the same fact, and absent keys already demote.
//!
//! ⚠ **Only a PER-VENUE fail enforces; a global no-go does NOT ground the process**: a daemon that
//! refuses to start under `Restart=on-failure` crash-loops (`crates/vike-datahub/src/recorder.rs`'s
//! `--exit-on-silence` doc). A global FAIL is logged at `error` and the mount proceeds.
//!
//! ⚠ **Accepted residual:** a FAST non-auth error (`429`, `503`) inside the bound reads as Rejected
//! and demotes, after one retry ([`crate::startup`]'s `CREDENTIAL_PROBE_ATTEMPTS`) — PAPER is the
//! safe side, but the evidence is of a broken venue, not broken keys. Reopens with a probe that
//! reads the venue's auth-vs-throttle distinction; meanwhile `VIKE_PREFLIGHT_SKIP=1` (every leg).
//!
//! ⚠ **An UNMEASURABLE clock never degrades anything — a decision.** ②-④ never demote or flip
//! the go bit: a public endpoint can be briefly slow or rate-limited, and a daemon refusing to
//! start over it is taken down by its own preflight (same argument). A MEASURED skew past the
//! venue's [`ClockPolicy`] `fail_ms` is evidence: it FAILs, and that venue mounts PAPER (table).
//!
//! # The skip override
//!
//! [`preflight_skipped`] is `true` iff [`PREFLIGHT_SKIP_ENV`] is the EXACT string `"1"` in the REAL
//! process env: an EMPTY report ([`PreflightReport::skipped`], `go() == true`) WITHOUT a probe.
//!
//! # Secrets
//!
//! A probe's error string is embedded verbatim in a [`CheckReport::message`], which may be printed
//! or logged: probes MUST NOT put an API key, secret or passphrase in it.

mod checks;
mod clock;
mod config;
mod probes;
mod report;
mod run;

pub use checks::{check_credentials, check_disk_headroom, check_network};
pub use clock::{ClockPolicy, ClockSample, check_clock_skew};
pub use config::{
    CANARY_CLOCK_WARN_MS, CHECK_CLOCK_SKEW, CHECK_CREDENTIALS, CHECK_DISK, CHECK_NETWORK,
    DEFAULT_CLOCK_BUDGET_MS, DEFAULT_CLOCK_FAIL_MS, DEFAULT_CLOCK_SAMPLES, DEFAULT_CLOCK_WARN_MS,
    DEFAULT_CREDENTIAL_BUDGET_MS, DEFAULT_DISK_FAIL_BYTES, DEFAULT_DISK_WARN_BYTES,
    MAX_CLOCK_BUDGET_MS, MAX_CREDENTIAL_BUDGET_MS, NO_PROBE, PER_VENUE_CLOCK_ALLOWANCE_MS,
    PER_VENUE_CREDENTIAL_ALLOWANCE_MS, PREFLIGHT_SKIP_ENV, PreflightConfig, REMEDY_CLOCK,
    clock_budget_for, credential_budget_for,
};
pub use probes::{CredentialGap, FnProbes, PreflightProbes, ServerTimeGap};
pub use report::{CheckReport, CheckStatus, PreflightReport, VenueDisposition};
pub use run::{preflight_skipped, run_preflight, run_preflight_gated};

#[cfg(doc)]
use vike_bridge_core::NetProbeHandle;

#[cfg(test)]
use config::{REMEDY_CLOCK_UNMEASURED, REMEDY_CLOCK_UNREACHABLE};
#[cfg(test)]
use report::fmt_bytes;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::path::{Path, PathBuf};

#[path = "preflight_tests/mod.rs"]
#[cfg(test)]
mod preflight_tests;
