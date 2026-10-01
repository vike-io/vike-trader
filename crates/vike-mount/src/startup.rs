//! The **mount-site wiring** of [`crate::preflight`] — the one place the pure go/no-go gate is
//! handed REAL probes. `vike_mount::build_node` calls [`run_startup_preflight`] once, at the top of
//! the node assembly, before any venue is mounted.
//!
//! `preflight` itself performs no I/O: every observation arrives through its four-method
//! [`PreflightProbes`](crate::preflight::PreflightProbes) seam, and its unwired legs return
//! [`NO_PROBE`](crate::preflight::NO_PROBE). This module supplies the legs that CAN be supplied
//! today, and is explicit about the one that cannot:
//!
//! - **network — ARMED.** A real [`NetProbe`] is spawned (its FIRST round runs immediately), waited
//!   on for at most [`DEFAULT_NET_PROBE_WAIT`], read through its handle, and then dropped — which
//!   signals stop without joining, exactly as [`vike_bridge_core::NetProbeThread`]'s `Drop`
//!   documents. Nothing is left running past the preflight. The probe is spawned ONLY when at
//!   least one venue would mount live UNDER THE CEILING ([`crate::would_mount_live_under_policy`]
//!   over the canonical `vike_model::VENUES` roster — pure, no network): a pure-paper mount — by
//!   absent credentials or by `policy.toml` — places no order and opens
//!   no venue socket, so it stays entirely offline and the network leg honestly WARNs "no NetProbe
//!   wired" instead of quietly resolving two names for nothing.
//! - **clock skew — ARMED from the roster declaration table** ([`crate::server_time`]). Every
//!   venue that WOULD MOUNT LIVE UNDER THE CEILING gets a clock row: a MEASUREMENT where an
//!   endpoint is wired (read
//!   against the same demo/mainnet host that venue's own arm binds — the same class of blocking
//!   startup pre-fetch the live arms' `fetch_*_properties` calls already are), an explicit
//!   "publishes one, did not answer" WARN where the read fails, and a DECLARED row carrying its
//!   reason where the venue has no clock leg — NOT-APPLICABLE where a drift costs that venue
//!   nothing, a WARN where its auth signs the clock into the order path (④; polymarket is the one
//!   such row). None of the last three is ever a FAIL, so a venue can never be degraded by OUR
//!   inability to measure it — and a MEASURED skew can only FAIL a venue that actually rejects
//!   orders over drift (`crate::server_time::clock_policy_of`).
//! - **credentials — ARMED, through each venue's own `ReconClient`.** [`authed_read_probes`] builds
//!   one PROBE per venue whose credentials resolve, each performing that venue's own cheap
//!   authenticated read (`fetch_balance`), and the leg calls it. ⚠ INVARIANT: the configured
//!   [`PreflightConfig::credential_venues`] list is DERIVED from that probe map, never written by
//!   hand, because an unwired credential leg FAILs by design ("we could not prove these keys work"
//!   must not mount a venue live) — listing a venue we cannot authed-read would manufacture a FAIL.
//!
//!   Two SHAPES of probe, and the difference is load-bearing rather than cosmetic:
//!
//!   * **Eagerly-built** (binance/bybit/okx through their bridges' `VenueMount::credential_probe`
//!     (`RecordsIdentity`), alpaca via its own `crates/bridges/alpaca/src/recon_client.rs`'s
//!     `recon_client`): construction is PURE — a signer plus a transport, no network — so the
//!     client is built up front and the probe closure just calls `fetch_balance`.
//!   * **Lazily-built** (ctrader, through `crates/bridges/ctrader/src/mount.rs`'s
//!     `CtraderVenueMount`): `crates/bridges/ctrader/src/recon_client.rs`'s `recon_client`
//!     CONNECTS during construction (one multiplexed protobuf socket, authenticated at handshake —
//!     there is no cheaper authed read at this venue). Building it eagerly would be a silent hole
//!     in exactly the case the leg exists for: a refused grant returns `None`, the venue would then
//!     be absent from the probe map, and absent from the map means NO ROW AT ALL — an expired token
//!     would produce silence rather than the FAIL it is. So ctrader's probe defers construction
//!     into the closure, where a refused handshake becomes the leg's error instead of vanishing.
//!
//!   A CONTRACT row offers its probe through `VenueMount::credential_probe`, in one of the same two
//!   shapes: `CredentialProbe::RecordsIdentity` carries an eagerly-built client, and
//!   `CredentialProbe::ReadOnly` a closure that may connect inside itself. That call runs on this
//!   thread, outside the bound its probe later runs under, which is why the trait's doc states the
//!   same rule for every bridge.
//!
//!   ⚠ **Every probe's error NAMES THE HOST** it could not reach or that refused it. The rehearsal
//!   this leg comes from spent its whole diagnosis budget establishing, by hand and after the fact,
//!   that every Alpaca host was TCP-unreachable from that box while the mount had already announced
//!   `exec="LIVE"` — a fact the daemon could have printed at startup.
//!
//!   ⚠ **That leg now also RECORDS which account each key is**, right after its balance read, and
//!   the ordering is load-bearing: on binance spot the account id rides `/api/v3/account`, the same
//!   body the balance read pulls, so recording after it is free there. It never changes the probe's
//!   verdict — a key that answered its balance is a working key, and a bookkeeping field that did
//!   not come back must not demote a venue to paper.
//!   `crate::book_identity::record_authenticated_account` is the whole of it, and it is the site
//!   that covers a venue this box ARMS but never MOUNTS — measured on the CI box as 16 `account` rows
//!   against a profile that mounts one.
//!
//! - **disk headroom — ARMED on unix, DECLARED on Windows.** Free space is not reachable from
//!   `std`: it needs a platform syscall, so it costs either a dependency or an `unsafe` block, and
//!   the workspace lint policy forbids `unsafe_code`. That was the stated reason this leg stayed
//!   dark, and it was the wrong trade — a full disk kills the live `RecorderSink` writer and the
//!   journal WAL silently, and the CI box was measured at ~70% of one filesystem consumed with the gate
//!   still unwired. [`free_space_bytes`] arms it through `rustix::fs::statvfs`, a SAFE call in a
//!   crate `Cargo.lock` had already resolved, so the leg costs no `unsafe`, no lint exemption and
//!   no new package. ⚠ rustix's `statvfs` is POSIX-only, so on Windows the probe returns a DECLARED
//!   reason rather than a number, and [`disk_dirs`] renders that as one honest row instead of the
//!   silent absence this leg used to be. That asymmetry is the right way round: both shipped
//!   daemons run on the CI box, which is Linux.
//!
//! # ⚠ The ARMING CEILING gates every leg, and what the ceiling is doing in a CLOCK leg
//!
//! Every venue set below is derived through [`crate::would_mount_live_under_policy`] — the SAME
//! predicate `make_engine` arms on, reached through the same [`crate::venue_ceiling`], so the
//! preflight and the mount cannot disagree about what is armed. `policy: None` reads all-`paper`
//! and contacts nothing.
//!
//! Until that was threaded, this module derived its work from CREDENTIAL PRESENCE alone
//! (`crate::would_mount_live`, uncapped) and knocked on venues the operator had turned off. A
//! deployment whose `policy.toml` said `ig = "paper"` still had IG contacted at every start — and
//! IG's clock row is the one CREDENTIALED clock read on the roster
//! (`crates/bridges/ig/src/mount.rs`'s `IgVenueMount::server_time_ms` sends `X-IG-API-KEY`), while
//! the credential leg beside it issues a SIGNED `fetch_balance`. An operator who sets a venue to
//! `paper` has said *do not touch this account*, and the preflight was authenticating against it
//! anyway.
//!
//! ## Account-scoped versus box-scoped, because the legs genuinely differ
//!
//! - **credentials — ACCOUNT-SCOPED end to end.** A signed balance read (and, at ctrader, an
//!   authenticated socket opened at the venue). Gating it is the whole point.
//! - **disk — BOX-SCOPED end to end.** `statvfs` on a local directory; no venue, no credential, no
//!   ceiling. Ungated, and it still runs on an all-paper box: a full disk kills the journal WAL
//!   whether or not anything is armed.
//! - **network — BOX-SCOPED in what it measures**, and it touches no venue at all (it resolves
//!   `one.one.one.one` and `dns.google`). Gated anyway — see
//!   [`any_venue_would_mount_live`] for the two reasons, the load-bearing one being that this leg's
//!   own verdict line ASSERTS there is no live order path.
//! - **clock skew — the interesting one, and the reason this section exists.** The QUANTITY is
//!   box-scoped (this host's offset from a reference), which is the argument for reading it
//!   everywhere. But the CONTACT is with the venue, the VERDICT is per-venue (thresholds come from
//!   that venue's own risk through `crate::server_time::clock_policy_of`), and the CONSEQUENCE is
//!   per-venue (a FAIL demotes that venue to paper — a no-op for a venue that is already paper). So a
//!   paper-capped venue in this list buys a reading nothing can act on and pays for it with a
//!   network contact the operator forbade. It is gated.
//!
//!   ⚠ **This does NOT leave a box with no clock check that used to have one.** `clock_venues` has
//!   never been a "pick any reachable venue as a time source" list — it has always been derived
//!   from live intent, so a box with no credentials already had an EMPTY clock leg
//!   (`no_credentials_means_no_clock_venues`, and the whole of
//!   `an_empty_credentials_map_preflights_offline`). The ceiling widens that accepted default onto
//!   the set where the check's verdict is inert; it introduces no new hole. A genuine BOX clock
//!   check — one that should run on an all-paper box — would have to be a venue-independent leg
//!   (NTP, or a neutral host), not a disarmed venue pressed into service as a clock.
//!
//! ## The one residual, CLOSED by decision 0095
//!
//! The ceiling decides WHICH venues are read and, in the credential leg, WHICH TIER is signed
//! (the `live_permitted` each contract row's `credential_probe` is asked with — the CEX trio's
//! probes were built by `authed_read_clients` until docs/decisions/0096 moved each into its
//! bridge). It used NOT to reach the clock FETCHERS' own tier choice:
//! `crate::server_time::ClockFetch` was a `fn(&HashMap<String, String>)` in a `const` table, so
//! binance/bybit/okx/aster/hyperliquid resolved their host from `{VENUE}_MAINNET` alone — under a
//! `demo` ceiling with that flag armed, the clock read went to the MAINNET host while the mount
//! bound the demo one, measuring a clock that would not judge our orders (the two tiers genuinely
//! differ; `crate::server_time`'s "tier discipline" section has the paired measurement). Decision
//! 0095 widened `ClockFetch` to `fn(&HashMap<String, String>, bool)` and threaded the SAME
//! `ceiling_permits_live` fold [`run_startup_preflight`]'s probe closure computes per venue. That
//! table is gone since the venue mount contract (docs/decisions/0096): each fetcher is its
//! bridge's `VenueMount::server_time_ms`, handed that fold as `MountInputs::live_permitted` on the
//! inputs its mount is handed, so every fetcher reads the tier the mount actually binds.
//!
//! ⚠ **The two venue lists are SEPARATE, and merging them was the second half of the defect.**
//! `PreflightConfig::venues` used to feed both legs and was derived from `AUTHED_READ_MARKETS` (the
//! credential leg's CEX table, deleted with docs/decisions/0096's okx port), so the clock leg ran
//! for the crypto-CEX trio and NOTHING else: wiring an endpoint for deribit or
//! hyperliquid would have changed nothing at all, because those venues were never in the checked
//! set. They cannot simply be unified in the other direction either — a credential-leg entry FAILs
//! (degrading that venue), while the clock leg only ever WARNs — so the clock list is derived from
//! LIVE INTENT ([`crate::would_mount_live_under_policy`], pure and network-free) and the credential
//! list stays derived from client-buildability — with the SAME ceiling applied to it as a second
//! conjunct, because that list is the account-scoped one.
//!
//! # Cost, and what runs by default
//!
//! With NO credentials — CI, and every paper mount — [`authed_read_probes`] returns empty AND no
//! venue would mount live, so BOTH venue lists are empty, no `NetProbe` is spawned and **not one
//! network call is made**: the whole preflight is a handful of `HashMap` lookups producing a
//! one-row report. **An all-paper `policy.toml` — or no policy at all — reaches that same state
//! with a FULL credential store**, which is the ceiling doing its job and is the fresh-box default.
//! Only a mount that resolves live credentials AND is armed for them pays anything, and it pays
//! per VENUE IT IS ABOUT TO MOUNT LIVE: one clock read per wired venue (keyless for all but ig),
//! ZERO for a declared venue (③ needs no network at all — it is a table lookup), plus ONE signed
//! balance read per credentialed CEX venue, sequential, plus the bounded
//! [`DEFAULT_NET_PROBE_WAIT`] for the first DNS round. Warm, that is tens of milliseconds.
//!
//! A clock read may be repeated — at most [`crate::preflight::DEFAULT_CLOCK_SAMPLES`] times, and
//! only while the reading is inconclusive (see the preflight's sampling section). A healthy venue
//! answers in one.
//!
//! ⚠ **The clock leg is a BLOCKING startup stall, and the arithmetic is what bounds it.** This lane
//! took the wired set from ONE venue to seven, each readable up to
//! [`crate::preflight::DEFAULT_CLOCK_SAMPLES`] times; on the shared 30 s agent
//! (`vike_bridge_core::http`'s `blocking_agent`) that is **7 × 3 × 30 s = 630 s** — a mount parked
//! for ten and a half minutes by a check whose worst finding is a WARN, where the one-venue version
//! it replaced was bounded by 30 s. Three named bounds replace that: every fetcher runs on an agent
//! bounded by `crate::server_time::CLOCK_READ_TIMEOUT`; the leg as a whole is capped by
//! [`crate::preflight::clock_budget_for`] — DERIVED from how many venues are actually read, one
//! shared pool across every venue and every resample, and deliberately not a number restated here
//! (this paragraph used to cite the FLOOR, `DEFAULT_CLOCK_BUDGET_MS`, as if it were the budget, and
//! was a floor's worth wrong from the day `docs/decisions/0027-clock-budget-derived-from-the-roster.md`
//! derived it); and, since a transport timeout turned out not to bound the thing that actually parks
//! a mount, the abandon seam below. Worst case is **budget + one in-flight read**, and the
//! ARITHMETIC — which bound the in-flight read actually is, and what the sum comes to today and at
//! most — lives ONCE, in [`crate::preflight`]'s "the clock leg is BOUNDED" section, against a
//! healthy cost the live smoke MEASURED at 1781 ms from the CI box.
//!
//! ⚠ **NEITHER of the first two bounds can preempt a wedged NAME RESOLUTION, and that was the
//! hole.** A ureq timeout is a timer over an in-flight request; `std` offers no timeout on name
//! resolution at all (`vike_bridge_core::net_probe`'s own caveat says so), so on a box whose
//! resolver is wedged the FIRST clock read parks the mount for however long the OS resolver takes
//! to give up — at the top of `vike_mount::build_node`, before one venue is up, over a check whose
//! worst finding is a WARN. The leg-wide budget is no help either: it is checked BETWEEN reads, so
//! it bounds how MANY reads happen and never how long one of them takes.
//!
//! So a WIRED clock read is issued through the SAME [`bounded_probe`] the credential leg already
//! uses — [`CLOCK_PROBE_TIMEOUT`] per read, on a thread the mount can walk away from — and an
//! abandoned read becomes outcome ② (a WARN that degrades nothing) instead of an unbounded stall.
//! ⚠ It is NOT a fan-out and NOT a per-venue slice: the leg still reads one venue at a time, in
//! roster order, out of one shared budget, exactly as
//! `docs/decisions/0027-clock-budget-derived-from-the-roster.md` decided. A DECLARED venue is not
//! routed through the probe at all (its answer is a table lookup — see [`bounded_server_time_ms`]),
//! so nothing about the offline path changes: a mount with no armed venue still spawns no thread
//! and makes no call. The residual — an abandoned probe leaves a thread running — is declared on
//! [`bounded_probe`], for both legs.
//!
//! # The credential leg is BOUNDED — without any `ReconClient` gaining a knob
//!
//! ⚠ It used to be bounded by nothing at all: each `fetch_balance` rode its own `ReconClient`'s
//! 30 s agent, so the leg was worth up to `30 s × credentialed venues` — a ceiling that GREW with
//! the roster (alpaca and ctrader each raised it, and ctrader additionally pays a TCP+TLS handshake
//! inside its own closure). On the Windows dev box 2026-08-22 a geo-blocked alpaca probe sat ~20 s
//! on a single TCP connect at the top of a mount.
//!
//! ⚠ **The obvious cure was the wrong one.** `ReconClient` is a SHARED HOME — every venue's
//! reconcile path implements it, and this workspace extends such a home by one coordinated change
//! informed by every consumer, never by a per-venue patch. Giving it a timeout parameter would have
//! been exactly that patch, spread over eleven venues, to serve one caller. So the bound is applied
//! where the blocking call is ISSUED instead, and no venue client changes at all:
//!
//! - [`CREDENTIAL_PROBE_TIMEOUT`] — the per-attempt ceiling. [`bounded_probe`] runs the probe on a
//!   spawned thread and `recv_timeout`s it, so an unresponsive venue is ABANDONED rather than
//!   waited out. The abandoned thread is harmless: it finishes on its own agent's timeout — or,
//!   when what wedged was NAME RESOLUTION (the case no request timer sees; the clock section
//!   above), whenever the OS resolver gives up — finds its result channel dropped, and exits. This
//!   is the same "signal stop without joining" shape [`NetProbeThread`]'s `Drop` already uses, and
//!   for the same reason — a wedged blocking call must never be something a mount waits on.
//!   [`bounded_probe`] declares that residual once, for both legs.
//! - [`CREDENTIAL_PROBE_ATTEMPTS`] — ONE retry, and only for a probe that ANSWERED with an error.
//!   A probe that timed out is never retried: its bound was already spent proving nothing.
//! - [`crate::preflight::credential_budget_for`] — the leg-wide total, derived per credentialed
//!   venue exactly as the clock budget is.
//!
//! Worst case is `budget + one venue's attempts` =
//! `MAX_CREDENTIAL_BUDGET_MS + CREDENTIAL_PROBE_ATTEMPTS × CREDENTIAL_PROBE_TIMEOUT`, a fixed
//! ceiling that does not move when a venue joins the roster. `VIKE_PREFLIGHT_SKIP=1` removes every
//! leg entirely.
//!
//! # The report is ENFORCED — and the bound above is what earns that
//!
//! [`crate::preflight::PreflightReport::venue_disposition`] computes a degrade-to-paper decision,
//! and `vike_mount::build_node_with_preflight` now ACTS on it: a venue with a hard per-venue FAIL is
//! mounted PAPER for that session.
//!
//! ⚠ For its whole life this report was advisory, and the stated reason was sound: "a per-venue
//! FAIL is raised by ANY authed-read error, including a transient timeout", so acting on it would
//! let one flaky startup second silently turn a live venue into a paper one. What was missing was a
//! confirmed-vs-transient distinction. [`CREDENTIAL_PROBE_TIMEOUT`] IS that distinction —
//! [`bounded_probe`] returns [`CredentialGap::Unanswered`] for a silence and
//! [`CredentialGap::Rejected`] only for an answer — so a timeout now WARNs and never demotes, while
//! a venue that answered and refused our signed read demotes. See
//! [`crate::preflight`]'s policy section for the full argument and the one accepted residual.
//!
//! # The skip
//!
//! `VIKE_PREFLIGHT_SKIP=1` (the EXACT string, read from the REAL process env — see
//! [`crate::preflight::preflight_skipped`]) returns the empty skipped report WITHOUT building a
//! probe, spawning a thread or resolving a credential, so the off path costs nothing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::{NetProbe, NetProbeThread};
use vike_exec::recon::ReconClient;

