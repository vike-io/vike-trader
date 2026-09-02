//! The **mount-site wiring** of [`crate::preflight`] — the one place the pure go/no-go gate is
//! handed REAL probes. `vike_run::build_node` calls [`run_startup_preflight`] once, at the top of
//! the twelve-venue assembly, before any venue is mounted.
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
//!   orders over drift (`crate::server_time::ClockRisk::policy`).
//! - **credentials — ARMED, through each venue's own `ReconClient`.** [`authed_read_probes`] builds
//!   one PROBE per venue whose credentials resolve, each performing that venue's own cheap
//!   authenticated read (`fetch_balance`), and the leg calls it. ⚠ INVARIANT: the configured
//!   [`PreflightConfig::credential_venues`] list is DERIVED from that probe map, never written by
//!   hand, because an unwired credential leg FAILs by design ("we could not prove these keys work"
//!   must not mount a venue live) — listing a venue we cannot authed-read would manufacture a FAIL.
//!
//!   Two SHAPES of probe, and the difference is load-bearing rather than cosmetic:
//!
//!   * **Eagerly-built** (binance/bybit/okx via `build_recon_client`, alpaca via its own
//!     `vike_alpaca::recon_client`): construction is PURE — a signer plus a transport, no network —
//!     so the client is built up front and the probe closure just calls `fetch_balance`.
//!   * **Lazily-built** (ctrader): `vike_ctrader::recon_client` CONNECTS during construction (one
//!     multiplexed protobuf socket, authenticated at handshake — there is no cheaper authed read at
//!     this venue). Building it eagerly would be a silent hole in exactly the case the leg exists
//!     for: a refused grant returns `None`, the venue would then be absent from the probe map, and
//!     absent from the map means NO ROW AT ALL — an expired token would produce silence rather than
//!     the FAIL it is. So ctrader's probe defers construction into the closure, where a refused
//!     handshake becomes the leg's error instead of vanishing.
//!
//!   ⚠ **Every probe's error NAMES THE HOST** it could not reach or that refused it. The rehearsal
//!   this leg comes from spent its whole diagnosis budget establishing, by hand and after the fact,
//!   that every Alpaca host was TCP-unreachable from that box while the mount had already announced
//!   `exec="LIVE"` — a fact the daemon could have printed at startup.
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
//! IG's clock row is the one CREDENTIALED read in the table
//! (`crate::server_time`'s `ig_time` sends `X-IG-API-KEY`), while the credential leg beside it
//! issues a SIGNED `fetch_balance`. An operator who sets a venue to `paper` has said *do not touch
//! this account*, and the preflight was authenticating against it anyway.
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
//!   that venue's own `crate::server_time::ClockRisk::policy`), and the CONSEQUENCE is per-venue
//!   (a FAIL demotes that venue to paper — a no-op for a venue that is already paper). So a
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
//! ## The one residual, declared
//!
//! The ceiling decides WHICH venues are read and, in the credential leg, WHICH TIER is signed
//! ([`authed_read_clients`]). It does **not** reach the clock FETCHERS' own tier choice:
//! `crate::server_time::ClockFetch` is a `fn(&HashMap<String, String>)` in a `const` table, so
//! binance/bybit/okx/aster/hyperliquid still resolve their host from `{VENUE}_MAINNET` alone. Under
//! a `demo` ceiling with that flag armed, the clock read therefore goes to the MAINNET host while
//! the mount binds the demo one — measuring a clock that will not judge our orders (the two tiers
//! genuinely differ; `crate::server_time`'s "tier discipline" section has the paired measurement).
//! It is KEYLESS and WARN-only, so no account is touched and nothing can be demoted by it, which is
//! why it is declared here rather than fixed with a widened `ClockFetch` signature.
//!
//! ⚠ **The two venue lists are SEPARATE, and merging them was the second half of the defect.**
//! `PreflightConfig::venues` used to feed both legs and was derived from [`AUTHED_READ_MARKETS`],
//! so the clock leg ran for the crypto-CEX trio and NOTHING else: wiring an endpoint for deribit or
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
//! it replaced was bounded by 30 s. Two named bounds replace that: every fetcher runs on an agent
//! bounded by `crate::server_time::CLOCK_READ_TIMEOUT` (3 s), and the leg as a whole is capped by
//! [`crate::preflight::DEFAULT_CLOCK_BUDGET_MS`] (5 s across every venue and every resample), so the
//! worst case is **budget + one in-flight read ≈ 8 s**, against a healthy cost the live smoke
//! MEASURED at 1781 ms from the CI box.
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
//!   waited out. The abandoned thread is harmless: it finishes on its own agent's timeout, finds
//!   its result channel dropped, and exits. This is the same "signal stop without joining" shape
//!   [`NetProbeThread`]'s `Drop` already uses, and for the same reason — a wedged blocking call
//!   must never be something a mount waits on.
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
//! and `vike_run::build_node_with_preflight` now ACTS on it: a venue with a hard per-venue FAIL is
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

use vike_bridge_core::credentials::{load_credentials_from, Environment};
use vike_bridge_core::{NetProbe, NetProbeThread};
use vike_exec::recon::ReconClient;

use crate::preflight::{
    credential_budget_for, preflight_skipped, run_preflight_gated, ClockPolicy, CredentialGap,
    FnProbes, PreflightConfig, PreflightReport, ServerTimeGap,
};

/// The venue → reconcile-client map the credential leg reads through. Aliased so the `Mutex`
/// wrapper below stays under `clippy::type_complexity` (a `-D warnings` gate).
type ReconClients = HashMap<String, Box<dyn ReconClient>>;

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

/// The `(venue, symbol)` rows the credential leg can authed-read: exactly the venues
/// [`crate::build_recon_client`] builds a client for from plain `Credentials` (the crypto-CEX
/// trio). Every other reconciled venue builds its `ReconClient` INLINE in its `make_engine` arm
/// over a bespoke handshake (deribit's authed order-WS, hyperliquid/aster's bespoke signers,
/// ctrader's protobuf socket, …) which this pre-mount, network-free construction step cannot
/// produce — so those venues are simply not listed, and preflight never demotes a venue it did not
/// check. The symbols mirror `vike_run::WIRED_MARKETS`; they only scope the recon client's own
/// order/position reads, while the balance read this module issues is account-wide.
const AUTHED_READ_MARKETS: &[(&str, &str)] =
    &[("binance", "BTCUSDT"), ("bybit", "BTCUSDT"), ("okx", "BTC-USDT-SWAP")];

/// The venues whose credential probe is built by their OWN bridge factory rather than by
/// [`crate::build_recon_client`] — the same split `make_engine` already has (those venues build
/// their `ReconClient` inline over a bespoke handshake).
///
/// Symbols mirror `vike_run::WIRED_MARKETS`; they scope the recon client's order/position reads,
/// while the balance read this module issues is account-wide.
///
/// ⚠ **No MEASURED healthy reading accompanies either row, and that is a statement, not an
/// omission.** The clock leg's [`crate::preflight`] table carries `(skew, rtt)` pairs measured
/// against real hosts; this leg has no numeric reading to pin, and in any case neither venue could
/// be measured healthy from the box that added them: every Alpaca host is TCP-unreachable from the
/// Windows dev box (measured — the alpaca+ctrader live rehearsal (PR #1407), Evidence 4: DNS
/// resolves, TCP 443 times out on BOTH tiers, while binance and github answer 200), and the
/// cTrader demo grant available there was expired at the venue (`CH_ACCESS_TOKEN_INVALID`). So
/// these rows are proven by their FAILURE paths and by the offline tests below; a PASS from either
/// remains unwitnessed here and wants a run from a box with the egress and a live grant.
const INLINE_AUTHED_READ_MARKETS: &[(&str, &str)] = &[("alpaca", "AAPL"), ("ctrader", "EURUSD")];

