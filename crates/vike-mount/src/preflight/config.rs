//! The preflight's numbers and words: budgets, thresholds, check names, remedies, the config.

use std::collections::HashMap;
use std::path::PathBuf;

use super::clock::ClockPolicy;
#[cfg(doc)]
use super::probes::FnProbes;

/// Skips preflight entirely; exact string `"1"` only (module doc).
pub const PREFLIGHT_SKIP_ENV: &str = "VIKE_PREFLIGHT_SKIP";

/// Absolute skew (ms) at or beyond which the clock check WARNs: 10% of the signers'
/// `recv_window: 5000` (derivation: module doc).
pub const DEFAULT_CLOCK_WARN_MS: i64 = 500;

/// Absolute skew (ms) at or beyond which the clock check FAILs (degrading the venue to paper): half
/// the signers' `recv_window: 5000` (derivation: module doc).
pub const DEFAULT_CLOCK_FAIL_MS: i64 = 2_500;

/// WARN threshold where auth cannot reject an order over drift — deribit, ig (no timestamp),
/// hyperliquid (nonce window measured in days) — so a reading is dominated by THEIR clock:
/// hyperliquid's testnet ranged -220..-424 ms over 40 samples from NTP-disciplined the CI box in three
/// minutes (2026-08-09) while bybit/okx/aster read 9-37 ms, binance 236-247 (rtt 723-757). 1000
/// ms = 2.4x that worst reading (~3.5x the 283 ms it proves after the ±rtt/2 floor), far below an
/// undisciplined host's seconds. WARN only: `crate::server_time`'s `clock_policy_of` gives no FAIL
/// (a fabricated consequence).
pub const CANARY_CLOCK_WARN_MS: i64 = 1_000;

/// Budget (ms) for the WHOLE clock leg, every venue and resample (module doc, "the clock leg is
/// BOUNDED": 630 s of stall without it; a healthy mount measured 1781 ms); [`clock_budget_for`]'s
/// floor.
pub const DEFAULT_CLOCK_BUDGET_MS: i64 = 5_000;

/// What each WIRED venue buys the clock leg: the slowest healthy round trip recorded here is
/// binance's **757 ms**, so ~60% margin; the budget is this times the venues actually read.
///
/// ⚠ A PER-VENUE allowance, not a per-venue DEADLINE: ONE shared pool consumed in roster order, so
/// a resampling (inconclusive) venue may spend a neighbour's share — a conclusive answer beats an
/// even split. The sizing must still let the pass finish: see
/// `the_budget_absorbs_one_dead_venue_and_still_reads_the_rest`.
pub const PER_VENUE_CLOCK_ALLOWANCE_MS: i64 = 1_200;

/// Ceiling for the derived budget: the leg runs before a trading daemon arms, so roster growth hits
/// a wall somebody chose.
pub const MAX_CLOCK_BUDGET_MS: i64 = 15_000;

/// The clock leg's total budget for `wired` readable venues — DERIVED, so a venue joining the
/// roster buys time instead of squeezing the ones behind it. The fixed 5000 ms it replaced: by
/// 2026-08-22 the pinned healthy reads summed to 2973 ms (~59%), and one unreachable venue (a full
/// [`crate::server_time::CLOCK_READ_TIMEOUT`]) left eight venues unread
/// (`docs/decisions/0027-clock-budget-derived-from-the-roster.md`).
#[must_use]
pub fn clock_budget_for(wired: usize) -> i64 {
    let derived = (wired as i64).saturating_mul(PER_VENUE_CLOCK_ALLOWANCE_MS);
    derived.clamp(DEFAULT_CLOCK_BUDGET_MS, MAX_CLOCK_BUDGET_MS)
}

/// [`credential_budget_for`]'s floor: a one- or two-venue mount is never squeezed.
pub const DEFAULT_CREDENTIAL_BUDGET_MS: i64 = 6_000;

/// What each CREDENTIALED venue buys: a signed balance read is one REST round trip (tens to low
/// hundreds of ms warm), so 3 s is an order of magnitude of margin.
pub const PER_VENUE_CREDENTIAL_ALLOWANCE_MS: i64 = 3_000;