use crate::preflight::{
    ClockPolicy, CredentialGap, FnProbes, PreflightConfig, PreflightReport, ServerTimeGap,
    credential_budget_for, preflight_skipped, run_preflight_gated,
};

/// How long [`run_startup_preflight`] waits for the spawned [`NetProbe`]'s FIRST round before
/// reading its handle. A warm resolver answers in single-digit milliseconds; a wedged one cannot
/// be interrupted (`std` name resolution has no timeout — see [`vike_bridge_core::net_probe`]'s
/// caveat), which is exactly why the wait is bounded HERE and the probe runs on its own thread:
/// past this deadline the network leg simply WARNs "has not completed a round yet" and the mount
/// carries on.
pub const DEFAULT_NET_PROBE_WAIT: Duration = Duration::from_millis(500);

/// Poll granularity while waiting out [`DEFAULT_NET_PROBE_WAIT`].
const NET_PROBE_POLL: Duration = Duration::from_millis(10);

/// The PER-ATTEMPT ceiling on one venue's authed read — the bound that did not exist, and the seam
/// that separates "this venue refused us" from "we never heard back".
///
/// **5 s**, sized from what the probe actually is: one signed REST round trip (a `fetch_balance`),
/// which a healthy venue answers in tens to low hundreds of ms — the module doc's own cost note
/// says "warm, that is tens of milliseconds". Five seconds is therefore ~20x a healthy answer, so a
/// merely slow link is never mistaken for a dead one, while a genuinely unreachable host costs 5 s
/// instead of the 30 s its own agent would have spent (measured: ~20 s on one geo-blocked alpaca
/// TCP connect from the Windows dev box, 2026-08-22).
///
/// ⚠ It is not the `ReconClient`'s timeout and must not be confused for one — the client keeps its
/// own 30 s agent untouched. This is how long the MOUNT waits before abandoning the attempt.
pub const CREDENTIAL_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The PER-ATTEMPT ceiling on one venue's CLOCK read — the clock leg's twin of
/// [`CREDENTIAL_PROBE_TIMEOUT`], and the bound that leg did not have either.
///
/// **One second past `crate::server_time::CLOCK_READ_TIMEOUT`, and DERIVED from it** rather than
/// written down, because the two are not independent numbers: the transport's own global timeout is
/// what bounds every clock read that ureq CAN bound, and this is only the ceiling on the case it
/// CANNOT — a wedged name resolution, which no request timer preempts (the module doc's abandon
/// section, and `vike_bridge_core::net_probe`'s own caveat).
///
/// ⚠ The one-second gap is the whole reason this is not simply `CLOCK_READ_TIMEOUT`. Both deadlines
/// start at the same instant and this one starts marginally EARLIER (the worker is spawned before
/// the mount begins waiting), so equal values would abandon essentially every ordinary timeout a
/// hair before the transport reported it — trading the venue's own error text ("connection timed
/// out to <host>", the fact an operator needs) for our generic "did not answer" on every timed-out
/// read. A second is long enough that a read ureq can bound reports itself first, and short enough
/// that a read ureq cannot bound is abandoned a second later instead of never.
///
/// ⚠ **"Reports itself first" holds for six of the seven wired clock reads, and the seventh is a
/// declared residual.** Each makes exactly ONE request on its bounded agent — except
/// hyperliquid's, `crates/bridges/hyperliquid/src/mount.rs`'s `server_time_ms` (it was
/// `crate::server_time`'s `hyperliquid_time` until docs/decisions/0096 moved the read into the
/// bridge), which goes through `crates/bridges/hyperliquid/src/transport.rs`'s `info`: a RETRY LOOP
/// above ureq (`INFO_MAX_ATTEMPTS` attempts, each on the 3 s agent, with a 200 ms then 400 ms
/// backoff), so a venue answering slow `429`s/`5xx`s can legitimately occupy that read for longer
/// than this ceiling before its third attempt succeeds. Such a read is abandoned here with our
/// generic "did not answer" where the venue's own error text — or its eventual answer — would have
/// been truer. Accepted rather than sized around: the retry fires only on a fast-answering
/// throttle or server error (an ambiguous timeout returns on the first attempt, per that method's
/// own doc), the worst outcome either way is outcome ② (a WARN that degrades nothing), and
/// widening this ceiling to cover the loop would cost every OTHER venue's wedged-resolver case the
/// same seconds on every mount.
pub const CLOCK_PROBE_TIMEOUT: Duration =
    crate::server_time::CLOCK_READ_TIMEOUT.saturating_add(Duration::from_secs(1));

