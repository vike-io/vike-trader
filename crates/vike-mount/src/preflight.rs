//! Startup **preflight** — the pure go/no-go gate that runs ONCE at mount, before the first live
//! order, turning four failure modes that were previously discovered by a rejected order (or by
//! silent data loss) into a structured report an operator reads at t=0.
//!
//! # Where this is wired
//!
//! [`crate::startup::run_startup_preflight`] is the ONE production caller: it builds the real
//! probes (the venue server-time read, each reconcile client's cheap authed read, and a spawned
//! [`vike_bridge_core::NetProbe`]) and `vike_run::build_node` runs it at the top of the twelve-venue
//! mount, logging the report. This module itself stays PURE — it lives here, in the mount
//! composition root, rather than in `vike-app-core` (its original home, where nothing constructed
//! it) so the GUI and the headless daemon get the same gate through the same `build_node` without
//! either dragging an egui-shaped crate into its graph.
//!
//! # Why this exists
//!
//! Every check here COMPOSES machinery that already existed in the workspace but that nothing ran
//! at startup before that wiring:
//!
//! - **clock skew** — `BinanceSpotRest::server_time_offset` / `AsterSpotRest::server_time_offset`
//!   exist, but every caller outside binance's own exec spawn was an `#[ignore]`d smoke. A bad host
//!   clock was otherwise discovered as a binance `-1021 INVALID_TIMESTAMP` (or a bybit `10002`) on
//!   the FIRST signed order. WHICH venues this leg can measure — and, for each of the rest, WHY
//!   not — is [`crate::server_time`]'s roster-gated declaration table, never a fall-through here.
//! - **credential validity** — every reconciled venue already builds a `ReconClient` that can do a
//!   cheap authed read. A dead key was otherwise discovered at the first `submit`.
//! - **disk headroom** — nothing checked it anywhere, and this leg was UNWIRED for a year because
//!   free space is not reachable from `std`. It is wired now: [`crate::startup::free_space_bytes`]
//!   reads it through a SAFE `rustix::fs::statvfs` on unix (no `unsafe`, and no new package — the
//!   crate was already resolved in `Cargo.lock`) and reports a DECLARED gap on Windows, where the
//!   equivalent call has no dependency-free safe route. A full disk silently kills the live
//!   `RecorderSink` writer and the journal WAL, and the CI box was measured at ~70% of one filesystem
//!   consumed while this leg was dark.
//! - **network** — [`vike_bridge_core::NetProbe`] exists (opt-in DNS liveness) but is off by
//!   default, so a dead resolver surfaced only as every venue reconnect failing at the hostname
//!   step.
//!
//! On mainnet day the first real order must not be the probe.
//!
//! # Pure core, injected probes
//!
//! Nothing in this module performs I/O. Every observation arrives through [`PreflightProbes`] — a
//! four-method, object-safe trait — so the whole decision surface is unit-testable with no network,
//! no venue and no filesystem. [`FnProbes`] is the closure-backed implementation both the tests and
//! a real mount site would use; its unwired legs return [`NO_PROBE`], which never reads as a silent
//! PASS (the clock and disk legs WARN — "could not measure" is not evidence of a fault — while the
//! credential leg FAILs, because "we could not prove these keys work" must not mount a venue live).
//! ⚠ An unwired credential leg is [`CredentialGap::Rejected`] deliberately: `NO_PROBE` is a defect
//! in OUR wiring, and the answer to "the check you asked for does not exist" must be the strict one.
//! The network leg deliberately takes a [`NetProbeHandle`] rather than probing
//! itself: reading it is a single relaxed atomic load and that probe's state machine is already
//! tested in `vike-bridge-core` — this module must not grow a second probe.
//!
//! # The clock leg has FOUR outcomes, not two
//!
//! "This adapter has no endpoint wired" and "the venue did not answer" are OPPOSITE facts: the
//! first is a permanent property of our code, the second means something is wrong right now. They
//! used to arrive here as one `Err(String)` and left as one identical WARN — the same
//! silent-degrade class as a misconfigured proxy reporting success. [`ServerTimeGap`] splits them,
//! and [`check_clock_skew`] renders four distinct rows:
//!
//! | outcome | status | when |
//! |---|---|---|
//! | ① measured | `Pass` / `Warn` / `Fail` | a number, against that venue's thresholds below |
//! | ② unreachable | [`CheckStatus::Warn`] | the venue publishes a clock and the read FAILED |
//! | ③ declared, nothing at stake | [`CheckStatus::NotApplicable`] | [`crate::server_time`] declares no leg, with a reason, at a venue a drift cannot cost |
//! | ④ declared, ORDERS at stake | [`CheckStatus::Warn`] | no leg, at a venue whose auth signs the clock into the order path |
//!
//! ③ is deliberately NOT a warning. A row that fires on every healthy mount is noise, and noise is
//! what buried the original defect. ② stays a WARN rather than a FAIL for the reason argued below
//! (a preflight must not ground a daemon over one slow public endpoint) — what makes it loud is
//! that its message and remedy now say a venue that publishes a clock did not answer, instead of
//! being indistinguishable from a venue that publishes none.
//!
//! ④ exists because ③'s "nothing to see here" was being printed over the roster's ONE genuinely
//! order-affecting gap. Polymarket signs `POLY_TIMESTAMP` into every authenticated CLOB request
//! (`crates/bridges/polymarket/src/auth.rs`'s `l2_auth_headers`) and this preflight measures no
//! clock for it, so a drifted host clock is an unmeasured hazard there — not a non-issue. The split
//! is a field on the declaration row, so a venue cannot acquire the quiet status by accident.
//!
//! # The clock-skew thresholds are PER VENUE, because one global number was measurably false
//!
//! `vike_bridge_core::signer`'s `BinanceHmacSigner` and `BybitV5Signer` BOTH hard-code
//! `recv_window: 5000` (ms). A venue rejects a signed request whose stamped `timestamp` falls
//! outside that window relative to ITS clock — binance with `-1021` (`INVALID_TIMESTAMP`, mapped in
//! `vike_binance::error_codes`), bybit with `10002`. The stamped timestamp is our LOCAL clock plus
//! an optional `Signer::set_offset_ms` correction that only binance's exec spawn path applies
//! today, and only for the clients it owns — so elsewhere the raw local clock is what goes on the
//! wire. Those 5000 ms are therefore a budget shared between local clock skew, one-way network
//! latency, and venue-side queueing. For a venue that works that way:
//!
//! - [`DEFAULT_CLOCK_FAIL_MS`] = `2500` — **half** the budget. Past this point one ordinary latency
//!   spike is enough to push a request outside the window, so orders start failing intermittently
//!   and unreproducibly. That is a no-go for the venue, not a warning.
//! - [`DEFAULT_CLOCK_WARN_MS`] = `500` — **10%** of the budget. Orders still go through, but the
//!   host clock is visibly not being disciplined, and that is exactly the drift that becomes a
//!   `-1021` an hour later.
//!
//! ⚠ **NOT every venue works that way, and applying those two numbers to the ones that do not was
//! wrong in both directions.** `crate::server_time`'s `ClockRisk::policy` is the authority; the two
//! rules and their evidence:
//!
//! 1. **Only a venue that REJECTS an order over drift may FAIL** (and so degrade itself to paper
//!    via [`PreflightReport::venue_disposition`]). Deribit's `client_credentials` auth and ig's
//!    session-token pair stamp no timestamp at all, and hyperliquid binds the clock into a nonce
//!    valid for about a DAY — demoting any of them to paper over a 2.5 s reading would punish a
//!    venue for a fault its auth cannot suffer. Their policy carries `fail_ms: None`.
//! 2. **The canary venues warn later**, at [`CANARY_CLOCK_WARN_MS`] = `1000`, because what a
//!    reading at them measures is dominated by THEIR clock rather than ours.
//!
//! **The MEASUREMENT that forced rule 2** (the CI box, an NTP-disciplined box — `timedatectl` reports
//! "System clock synchronized: yes"; 2026-08-09; every reading midpoint-corrected exactly as
//! [`check_clock_skew`] corrects, and reproducible with
//! `crates/vike-mount/tests/server_time_smoke.rs`). The four recv-window venues agree with this
//! host to within **tens of ms** — bybit +9..23 (both hosts), okx +10..37, aster +15..33, binance's
//! demo host +236..247 over a ~730 ms round trip — which is what pins the host clock itself as good
//! to that order. Hyperliquid does not: **40 samples of its testnet node inside three minutes
//! ranged from -220 ms to -424 ms**, with the round trip pinned at ~285 ms throughout, and its
//! mainnet node read -59..-221 in the same window. That is the venue's clock wandering, not ours.
//!
//! ⚠ **This is the correction to a derivation that was already false when it shipped.** The
//! previous pin was `WORST_HEALTHY_SKEW_MS = 288` with a const-assert that the warn threshold clear
//! it by half again (500 > 432). The very next run of the branch's own measurement produced -424,
//! which fails that assert (500 > 636 is false) and sits **76 ms from warning on a healthy box**. A
//! threshold that warns on a healthy host is the false-alarm twin of the silent degrade this whole
//! lane exists to fix, so the fix is the per-venue split above and the measurement is pinned as
//! `(skew, rtt)` PAIRS in [`MEASURED_HEALTHY_READINGS`] — replayed through the real check, not
//! compared against a bare number.
//!
//! # A reading is only as sharp as its round trip: the ±rtt/2 floor
//!
//! The midpoint correction assumes a SYMMETRIC round trip, so its residual error is bounded by
//! ±rtt/2 — and that is a FLOOR set by the network, not a nuisance to be averaged away. A venue 300
//! ms away simply cannot be measured to 50 ms. Path asymmetry inside that band is real and
//! measured: bybit's demo host once read **182 ms of apparent skew on a 549 ms round trip** while
//! every other rep on the same host read 13-23 ms.
//!
//! So the verdict is taken from the band's LOWER bound — `|skew| - rtt/2`, floored at zero — i.e.
//! from what the reading PROVES rather than from what it suggests. Two consequences, both
//! deliberate:
//!
//! - a slow link can no longer manufacture a warning (binance's demo host, +247 ms over a 757 ms
//!   round trip, proves a lower bound of 0);
//! - a slow link resolves less, so a genuine 600 ms drift may only be provable at the venues with a
//!   tight round trip. That is not a hole: the host clock is SHARED, the leg runs per venue, and
//!   the tightest link on the roster is the one that resolves it. `bybit`'s 195 ms round trip
//!   proves what binance's 750 ms one cannot.
//!
//! All of it is [`PreflightConfig`] state, not values baked into the logic — a venue with a tighter
//! window can be tightened without touching this module.
//!
//! # Sampling: the minimum round trip of up to [`DEFAULT_CLOCK_SAMPLES`], and only when it matters
//!
//! Since the band width IS the measurement error, a tighter round trip is a sharper reading, and
//! AVERAGING does not help (an asymmetric outlier is not noise around a true value, it is a bias in
//! one direction). So this module keeps the sample with the SMALLEST round trip, and resamples only
//! while the reading is INCONCLUSIVE — i.e. while the ±rtt/2 band straddles a threshold, so a
//! tighter round trip could still change the verdict.
//!
//! That costs ONE call in every ordinary case: a healthy reading (tens of ms over a few hundred ms
//! of flight) is unambiguously a PASS at both ends of its band and stops immediately, and so does a
//! grossly-drifted clock. The extra calls are spent only on the readings that would otherwise be
//! coin flips.
//!
//! # The clock leg is BOUNDED — the arithmetic, and what bounds it
//!
//! ⚠ Every clock read is a BLOCKING REST call on the mount's own thread, before the first venue is
//! mounted. Left unbounded that is a startup stall, and the arithmetic is not small: this lane took
//! the wired set from one venue to seven, each of which may be read up to
//! [`DEFAULT_CLOCK_SAMPLES`] times, and the shared `vike_bridge_core::http` agent's global timeout
//! is 30 s — **7 × 3 × 30 s = 630 s**, i.e. a mount parked for ten and a half minutes by a preflight
//! whose worst finding is a WARN. (The one-venue version it replaced was bounded by 30 s, and the
//! sentence stating that bound was deleted rather than updated.)
//!
//! Three bounds, all named and all observable in the report:
//!
//! - `crate::server_time`'s `CLOCK_READ_TIMEOUT` (3 s) — the transport's PER-READ ceiling, on a
//!   dedicated bounded agent rather than the 30 s order-path one. It bounds every read ureq CAN
//!   bound — an in-flight request — and, being a request timer, nothing before one: a name
//!   resolution that never returns is invisible to it.
//! - `crate::startup::CLOCK_PROBE_TIMEOUT` — the MOUNT's per-read ceiling, one second above the
//!   transport's, on the case the transport cannot see. The read runs on a thread the mount
//!   abandons at that ceiling (`crate::startup`'s abandon section carries the seam, and its
//!   residual), and an abandoned read is outcome ② — a WARN naming the bound, never a demotion.
//! - [`clock_budget_for`] — a TOTAL budget for the whole leg, spanning every venue and every
//!   resample, DERIVED from how many venues are actually read
//!   ([`PER_VENUE_CLOCK_ALLOWANCE_MS`] each, clamped between [`DEFAULT_CLOCK_BUDGET_MS`] and
//!   [`MAX_CLOCK_BUDGET_MS`]). It is checked before each read against the injected clock, so it
//!   needs no wall-clock of its own and is exact under test. A venue the budget never reached says
//!   so in its own row (a WARN naming the budget) rather than vanishing.
//!
//! ⚠ **The budget is DERIVED because a fixed one rotted.** It was `5000` flat, sized against a
//! SIX-read **1781 ms** measurement from the CI box on 2026-08-09 — comfortable then, and never
//! revisited as venues were added. By 2026-08-22 the wired roster's pinned healthy readings summed
//! to **2973 ms**, so one unreachable venue (a full 3 s) needed **5973 ms** against a 5000 ms
//! budget: it did not fit, and eight venues went unread on the dev box when bybit's endpoint
//! stopped answering — aster among them, which runs against mainnet in practice and whose auth
//! binds the clock into the order path. Nothing was broken; the number was simply smaller than the
//! roster it had to cover. The requirement it is now sized against — **absorb one dead venue and
//! still read the rest** — is arithmetic over this module's own measurements in
//! `the_budget_absorbs_one_dead_venue_and_still_reads_the_rest`, so the roster growing reddens that
//! test rather than silently squeezing the venues at the end of it.
//!
//! Worst case is therefore `budget + one in-flight read`, i.e. at most
//! `MAX_CLOCK_BUDGET_MS + crate::startup::CLOCK_PROBE_TIMEOUT` = **19 s** and today **12.4 s**,
//! against 630 s. ⚠ "One in-flight read" is bounded by the mount's own ABANDON ceiling rather than
//! by `crate::server_time::CLOCK_READ_TIMEOUT`, because that transport timer cannot preempt a
//! wedged name resolution and this sentence used to claim a bound it therefore did not have — see
//! `crate::startup`'s abandon section for the seam that makes it true. ⚠ The
//! CREDENTIAL leg is bounded SEPARATELY — see the next section. It does NOT SPEND this budget:
//! [`run_preflight`] interleaves the
//! two legs per venue, so it measures each credential probe and pushes the clock deadline out by
//! that much. Charging it instead was a real defect — on the Windows dev box 2026-08-22 a
//! geo-blocked alpaca probe sat ~20 s on a TCP connect, and alpaca/aster/hyperliquid each reported
//! their clock "not read" while the clock leg itself had spent 3.2 s of its 5 s. Worse, the row blamed "an earlier
//! venue's clock read", which is a lie: every clock row was fast and healthy.
//!
//! # The CREDENTIAL leg is bounded too — and the bound is what MAKES the FAIL mean something
//!
//! ⚠ Until this lane the credential leg had NO bound at all. Each `fetch_balance` rode its own
//! `ReconClient`'s 30 s agent and ctrader additionally paid a TCP+TLS handshake inside its own
//! closure, so the leg was worth up to `30 s × credentialed venues` — a number that GROWS with the
//! roster and had no ceiling anywhere. The same geo-blocked alpaca probe measured above sat ~20 s
//! on a single TCP connect, at the top of a mount, before one venue was up.
//!
//! Bounding it needed no change to any `ReconClient` and no new knob on that shared trait. The
//! bound is applied where the blocking call is ISSUED, by [`crate::startup`]:
//!
//! - `crate::startup::CREDENTIAL_PROBE_TIMEOUT` — the PER-ATTEMPT ceiling. The probe runs on a
//!   spawned thread and the mount thread `recv_timeout`s it, so an unresponsive venue is ABANDONED
//!   rather than waited out (the abandoned thread finishes on its own agent's timeout and exits).
//! - `crate::startup::CREDENTIAL_PROBE_ATTEMPTS` — one RETRY, spent only on a probe that ANSWERED
//!   with an error. A timeout is never retried: the bound was already spent proving nothing.
//! - [`credential_budget_for`] — a TOTAL budget for the whole leg, derived per credentialed venue
//!   exactly as [`clock_budget_for`] is, checked against the injected clock before each venue.
//!
//! Worst case is `budget + one venue's attempts`, i.e.
//! `MAX_CREDENTIAL_BUDGET_MS + CREDENTIAL_PROBE_ATTEMPTS × CREDENTIAL_PROBE_TIMEOUT` — a fixed
//! ceiling that does not move when a venue joins the roster.
//!
//! ⚠ The timeout is ALSO what makes the enforcement above defensible. It is the seam that separates
//! "the venue refused us" from "we never heard back", and only the first one demotes.
//!
//! # Policy: a venue FAIL degrades that venue to paper — and it is now ENFORCED
//!
//! This mirrors the established live gate — "absent credentials ARE the live gate: loaders return
//! `None` -> stay paper" (root `CLAUDE.md`). A per-venue hard FAIL (bad clock, dead credentials)
//! marks exactly that venue [`VenueDisposition::Paper`] via
//! [`PreflightReport::venue_disposition`]; every other venue is untouched and the app always
//! starts. A GLOBAL hard FAIL (no disk headroom, no internet) is what flips
//! [`PreflightReport::go`] to `false` — those are process-wide conditions no single venue owns.
//! Nothing here panics, logs, or mutates anything: this module only DECIDES, and
//! `vike_run::build_node_with_preflight` is the one site that acts.
//!
//! ⚠ **The disposition used to be computed and thrown away, and that was the defect.** For its
//! whole life this report was ADVISORY: `build_node` logged a `FAIL` row and mounted the venue LIVE
//! anyway, so an operator could read a preflight verdict and believe it had gated something it had
//! not. The stated reason for not enforcing was real — "a per-venue FAIL is raised by ANY
//! authed-read error, a transient timeout included" — and the stated blocker was a
//! confirmed-vs-transient distinction that did not exist. **[`CredentialGap`] is that distinction**,
//! so the blocker is gone and the disposition is enforced:
//!
//! | evidence | status | mount |
//! |---|---|---|
//! | the venue ANSWERED and refused the signed read ([`CredentialGap::Rejected`]) | `Fail` | PAPER |
//! | the probe did not answer inside its bound ([`CredentialGap::Unanswered`]) | `Warn` | live, unchanged |
//! | a PROVEN skew past a venue's own `fail_ms` | `Fail` | PAPER |
//! | anything we merely could not measure (②/③/④, a budget miss, an unqueryable disk) | `Warn`/`N/A` | live, unchanged |
//!
//! The argument that settles it: refused keys and ABSENT keys are the same fact — we do not have
//! working credentials for this venue — and absent keys already demote, silently and without
//! debate. The old behaviour was therefore backwards, mounting LIVE precisely when the venue had
//! just told us our keys do not work while mounting PAPER when they were merely missing.
//!
//! ⚠ **Only a PER-VENUE fail enforces. A global no-go does NOT ground the process**, and that
//! asymmetry is deliberate: a daemon that refuses to start under `Restart=on-failure` crash-loops,
//! which is the failure `crates/vike-recorder/src/recorder_cli.rs`'s `--exit-on-silence`
//! doc already argues against. A global FAIL is logged at `error` and the mount proceeds.
//!
//! ⚠ **The accepted residual, stated rather than hidden:** a venue that answers a non-auth error
//! FAST (a `429`, a `503`) inside the bound reads as Rejected and is demoted. The wiring retries
//! once before believing it ([`crate::startup`]'s `CREDENTIAL_PROBE_ATTEMPTS`), so a single blip
//! cannot do it, and PAPER is the safe side of that error — but it is a demotion on evidence of a
//! broken venue rather than of broken keys. What would reopen it is a probe that can read the
//! venue's own auth-vs-throttle distinction; `VIKE_PREFLIGHT_SKIP=1` is the operator's escape hatch
//! meanwhile, and it removes every leg rather than just this one.
//!
//! ⚠ **An UNMEASURABLE clock never degrades anything, and that is a decision, not an oversight.**
//! Both ② and ③ stay non-FAIL, so neither can demote a venue or flip the go bit. The reasoning is
//! the one `crates/vike-recorder/src/recorder_cli.rs`'s module doc already argues for
//! `--exit-on-silence`: a quiet venue there is legitimately quiet often enough that killing the
//! daemon over it "would be a worse failure than the one being fixed", and under
//! `Restart=on-failure` it would crash-loop. Same shape here — a public server-time endpoint can be
//! briefly slow, rate-limited or behind a hiccuping CDN, and a live trading daemon that refuses to
//! start over that has been taken down by its own preflight. A MEASURED skew past
//! [`PreflightConfig::clock_fail_ms`] is different in kind: it is evidence, and it FAILs (which
//! `vike_run::build_node` still only logs — see `crate::startup`'s "the report is ADVISORY here").
//!
//! # The skip override
//!
//! [`preflight_skipped`] reads [`PREFLIGHT_SKIP_ENV`] and is `true` iff the value is the EXACT
//! string `"1"` — the same deliberately-unfuzzy idiom as `VIKE_RECONCILE` in
//! `vike_ops::reconcile_config` (one unambiguous on-string to grep for in an incident;
//! `"true"` / `"yes"` / `"0"` all stay off). Like every other `VIKE_*` feature toggle it must be
//! sourced from the REAL process env, not the credentials `.env` map — see
//! `vike_ops::reconcile_config`'s module doc for why. Skipping yields an EMPTY report
//! ([`PreflightReport::skipped`] = `true`, no checks,
//! `go() == true`, no degraded venues) WITHOUT calling a single probe, so every venue keeps
//! whatever disposition it would have had with no preflight at all.
//!
//! # Secrets
//!
//! A probe's error string is embedded verbatim in a [`CheckReport::message`], which an operator may
//! print or log. Probe implementations MUST NOT put an API key, secret or passphrase in that
//! string — the report carries the venue slug and the venue's own error text only.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use vike_bridge_core::NetProbeHandle;
use vike_bridge_core::net_probe::wall_clock_ms;

