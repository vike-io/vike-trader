//! (b) The credential leg, (c) the disk-headroom leg and (d) the network leg.

use std::path::Path;

use vike_model::NetReading;
use vike_utility::bytes::iec_bytes;

use super::config::{
    CHECK_CREDENTIALS, CHECK_DISK, CHECK_NETWORK, PreflightConfig, REMEDY_CREDENTIALS,
    REMEDY_CREDENTIALS_BUDGET, REMEDY_CREDENTIALS_UNANSWERED, REMEDY_DISK, REMEDY_DISK_UNKNOWN,
    REMEDY_NETWORK, REMEDY_NETWORK_UNKNOWN,
};
use super::probes::PreflightProbes;
use super::report::{PreflightCheckReport, PreflightCheckStatus, global_report, venue_report};
use vike_model::CredentialGap;

/// (b) CREDENTIAL VALIDITY for one venue: one cheap authenticated read, three outcomes:
///
/// - `Ok(())` ⇒ [`PreflightCheckStatus::Pass`];
/// - [`CredentialGap::Rejected`] ⇒ [`PreflightCheckStatus::Fail`]: the venue ANSWERED and refused —
///   confirmed evidence, so this venue's `demo`/`live` accounts do not run;
/// - [`CredentialGap::Unanswered`] ⇒ [`PreflightCheckStatus::Warn`]: evidence about the path, not the keys,
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
) -> PreflightCheckReport {
    if deadline_ms.is_some_and(|deadline| probes.local_now_ms() >= deadline) {
        let msg = "not checked: the credential leg's budget was spent before this venue's turn"
            .to_string();
        return venue_report(
            CHECK_CREDENTIALS,
            venue,
            PreflightCheckStatus::Warn,
            msg,
            REMEDY_CREDENTIALS_BUDGET,
        );
    }
    match probes.venue_authed_read(venue) {
        Ok(()) => {
            let msg = "authenticated read accepted".to_string();
            let status = PreflightCheckStatus::Pass;
            venue_report(CHECK_CREDENTIALS, venue, status, msg, "")
        }
        // CONFIRMED: the venue answered. This is the row that demotes.
        Err(CredentialGap::Rejected(e)) => {
            let msg = format!("authenticated read rejected: {e}");
            let status = PreflightCheckStatus::Fail;
            venue_report(CHECK_CREDENTIALS, venue, status, msg, REMEDY_CREDENTIALS)
        }
        // TRANSIENT: we never heard back. Loud, and deliberately harmless.
        Err(CredentialGap::Unanswered { waited_ms, detail }) => {
            // `0`: the venue answered with no verdict (a 5xx, a throttle, a refused connect), so no bound was waited out.
            let within =
                if waited_ms == 0 { String::new() } else { format!(" within {waited_ms} ms") };
            let msg = format!(
                "authenticated read did NOT answer{within}: {detail} (this proves nothing about the credentials, so it does not degrade this venue)"
            );
            venue_report(
                CHECK_CREDENTIALS,
                venue,
                PreflightCheckStatus::Warn,
                msg,
                REMEDY_CREDENTIALS_UNANSWERED,
            )
        }
    }
}

/// (c) DISK HEADROOM for one watched directory (`label`, e.g. `"journal"`). Below
/// `config.disk_fail_bytes` FAILs, below `config.disk_warn_bytes` WARNs, unqueryable WARNs. GLOBAL
/// (`venue: None`): a full disk kills the recorders and the WAL for every venue at once.
#[must_use]
pub fn check_disk_headroom(
    label: &str,
    dir: &Path,
    config: &PreflightConfig,
    probes: &dyn PreflightProbes,
) -> PreflightCheckReport {
    let scope = format!("{label} ({})", dir.display());
    let free = match probes.free_space_bytes(dir) {
        Ok(free) => free,
        Err(e) => {
            let msg = format!("{scope}: free space unavailable: {e}");
            let status = PreflightCheckStatus::Warn;
            return global_report(CHECK_DISK, status, msg, REMEDY_DISK_UNKNOWN);
        }
    };
    let (status, remedy) = if free < config.disk_fail_bytes {
        (PreflightCheckStatus::Fail, REMEDY_DISK)
    } else if free < config.disk_warn_bytes {
        (PreflightCheckStatus::Warn, REMEDY_DISK)
    } else {
        (PreflightCheckStatus::Pass, "")
    };
    let have = iec_bytes(free);
    let warn = iec_bytes(config.disk_warn_bytes);
    let fail = iec_bytes(config.disk_fail_bytes);
    let msg = format!("{scope}: {have} free (warn {warn}, fail {fail})");
    global_report(CHECK_DISK, status, msg, remedy)
}

/// (d) NETWORK: reads the EXISTING bridge-core `NetProbe` through the [`NetReading`] its venue half
/// took off the probe handle (this module must never grow a second probe). A probe with no
/// completed round WARNs (`internet_up` starts optimistically `true`, which is not a
/// measurement); a measured-down probe is a GLOBAL FAIL.
///
/// `expected`: `crate::startup` spawns a probe only when a venue would go live.
/// Absent-and-expected WARNs; absent-and-not-expected is [`PreflightCheckStatus::NotApplicable`] (nothing
/// will place an order) — collapsing the two made every credential-free start WARN.
#[must_use]
pub fn check_network(net: Option<&NetReading>, expected: bool) -> PreflightCheckReport {
    let (status, msg, remedy) = match net {
        None if !expected => (
            PreflightCheckStatus::NotApplicable,
            "no venue would go live, so no order path depends on connectivity".to_string(),
            "",
        ),
        None => {
            let msg = "no NetProbe wired".to_string();
            (PreflightCheckStatus::Warn, msg, REMEDY_NETWORK_UNKNOWN)
        }
        Some(NetReading::Unknown) => {
            let msg = "NetProbe has not completed a round yet".to_string();
            (PreflightCheckStatus::Warn, msg, REMEDY_NETWORK_UNKNOWN)
        }
        Some(NetReading::Alive { rounds: Some(rounds) }) => {
            let msg = format!("internet up after {rounds} probe round(s)");
            (PreflightCheckStatus::Pass, msg, "")
        }
        Some(NetReading::Alive { rounds: None }) => {
            let msg =
                "internet up (the gatewayhub did not say after how many probe rounds)".to_string();
            (PreflightCheckStatus::Pass, msg, "")
        }
        Some(NetReading::Dead { since_ms }) => {
            let msg = format!("internet DOWN since epoch ms {since_ms}");
            (PreflightCheckStatus::Fail, msg, REMEDY_NETWORK)
        }
    };
    global_report(CHECK_NETWORK, status, msg, remedy)
}