/// How many attempts one venue's credential probe gets. TWO — i.e. exactly one retry, and only for
/// an attempt that ANSWERED with an error.
///
/// The retry is what makes a FAIL mean "confirmed" rather than "failed once". A demotion to paper
/// is now a real consequence (see the module doc's enforcement section), so a single blip — a
/// momentary `429`, a connection reset — must not be able to cause one on its own. A probe that
/// TIMED OUT is never retried: it already spent its bound and learned nothing, and paying the bound
/// twice to learn nothing twice would just double the leg's worst case.
pub const CREDENTIAL_PROBE_ATTEMPTS: usize = 2;

/// The pause between a failed attempt and its retry. Short enough to stay inside the leg's budget,
/// long enough that an instantaneous transport blip is not simply re-hit.
const CREDENTIAL_RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// The longest thread name Linux will keep. The kernel's `TASK_COMM_LEN` is 16 bytes INCLUDING the
/// NUL, and `std`'s `pthread_setname_np` path TRUNCATES a longer name to fit rather than failing —
/// silently, on the one platform both shipped daemons run on and the only one anyone attaches a
/// debugger to. ⚠ The credential name SHIPPED over this limit (`vike-preflight-cred`, 19 bytes),
/// and the first draft of the clock name copied it (`vike-preflight-clock`) — so a stack dump on
/// the CI box would show the one leg's worker as `vike-preflight-`, and BOTH legs'
/// workers as that same prefix, which defeats the whole reason the names exist. The `const`
/// assertion beneath the names holds each one under it AT COMPILE TIME, so a longer name is a
/// build error rather than a truncated dump; `probe_thread_names_survive_linux_truncation` states
/// the same in a test that can also say which name overran, and adds that the two must differ.
const THREAD_NAME_MAX_BYTES: usize = 15;