/// The env var that skips preflight entirely. Exact string `"1"` — see the module doc.
pub const PREFLIGHT_SKIP_ENV: &str = "VIKE_PREFLIGHT_SKIP";

/// Absolute clock skew (ms) at or beyond which the clock check WARNs. 10% of the signers'
/// `recv_window: 5000` — see the module doc for the derivation.
pub const DEFAULT_CLOCK_WARN_MS: i64 = 500;

/// Absolute clock skew (ms) at or beyond which the clock check FAILs (and the venue degrades to
/// paper). Half the signers' `recv_window: 5000` — see the module doc for the derivation.
pub const DEFAULT_CLOCK_FAIL_MS: i64 = 2_500;

/// The WARN threshold for a venue whose auth cannot reject an order over clock drift — deribit, ig
/// (no timestamp at all) and hyperliquid (a nonce window measured in days). A full second, chosen
/// because a reading at those venues is dominated by THEIR clock, not ours: hyperliquid's testnet
/// node ranged -220..-424 ms across 40 samples from an NTP-disciplined the CI box inside three minutes
/// (2026-08-09), while the four recv-window venues read 9-37 ms in the same window. 1000 ms is 2.4x
/// that worst venue-side reading before the ±rtt/2 floor is applied and ~2.7x after it, and it is
/// still far below any drift an undisciplined host reaches — those run to seconds and up.
///
/// It is a WARN and nothing more: `crate::server_time`'s `ClockRisk::policy` gives these venues no
/// FAIL threshold, because degrading a venue to paper over a fault its auth cannot suffer would be
/// a fabricated consequence.
pub const CANARY_CLOCK_WARN_MS: i64 = 1_000;

/// Total wall-clock budget (ms) for the WHOLE clock leg — every venue, every resample — checked
/// against the injected clock before each read. See the module doc's "the clock leg is BOUNDED"
/// section: the leg it bounds is worth 630 s of blocking startup stall without it, and a healthy
/// mount measured 1781 ms of this.
/// Floor for the clock leg's total budget — what the leg was always given, kept so a SMALL roster
/// behaves exactly as before this derivation existed.
pub const DEFAULT_CLOCK_BUDGET_MS: i64 = 5_000;

/// What each WIRED venue buys the clock leg. Sized from this module's own pinned measurements: the
/// slowest healthy round trip ever recorded here is binance's **757 ms**, so this covers the
/// slowest real read with ~60% of margin, and the leg's budget is that times the number of venues
/// actually read.
///
/// ⚠ It is a PER-VENUE allowance rather than a per-venue DEADLINE, deliberately. The leg keeps ONE
/// shared pool consumed in roster order, so a venue that resamples may legitimately spend a
/// neighbour's share — which is right, because resampling only happens when a reading is
/// inconclusive against that venue's own thresholds, and a conclusive answer is worth more than an
/// even split. What the sizing must guarantee is that the pool is large enough for the pass to
/// finish anyway; see `the_budget_absorbs_one_dead_venue_and_still_reads_the_rest`.
pub const PER_VENUE_CLOCK_ALLOWANCE_MS: i64 = 1_200;

/// Ceiling for the derived budget. The leg runs before a trading daemon arms, so a roster large
/// enough to make startup unbounded must hit a wall rather than scale forever. Nothing reaches it
/// today; it exists so that growth is capped by a number somebody chose.
pub const MAX_CLOCK_BUDGET_MS: i64 = 15_000;

/// The clock leg's total budget for a roster of `wired` readable venues — DERIVED, never written
/// down, so a venue joining the roster buys the leg more time instead of quietly squeezing the
/// venues behind it.
///
/// The fixed 5000 ms this replaces was sized (module doc) against a SIX-read 1781 ms measurement
/// from the CI box on 2026-08-09 and never revisited as venues were added. By 2026-08-22 a healthy pass
/// spent ~69% of it, so a single unreachable venue — costing a full
/// [`crate::server_time::CLOCK_READ_TIMEOUT`] — pushed the leg past its budget and left eight
/// venues unread, aster among them.
#[must_use]
pub fn clock_budget_for(wired: usize) -> i64 {
    let derived = (wired as i64).saturating_mul(PER_VENUE_CLOCK_ALLOWANCE_MS);
    derived.clamp(DEFAULT_CLOCK_BUDGET_MS, MAX_CLOCK_BUDGET_MS)
}

/// Floor for the CREDENTIAL leg's total budget — what a one- or two-venue mount gets regardless of
/// the derivation, so a small roster is never squeezed by arithmetic sized for a large one.
pub const DEFAULT_CREDENTIAL_BUDGET_MS: i64 = 6_000;

/// What each CREDENTIALED venue buys the credential leg. A signed balance read against a healthy
/// venue is a single REST round trip — tens to low hundreds of ms warm — so 3 s covers a slow-link
/// answer with an order of magnitude of margin while still summing to a bound that a roster can
/// grow into rather than through.
pub const PER_VENUE_CREDENTIAL_ALLOWANCE_MS: i64 = 3_000;

/// Ceiling for the derived credential budget, the twin of [`MAX_CLOCK_BUDGET_MS`]. The leg runs
/// before a trading daemon arms, so roster growth must hit a wall somebody chose rather than scale
/// forever — which is exactly what the UNBOUNDED version of this leg did.
pub const MAX_CREDENTIAL_BUDGET_MS: i64 = 20_000;

/// The credential leg's total budget for `credentialed` venues — DERIVED, never written down, for
/// the same reason [`clock_budget_for`] is: a fixed number sized against today's roster rots into
/// one that silently drops the venues at the end of the list.
#[must_use]
pub fn credential_budget_for(credentialed: usize) -> i64 {
    let derived = (credentialed as i64).saturating_mul(PER_VENUE_CREDENTIAL_ALLOWANCE_MS);
    derived.clamp(DEFAULT_CREDENTIAL_BUDGET_MS, MAX_CREDENTIAL_BUDGET_MS)
}

/// How many times the clock leg may read one venue before concluding. THREE, and almost always
/// spent as one: a sample is taken again only while its ±rtt/2 uncertainty straddles a threshold
/// (see the module doc's sampling section), and the smallest round trip wins. Three is what it
/// takes for a single asymmetric outlier — the shape actually measured, one bad rep in six — to be
/// outvoted by a neighbour rather than believed.
pub const DEFAULT_CLOCK_SAMPLES: usize = 3;

/// Free bytes below which a watched directory WARNs. 5 GiB — comfortably more than a day of live
/// tick/book recording, so the warning arrives with time to act.
pub const DEFAULT_DISK_WARN_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Free bytes below which a watched directory FAILs. 1 GiB — below this the recorder flush and the
/// journal WAL are one busy session away from failing mid-write.
pub const DEFAULT_DISK_FAIL_BYTES: u64 = 1024 * 1024 * 1024;

/// Check name for the per-venue clock-skew leg.
pub const CHECK_CLOCK_SKEW: &str = "clock_skew";
/// Check name for the per-venue credential-validity leg.
pub const CHECK_CREDENTIALS: &str = "credentials";
/// Check name for the per-directory disk-headroom leg.
pub const CHECK_DISK: &str = "disk_headroom";
/// Check name for the single, global network leg.
pub const CHECK_NETWORK: &str = "network";

/// What an unwired [`FnProbes`] leg returns — an honest "not wired". No check ever renders it as a
/// PASS: the clock and disk legs WARN, the credential leg FAILs.
pub const NO_PROBE: &str = "no probe wired";

/// The FALLBACK remedy for a measured out-of-band skew, used only when the caller declared no
/// per-venue text in [`PreflightConfig::clock_policies`]. Deliberately says nothing about recv
/// windows: `crate::server_time`'s `ClockRisk` is what knows whether this venue rejects orders over
/// drift, and a generic line that claimed it would be false on half the roster.
pub(crate) const REMEDY_CLOCK: &str = "sync the host clock (NTP / w32tm) — the host's own time is wrong, whatever the venue does \
     with it";
/// ② — the venue publishes a clock and did not give us one.
const REMEDY_CLOCK_UNREACHABLE: &str = "this venue PUBLISHES a server-time endpoint and it did not answer — check egress and the \
     venue's status page before mounting live; this is not the same as a venue that publishes none";
/// ④ — no leg here, at a venue whose auth binds the clock into the order path. There is nothing to
/// re-run, so the action is to verify the host clock by other means and to know the gap exists.
const REMEDY_CLOCK_UNMEASURED: &str = "verify this host's clock by other means before trading this venue live (`timedatectl` / \
     `w32tm /query /status`) — this row is a DISCLOSED gap in the preflight, not a measurement";
/// The leg's total budget ran out before this venue was reached — see [`DEFAULT_CLOCK_BUDGET_MS`].
const REMEDY_CLOCK_BUDGET: &str = "an earlier venue's clock read consumed the leg's whole budget — check that venue's row, or \
     raise PreflightConfig::clock_budget_ms if every venue here is genuinely this slow";
const REMEDY_CREDENTIALS: &str = "this venue is MOUNTED PAPER for this session — check its credentials in the store \
     `vike-cli secrets path` prints (the common shape is {VENUE}_{SIM|DEMO|LIVE}_API_KEY/\
     _API_SECRET, but several venues use their own; this row's own message names the keys the probe \
     actually looked for) and restart";
/// The probe never came back inside its bound. NOT proof of a bad key, so it never demotes — which
/// is precisely why this remedy has to say the venue is still LIVE.
const REMEDY_CREDENTIALS_UNANSWERED: &str = "this venue is STILL MOUNTED LIVE — a probe that did not answer proves nothing about the keys, \
     so it never demotes; check egress to this venue's host and the venue's status page, because an \
     order will take the same path this probe could not complete";
/// The leg's total budget ran out before this venue was reached — see [`credential_budget_for`].
const REMEDY_CREDENTIALS_BUDGET: &str = "an earlier venue's credential probe consumed the leg's whole budget — check that venue's row; \
     this venue is STILL MOUNTED LIVE, unchecked";
const REMEDY_DISK: &str =
    "free space or repoint the directory — the live recorder and the journal WAL write here";
const REMEDY_DISK_UNKNOWN: &str = "could not query free space; check that the directory exists";
const REMEDY_NETWORK: &str =
    "no configured host resolves — check the local resolver / VPN before mounting live venues";
const REMEDY_NETWORK_UNKNOWN: &str =
    "network liveness unknown; spawn a vike_bridge_core::NetProbe to make it observable";

/// The verdict of one check. Ordered `NotApplicable < Pass < Warn < Fail`, which is what makes
/// [`PreflightReport::worst`] a plain `max()` — and what keeps a declared not-applicable row from
/// ever raising the severity of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckStatus {
    /// DECLARED not-applicable: there is nothing to check here, and a named row says why (③ in the
    /// module doc). A permanent property of the venue or of this code — NOT a fault, NOT a failure
    /// to measure, and never rendered as a warning. It sorts BELOW [`CheckStatus::Pass`] because it
    /// asserts even less: a pass measured something.
    NotApplicable,
    /// Measured, and within limits.
    Pass,
    /// Either measured-but-marginal, or NOT measurable. A probe that could not answer warns: it
    /// never passes, and it never grounds anything on its own.
    Warn,
    /// Measured, and out of limits. On a per-venue check this degrades that venue to paper; on a
    /// global check it flips [`PreflightReport::go`] to `false`.
    Fail,
}

impl CheckStatus {
    /// Uppercase tag used by [`PreflightReport::lines`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::NotApplicable => "N/A",
            CheckStatus::Pass => "PASS",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        }
    }

    /// `true` for [`CheckStatus::Fail`] only.
    #[must_use]
    pub fn is_fail(self) -> bool {
        self == CheckStatus::Fail
    }
}

impl fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One check's structured outcome. `venue` is `Some` for the per-venue legs (clock, credentials)
/// and `None` for the global ones (disk, network) — that split IS the degrade-to-paper policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// Stable machine name: one of [`CHECK_CLOCK_SKEW`] / [`CHECK_CREDENTIALS`] / [`CHECK_DISK`] /
    /// [`CHECK_NETWORK`].
    pub name: String,
    /// The venue this check is scoped to, or `None` for a process-global check.
    pub venue: Option<String>,
    /// The verdict.
    pub status: CheckStatus,
    /// What was observed, in operator language. Never contains a secret (see the module doc).
    pub message: String,
    /// What to DO about it; empty for a passing check.
    pub remediation: String,
}

/// Why a clock leg produced no number — outcomes ② and ③ of the module doc, and the reason this is
/// an enum rather than the `String` it used to be.
///
/// The two are OPPOSITE facts and must never render alike: one is a live problem at a venue that
/// publishes a clock, the other is a permanent, DECLARED property with a reason attached.
/// [`ServerTimeGap::NotChecked`] takes `&'static str` deliberately — a declaration comes from a
/// table row that was written by a human, so it cannot be manufactured out of a runtime failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerTimeGap {
    /// ② The venue PUBLISHES a server clock this workspace reads, and this attempt did not get one
    /// (timeout, DNS, HTTP error, an unparseable body). Carries the venue's own error text — never
    /// a URL and never a credential (see the module doc's secrets note). Also what an UNWIRED probe
    /// leg returns ([`NO_PROBE`]), because "no probe was supplied" is likewise not a declaration
    /// about the venue.
    Unreachable(String),
    /// ③ DECLARED, and nothing is at stake: no clock leg exists for this venue, this is the reason
    /// — the row's own sentence from `crate::server_time`'s `CLOCK_SOURCES` — and a drifted clock
    /// could not cost this venue an order anyway.
    NotChecked(&'static str),
    /// ④ DECLARED, with ORDERS at stake: no clock leg exists here either, but this venue's auth
    /// binds the clock into the order path, so the gap is an unmeasured HAZARD rather than a
    /// non-issue. Rendered [`CheckStatus::Warn`], never NOT-APPLICABLE — the distinction exists
    /// because the polymarket row was printing "nothing to check here" over the roster's one
    /// order-affecting gap.
    UnmeasuredRisk {
        /// Why there is no leg (the row's own sentence).
        reason: &'static str,
        /// What a drifted clock costs AT THIS VENUE, in that venue's own terms.
        at_stake: &'static str,
    },
}

