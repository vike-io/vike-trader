//! `NetProbe` — an opt-in **background internet-liveness monitor**: a slow-cadence thread that
//! DNS-resolves a couple of well-known names and maintains a lock-free `AtomicBool internet_up`
//! plus the timestamps of its last transitions (net-hardening).
//!
//! ## Why this exists next to [`crate::connectivity`]
//! The two are complements, not duplicates, and answer different questions at different times:
//!
//! | | [`crate::connectivity`] ([`ConnectivityProbe`](crate::connectivity::ConnectivityProbe)) | this module ([`NetProbe`]) |
//! |---|---|---|
//! | when | ON DEMAND, at the instant a venue socket drops | CONTINUOUSLY, on a slow timer |
//! | shape | one blocking call, returns a verdict | background thread + shared readable state |
//! | probes | TCP connect to literal **IPs** | **DNS resolve** of well-known **names** |
//! | answers | "this disconnect — our fault or the venue's?" | "is our internet up right now, and since when?" |
//!
//! The DNS angle is the substantive difference, not a stylistic one. `connectivity` uses literal IPs
//! **on purpose** (see its `DEFAULT_NEUTRAL_ENDPOINTS` note) so a probe never blocks on name
//! resolution — which means it structurally CANNOT observe a resolver failure. But "TCP to 1.1.1.1
//! works, yet nothing resolves" is a real and nasty local-network state: every venue reconnect fails
//! at the hostname step while a literal-IP probe cheerfully reports the network healthy. This module
//! covers exactly that blind spot by resolving NAMES, and keeps the answer standing by so any number
//! of readers can sample it for the price of one relaxed atomic load — no I/O, no blocking, no
//! probing on the reader's thread.
//!
//! ## OFF by default — nothing here runs unless asked
//! Constructing a [`NetProbe`] spawns nothing. A thread exists only after an explicit
//! [`NetProbe::spawn`], and no other code in this crate calls it, so an unmodified build is
//! byte-identical to one without this module. [`NetProbe::probe_once`] /
//! [`NetProbe::probe_once_with`] also let a caller drive the fold synchronously with no thread at
//! all (which is how the unit tests exercise the whole state machine offline).
//!
//! ## The state machine: slow to condemn, quick to forgive
//! Each round probes the configured hosts and folds them with the same rule as its sibling —
//! reachable if ANY host resolves, so one dead name is never on its own mistaken for a dead
//! internet. That observation then feeds a debounced flip:
//!
//! - a failure round increments a consecutive-failure counter; the flag flips **down** only once
//!   that counter reaches `failures_before_down` (default [`DEFAULT_FAILURES_BEFORE_DOWN`]), so a
//!   single DNS hiccup cannot flap a false outage;
//! - any successful round clears the counter and flips **up** immediately — recovery is never
//!   delayed, because a stale "down" is the more damaging error for a consumer to act on.
//!
//! Every flip stamps a transition timestamp ([`NetProbeHandle::last_up_ms`] /
//! [`NetProbeHandle::last_down_ms`]) and bumps [`NetProbeHandle::transitions`], so a consumer can
//! tell "down for 40 seconds" from "flapping every round" without keeping its own history.
//!
//! ## Clock discipline & purity
//! Clock-free in the same sense as [`crate::stream_health`]: every fold takes `now_ms` as a
//! parameter, so transitions are unit-tested against a mock clock with zero real sleeps. Only the
//! spawned thread reads the wall clock ([`wall_clock_ms`]). The reachability test is likewise an
//! injectable seam ([`NetProbe::probe_once_with`]), which is how the tests drive the machine with a
//! scripted resolver and never touch the network.
//!
//! ## Cost, dependencies, and one honest caveat
//! Pure `std` — [`ToSocketAddrs`], atomics, one thread — so no new dependency and no TLS is
//! reachable from here. A round is a couple of DNS lookups every [`DEFAULT_PROBE_INTERVAL`] by
//! default; readers pay a single relaxed load. **Caveat:** `std` offers no timeout on name
//! resolution, so a wedged resolver can park the probe thread for however long the OS resolver takes
//! to give up. That is precisely why this belongs on its own dedicated thread and is bounded to a
//! slow cadence — it must never be called from a venue's read loop, and never from the vike-core
//! fold. Shutdown uses the crate's [`sleep_unless_stopped`], so a stop is honoured within ~100 ms
//! except across one in-flight resolve.