/// Thread name for a credential probe's spawned worker. Named rather than open-coded because an
/// ABANDONED worker outlives the call that started it (see [`bounded_probe`]'s residual), so this
/// string is what a stack dump has to say which leg is parked — provided it fits
/// [`THREAD_NAME_MAX_BYTES`], which is why it is this short.
const CREDENTIAL_PROBE_THREAD: &str = "vike-pf-cred";

/// Thread name for a clock probe's spawned worker — the twin of [`CREDENTIAL_PROBE_THREAD`], and
/// the one that matters most: the wedged-resolver case this bound exists for is precisely the one
/// whose worker is still parked when an operator looks.
const CLOCK_PROBE_THREAD: &str = "vike-pf-clock";

// Both names fit what Linux keeps — see `THREAD_NAME_MAX_BYTES` for why a longer one is not merely
// untidy. A build error here, not a silently-truncated stack dump on the CI box.
const _: () = {
    assert!(CREDENTIAL_PROBE_THREAD.len() <= THREAD_NAME_MAX_BYTES);
    assert!(CLOCK_PROBE_THREAD.len() <= THREAD_NAME_MAX_BYTES);
};

/// The venue clock leg — `crate::server_time`'s roster-gated dispatch, re-exported here because
/// this is where the preflight's probes are assembled. Returns an ABSOLUTE epoch-ms stamp (which is
/// what [`crate::preflight::PreflightProbes::venue_server_time_ms`] wants — not the OFFSET
/// `BinanceSpotRest::server_time_offset` returns), or one of the two DISTINCT gaps.
///
/// It is a one-line delegation on purpose: the per-venue knowledge (which endpoint, which tier,
/// what a drift costs there, and for the unwired venues WHY) belongs in one table with a roster
/// completeness test, not in a match in the wiring module — which is exactly where it used to live,
/// as a two-arm match whose catch-all told an operator nothing.
///
/// ⚠ **UNBOUNDED, deliberately.** This is the read itself: a `Wired` venue's fetcher runs on the
/// caller's thread, bounded by its agent's `crate::server_time::CLOCK_READ_TIMEOUT` and by nothing
/// that can preempt a wedged name resolution. The preflight never calls it directly — it goes
/// through [`bounded_server_time_ms`], which spawns this on a thread the mount can walk away from at
/// [`CLOCK_PROBE_TIMEOUT`]. A new caller wants that one too, unless it is itself the thread that
/// may be abandoned.
pub fn venue_server_time_ms(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    live_permitted: bool,
) -> Result<i64, ServerTimeGap> {
    crate::server_time::venue_server_time_ms(registry, venue, vars, live_permitted)
}

/// One venue's clock read, ABANDONED at [`CLOCK_PROBE_TIMEOUT`] — [`venue_server_time_ms`] issued
/// through the very same `bounded_probe` the credential leg beside it uses.
///
/// ⚠ **The hole this closes is not a slow venue, it is a wedged RESOLVER.** Every fetcher in
/// `crate::server_time` rides an agent bounded by `crate::server_time::CLOCK_READ_TIMEOUT`, and
/// that bound is a timer over an in-flight request — `std` name resolution has no timeout at all
/// (`vike_bridge_core::net_probe`'s own caveat), so a box whose DNS is wedged parks the FIRST clock
/// read for as long as the OS resolver takes to give up, at the very top of a mount, before one
/// venue is up. The leg-wide budget cannot save it either: `crate::preflight::check_clock_skew`
/// checks the budget BETWEEN reads, so it bounds how MANY reads happen and never how long one of
/// them takes.
///
/// An abandoned read is outcome ② — [`ServerTimeGap::Unreachable`], a WARN that degrades nothing —
/// which is the honest reading: this venue publishes a clock and we did not get one. It can never
/// demote a venue, so a broken resolver costs a noisy line rather than a paper mount.
///
/// ⚠ A venue with no WIRED endpoint is deliberately NOT routed through the probe: its answer comes
/// from its registry row's declared clock (`crate::server_time`'s `clock_decl`) with no I/O, so
/// there is nothing to abandon and bounding it would spawn a thread to run a `match`. Only a read
/// that can BLOCK is spawned, which is also what keeps a mount whose armed venues are all DECLARED
/// at zero threads.
///
/// ⚠ It is deliberately NOT a fan-out: the leg still reads one venue at a time, in roster order,
/// out of one shared budget. `docs/decisions/0027-clock-budget-derived-from-the-roster.md` rejected
/// per-venue slicing and this changes nothing it decided — the budget derivation is untouched.
///
/// `policy` is the deployment's, shared by refcount like `vars`: a contract row's clock is read on
/// the inputs its mount is handed (`crate::server_time::venue_server_time_ms_under_policy`).
///
/// Public so the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` can reach it
/// (docs/decisions/0096: roster tests run where the registry is).
pub fn bounded_server_time_ms(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &Arc<HashMap<String, String>>,
    live_permitted: bool,
    policy: &Arc<Option<crate::MountPolicy>>,
) -> Result<i64, ServerTimeGap> {
    if !matches!(
        crate::server_time::clock_decl(registry, venue),
        Some(vike_bridge_core::venue_mount::ClockDecl::Wired { .. })
    ) {
        return crate::server_time::venue_server_time_ms_under_policy(
            registry,
            venue,
            vars,
            live_permitted,
            policy.as_ref().as_ref(),
        );
    }
    let probe: BoundedProbe<Result<i64, ServerTimeGap>> = {
        let venue = venue.to_string();
        let vars = Arc::clone(vars);
        let policy = Arc::clone(policy);
        Arc::new(move || {
            crate::server_time::venue_server_time_ms_under_policy(
                registry,
                &venue,
                &vars,
                live_permitted,
                policy.as_ref().as_ref(),
            )
        })
    };
    bounded_probe(&probe, CLOCK_PROBE_TIMEOUT, CLOCK_PROBE_THREAD)
        .unwrap_or_else(|waited_ms| Err(abandoned_clock_gap(waited_ms)))
}

/// What an ABANDONED clock read reports: outcome ② and nothing else.
///
/// ⚠ Named rather than inlined so the choice has something to assert against, because the two
/// neighbouring variants would each be a lie in the operator's favour.
/// [`ServerTimeGap::NotChecked`] renders NOT-APPLICABLE — it would print "nothing to check here"
/// over a venue whose clock we simply failed to reach, and it takes a `&'static str` precisely so a
/// runtime failure cannot manufacture one. [`ServerTimeGap::UnmeasuredRisk`] asserts a DECLARED
/// property of the venue, which this is not. `Unreachable` says the true thing: this venue publishes
/// a clock and this attempt did not get one. It is a WARN, so no venue can be demoted by our own
/// resolver being wedged.
///
/// ⚠ **Residual: two things that are not a wedged read render as this row too.** [`bounded_probe`]
/// maps a worker that exited without answering (a panic inside a venue client — its sender dropped,
/// so `recv_timeout` reads `Disconnected`) onto the same `Err(bound)` as a genuine timeout, so a
/// panicking fetcher produces this text after ~0 ms rather than after the bound it names; and a
/// worker that could not be SPAWNED at all comes back as `Err(0)` (a fault on THIS box, not the
/// venue's — that arm's own comment says why it is not reported as a refusal), so that case reads
/// "inside the mount's own 0 ms bound". The row is written so that it stays TRUE in both — no
/// answer did arrive inside the bound named, and the mount did stop waiting — but "abandoned" is
/// then a description of what the mount did, not of a thread still running. The verdict is right
/// for all three (② WARN, nothing demoted: a dead or unspawnable worker is no more evidence about
/// the venue than a silent one), and both are the pre-existing shape of the credential leg, whose
/// `Unanswered` has rendered `waited_ms` from the same two arms since [`bounded_probe`] was written;
/// a panicking fetcher is a bug this row makes visible rather than one it hides.
fn abandoned_clock_gap(waited_ms: u64) -> ServerTimeGap {
    ServerTimeGap::Unreachable(format!(
        "no answer inside the mount's own {waited_ms} ms bound, so the mount stopped waiting (the \
         read's own transport timeout cannot preempt a wedged name resolution, so the read was \
         abandoned, not cancelled — a worker that died before answering reads the same here)"
    ))
}