/// Why a credential probe did not succeed — and the whole reason the degrade-to-paper decision is
/// now ENFORCED rather than advisory.
///
/// It is the credential leg's twin of [`ServerTimeGap`], and it exists for the identical reason:
/// "the venue answered and refused us" and "we never heard back" are OPPOSITE facts that used to
/// arrive here as one `String` and leave as one identical FAIL. That collapse is exactly what
/// `vike_run::build_node` cited when it declined to act on [`PreflightReport::venue_disposition`] —
/// "a per-venue FAIL is raised by ANY authed-read error, including a transient timeout". Splitting
/// them is what makes a FAIL mean *the venue told us these keys do not work*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialGap {
    /// The venue ANSWERED, inside the probe's bound, and refused the signed read (or the transport
    /// returned a definite error). CONFIRMED evidence: this is a [`CheckStatus::Fail`], and it
    /// demotes the venue to paper. Carries the venue's own error text — never a key, secret or
    /// passphrase (see the module doc's secrets note).
    ///
    /// This is also what an UNWIRED probe leg returns ([`NO_PROBE`]): a check we asked for and did
    /// not build is a defect in our wiring, and the strict answer is the safe one there.
    Rejected(String),
    /// The probe did not come back inside `crate::startup::CREDENTIAL_PROBE_TIMEOUT`, or the leg's
    /// budget was spent before this venue's turn. NOT evidence about the credentials, so it is a
    /// [`CheckStatus::Warn`] and NEVER demotes — the same rule the clock leg's ②/③/④ already obey.
    Unanswered {
        /// How long the mount thread actually waited, in ms — so the row states its own bound.
        waited_ms: u64,
        /// What the wait was on, in operator language. Never contains a secret.
        detail: String,
    },
}

impl From<String> for CredentialGap {
    /// A bare error string is the CONFIRMED half. Every probe that answers at all answers here, and
    /// only the bounded runner in [`crate::startup`] — which owns the clock the wait is measured on
    /// — can construct [`CredentialGap::Unanswered`].
    fn from(e: String) -> Self {
        CredentialGap::Rejected(e)
    }
}

/// The thresholds and remediation text ONE venue's clock reading is judged against.
///
/// It replaced a bare per-venue remedy string because the words were not the only thing that was
/// venue-specific: so are the numbers, and so is whether a FAIL is even meaningful. See the module
/// doc's "the clock-skew thresholds are PER VENUE" section — `crate::server_time`'s
/// `ClockRisk::policy` is the authority that fills it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockPolicy {
    /// Proven skew (ms) at or beyond which this venue's row WARNs.
    pub warn_ms: i64,
    /// Proven skew (ms) at or beyond which this venue's row FAILs — which degrades that venue to
    /// paper. `None` means this venue can never be failed by the clock leg, because its auth cannot
    /// reject an order over drift; the row tops out at a WARN.
    pub fail_ms: Option<i64>,
    /// What to DO about a reading past `warn_ms`, in this venue's own terms.
    pub remedy: &'static str,
}

impl ClockPolicy {
    /// The verdict `magnitude` (a PROVEN skew — see [`ClockSample::proven_magnitude`]) implies
    /// under this policy. The single place a threshold is applied.
    #[must_use]
    fn status(self, magnitude: i64) -> CheckStatus {
        if self.fail_ms.is_some_and(|fail| magnitude >= fail) {
            CheckStatus::Fail
        } else if magnitude >= self.warn_ms {
            CheckStatus::Warn
        } else {
            CheckStatus::Pass
        }
    }
}

/// Whether a venue may be mounted live, or must fall back to the paper exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueDisposition {
    /// No hard failure for this venue — mount it as configured.
    Live,
    /// A hard FAIL for this venue — mount it paper instead (never panic, never mount half-live).
    Paper,
}

/// Thresholds plus the things to check. `venues` and `dirs` are ordered and the report preserves
/// that order, so operator output is deterministic run to run.
#[derive(Debug, Clone)]
pub struct PreflightConfig {
    /// Venue slugs to run the CLOCK leg against. Empty = no clock legs.
    ///
    /// ⚠ **Separate from [`PreflightConfig::credential_venues`] on purpose, and it was a real
    /// defect that they were one list.** The credential leg can only name venues an authed read was
    /// actually built for — an unwired credential probe FAILs by design, so listing a venue there
    /// that cannot be authed-read MANUFACTURES a failure. The clock leg has the opposite shape: it
    /// needs no credential (most of the wired endpoints are keyless) and it never fails on absence.
    /// While the two shared one list, the checked set was `authed_read_clients().keys()` — the
    /// crypto-CEX trio — so every other venue's clock went unmeasured no matter what endpoint was
    /// wired for it.
    pub clock_venues: Vec<String>,
    /// Venue slugs to run the CREDENTIAL leg against — only venues the caller can actually perform
    /// a cheap authed read for (see the field above). Empty = no credential legs.
    pub credential_venues: Vec<String>,
    /// Per-venue thresholds + remediation text for a MEASURED skew, keyed by venue slug; a venue
    /// with no entry falls back to `(clock_warn_ms, Some(clock_fail_ms), REMEDY_CLOCK)`.
    ///
    /// This is a per-venue map rather than one pair of numbers because the consequence of a drifted
    /// clock is per-venue: binance/bybit/okx/aster reject signed orders over it, deribit and ig
    /// cannot (their auth stamps no timestamp), hyperliquid's nonce window is a DAY wide. A single
    /// "rejected against a 5000 ms recvWindow" line — which is what this module used to print for
    /// every venue — is a false statement on half the roster, the same class of defect as a
    /// capability table shipping a false row; and a single FAIL threshold degrades venues to paper
    /// for a fault their auth cannot suffer. `crate::server_time::clock_policy` is the authority
    /// that fills it.
    pub clock_policies: HashMap<String, ClockPolicy>,
    /// How many times the clock leg may read one venue before concluding (see the module doc's
    /// sampling section). `0` is treated as `1`.
    pub clock_samples: usize,
    /// TOTAL budget (ms) for the whole clock leg — every venue, every resample — measured on the
    /// injected clock. `0` or negative disables the bound entirely (what a single-venue unit test
    /// wants); [`DEFAULT_CLOCK_BUDGET_MS`] is the default. See the module doc's "the clock leg is
    /// BOUNDED" section for the arithmetic this exists to cap.
    pub clock_budget_ms: i64,
    /// TOTAL budget (ms) for the whole CREDENTIAL leg, measured on the injected clock — the twin of
    /// `clock_budget_ms`, and the bound this leg spent its whole life without. `0` or negative
    /// disables it (what a single-venue unit test wants); [`DEFAULT_CREDENTIAL_BUDGET_MS`] is the
    /// default and [`credential_budget_for`] is what a real mount site derives.
    pub credential_budget_ms: i64,
    /// `(label, path)` directories to check headroom on — typically the journal dir and the
    /// hist-store dir. Empty = no disk legs.
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

/// The injected observation seam — the ONLY place this module could touch the outside world, and it
/// never does: every method is supplied by the caller. Object-safe on purpose (`&dyn` everywhere
/// below), so a test double and real wiring are interchangeable with no generics.
pub trait PreflightProbes {
    /// Local wall clock in epoch ms. Injected so skew is deterministic under test.
    fn local_now_ms(&self) -> i64;

    /// The venue's own server time in epoch ms — real wiring is `crate::server_time`'s
    /// `venue_server_time_ms`, which dispatches through the roster-gated declaration table.
    ///
    /// The `Err` side is a two-variant [`ServerTimeGap`], not a string, because "the venue did not
    /// answer" (②, a WARN) and "this venue has no clock leg, here is why" (③, NOT-APPLICABLE) are
    /// opposite facts that used to be indistinguishable here.
    ///
    /// **Units:** this returns an ABSOLUTE epoch-ms timestamp, whereas the venue helpers it wraps
    /// (`BinanceSpotRest::server_time_offset` / `AsterSpotRest::server_time_offset`) return an
    /// OFFSET — `server - local_now_ms`, the value `Signer::set_offset_ms` wants. Real wiring
    /// therefore converts: `let t = local_now_ms(); Ok(t + rest.server_time_offset(t)?)` (any
    /// `local_now_ms` reading works — the offset cancels the one it was measured against).
    fn venue_server_time_ms(&self, venue: &str) -> Result<i64, ServerTimeGap>;

    /// A CHEAP authenticated read against the venue — real wiring reuses that venue's `ReconClient`
    /// balance fetch, which every reconciled venue already builds. `Ok(())` means the credentials
    /// signed and were accepted.
    ///
    /// The `Err` side is [`CredentialGap`], not a string, because a REFUSAL degrades this venue to
    /// paper while a probe that never answered must not. Neither variant's text may contain a
    /// secret (see the module doc).
    fn venue_authed_read(&self, venue: &str) -> Result<(), CredentialGap>;

    /// Free bytes on the filesystem holding `dir`. `Err(reason)` means "could not query", which
    /// warns rather than fails — a preflight must not ground the app on its own inability to
    /// measure.
    fn free_space_bytes(&self, dir: &Path) -> Result<u64, String>;
}

/// Closure-backed [`PreflightProbes`]. Built with [`FnProbes::new`] plus the `with_*` builders; any
/// leg left unset returns [`NO_PROBE`], so a partially-wired preflight warns loudly instead of
/// quietly passing. `local_now_ms` defaults to the real wall clock
/// ([`vike_bridge_core::net_probe::wall_clock_ms`], reused rather than reimplemented).
///
/// Every leg is `Send + Sync`, so `FnProbes` itself is — a mount site may build the probes on the
/// main thread and run the preflight on a spawned one (the real legs do blocking REST reads).
/// Wall-clock leg of [`FnProbes`]. Aliased so the boxed closure types stay under
/// `clippy::type_complexity` (a `-D warnings` gate) without an `allow`.
type ClockFn = Box<dyn Fn() -> i64 + Send + Sync>;
/// Per-venue leg returning a value: the venue slug in, a value or an error out.
type VenueFn<T, E = String> = Box<dyn Fn(&str) -> Result<T, E> + Send + Sync>;
/// Filesystem leg: a directory in, its free bytes out.
type PathFn<T> = Box<dyn Fn(&Path) -> Result<T, String> + Send + Sync>;

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
            // An unwired probe is ②, not ③: it says nothing about the venue, so it must never
            // render as this crate's "that venue publishes no clock" declaration.
            server_time_ms: Box::new(|_: &str| {
                Err(ServerTimeGap::Unreachable(NO_PROBE.to_string()))
            }),
            // An unwired credential leg is the CONFIRMED half deliberately — see
            // `CredentialGap::Rejected`. "the check you asked for does not exist" must not be able
            // to read as "we simply did not hear back", which never demotes.
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

    /// Wire the cheap authenticated read.
    ///
    /// Generic over the error so a probe that knows only "it failed" may keep returning a `String`
    /// (which [`CredentialGap::from`] classifies as the CONFIRMED half) while the bounded runner in
    /// [`crate::startup`] — the one caller that owns a clock and can tell a refusal from a silence
    /// — returns the enum outright.
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

/// The aggregate go/no-go report: every check in the order it ran, plus whether the whole run was
/// skipped by [`PREFLIGHT_SKIP_ENV`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreflightReport {
    /// Every check that ran, in deterministic order (network, then dirs, then per-venue).
    pub checks: Vec<CheckReport>,
    /// `true` when preflight was skipped by env. `checks` is then empty and every accessor below
    /// reads as "nothing objected" — exactly the pre-preflight behaviour.
    pub skipped: bool,
}

impl PreflightReport {
    /// The worst status observed; [`CheckStatus::Pass`] for an empty or skipped report.
    #[must_use]
    pub fn worst(&self) -> CheckStatus {
        self.checks.iter().map(|c| c.status).max().unwrap_or(CheckStatus::Pass)
    }

    /// The go/no-go bit: `false` iff some GLOBAL (venue-less) check hard-failed. A per-venue FAIL
    /// never blocks the go — that venue degrades to paper instead (see the module doc).
    #[must_use]
    pub fn go(&self) -> bool {
        !self.checks.iter().any(|c| c.status.is_fail() && c.venue.is_none())
    }

    /// Venues with at least one hard FAIL, in first-seen order, deduplicated.
    #[must_use]
    pub fn degraded_venues(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in &self.checks {
            if !c.status.is_fail() {
                continue;
            }
            let Some(v) = c.venue.as_ref() else { continue };
            if !out.iter().any(|seen| seen == v) {
                out.push(v.clone());
            }
        }
        out
    }

    /// The degrade-to-paper decision for one venue: [`VenueDisposition::Paper`] iff that venue has
    /// a hard FAIL in this report. A venue that was never checked (and any venue in a skipped
    /// report) reads [`VenueDisposition::Live`] — preflight only ever DEMOTES, it never promotes.
    #[must_use]
    pub fn venue_disposition(&self, venue: &str) -> VenueDisposition {
        let want = Some(venue);
        let failed = self.checks.iter().any(|c| c.status.is_fail() && c.venue.as_deref() == want);
        if failed { VenueDisposition::Paper } else { VenueDisposition::Live }
    }

    /// Every hard-failing check, in order.
    #[must_use]
    pub fn failures(&self) -> Vec<&CheckReport> {
        self.checks.iter().filter(|c| c.status.is_fail()).collect()
    }

    /// One printable line per check: `"[FAIL] credentials (okx): … — remediation"`. The mount site
    /// owns whether these are printed or logged; nothing in this module logs.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::with_capacity(self.checks.len());
        for c in &self.checks {
            let mut s = String::new();
            s.push('[');
            s.push_str(c.status.as_str());
            s.push_str("] ");
            s.push_str(&c.name);
            if let Some(v) = &c.venue {
                s.push_str(" (");
                s.push_str(v);
                s.push(')');
            }
            s.push_str(": ");
            s.push_str(&c.message);
            if !c.remediation.is_empty() {
                s.push_str(" — ");
                s.push_str(&c.remediation);
            }
            out.push(s);
        }
        out
    }
}

/// [`PREFLIGHT_SKIP_ENV`] -> skip the whole preflight. `true` iff the EXACT string `"1"`; unset or
/// anything else (`"true"`, `"yes"`, `"0"`, …) runs it. Same deliberately-unfuzzy idiom as
/// `vike_ops::reconcile_config::reconcile_enabled`'s `VIKE_RECONCILE` gate. `vars` must be
/// built from the REAL process env (`std::env::vars()`), not the credentials `.env` map.
#[must_use]
pub fn preflight_skipped(vars: &HashMap<String, String>) -> bool {
    vars.get(PREFLIGHT_SKIP_ENV).map(String::as_str) == Some("1")
}

/// One clock reading: what it measured, and how much flight it was measured over.
#[derive(Debug, Clone, Copy)]
struct ClockSample {
    /// `server - midpoint(local)`, signed — a leading venue clock is positive.
    skew_ms: i64,
    /// `t1 - t0`: the round trip whose SYMMETRY the midpoint correction assumes, and therefore the
    /// width (±`rtt_ms / 2`) of this reading's uncertainty.
    rtt_ms: i64,
}

impl ClockSample {
    /// A lagging local clock is as fatal as a leading one, so every verdict is on the magnitude.
    fn magnitude(self) -> i64 {
        self.skew_ms.saturating_abs()
    }

    /// The half-width of this reading's uncertainty: the midpoint correction cancels a SYMMETRIC
    /// round trip exactly and leaves the path ASYMMETRY, which is bounded by `rtt / 2`.
    fn slack(self) -> i64 {
        self.rtt_ms / 2
    }

    /// The skew this reading actually PROVES — its band's lower bound, floored at zero. This, not
    /// the point estimate, is what the thresholds are applied to: over a 750 ms round trip a 247 ms
    /// point estimate proves nothing at all, and treating it as a number would let a slow link
    /// manufacture a warning (module doc, "a reading is only as sharp as its round trip").
    fn proven_magnitude(self) -> i64 {
        self.magnitude().saturating_sub(self.slack()).max(0)
    }

    /// The most this reading could be hiding — the band's upper bound.
    fn possible_magnitude(self) -> i64 {
        self.magnitude().saturating_add(self.slack())
    }
}

/// Whether this reading's verdict is safe from its own measurement error: the same status at BOTH
/// ends of the ±rtt/2 uncertainty band, so a tighter round trip could not change it. A reading that
/// is inconclusive gets sampled again (module doc, sampling section) — an exact reading (`rtt` 0)
/// is always conclusive, which is why a deterministic test double is never resampled.
fn sample_is_conclusive(sample: ClockSample, policy: ClockPolicy) -> bool {
    policy.status(sample.proven_magnitude()) == policy.status(sample.possible_magnitude())
}

/// The thresholds `venue` is judged against: its own declared row, else the config's global pair.
/// The fallback deliberately keeps a FAIL — a caller that declared nothing gets the historical
/// behaviour, and only a venue whose row says "this cannot reject an order" loses it.
fn policy_for(venue: &str, cfg: &PreflightConfig) -> ClockPolicy {
    cfg.clock_policies.get(venue).copied().unwrap_or(ClockPolicy {
        warn_ms: cfg.clock_warn_ms,
        fail_ms: Some(cfg.clock_fail_ms),
        remedy: REMEDY_CLOCK,
    })
}