use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::user_data::sleep_unless_stopped;

/// Two well-known, independently-operated names to resolve. NAMES on purpose (the whole point of
/// this module vs [`crate::connectivity`]): resolving them exercises the local resolver, so a broken
/// DNS path is observable rather than invisible. Two different operators (Cloudflare, Google) so one
/// operator's outage cannot alone read as "our internet is down" — the fold needs BOTH to fail.
pub const DEFAULT_PROBE_HOSTS: &[&str] = &[
    "one.one.one.one", // Cloudflare
    "dns.google",      // Google
];

/// Port paired with each host for [`ToSocketAddrs`]. Resolution is all that is used — nothing ever
/// connects to it — but a real port keeps the resolved `SocketAddr`s meaningful for a caller that
/// reuses [`dns_resolves`].
pub const PROBE_PORT: u16 = 443;

/// Default cadence between probe rounds. Deliberately slow: this monitors a background condition
/// that changes on human timescales, and each round costs real DNS traffic.
pub const DEFAULT_PROBE_INTERVAL: Duration = Duration::from_secs(30);

/// Default consecutive failed rounds before the flag flips down. `2` rides out a single transient
/// resolver hiccup while still condemning a genuine outage within ~1 interval of extra delay.
pub const DEFAULT_FAILURES_BEFORE_DOWN: u32 = 2;

/// Epoch-ms wall clock — the ONLY clock read in this module, and only by the spawned thread. Every
/// fold takes `now_ms` as a parameter instead, so the state machine stays deterministic under test.
/// Saturates rather than panicking on a pre-epoch clock.
#[must_use]
pub fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The real reachability test: does `host` RESOLVE? `true` iff name resolution yields ≥1 address.
/// Note a literal IP resolves trivially (std parses it without a DNS lookup), so passing one here
/// tests nothing about the resolver — pass NAMES, which is what [`DEFAULT_PROBE_HOSTS`] are.
///
/// No timeout is available on `std` name resolution; see the module doc's caveat.
#[must_use]
pub fn dns_resolves(host: &str) -> bool {
    match (host, PROBE_PORT).to_socket_addrs() {
        Ok(mut addrs) => addrs.next().is_some(),
        Err(_) => false,
    }
}

/// A flip of the internet-up flag, returned by the fold so a caller can log/act on the EDGE rather
/// than poll for it. `None` from a fold means "no change this round".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetTransition {
    /// The flag flipped to down: `failures_before_down` consecutive rounds resolved nothing.
    WentDown {
        /// `now_ms` of the round that tripped it.
        at_ms: i64,
    },
    /// The flag flipped back to up: a round resolved at least one host.
    CameUp {
        /// `now_ms` of the round that recovered it.
        at_ms: i64,
    },
}

impl NetTransition {
    /// The timestamp this transition happened at.
    #[must_use]
    pub fn at_ms(self) -> i64 {
        match self {
            NetTransition::WentDown { at_ms } | NetTransition::CameUp { at_ms } => at_ms,
        }
    }
}