/// The venue clock leg — `crate::server_time`'s roster-gated dispatch, re-exported here because
/// this is where the preflight's probes are assembled. Returns an ABSOLUTE epoch-ms stamp (which is
/// what [`crate::preflight::PreflightProbes::venue_server_time_ms`] wants — not the OFFSET
/// `BinanceSpotRest::server_time_offset` returns), or one of the two DISTINCT gaps.
///
/// It is a one-line delegation on purpose: the per-venue knowledge (which endpoint, which tier,
/// what a drift costs there, and for the unwired venues WHY) belongs in one table with a roster
/// completeness test, not in a match in the wiring module — which is exactly where it used to live,
/// as a two-arm match whose catch-all told an operator nothing.
pub fn venue_server_time_ms(
    venue: &str,
    vars: &HashMap<String, String>,
) -> Result<i64, ServerTimeGap> {
    crate::server_time::venue_server_time_ms(venue, vars)
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
fn clock_venues(
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> Vec<String> {
    vike_model::VENUES
        .iter()
        .filter(|venue| crate::would_mount_live_under_policy(venue, vars, policy))
        .map(|venue| (*venue).to_string())
        .collect()
}

/// The per-venue thresholds + remediation text for a MEASURED skew, from each venue's own declared
/// `ClockRisk`. Only wired venues have one (an unmeasurable venue has nothing to judge), and only
/// the venues that REJECT orders over drift carry a FAIL threshold — see
/// `crate::server_time::ClockRisk::policy`.
fn clock_policies(venues: &[String]) -> HashMap<String, ClockPolicy> {
    venues
        .iter()
        .filter_map(|venue| crate::server_time::clock_policy(venue).map(|p| (venue.clone(), p)))
        .collect()
}

/// Build the reconcile clients the credential leg reads through — one per [`AUTHED_READ_MARKETS`]
/// row whose credentials RESOLVE, using the same tier switch `make_engine` uses (`{VENUE}_MAINNET=1`
/// ⇒ the LIVE key set, else DEMO). Construction is PURE: `build_recon_client` only assembles a
/// signer + transport, so nothing here touches the network — the one authed round-trip happens when
/// the leg calls `fetch_balance`.
///
/// Absent credentials ARE the live gate, so a venue with no keys yields no client, is therefore not
/// listed in [`PreflightConfig::venues`], and is never checked (preflight only ever DEMOTES a venue
/// it checked — an unchecked one stays `Live`).
///
/// ⚠ **The arming ceiling is the SECOND gate, and it is the account-scoped one.** This leg issues a
/// SIGNED `fetch_balance` against the operator's real account, so a venue this deployment capped to
/// `paper` must never reach it — the whole meaning of `paper` is "do not touch this account". The
/// gate is [`crate::would_mount_live_under_policy`], the same predicate `make_engine` arms on, so a
/// venue is probed if and only if it is about to be mounted live.
///
/// ⚠ The ceiling also decides the TIER, through the same [`crate::ceiling_permits_live`] fold
/// `make_engine` applies. `cex_mainnet_enabled` alone was the whole answer here, so a `demo`-capped
/// binance with `BINANCE_MAINNET=1` and both key sets present was sent a signed **MAINNET** balance
/// read while the mount bound the demo host — an authenticated read against exactly the real-money
/// account the ceiling exists to fence off.
/// Whether one CEX venue's credential probe signs against the REAL-MONEY tier — the
/// `{VENUE}_MAINNET` switch as a CONJUNCT with the arming ceiling, spelled exactly as
/// `make_engine_with_legs` spells it (`cex_mainnet_enabled(venue, vars) && ceiling_permits_live`).
///
/// Named rather than open-coded so the property has something to assert against: this probe SIGNS,
/// so getting the tier wrong is not a cosmetic divergence from the mount — it is an authenticated
/// read against a live account under a `demo` ceiling.
fn probe_mainnet(
    venue: &str,
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> bool {
    crate::cex_mainnet_enabled(venue, vars)
        && crate::ceiling_permits_live(crate::venue_ceiling(policy, venue))
}

pub fn authed_read_clients(
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> ReconClients {
    let mut out = ReconClients::new();
    for &(venue, symbol) in AUTHED_READ_MARKETS {
        if !crate::would_mount_live_under_policy(venue, vars, policy) {
            continue;
        }
        let mainnet = probe_mainnet(venue, vars, policy);
        let env = if mainnet { Environment::Live } else { Environment::Demo };
        let Some(creds) = load_credentials_from(venue, env, vars) else { continue };
        // The trailing ct_val is the OKX contracts→base scale, unused by the balance read and by
        // every non-okx venue; the shared fallback keeps this step network-free (the real value
        // comes from `make_engine`'s own instrument pre-fetch).
        let Some(client) = crate::build_recon_client(
            venue,
            symbol,
            &creds,
            crate::fallback::OKX_FALLBACK_CTVAL,
            mainnet,
        ) else {
            continue;
        };
        out.insert(venue.to_string(), client);
    }
    out
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
type CredentialProbe = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// The venue → probe map the credential leg reads through, and the authority for
/// [`PreflightConfig::credential_venues`].
type CredentialProbes = HashMap<String, CredentialProbe>;

/// Build one credential probe per venue whose credentials RESOLVE — the eager CEX trio and alpaca,
/// plus lazily-constructed ctrader.
///
/// Absent credentials ARE the live gate, so a venue with no keys yields no probe, is therefore not
/// listed in [`PreflightConfig::credential_venues`], and is never checked (preflight only ever
/// DEMOTES a venue it checked — an unchecked one stays `Live`).
///
/// ⚠ **So is the arming ceiling**, on every one of the three branches below — see
/// [`authed_read_clients`] for why an account-scoped probe is the one leg that may not be
/// over-inclusive "for free". A venue capped to `paper` yields no probe, and ctrader's branch is
/// the one where that matters most: its probe OPENS AND AUTHENTICATES a protobuf socket at the
/// venue, so an un-gated capped ctrader would log a live session in at the venue during a mount
/// that is about to hand it a paper client.
pub fn authed_read_probes(
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> CredentialProbes {
    let mut out = CredentialProbes::new();

    // 1. The eager, `build_recon_client`-shaped venues. Construction is pure; only `fetch_balance`
    //    touches the network. (The ceiling gate lives inside `authed_read_clients`, with the tier.)
    for (venue, client) in authed_read_clients(vars, policy) {
        let client = Mutex::new(client);
        out.insert(
            venue,
            Arc::new(move || {
                let guard =
                    client.lock().map_err(|_| "preflight probe lock poisoned".to_string())?;
                guard.fetch_balance().map(|_| ())
            }) as CredentialProbe,
        );
    }

    // 2. alpaca — its own factory, also PURE to construct (a fresh `TokenSource`/`AlpacaRest`; the
    //    OAuth2 client-credentials mint happens on the first request). The error names BOTH hosts
    //    because they are different machines and either can be the one that is unreachable: the
    //    mint goes to `authx`, the balance read to `broker`.
    //    ⚠ The ceiling is the OUTER conjunct, spelled `then().flatten()` rather than a nested `if`
    //    so the credential loader is not even called for a disarmed venue.
    let alpaca_cfg = crate::would_mount_live_under_policy("alpaca", vars, policy)
        .then(|| vike_alpaca::load_alpaca_config_from(Environment::Demo, vars))
        .flatten();
    if let Some(cfg) = alpaca_cfg {
        let symbol = symbol_for("alpaca");
        if let Some(client) = vike_alpaca::recon_client(&cfg, symbol) {
            let authx = cfg.hosts.authx.to_string();
            let broker = cfg.hosts.broker.to_string();
            let client = Mutex::new(client);
            out.insert(
                "alpaca".to_string(),
                Arc::new(move || {
                    let guard =
                        client.lock().map_err(|_| "preflight probe lock poisoned".to_string())?;
                    guard.fetch_balance().map(|_| ()).map_err(|e| {
                        format!(
                            "{e} (OAuth2 token mint host {authx}, balance read host {broker}; \
                             credentials \
                             ALPACA_SANDBOX_CLIENT_ID/_CLIENT_SECRET/_ACCOUNT_ID)"
                        )
                    })
                }) as CredentialProbe,
            );
        }
    }

    // 3. ctrader — LAZY, and the laziness is the point. `vike_ctrader::recon_client` opens and
    //    AUTHENTICATES a protobuf socket during construction (the venue has no cheaper authed
    //    read), so building it here would turn a refused grant into `None` → no map entry → NO ROW,
    //    which is the silent absence this whole leg exists to remove. Deferring into the closure
    //    makes a refused handshake the leg's FAIL instead.
    let ctrader_cfg = crate::would_mount_live_under_policy("ctrader", vars, policy)
        .then(|| vike_ctrader::config::CtraderConfig::from_vars(Environment::Demo, vars))
        .flatten();
    if let Some(cfg) = ctrader_cfg {
        let symbol = symbol_for("ctrader").to_string();
        let endpoint = format!("{}:{}", cfg.host, cfg.port);
        out.insert(
            "ctrader".to_string(),
            Arc::new(move || {
                match vike_ctrader::recon_client(&cfg, &symbol) {
                    Some(client) => client.fetch_balance().map(|_| ()).map_err(|e| {
                        format!("{e} (authenticated protobuf socket {endpoint})")
                    }),
                    // The handshake is where cTrader checks the grant, so this IS the credential
                    // verdict — an expired `CTRADER_DEMO_ACCESS_TOKEN` lands here.
                    None => Err(format!(
                        "connect/auth refused at {endpoint} — check reachability and the                          CTRADER_DEMO_ACCESS_TOKEN/_REFRESH_TOKEN grant (re-issue it with                          `{}`)",
                        vike_ctrader::token_store::REAUTHORIZE_CMD
                    )),
                }
            }) as CredentialProbe,
        );
    }

    out
}

/// The symbol a venue's credential probe scopes its recon client to, from the one table that
/// declares it. Panics only on a programming error (a venue asked for that no table names), which
/// `every_inline_authed_read_market_is_tabled` makes unreachable.
fn symbol_for(venue: &str) -> &'static str {
    AUTHED_READ_MARKETS
        .iter()
        .chain(INLINE_AUTHED_READ_MARKETS.iter())
        .find(|(v, _)| *v == venue)
        .map(|(_, sym)| *sym)
        .unwrap_or("")
}

/// Run ONE credential probe with a hard wall-clock bound, on a thread the caller can walk away
/// from. THE bound the credential leg never had — and the seam that makes a FAIL mean something.
///
/// `Ok(Ok(()))` = accepted; `Ok(Err(e))` = the venue ANSWERED and refused; `Err(waited)` = nothing
/// came back inside `timeout`.
///
/// ⚠ The probe is not cancelled, because a blocking `ureq` call cannot be: it is ABANDONED. The
/// spawned thread runs to its own agent's completion, finds the result channel dropped, and exits —
/// the same "signal stop, never join" shape [`NetProbeThread`]'s `Drop` uses, and for the same
/// reason. Nothing downstream can observe the abandoned thread: it owns its own `ReconClient` handle
/// through the shared [`CredentialProbe`], writes to nothing else, and the preflight is over long
/// before a venue mounts.
fn bounded_probe(probe: &CredentialProbe, timeout: Duration) -> Result<Result<(), String>, u64> {
    let (tx, rx) = std::sync::mpsc::channel();
    let probe = Arc::clone(probe);
    // A spawn failure must not be reported as a venue refusing us: it is a fault on THIS box.
    // Falling back to the blocking call would reintroduce the unbounded wait, so it is reported as
    // the un-answer it is.
    if std::thread::Builder::new()
        .name("vike-preflight-cred".to_string())
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
            match bounded_probe(&probe, CREDENTIAL_PROBE_TIMEOUT) {
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
/// ⚠ This is deliberately NOT a new "force paper" flag threaded through twelve `make_engine` arms.
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
fn any_venue_would_mount_live(
    vars: &HashMap<String, String>,
    policy: Option<&crate::MountPolicy>,
) -> bool {
    vike_model::VENUES.iter().any(|venue| crate::would_mount_live_under_policy(venue, vars, policy))
}

/// Run the startup preflight with the REAL probes. `vars` is the workspace `.env` credentials map
/// (the same one `make_engine` gates on); the SKIP flag is read from the real process env, never
/// from `vars`, because it is a shell-exported operator toggle like `VIKE_RECONCILE` (the
/// `.env`-is-not-exported gotcha).
///
/// Never panics, never blocks a mount, and returns a report the caller logs. See the module doc for
/// which legs are armed and what this costs.
///
/// ⚠ `dirs` is a PARAMETER rather than something this module resolves, and that is the whole reason
/// the disk leg can be honest. The paths a mount actually writes to are decided ABOVE here — the
/// journal directory comes from a `RunProfile`'s `[sinks.journal]` or `VIKE_JOURNAL_DIR` and lands
/// in `CoreConfig::journal` — so a copy of that resolution in this module would be a second
/// authority that could disagree with the first, and would measure a directory nothing writes to.
/// `vike_run::build_node` passes the dir it actually built the journal with. An empty slice means
/// no disk leg at all, which is what a paper/CI mount wants and what keeps the offline property.
///
/// ⚠ `policy` is this deployment's per-venue ARMING CEILING, and it is the parameter that makes the
/// preflight obey the operator. **`None` reads all-`paper`**, exactly as [`crate::make_engine`]'s
/// own ceiling seam does and through the same [`crate::venue_ceiling`] — so a caller that threads
/// no policy contacts NOTHING, and the widening mistake has to be typed rather than reached by
/// omission. `vike_run::build_node` passes `Some(&cfg.policy)`, which is the one production call
/// site.
pub fn run_startup_preflight(
    vars: &HashMap<String, String>,
    dirs: &[(String, PathBuf)],
    policy: Option<&crate::MountPolicy>,
) -> PreflightReport {
    // The skip flag is a `VIKE_*` operator toggle, so it must come from the REAL process env —
    // reading it from the credentials map would make a shell-exported `=1` invisible.
    let process_env: HashMap<String, String> = std::env::vars().collect();
    if preflight_skipped(&process_env) {
        // Short-circuit BEFORE any probe is built or any thread is spawned: `run_preflight_gated`
        // returns the empty skipped report without touching either argument.
        return run_preflight_gated(
            &process_env,
            &PreflightConfig::default(),
            &FnProbes::new(),
            None,
        );
    }

    let probes_by_venue = authed_read_probes(vars, policy);
    // The CREDENTIAL leg's list IS the authed-readable set (see the module doc's INVARIANT)…
    let mut credential_venues: Vec<String> = probes_by_venue.keys().cloned().collect();
    credential_venues.sort();
    // …and the CLOCK leg's is every venue about to mount live, which is a different question.
    let clock_venues = clock_venues(vars, policy);
    let clock_policies = clock_policies(&clock_venues);
    // The budget is DERIVED from how many of those venues are actually READ — a venue with no clock
    // leg costs nothing, so only the wired ones buy time. Deriving it here rather than taking
    // `PreflightConfig::default()`'s floor is what stops it rotting as the roster grows: the fixed
    // 5000 ms was sized against a six-read measurement and, by the time ten venues mounted, one
    // unreachable venue was enough to leave eight of them unread.
    let wired = clock_venues
        .iter()
        .filter(|v| {
            matches!(
                crate::server_time::clock_source(v),
                Some(crate::server_time::ClockSource::Wired { .. })
            )
        })
        .count();
    let probes = FnProbes::new()
        .with_venue_server_time_ms({
            let vars = vars.clone();
            move |venue: &str| venue_server_time_ms(venue, &vars)
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

    let net =
        any_venue_would_mount_live(vars, policy).then(|| spawn_net_probe(DEFAULT_NET_PROBE_WAIT));
    let handle = net.as_ref().map(NetProbeThread::handle);
    let report = run_preflight_gated(&process_env, &cfg, &probes, handle.as_ref());
    // Signals stop without joining — see `NetProbeThread`'s `Drop` doc.
    drop(net);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::preflight::{CheckStatus, CHECK_CLOCK_SKEW, CHECK_CREDENTIALS, CHECK_NETWORK};
    use vike_config::VenueMode;

    /// **The widest arming ceiling — every roster venue at `live`.**
    ///
    /// ⚠ Nearly every test below needs this, and passing `None` instead would make most of them
    /// VACUOUS rather than red: `None` reads all-`paper`, so a test asserting "absent credentials
    /// mean no probe" would pass on a box where the credentials are present and the CEILING is
    /// what suppressed them. The credential gate and the ceiling gate produce the same empty map,
    /// so a scenario that leaves the ceiling closed cannot tell the two apart — and a test that
    /// cannot tell them apart is not testing the one it names. Every test that is about
    /// CREDENTIALS therefore holds the ceiling wide open, and the ceiling gets its own tests below.
    fn all_live() -> crate::MountPolicy {
        all_venues_at(VenueMode::Live)
    }

    /// Every roster venue DECLARED at one ceiling — the knob the tests below sweep.
    fn all_venues_at(mode: VenueMode) -> crate::MountPolicy {
        let mut venues = vike_config::VenuePolicy::default();
        for venue in vike_model::VENUES {
            venues = venues.declare(venue, mode);
        }
        crate::MountPolicy { venues, ..crate::MountPolicy::default() }
    }

    /// The widest ceiling with ONE venue capped to `paper` — the operator saying "not this one".
    fn all_live_except(paper: &str) -> crate::MountPolicy {
        let mut policy = all_live();
        policy.venues = policy.venues.declare(paper, VenueMode::Paper);
        policy
    }

    fn vars_of(kv: &[(&str, &str)]) -> HashMap<String, String> {
        kv.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// A credential map that arms a real spread of the roster through each arm's OWN loader — the
    /// same set `a_withheld_venue_would_no_longer_mount_live` drives over, for the same reason: a
    /// ceiling test whose scenario arms one venue proves almost nothing.
    fn credentialled() -> HashMap<String, String> {
        vars_of(&[
            ("BINANCE_DEMO_API_KEY", "k"),
            ("BINANCE_DEMO_API_SECRET", "s"),
            ("BYBIT_DEMO_API_KEY", "k"),
            ("BYBIT_DEMO_API_SECRET", "s"),
            ("OKX_DEMO_API_KEY", "k"),
            ("OKX_DEMO_API_SECRET", "s"),
            ("OKX_DEMO_API_PASSPHRASE", "p"),
            ("DERIBIT_DEMO_API_KEY", "k"),
            ("DERIBIT_DEMO_API_SECRET", "s"),
            ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
            ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
            ("ALPACA_SANDBOX_ACCOUNT_ID", "acct-1"),
            ("CTRADER_CLIENT_ID", "app"),
            ("CTRADER_CLIENT_SECRET", "app-secret"),
            ("CTRADER_DEMO_ACCESS_TOKEN", "AT"),
            ("CTRADER_DEMO_REFRESH_TOKEN", "RT"),
            ("IG_DEMO_API_KEY", "k"),
            ("IG_DEMO_IDENTIFIER", "id"),
            ("IG_DEMO_PASSWORD", "pw"),
        ])
    }

    /// Every row's venue is on the canonical roster, and its symbol is non-empty — the same
    /// tie-in `vike_run::WIRED_MARKETS`' own test uses, so a typo here cannot silently check
    /// nothing.
    #[test]
    fn authed_read_markets_are_on_the_canonical_roster() {
        for &(venue, symbol) in AUTHED_READ_MARKETS {
            assert!(
                vike_model::VENUES.contains(&venue),
                "AUTHED_READ_MARKETS names {venue}, which is not in vike_model::VENUES"
            );
            assert!(!symbol.is_empty(), "{venue} must name the symbol its recon client scopes to");
        }
    }

    /// Every listed venue must actually be one `build_recon_client` can build for — otherwise the
    /// credential leg would FAIL a venue purely because this table over-claims.
    #[test]
    fn every_authed_read_market_yields_a_recon_client_when_credentialed() {
        let creds = vike_bridge_core::Credentials {
            api_key: "test-key".to_string(),
            api_secret: "test-secret".to_string(),
            passphrase: Some("test-pass".to_string()),
        };
        for &(venue, symbol) in AUTHED_READ_MARKETS {
            assert!(
                crate::build_recon_client(
                    venue,
                    symbol,
                    &creds,
                    crate::fallback::OKX_FALLBACK_CTVAL,
                    // Demo tier: this asserts the table/factory agree, and construction is pure —
                    // the tier only picks a host, which no assertion here reads.
                    false,
                )
                .is_some(),
                "{venue} is listed for the credential leg but builds no ReconClient"
            );
        }
    }

    /// Absent credentials ⇒ no client, hence no checked venue — the live gate, unchanged. Pure:
    /// client construction never touches the network.
    #[test]
    fn no_credentials_means_no_authed_read_clients() {
        assert!(authed_read_clients(&HashMap::new(), Some(&all_live())).is_empty());
    }

    /// A venue with NO clock leg reports the DECLARED gap (③) carrying its reason — not a fake
    /// time, and not the "did not answer" gap that means something is wrong. Every venue exercised
    /// here is a `NotWired` row, so this touches no network. (The wired venues are deliberately not
    /// exercised: those are real network reads.)
    #[test]
    fn a_declared_clock_venue_reports_its_reason_rather_than_a_fake_time() {
        let vars = HashMap::new();
        for venue in ["ctrader", "oanda", "alpaca"] {
            match venue_server_time_ms(venue, &vars) {
                Err(ServerTimeGap::NotChecked(reason)) => {
                    assert!(!reason.is_empty(), "{venue} declares no reason");
                }
                other => panic!("{venue} must report a DECLARED gap, got {other:?}"),
            }
        }
    }

    /// An off-roster string is the OTHER gap: it says nothing about a venue, so it must not read as
    /// a declaration.
    #[test]
    fn an_unknown_venue_is_not_a_declaration() {
        let vars = HashMap::new();
        match venue_server_time_ms("not-a-venue", &vars) {
            Err(ServerTimeGap::Unreachable(e)) => assert!(e.contains("not-a-venue"), "{e}"),
            other => panic!("an unknown venue must not read as declared: {other:?}"),
        }
    }

    /// THE decoupling, at the wiring site: the clock list is derived from live INTENT over the
    /// canonical roster, so it is not confined to the venues `build_recon_client` can build for —
    /// which is what kept every non-CEX venue's clock unmeasured no matter what was wired.
    #[test]
    fn the_clock_venue_list_is_not_confined_to_the_authed_read_markets() {
        // A pure, network-free live-intent map for a venue that has NO authed-read client here.
        let vars: HashMap<String, String> =
            [("IG_DEMO_API_KEY", "k"), ("IG_DEMO_IDENTIFIER", "id"), ("IG_DEMO_PASSWORD", "pw")]
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
        assert!(clock_venues(&vars, Some(&all_live())).contains(&"ig".to_string()));
        assert!(
            !AUTHED_READ_MARKETS.iter().any(|(v, _)| *v == "ig"),
            "precondition: ig has no authed-read client, so the old shared list excluded it"
        );
        assert!(authed_read_clients(&vars, Some(&all_live())).is_empty(), "…and still does");
        // …and every listed venue carries a policy iff its clock is actually wired.
        let policies = clock_policies(&clock_venues(&vars, Some(&all_live())));
        assert_eq!(policies.get("ig").copied(), crate::server_time::clock_policy("ig"));
        assert_eq!(
            policies["ig"].fail_ms, None,
            "ig's auth stamps no timestamp, so its clock leg may never degrade it to paper"
        );
    }

    /// No credentials ⇒ no live intent ⇒ no clock legs, so the offline property survives the
    /// decoupling.
    #[test]
    fn no_credentials_means_no_clock_venues() {
        assert!(clock_venues(&HashMap::new(), Some(&all_live())).is_empty());
    }

    /// The credential leg over an EMPTY probe map errors rather than silently passing — the
    /// property that makes deriving the venue list from the map load-bearing.
    #[test]
    fn the_authed_read_probe_errors_for_an_unbuilt_venue() {
        let e =
            authed_read_probe(CredentialProbes::new())("binance").expect_err("no client was built");
        // …and it is the CONFIRMED half: an absent probe is a defect in our own wiring, and must
        // not be able to read as the "we never heard back" gap, which never demotes a venue.
        match e {
            CredentialGap::Rejected(msg) => assert!(msg.contains("binance"), "{msg}"),
            other => panic!("an unbuilt venue must be Rejected, not {other:?}"),
        }
    }

    /// Every inline row names a canonical-roster venue and a non-empty symbol, and `symbol_for`
    /// resolves it — the tie-in that stops a typo from silently scoping a probe to `""`.
    #[test]
    fn every_inline_authed_read_market_is_tabled() {
        for &(venue, symbol) in INLINE_AUTHED_READ_MARKETS {
            assert!(
                vike_model::VENUES.contains(&venue),
                "INLINE_AUTHED_READ_MARKETS names {venue}, which is not in vike_model::VENUES"
            );
            assert!(!symbol.is_empty(), "{venue} must name the symbol its recon client scopes to");
            assert_eq!(symbol_for(venue), symbol, "symbol_for must resolve {venue}");
        }
        // …and the two tables are disjoint: a venue built BOTH ways would insert twice and the
        // second write would silently win.
        for &(venue, _) in INLINE_AUTHED_READ_MARKETS {
            assert!(
                !AUTHED_READ_MARKETS.iter().any(|(v, _)| *v == venue),
                "{venue} is in both probe tables"
            );
        }
    }

    /// THE Finding-B fix, offline: credentialed alpaca and ctrader each get a credential probe, so
    /// each gets a preflight ROW. Before this, neither venue was in `credential_venues` at all —
    /// the mount announced `exec="LIVE" network="SANDBOX"` while every Alpaca host was unreachable
    /// and nothing had asked whether the keys worked
    /// (the alpaca+ctrader live rehearsal (PR #1407), Finding B).
    ///
    /// Network-free BY CONSTRUCTION, which is also the claim: building the probes must touch
    /// nothing. alpaca's client is a pure `TokenSource`/`AlpacaRest` assembly and ctrader's is
    /// deferred into its closure, so this test runs offline with bogus credentials.
    #[test]
    fn alpaca_and_ctrader_get_a_credential_probe_when_credentialed() {
        let vars: HashMap<String, String> = [
            ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
            ("ALPACA_SANDBOX_CLIENT_SECRET", "csec"),
            ("ALPACA_SANDBOX_ACCOUNT_ID", "acct-1"),
            ("CTRADER_CLIENT_ID", "app"),
            ("CTRADER_CLIENT_SECRET", "app-secret"),
            ("CTRADER_DEMO_ACCESS_TOKEN", "AT"),
            ("CTRADER_DEMO_REFRESH_TOKEN", "RT"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();

        let probes = authed_read_probes(&vars, Some(&all_live()));
        assert!(probes.contains_key("alpaca"), "alpaca must get a credential row");
        assert!(probes.contains_key("ctrader"), "ctrader must get a credential row");
        // …and neither is reachable through `build_recon_client`, which is why they needed their
        // own construction rather than a row in AUTHED_READ_MARKETS.
        assert!(!AUTHED_READ_MARKETS.iter().any(|(v, _)| *v == "alpaca" || *v == "ctrader"));
    }

    /// ⚠ The laziness that makes ctrader's row exist AT ALL. Its `ReconClient` authenticates during
    /// construction, so an eagerly-built probe would return `None` for a REFUSED grant — and a
    /// venue absent from the probe map gets NO ROW, turning an expired token back into silence.
    /// Here the grant is nonsense and the host is never dialled, yet the row is present.
    #[test]
    fn a_ctrader_grant_that_could_not_connect_still_yields_a_row() {
        let vars: HashMap<String, String> = [
            ("CTRADER_CLIENT_ID", "app"),
            ("CTRADER_CLIENT_SECRET", "app-secret"),
            ("CTRADER_DEMO_ACCESS_TOKEN", "definitely-expired"),
            ("CTRADER_DEMO_REFRESH_TOKEN", "also-expired"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();

        let probes = authed_read_probes(&vars, Some(&all_live()));
        assert!(
            probes.contains_key("ctrader"),
            "a refused grant must still be CHECKED — an absent row is the silent failure this leg \
             exists to remove"
        );
    }

    /// THE BOUND, and the thing that was missing for the credential leg's whole life: a probe that
    /// never answers is ABANDONED at [`CREDENTIAL_PROBE_TIMEOUT`], not waited out.
    ///
    /// The fake probe parks for far longer than the bound — the shape of the ~20 s geo-blocked
    /// alpaca TCP connect measured on the dev box — and the assertion is on WALL CLOCK: the call
    /// must return in about the bound, not in about the probe's own duration.
    #[test]
    fn an_unresponsive_credential_probe_is_abandoned_at_the_bound() {
        let probe: CredentialProbe = Arc::new(|| {
            std::thread::sleep(CREDENTIAL_PROBE_TIMEOUT * 6);
            Ok(())
        });
        let t0 = Instant::now();
        let outcome = bounded_probe(&probe, CREDENTIAL_PROBE_TIMEOUT);
        let waited = t0.elapsed();
        assert!(outcome.is_err(), "a probe that did not answer must not report a verdict");
        assert!(
            waited < CREDENTIAL_PROBE_TIMEOUT * 3,
            "the mount waited {waited:?}, i.e. it waited the PROBE out rather than its own bound"
        );
    }

    /// …and the other half: a probe that answers inside the bound is not disturbed by it.
    #[test]
    fn a_prompt_credential_probe_answers_through_the_bound() {
        let ok: CredentialProbe = Arc::new(|| Ok(()));
        assert_eq!(bounded_probe(&ok, CREDENTIAL_PROBE_TIMEOUT), Ok(Ok(())));
        let refused: CredentialProbe = Arc::new(|| Err("401 invalid api key".to_string()));
        assert_eq!(
            bounded_probe(&refused, CREDENTIAL_PROBE_TIMEOUT),
            Ok(Err("401 invalid api key".to_string()))
        );
    }

    /// THE confirmed-vs-transient split, at the leg: the two `Err` shapes are what decide whether a
    /// venue is demoted, so they must never be reachable from each other's cause.
    ///
    /// A venue that ANSWERS and refuses is `Rejected` — and it is only reported after the RETRY, so
    /// a demotion needs the venue to refuse twice.
    #[test]
    fn an_answering_venue_is_rejected_and_is_retried_before_it_is_believed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let mut probes = CredentialProbes::new();
        probes.insert(
            "okx".to_string(),
            Arc::new(move || {
                seen.fetch_add(1, Ordering::Relaxed);
                Err("401 invalid api key".to_string())
            }) as CredentialProbe,
        );
        match authed_read_probe(probes)("okx") {
            Err(CredentialGap::Rejected(msg)) => assert!(msg.contains("401"), "{msg}"),
            other => panic!("an answering venue must be Rejected, got {other:?}"),
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            CREDENTIAL_PROBE_ATTEMPTS,
            "a demotion must be CONFIRMED — one refusal is a blip, not a verdict"
        );
    }

    /// …and the venue that never answers is `Unanswered`, which never demotes. ⚠ It must ALSO not
    /// be retried: re-waiting the bound would double the leg's worst case to learn the same nothing
    /// twice, which is the arithmetic the whole bound exists to cap.
    #[test]
    fn a_silent_venue_is_unanswered_and_is_not_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let mut probes = CredentialProbes::new();
        probes.insert(
            "alpaca".to_string(),
            Arc::new(move || {
                seen.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(CREDENTIAL_PROBE_TIMEOUT * 6);
                Ok(())
            }) as CredentialProbe,
        );
        match authed_read_probe(probes)("alpaca") {
            Err(CredentialGap::Unanswered { waited_ms, detail }) => {
                assert!(waited_ms > 0, "the row must state its own bound");
                assert!(!detail.is_empty());
            }
            other => panic!("a silent venue must be Unanswered, got {other:?}"),
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1, "a timeout is never retried");
    }

    /// THE ENFORCEMENT MECHANISM, closed at the only layer that can see it: withholding a venue's
    /// credentials makes `would_mount_live` FALSE for it — which is what actually turns
    /// `make_engine` onto the paper fallback. `vike-run` asserts the map transformation; only this
    /// crate can assert what the map transformation MEANS.
    ///
    /// Driven over every canonical-roster venue that a plausible credential set can arm, so a venue
    /// whose key spelling escapes the `{VENUE}_` prefix rule would redden this rather than mounting
    /// live after being demoted.
    #[test]
    fn a_withheld_venue_would_no_longer_mount_live() {
        // Every live-arm gate this build compiles, spelled as its own loader wants it.
        let vars = credentialled();

        let armed: Vec<&str> = vike_model::VENUES
            .iter()
            .copied()
            .filter(|v| crate::would_mount_live(v, &vars))
            .collect();
        assert!(
            armed.len() >= 5,
            "precondition: this map must arm a real set of venues, armed = {armed:?}"
        );

        for venue in &armed {
            let mut demoted = vars.clone();
            let withheld = withhold_venue_credentials(&mut demoted, venue);
            assert!(withheld > 0, "{venue} was armed, so it must have had keys to withhold");
            assert!(
                !crate::would_mount_live(venue, &demoted),
                "{venue} still reads as live-intent after its credentials were withheld — the \
                 preflight's demotion would be announced and then not happen"
            );
            // …and ONLY that venue moved: preflight demotes one venue, never a neighbour.
            for other in armed.iter().filter(|o| *o != venue) {
                assert!(
                    crate::would_mount_live(other, &demoted),
                    "withholding {venue} also demoted {other}"
                );
            }
        }
    }

    /// THE DISK LEG, armed: the probe returns a real number for a real directory, and an ERROR
    /// (which the leg renders as a WARN, never a fake PASS) for one that is not there.
    ///
    /// ⚠ On Windows both halves take the declared-gap path, which is the honest answer there rather
    /// than a skipped test — so this asserts the CONTRACT (`Ok` is plausible, `Err` names a reason)
    /// on both platforms and the measurement only where it can be taken. What it cannot see is the
    /// `f_bavail`-vs-`f_bfree` choice: on an unreserved filesystem the two are equal, so that
    /// remains a documented judgement (see [`free_space_bytes`]) rather than a pinned one.
    #[test]
    fn the_disk_probe_measures_a_real_directory_and_declares_a_missing_one() {
        let here = std::env::temp_dir();
        let measured = free_space_bytes(&here);
        // ⚠ The two arms are `#[cfg]`-selected rather than branched on `cfg!(…)`: a runtime `if`
        // over a compile-time constant makes clippy's `assertions_on_constants` fire under the
        // `-D warnings` gate, and it would also let the WRONG arm compile-check into nothing.
        #[cfg(unix)]
        {
            let free = measured.expect("unix must MEASURE, not declare");
            assert!(free > 0, "a writable temp dir reporting ZERO free bytes is not a reading");
            // A directory that does not exist is an ERROR, so `check_disk_headroom` WARNs naming
            // it — never a silent absence, and never a PASS.
            let missing = here.join("vike-preflight-no-such-dir-8f3a1c");
            assert!(free_space_bytes(&missing).is_err(), "an absent directory cannot be measured");
        }
        #[cfg(not(unix))]
        {
            let reason = measured.expect_err("this platform cannot measure, so it must DECLARE");
            assert!(!reason.is_empty(), "a declared gap must carry its reason");
        }
    }

    /// The watched set is deduplicated by PATH, not by label: a journal written inside the store
    /// root is ONE filesystem, and two rows for it would double every finding without adding a fact.
    #[test]
    fn the_disk_dirs_are_deduplicated_by_path() {
        let p = PathBuf::from("/srv/vike/data");
        let out = disk_dirs(&[
            ("journal".to_string(), p.clone()),
            ("hist-store".to_string(), p.clone()),
            ("other".to_string(), PathBuf::from("/srv/vike/other")),
        ]);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].0, "journal", "the FIRST label wins, so the order is deterministic");
    }

    /// Absent credentials ⇒ no probe for either venue, so the offline/paper property is intact.
    #[test]
    fn no_credentials_means_no_credential_probes() {
        assert!(authed_read_probes(&HashMap::new(), Some(&all_live())).is_empty());
    }

    /// Partial credentials are the live gate, not a half-armed probe: alpaca needs all three of
    /// client id/secret/account, ctrader needs both halves of the grant.
    #[test]
    fn partial_credentials_yield_no_probe() {
        let alpaca_partial: HashMap<String, String> =
            [("ALPACA_SANDBOX_CLIENT_ID", "cid"), ("ALPACA_SANDBOX_CLIENT_SECRET", "csec")]
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
        assert!(!authed_read_probes(&alpaca_partial, Some(&all_live())).contains_key("alpaca"));

        let ctrader_partial: HashMap<String, String> = [
            ("CTRADER_CLIENT_ID", "app"),
            ("CTRADER_CLIENT_SECRET", "sec"),
            ("CTRADER_DEMO_ACCESS_TOKEN", "AT"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
        assert!(!authed_read_probes(&ctrader_partial, Some(&all_live())).contains_key("ctrader"));
    }

    /// THE default path: an empty credentials map runs the preflight with ZERO network — no venue
    /// is checked (no creds ⇒ no authed-read client) and no `NetProbe` is spawned (no venue would
    /// mount live). With nothing live, the single network row is DECLARED not-applicable rather
    /// than WARNing a developer's TODO at an operator on every paper start — and it still never
    /// claims a measurement it did not take. This is the shape `vike_run::build_node` sees on
    /// every paper/CI mount.
    #[test]
    fn an_empty_credentials_map_preflights_offline() {
        // ⚠ The WIDEST ceiling, deliberately: the claim is that ABSENT CREDENTIALS keep this
        // offline, and `None` (all-`paper`) would keep it offline for the other reason — the same
        // empty report from a different cause, which is not what this test's name says.
        let report = run_startup_preflight(&HashMap::new(), &[], Some(&all_live()));
        // A developer box could have the skip flag exported; then the report is empty by design.
        if report.skipped {
            assert!(report.checks.is_empty());
            return;
        }
        assert_eq!(
            report.checks.len(),
            1,
            "only the global network leg runs: {:?}",
            report.lines()
        );
        assert_eq!(report.checks[0].name, CHECK_NETWORK);
        assert_eq!(report.checks[0].status, CheckStatus::NotApplicable);
        assert_ne!(report.checks[0].status, CheckStatus::Pass, "never a fake PASS");
        assert!(report.go(), "nothing here is a no-go");
        assert!(report.degraded_venues().is_empty(), "nothing was checked, so nothing degrades");
        assert!(!report.checks.iter().any(|c| c.name == CHECK_CLOCK_SKEW));
        assert!(!report.checks.iter().any(|c| c.name == CHECK_CREDENTIALS));
    }

    // ─── THE ARMING CEILING reaches the preflight ──────────────────────────────────────────────
    //
    // Every test in this block drives the SAME credential map (`credentialled()`, which arms a real
    // spread of the roster) and varies ONLY the ceiling, so a green result can be attributed to the
    // ceiling and to nothing else. Offline by construction: every predicate under test is one of
    // the pure, network-free live-intent probes, and no probe closure is ever CALLED.

    /// **THE DEFECT, as a test.** A venue with valid credentials that the operator capped to
    /// `paper` is not contacted by ANY leg — it is absent from the clock list and from the
    /// credential probe map.
    ///
    /// The measured incident: a box whose `policy.toml` said `ig = "paper"` still had the preflight
    /// reach out to IG at every start, and IG's clock row is the one CREDENTIALED read in
    /// `crate::server_time`'s table (`X-IG-API-KEY`). `paper` means "do not touch this account".
    ///
    /// ⚠ The precondition is the half that makes this non-vacuous: under the WIDEST ceiling the
    /// very same map DOES contact ig, so the assertions below are about the ceiling rather than
    /// about a map that never armed anything.
    #[test]
    fn a_paper_capped_venue_with_credentials_is_never_contacted() {
        let vars = credentialled();

        let wide = all_live();
        assert!(
            clock_venues(&vars, Some(&wide)).contains(&"ig".to_string()),
            "precondition: these credentials DO arm ig, so capping it is what changes the answer"
        );

        let capped = all_live_except("ig");
        assert!(
            !clock_venues(&vars, Some(&capped)).contains(&"ig".to_string()),
            "ig is capped to `paper` and the preflight still reads its clock — with its API key"
        );
        assert!(
            !authed_read_probes(&vars, Some(&capped)).contains_key("ig"),
            "a capped venue must not get a credential probe either"
        );
    }

    /// …and the same for a venue whose probe SIGNS a balance read, which is the account-scoped leg.
    /// binance is capped; bybit and okx, credentialled identically and left armed, must be
    /// untouched — a ceiling that disarmed a neighbour would be its own defect.
    #[test]
    fn capping_one_venue_leaves_its_neighbours_checked() {
        let vars = credentialled();
        let capped = all_live_except("binance");

        let probes = authed_read_probes(&vars, Some(&capped));
        assert!(
            !probes.contains_key("binance"),
            "binance is `paper`: nothing may be signed for it"
        );
        assert!(probes.contains_key("bybit"), "bybit was left armed and must still be checked");
        assert!(probes.contains_key("okx"), "okx was left armed and must still be checked");

        let clocks = clock_venues(&vars, Some(&capped));
        assert!(!clocks.contains(&"binance".to_string()));
        assert!(clocks.contains(&"bybit".to_string()));
        assert!(clocks.contains(&"okx".to_string()));
    }

    /// **An armed venue is checked EXACTLY as before.** The `live` ceiling reproduces the sets the
    /// uncapped predicate produced, venue for venue — so this lane narrows what a disarmed
    /// deployment contacts and changes nothing for an armed one.
    #[test]
    fn an_armed_venue_is_checked_exactly_as_before() {
        let vars = credentialled();
        let wide = all_live();

        let before: Vec<String> = vike_model::VENUES
            .iter()
            .filter(|v| crate::would_mount_live(v, &vars))
            .map(|v| (*v).to_string())
            .collect();
        assert_eq!(clock_venues(&vars, Some(&wide)), before, "the widest ceiling must be a no-op");

        let mut probed: Vec<String> = authed_read_probes(&vars, Some(&wide)).into_keys().collect();
        probed.sort();
        assert!(
            probed.iter().any(|v| v == "binance") && probed.iter().any(|v| v == "ctrader"),
            "precondition: both probe SHAPES are exercised, eager and lazy — {probed:?}"
        );
        assert!(any_venue_would_mount_live(&vars, Some(&wide)), "…and the net probe still arms");
    }

    /// **THE PROPERTY, asserted rather than assumed: the ceiling can only ever NARROW what is
    /// contacted.** For every ceiling — including the widest — each derived set is a SUBSET of the
    /// uncapped answer, and every element of it is a venue the mount would itself arm at that
    /// ceiling.
    ///
    /// A subset check is the honest shape here. "Fewer venues" would pass for a fix that dropped
    /// the wrong venue, and "equal under `live`" alone would say nothing about `demo`.
    #[test]
    fn the_ceiling_can_only_narrow_what_is_contacted() {
        let vars = credentialled();
        let uncapped: Vec<&str> = vike_model::VENUES
            .iter()
            .copied()
            .filter(|v| crate::would_mount_live(v, &vars))
            .collect();
        assert!(uncapped.len() >= 5, "precondition: a real set must be armed, {uncapped:?}");

        for mode in [VenueMode::Paper, VenueMode::Demo, VenueMode::Live] {
            let policy = all_venues_at(mode);

            for venue in clock_venues(&vars, Some(&policy)) {
                assert!(
                    uncapped.contains(&venue.as_str()),
                    "{mode:?}: the clock leg reached {venue}, which the uncapped answer excluded"
                );
                assert!(
                    crate::would_mount_live_under_policy(&venue, &vars, Some(&policy)),
                    "{mode:?}: {venue} is checked but this mount would not arm it"
                );
            }
            for venue in authed_read_probes(&vars, Some(&policy)).keys() {
                assert!(
                    uncapped.contains(&venue.as_str()),
                    "{mode:?}: a credential probe was built for {venue}, which is not even armed \
                     at the widest ceiling"
                );
                assert!(
                    crate::would_mount_live_under_policy(venue, &vars, Some(&policy)),
                    "{mode:?}: {venue} would be signed for but this mount would not arm it"
                );
            }
            if mode == VenueMode::Paper {
                assert!(!any_venue_would_mount_live(&vars, Some(&policy)));
            }
        }
    }

    /// **The offline property, STRENGTHENED rather than merely preserved:** no credentials means no
    /// network call under EVERY ceiling — the widest included, which is the one that could have
    /// regressed. (The narrow ceilings hold it for a second, independent reason, and that
    /// redundancy is the point of the lane.)
    #[test]
    fn no_credentials_means_no_network_call_under_every_ceiling() {
        let empty = HashMap::new();
        for mode in [VenueMode::Paper, VenueMode::Demo, VenueMode::Live] {
            let policy = all_venues_at(mode);
            assert!(clock_venues(&empty, Some(&policy)).is_empty(), "{mode:?}");
            assert!(authed_read_probes(&empty, Some(&policy)).is_empty(), "{mode:?}");
            assert!(authed_read_clients(&empty, Some(&policy)).is_empty(), "{mode:?}");
            assert!(!any_venue_would_mount_live(&empty, Some(&policy)), "{mode:?}");
        }
    }

    /// **An ALL-PAPER box preflights cleanly** — a clean no-op, not a failure, and not a silent
    /// skip. Full credential store, no policy at all (`None` ⇒ every venue `paper`, which is the
    /// fresh-box default `MountPolicy::default()` carries): the report is exactly the one global
    /// network row, NOT-APPLICABLE, `go()` is true and nothing is degraded.
    ///
    /// Offline by construction — not one leg has a venue, so nothing is dialled, which is also why
    /// this can be a unit test at all.
    ///
    /// ⚠ The skip is NOT silent: the mount's own `crate::report_capped_to_paper` WARNs per venue
    /// whose credentials would have armed it, and `crate::venue_arming_migration` says it once with
    /// a paste-ready fix. This test asserts the preflight's half — that being told nothing was
    /// checked is never dressed up as a PASS.
    #[test]
    fn an_all_paper_box_preflights_cleanly() {
        let report = run_startup_preflight(&credentialled(), &[], None);
        if report.skipped {
            assert!(report.checks.is_empty(), "a skipped preflight runs no check at all");
            return;
        }
        assert_eq!(
            report.checks.len(),
            1,
            "an all-paper box checks no venue at all: {:?}",
            report.lines()
        );
        assert_eq!(report.checks[0].name, CHECK_NETWORK);
        assert_eq!(report.checks[0].status, CheckStatus::NotApplicable);
        assert_ne!(report.checks[0].status, CheckStatus::Pass, "never a fake PASS");
        assert!(report.go(), "an all-paper box is not a no-go");
        assert!(report.degraded_venues().is_empty(), "nothing was checked, so nothing degrades");
        assert!(!report.checks.iter().any(|c| c.name == CHECK_CLOCK_SKEW));
        assert!(!report.checks.iter().any(|c| c.name == CHECK_CREDENTIALS));
    }

    /// **The TIER half of the ceiling, at the one leg that SIGNS.** `probe_mainnet` is
    /// `make_engine`'s own `cex_mainnet_enabled && ceiling_permits_live` conjunct; before it, this
    /// module read the `{VENUE}_MAINNET` flag alone, so a `demo`-capped binance was sent a signed
    /// **MAINNET** balance read while the mount bound the demo host.
    ///
    /// Asserted against `crate::cex_mainnet_enabled` rather than against a literal `true`, so the
    /// precondition and the claim cannot drift apart.
    #[test]
    fn a_demo_ceiling_never_signs_against_the_real_money_tier() {
        let vars = vars_of(&[
            ("BINANCE_MAINNET", "1"),
            ("BINANCE_LIVE_API_KEY", "lk"),
            ("BINANCE_LIVE_API_SECRET", "ls"),
            ("BINANCE_DEMO_API_KEY", "dk"),
            ("BINANCE_DEMO_API_SECRET", "ds"),
        ]);
        assert!(
            crate::cex_mainnet_enabled("binance", &vars),
            "precondition: the flag is armed, so only the ceiling can refuse the live tier"
        );

        assert!(probe_mainnet("binance", &vars, Some(&all_live())), "a `live` ceiling permits it");

        let demo = all_venues_at(VenueMode::Demo);
        assert!(
            !probe_mainnet("binance", &vars, Some(&demo)),
            "a `demo` ceiling with the mainnet flag armed still signed against the LIVE account"
        );
        assert!(
            !probe_mainnet("binance", &vars, None),
            "and no policy at all is the safe end, not the wide one"
        );
        // …and the venue is still CHECKED at the demo ceiling — narrowing the tier must not
        // silently drop the row, which would trade one blind spot for another.
        assert!(authed_read_probes(&vars, Some(&demo)).contains_key("binance"));
    }
}