/// The venues the CLOCK leg runs for: every canonical-roster venue this `vars` map would mount
/// LIVE **under this deployment's arming ceiling**, in roster order (deterministic, and the same
/// order every other roster walk uses).
///
/// ⚠ Derived from live INTENT rather than from the authed-read client map — see the module doc's
/// two-lists note. [`crate::would_mount_live_under_policy`] is pure and network-free (it calls each
/// arm's own config loader), so this costs nothing on a paper mount and yields an EMPTY list there,
/// which keeps the "no credentials ⇒ not one network call" property intact.
///
/// ⚠ **It is the CEILING-AWARE predicate, and it is the same one `make_engine` gates on** — see the
/// module doc's "what the ceiling is doing in a CLOCK leg" section for why a check whose measured
/// QUANTITY is box-scoped is nonetheless gated per venue.
///
/// A venue with no wired endpoint is deliberately still LISTED: its row is the DECLARED
/// not-applicable line, which is the disclosure — silence would be the old defect wearing a
/// different hat.
///
/// Public so the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` can reach it
/// (docs/decisions/0096: roster tests run where the registry is).
pub fn clock_venues(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> Vec<String> {
    vike_model::VENUES
        .iter()
        .filter(|venue| crate::would_mount_live_under_policy(registry, venue, vars, policy))
        .map(|venue| (*venue).to_string())
        .collect()
}

/// The per-venue thresholds + remediation text for a MEASURED skew, from each venue's own declared
/// `ClockRisk`. Only wired venues have one (an unmeasurable venue has nothing to judge), and only
/// the venues that REJECT orders over drift carry a FAIL threshold — see
/// `crate::server_time::clock_policy_of`.
///
/// Public so the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` can reach it
/// (docs/decisions/0096: roster tests run where the registry is).
pub fn clock_policies(
    registry: &'static [crate::VenueRow],
    venues: &[String],
) -> HashMap<String, ClockPolicy> {
    venues
        .iter()
        .filter_map(|venue| {
            crate::server_time::clock_policy(registry, venue).map(|p| (venue.clone(), p))
        })
        .collect()
}

/// One venue's credential probe: perform that venue's cheap authenticated read, `Ok(())` if the
/// request SIGNED and was ACCEPTED.
///
/// A closure rather than a prepared client because the two probe SHAPES differ in WHEN the client
/// may be built — see the module doc.
///
/// ⚠ `Arc<… + Send + Sync>` rather than the `Box<… + Send>` it was, and both halves are
/// load-bearing: [`bounded_probe`] MOVES a handle onto a spawned thread (so the probe must be
/// `Send + Sync + 'static`) and may spawn a SECOND one for the retry (so ownership must be
/// shareable, not consumed). Every existing closure already satisfies the wider bound — each holds
/// only a `Mutex<Box<dyn ReconClient>>` (`ReconClient: Send`, and `Mutex<T>: Sync` whenever
/// `T: Send`) or plain config data — so nothing about their construction changes.
type CredentialProbe = BoundedProbe<Result<(), String>>;

/// Anything [`bounded_probe`] can run: a nullary closure it MOVES onto a spawned thread and can
/// then walk away from. Both legs' probes are one of these — the credential leg's answers
/// `Result<(), String>`, the clock leg's `Result<i64, ServerTimeGap>` — and the alias exists so the
/// abandonment is written once rather than once per leg. See [`CredentialProbe`] for why the bound
/// is `Send + Sync` rather than the narrower `Send` a single-shot spawn would need.
type BoundedProbe<T> = Arc<dyn Fn() -> T + Send + Sync>;

/// The venue → probe map the credential leg reads through, and the authority for
/// [`PreflightConfig::credential_venues`].
type CredentialProbes = HashMap<String, CredentialProbe>;

/// What an identity-recording probe hands the venue's answer to:
/// `crate::book_identity::record_authenticated_account`, except in a test that captures it.
type IdentityRecorder = fn(
    &str,
    &vike_model::account_keys::AccountLabel,
    vike_config::VenueMode,
    Option<&dyn ReconClient>,
    &vike_bridge_core::account_directory::AccountDirectory,
);

/// **The credential probe that reads the balance and then records which account the key is.**
/// [`authed_read_probes`] builds it for every contract row whose `credential_probe` answers
/// `CredentialProbe::RecordsIdentity` — binance's, bybit's and okx's.
/// `record` is [`authed_read_probes`]' production recorder, or a test's.
fn identity_recording_probe(
    venue: String,
    client: Box<dyn ReconClient>,
    tier: vike_config::VenueMode,
    directory: vike_bridge_core::account_directory::AccountDirectory,
    record: IdentityRecorder,
) -> CredentialProbe {
    let client = Mutex::new(client);
    Arc::new(move || {
        let guard = client.lock().map_err(|_| "preflight probe lock poisoned".to_string())?;
        // ⚠ ORDERING, and it is the same rule `make_engine_for_account` states at its own call
        // site: the BALANCE first. On binance spot the account id rides `/api/v3/account`, which
        // is exactly the body this read pulls, so recording after it costs that venue nothing —
        // and recording before it would spend a second signed request to fetch the body this line
        // was about to fetch anyway.
        guard.fetch_balance()?;
        // ⚠ The probe's VERDICT is the balance read alone. A venue that answered its balance and
        // then could not name its account is a HEALTHY credential — demoting it over a bookkeeping
        // field would turn a live venue into paper for a reason that has nothing to do with whether
        // the key works. `record_authenticated_account` returns `()` and warns on its own failures,
        // so there is no way for this line to say no.
        record(
            &venue,
            &vike_model::account_keys::AccountLabel::Default,
            tier,
            Some(&**guard),
            &directory,
        );
        Ok(())
    })
}

/// Build one credential probe per venue whose credentials RESOLVE — every contract row's own
/// `credential_probe` (alpaca's, binance's, bybit's and okx's eager, ctrader's lazy).
///
/// Absent credentials ARE the live gate, so a venue with no keys yields no probe, is therefore not
/// listed in [`PreflightConfig::credential_venues`], and is never checked (preflight only ever
/// DEMOTES a venue it checked — an unchecked one stays `Live`).
///
/// ⚠ **So is the arming ceiling, and it is the account-scoped gate**, on branch 4 below (the number
/// is kept as the ports delete branches, because later tasks name them). A probe issues a SIGNED
/// read against the operator's real account, so a venue this deployment capped to `paper` must
/// never reach it — the whole meaning of `paper` is "do not touch this account". The gate is
/// [`crate::would_mount_live_under_policy`], the same predicate `make_engine` arms on, so a venue
/// is probed if and only if it is about to be mounted live. A venue capped to `paper` yields no
/// probe, and ctrader's is the one where that matters most: its probe OPENS AND AUTHENTICATES a
/// protobuf socket at the venue (inside the closure `crates/bridges/ctrader/src/mount.rs`'s
/// `CtraderVenueMount::credential_probe` returns), so an un-gated capped ctrader would log a live
/// session in at the venue during a mount that is about to hand it a paper client.
///
/// ⚠ The ceiling also decides the TIER: each row's `credential_probe` is asked with the same
/// [`crate::ceiling_permits_live`] fold `make_engine` applies, so a `demo` ceiling never signs a
/// read against the real-money account. Before decision 0095, `cex_mainnet_enabled` alone was the
/// whole answer here, so a `demo`-capped binance with `BINANCE_MAINNET=1` and both key sets present
/// was sent a signed **MAINNET** balance read while the mount bound the demo host. Each CEX bridge
/// pins its half under `a_demo_ceiling_never_signs_against_the_real_money_tier`.
///
/// ⚠ **And the ceiling is ENFORCED here, not just handed over.** A bridge that answers
/// `RecordsIdentity { bound_tier: Live }` under a ceiling below `live` has chosen its LIVE keys
/// despite `live_permitted` being `false`; its probe is not built (the venue is absent from the
/// map, at `error!`), because the client it wraps would read the real-money balance. That is a
/// third place the ceiling interlock holds, beside the resolve half and the mount half in
/// `crate::contract` (whose `within_the_ceiling` covers the identity record too), and
/// `a_probe_bound_to_the_live_tier_under_a_demo_ceiling_is_never_built` pins it. A `ReadOnly`
/// probe is the bridge's own closure and carries no tier to check.
pub fn authed_read_probes(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> CredentialProbes {
    authed_read_probes_with(
        registry,
        vars,
        policy,
        crate::book_identity::record_authenticated_account,
    )
}

/// [`authed_read_probes`] over a caller-supplied identity recorder — the test seam.
fn authed_read_probes_with(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
    record: IdentityRecorder,
) -> CredentialProbes {
    let mut out = CredentialProbes::new();

    // The DIRECTORY is cloned ONCE, outside the loop: these closures are `'static` (`bounded_probe`
    // moves them onto a spawned thread it may then walk away from), so they cannot borrow the
    // policy. It holds the `account` rows and the key-name map the composition root already read.
    let directory = crate::arming::directory_of(policy).clone();

    // 4. Every CONTRACT row that offers a probe, behind the ceiling gate (the fn doc).
    let default_account = vike_model::account_keys::AccountLabel::Default;
    for row in registry {
        let crate::VenueRow::Mount(m) = row else { continue };
        let venue = m.venue();
        if !crate::would_mount_live_under_policy(registry, venue, vars, policy) {
            continue;
        }
        let process = crate::contract::process_facts();
        let live = crate::ceiling_permits_live(crate::venue_ceiling(policy, venue));
        let inputs =
            crate::contract::inputs_for(*m, &default_account, vars, live, policy, &process);
        match m.credential_probe(&inputs) {
            Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
                client,
                bound_tier,
            }) => {
                // ⚠ **THE CEILING INTERLOCK, startup-probe half.** The client this probe wraps does
                // the balance read, so a probe BOUND to the LIVE tier under a ceiling below `live`
                // is a signed read against the real-money account that ceiling forbids — and this
                // leg reaches neither `resolve` nor `mount`, so `crate::contract`'s two halves
                // cannot see it. It is not built: the venue gets no row, which is the state of a
                // venue with no probe (never checked, therefore never demoted). A correct bridge
                // never gets here — `live` was `false`, so it could not have chosen its LIVE keys.
                // Same level and fields as `crate::contract`'s `refuse_beyond_the_ceiling`; the
                // account is the DEFAULT one, the only account this leg ever asks about.
                if bound_tier == vike_bridge_core::venue_mount::Tier::Live && !live {
                    tracing::error!(
                        venue,
                        account = %default_account,
                        "{venue}: the bridge went past this account's arming ceiling — its \
                         `credential_probe` bound the LIVE tier while the ceiling is below `live`. \
                         Refused: no startup credential probe is built for it. A bridge may reach \
                         its live tier only when `MountInputs::live_permitted` is true, so this is \
                         a defect in the bridge."
                    );
                    continue;
                }
                // ⚠ **THIS LEG ALSO RECORDS WHICH ACCOUNT THE KEY IS**, and it is the site that
                // covers a venue this box ARMS but never MOUNTS. MEASURED on the CI box 2026-09-20:
                // 16 `account` rows across 13 venues, and a `tradehub.toml` that mounts exactly
                // one. `make_engine_for_account`'s own call would have recorded that one and left
                // the store asking about the rest.
                // `crate::book_identity::record_authenticated_account` carries the whole argument,
                // including why deribit is NOT reachable from here and needs the mount site too.
                //
                // The tier the bridge's credentials BIND — never the ceiling, which may be wider.
                let tier = crate::contract::tier_mode(bound_tier);
                out.insert(
                    venue.to_string(),
                    identity_recording_probe(
                        venue.to_string(),
                        client,
                        tier,
                        directory.clone(),
                        record,
                    ),
                );
            }
            Some(vike_bridge_core::venue_mount::CredentialProbe::ReadOnly(probe)) => {
                out.insert(venue.to_string(), probe);
            }
            None => {}
        }
    }

    out
}