/// The shared, lock-free state a [`NetProbe`] maintains and any number of [`NetProbeHandle`]s read.
///
/// **Single-writer.** [`Self::observe`] takes `&self` (the state is shared behind an `Arc`) but is
/// only ever called from the ONE probing thread — or, in tests, from the one thread driving it. Its
/// read-modify-write of the failure counter is therefore race-free by construction; readers only
/// ever load. Every field is `Relaxed`: these are independent diagnostic scalars, not a lock, and no
/// reader derives memory safety from their ordering.
#[derive(Debug)]
pub struct NetProbeState {
    /// The headline flag. Starts `true` — optimistic, so a reader that samples before the first
    /// round completes never sees a phantom outage. Pair with [`Self::has_probed`] to tell
    /// "confirmed up" from "not yet checked".
    up: AtomicBool,
    /// Completed probe rounds. `0` = never probed.
    checks: AtomicU64,
    /// Total flips of `up` (either direction) — lets a consumer spot flapping.
    transitions: AtomicU64,
    /// Consecutive failed rounds; the debounce counter behind `failures_before_down`.
    consecutive_failures: AtomicU32,
    /// `now_ms` of the most recent completed round (`0` = never).
    last_check_ms: AtomicI64,
    /// `now_ms` of the most recent flip INTO up (`0` = never flipped up since construction).
    last_up_ms: AtomicI64,
    /// `now_ms` of the most recent flip INTO down (`0` = never flipped down).
    last_down_ms: AtomicI64,
    /// Set to stop the spawned thread's loop.
    stop: AtomicBool,
}

impl Default for NetProbeState {
    fn default() -> Self {
        NetProbeState {
            up: AtomicBool::new(true),
            checks: AtomicU64::new(0),
            transitions: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
            last_check_ms: AtomicI64::new(0),
            last_up_ms: AtomicI64::new(0),
            last_down_ms: AtomicI64::new(0),
            stop: AtomicBool::new(false),
        }
    }
}

impl NetProbeState {
    /// Fold ONE round's observation into the state at `now_ms`, returning the flip it caused (if
    /// any). `reachable` is the already-folded verdict over the configured hosts (any host resolved).
    /// This is the whole state machine: debounced down, immediate up. Single-writer (see the type
    /// doc).
    pub fn observe(
        &self,
        reachable: bool,
        now_ms: i64,
        failures_before_down: u32,
    ) -> Option<NetTransition> {
        self.checks.fetch_add(1, Ordering::Relaxed);
        self.last_check_ms.store(now_ms, Ordering::Relaxed);

        if reachable {
            // Quick to forgive: any success clears the debounce and recovers immediately.
            self.consecutive_failures.store(0, Ordering::Relaxed);
            if self.up.load(Ordering::Relaxed) {
                return None;
            }
            self.up.store(true, Ordering::Relaxed);
            self.last_up_ms.store(now_ms, Ordering::Relaxed);
            self.transitions.fetch_add(1, Ordering::Relaxed);
            return Some(NetTransition::CameUp { at_ms: now_ms });
        }

        // Slow to condemn: only flip down once the debounce threshold is reached. `saturating_add`
        // keeps a very long outage from wrapping the counter back under the threshold.
        let failures = self.consecutive_failures.load(Ordering::Relaxed).saturating_add(1);
        self.consecutive_failures.store(failures, Ordering::Relaxed);
        // A threshold of 0 is normalised to 1 — "flip on the first failure" — so a mis-configured
        // 0 can never mean "never flip down".
        if failures < failures_before_down.max(1) || !self.up.load(Ordering::Relaxed) {
            return None;
        }
        self.up.store(false, Ordering::Relaxed);
        self.last_down_ms.store(now_ms, Ordering::Relaxed);
        self.transitions.fetch_add(1, Ordering::Relaxed);
        Some(NetTransition::WentDown { at_ms: now_ms })
    }

    /// Whether at least one round has completed (so `internet_up` is a measurement, not the
    /// optimistic initial value).
    #[must_use]
    pub fn has_probed(&self) -> bool {
        self.checks.load(Ordering::Relaxed) > 0
    }
}

/// A cheap, cloneable READER of a probe's state. Hand one to anything that wants to know whether the
/// box is online — a venue reconnect loop deciding how loudly to complain, a health surface, an
/// operator view. Every accessor is a single relaxed atomic load: no I/O, no blocking, no probing.
#[derive(Debug, Clone)]
pub struct NetProbeHandle {
    state: Arc<NetProbeState>,
}

impl NetProbeHandle {
    /// The headline flag: is the internet up? `true` before the first round completes (optimistic —
    /// see [`Self::has_probed`]).
    #[must_use]
    pub fn internet_up(&self) -> bool {
        self.state.up.load(Ordering::Relaxed)
    }

