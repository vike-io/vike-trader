//! The injected observation seam: the probe trait, its closure-backed impl, the gap enums.

use std::fmt;
use std::path::Path;

use vike_bridge_core::net_probe::wall_clock_ms;

use super::config::NO_PROBE;
#[cfg(doc)]
use super::report::{CheckStatus, PreflightReport};

/// Why a clock leg produced no number (module doc ②-④, one variant each): different facts,
/// never one row. Declared variants take `&'static str`, so a runtime failure cannot fake a
/// declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerTimeGap {
    /// ② The venue PUBLISHES a clock and this attempt got none. Carries the venue's error text,
    /// never a URL or a credential. Also an UNWIRED leg's answer ([`NO_PROBE`]).
    Unreachable(String),
    /// ③ DECLARED, nothing at stake: no clock leg, with the row's own reason
    /// (`crate::server_time`'s `clock_decl`); a drifted clock could not cost this venue an order.
    NotChecked(&'static str),
    /// ④ DECLARED, ORDERS at stake: no clock leg, but the venue's auth binds the clock into the
    /// order path — an unmeasured HAZARD, rendered [`CheckStatus::Warn`], never NOT-APPLICABLE
    /// (polymarket printed "nothing to check here" over the roster's one order-affecting gap).
    UnmeasuredRisk {
        /// Why there is no leg (the row's own sentence).
        reason: &'static str,
        /// What a drifted clock costs AT THIS VENUE, in that venue's own terms.
        at_stake: &'static str,
    },
}

/// Why a credential probe failed — the twin of [`ServerTimeGap`] that makes
/// [`PreflightReport::venue_disposition`] enforceable: only "refused us" may FAIL; "never heard
/// back" never does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialGap {
    /// The venue ANSWERED inside the bound and refused (or a definite transport error): a
    /// [`CheckStatus::Fail`] demoting to paper. Venue error text, never a key/secret/passphrase.
    /// Also an UNWIRED leg's answer ([`NO_PROBE`]): our wiring defect, and strict is safe.
    Rejected(String),
    /// No answer inside `crate::startup::CREDENTIAL_PROBE_TIMEOUT`, or the leg's budget ran out
    /// first. NOT evidence about the credentials: a [`CheckStatus::Warn`] that NEVER demotes.
    Unanswered {
        /// How long the mount thread actually waited, in ms (the row states its own bound).
        waited_ms: u64,
        /// What the wait was on, in operator language. Never contains a secret.
        detail: String,
    },
}

impl From<String> for CredentialGap {
    /// A bare error string is the CONFIRMED half; only the bounded runner in [`crate::startup`]
    /// (which owns the clock the wait is measured on) constructs [`CredentialGap::Unanswered`].
    fn from(e: String) -> Self {
        CredentialGap::Rejected(e)
    }
}

/// The injected observation seam: this module never touches the outside world, the caller supplies
/// every method. Object-safe on purpose (`&dyn`), so test doubles and real wiring interchange.
pub trait PreflightProbes {
    /// Local wall clock in epoch ms. Injected so skew is deterministic under test.
    fn local_now_ms(&self) -> i64;

    /// The venue's server time in epoch ms; real wiring is `crate::server_time`'s
    /// `venue_server_time_ms` (the roster-gated declaration table). `Err` is a [`ServerTimeGap`]
    /// because ② (a WARN), ③ (NOT-APPLICABLE) and ④ (a WARN) are different facts.
    ///
    /// **Units:** ABSOLUTE epoch ms, whereas the wrapped helpers
    /// (`BinanceSpotRest::server_time_offset` / `AsterSpotRest::server_time_offset`) return the
    /// OFFSET `server - local_now_ms` that `Signer::set_offset_ms` wants. Convert:
    /// `let t = local_now_ms(); Ok(t + rest.server_time_offset(t)?)` (the offset cancels the
    /// reading it was measured against).
    fn venue_server_time_ms(&self, venue: &str) -> Result<i64, ServerTimeGap>;

    /// A CHEAP authenticated read; real wiring reuses the venue's `ReconClient` balance fetch.
    /// `Ok(())` = the credentials signed and were accepted. `Err` is a [`CredentialGap`]: a REFUSAL
    /// demotes, a silence must not. No variant's text may contain a secret.
    fn venue_authed_read(&self, venue: &str) -> Result<(), CredentialGap>;

