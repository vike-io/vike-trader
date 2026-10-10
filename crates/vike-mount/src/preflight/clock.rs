//! (a) The clock-skew leg: the per-venue policy, one reading's band, and the check itself.

use super::config::{
    CHECK_CLOCK_SKEW, NO_PROBE, PreflightConfig, REMEDY_CLOCK, REMEDY_CLOCK_BUDGET,
    REMEDY_CLOCK_UNMEASURED, REMEDY_CLOCK_UNREACHABLE,
};
use super::probes::{PreflightProbes, ServerTimeGap};
use super::report::{CheckReport, CheckStatus, venue_report};

/// The thresholds and remediation text ONE venue's clock reading is judged against: the numbers,
/// the words and whether a FAIL is meaningful are all per venue (module doc, "the clock-skew
/// thresholds are PER VENUE"); `crate::server_time`'s `clock_policy_of` fills it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockPolicy {
    /// Proven skew (ms) at or beyond which this venue's row WARNs.
    pub warn_ms: i64,
    /// Proven skew (ms) at or beyond which this venue's row FAILs (degrades it to paper). `None`:
    /// its auth cannot reject an order over drift, so the row tops out at a WARN.
    pub fail_ms: Option<i64>,
    /// What to DO about a reading past `warn_ms`, in this venue's own terms.
    pub remedy: &'static str,
}

impl ClockPolicy {
    /// The verdict a PROVEN skew ([`ClockSample::proven_magnitude`]) implies. The single place a
    /// threshold is applied.
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

/// One clock reading: what it measured, and how much flight it was measured over.
///
/// Public, with [`ClockSample::proven_magnitude`], for the roster tests in
/// `crates/vike-tradehub/tests/mount_roster.rs` (docs/decisions/0096), which replay MEASURED
/// readings under each venue's declared policy. A plain value: every field combination is valid.
#[derive(Debug, Clone, Copy)]
pub struct ClockSample {
    /// `server - midpoint(local)`, signed — a leading venue clock is positive.
    pub skew_ms: i64,
    /// `t1 - t0`: the round trip whose SYMMETRY the midpoint assumes; ±`rtt_ms / 2` is the
    /// reading's uncertainty.
    pub rtt_ms: i64,
}

impl ClockSample {
    /// A lagging local clock is as fatal as a leading one, so every verdict is on the magnitude.
    fn magnitude(self) -> i64 {
        self.skew_ms.saturating_abs()
    }

    /// Half-width of the uncertainty: the midpoint cancels a SYMMETRIC round trip and leaves the
    /// path ASYMMETRY, bounded by `rtt / 2`.
    fn slack(self) -> i64 {
        self.rtt_ms / 2
    }

    /// The skew this reading PROVES — its band's lower bound, floored at zero; the thresholds apply
    /// to this, not the point estimate: over a 750 ms round trip a 247 ms estimate proves nothing,
    /// and a slow link would manufacture a warning (module doc, "a reading is only as sharp as its
    /// round trip").
    #[must_use]
    pub fn proven_magnitude(self) -> i64 {
        self.magnitude().saturating_sub(self.slack()).max(0)
    }

    /// The most this reading could be hiding — the band's upper bound.
    fn possible_magnitude(self) -> i64 {
        self.magnitude().saturating_add(self.slack())
    }
}

/// The same status at BOTH ends of the ±rtt/2 band, so a tighter round trip could not change it.
/// An inconclusive reading is sampled again; an exact one (`rtt` 0, any test double) never is.
fn sample_is_conclusive(sample: ClockSample, policy: ClockPolicy) -> bool {
    policy.status(sample.proven_magnitude()) == policy.status(sample.possible_magnitude())
}

/// `venue`'s declared row, else the config's global pair — which deliberately keeps a FAIL: only a
/// row saying "this cannot reject an order" loses it.
fn policy_for(venue: &str, cfg: &PreflightConfig) -> ClockPolicy {
    cfg.clock_policies.get(venue).copied().unwrap_or(ClockPolicy {
        warn_ms: cfg.clock_warn_ms,
        fail_ms: Some(cfg.clock_fail_ms),
        remedy: REMEDY_CLOCK,
    })
}

/// (a) CLOCK SKEW for one venue — the check with FOUR outcomes (module doc):
///
/// - ① a MEASUREMENT, judged against the venue's [`ClockPolicy`] (`cfg.clock_policies`, else the
///   global `cfg.clock_warn_ms` / `cfg.clock_fail_ms` pair);
/// - ② [`ServerTimeGap::Unreachable`] ⇒ [`CheckStatus::Warn`]: a venue that publishes a clock did
///   not answer. Never a FAIL — being unable to measure is no evidence of a bad clock;
/// - ③ [`ServerTimeGap::NotChecked`] ⇒ [`CheckStatus::NotApplicable`] plus the declared reason;
/// - ④ [`ServerTimeGap::UnmeasuredRisk`] ⇒ [`CheckStatus::Warn`]: no leg, but orders are at stake.
///
/// **The server stamp is compared against the MIDPOINT of local samples taken on BOTH sides of the
/// read** (NTP-style): a single pre-call sample would carry the whole RTT as bias in the measured
/// skew — enough on a slow link (the Dublin proxy) to push a disciplined clock past
/// `clock_warn_ms`. The residual is the path ASYMMETRY only; `cfg.clock_samples` beats it down.
///
/// ⚠ A read that fails AFTER a good sample keeps the good sample (already paid for).
///
/// `deadline_ms`: the leg-wide budget's expiry on the SAME injected clock (`None` = unbounded),
/// checked before every read, so at most one venue read runs past it.
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
        // The BUDGET, checked BEFORE the read: the leg costs at most budget + one in-flight read.
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
            // ③ — DECLARED, nothing at stake, cannot change between samples: return, no remedy.
            Err(ServerTimeGap::NotChecked(reason)) => {
                let msg = format!("no clock leg for this venue: {reason}");
                let status = CheckStatus::NotApplicable;
                return venue_report(CHECK_CLOCK_SKEW, venue, status, msg, "");
            }
            // ④ — DECLARED, but the venue's auth binds the clock into the order path: an
            // unmeasured hazard, so a WARN, never NOT-APPLICABLE.
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
        // Keep the TIGHTEST round trip, never an average: an asymmetric outlier is a one-way bias.
        if best.is_none_or(|b| sample.rtt_ms < b.rtt_ms) {
            best = Some(sample);
        }
        if sample_is_conclusive(sample, policy) {
            break;
        }
    }
    let Some(sample) = best else {
        // Unreachable (every sample-less exit returned above); rendered as ② rather than a panic.
        let msg = format!("server time UNAVAILABLE from a venue that publishes it: {NO_PROBE}");
        return venue_report(
            CHECK_CLOCK_SKEW,
            venue,
            CheckStatus::Warn,
            msg,
            REMEDY_CLOCK_UNREACHABLE,
        );
    };
    // The verdict is on what the reading PROVES, never the raw point estimate.
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