    /// Has any round completed yet? `false` means [`Self::internet_up`] is still the optimistic
    /// initial value rather than an observation.
    #[must_use]
    pub fn has_probed(&self) -> bool {
        self.state.has_probed()
    }

    /// Completed probe rounds.
    #[must_use]
    pub fn checks(&self) -> u64 {
        self.state.checks.load(Ordering::Relaxed)
    }

    /// Total flips of the flag in either direction — repeated flips over a short window mean a
    /// flapping link, which reads very differently from one clean outage.
    #[must_use]
    pub fn transitions(&self) -> u64 {
        self.state.transitions.load(Ordering::Relaxed)
    }

    /// `now_ms` of the most recent completed round; `0` if never probed.
    #[must_use]
    pub fn last_check_ms(&self) -> i64 {
        self.state.last_check_ms.load(Ordering::Relaxed)
    }

    /// `now_ms` of the most recent flip INTO up; `0` if it has never flipped up (i.e. it has been up
    /// since construction, or has never recovered).
    #[must_use]
    pub fn last_up_ms(&self) -> i64 {
        self.state.last_up_ms.load(Ordering::Relaxed)
    }

    /// `now_ms` of the most recent flip INTO down; `0` if it has never flipped down.
    #[must_use]
    pub fn last_down_ms(&self) -> i64 {
        self.state.last_down_ms.load(Ordering::Relaxed)
    }

    /// How long the internet has been down as of `now_ms`, or `None` if it is up. Derived from
    /// [`Self::last_down_ms`], so it is meaningful exactly while the flag is down.
    #[must_use]
    pub fn downtime_ms(&self, now_ms: i64) -> Option<i64> {
        if self.internet_up() {
            return None;
        }
        Some(now_ms - self.state.last_down_ms.load(Ordering::Relaxed))
    }
}

/// How a [`NetProbe`] is configured. Built through [`NetProbe::new`] / [`NetProbe::with_defaults`].
#[derive(Debug, Clone)]
pub struct NetProbeConfig {
    /// Names to resolve. Non-empty (enforced by [`NetProbe::new`]).
    pub hosts: Vec<String>,
    /// Cadence between rounds.
    pub interval: Duration,
    /// Consecutive failed rounds before flipping down (`0` is treated as `1`).
    pub failures_before_down: u32,
}

impl Default for NetProbeConfig {
    fn default() -> Self {
        NetProbeConfig {
            hosts: DEFAULT_PROBE_HOSTS.iter().map(|h| (*h).to_string()).collect(),
            interval: DEFAULT_PROBE_INTERVAL,
            failures_before_down: DEFAULT_FAILURES_BEFORE_DOWN,
        }
    }
}

/// The probe itself: configuration + the shared state it maintains. **Constructing one starts
/// nothing** — call [`Self::spawn`] for the background thread, or drive [`Self::probe_once`]
/// yourself.
#[derive(Debug)]
pub struct NetProbe {
    cfg: NetProbeConfig,
    state: Arc<NetProbeState>,
}

impl NetProbe {
    /// Build a probe from `cfg`. Returns `None` when `hosts` is empty — an empty probe could only
    /// ever fold to "unreachable" and would condemn a perfectly good link, so "nothing configured"
    /// is "no probe" (which is also how the feature stays off by default).
    #[must_use]
    pub fn new(cfg: NetProbeConfig) -> Option<Self> {
        if cfg.hosts.is_empty() {
            return None;
        }
        Some(NetProbe { cfg, state: Arc::new(NetProbeState::default()) })
    }

    /// The zero-config opt-in: [`DEFAULT_PROBE_HOSTS`] at [`DEFAULT_PROBE_INTERVAL`]. Always valid.
    #[must_use]
    pub fn with_defaults() -> Self {
        NetProbe { cfg: NetProbeConfig::default(), state: Arc::new(NetProbeState::default()) }
    }