/// (a) CLOCK SKEW for one venue — the check with THREE outcomes (module doc):
///
/// - a MEASUREMENT, compared against `cfg.clock_fail_ms` / `cfg.clock_warn_ms`;
/// - [`ServerTimeGap::Unreachable`] ⇒ [`CheckStatus::Warn`], saying so: this venue publishes a
///   clock and did not answer. Not a FAIL — being unable to measure is not evidence of a bad
///   clock, and it must never degrade a venue on its own;
/// - [`ServerTimeGap::NotChecked`] ⇒ [`CheckStatus::NotApplicable`] plus the declared reason. A
///   permanent property, never a warning, never a fault.
///
/// **The local clock is sampled on BOTH sides of the venue read and the MIDPOINT is what the server
/// stamp is compared against** (the standard NTP-style round-trip correction). A real
/// `venue_server_time_ms` is a blocking REST call, so a single pre-call sample would sit a full
/// round-trip in the past and the measured skew would carry that RTT as bias — enough, on a slow
/// link (e.g. through the Dublin proxy), to push a perfectly-disciplined clock past
/// `clock_warn_ms`. The midpoint assumes a symmetric round trip, so the residual error is only the
/// path ASYMMETRY, not the whole RTT — and `cfg.clock_samples` is how that residual is beaten down
/// when it is large enough to matter.
///
/// ⚠ A read that fails AFTER a good sample keeps the good sample rather than discarding a
/// measurement we already paid for.
///
/// `deadline_ms` is the leg-wide budget's expiry on the SAME injected clock (`None` = unbounded,
/// which is what a single-venue call wants). It is checked before every read, so this function can
/// stall for at most one venue read past it — see the module doc's bound.
#[must_use]
pub fn check_clock_skew(
    venue: &str,
    cfg: &PreflightConfig,
    probes: &dyn PreflightProbes,
    deadline_ms: Option<i64>,
) -> CheckReport {
    let policy = policy_for(venue, cfg);
    let mut best: Option<ClockSample> = None;
    let mut taken = 0usize;
    for _ in 0..cfg.clock_samples.max(1) {
        let t0 = probes.local_now_ms();
        // The BUDGET, checked before the read rather than after it: past the deadline this venue is
        // not read at all, so the leg's total cost cannot exceed the budget plus one in-flight read.
        if deadline_ms.is_some_and(|deadline| t0 >= deadline) {
            if best.is_some() {
                break;
            }
            let budget = cfg.clock_budget_ms;
            let msg = format!(
                "not read: the clock leg's {budget} ms budget was spent before this venue's turn"
            );
            return venue_report(
                CHECK_CLOCK_SKEW,
                venue,
                CheckStatus::Warn,
                msg,
                REMEDY_CLOCK_BUDGET,
            );
        }
        let server = match probes.venue_server_time_ms(venue) {
            Ok(server) => server,
            // ③ — a DECLARED property of the venue, and nothing is at stake. It cannot change
            // between samples and it is not a fault, so it returns immediately with no remediation.
            Err(ServerTimeGap::NotChecked(reason)) => {
                let msg = format!("no clock leg for this venue: {reason}");
                let status = CheckStatus::NotApplicable;
                return venue_report(CHECK_CLOCK_SKEW, venue, status, msg, "");
            }
            // ④ — DECLARED, but this venue's auth binds the clock into the order path, so the gap
            // is an unmeasured hazard rather than a non-issue. A WARN, never NOT-APPLICABLE.
            Err(ServerTimeGap::UnmeasuredRisk { reason, at_stake }) => {
                let msg = format!(
                    "no clock leg for this venue, and ORDERS are at stake: \
                                   {at_stake} ({reason})"
                );
                return venue_report(
                    CHECK_CLOCK_SKEW,
                    venue,
                    CheckStatus::Warn,
                    msg,
                    REMEDY_CLOCK_UNMEASURED,
                );
            }
            // ② — a venue that publishes a clock did not answer.
            Err(ServerTimeGap::Unreachable(e)) => {
                if best.is_some() {
                    break;
                }
                let msg = format!("server time UNAVAILABLE from a venue that publishes it: {e}");
                let status = CheckStatus::Warn;
                return venue_report(
                    CHECK_CLOCK_SKEW,
                    venue,
                    status,
                    msg,
                    REMEDY_CLOCK_UNREACHABLE,
                );
            }
        };
        let t1 = probes.local_now_ms();
        taken += 1;
        let rtt_ms = t1.saturating_sub(t0).max(0);
        let local = t0.saturating_add(t1) / 2;
        let sample = ClockSample { skew_ms: server.saturating_sub(local), rtt_ms };
        // Keep the TIGHTEST round trip, never an average: an asymmetric outlier is a bias in one
        // direction, so averaging it in moves the answer instead of cancelling it.
        if best.is_none_or(|b| sample.rtt_ms < b.rtt_ms) {
            best = Some(sample);
        }
        if sample_is_conclusive(sample, policy) {
            break;
        }
    }
    let Some(sample) = best else {
        // Unreachable in practice: the loop runs at least once, and every exit without a sample
        // returned above. Rendered as ② rather than panicking — a preflight never grounds a mount
        // on its own confusion.
        let msg = format!("server time UNAVAILABLE from a venue that publishes it: {NO_PROBE}");
        return venue_report(
            CHECK_CLOCK_SKEW,
            venue,
            CheckStatus::Warn,
            msg,
            REMEDY_CLOCK_UNREACHABLE,
        );
    };
    // The verdict is on what the reading PROVES (its band's lower bound), never on the raw point
    // estimate — the ±rtt/2 residual is a measurement floor, not noise to be believed through.
    let status = policy.status(sample.proven_magnitude());
    let remedy = if status == CheckStatus::Pass { "" } else { policy.remedy };
    let skew = sample.skew_ms;
    let rtt = sample.rtt_ms;
    let proven = sample.proven_magnitude();
    let warn = policy.warn_ms;
    let fail = policy.fail_ms.map_or_else(
        || "n/a (this venue cannot reject an order over drift)".to_string(),
        |f| format!("{f} ms"),
    );
    let msg = format!(
        "clock skew {skew} ms vs venue (rtt {rtt} ms, best of {taken} sample(s); proven \
         |skew| >= {proven} ms after the ±rtt/2 floor; warn {warn} ms, fail {fail})"
    );
    venue_report(CHECK_CLOCK_SKEW, venue, status, msg, remedy)
}

/// (b) CREDENTIAL VALIDITY for one venue: one cheap authenticated read, with THREE outcomes rather
/// than the two it used to have (module doc, "the disposition used to be computed and thrown away"):
///
/// - `Ok(())` ⇒ [`CheckStatus::Pass`];
/// - [`CredentialGap::Rejected`] ⇒ [`CheckStatus::Fail`] — the venue ANSWERED and refused, which is
///   CONFIRMED evidence and therefore degrades this venue to paper;
/// - [`CredentialGap::Unanswered`] ⇒ [`CheckStatus::Warn`] — we never heard back, which is evidence
///   about the path and not about the keys, so it must never demote anything.
///
/// Never a panic, never a process no-go. The probe's error text is embedded verbatim, so it must
/// not carry a secret.
///
/// `deadline_ms` is the leg-wide budget's expiry on the injected clock (`None` = unbounded, which
/// is what a single-venue unit test wants). Checked BEFORE the probe, so the leg's total cost cannot
/// exceed the budget plus one in-flight probe. A venue the budget never reached says so in its own
/// row — a WARN, because an unrun check is not a finding.
#[must_use]
pub fn check_credentials(
    venue: &str,
    probes: &dyn PreflightProbes,
    deadline_ms: Option<i64>,
) -> CheckReport {
    if deadline_ms.is_some_and(|deadline| probes.local_now_ms() >= deadline) {
        let msg = "not checked: the credential leg's budget was spent before this venue's turn"
            .to_string();
        return venue_report(
            CHECK_CREDENTIALS,
            venue,
            CheckStatus::Warn,
            msg,
            REMEDY_CREDENTIALS_BUDGET,
        );
    }
    match probes.venue_authed_read(venue) {
        Ok(()) => {
            let msg = "authenticated read accepted".to_string();
            let status = CheckStatus::Pass;
            venue_report(CHECK_CREDENTIALS, venue, status, msg, "")
        }
        // CONFIRMED: the venue answered. This is the row that demotes.
        Err(CredentialGap::Rejected(e)) => {
            let msg = format!("authenticated read rejected: {e}");
            let status = CheckStatus::Fail;
            venue_report(CHECK_CREDENTIALS, venue, status, msg, REMEDY_CREDENTIALS)
        }
        // TRANSIENT: we never heard back. Loud, and deliberately harmless.
        Err(CredentialGap::Unanswered { waited_ms, detail }) => {
            let msg = format!(
                "authenticated read did NOT answer within {waited_ms} ms: {detail} (this proves                  nothing about the credentials, so it does not degrade this venue)"
            );
            venue_report(
                CHECK_CREDENTIALS,
                venue,
                CheckStatus::Warn,
                msg,
                REMEDY_CREDENTIALS_UNANSWERED,
            )
        }
    }
}

/// (c) DISK HEADROOM for one watched directory (`label` names it in the report — e.g. `"journal"`,
/// `"hist"`). Below `cfg.disk_fail_bytes` FAILs, below `cfg.disk_warn_bytes` WARNs. This is a
/// GLOBAL check (`venue: None`): a full disk kills the recorders and the WAL for every venue at
/// once, so it flips the go bit rather than demoting one venue. An unqueryable directory WARNs.
#[must_use]
pub fn check_disk_headroom(
    label: &str,
    dir: &Path,
    cfg: &PreflightConfig,
    probes: &dyn PreflightProbes,
) -> CheckReport {
    let scope = format!("{label} ({})", dir.display());
    let free = match probes.free_space_bytes(dir) {
        Ok(free) => free,
        Err(e) => {
            let msg = format!("{scope}: free space unavailable: {e}");
            let status = CheckStatus::Warn;
            return global_report(CHECK_DISK, status, msg, REMEDY_DISK_UNKNOWN);
        }
    };
    let (status, remedy) = if free < cfg.disk_fail_bytes {
        (CheckStatus::Fail, REMEDY_DISK)
    } else if free < cfg.disk_warn_bytes {
        (CheckStatus::Warn, REMEDY_DISK)
    } else {
        (CheckStatus::Pass, "")
    };
    let have = fmt_bytes(free);
    let warn = fmt_bytes(cfg.disk_warn_bytes);
    let fail = fmt_bytes(cfg.disk_fail_bytes);
    let msg = format!("{scope}: {have} free (warn {warn}, fail {fail})");
    global_report(CHECK_DISK, status, msg, remedy)
}

/// (d) NETWORK: reads the EXISTING [`vike_bridge_core::NetProbe`]'s shared state through a
/// [`NetProbeHandle`] — one relaxed atomic load, no probing here (this module must never grow a
/// second probe). A probe that has not completed a round yet WARNs: the handle's `internet_up`
/// starts optimistically `true`, so an unprobed `true` is not a measurement. A measured-down probe
/// is a GLOBAL FAIL.
///
/// `expected` says whether a probe SHOULD be here — `crate::startup` spawns one only when a venue
/// would mount live. Absent-and-expected is an unknown worth a WARN; absent-and-not-expected is a
/// DECLARED non-issue (nothing will place an order), reported [`CheckStatus::NotApplicable`], the
/// same not-applicable-vs-fault split `crate::server_time`'s table draws for a venue publishing no
/// clock. Collapsing the two made every credential-free start WARN with a developer's TODO.
#[must_use]
pub fn check_network(net: Option<&NetProbeHandle>, expected: bool) -> CheckReport {
    let (status, msg, remedy) = match net {
        None if !expected => (
            CheckStatus::NotApplicable,
            "no venue would mount live, so no order path depends on connectivity".to_string(),
            "",
        ),
        None => {
            let msg = "no NetProbe wired".to_string();
            (CheckStatus::Warn, msg, REMEDY_NETWORK_UNKNOWN)
        }
        Some(h) if !h.has_probed() => {
            let msg = "NetProbe has not completed a round yet".to_string();
            (CheckStatus::Warn, msg, REMEDY_NETWORK_UNKNOWN)
        }
        Some(h) if h.internet_up() => {
            let msg = format!("internet up after {} probe round(s)", h.checks());
            (CheckStatus::Pass, msg, "")
        }
        Some(h) => {
            let msg = format!("internet DOWN since epoch ms {}", h.last_down_ms());
            (CheckStatus::Fail, msg, REMEDY_NETWORK)
        }
    };
    global_report(CHECK_NETWORK, status, msg, remedy)
}

/// Every venue this run touches, in a deterministic first-seen order: the clock list, then any
/// credential-only venue not already named. Grouping the report per VENUE (rather than per leg)
/// keeps an operator's eye on one venue at a time, which is how the degrade-to-paper decision is
/// read.
fn checked_venues(cfg: &PreflightConfig) -> Vec<&str> {
    let mut out: Vec<&str> =
        Vec::with_capacity(cfg.clock_venues.len() + cfg.credential_venues.len());
    for venue in cfg.clock_venues.iter().chain(cfg.credential_venues.iter()) {
        if !out.contains(&venue.as_str()) {
            out.push(venue.as_str());
        }
    }
    out
}

/// Run every configured check and aggregate. Deterministic order: the network leg, then each
/// `cfg.dirs` entry in order, then each venue in [`checked_venues`] order with whichever of its two
/// legs are configured (clock before credentials). Performs no I/O of its own and never panics.
///
/// ⚠ The two venue lists are INDEPENDENT (see [`PreflightConfig::clock_venues`]) — a venue may be
/// clock-checked without being authed-readable, which is the whole point: most wired clock
/// endpoints are keyless, while the credential leg can only name venues an authed read was built
/// for.
#[must_use]
pub fn run_preflight(
    cfg: &PreflightConfig,
    probes: &dyn PreflightProbes,
    net: Option<&NetProbeHandle>,
) -> PreflightReport {
    let venues = checked_venues(cfg);
    let mut checks = Vec::with_capacity(1 + cfg.dirs.len() + venues.len() * 2);
    checks.push(check_network(net, !venues.is_empty()));
    for (label, dir) in &cfg.dirs {
        checks.push(check_disk_headroom(label, dir, cfg, probes));
    }
    // ONE deadline for the whole clock leg, taken from the injected clock — the leg is a series of
    // blocking REST reads and its total is what has to be bounded, not each read (module doc).
    // Computed only when the leg has work, so a run with no clock venue reads no clock at all.
    let mut clock_deadline = (cfg.clock_budget_ms > 0 && !cfg.clock_venues.is_empty())
        .then(|| probes.local_now_ms().saturating_add(cfg.clock_budget_ms));
    // …and its twin for the CREDENTIAL leg, which used to have no bound at all. Same shape, same
    // injected clock, computed only when that leg has work.
    let mut credential_deadline = (cfg.credential_budget_ms > 0
        && !cfg.credential_venues.is_empty())
    .then(|| probes.local_now_ms().saturating_add(cfg.credential_budget_ms));
    for venue in venues {
        if cfg.clock_venues.iter().any(|v| v == venue) {
            // Symmetrically to the credential arm below: a clock read is not credential-leg work,
            // so the credential deadline is pushed out by whatever this costs. Without that, one
            // unreachable clock endpoint would silently eat the credential budget and every venue
            // behind it would report "not checked" while blaming a leg that was not at fault —
            // the exact lie the clock side already had to be fixed for.
            let started = credential_deadline.map(|_| probes.local_now_ms());
            checks.push(check_clock_skew(venue, cfg, probes, clock_deadline));
            if let (Some(t0), Some(deadline)) = (started, credential_deadline) {
                let spent = probes.local_now_ms().saturating_sub(t0).max(0);
                credential_deadline = Some(deadline.saturating_add(spent));
            }
        }
        if cfg.credential_venues.iter().any(|v| v == venue) {
            // The credential probe is NOT part of the clock leg, so its cost is pushed OUT of the
            // clock budget rather than charged to it. These are authed reads bounded by
            // `crate::startup`'s own per-attempt ceiling, so ONE unreachable venue would otherwise
            // spend the whole clock budget and drop the clock check of every venue behind it —
            // reported as "an earlier venue's clock read consumed it", which is a lie that sends an
            // operator to healthy rows.
            // The clock is read only when a deadline exists, so a run with no clock leg still
            // touches the clock ZERO times.
            let started = clock_deadline.map(|_| probes.local_now_ms());
            checks.push(check_credentials(venue, probes, credential_deadline));
            if let (Some(t0), Some(deadline)) = (started, clock_deadline) {
                let spent = probes.local_now_ms().saturating_sub(t0).max(0);
                clock_deadline = Some(deadline.saturating_add(spent));
            }
        }
    }
    PreflightReport { checks, skipped: false }
}

/// [`run_preflight`] behind the [`PREFLIGHT_SKIP_ENV`] gate. When skipped it returns the EMPTY
/// report (`skipped: true`) WITHOUT calling a single probe — so the skip path costs nothing and
/// leaves every venue [`VenueDisposition::Live`], i.e. exactly as if no preflight existed.
#[must_use]
pub fn run_preflight_gated(
    vars: &HashMap<String, String>,
    cfg: &PreflightConfig,
    probes: &dyn PreflightProbes,
    net: Option<&NetProbeHandle>,
) -> PreflightReport {
    if preflight_skipped(vars) {
        return PreflightReport { checks: Vec::new(), skipped: true };
    }
    run_preflight(cfg, probes, net)
}

/// Build a process-global (venue-less) check row.
fn global_report(name: &str, status: CheckStatus, message: String, remedy: &str) -> CheckReport {
    CheckReport {
        name: name.to_string(),
        venue: None,
        status,
        message,
        remediation: remedy.to_string(),
    }
}

/// Build a venue-scoped check row — the `venue: Some(..)` that makes a FAIL degrade to paper.
fn venue_report(
    name: &str,
    venue: &str,
    status: CheckStatus,
    message: String,
    remedy: &str,
) -> CheckReport {
    CheckReport {
        name: name.to_string(),
        venue: Some(venue.to_string()),
        status,
        message,
        remediation: remedy.to_string(),
    }
}