/// Ceiling for the derived credential budget, the twin of [`MAX_CLOCK_BUDGET_MS`] (this leg was
/// once UNBOUNDED).
pub const MAX_CREDENTIAL_BUDGET_MS: i64 = 20_000;

/// The credential leg's total budget for `credentialed` venues — DERIVED, like
/// [`clock_budget_for`], so a growing roster never silently drops the venues at the end.
#[must_use]
pub fn credential_budget_for(credentialed: usize) -> i64 {
    let derived = (credentialed as i64).saturating_mul(PER_VENUE_CREDENTIAL_ALLOWANCE_MS);
    derived.clamp(DEFAULT_CREDENTIAL_BUDGET_MS, MAX_CREDENTIAL_BUDGET_MS)
}

/// Reads of one venue before concluding; almost always spent as one: a sample is retaken only while
/// its ±rtt/2 band straddles a threshold, and the smallest round trip wins. Three lets a single
/// asymmetric outlier (the shape actually measured: one bad rep in six) be outvoted.
pub const DEFAULT_CLOCK_SAMPLES: usize = 3;

/// Free bytes below which a watched directory WARNs: more than a day of live tick/book recording.
pub const DEFAULT_DISK_WARN_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Free bytes below which a watched directory FAILs: the recorder flush and the journal WAL are one
/// busy session away from failing mid-write.
pub const DEFAULT_DISK_FAIL_BYTES: u64 = 1024 * 1024 * 1024;

/// Check name: per-venue clock skew.
pub const CHECK_CLOCK_SKEW: &str = "clock_skew";
/// Check name: per-venue credential validity.
pub const CHECK_CREDENTIALS: &str = "credentials";
/// Check name: per-directory disk headroom.
pub const CHECK_DISK: &str = "disk_headroom";
/// Check name: the single, global network leg.
pub const CHECK_NETWORK: &str = "network";

/// What an unwired [`FnProbes`] leg returns. Never rendered as a PASS: the clock and disk legs
/// WARN, the credential leg FAILs.
pub const NO_PROBE: &str = "no probe wired";

/// The FALLBACK remedy for a measured out-of-band skew, when [`PreflightConfig::clock_policies`]
/// declares no per-venue text. Says nothing about recv windows: that is per venue
/// (`vike_bridge_core::venue_mount`'s `ClockRisk`) and false on half the roster. Public for
/// `crates/vike-tradehub/tests/mount_roster.rs` (docs/decisions/0096).
pub const REMEDY_CLOCK: &str = "sync the host clock (NTP / w32tm) — the host's own time is wrong, whatever the venue does \
     with it";
/// ② — a venue that publishes a clock did not answer.
pub(super) const REMEDY_CLOCK_UNREACHABLE: &str = "this venue PUBLISHES a server-time endpoint and it did not answer — check egress and the \
     venue's status page before mounting live; this is not the same as a venue that publishes none";
/// ④ — no leg, at a venue whose auth binds the clock into the order path: nothing to re-run.
pub(super) const REMEDY_CLOCK_UNMEASURED: &str = "verify this host's clock by other means before trading this venue live (`timedatectl` / \
     `w32tm /query /status`) — this row is a DISCLOSED gap in the preflight, not a measurement";
/// The budget ran out before this venue's turn ([`DEFAULT_CLOCK_BUDGET_MS`]).
pub(super) const REMEDY_CLOCK_BUDGET: &str = "an earlier venue's clock read consumed the leg's whole budget — check that venue's row, or \
     raise PreflightConfig::clock_budget_ms if every venue here is genuinely this slow";
pub(super) const REMEDY_CREDENTIALS: &str = "this venue is MOUNTED PAPER for this session — check its credentials in the store \
     `vike-cli secrets path` prints (the common shape is {VENUE}_{SIM|DEMO|LIVE}_API_KEY/\
     _API_SECRET, but several venues use their own; this row's own message names the keys the probe \
     actually looked for) and restart";
/// The probe never came back inside its bound: it never demotes, so the remedy says STILL LIVE.
pub(super) const REMEDY_CREDENTIALS_UNANSWERED: &str = "this venue is STILL MOUNTED LIVE — a probe that did not answer proves nothing about the keys, \
     so it never demotes; check egress to this venue's host and the venue's status page, because an \
     order will take the same path this probe could not complete";