    /// A cloneable reader of this probe's state. Valid whether or not a thread was ever spawned.
    #[must_use]
    pub fn handle(&self) -> NetProbeHandle {
        NetProbeHandle { state: Arc::clone(&self.state) }
    }

    /// The configured hosts.
    #[must_use]
    pub fn hosts(&self) -> &[String] {
        &self.cfg.hosts
    }

    /// Run ONE round with an INJECTED resolver — the test/customisation seam. `resolve(host) -> true`
    /// means that host resolved. Folds the hosts with "reachable if ANY resolved" (short-circuiting
    /// on the first success, so the healthy case costs one lookup), then applies the debounce state
    /// machine at `now_ms`. Returns the flip it caused, if any.
    pub fn probe_once_with(
        &self,
        mut resolve: impl FnMut(&str) -> bool,
        now_ms: i64,
    ) -> Option<NetTransition> {
        let reachable = self.cfg.hosts.iter().any(|h| resolve(h.as_str()));
        self.state.observe(reachable, now_ms, self.cfg.failures_before_down)
    }

    /// Run ONE round with the REAL resolver ([`dns_resolves`]) at `now_ms`. Blocking (name
    /// resolution); must not be called from a hot path — see the module doc's caveat.
    pub fn probe_once(&self, now_ms: i64) -> Option<NetTransition> {
        self.probe_once_with(dns_resolves, now_ms)
    }

    /// Signal the spawned thread to stop. Safe to call whether or not one was spawned.
    pub fn stop(&self) {
        self.state.stop.store(true, Ordering::Relaxed);
    }

    /// Spawn the background monitor — **the only place in this module that creates a thread**, and
    /// nothing in this crate calls it. The thread probes immediately, then every `interval`, until
    /// stopped. Each flip is disclosed on ONE `tracing` line (a fault-transition boundary, not
    /// per-message logging), and the returned [`NetProbeThread`] owns the join handle.
    #[must_use]
    pub fn spawn(self) -> NetProbeThread {
        let state = Arc::clone(&self.state);
        let join = std::thread::Builder::new()
            .name("net-probe".to_string())
            .spawn(move || self.run())
            .ok();
        NetProbeThread { state, join }
    }

    /// The spawned thread's body: probe, disclose any flip, nap (stop-responsive), repeat.
    fn run(self) {
        while !self.state.stop.load(Ordering::Relaxed) {
            if let Some(flip) = self.probe_once(wall_clock_ms()) {
                let hosts = self.cfg.hosts.len();
                match flip {
                    // Losing the internet is the actionable, louder case: it can masquerade as every
                    // venue dying at once.
                    NetTransition::WentDown { at_ms } => tracing::warn!(
                        at_ms,
                        hosts,
                        "net probe: internet DOWN — no configured host resolves"
                    ),
                    NetTransition::CameUp { at_ms } => {
                        tracing::info!(at_ms, hosts, "net probe: internet recovered")
                    }
                }
            }
            sleep_unless_stopped(&self.state.stop, self.cfg.interval);
        }
    }
}

/// A spawned [`NetProbe`]'s thread: its state (readable via [`Self::handle`]) plus the join handle.
///
/// **Drop signals stop but does NOT join.** Joining could block for however long an in-flight DNS
/// resolve takes to give up, and a `Drop` that can park an unrelated shutdown path for tens of
/// seconds is a worse trade than letting one idle thread wind itself down. Call [`Self::join`]
/// explicitly for a deterministic teardown.
#[derive(Debug)]
pub struct NetProbeThread {
    state: Arc<NetProbeState>,
    join: Option<JoinHandle<()>>,
}

impl NetProbeThread {
    /// A cloneable reader of the running probe's state.
    #[must_use]
    pub fn handle(&self) -> NetProbeHandle {
        NetProbeHandle { state: Arc::clone(&self.state) }
    }

    /// Signal the thread to stop, without waiting.
    pub fn stop(&self) {
        self.state.stop.store(true, Ordering::Relaxed);
    }

