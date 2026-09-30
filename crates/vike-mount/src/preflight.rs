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
//! (`crates/bridges/polymarket/src/exec_plane/auth.rs`'s `l2_auth_headers`) and this preflight measures no
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
//! wrong in both directions.** `crate::server_time`'s `clock_policy_of` is the authority; the two
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
//! `(skew, rtt)` PAIRS in `preflight_tests::MEASURED_HEALTHY_READINGS` — replayed through the real check, not
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
//! which is the failure `crates/vike-datahub/src/recorder.rs`'s `--exit-on-silence`
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
//! the one `crates/vike-datahub/src/recorder.rs`'s module doc already argues for
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
//! `vike_tradehub::reconcile_config` (one unambiguous on-string to grep for in an incident;
//! `"true"` / `"yes"` / `"0"` all stay off). Like every other `VIKE_*` feature toggle it must be
//! sourced from the REAL process env, not the credentials `.env` map — see
//! `vike_tradehub::reconcile_config`'s module doc for why. Skipping yields an EMPTY report
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
/// It is a WARN and nothing more: `crate::server_time`'s `clock_policy_of` gives these venues no
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
/// windows: `vike_bridge_core::venue_mount`'s `ClockRisk` is what knows whether this venue rejects
/// orders over drift, and a generic line that claimed it would be false on half the roster.
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
/// `clock_policy_of` is the authority that fills it.
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
/// `vike_tradehub::reconcile_config::reconcile_enabled`'s `VIKE_RECONCILE` gate. `vars` must be
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

#[path = "preflight_tests.rs"]
#[cfg(test)]
mod preflight_tests;