/// Run ONE probe with a hard wall-clock bound, on a thread the caller can walk away from. THE bound
/// the credential leg never had — and the seam that makes a FAIL mean something — now shared with
/// the CLOCK leg, which had the identical hole for the identical reason (module doc).
///
/// `Ok(v)` = the probe answered inside `timeout`, with whatever it answers; `Err(waited)` = nothing
/// came back. For a credential probe that reads `Ok(Ok(()))` = accepted and `Ok(Err(e))` = the venue
/// ANSWERED and refused.
///
/// ⚠ Generic over the probe's own result type DELIBERATELY, and the two legs keep their separate
/// vocabularies (`Result<(), String>` against `Result<i64, ServerTimeGap>`) because they mean
/// different things — a refusal demotes a venue, a stale clock only warns. What is shared is the
/// ABANDONMENT and nothing else, so this stayed one primitive instead of becoming two. `name` is the
/// spawned worker's thread name, so a stack dump says which leg is stuck; it is the only per-leg
/// knob here.
///
/// ⚠ The probe is not cancelled, because a blocking `ureq` call cannot be: it is ABANDONED. The
/// spawned thread runs to its own agent's completion, finds the result channel dropped, and exits —
/// the same "signal stop, never join" shape [`NetProbeThread`]'s `Drop` uses, and for the same
/// reason. Nothing downstream can observe the abandoned thread: it owns its own handle through the
/// shared [`BoundedProbe`], writes to nothing else, and the preflight is over long before a venue
/// mounts.
///
/// ⚠ **The residual, declared for both legs: an abandoned probe LEAVES A THREAD RUNNING.** In the
/// ordinary case the worker exits on its own transport timeout — a credential worker on its
/// `ReconClient`'s 30 s agent, a clock worker on `crate::server_time::CLOCK_READ_TIMEOUT` — EXCEPT
/// the one case the clock bound exists for, and it is the SAME case for BOTH legs: a wedged
/// `getaddrinfo`. Every probe here resolves names through the one `std` resolver, which no ureq
/// agent can preempt, so a worker of EITHER leg parks until the OS resolver gives up and this
/// process therefore carries one parked thread for that long. That is the trade: one leaked thread
/// on a box whose DNS is broken, instead of a mount that never arms. It is bounded in COUNT rather
/// than in time — at most ONE abandoned worker per venue per preflight, for either leg, and the
/// preflight runs once per process: `crate::preflight::check_clock_skew` returns the venue's row
/// on the first [`ServerTimeGap::Unreachable`] and never resamples past it, and
/// [`authed_read_probe`] never retries a silence (its one retry is spent only on an attempt that
/// ANSWERED, whose worker has therefore already exited). Neither leg can leak a second thread for
/// the same venue.
///
/// ⚠ A worker that DIES is indistinguishable from one that is abandoned: `recv_timeout`'s
/// `Disconnected` (the probe panicked, so its sender dropped) lands in the same `Err(bound)` arm as
/// a timeout, after however long the panic took rather than after the bound. Both legs render it
/// as their un-answer — see [`abandoned_clock_gap`] for the clock leg's wording and why the verdict
/// is right for both; the credential leg's `Unanswered` carries the same shape.
fn bounded_probe<T: Send + 'static>(
    probe: &BoundedProbe<T>,
    timeout: Duration,
    name: &str,
) -> Result<T, u64> {
    let (tx, rx) = std::sync::mpsc::channel();
    let probe = Arc::clone(probe);
    // A spawn failure must not be reported as a venue refusing us: it is a fault on THIS box.
    // Falling back to the blocking call would reintroduce the unbounded wait, so it is reported as
    // the un-answer it is.
    if std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            // The receiver is gone on a timeout; that send failing is the normal abandoned path.
            let _ = tx.send(probe());
        })
        .is_err()
    {
        return Err(0);
    }
    match rx.recv_timeout(timeout) {
        Ok(result) => Ok(result),
        // Timeout AND Disconnected land here alike. Disconnected means the probe thread died
        // without sending (a panic inside a venue client), which is likewise not evidence about the
        // operator's keys — so it must not be allowed to demote a venue either.
        Err(_) => Err(timeout.as_millis().min(u128::from(u64::MAX)) as u64),
    }
}

