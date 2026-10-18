//! The **mount-site wiring** of [`crate::preflight`]: the one place the pure go/no-go gate is
//! handed REAL probes. `vike_tradehub_core::assemble_runtime` calls [`run_startup_preflight`] once, at the top of
//! the runtime assembly, before any venue is mounted.
//!
//! ⚠ **Wave 2, W9a: this module is the ORCHESTRATOR.** It walks the legs and owns the disk leg;
//! every blocking read that needs a bridge — the clock reads over each row's declared clock, the
//! credential reads over each row's `credential_probe`, the network probe, and their bounds — is the
//! venue half's (`crates/vike-gatewayhub/src/trade/adapters/startup.rs`) and reaches this module
//! through [`crate::VenueBackend`]'s `preflight_plan`, `server_time_ms`, `authed_read` and
//! `net_reading`. The prose below describes the legs as the pair of modules runs them.
//!
//! `preflight` performs no I/O: every observation arrives through its four-method
//! [`PreflightProbes`](crate::preflight::PreflightProbes) seam, and an unwired leg returns
//! [`NO_PROBE`](crate::preflight::NO_PROBE). The legs:
//!
//! - **network — ARMED.** A real `NetProbe` is spawned, waited on for at most
//!   `DEFAULT_NET_PROBE_WAIT`, read, then dropped (stop without join,
//!   `vike_bridge_core::NetProbeThread`'s `Drop`). Spawned ONLY when a venue would go live
//!   AT ITS ACCOUNT'S TIER ([`crate::venue_would_go_live`] over `vike_model::VENUES`): a
//!   pure-paper mount — no credentials, or every account `paper` — stays offline and the leg WARNs
//!   "no NetProbe wired".
//! - **clock skew — ARMED from the roster declaration table** (`crates/vike-gatewayhub/src/trade/adapters/server_time.rs`). Every
//!   venue that WOULD MOUNT LIVE AT ITS ACCOUNT'S TIER gets a row: a MEASUREMENT where an endpoint
//!   is
//!   wired (against the demo/mainnet host that venue's arm binds), a "publishes one, did not
//!   answer" WARN where the read fails, and a DECLARED row with its reason where there is no clock
//!   leg — NOT-APPLICABLE where drift costs nothing, a WARN where auth signs the clock into the
//!   order path (④; polymarket). None of the last three is ever a FAIL, so our inability to measure
//!   never degrades a venue, and a MEASURED skew FAILs only a venue that rejects orders over drift
//!   (`crate::server_time::clock_policy_of`).
//! - **credentials — ARMED, through each venue's own `ReconClient`.** `authed_read_probes`
//!   builds one PROBE (a cheap `fetch_balance`) per venue whose credentials resolve. ⚠ INVARIANT:
//!   [`PreflightConfig::credential_venues`] is DERIVED from that probe map, never hand-written — an
//!   unwired credential leg FAILs by design, so listing a venue we cannot authed-read would
//!   manufacture a FAIL.
//!
//!   Two probe SHAPES, and the difference is load-bearing:
//!
//!   * **Eagerly-built** (binance/bybit/okx via their bridges' `VenueAdapter::credential_probe`
//!     (`RecordsIdentity`); alpaca via `crates/bridges/alpaca/src/recon_client.rs`'s
//!     `recon_client`): construction is PURE (signer + transport), so the closure just calls
//!     `fetch_balance`.
//!   * **Lazily-built** (ctrader, `crates/bridges/ctrader/src/adapter.rs`'s `CtraderVenueAdapter`):
//!     `crates/bridges/ctrader/src/recon_client.rs`'s `recon_client` CONNECTS while constructing.
//!     Built eagerly, a refused grant returns `None` and the venue gets NO ROW — an expired token
//!     would read as silence, not a FAIL — so construction is deferred into the closure.
//!
//!   A contract row offers either shape via `VenueAdapter::credential_probe` (`RecordsIdentity` /
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
//! Every venue set below is derived through [`crate::venue_would_go_live`] — the SAME
//! predicate `make_engine` arms on, via the same `vike_config::account_arming`'s `account_tier` — so preflight
//! and mount cannot disagree about what is armed. `policy: None` reads all-`paper` and contacts
//! nothing. Gated on credential PRESENCE alone (`crate::would_go_live`), a box whose ig account
//! was `paper` still had IG contacted every start — a CREDENTIALED clock read
//! (`crates/bridges/ig/src/adapter.rs`'s `IgVenueAdapter::server_time_ms` sends `X-IG-API-KEY`) beside
//! a SIGNED `fetch_balance` — on an account the operator had said not to touch.
//!
//! ## Account-scoped versus box-scoped, because the legs genuinely differ
//!
//! - **credentials — ACCOUNT-SCOPED** (a signed read; at ctrader an authenticated socket). Gated.
//! - **disk — BOX-SCOPED** (`statvfs`). Ungated: a full disk kills the journal WAL on an all-paper
//!   box too.
//! - **network — BOX-SCOPED** (it resolves `one.one.one.one` and `dns.google`), gated anyway:
//!   [`any_venue_would_go_live`] gives the reasons.
//! - **clock skew — a box-scoped QUANTITY**, but the CONTACT is with the venue, and the VERDICT
//!   (`crate::server_time::clock_policy_of`) and CONSEQUENCE (a stopped venue) are per-venue: a
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
//! `VenueAdapter::server_time_ms` (docs/decisions/0096), handed the account's tier
//! [`run_startup_preflight`] reads per venue, as `AdapterInputs::tier`.
//!
//! ⚠ **The two venue lists are SEPARATE, and merging them was the second half of the defect.** One
//! `PreflightConfig::venues` from `AUTHED_READ_MARKETS` (the credential leg's CEX table, gone with
//! docs/decisions/0096) fed both legs, so the clock ran for the CEX trio only and wiring deribit or
//! hyperliquid changed nothing. Nor can they unify the other way: a credential entry FAILs
//! (degrading the venue), a clock entry only WARNs. So the clock list is LIVE INTENT
//! ([`crate::venue_would_go_live`]) and the credential list is client-buildability, with
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
//! `DEFAULT_NET_PROBE_WAIT` — warm, tens of milliseconds. A clock read repeats (at most
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
//! clock read at the top of `vike_tradehub_core::assemble_runtime`; the budget is checked only BETWEEN reads.
//!
//! So a WIRED clock read goes through the SAME `bounded_probe` — `CLOCK_PROBE_TIMEOUT` per
//! read — and an abandoned read is outcome ② (a WARN). ⚠ NOT a fan-out or a per-venue slice
//! (`docs/decisions/0027-clock-budget-derived-from-the-roster.md`); a DECLARED venue is never
//! probed (`bounded_server_time_ms`). `bounded_probe` declares the leaked-thread residual.
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
//! - `CREDENTIAL_PROBE_TIMEOUT` per attempt, via `bounded_probe`, which ABANDONS an
//!   unresponsive venue's thread (`NetProbeThread`'s `Drop` shape).
//! - `CREDENTIAL_PROBE_ATTEMPTS`: ONE retry, only after an ANSWERED error, never a timeout.
//! - [`crate::preflight::credential_budget_for`]: the leg total, derived per credentialed venue.
//!
//! Worst case = `MAX_CREDENTIAL_BUDGET_MS + CREDENTIAL_PROBE_ATTEMPTS × CREDENTIAL_PROBE_TIMEOUT`,
//! fixed as the roster grows. The `flags.preflight_skip` row removes every leg.
//!
//! # The report is ENFORCED — and the bound above is what earns that
//!
//! `vike_tradehub_core::assemble_runtime_with_preflight` ACTS on
//! [`crate::preflight::PreflightReport::venue_disposition`]: a hard per-venue FAIL mounts that
//! venue PAPER for the session.
//!
//! ⚠ It was advisory while ANY authed-read error — a transient timeout too — raised a FAIL. The
//! bound is the confirmed-vs-transient distinction: `bounded_probe` yields
//! [`CredentialGap::Unanswered`] for a silence (WARN, never demotes) and
//! [`CredentialGap::Rejected`] only for an answer (demotes). [`crate::preflight`]'s policy section
//! has the full argument and the one accepted residual.
//!
//! # The skip
//!
//! The `flags.preflight_skip` row, folded into the caller's map as `VIKE_PREFLIGHT_SKIP` = the
//! EXACT string `"1"` (see [`crate::preflight::preflight_skipped`]), returns the empty skipped report
//! WITHOUT building a probe, spawning a thread or resolving a credential.

#[cfg(doc)]
use crate::preflight::{CredentialGap, PreflightConfig};

mod run;

pub use run::{
    any_venue_would_go_live, free_space_bytes, run_startup_preflight, withhold_venue_credentials,
};

#[cfg(doc)]
use run::disk_dirs;

#[cfg(test)]
use run::{disk_dirs, disk_floors};

#[path = "startup_tests.rs"]
#[cfg(test)]
mod startup_tests;