/// Human-readable byte size for report messages. Binary units (GiB/MiB) to match how free space is
/// reported by the OS tools an operator would cross-check against.
fn fmt_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use vike_bridge_core::{DEFAULT_PROBE_INTERVAL, NetProbe, NetProbeConfig};

    /// A fixed local clock so skew arithmetic is exact.
    const NOW: i64 = 1_700_000_000_000;

    /// The env-map builder — same helper shape as `reconcile_config`'s tests.
    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A REAL reading, replayed: the local clock either side of one bybit `/v5/market/time` call
    /// and bybit's own stamp, measured from the CI box on 2026-08-08 (rtt 193 ms, +11 ms of skew after
    /// the midpoint correction). Used by the tests that simulate a drifted HOST clock by shifting
    /// these local samples — the comparison input — rather than the box's clock.
    const MEASURED_T0: i64 = 1_786_218_814_267;
    const MEASURED_T1: i64 = 1_786_218_814_460;
    const MEASURED_SERVER: i64 = 1_786_218_814_374;
    /// The skew that reading actually produces: `374 - (267 + 460) / 2`.
    const MEASURED_SKEW_MS: i64 = 11;

    /// All four legs wired healthy: zero skew, accepted credentials, ample free space.
    fn healthy() -> FnProbes {
        FnProbes::new()
            .with_now_ms(|| NOW)
            .with_venue_server_time_ms(|_: &str| Ok(NOW))
            .with_venue_authed_read(|_: &str| Ok::<(), CredentialGap>(()))
            .with_free_space_bytes(|_: &Path| Ok(DEFAULT_DISK_WARN_BYTES))
    }

    /// The measured bybit round trip above, replayed with the local clock shifted by
    /// `host_offset_ms` (positive = this box reads AHEAD of real time).
    fn replay_measured(host_offset_ms: i64) -> FnProbes {
        let samples = Arc::new(AtomicUsize::new(0));
        healthy()
            .with_now_ms(move || {
                let first = samples.fetch_add(1, Ordering::Relaxed).is_multiple_of(2);
                host_offset_ms + if first { MEASURED_T0 } else { MEASURED_T1 }
            })
            .with_venue_server_time_ms(|_: &str| Ok(MEASURED_SERVER))
    }

    /// A probe scripted with `(rtt_ms, skew_ms)` readings, consumed in order: the local clock
    /// advances `rtt_ms` across each venue read, and the venue stamps `midpoint + skew_ms`, so the
    /// check MEASURES exactly the scripted skew over exactly the scripted round trip.
    fn scripted_samples(script: &[(i64, i64)]) -> FnProbes {
        let script: Vec<(i64, i64)> = script.to_vec();
        let for_server = script.clone();
        // (index of the current reading, whether the next clock read is its t0, that t0)
        let state = Arc::new(Mutex::new((0usize, true, NOW)));
        let for_now = Arc::clone(&state);
        let by_server = Arc::clone(&state);
        healthy()
            .with_now_ms(move || {
                let mut g = for_now.lock().unwrap();
                let (i, is_t0, t0) = *g;
                let rtt = script.get(i).map_or(0, |s| s.0);
                if is_t0 {
                    *g = (i, false, t0);
                    t0
                } else {
                    *g = (i + 1, true, t0 + rtt);
                    t0 + rtt
                }
            })
            .with_venue_server_time_ms(move |_: &str| {
                let (i, _, t0) = *by_server.lock().unwrap();
                let (rtt, skew) = for_server[i.min(for_server.len() - 1)];
                Ok(t0 + rtt / 2 + skew)
            })
    }

    /// A clock probe that declares this venue has no leg — outcome ③.
    fn declared(reason: &'static str) -> FnProbes {
        healthy().with_venue_server_time_ms(move |_: &str| Err(ServerTimeGap::NotChecked(reason)))
    }

    /// A clock probe that declares no leg at a venue whose clock IS on the order path — outcome ④.
    fn at_risk(reason: &'static str, at_stake: &'static str) -> FnProbes {
        healthy().with_venue_server_time_ms(move |_: &str| {
            Err(ServerTimeGap::UnmeasuredRisk { reason, at_stake })
        })
    }

    /// A clock probe whose venue publishes an endpoint that did not answer — outcome ②.
    fn unreachable(why: &'static str) -> FnProbes {
        healthy().with_venue_server_time_ms(move |_: &str| {
            Err(ServerTimeGap::Unreachable(why.to_string()))
        })
    }

    /// Healthy probes, except the venue server clock reads `ms` ahead of ours.
    fn skewed(ms: i64) -> FnProbes {
        healthy().with_venue_server_time_ms(move |_: &str| Ok(NOW + ms))
    }

    /// `n` identical readings of `(rtt_ms, skew_ms)` — a venue whose behaviour does not change
    /// between samples, which is what makes "resampling cannot rescue a genuinely bad clock"
    /// testable.
    fn repeated_sample(n: usize, rtt: i64, skew: i64) -> FnProbes {
        let script: Vec<(i64, i64)> = std::iter::repeat_n((rtt, skew), n).collect();
        scripted_samples(&script)
    }

    /// A server-time probe that is fatally skewed for `binance` and fine for everyone else.
    fn binance_skew_only(venue: &str) -> Result<i64, ServerTimeGap> {
        if venue == "binance" { Ok(NOW + DEFAULT_CLOCK_FAIL_MS) } else { Ok(NOW) }
    }

    /// An authed-read probe that rejects `bybit` and accepts everyone else.
    fn bybit_auth_fails(venue: &str) -> Result<(), String> {
        if venue == "bybit" { Err("403".to_string()) } else { Ok(()) }
    }

    /// An authed-read probe that always rejects — the dead-credentials fake.
    fn auth_rejected(_venue: &str) -> Result<(), String> {
        Err("401 invalid api key".to_string())
    }

    /// A free-space probe that cannot answer at all.
    fn disk_unqueryable(_dir: &Path) -> Result<u64, String> {
        Err("no such directory".to_string())
    }

    /// A config running BOTH venue legs over exactly `venues`, defaults everywhere else — the
    /// shape a fully-credentialed CEX mount produces. The two lists are independent in general
    /// (`the_clock_and_credential_venue_lists_are_independent` covers that).
    fn cfg_for(venues: &[&str]) -> PreflightConfig {
        let venues: Vec<String> = venues.iter().map(|v| (*v).to_string()).collect();
        PreflightConfig {
            clock_venues: venues.clone(),
            credential_venues: venues,
            ..PreflightConfig::default()
        }
    }

    /// One journal dir plus one venue — the shape a real mount would use.
    fn cfg_full() -> PreflightConfig {
        let dirs = vec![("journal".to_string(), PathBuf::from("/data/journal"))];
        PreflightConfig { dirs, ..cfg_for(&["binance"]) }
    }

    /// Defaults, plus ONE venue's declared clock policy.
    fn cfg_with_policy(venue: &str, policy: ClockPolicy) -> PreflightConfig {
        let mut clock_policies = HashMap::new();
        clock_policies.insert(venue.to_string(), policy);
        PreflightConfig { clock_policies, ..PreflightConfig::default() }
    }

    /// A probe whose every local-clock read advances the wall clock by `step_ms` — the shape a
    /// slow blocking REST read has — and whose venue stamp always lands exactly on the midpoint,
    /// so a reading measures ZERO skew and the only thing under test is the leg's BUDGET.
    fn ticking_clock(step_ms: i64) -> FnProbes {
        let now = Arc::new(AtomicI64::new(NOW));
        let for_now = Arc::clone(&now);
        let for_server = Arc::clone(&now);
        healthy()
            .with_now_ms(move || for_now.fetch_add(step_ms, Ordering::Relaxed))
            // Called between this read's t0 and t1, i.e. one step after t0: the midpoint is
            // `load - step/2`.
            .with_venue_server_time_ms(move |_: &str| {
                Ok(for_server.load(Ordering::Relaxed) - step_ms / 2)
            })
    }

    /// A NetProbe whose ONE completed round observed `reachable` — injected resolver, no network.
    fn net_probe(reachable: bool) -> NetProbe {
        let cfg = NetProbeConfig {
            hosts: vec!["scripted-host".to_string()],
            interval: DEFAULT_PROBE_INTERVAL,
            failures_before_down: 1,
        };
        let p = NetProbe::new(cfg).expect("non-empty host list");
        let _ = p.probe_once_with(|_: &str| reachable, NOW);
        p
    }

    /// The disk check's status for `free` bytes under `cfg`.
    fn disk_status(free: u64, cfg: &PreflightConfig) -> CheckStatus {
        let probes = FnProbes::new().with_free_space_bytes(move |_: &Path| Ok(free));
        check_disk_headroom("journal", Path::new("/j"), cfg, &probes).status
    }

    // ---- (a) clock skew: thresholds derived from the signers' recvWindow=5000 -------------------

    #[test]
    fn clock_skew_inside_the_warn_band_passes() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("binance", &cfg, &skewed(120), None);
        assert_eq!(r.status, CheckStatus::Pass);
        assert_eq!(r.venue.as_deref(), Some("binance"));
        assert_eq!(r.name, CHECK_CLOCK_SKEW);
        assert!(r.remediation.is_empty(), "a passing check carries no remediation");
    }

    /// The warn boundary is inclusive (`>=`).
    #[test]
    fn clock_skew_at_the_warn_threshold_warns() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("binance", &cfg, &skewed(DEFAULT_CLOCK_WARN_MS), None);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(!r.remediation.is_empty());
    }

    #[test]
    fn clock_skew_just_below_the_fail_threshold_only_warns() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("binance", &cfg, &skewed(DEFAULT_CLOCK_FAIL_MS - 1), None);
        assert_eq!(r.status, CheckStatus::Warn);
    }

    #[test]
    fn clock_skew_at_the_fail_threshold_fails() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("binance", &cfg, &skewed(DEFAULT_CLOCK_FAIL_MS), None);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("2500"), "the measured skew is disclosed: {}", r.message);
    }

    /// A LAGGING local clock is just as fatal as a leading one — the check is on the magnitude.
    #[test]
    fn negative_clock_skew_is_measured_by_magnitude() {
        let cfg = PreflightConfig::default();
        let f = check_clock_skew("binance", &cfg, &skewed(-DEFAULT_CLOCK_FAIL_MS), None);
        assert_eq!(f.status, CheckStatus::Fail);
        let w = check_clock_skew("binance", &cfg, &skewed(-DEFAULT_CLOCK_WARN_MS), None);
        assert_eq!(w.status, CheckStatus::Warn);
    }

    /// The thresholds are config, not values baked into the logic.
    #[test]
    fn clock_thresholds_are_configurable() {
        let cfg =
            PreflightConfig { clock_warn_ms: 10, clock_fail_ms: 50, ..PreflightConfig::default() };
        assert_eq!(check_clock_skew("v", &cfg, &skewed(5), None).status, CheckStatus::Pass);
        assert_eq!(check_clock_skew("v", &cfg, &skewed(20), None).status, CheckStatus::Warn);
        assert_eq!(check_clock_skew("v", &cfg, &skewed(60), None).status, CheckStatus::Fail);
    }

    /// The RTT correction: a PERFECTLY-synced venue clock read over a slow round trip must still
    /// measure ~zero skew. A pre-call-only local sample would have booked the whole 800 ms flight
    /// as skew and warned; the midpoint cancels it.
    #[test]
    fn clock_skew_is_measured_against_the_round_trip_midpoint() {
        let cfg = PreflightConfig::default();
        let rtt = 800;
        assert!(rtt >= DEFAULT_CLOCK_WARN_MS, "precondition: an uncorrected RTT would warn");
        // The venue stamps its (identical) clock mid-flight, i.e. rtt/2 after our first sample.
        let r = check_clock_skew("binance", &cfg, &repeated_sample(1, rtt, 0), None);
        assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
        assert!(r.message.contains("clock skew 0 ms"), "{}", r.message);
    }

    /// ...and the correction does not hide a REAL skew riding on the same slow link. What the slow
    /// link costs is RESOLUTION, not detection: 2500 ms measured over an 800 ms round trip proves
    /// only 2100 ms, so it warns rather than failing, and the SAME drift over a tight link (the
    /// venue next door on the same roster) proves the whole thing and fails.
    #[test]
    fn a_real_skew_is_still_detected_through_a_slow_round_trip() {
        let cfg = PreflightConfig::default();
        let probes = repeated_sample(DEFAULT_CLOCK_SAMPLES, 800, DEFAULT_CLOCK_FAIL_MS);
        let r = check_clock_skew("binance", &cfg, &probes, None);
        assert_eq!(r.status, CheckStatus::Warn, "{}", r.message);
        assert!(r.message.contains("clock skew 2500 ms"), "{}", r.message);
        assert!(r.message.contains("proven |skew| >= 2100 ms"), "{}", r.message);
        assert!(
            r.message.contains(&format!("best of {DEFAULT_CLOCK_SAMPLES} sample(s)")),
            "a reading whose band straddles the fail threshold is looked at again: {}",
            r.message
        );
        // The tight-link twin: the host clock is SHARED, so the venue with the sharpest round trip
        // is the one that resolves it — which is why this rule is not a hole.
        let tight = check_clock_skew("bybit", &cfg, &repeated_sample(1, 60, 2_600), None);
        assert_eq!(tight.status, CheckStatus::Fail, "{}", tight.message);
    }

    /// The local clock is sampled on BOTH sides of the venue read — that is what makes the
    /// midpoint available at all.
    #[test]
    fn the_local_clock_is_sampled_on_both_sides_of_the_venue_read() {
        let cfg = PreflightConfig::default();
        let samples = Arc::new(AtomicUsize::new(0));
        let s = Arc::clone(&samples);
        let probes = healthy().with_now_ms(move || {
            s.fetch_add(1, Ordering::Relaxed);
            NOW
        });
        let r = check_clock_skew("binance", &cfg, &probes, None);
        assert_eq!(r.status, CheckStatus::Pass);
        assert_eq!(samples.load(Ordering::Relaxed), 2, "one sample each side of the venue read");
    }

    /// Not being able to MEASURE the skew is not evidence of a bad clock: warn, never fail, so it
    /// can never degrade a venue on its own.
    #[test]
    fn unmeasurable_clock_skew_warns_and_does_not_degrade() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("binance", &cfg, &unreachable("t/o"), None);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("t/o"));
    }

    // ---- the THREE outcomes: measured / unreachable / declared ---------------------------------

    /// ③ A venue that publishes no clock is NOT-APPLICABLE and carries its declared reason. It is
    /// not a warning, so it can never look like a fault on a healthy mount — the whole defect this
    /// split exists to fix.
    #[test]
    fn a_declared_venue_is_not_applicable_and_states_its_reason() {
        let cfg = PreflightConfig::default();
        let why = "the Open API protobuf schema publishes no server time at all";
        let r = check_clock_skew("ctrader", &cfg, &declared(why), None);
        assert_eq!(r.status, CheckStatus::NotApplicable);
        assert_eq!(r.status.as_str(), "N/A", "it must not read as PASS/WARN/FAIL");
        assert!(r.message.contains(why), "{}", r.message);
        assert!(r.remediation.is_empty(), "there is nothing to remediate: {}", r.remediation);
    }

    /// ② A venue that DOES publish a clock and did not answer says exactly that — the fact that
    /// used to be indistinguishable from ③.
    #[test]
    fn an_unreachable_venue_warns_and_says_the_venue_publishes_one() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("bybit", &cfg, &unreachable("connection timed out"), None);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(
            r.message.contains("publishes it"),
            "the row says the venue HAS one: {}",
            r.message
        );
        assert!(r.message.contains("connection timed out"), "{}", r.message);
        assert_eq!(r.remediation, REMEDY_CLOCK_UNREACHABLE);
    }

    /// ④ A venue with NO leg whose auth signs the clock into the order path is a WARN carrying what
    /// is at stake — NOT the quiet not-applicable row. This is the polymarket shape: the roster's
    /// one order-affecting clock gap was being printed as "nothing to check here".
    #[test]
    fn a_declared_venue_with_orders_at_stake_warns_and_says_what_is_at_stake() {
        let cfg = PreflightConfig::default();
        const REASON: &str = "its CLOB is reachable only through the SOCKS egress proxy";
        const AT_STAKE: &str = "polymarket signs POLY_TIMESTAMP into every authenticated request";
        let r = check_clock_skew("polymarket", &cfg, &at_risk(REASON, AT_STAKE), None);
        assert_eq!(r.status, CheckStatus::Warn, "an unmeasured HAZARD is not a shrug");
        assert!(r.message.contains(AT_STAKE), "{}", r.message);
        assert!(r.message.contains(REASON), "{}", r.message);
        assert_eq!(r.remediation, REMEDY_CLOCK_UNMEASURED);
        // …and it still degrades nothing: the gap is ours, the venue is not at fault.
        let cfg = PreflightConfig {
            clock_venues: vec!["polymarket".to_string()],
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &at_risk(REASON, AT_STAKE), None);
        assert!(report.go());
        assert!(report.degraded_venues().is_empty());
        assert_eq!(report.venue_disposition("polymarket"), VenueDisposition::Live);
    }

    /// The THREE gaps must never render alike — status, message and remedy all differ. ③ and ④ are
    /// the pair that used to be one, and they differ in the field an operator reads FIRST.
    #[test]
    fn the_declared_and_unreachable_gaps_are_distinguishable() {
        let cfg = PreflightConfig::default();
        let d = check_clock_skew(
            "ctrader",
            &cfg,
            &declared("no server time exists in the schema"),
            None,
        );
        let u = check_clock_skew("bybit", &cfg, &unreachable("connection timed out"), None);
        let a =
            check_clock_skew("polymarket", &cfg, &at_risk("no proxy yet", "orders at stake"), None);
        assert_ne!(d.status, u.status);
        assert_ne!(d.remediation, u.remediation);
        assert_ne!(d.message, u.message);
        assert_ne!(d.status, a.status, "③ is quiet, ④ is a warning — that IS the split");
        assert_ne!(a.remediation, u.remediation, "…and ④ is not the 'venue did not answer' line");
        assert_ne!(a.message, u.message);
    }

    /// A declared row raises NOTHING: not the worst status, not a degrade, not the go bit.
    #[test]
    fn a_declared_clock_leg_grounds_nothing() {
        let cfg = PreflightConfig {
            clock_venues: vec!["ctrader".to_string()],
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &declared("no server time exists in the schema"), None);
        let clock: Vec<_> = report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
        assert_eq!(clock.len(), 1);
        assert_eq!(clock[0].status, CheckStatus::NotApplicable);
        assert!(report.go());
        assert!(report.degraded_venues().is_empty());
        assert_eq!(report.venue_disposition("ctrader"), VenueDisposition::Live);
        assert!(
            CheckStatus::NotApplicable < CheckStatus::Pass,
            "N/A must sort below PASS so it never becomes a run's worst status"
        );
    }

    // ---- sampling: the tightest round trip, and only when it matters ---------------------------

    /// A conclusive first reading is NOT resampled — the ordinary case costs one call.
    #[test]
    fn a_conclusive_reading_is_not_resampled() {
        let cfg = PreflightConfig::default();
        // A second reading is scripted, and must never be reached.
        let r =
            check_clock_skew("bybit", &cfg, &scripted_samples(&[(200, 10), (200, 4_000)]), None);
        assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
        assert!(r.message.contains("best of 1 sample(s)"), "{}", r.message);
        assert!(r.message.contains("clock skew 10 ms"), "{}", r.message);
    }

    /// An INCONCLUSIVE reading (its ±rtt/2 band straddles a threshold) is resampled, and the
    /// TIGHTEST round trip wins — the reading a slow, asymmetric hop cannot fake.
    #[test]
    fn an_inconclusive_reading_is_resampled_and_the_tightest_round_trip_wins() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("bybit", &cfg, &scripted_samples(&[(400, 400), (100, 100)]), None);
        assert_eq!(r.status, CheckStatus::Pass, "the tight sample decides: {}", r.message);
        assert!(r.message.contains("clock skew 100 ms"), "{}", r.message);
        assert!(r.message.contains("rtt 100 ms"), "{}", r.message);
        assert!(r.message.contains("best of 2 sample(s)"), "{}", r.message);
    }

    /// …and it is genuinely the TIGHTEST round trip that wins, not merely the last one. Three
    /// inconclusive readings whose middle one is the fastest: keeping the latest instead would
    /// report the 900 ms hop's 800 ms and WARN, which is the false alarm the whole rule exists to
    /// prevent.
    #[test]
    fn the_tightest_round_trip_wins_even_when_it_is_not_the_last() {
        let cfg = PreflightConfig::default();
        let script = [(600, 400), (200, 450), (900, 800)];
        assert_eq!(cfg.clock_samples, script.len(), "every reading must be taken");
        let r = check_clock_skew("bybit", &cfg, &scripted_samples(&script), None);
        assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
        assert!(r.message.contains("clock skew 450 ms"), "{}", r.message);
        assert!(r.message.contains("rtt 200 ms"), "{}", r.message);
        assert!(r.message.contains("best of 3 sample(s)"), "{}", r.message);
    }

    /// …and resampling cannot hide a REAL skew: a genuinely-drifted clock survives every sample.
    #[test]
    fn resampling_does_not_hide_a_real_skew() {
        let cfg = PreflightConfig::default();
        let r =
            check_clock_skew("bybit", &cfg, &scripted_samples(&[(400, 2_600), (100, 2_600)]), None);
        assert_eq!(r.status, CheckStatus::Fail, "{}", r.message);
        assert!(r.message.contains("clock skew 2600 ms"), "{}", r.message);
    }

    /// THE MEASURED HAZARD, replayed: bybit's demo host once read **182 ms of apparent skew over a
    /// 549 ms round trip** (the CI box, 2026-08-08) while every other rep on that host read 13-23 ms.
    /// That is path asymmetry surviving the midpoint correction, not clock error, and it must not
    /// warn — 36% of the warn threshold from one sample is exactly how a check earns its reputation
    /// for crying wolf.
    #[test]
    fn the_measured_round_trip_artifact_does_not_warn() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("bybit", &cfg, &scripted_samples(&[(549, 182)]), None);
        assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
    }

    /// The sample budget is config, not a value baked into the logic.
    #[test]
    fn the_sample_budget_is_configurable() {
        let cfg = PreflightConfig { clock_samples: 1, ..PreflightConfig::default() };
        // Inconclusive, but the budget forbids a second look, so the wide reading stands.
        let r = check_clock_skew("bybit", &cfg, &scripted_samples(&[(400, 400), (100, 100)]), None);
        assert!(r.message.contains("best of 1 sample(s)"), "{}", r.message);
        assert!(r.message.contains("clock skew 400 ms"), "{}", r.message);
    }

    /// A read that fails AFTER a good sample keeps the measurement already paid for, rather than
    /// throwing it away for a "could not measure" warning.
    #[test]
    fn a_late_read_failure_keeps_the_sample_already_taken() {
        let cfg = PreflightConfig::default();
        let reads = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&reads);
        // The local clock advances 400 ms across each read, so the first sample is |skew| 600 over
        // a 400 ms round trip — a band of [400, 800] that straddles the warn threshold, hence
        // INCONCLUSIVE and resampled. The second read then fails.
        let ticks = Arc::new(AtomicI64::new(NOW));
        let t = Arc::clone(&ticks);
        let probes = healthy()
            .with_now_ms(move || t.fetch_add(400, Ordering::Relaxed))
            .with_venue_server_time_ms(move |_: &str| {
                if c.fetch_add(1, Ordering::Relaxed) == 0 {
                    // midpoint (NOW + 200) + 600
                    Ok(NOW + 800)
                } else {
                    Err(ServerTimeGap::Unreachable("connection reset".to_string()))
                }
            });
        let r = check_clock_skew("bybit", &cfg, &probes, None);
        assert_eq!(reads.load(Ordering::Relaxed), 2, "the inconclusive reading WAS retaken");
        assert!(r.message.contains("clock skew 600 ms"), "{}", r.message);
        assert!(r.message.contains("best of 1 sample(s)"), "the failed read is not a sample");
        assert_eq!(r.status, CheckStatus::Pass, "600 ms over a 400 ms rtt proves only 400 ms");
    }

    // ---- the REAL reading, and a simulated host drift on top of it -----------------------------

    /// The measured the CI box bybit reading passes — an actual `/v5/market/time` round trip against an
    /// NTP-disciplined box, replayed through the real check.
    #[test]
    fn the_measured_bybit_reading_passes() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("bybit", &cfg, &replay_measured(0), None);
        assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
        let want = format!("clock skew {MEASURED_SKEW_MS} ms");
        assert!(r.message.contains(&want), "{}", r.message);
        assert!(r.message.contains("rtt 193 ms"), "{}", r.message);
    }

    /// …and the SAME reading with the host clock shifted warns, then fails, with the arithmetic
    /// disclosed. This is the proof the check fires on a real drift rather than only on a
    /// hand-built fixture: only the local samples move, exactly as a mis-set host clock would move
    /// them.
    #[test]
    fn a_simulated_host_drift_on_the_measured_reading_warns_then_fails() {
        let cfg = PreflightConfig::default();

        // Host clock 1 s FAST: the venue now looks 989 ms behind us.
        let w = check_clock_skew("bybit", &cfg, &replay_measured(1_000), None);
        assert_eq!(w.status, CheckStatus::Warn, "{}", w.message);
        let want = format!("clock skew {} ms", MEASURED_SKEW_MS - 1_000);
        assert!(w.message.contains(&want), "{}", w.message);
        assert_eq!(w.remediation, REMEDY_CLOCK, "no per-venue remedy configured here");

        // Host clock 3 s SLOW: past half the recv-window budget, so the venue is a no-go.
        let f = check_clock_skew("bybit", &cfg, &replay_measured(-3_000), None);
        assert_eq!(f.status, CheckStatus::Fail, "{}", f.message);
        let want = format!("clock skew {} ms", MEASURED_SKEW_MS + 3_000);
        assert!(f.message.contains(&want), "{}", f.message);
    }

    // ---- the per-venue remedy ------------------------------------------------------------------

    /// The remedy for a measured skew comes from the VENUE's row, so the report can never assert a
    /// recv-window rejection at a venue whose auth stamps no timestamp.
    #[test]
    fn a_measured_skew_uses_the_venues_own_remedy() {
        const DERIBIT_REMEDY: &str = "this venue's auth stamps no timestamp";
        let cfg = cfg_with_policy(
            "deribit",
            ClockPolicy { warn_ms: DEFAULT_CLOCK_WARN_MS, fail_ms: None, remedy: DERIBIT_REMEDY },
        );

        let d = check_clock_skew("deribit", &cfg, &skewed(DEFAULT_CLOCK_WARN_MS), None);
        assert_eq!(d.status, CheckStatus::Warn);
        assert_eq!(d.remediation, DERIBIT_REMEDY);

        let b = check_clock_skew("bybit", &cfg, &skewed(DEFAULT_CLOCK_WARN_MS), None);
        assert_eq!(
            b.remediation, REMEDY_CLOCK,
            "an undeclared venue falls back to the generic line"
        );
        assert!(
            !REMEDY_CLOCK.contains("recv"),
            "the FALLBACK must claim no rejection mechanism — it does not know the venue"
        );
    }

    /// THE per-venue FAIL rule: a venue whose declared policy carries no `fail_ms` tops out at a
    /// WARN however far its clock reads, so the clock leg can never degrade it to paper. The
    /// SAME reading at a recv-window venue fails. (`crate::server_time::ClockRisk::policy` is what
    /// fills this in production; here the two policies are spelled out so the RULE is what is
    /// under test.)
    #[test]
    fn a_venue_that_cannot_reject_an_order_over_drift_never_fails_its_clock_check() {
        let canary = cfg_with_policy(
            "deribit",
            ClockPolicy { warn_ms: CANARY_CLOCK_WARN_MS, fail_ms: None, remedy: "canary" },
        );
        // Twenty times the recv-window FAIL threshold, and still only a warning.
        let huge = DEFAULT_CLOCK_FAIL_MS * 20;
        let r = check_clock_skew("deribit", &canary, &skewed(huge), None);
        assert_eq!(r.status, CheckStatus::Warn, "{}", r.message);
        assert!(r.message.contains("cannot reject an order"), "the row says why: {}", r.message);
        let report = run_preflight(
            &PreflightConfig { clock_venues: vec!["deribit".to_string()], ..canary },
            &skewed(huge),
            None,
        );
        assert!(report.degraded_venues().is_empty(), "a canary venue is never degraded by a clock");
        assert_eq!(report.venue_disposition("deribit"), VenueDisposition::Live);

        // …and the same reading at a venue that DOES reject orders over drift is a FAIL.
        let signed = cfg_with_policy(
            "bybit",
            ClockPolicy {
                warn_ms: DEFAULT_CLOCK_WARN_MS,
                fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
                remedy: "signed",
            },
        );
        assert_eq!(
            check_clock_skew("bybit", &signed, &skewed(huge), None).status,
            CheckStatus::Fail
        );
    }

    /// A passing clock check carries no remedy, declared or otherwise.
    #[test]
    fn a_passing_clock_check_carries_no_venue_remedy() {
        let cfg = cfg_with_policy(
            "deribit",
            ClockPolicy {
                warn_ms: DEFAULT_CLOCK_WARN_MS,
                fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
                remedy: "never shown",
            },
        );
        assert!(check_clock_skew("deribit", &cfg, &skewed(1), None).remediation.is_empty());
    }

    /// An unwired clock leg reports NO_PROBE rather than quietly passing.
    #[test]
    fn an_unwired_clock_probe_warns_rather_than_passes() {
        let cfg = PreflightConfig::default();
        let r = check_clock_skew("binance", &cfg, &FnProbes::new(), None);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains(NO_PROBE), "{}", r.message);
    }

    /// The clock probe is called with the venue slug being checked, once per configured venue.
    #[test]
    fn clock_probe_receives_the_venue_slug() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let probes = healthy().with_venue_server_time_ms(move |v: &str| {
            sink.lock().unwrap().push(v.to_string());
            Ok(NOW)
        });
        let _ = run_preflight(&cfg_for(&["binance", "bybit"]), &probes, None);
        let want = vec!["binance".to_string(), "bybit".to_string()];
        assert_eq!(*seen.lock().unwrap(), want);
    }

    // ---- (b) credential validity ----------------------------------------------------------------

    #[test]
    fn accepted_authed_read_passes() {
        let r = check_credentials("okx", &healthy(), None);
        assert_eq!(r.status, CheckStatus::Pass);
        assert_eq!(r.venue.as_deref(), Some("okx"));
        assert_eq!(r.name, CHECK_CREDENTIALS);
        assert!(r.remediation.is_empty());
    }

    #[test]
    fn rejected_authed_read_fails_that_venue() {
        let bad = healthy().with_venue_authed_read(auth_rejected);
        let r = check_credentials("okx", &bad, None);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("401 invalid api key"));
        assert!(!r.remediation.is_empty());
    }

    /// An unwired credential probe FAILS (unlike the clock leg): "we could not prove these keys
    /// work" must not mount a venue live.
    #[test]
    fn an_unwired_credential_probe_fails() {
        let r = check_credentials("okx", &FnProbes::new(), None);
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.message.contains(NO_PROBE));
    }

    /// THE distinction the enforcement rests on: a probe that did not ANSWER is a WARN, never a
    /// FAIL — so it cannot degrade a venue and cannot flip the go bit. Being unable to measure is
    /// not evidence, which is the same rule the clock leg's ②/③/④ already obey.
    #[test]
    fn an_unanswered_credential_probe_warns_and_never_degrades() {
        let silent = healthy().with_venue_authed_read(|_: &str| {
            Err(CredentialGap::Unanswered {
                waited_ms: 5_000,
                detail: "no answer from the venue".to_string(),
            })
        });
        let r = check_credentials("alpaca", &silent, None);
        assert_eq!(r.status, CheckStatus::Warn, "a silence must never demote a venue");
        assert_ne!(r.status, CheckStatus::Fail);
        assert!(r.message.contains("5000 ms"), "the row states its own bound: {}", r.message);
        assert!(!r.remediation.is_empty());

        // …and the report agrees: nothing degrades, and the go bit is untouched.
        let cfg = PreflightConfig {
            credential_venues: vec!["alpaca".to_string()],
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &silent, None);
        assert!(report.degraded_venues().is_empty(), "{:?}", report.lines());
        assert_eq!(
            report.venue_disposition("alpaca"),
            VenueDisposition::Live,
            "a venue we merely could not reach must be mounted exactly as configured"
        );
        assert!(report.go());
    }

    /// …and its opposite, which is the row that now has teeth: a venue that ANSWERED and refused is
    /// a FAIL, degrades, and reads `Paper`.
    #[test]
    fn a_rejected_credential_probe_degrades_that_venue_to_paper() {
        let refused = healthy()
            .with_venue_authed_read(|_: &str| Err(CredentialGap::Rejected("401".to_string())));
        let cfg = PreflightConfig {
            credential_venues: vec!["okx".to_string()],
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &refused, None);
        assert_eq!(report.degraded_venues(), vec!["okx".to_string()]);
        assert_eq!(report.venue_disposition("okx"), VenueDisposition::Paper);
        // A per-venue FAIL never grounds the process — only a GLOBAL one flips the go bit.
        assert!(report.go(), "a venue-scoped FAIL must not be a process no-go");
    }

    /// THE credential-leg BOUND, in the pure core: a venue the leg's budget never reached reports
    /// its own row — a WARN naming the budget — rather than vanishing, and rather than being
    /// reported as a credential failure it never measured.
    #[test]
    fn a_credential_venue_past_the_budget_is_warned_not_failed() {
        // A clock that has already passed the deadline the caller hands in.
        let probes = healthy().with_now_ms(|| NOW).with_venue_authed_read(auth_rejected);
        let r = check_credentials("bybit", &probes, Some(NOW - 1));
        assert_eq!(r.status, CheckStatus::Warn, "an UNRUN check is not a finding");
        assert_ne!(r.status, CheckStatus::Fail, "…and must never degrade the venue");
        assert!(r.message.contains("not checked"), "{}", r.message);
    }

    /// The leg's budget is DERIVED from how many venues it must cover, clamped at both ends — the
    /// same shape (and the same rot argument) as [`clock_budget_for`].
    #[test]
    fn the_credential_budget_is_derived_and_clamped() {
        assert_eq!(
            credential_budget_for(0),
            DEFAULT_CREDENTIAL_BUDGET_MS,
            "a floor for a small roster"
        );
        assert_eq!(credential_budget_for(1), DEFAULT_CREDENTIAL_BUDGET_MS);
        assert_eq!(credential_budget_for(4), 4 * PER_VENUE_CREDENTIAL_ALLOWANCE_MS);
        assert!(credential_budget_for(4) > DEFAULT_CREDENTIAL_BUDGET_MS, "…and it GROWS");
        assert_eq!(credential_budget_for(10_000), MAX_CREDENTIAL_BUDGET_MS, "…up to a chosen wall");
        // The requirement it is sized against: a roster of credentialed venues must still fit its
        // per-venue allowance, so a venue joining buys time instead of squeezing its neighbours.
        for n in 1..=(MAX_CREDENTIAL_BUDGET_MS / PER_VENUE_CREDENTIAL_ALLOWANCE_MS) as usize {
            assert!(
                credential_budget_for(n) >= n as i64 * PER_VENUE_CREDENTIAL_ALLOWANCE_MS,
                "{n} credentialed venues do not fit their own allowance"
            );
        }
    }

    /// ⚠ The two legs must not spend EACH OTHER's budget. A slow CLOCK read is not credential-leg
    /// work, so the credential deadline is pushed out by it — without that, one unreachable clock
    /// endpoint would silently drop every credential row behind it and blame a leg that was fine.
    /// (The mirror direction — credential cost not charged to the clock budget — is
    /// `a_slow_credential_probe_does_not_spend_the_clock_budget`.)
    #[test]
    fn a_slow_clock_read_does_not_spend_the_credential_budget() {
        // The clock is a shared cursor. Reading it is FREE (+1 ms, so bookkeeping reads are not
        // themselves charged as work — that would make the accounting untestable); the thing that
        // takes time is the blocking venue READ, which advances the cursor by 4 s. That models the
        // real shape: one slow endpoint, and every credential venue behind it.
        let cursor = Arc::new(AtomicI64::new(NOW));
        let for_now = Arc::clone(&cursor);
        let for_server = Arc::clone(&cursor);
        let probes = healthy()
            .with_now_ms(move || for_now.fetch_add(1, Ordering::Relaxed))
            .with_venue_server_time_ms(move |_: &str| {
                // The venue answers 4 s later, with a stamp matching the (advanced) local clock —
                // so this is a SLOW read, not a skewed one, and no clock row can fail on it.
                Ok(for_server.fetch_add(4_000, Ordering::Relaxed) + 4_000)
            })
            .with_venue_authed_read(|_: &str| Ok::<(), CredentialGap>(()));
        let cfg = PreflightConfig {
            clock_venues: vec!["binance".to_string(), "bybit".to_string()],
            credential_venues: vec!["binance".to_string(), "bybit".to_string()],
            clock_budget_ms: 0, // the clock leg is not what is under test here
            credential_budget_ms: 6_000,
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &probes, None);
        let starved: Vec<&CheckReport> = report
            .checks
            .iter()
            .filter(|c| c.name == CHECK_CREDENTIALS && c.message.contains("not checked"))
            .collect();
        assert!(
            starved.is_empty(),
            "a clock read spent the CREDENTIAL budget: {:?}",
            report.lines()
        );
    }

    /// The seam is PER VENUE: one venue's dead key must not condemn the others.
    #[test]
    fn credential_failure_is_scoped_to_its_own_venue() {
        let probes = healthy().with_venue_authed_read(bybit_auth_fails);
        let report = run_preflight(&cfg_for(&["binance", "bybit", "okx"]), &probes, None);
        assert_eq!(report.degraded_venues(), vec!["bybit".to_string()]);
        assert_eq!(report.venue_disposition("bybit"), VenueDisposition::Paper);
        assert_eq!(report.venue_disposition("binance"), VenueDisposition::Live);
        assert_eq!(report.venue_disposition("okx"), VenueDisposition::Live);
    }

    // ---- (c) disk headroom ----------------------------------------------------------------------

    #[test]
    fn ample_free_space_passes() {
        let cfg = PreflightConfig::default();
        let r = check_disk_headroom("journal", Path::new("/j"), &cfg, &healthy());
        assert_eq!(r.status, CheckStatus::Pass, "at the warn floor exactly, still ample");
        assert_eq!(r.venue, None, "disk is a GLOBAL check, not a per-venue one");
        assert_eq!(r.name, CHECK_DISK);
        assert!(r.message.contains("journal"), "{}", r.message);
    }

    #[test]
    fn free_space_below_the_warn_floor_warns() {
        let cfg = PreflightConfig::default();
        assert_eq!(disk_status(DEFAULT_DISK_WARN_BYTES - 1, &cfg), CheckStatus::Warn);
    }

    #[test]
    fn free_space_below_the_fail_floor_fails() {
        let cfg = PreflightConfig::default();
        assert_eq!(disk_status(DEFAULT_DISK_FAIL_BYTES - 1, &cfg), CheckStatus::Fail);
        assert_eq!(disk_status(0, &cfg), CheckStatus::Fail);
    }

    #[test]
    fn disk_floors_are_configurable() {
        let cfg = PreflightConfig {
            disk_warn_bytes: 2_000,
            disk_fail_bytes: 1_000,
            ..PreflightConfig::default()
        };
        assert_eq!(disk_status(5_000, &cfg), CheckStatus::Pass);
        assert_eq!(disk_status(1_500, &cfg), CheckStatus::Warn);
        assert_eq!(disk_status(500, &cfg), CheckStatus::Fail);
    }

    /// An unqueryable directory warns — a preflight must not ground the app on its own inability
    /// to measure.
    #[test]
    fn unqueryable_free_space_warns() {
        let cfg = PreflightConfig::default();
        let probes = FnProbes::new().with_free_space_bytes(disk_unqueryable);
        let r = check_disk_headroom("journal", Path::new("/nope"), &cfg, &probes);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.contains("no such directory"));
    }

    /// Every configured dir is checked, and the configured path reaches the probe.
    #[test]
    fn every_configured_dir_is_checked() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let probes = healthy().with_free_space_bytes(move |p: &Path| {
            sink.lock().unwrap().push(p.display().to_string());
            Ok(DEFAULT_DISK_WARN_BYTES)
        });
        let dirs = vec![
            ("journal".to_string(), PathBuf::from("/data/journal")),
            ("hist".to_string(), PathBuf::from("/market_data/hist")),
        ];
        let cfg = PreflightConfig { dirs, ..PreflightConfig::default() };
        let report = run_preflight(&cfg, &probes, None);
        assert_eq!(seen.lock().unwrap().len(), 2);
        let disk_checks = report.checks.iter().filter(|c| c.name == CHECK_DISK).count();
        assert_eq!(disk_checks, 2, "one disk check per configured dir");
    }

    // ---- (d) network: the EXISTING NetProbe, read through its handle ----------------------------

    #[test]
    fn no_net_probe_wired_warns() {
        let r = check_network(None, true);
        assert_eq!(r.status, CheckStatus::Warn);
        assert_eq!(r.venue, None);
        assert_eq!(r.name, CHECK_NETWORK);
    }

    /// A run with NO venue to check is a paper run: nothing here will place an order, so the order
    /// path's connectivity is not a fault to report — it is NOT APPLICABLE. It used to WARN on
    /// every credential-free start, with text addressed to a DEVELOPER ("spawn a
    /// vike_bridge_core::NetProbe to make it observable"), which is exactly the fires-forever-on-a
    /// -healthy-box shape this crate's clock table was built to remove.
    #[test]
    fn a_run_with_no_venue_to_check_does_not_warn_about_a_missing_net_probe() {
        let report = run_preflight(&PreflightConfig::default(), &healthy(), None);
        let net: Vec<&CheckReport> =
            report.checks.iter().filter(|c| c.name == CHECK_NETWORK).collect();
        assert_eq!(net.len(), 1, "the row is still PRESENT, never vanished");
        assert_eq!(net[0].status, CheckStatus::NotApplicable, "{}", net[0].message);
        assert!(report.go());
    }

    /// …and the WARN survives where it means something: a venue IS being checked, so a live mount
    /// is in play and its network liveness genuinely is unknown.
    #[test]
    fn a_missing_net_probe_still_warns_when_a_venue_is_being_checked() {
        let cfg =
            PreflightConfig { clock_venues: vec!["binance".to_string()], ..Default::default() };
        let report = run_preflight(&cfg, &healthy(), None);
        let net = report.checks.iter().find(|c| c.name == CHECK_NETWORK).expect("a network row");
        assert_eq!(net.status, CheckStatus::Warn, "{}", net.message);
    }

    /// An unprobed handle reads `internet_up() == true` optimistically — that is NOT a measurement,
    /// so it must warn, not pass.
    #[test]
    fn an_unprobed_net_handle_warns_rather_than_passes() {
        let p = NetProbe::with_defaults();
        let h = p.handle();
        assert!(h.internet_up(), "precondition: optimistic");
        assert!(!h.has_probed(), "precondition: unmeasured");
        assert_eq!(check_network(Some(&h), true).status, CheckStatus::Warn);
    }

    #[test]
    fn a_measured_up_net_probe_passes() {
        let p = net_probe(true);
        let r = check_network(Some(&p.handle()), true);
        assert_eq!(r.status, CheckStatus::Pass);
        assert!(r.remediation.is_empty());
    }

    #[test]
    fn a_measured_down_net_probe_fails_globally() {
        let p = net_probe(false);
        let r = check_network(Some(&p.handle()), true);
        assert_eq!(r.status, CheckStatus::Fail);
        assert_eq!(r.venue, None, "network is global, so it flips the go bit");
    }

    /// The handle is Arc-shared state, so preflight never depends on the probe's lifetime.
    #[test]
    fn a_net_handle_outlives_its_probe() {
        let p = net_probe(false);
        let h = p.handle();
        drop(p);
        assert_eq!(check_network(Some(&h), true).status, CheckStatus::Fail);
    }

    // ---- the aggregate decision -----------------------------------------------------------------

    /// All-green: go, nothing degraded, worst == Pass.
    #[test]
    fn an_all_green_run_is_a_go() {
        let p = net_probe(true);
        let probes = healthy().with_venue_server_time_ms(|_: &str| Ok(NOW + 10));
        let report = run_preflight(&cfg_full(), &probes, Some(&p.handle()));
        assert!(!report.skipped);
        assert_eq!(report.worst(), CheckStatus::Pass);
        assert!(report.go());
        assert!(report.degraded_venues().is_empty());
        assert!(report.failures().is_empty());
        assert_eq!(report.checks.len(), 4, "network + 1 dir + clock + credentials");
    }

    /// Deterministic ordering: network first, then dirs, then per-venue clock+credentials.
    #[test]
    fn checks_are_emitted_in_a_deterministic_order() {
        let dirs = vec![("journal".to_string(), PathBuf::from("/j"))];
        let cfg = PreflightConfig { dirs, ..cfg_for(&["binance", "bybit"]) };
        let report = run_preflight(&cfg, &healthy(), None);
        let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
        let want_names = vec![
            CHECK_NETWORK,
            CHECK_DISK,
            CHECK_CLOCK_SKEW,
            CHECK_CREDENTIALS,
            CHECK_CLOCK_SKEW,
            CHECK_CREDENTIALS,
        ];
        assert_eq!(names, want_names);
        let venues: Vec<_> = report.checks.iter().map(|c| c.venue.as_deref()).collect();
        let b = Some("binance");
        let y = Some("bybit");
        assert_eq!(venues, vec![None, None, b, b, y, y]);
    }

    /// THE degrade-to-paper policy: a per-venue hard FAIL never blocks the go — that venue goes
    /// paper and the process still starts (the "absent credentials => stay paper" idiom).
    #[test]
    fn a_venue_failure_degrades_that_venue_but_is_still_a_go() {
        let probes = healthy().with_venue_server_time_ms(binance_skew_only);
        let report = run_preflight(&cfg_for(&["binance", "okx"]), &probes, None);
        assert!(report.go(), "a venue-scoped failure must never be a process no-go");
        assert_eq!(report.worst(), CheckStatus::Fail);
        assert_eq!(report.degraded_venues(), vec!["binance".to_string()]);
        assert_eq!(report.venue_disposition("binance"), VenueDisposition::Paper);
        assert_eq!(report.venue_disposition("okx"), VenueDisposition::Live);
    }

    /// THE decoupling: the clock list and the credential list are independent, because a clock
    /// endpoint needs no credential and the credential leg FAILs any venue it cannot authed-read.
    /// While they were one list, the clock leg only ever ran for venues that had a reconcile client
    /// (the crypto-CEX trio) — so every other venue's endpoint could be wired and still never
    /// measured.
    #[test]
    fn the_clock_and_credential_venue_lists_are_independent() {
        let cfg = PreflightConfig {
            clock_venues: vec!["deribit".to_string(), "binance".to_string()],
            credential_venues: vec!["binance".to_string()],
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &healthy(), None);
        let rows: Vec<(&str, Option<&str>)> =
            report.checks.iter().map(|c| (c.name.as_str(), c.venue.as_deref())).collect();
        let want = vec![
            (CHECK_NETWORK, None),
            // deribit is clock-checked without an authed read…
            (CHECK_CLOCK_SKEW, Some("deribit")),
            // …and binance gets both legs, grouped together.
            (CHECK_CLOCK_SKEW, Some("binance")),
            (CHECK_CREDENTIALS, Some("binance")),
        ];
        assert_eq!(rows, want);
        assert!(report.go());
        assert!(
            report.degraded_venues().is_empty(),
            "a clock-only venue must never be failed by the credential leg it was not listed for"
        );
    }

    /// A venue that was never checked is never demoted — preflight only DEMOTES.
    #[test]
    fn an_unchecked_venue_stays_live() {
        let report = run_preflight(&cfg_for(&[]), &FnProbes::new(), None);
        assert_eq!(report.venue_disposition("deribit"), VenueDisposition::Live);
    }

    /// A venue with two failing legs is listed ONCE.
    #[test]
    fn degraded_venues_are_deduplicated() {
        let bad = skewed(DEFAULT_CLOCK_FAIL_MS);
        let probes = bad.with_venue_authed_read(auth_rejected);
        let report = run_preflight(&cfg_for(&["aster"]), &probes, None);
        assert_eq!(report.failures().len(), 2, "both venue legs failed");
        assert_eq!(report.degraded_venues(), vec!["aster".to_string()], "but listed once");
    }

    /// A GLOBAL hard failure (full disk) IS a no-go, and demotes no single venue.
    #[test]
    fn a_global_failure_is_a_no_go() {
        let probes = healthy().with_free_space_bytes(|_: &Path| Ok(0));
        let report = run_preflight(&cfg_full(), &probes, None);
        assert!(!report.go(), "no disk headroom is a process-wide no-go");
        assert!(report.degraded_venues().is_empty(), "a global fail demotes no single venue");
        assert_eq!(report.venue_disposition("binance"), VenueDisposition::Live);
    }

    /// Warnings alone never block anything.
    #[test]
    fn warnings_alone_are_still_a_go() {
        let probes = skewed(DEFAULT_CLOCK_WARN_MS);
        let report = run_preflight(&cfg_for(&["binance"]), &probes, None);
        assert_eq!(report.worst(), CheckStatus::Warn);
        assert!(report.go());
        assert!(report.degraded_venues().is_empty());
        assert_eq!(report.venue_disposition("binance"), VenueDisposition::Live);
    }

    /// Pass < Warn < Fail, which is what makes `worst()` a plain max().
    #[test]
    fn status_severity_is_ordered() {
        assert!(CheckStatus::Pass < CheckStatus::Warn);
        assert!(CheckStatus::Warn < CheckStatus::Fail);
        assert_eq!(CheckStatus::Fail.as_str(), "FAIL");
        assert!(CheckStatus::Fail.is_fail());
        assert!(!CheckStatus::Warn.is_fail());
    }

    /// Every check renders as one operator line, with remediation only where there is one.
    #[test]
    fn lines_render_status_name_scope_and_remediation() {
        let probes = healthy().with_venue_authed_read(auth_rejected);
        let report = run_preflight(&cfg_for(&["okx"]), &probes, None);
        let lines = report.lines();
        assert_eq!(lines.len(), 3, "network + clock + credentials");
        let head = "[FAIL] credentials (okx): ";
        assert!(lines.iter().any(|l| l.starts_with(head)), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains(" — ")), "a failing line carries remediation");
    }

    // ---- the skip override (the OFF path) -------------------------------------------------------

    #[test]
    fn preflight_skipped_true_only_for_exact_one() {
        assert!(preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "1")])));
        assert!(!preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "true")])));
        assert!(!preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "yes")])));
        assert!(!preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "0")])));
        assert!(!preflight_skipped(&map(&[("VIKE_PREFLIGHT_SKIP", "")])));
        assert!(!preflight_skipped(&map(&[])));
    }

    /// THE off-path test: with the skip set, NOT ONE probe is called, the report is empty, it is a
    /// go, and every venue stays Live — indistinguishable from having no preflight at all.
    #[test]
    fn the_skip_override_calls_no_probe_and_leaves_every_venue_live() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c1 = Arc::clone(&calls);
        let c2 = Arc::clone(&calls);
        let c3 = Arc::clone(&calls);
        let c4 = Arc::clone(&calls);
        let probes = FnProbes::new()
            .with_now_ms(move || {
                c1.fetch_add(1, Ordering::Relaxed);
                NOW
            })
            .with_venue_server_time_ms(move |_: &str| {
                c2.fetch_add(1, Ordering::Relaxed);
                Ok(NOW + DEFAULT_CLOCK_FAIL_MS)
            })
            .with_venue_authed_read(move |_: &str| {
                c3.fetch_add(1, Ordering::Relaxed);
                Err("401".to_string())
            })
            .with_free_space_bytes(move |_: &Path| {
                c4.fetch_add(1, Ordering::Relaxed);
                Ok(0)
            });
        let vars = map(&[("VIKE_PREFLIGHT_SKIP", "1")]);

        let report = run_preflight_gated(&vars, &cfg_full(), &probes, None);

        assert_eq!(calls.load(Ordering::Relaxed), 0, "the skip path must not probe anything");
        assert!(report.skipped);
        assert!(report.checks.is_empty());
        assert_eq!(report.worst(), CheckStatus::Pass);
        assert!(report.go());
        assert!(report.degraded_venues().is_empty());
        assert_eq!(report.venue_disposition("binance"), VenueDisposition::Live);
        assert!(report.lines().is_empty());
        assert!(report.failures().is_empty());
    }

    /// Unset (the DEFAULT) runs the checks — the skip is opt-in, not opt-out.
    #[test]
    fn unset_skip_runs_the_checks() {
        let vars = map(&[]);
        let cfg = cfg_for(&["binance"]);
        let report = run_preflight_gated(&vars, &cfg, &healthy(), None);
        assert!(!report.skipped);
        assert_eq!(report.checks.len(), 3, "network + clock + credentials");
    }

    /// A non-exact value (`"true"`) does NOT skip — same unfuzzy idiom as VIKE_RECONCILE.
    #[test]
    fn a_fuzzy_skip_value_does_not_skip() {
        let vars = map(&[("VIKE_PREFLIGHT_SKIP", "true")]);
        let cfg = cfg_for(&["binance"]);
        let report = run_preflight_gated(&vars, &cfg, &healthy(), None);
        assert!(!report.skipped);
        assert!(!report.checks.is_empty());
    }

    // ---- misc -----------------------------------------------------------------------------------

    /// An empty config still produces the one global network check (and is a go). With no venue to
    /// check, nothing would mount live, so an unwired probe is DECLARED not-applicable rather than
    /// an unknown — and must still never read as a measurement that was taken.
    #[test]
    fn an_empty_config_still_reports_the_network_check() {
        let cfg = PreflightConfig::default();
        let report = run_preflight(&cfg, &FnProbes::new(), None);
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].name, CHECK_NETWORK);
        assert!(report.go());
        assert_eq!(report.checks[0].status, CheckStatus::NotApplicable);
        assert_ne!(report.checks[0].status, CheckStatus::Pass, "never a fake PASS");
    }

    #[test]
    fn fmt_bytes_renders_binary_units() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(fmt_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(fmt_bytes(DEFAULT_DISK_WARN_BYTES), "5.0 GiB");
    }

    /// The default thresholds are the documented fractions of the signers' recvWindow=5000.
    #[test]
    fn default_clock_thresholds_are_derived_from_the_recv_window() {
        const RECV_WINDOW_MS: i64 = 5_000;
        assert_eq!(DEFAULT_CLOCK_FAIL_MS, RECV_WINDOW_MS / 2, "fail at half the budget");
        assert_eq!(DEFAULT_CLOCK_WARN_MS, RECV_WINDOW_MS / 10, "warn at 10% of the budget");
        let d = PreflightConfig::default();
        assert_eq!(d.clock_warn_ms, DEFAULT_CLOCK_WARN_MS);
        assert_eq!(d.clock_fail_ms, DEFAULT_CLOCK_FAIL_MS);
        assert_eq!(d.clock_samples, DEFAULT_CLOCK_SAMPLES);
        assert_eq!(d.clock_budget_ms, DEFAULT_CLOCK_BUDGET_MS);
        assert!(d.clock_venues.is_empty(), "nothing is checked unless configured");
        assert!(d.credential_venues.is_empty(), "nothing is checked unless configured");
        assert!(d.clock_policies.is_empty(), "no venue policy unless the caller supplies one");
        assert!(d.dirs.is_empty(), "nothing is checked unless configured");
    }

    /// EVERY reading taken on a healthy, NTP-disciplined box, as the `(venue, skew_ms, rtt_ms)`
    /// PAIRS they were measured as — never a bare magnitude, because the round trip is half of what
    /// a reading means (module doc, "a reading is only as sharp as its round trip").
    ///
    /// the CI box (`timedatectl`: "System clock synchronized: yes"), 2026-08-09, reproducible with
    /// `crates/vike-mount/tests/server_time_smoke.rs`. Each row is a real curl-and-`date` sample,
    /// midpoint-corrected exactly as [`check_clock_skew`] corrects. The hyperliquid rows are the
    /// EXTREMES of a 40-sample soak of its testnet node inside three minutes, plus its mainnet
    /// node's own range; the two ig rows are the CI box (2026-08-08) and the Windows dev box.
    const MEASURED_HEALTHY_READINGS: &[(&str, i64, i64)] = &[
        ("binance", 236, 723),
        ("binance", 247, 757),
        ("bybit", 9, 200),
        ("bybit", 23, 221),
        ("okx", 10, 307),
        ("okx", 37, 335),
        ("aster", 15, 269),
        ("aster", 33, 320),
        ("deribit", 15, 55),
        ("deribit", 25, 75),
        ("hyperliquid", -220, 296),
        ("hyperliquid", -424, 282),
        ("hyperliquid", -59, 493),
        ("hyperliquid", -221, 283),
        ("ig", 24, 94),
        ("ig", 160, 444),
        // The worst RTT-asymmetry ARTIFACT ever seen here: bybit demo read 182 ms of apparent skew
        // over a 549 ms round trip while every other rep on the same host read 13-23 ms
        // (2026-08-08). It is path asymmetry surviving the midpoint correction, not clock error.
        ("bybit", 182, 549),
    ];

    /// THE derivation, and it is a MEASUREMENT: every reading above, replayed through the real
    /// check under the venue's REAL declared policy, must PASS. A threshold that warns on a healthy
    /// box is the false-alarm twin of the silent degrade this lane exists to fix.
    ///
    /// ⚠ This replaced a pin of the same intent that was ALREADY FALSE when it shipped — see
    /// `the_replaced_global_derivation_is_falsified_by_its_own_measurement` below.
    #[test]
    fn every_measured_healthy_reading_passes_under_its_venues_real_policy() {
        for &(venue, skew, rtt) in MEASURED_HEALTHY_READINGS {
            let policy = crate::server_time::clock_policy(venue)
                .unwrap_or_else(|| panic!("{venue} is measured here, so its clock must be wired"));
            let cfg = cfg_with_policy(venue, policy);
            // Every sample repeats the reading, so a resample cannot rescue (or degrade) it: the
            // verdict is the one this measurement produces however many times it is looked at.
            let probes = repeated_sample(DEFAULT_CLOCK_SAMPLES, rtt, skew);
            let r = check_clock_skew(venue, &cfg, &probes, None);
            assert_eq!(
                r.status,
                CheckStatus::Pass,
                "{venue} MEASURED {skew} ms over a {rtt} ms round trip on a disciplined host, and \
                 this threshold fires on it: {}",
                r.message
            );
            // …and with margin: what the reading PROVES is at most half the venue's warn floor, so
            // an ordinary bad minute cannot cross it either. (hyperliquid's -424/282 is the
            // tightest row: it proves 283 ms against a 1000 ms floor.)
            let proven = ClockSample { skew_ms: skew, rtt_ms: rtt }.proven_magnitude();
            assert!(
                proven * 2 <= policy.warn_ms,
                "{venue}'s {skew}/{rtt} reading proves {proven} ms against a {} ms floor — under \
                 half the margin this check needs to stay quiet on a healthy box",
                policy.warn_ms
            );
        }
    }

    /// The falsification, kept as a test because it is the ARGUMENT for the per-venue split.
    ///
    /// The shipped derivation pinned `WORST_HEALTHY_SKEW_MS = 288` and const-asserted that the
    /// global 500 ms warn threshold clears it by half again. Re-running that same measurement
    /// produced -424 ms from an NTP-disciplined box: the assert is false against it, and the point
    /// estimate sits 76 ms from warning. Hence hyperliquid is judged against
    /// [`CANARY_CLOCK_WARN_MS`] with no FAIL at all, rather than the recv-window pair.
    #[test]
    fn the_replaced_global_derivation_is_falsified_by_its_own_measurement() {
        const PREVIOUSLY_PINNED_WORST_MS: i64 = 288;
        let worst = MEASURED_HEALTHY_READINGS
            .iter()
            .map(|&(_, skew, _)| skew.abs())
            .max()
            .expect("the table is not empty");
        assert!(
            worst > PREVIOUSLY_PINNED_WORST_MS,
            "the pinned worst-healthy reading was stale the next time it was measured"
        );
        assert!(
            DEFAULT_CLOCK_WARN_MS <= worst + worst / 2,
            "…and the half-again margin the old pin const-asserted does not hold against {worst} ms"
        );
        assert_eq!(
            DEFAULT_CLOCK_WARN_MS - worst,
            76,
            "the global threshold sits this many ms from warning on a HEALTHY box"
        );
        let hl = crate::server_time::clock_policy("hyperliquid").expect("wired");
        assert_eq!(hl.warn_ms, CANARY_CLOCK_WARN_MS, "so it is not judged by that threshold");
        assert_eq!(hl.fail_ms, None, "and it can never be degraded to paper by this leg");
    }

    /// The ±rtt/2 floor, at the row that needs it: binance's demo host reads +247 ms over a 757 ms
    /// round trip on a healthy box, which PROVES nothing at all — a link that slow cannot resolve
    /// half a second. Judging the point estimate instead would have put a 247 ms reading half way
    /// to a warning for no reason.
    #[test]
    fn a_slow_round_trip_cannot_manufacture_a_warning() {
        let cfg = PreflightConfig::default();
        // Every sample repeats the same reading — a venue whose behaviour does not change between
        // looks, which is what makes "no amount of resampling turns this into a warning" the claim.
        let slow = repeated_sample(DEFAULT_CLOCK_SAMPLES, 757, 247);
        let r = check_clock_skew("binance", &cfg, &slow, None);
        assert_eq!(r.status, CheckStatus::Pass, "{}", r.message);
        assert!(
            r.message.contains("proven |skew| >= 0 ms"),
            "the floor is disclosed: {}",
            r.message
        );
        // …and a comparable skew over a TIGHT link does warn: the rule costs resolution, not
        // detection.
        let tight = check_clock_skew("binance", &cfg, &repeated_sample(1, 40, 520), None);
        assert_eq!(tight.status, CheckStatus::Warn, "{}", tight.message);
    }

    // ---- the budget is SIZED AGAINST THE ROSTER, not written down ------------------------------

    /// THE REQUIREMENT, stated as arithmetic over this module's own pinned measurements: the clock
    /// leg's budget must absorb **one timing-out venue and still read every other one**. A venue
    /// that answers nothing costs a full `CLOCK_READ_TIMEOUT`, and one venue being unreachable is
    /// an ordinary Tuesday — not the pathological case the budget exists to bound.
    ///
    /// It failed that at the fixed 5000 ms. Measured on the Windows dev box 2026-08-22: bybit's
    /// endpoint stopped answering, its read spent 3 s of the 5 s, and EIGHT venues behind it
    /// reported "not read" — including aster, which runs against mainnet in practice and whose auth
    /// binds the clock into the order path. Nothing was broken; the budget was simply smaller than
    /// the roster it had to cover, having been sized (module doc) against a SIX-read 1781 ms
    /// measurement and never revisited as venues were added.
    #[test]
    fn the_budget_absorbs_one_dead_venue_and_still_reads_the_rest() {
        // Each wired venue's WORST pinned healthy round trip — what a good pass actually costs.
        let mut healthy_leg_ms = 0i64;
        let mut wired = 0usize;
        for (venue, source) in crate::server_time::CLOCK_SOURCES {
            if !matches!(source, crate::server_time::ClockSource::Wired { .. }) {
                continue;
            }
            wired += 1;
            let worst = MEASURED_HEALTHY_READINGS
                .iter()
                .filter(|(v, _, _)| v == venue)
                .map(|(_, _, rtt)| *rtt)
                .max();
            // A wired venue with no pinned reading contributes its ceiling rather than nothing —
            // an unmeasured venue is not a free one.
            healthy_leg_ms +=
                worst.unwrap_or(crate::server_time::CLOCK_READ_TIMEOUT.as_millis() as i64);
        }
        let dead_venue_ms = crate::server_time::CLOCK_READ_TIMEOUT.as_millis() as i64;
        let required = healthy_leg_ms + dead_venue_ms;
        let budget = clock_budget_for(wired);
        assert!(
            budget >= required,
            "{wired} wired venues cost {healthy_leg_ms} ms on a healthy pass; one dead venue adds \
             {dead_venue_ms} ms, so the leg needs >= {required} ms and the budget is {budget} ms. \
             Raise PER_VENUE_CLOCK_ALLOWANCE_MS — do not special-case a venue."
        );
    }

    /// …and it GROWS with the roster, which is the half that stops it rotting: the fixed constant
    /// was sized against six reads and silently covered a growing roster for weeks.
    #[test]
    fn the_budget_grows_when_a_venue_joins_the_wired_roster() {
        assert!(
            clock_budget_for(8) > clock_budget_for(7),
            "a venue joining the wired roster must buy the leg more time"
        );
    }

    /// …but stays BOUNDED at both ends: a small roster never drops below the floor the leg was
    /// always given, and a large one cannot make a trading daemon's startup unbounded.
    #[test]
    fn the_budget_is_clamped_at_both_ends() {
        assert_eq!(clock_budget_for(0), DEFAULT_CLOCK_BUDGET_MS, "floor");
        assert_eq!(clock_budget_for(1), DEFAULT_CLOCK_BUDGET_MS, "floor");
        assert_eq!(clock_budget_for(10_000), MAX_CLOCK_BUDGET_MS, "ceiling");
        // A const block: the relationship is a compile-time fact, not a runtime one.
        const { assert!(MAX_CLOCK_BUDGET_MS > DEFAULT_CLOCK_BUDGET_MS) };
    }

    // ---- the leg's TOTAL budget ------------------------------------------------------------------

    /// THE BOUND: the clock leg is a series of blocking REST reads, and the budget caps the WHOLE
    /// leg rather than each read. With a budget worth less than two reads, the first venue is
    /// measured and every later one says — in its own row — that it was never read, so the leg's
    /// cost is bounded no matter how many venues are wired.
    #[test]
    fn the_leg_budget_stops_reading_and_says_so() {
        let read_ms = 3_000;
        let cfg = PreflightConfig {
            clock_venues: vec!["binance".to_string(), "bybit".to_string(), "okx".to_string()],
            clock_budget_ms: 5_000,
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &ticking_clock(read_ms), None);
        let clock: Vec<&CheckReport> =
            report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
        assert_eq!(clock.len(), 3, "every venue still gets a ROW");
        assert_eq!(clock[0].status, CheckStatus::Pass, "{}", clock[0].message);
        for row in &clock[1..] {
            assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
            assert!(row.message.contains("5000 ms budget"), "the row names it: {}", row.message);
            assert!(row.message.contains("not read"), "{}", row.message);
        }
        assert!(report.go(), "an unread venue never grounds a mount");
        assert!(report.degraded_venues().is_empty(), "…and never degrades one either");
    }

    /// The budget counts EVERY read, RESAMPLES INCLUDED — the arithmetic it exists to cap is
    /// `venues × samples × per-read timeout`, not `venues × timeout`. A venue whose reading is
    /// inconclusive would otherwise take three reads on its own.
    #[test]
    fn the_budget_counts_resamples_of_a_single_venue() {
        // Each read costs 2000 ms of wall clock and lands |skew| 700 over a 2000 ms round trip:
        // the band is [0, 1700], which straddles the 500 ms warn threshold, so every sample is
        // INCONCLUSIVE and asks to be retaken.
        let probes = || {
            let now = Arc::new(AtomicI64::new(NOW));
            let for_now = Arc::clone(&now);
            let for_server = Arc::clone(&now);
            healthy()
                .with_now_ms(move || for_now.fetch_add(2_000, Ordering::Relaxed))
                .with_venue_server_time_ms(move |_: &str| {
                    Ok(for_server.load(Ordering::Relaxed) - 1_000 + 700)
                })
        };
        let cfg = PreflightConfig {
            clock_venues: vec!["binance".to_string(), "bybit".to_string()],
            clock_budget_ms: 5_000,
            clock_samples: 3,
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &probes(), None);
        let clock: Vec<&CheckReport> =
            report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
        assert!(
            clock[0].message.contains("best of 1 sample(s)"),
            "the budget stopped an inconclusive reading from being retaken: {}",
            clock[0].message
        );
        assert!(
            clock[1].message.contains("budget"),
            "…and the second venue was never read at all: {}",
            clock[1].message
        );

        // THE MUTATION: with the budget off, the same probes spend all three samples on the first
        // venue and then read the second — which is the 630 s arithmetic the budget exists to cap.
        let unbounded = PreflightConfig { clock_budget_ms: 0, ..cfg };
        let report = run_preflight(&unbounded, &probes(), None);
        let clock: Vec<&CheckReport> =
            report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
        for row in clock {
            assert!(
                row.message.contains("best of 3 sample(s)"),
                "unbounded, every venue is read until its sample budget runs out: {}",
                row.message
            );
        }
    }

    /// THE ACCOUNTING: the budget bounds the CLOCK leg, and ONLY the clock leg. `run_preflight`
    /// interleaves each venue's credential probe between the clock reads, and a credential probe is
    /// a blocking authed read of its own — a geo-blocked venue sits on a TCP connect for tens of
    /// seconds. Charging that wall clock to the clock budget silently drops the clock check of
    /// every venue BEHIND it, and the row it emits blames "an earlier venue's clock read", sending
    /// an operator to inspect rows that are all fast and healthy. Measured on the Windows dev box
    /// 2026-08-22: the whole clock leg cost 3.2 s of its 5 s budget, the preflight took 27 s, and
    /// alpaca/aster/hyperliquid each reported their clock "not read".
    #[test]
    fn a_slow_credential_probe_does_not_spend_the_clock_budget() {
        let now = Arc::new(AtomicI64::new(NOW));
        let (for_now, for_server, for_auth) =
            (Arc::clone(&now), Arc::clone(&now), Arc::clone(&now));
        // Every clock read costs 100 ms; the ONE credential probe costs 10 s — twice the budget.
        let probes = healthy()
            .with_now_ms(move || for_now.fetch_add(100, Ordering::Relaxed))
            .with_venue_server_time_ms(move |_: &str| Ok(for_server.load(Ordering::Relaxed) - 50))
            .with_venue_authed_read(move |_: &str| {
                for_auth.fetch_add(10_000, Ordering::Relaxed);
                Ok::<(), CredentialGap>(())
            });
        let cfg = PreflightConfig {
            clock_venues: vec!["binance".to_string(), "bybit".to_string()],
            credential_venues: vec!["binance".to_string()],
            clock_budget_ms: 5_000,
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &probes, None);
        let clock: Vec<&CheckReport> =
            report.checks.iter().filter(|c| c.name == CHECK_CLOCK_SKEW).collect();
        assert_eq!(clock.len(), 2, "every clock venue still gets a ROW");
        assert!(
            !clock[1].message.contains("budget"),
            "the clock leg spent 200 ms of its 5000 ms budget — the 10 s credential probe is not \
             its cost: {}",
            clock[1].message
        );
        assert_eq!(clock[1].status, CheckStatus::Pass, "{}", clock[1].message);
    }

    /// A zero/negative budget is UNBOUNDED — the shape a single-venue caller or a test wants — and
    /// a run with no clock venue never reads the clock for a deadline it will not use.
    #[test]
    fn a_zero_budget_is_unbounded_and_an_empty_leg_reads_no_clock() {
        let cfg = PreflightConfig {
            clock_venues: vec!["binance".to_string(), "bybit".to_string(), "okx".to_string()],
            clock_budget_ms: 0,
            ..PreflightConfig::default()
        };
        let report = run_preflight(&cfg, &ticking_clock(3_000), None);
        let budgeted = report.checks.iter().filter(|c| c.message.contains("budget")).count();
        assert_eq!(budgeted, 0, "an unbounded leg reads every venue");

        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let probes = healthy().with_now_ms(move || {
            c.fetch_add(1, Ordering::Relaxed);
            NOW
        });
        let no_clock = PreflightConfig {
            credential_venues: vec!["binance".to_string()],
            // ⚠ BOTH budgets off. The CREDENTIAL leg gained a deadline of its own, and a deadline
            // is measured on the injected clock — so this property is now "no BOUNDED leg ⇒ no
            // clock read", not "no clock leg ⇒ no clock read". Leaving the credential budget at its
            // default here measured two reads and looked like a regression in the clock leg; it was
            // the new bound doing exactly its job.
            credential_budget_ms: 0,
            ..PreflightConfig::default()
        };
        let _ = run_preflight(&no_clock, &probes, None);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "no bounded leg ⇒ no deadline ⇒ no clock read"
        );

        // …and the twin the new bound needs: with the credential budget ON, the leg DOES consult
        // the clock, because that is what a deadline is. A bound nobody measures is not a bound.
        let counted = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&counted);
        let bounded_probes = healthy().with_now_ms(move || {
            c2.fetch_add(1, Ordering::Relaxed);
            NOW
        });
        let bounded = PreflightConfig {
            credential_venues: vec!["binance".to_string()],
            credential_budget_ms: DEFAULT_CREDENTIAL_BUDGET_MS,
            ..PreflightConfig::default()
        };
        let _ = run_preflight(&bounded, &bounded_probes, None);
        assert!(counted.load(Ordering::Relaxed) > 0, "a bounded credential leg must measure time");
    }

    /// FnProbes' Debug must never leak a closure's captured environment.
    #[test]
    fn fn_probes_debug_is_opaque() {
        assert_eq!(format!("{:?}", FnProbes::new()), "FnProbes(<injected closures>)");
    }

    /// Compile-time proof that the probe bundle crosses threads: a mount site builds it on the
    /// main thread and the real (blocking-REST) legs run wherever the preflight is driven from.
    #[test]
    fn fn_probes_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FnProbes>();
    }
}