    /// Signal the thread to stop and wait for it. Returns once it has exited — within ~100 ms plus
    /// any in-flight resolve.
    pub fn join(mut self) {
        self.stop();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for NetProbeThread {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe over scripted hosts with an explicit debounce threshold.
    fn probe(hosts: &[&str], failures_before_down: u32) -> NetProbe {
        NetProbe::new(NetProbeConfig {
            hosts: hosts.iter().map(|h| (*h).to_string()).collect(),
            interval: DEFAULT_PROBE_INTERVAL,
            failures_before_down,
        })
        .expect("non-empty host list")
    }

    // ---- initial state: optimistic, and honest about not having measured yet ---------------------

    /// Before any round, the flag reads up (no phantom outage for an early reader) but `has_probed`
    /// discloses that this is the initial value, not a measurement.
    #[test]
    fn starts_optimistically_up_and_unprobed() {
        let p = NetProbe::with_defaults();
        let h = p.handle();
        assert!(h.internet_up(), "an early reader must not see a phantom outage");
        assert!(!h.has_probed(), "nothing has been measured yet");
        assert_eq!(h.checks(), 0);
        assert_eq!(h.transitions(), 0);
        assert_eq!(h.last_check_ms(), 0);
        assert_eq!(h.downtime_ms(1_000), None, "up ⇒ no downtime");
    }

    /// Constructing a probe spawns nothing and touches no network (the OFF-by-default contract).
    #[test]
    fn construction_alone_probes_nothing() {
        let p = NetProbe::with_defaults();
        assert_eq!(p.handle().checks(), 0, "no round runs without an explicit call");
        assert_eq!(p.hosts().len(), DEFAULT_PROBE_HOSTS.len());
    }

    // ---- the debounce: slow to condemn ----------------------------------------------------------

    /// One failed round below the threshold must NOT flip the flag — a single DNS hiccup is not an
    /// outage.
    #[test]
    fn a_single_failure_does_not_flip_the_flag() {
        let p = probe(&["a", "b"], 2);
        assert_eq!(p.probe_once_with(|_| false, 1_000), None, "no flip on the first failure");
        let h = p.handle();
        assert!(h.internet_up(), "still up — the debounce has not been reached");
        assert!(h.has_probed(), "but a round DID complete");
        assert_eq!(h.checks(), 1);
        assert_eq!(h.transitions(), 0);
        assert_eq!(h.last_check_ms(), 1_000);
    }

    /// The Nth consecutive failure flips it down and stamps the transition timestamp.
    #[test]
    fn consecutive_failures_reaching_the_threshold_flip_it_down() {
        let p = probe(&["a", "b"], 2);
        assert_eq!(p.probe_once_with(|_| false, 1_000), None);
        assert_eq!(
            p.probe_once_with(|_| false, 2_000),
            Some(NetTransition::WentDown { at_ms: 2_000 }),
            "the second consecutive failure trips it"
        );
        let h = p.handle();
        assert!(!h.internet_up());
        assert_eq!(h.last_down_ms(), 2_000, "the transition timestamp is stamped");
        assert_eq!(h.transitions(), 1);
        assert_eq!(h.downtime_ms(6_000), Some(4_000), "down for 4s as of t=6000");
    }

    /// Staying down emits no further transitions — the edge fires once, not every round.
    #[test]
    fn staying_down_emits_no_further_transitions() {
        let p = probe(&["a"], 1);
        assert_eq!(
            p.probe_once_with(|_| false, 1_000),
            Some(NetTransition::WentDown { at_ms: 1_000 })
        );
        assert_eq!(p.probe_once_with(|_| false, 2_000), None, "already down");
        assert_eq!(p.probe_once_with(|_| false, 3_000), None, "still already down");
        let h = p.handle();
        assert_eq!(h.transitions(), 1, "exactly one flip");
        assert_eq!(h.last_down_ms(), 1_000, "the ORIGINAL down time, not the latest round");
        assert_eq!(h.checks(), 3, "but all three rounds counted");
    }

    /// An interrupted failure streak resets the debounce: fail, succeed, fail must NOT flip down
    /// with a threshold of 2 (the streak restarted).
    #[test]
    fn a_success_resets_the_failure_streak() {
        let p = probe(&["a"], 2);
        assert_eq!(p.probe_once_with(|_| false, 1_000), None);
        assert_eq!(p.probe_once_with(|_| true, 2_000), None, "up→up is not a transition");
        assert_eq!(p.probe_once_with(|_| false, 3_000), None, "streak restarted, so no flip yet");
        assert!(p.handle().internet_up());
        assert_eq!(
            p.probe_once_with(|_| false, 4_000),
            Some(NetTransition::WentDown { at_ms: 4_000 }),
            "now two in a row"
        );
    }

    /// A threshold of 0 is normalised to 1 rather than meaning "never flip down".
    #[test]
    fn a_zero_threshold_is_treated_as_one() {
        let p = probe(&["a"], 0);
        assert_eq!(
            p.probe_once_with(|_| false, 500),
            Some(NetTransition::WentDown { at_ms: 500 }),
            "a 0 threshold must not disable the flag"
        );
    }

    // ---- recovery: quick to forgive -------------------------------------------------------------

    /// One success recovers immediately — recovery is never debounced.
    #[test]
    fn one_success_recovers_immediately() {
        let p = probe(&["a"], 2);
        let _ = p.probe_once_with(|_| false, 1_000);
        let _ = p.probe_once_with(|_| false, 2_000);
        assert!(!p.handle().internet_up());
        assert_eq!(
            p.probe_once_with(|_| true, 3_000),
            Some(NetTransition::CameUp { at_ms: 3_000 }),
            "the very first success recovers"
        );
        let h = p.handle();
        assert!(h.internet_up());
        assert_eq!(h.last_up_ms(), 3_000);
        assert_eq!(h.last_down_ms(), 2_000, "the prior down stamp is retained");
        assert_eq!(h.transitions(), 2, "down + up");
        assert_eq!(h.downtime_ms(9_000), None, "recovered ⇒ no downtime");
    }

    /// A full down→up→down cycle counts three transitions and keeps both stamps current — the
    /// flapping signature a consumer needs to distinguish from one clean outage.
    #[test]
    fn a_flapping_link_is_visible_in_the_transition_count() {
        let p = probe(&["a"], 1);
        let _ = p.probe_once_with(|_| false, 1_000); // down
        let _ = p.probe_once_with(|_| true, 2_000); // up
        let _ = p.probe_once_with(|_| false, 3_000); // down again
        let h = p.handle();
        assert_eq!(h.transitions(), 3);
        assert_eq!(h.last_down_ms(), 3_000, "latest down");
        assert_eq!(h.last_up_ms(), 2_000, "latest up");
        assert!(!h.internet_up());
    }

    // ---- the host fold: ANY host resolving means up ---------------------------------------------

    /// One reachable host is enough — one operator's DNS outage must not read as our internet dying.
    #[test]
    fn any_single_resolving_host_keeps_it_up() {
        let p = probe(&["a", "b", "c"], 1);
        assert_eq!(p.probe_once_with(|h| h == "c", 1_000), None, "the last host alone suffices");
        assert!(p.handle().internet_up());
    }

    /// A down verdict requires EVERY host to fail to resolve.
    #[test]
    fn down_requires_every_host_to_fail() {
        let p = probe(&["a", "b", "c"], 1);
        let mut tried = Vec::new();
        let flip = p.probe_once_with(
            |h| {
                tried.push(h.to_string());
                false
            },
            1_000,
        );
        assert_eq!(flip, Some(NetTransition::WentDown { at_ms: 1_000 }));
        assert_eq!(tried, ["a", "b", "c"], "all hosts were tried before condemning");
    }

    /// The healthy path short-circuits on the first resolving host (one lookup, not N).
    #[test]
    fn the_healthy_path_short_circuits_on_the_first_host() {
        let p = probe(&["a", "b", "c"], 1);
        let mut tried = 0usize;
        let _ = p.probe_once_with(
            |_| {
                tried += 1;
                true
            },
            1_000,
        );
        assert_eq!(tried, 1, "any() stops at the first success");
    }

    // ---- construction invariants ----------------------------------------------------------------

    /// An empty host list is rejected: it could only ever fold to "unreachable".
    #[test]
    fn new_rejects_an_empty_host_list() {
        let cfg = NetProbeConfig { hosts: vec![], ..NetProbeConfig::default() };
        assert!(NetProbe::new(cfg).is_none());
    }

    /// The defaults use MORE THAN ONE independently-operated host (no single point of failure).
    #[test]
    fn defaults_have_no_single_point_of_failure() {
        assert!(DEFAULT_PROBE_HOSTS.len() > 1);
        assert_eq!(NetProbe::with_defaults().hosts().len(), DEFAULT_PROBE_HOSTS.len());
        // Names, not literal IPs — resolving them is what exercises the local resolver.
        for h in DEFAULT_PROBE_HOSTS {
            assert!(h.parse::<std::net::IpAddr>().is_err(), "{h} must be a NAME, not a literal IP");
        }
    }

    /// Handles share one state: a flip observed through the probe is visible to every reader.
    #[test]
    fn handles_share_the_same_state() {
        let p = probe(&["a"], 1);
        let (h1, h2) = (p.handle(), p.handle().clone());
        let _ = p.probe_once_with(|_| false, 7_000);
        assert!(!h1.internet_up());
        assert!(!h2.internet_up(), "a cloned handle sees the same state");
        assert_eq!(h2.last_down_ms(), 7_000);
    }

    // ---- the REAL resolver path, offline-deterministic -------------------------------------------

    /// `dns_resolves` is wired to real name resolution. Asserted with a LITERAL IP, which
    /// `to_socket_addrs` parses without any DNS traffic — so this is deterministic on any box,
    /// online or not. (The NXDOMAIN-negative is deliberately NOT asserted against the real resolver:
    /// resolvers that hijack NXDOMAIN would answer even for a reserved `.invalid` name, making it
    /// environment-dependent. The failure fold is fully covered by the injected-resolver tests.)
    #[test]
    fn dns_resolves_is_true_for_a_literal_address() {
        assert!(dns_resolves("127.0.0.1"), "a literal IP resolves without DNS");
    }

    /// The wall clock is the only real clock and returns a sane epoch-ms value.
    #[test]
    fn wall_clock_ms_is_a_plausible_epoch_ms() {
        // 2020-01-01 in epoch-ms; any real clock is well past it.
        assert!(wall_clock_ms() > 1_577_836_800_000);
    }

    // ---- the spawned thread: opt-in lifecycle, no real network needed ----------------------------

    /// `spawn` starts the monitor and `join` tears it down deterministically. The probe resolves the
    /// literal-IP host, so the round needs no network. A short interval keeps the test quick.
    #[test]
    fn spawn_runs_rounds_and_join_stops_it() {
        let p = NetProbe::new(NetProbeConfig {
            hosts: vec!["127.0.0.1".to_string()],
            interval: Duration::from_millis(50),
            failures_before_down: DEFAULT_FAILURES_BEFORE_DOWN,
        })
        .expect("one host");
        let thread = p.spawn();
        let h = thread.handle();
        // Wait (bounded) for the first round rather than sleeping a fixed span.
        let start = std::time::Instant::now();
        while !h.has_probed() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(h.has_probed(), "the spawned thread ran at least one round");
        assert!(h.internet_up(), "the literal-IP host resolves, so it stays up");
        thread.join();
    }

    /// Dropping the handle signals stop without joining (never blocks an unrelated teardown).
    #[test]
    fn dropping_the_thread_handle_signals_stop() {
        let p = NetProbe::new(NetProbeConfig {
            hosts: vec!["127.0.0.1".to_string()],
            interval: Duration::from_millis(50),
            failures_before_down: DEFAULT_FAILURES_BEFORE_DOWN,
        })
        .expect("one host");
        let state = Arc::clone(&p.state);
        drop(p.spawn());
        assert!(state.stop.load(Ordering::Relaxed), "Drop set the stop flag");
    }
}