/// The credential leg over a prepared [`CredentialProbes`] map: one cheap authed read per venue,
/// BOUNDED by [`CREDENTIAL_PROBE_TIMEOUT`] and retried once ([`CREDENTIAL_PROBE_ATTEMPTS`]).
///
/// `Ok` — including a venue whose `fetch_balance` answers `Ok(None)` — means the request SIGNED and
/// was ACCEPTED, which is the whole question this leg asks; the balance VALUE is not used. A venue
/// absent from the map is [`CredentialGap::Rejected`], but that can never fire in practice because
/// [`run_startup_preflight`] derives the checked venue list from this same map.
///
/// ⚠ **Which `Err` variant comes back is the whole enforcement decision** (module doc): a venue that
/// ANSWERED and refused is `Rejected` and will be mounted PAPER; a venue that never answered is
/// `Unanswered` and is mounted exactly as configured. The retry is spent only on the first case —
/// re-waiting a timeout would double the leg's worst case to learn the same nothing twice.
///
/// The `Mutex` guards the map, not a probe: it is released BEFORE the bounded call, so one wedged
/// venue cannot park the venues behind it on a lock (which is what holding it across the probe
/// would have done, quietly converting the per-venue bound back into a serial global one).
fn authed_read_probe(
    probes: CredentialProbes,
) -> impl Fn(&str) -> Result<(), CredentialGap> + Send + Sync + 'static {
    let probes = Mutex::new(probes);
    move |venue: &str| {
        let probe = {
            let guard = probes.lock().map_err(|_| {
                CredentialGap::Rejected("preflight probe lock poisoned".to_string())
            })?;
            guard.get(venue).map(Arc::clone).ok_or_else(|| {
                CredentialGap::Rejected(format!("no reconcile client built for {venue}"))
            })?
        };
        let mut last: Option<String> = None;
        for attempt in 0..CREDENTIAL_PROBE_ATTEMPTS.max(1) {
            if attempt > 0 {
                std::thread::sleep(CREDENTIAL_RETRY_BACKOFF);
            }
            match bounded_probe(&probe, CREDENTIAL_PROBE_TIMEOUT, CREDENTIAL_PROBE_THREAD) {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(e)) => last = Some(e),
                // No retry after a silence — see this fn's doc.
                Err(waited_ms) => {
                    let detail = format!(
                        "no answer from the venue within the mount's own bound (the client's \
                         transport keeps its own, longer timeout; this probe was abandoned, not \
                         cancelled){}",
                        last.map_or_else(String::new, |e| format!("; earlier attempt said: {e}"))
                    );
                    return Err(CredentialGap::Unanswered { waited_ms, detail });
                }
            }
        }
        // Every attempt ANSWERED and refused: confirmed, and this is the row that demotes.
        Err(CredentialGap::Rejected(
            last.unwrap_or_else(|| "authenticated read failed and reported nothing".to_string()),
        ))
    }
}

/// Free bytes on the filesystem holding `dir` — the disk leg, armed at last.
///
/// ⚠ On unix this is `rustix::fs::statvfs`, a SAFE wrapper over the POSIX syscall in a crate
/// `Cargo.lock` had already resolved: no `unsafe`, no `UNSAFE_EXEMPT` carve-out in a binary that
/// signs orders, and no new package in the audit surface. `f_bavail × f_frsize` is the
/// NON-PRIVILEGED free space — deliberately not `f_bfree`, which counts the root-reserved blocks a
/// daemon running as a normal user can never touch, and which would therefore report headroom the
/// recorder cannot actually write into.
///
/// On Windows the equivalent (`GetDiskFreeSpaceExW`) has no dependency-free safe route, so this
/// returns a DECLARED reason and the leg WARNs with it. That is the honest answer for the dev box;
/// both shipped daemons run on Linux.
#[cfg(unix)]
pub fn free_space_bytes(dir: &Path) -> Result<u64, String> {
    let stat = rustix::fs::statvfs(dir).map_err(|e| format!("statvfs failed: {e}"))?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// The Windows half of [`free_space_bytes`] — see that fn's doc for why it declares rather than
/// measures.
#[cfg(not(unix))]
pub fn free_space_bytes(_dir: &Path) -> Result<u64, String> {
    Err("free space is not queried on this platform (rustix's statvfs is POSIX-only, and the \
         Windows call has no dependency-free safe route); the leg is armed on unix, where both \
         shipped daemons run"
        .to_string())
}

/// Deduplicate the caller's watched directories, preserving order. Two labels can legitimately
/// resolve to ONE path (a journal written inside the store root), and two rows for one filesystem
/// would double every finding without adding a fact.
fn disk_dirs(dirs: &[(String, PathBuf)]) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::with_capacity(dirs.len());
    for (label, path) in dirs {
        if !out.iter().any(|(_, p)| p == path) {
            out.push((label.clone(), path.clone()));
        }
    }
    out
}

/// Spawn the [`NetProbe`] and wait — at most `wait` — for its first round to complete, so the
/// network leg reads a MEASUREMENT rather than the handle's optimistic initial value. The returned
/// [`NetProbeThread`] must be kept alive for as long as its handle is read; dropping it signals
/// stop WITHOUT joining (so a wedged in-flight DNS resolve can never park the mount).
pub fn spawn_net_probe(wait: Duration) -> NetProbeThread {
    let thread = NetProbe::with_defaults().spawn();
    let handle = thread.handle();
    let deadline = Instant::now() + wait;
    while !handle.has_probed() && Instant::now() < deadline {
        std::thread::sleep(NET_PROBE_POLL);
    }
    thread
}

/// ENFORCE a preflight demotion: remove every `{VENUE}_`-prefixed key from the credential map the
/// mount is about to read, so `make_engine` sees absent credentials and lands on the paper
/// fallback. Returns how many keys were withheld, which the caller discloses.
///
/// ⚠ This is deliberately NOT a new "force paper" flag threaded through every `make_engine` call.
/// **Absent credentials ARE the live gate** (root `CLAUDE.md`), so withholding them reuses the exact
/// path every credential-less venue already rides, instead of adding a parallel switch that could
/// disagree with it. The venue's inline `ReconClient` factory sits behind the same credentials, so a
/// demoted venue also reconciles nothing — which is the safe direction: reconciling a live account
/// against a paper engine is how `PositionDrift` imports live positions into paper books.
///
/// The PREFIX is the rule rather than an enumerated key list, for the reason
/// `crates/vike-tradehub/src/tradehub_cli.rs`'s `withhold_exec_credentials` already argues at length for its
/// `data_only` twin: every key family this workspace resolves for a venue is `{VENUE}_`-spelled
/// (`vike_bridge_core::credentials::load_credentials_from`, the per-venue config loaders,
/// `vike_model::attribution::attribution_for`), so a future spelling is withheld the day it exists,
/// and over-withholding can only make the mount MORE paper. `vike_model::credential_keys`' grid is
/// NOT sufficient here: it folds only the standard `{VENUE}_{TIER}_{SUFFIX}` shape, while alpaca
/// (`ALPACA_SANDBOX_CLIENT_ID`), ctrader (`CTRADER_DEMO_ACCESS_TOKEN`) and ig/oanda carry bespoke
/// ones that the grid cannot name — and those are exactly the venues this leg probes.
pub fn withhold_venue_credentials(vars: &mut HashMap<String, String>, venue: &str) -> usize {
    let prefix = format!("{}_", venue.to_uppercase());
    let withheld: Vec<String> = vars.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
    for key in &withheld {
        vars.remove(key);
    }
    withheld.len()
}

