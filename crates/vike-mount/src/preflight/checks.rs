//! (b) The credential leg, (c) the disk-headroom leg and (d) the network leg.

use std::path::Path;

use vike_bridge_core::NetProbeHandle;

use super::config::{
    CHECK_CREDENTIALS, CHECK_DISK, CHECK_NETWORK, PreflightConfig, REMEDY_CREDENTIALS,
    REMEDY_CREDENTIALS_BUDGET, REMEDY_CREDENTIALS_UNANSWERED, REMEDY_DISK, REMEDY_DISK_UNKNOWN,
    REMEDY_NETWORK, REMEDY_NETWORK_UNKNOWN,
};
use super::probes::{CredentialGap, PreflightProbes};
use super::report::{CheckReport, CheckStatus, fmt_bytes, global_report, venue_report};

/// (b) CREDENTIAL VALIDITY for one venue: one cheap authenticated read, three outcomes:
///
/// - `Ok(())` ⇒ [`CheckStatus::Pass`];
/// - [`CredentialGap::Rejected`] ⇒ [`CheckStatus::Fail`]: the venue ANSWERED and refused —
///   confirmed evidence, so this venue degrades to paper;
/// - [`CredentialGap::Unanswered`] ⇒ [`CheckStatus::Warn`]: evidence about the path, not the keys,
///   so it never demotes anything.
///
/// Never a panic, never a process no-go. The probe's error text is embedded verbatim: it must not
/// carry a secret. `deadline_ms` is the leg-wide budget's expiry on the injected clock (`None` =
/// unbounded), checked BEFORE the probe, so the leg costs at most the budget plus one in-flight
/// probe; a venue the budget never reached gets its own WARN row (an unrun check is not a finding).
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
                "authenticated read did NOT answer within {waited_ms} ms: {detail} (this proves nothing about the credentials, so it does not degrade this venue)"
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

/// (c) DISK HEADROOM for one watched directory (`label`, e.g. `"journal"`). Below
/// `cfg.disk_fail_bytes` FAILs, below `cfg.disk_warn_bytes` WARNs, unqueryable WARNs. GLOBAL
/// (`venue: None`): a full disk kills the recorders and the WAL for every venue at once.
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

/// (d) NETWORK: reads the EXISTING [`vike_bridge_core::NetProbe`] through a [`NetProbeHandle`]
/// (one relaxed atomic load; this module must never grow a second probe). A probe with no completed
/// round WARNs (`internet_up` starts optimistically `true`, which is not a measurement); a
/// measured-down probe is a GLOBAL FAIL.
///
/// `expected`: `crate::startup` spawns a probe only when a venue would mount live.
/// Absent-and-expected WARNs; absent-and-not-expected is [`CheckStatus::NotApplicable`] (nothing
/// will place an order) — collapsing the two made every credential-free start WARN.
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
