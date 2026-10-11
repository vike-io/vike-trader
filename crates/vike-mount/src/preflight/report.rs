//! The report vocabulary: check verdicts, check rows, venue dispositions, the aggregate.

use std::fmt;

#[cfg(doc)]
use super::config::{
    CHECK_CLOCK_SKEW, CHECK_CREDENTIALS, CHECK_DISK, CHECK_NETWORK, PREFLIGHT_SKIP_ENV,
};

/// The verdict of one check. Ordered `NotApplicable < Pass < Warn < Fail`, so
/// [`PreflightReport::worst`] is a plain `max()` and a not-applicable row never raises severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckStatus {
    /// DECLARED not-applicable (③ in the module doc): a permanent property of the venue or this
    /// code, never a fault nor a warning. Sorts BELOW [`CheckStatus::Pass`]: a pass measured
    /// something.
    NotApplicable,
    /// Measured, and within limits.
    Pass,
    /// Either measured-but-marginal, or NOT measurable: an unanswered probe warns, never passes,
    /// and never grounds anything on its own.
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

/// Whether a venue may be mounted live, or must fall back to the paper exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueDisposition {
    /// No hard failure for this venue — mount it as configured.
    Live,
    /// A hard FAIL for this venue — mount it paper instead (never panic, never mount half-live).
    Paper,
}

/// The aggregate go/no-go report: every check in the order it ran, plus whether the whole run was
/// skipped by [`PREFLIGHT_SKIP_ENV`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreflightReport {
    /// Every check that ran, in deterministic order (network, then dirs, then per-venue).
    pub checks: Vec<CheckReport>,
    /// `true` when skipped by the `flags.preflight_skip` row: `checks` is empty and every accessor
    /// reads "nothing objected".
    pub skipped: bool,
}

impl PreflightReport {
    /// The worst status observed; [`CheckStatus::Pass`] for an empty or skipped report.
    #[must_use]
    pub fn worst(&self) -> CheckStatus {
        self.checks.iter().map(|c| c.status).max().unwrap_or(CheckStatus::Pass)
    }

    /// The go/no-go bit: `false` iff some GLOBAL (venue-less) check hard-failed; a per-venue FAIL
    /// degrades that venue to paper instead.
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

    /// [`VenueDisposition::Paper`] iff that venue has a hard FAIL here; an unchecked venue (or a
    /// skipped report) reads [`VenueDisposition::Live`] — preflight only DEMOTES, never promotes.
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

/// Build a process-global (venue-less) check row.
pub(super) fn global_report(
    name: &str,
    status: CheckStatus,
    message: String,
    remedy: &str,
) -> CheckReport {
    CheckReport {
        name: name.to_string(),
        venue: None,
        status,
        message,
        remediation: remedy.to_string(),
    }
}

/// Build a venue-scoped check row — the `venue: Some(..)` that makes a FAIL degrade to paper.
pub(super) fn venue_report(
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

/// Human-readable byte size, in binary units (GiB/MiB) like the OS tools an operator cross-checks.
pub(super) fn fmt_bytes(bytes: u64) -> String {
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