/// The budget ran out before this venue's turn ([`credential_budget_for`]).
pub(super) const REMEDY_CREDENTIALS_BUDGET: &str = "an earlier venue's credential probe consumed the leg's whole budget — check that venue's row; \
     this venue is STILL MOUNTED LIVE, unchecked";
pub(super) const REMEDY_DISK: &str =
    "free space or repoint the directory — the live recorder and the journal WAL write here";
pub(crate) const REMEDY_DISK_UNKNOWN: &str =
    "could not query free space; check that the directory exists";
pub(super) const REMEDY_NETWORK: &str =
    "no configured host resolves — check the local resolver / VPN before mounting live venues";
pub(super) const REMEDY_NETWORK_UNKNOWN: &str =
    "network liveness unknown; spawn a vike_bridge_core::NetProbe to make it observable";

/// Thresholds plus the things to check; the report keeps the lists' order (deterministic output).
#[derive(Debug, Clone)]
pub struct PreflightConfig {
    /// Venue slugs to run the CLOCK leg against. Empty = no clock legs.
    ///
    /// ⚠ **Separate from [`PreflightConfig::credential_venues`] on purpose** (one list was a real
    /// defect): an unwired credential probe FAILs by design, so a venue with no authed read there
    /// MANUFACTURES a failure, while the clock leg needs no key. One list meant
    /// `authed_read_clients().keys()` (the crypto-CEX trio), so every other clock went unmeasured.
    pub clock_venues: Vec<String>,
    /// Venue slugs to run the CREDENTIAL leg against — only venues the caller can perform a cheap
    /// authed read for. Empty = no credential legs.
    pub credential_venues: Vec<String>,
    /// Per-venue thresholds + remedy for a MEASURED skew; no entry falls back to
    /// `(clock_warn_ms, Some(clock_fail_ms), REMEDY_CLOCK)`. Per venue because the consequence is
    /// (binance/bybit/okx/aster reject over drift, deribit and ig cannot, hyperliquid's nonce
    /// window is a DAY): one "5000 ms recvWindow" line was false on half the roster. Filled by
    /// `crate::startup::clock_policies`: `crate::server_time::clock_policy` per wired venue.
    pub clock_policies: HashMap<String, ClockPolicy>,
    /// Reads per venue before concluding (module doc, sampling). `0` is treated as `1`.
    pub clock_samples: usize,
    /// TOTAL clock-leg budget (ms), measured on the injected clock; `0`/negative = unbounded.
    pub clock_budget_ms: i64,
    /// TOTAL credential-leg budget (ms), measured on the injected clock; `0`/negative = unbounded.
    /// A real mount site derives [`credential_budget_for`].
    pub credential_budget_ms: i64,
    /// `(label, path)` directories to check headroom on (journal, hist store). Empty = none.
    pub dirs: Vec<(String, PathBuf)>,
    /// Absolute skew (ms) at or beyond which the clock check warns.
    pub clock_warn_ms: i64,
    /// Absolute skew (ms) at or beyond which the clock check fails.
    pub clock_fail_ms: i64,
    /// Free bytes below which a directory warns.
    pub disk_warn_bytes: u64,
    /// Free bytes below which a directory fails.
    pub disk_fail_bytes: u64,
}

impl Default for PreflightConfig {
    fn default() -> Self {
        PreflightConfig {
            clock_venues: Vec::new(),
            credential_venues: Vec::new(),
            clock_policies: HashMap::new(),
            clock_samples: DEFAULT_CLOCK_SAMPLES,
            clock_budget_ms: DEFAULT_CLOCK_BUDGET_MS,
            credential_budget_ms: DEFAULT_CREDENTIAL_BUDGET_MS,
            dirs: Vec::new(),
            clock_warn_ms: DEFAULT_CLOCK_WARN_MS,
            clock_fail_ms: DEFAULT_CLOCK_FAIL_MS,
            disk_warn_bytes: DEFAULT_DISK_WARN_BYTES,
            disk_fail_bytes: DEFAULT_DISK_FAIL_BYTES,
        }
    }
}