/// Whether ANY venue on the canonical roster would mount live from `vars` **under this
/// deployment's arming ceiling** — the pure, network-free live-INTENT probe `make_engine`'s own
/// pre-connect refusal uses. Gates whether a [`NetProbe`] is spawned at all (see the module doc).
///
/// ⚠ Ceiling-aware even though the [`NetProbe`] is the one leg that touches NO venue and NO account
/// (it resolves `one.one.one.one` and `dns.google` —
/// `vike_bridge_core::net_probe::DEFAULT_PROBE_HOSTS`). Two reasons, neither of them "for
/// symmetry": the leg's own rendered verdict is *"no venue would mount live, so no order path
/// depends on connectivity"* (`crate::preflight::check_network`), which an all-paper box makes TRUE
/// — an uncapped gate would have that row assert a live order path that this mount does not have —
/// and a second predicate for "is anything armed" is precisely the drift this lane exists to
/// remove.
///
/// Public so the roster tests in `crates/vike-tradehub/tests/mount_roster.rs` can reach it
/// (docs/decisions/0096: roster tests run where the registry is).
pub fn any_venue_would_mount_live(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> bool {
    vike_model::VENUES
        .iter()
        .any(|venue| crate::would_mount_live_under_policy(registry, venue, vars, policy))
}

/// **The preflight skip, resolved from BOTH sources** — the OR [`run_startup_preflight`] used to
/// spell inline, named and pure so the precedence a SAFETY OVERRIDE rests on is a test.
///
/// `process_env` is the REAL `std::env::vars()` sweep; `vars` is the caller's map, which for the
/// one production root is the credential store with the resolved `flags.preflight_skip`
/// OVERWRITTEN into it (`crates/vike-tradehub/src/tradehub_cli.rs`'s `fold_flags_into_vars`).
/// Both sides use [`preflight_skipped`]'s exact-`"1"` grammar, so the `"0"` an exported
/// `VIKE_PREFLIGHT_SKIP=0` resolves a `flags.toml` `true` down to skips nothing.
///
/// ⚠ **The invariant the caller must supply, and the residual if it does not.** This OR can only
/// WIDEN what the process env says. That is right for a FILE layer, because `vike_config::load`
/// applies the environment over the file before the value is folded
/// (`crates/vike-config/tests/flag_registry.rs`'s
/// `the_environment_overrides_the_row_for_every_flag`) and the fold OVERWRITES rather than
/// deferring to whatever the credential store held. A caller that passes a RAW credential map
/// instead gives `<project>/settings/secrets.env` a vote this reader never granted it — a line
/// there would skip the whole startup preflight past an operator who exported `=0`. `vike-tradehub`
/// is the only production root that reaches `vike_mount::build_node`, and it folds.
pub fn preflight_skip_requested(
    process_env: &HashMap<String, String>,
    vars: &HashMap<String, String>,
) -> bool {
    preflight_skipped(process_env) || preflight_skipped(vars)
}

/// Run the startup preflight with the REAL probes. `vars` is the workspace `.env` credentials map
/// (the same one `make_engine` gates on) with the composition root's resolved flags folded in; the
/// SKIP flag is read from the real process env FIRST and from `vars` second — see
/// [`preflight_skip_requested`], which is where that precedence and its caller-side invariant are
/// written down. It was process-env-ONLY until `flags.preflight_skip` was wired, for the
/// `.env`-is-not-exported gotcha, and that half has not moved.
///
/// Never panics, never blocks a mount, and returns a report the caller logs. See the module doc for
/// which legs are armed and what this costs.
///
/// ⚠ `dirs` is a PARAMETER rather than something this module resolves, and that is the whole reason
/// the disk leg can be honest. The paths a mount actually writes to are decided ABOVE here — the
/// journal directory comes from a `RunProfile`'s `[sinks.journal]` or `VIKE_JOURNAL_DIR` and lands
/// in `CoreConfig::journal` — so a copy of that resolution in this module would be a second
/// authority that could disagree with the first, and would measure a directory nothing writes to.
/// `vike_mount::build_node` passes the dir it actually built the journal with. An empty slice means
/// no disk leg at all, which is what a paper/CI mount wants and what keeps the offline property.
///
/// ⚠ `policy` is this deployment's per-venue ARMING CEILING, and it is the parameter that makes the
/// preflight obey the operator. **`None` reads all-`paper`**, exactly as [`crate::make_engine`]'s
/// own ceiling seam does and through the same [`crate::venue_ceiling`] — so a caller that threads
/// no policy contacts NOTHING, and the widening mistake has to be typed rather than reached by
/// omission. `vike_mount::build_node` passes `Some(&cfg.policy)`, which is the one production call
/// site.
pub fn run_startup_preflight(
    registry: &'static [crate::VenueRow],
    vars: &HashMap<String, String>,
    dirs: &[(String, PathBuf)],
    policy: Option<&crate::MountPolicy>,
) -> PreflightReport {
    // The skip flag is a `VIKE_*` operator toggle, so it must come from the REAL process env —
    // reading it from the credentials map ALONE would make a shell-exported `=1` invisible.
    //
    // ⚠ It is read from BOTH now, and `vars` is the second source rather than a replacement.
    // [`preflight_skip_requested`] is the OR, named and pure so the precedence is a TEST rather
    // than this paragraph; its doc carries the invariant the caller has to supply and the residual
    // for a caller that does not.
    let process_env: HashMap<String, String> = std::env::vars().collect();
    if preflight_skip_requested(&process_env, vars) {
        // Short-circuit BEFORE any probe is built or any thread is spawned: `run_preflight_gated`
        // returns the empty skipped report without touching either argument.
        return run_preflight_gated(
            &process_env,
            &PreflightConfig::default(),
            &FnProbes::new(),
            None,
        );
    }

    let probes_by_venue = authed_read_probes(registry, vars, policy);
    // The CREDENTIAL leg's list IS the authed-readable set (see the module doc's INVARIANT)…
    let mut credential_venues: Vec<String> = probes_by_venue.keys().cloned().collect();
    credential_venues.sort();
    // …and the CLOCK leg's is every venue about to mount live, which is a different question.
    let clock_venues = clock_venues(registry, vars, policy);
    let clock_policies = clock_policies(registry, &clock_venues);
    // The budget is DERIVED from how many of those venues are actually READ — a venue with no clock
    // leg costs nothing, so only the wired ones buy time. Deriving it here rather than taking
    // `PreflightConfig::default()`'s floor is what stops it rotting as the roster grows: the fixed
    // 5000 ms was sized against a six-read measurement and, by the time ten venues mounted, one
    // unreachable venue was enough to leave eight of them unread.
    let wired = clock_venues
        .iter()
        .filter(|v| {
            matches!(
                crate::server_time::clock_decl(registry, v),
                Some(vike_bridge_core::venue_mount::ClockDecl::Wired { .. })
            )
        })
        .count();
    let probes = FnProbes::new()
        .with_venue_server_time_ms({
            // ⚠ `Arc` rather than the plain clone this was: `bounded_server_time_ms` rebuilds a
            // probe closure per read (each one must OWN what it captures, because the mount may
            // abandon it), so the map — and the policy a contract row's clock read is handed —
            // are shared by refcount instead of copied per venue per sample.
            let vars = Arc::new(vars.clone());
            let policy = Arc::new(policy.cloned());
            move |venue: &str| {
                let live = crate::ceiling_permits_live(crate::venue_ceiling(
                    policy.as_ref().as_ref(),
                    venue,
                ));
                bounded_server_time_ms(registry, venue, &vars, live, &policy)
            }
        })
        .with_venue_authed_read(authed_read_probe(probes_by_venue))
        .with_free_space_bytes(free_space_bytes);
    let cfg = PreflightConfig {
        // The credential budget is DERIVED from how many venues are actually probed, for the same
        // reason the clock one is: a fixed number sized against today's roster silently drops the
        // venues at the end of tomorrow's.
        credential_budget_ms: credential_budget_for(credential_venues.len()),
        clock_venues,
        credential_venues,
        clock_policies,
        clock_budget_ms: crate::preflight::clock_budget_for(wired),
        dirs: disk_dirs(dirs),
        ..PreflightConfig::default()
    };

    let net = any_venue_would_mount_live(registry, vars, policy)
        .then(|| spawn_net_probe(DEFAULT_NET_PROBE_WAIT));
    let handle = net.as_ref().map(NetProbeThread::handle);
    let report = run_preflight_gated(&process_env, &cfg, &probes, handle.as_ref());
    // Signals stop without joining — see `NetProbeThread`'s `Drop` doc.
    drop(net);
    report
}

#[path = "startup_tests.rs"]
#[cfg(test)]
mod startup_tests;

/// **The preflight skip's precedence** — the second half of the chain that keeps a SAFETY OVERRIDE
/// from being armed by a file an operator has already contradicted.
///
/// The first half is proved in the composition root
/// (`crates/vike-tradehub/src/tradehub_cli.rs`'s
/// `a_process_env_value_beats_a_file_value_for_every_wired_key`): an exported
/// `VIKE_PREFLIGHT_SKIP=0` over a `flags.toml` `preflight_skip = true` resolves to `false` and is
/// written into `vars` as the exact string `"0"`. These tests start from that string.
#[path = "preflight_skip_precedence_tests.rs"]
#[cfg(test)]
mod preflight_skip_precedence_tests;