    /// Free bytes on the filesystem holding `dir`. `Err(reason)` = "could not query": a WARN, never
    /// a FAIL (a preflight must not ground the app on its own inability to measure).
    fn free_space_bytes(&self, dir: &Path) -> Result<u64, String>;
}

/// Local-clock leg. These aliases keep `clippy::type_complexity` quiet without an `allow`.
type ClockFn = Box<dyn Fn() -> i64 + Send + Sync>;
/// Per-venue leg: the venue slug in, a value or an error out.
type VenueFn<T, E = String> = Box<dyn Fn(&str) -> Result<T, E> + Send + Sync>;
/// Filesystem leg: a directory in, its free bytes out.
type PathFn<T> = Box<dyn Fn(&Path) -> Result<T, String> + Send + Sync>;

/// Closure-backed [`PreflightProbes`]: [`FnProbes::new`] plus `with_*`; an unset leg returns
/// [`NO_PROBE`], the clock defaults to [`vike_bridge_core::net_probe::wall_clock_ms`]. Legs are
/// `Send + Sync` (the preflight may run on a spawned thread).
pub struct FnProbes {
    now_ms: ClockFn,
    server_time_ms: VenueFn<i64, ServerTimeGap>,
    authed_read: VenueFn<(), CredentialGap>,
    free_space: PathFn<u64>,
}

impl Default for FnProbes {
    fn default() -> Self {
        FnProbes {
            now_ms: Box::new(wall_clock_ms),
            // An unwired probe is ②, never ③: it says nothing about the venue.
            server_time_ms: Box::new(|_: &str| {
                Err(ServerTimeGap::Unreachable(NO_PROBE.to_string()))
            }),
            // CONFIRMED deliberately: a missing check must not read as "did not hear back".
            authed_read: Box::new(|_: &str| Err(CredentialGap::Rejected(NO_PROBE.to_string()))),
            free_space: Box::new(|_: &Path| Err(NO_PROBE.to_string())),
        }
    }
}

impl fmt::Debug for FnProbes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FnProbes(<injected closures>)")
    }
}

impl FnProbes {
    /// Every leg unwired (each returns [`NO_PROBE`]) except the real wall clock.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the local clock — tests inject a fixed epoch-ms.
    #[must_use]
    pub fn with_now_ms(mut self, probe: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.now_ms = Box::new(probe);
        self
    }

    /// Wire the venue server-time read.
    #[must_use]
    pub fn with_venue_server_time_ms(
        mut self,
        probe: impl Fn(&str) -> Result<i64, ServerTimeGap> + Send + Sync + 'static,
    ) -> Self {
        self.server_time_ms = Box::new(probe);
        self
    }

    /// Wire the cheap authenticated read. A `String` error is CONFIRMED ([`CredentialGap::from`]);
    /// only [`crate::startup`]'s bounded runner can tell a refusal from a silence.
    #[must_use]
    pub fn with_venue_authed_read<E: Into<CredentialGap>>(
        mut self,
        probe: impl Fn(&str) -> Result<(), E> + Send + Sync + 'static,
    ) -> Self {
        self.authed_read = Box::new(move |venue: &str| probe(venue).map_err(Into::into));
        self
    }

    /// Wire the free-space query.
    #[must_use]
    pub fn with_free_space_bytes(
        mut self,
        probe: impl Fn(&Path) -> Result<u64, String> + Send + Sync + 'static,
    ) -> Self {
        self.free_space = Box::new(probe);
        self
    }
}

impl PreflightProbes for FnProbes {
    fn local_now_ms(&self) -> i64 {
        (self.now_ms)()
    }

    fn venue_server_time_ms(&self, venue: &str) -> Result<i64, ServerTimeGap> {
        (self.server_time_ms)(venue)
    }

    fn venue_authed_read(&self, venue: &str) -> Result<(), CredentialGap> {
        (self.authed_read)(venue)
    }

    fn free_space_bytes(&self, dir: &Path) -> Result<u64, String> {
        (self.free_space)(dir)
    }
}
