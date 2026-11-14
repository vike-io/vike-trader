//! **What a preflight probe of the venue half answers, on the wire**: the ONE mapping of a venue clock
//! read, a credential read and the network reading onto the lease wire's `ServerTimeGapWire`, `CredentialGapWire`
//! and `NetReading` (`vike_gatewayhub_client::proto`).
//!
//! The probe outcomes themselves are `vike_model::{ServerTimeGap, CredentialGap, NetReading}`, the
//! same values the tradehub's go/no-go report is written against
//! (`crates/vike-model/src/venues/preflight.rs`): the four outcomes of the clock leg and the "refused
//! versus silent" split of the credential leg, which is what lets only a REFUSAL stop a venue.

use vike_gatewayhub_client::proto::{
    CredentialGapWire, NetReadingWire, PreflightLegWire, ServerTimeGapWire,
};
use vike_model::{CredentialGap, NetReading, ServerTimeGap};

// ---- The wire's spelling of a probe outcome -------------------------------------------------------------

/// **The ONE mapping from a venue clock read to the wire's [`PreflightLegWire::Clock`]** (`TradeLaneBridge` and the
/// testkit's scripted lanes both answer through it, so a double cannot drift from the lane). A reading
/// carries the stamp as the venue gave it, the daemon's clock at its end and the round trip; a DECLARED gap
/// carries its own sentence (and, for an unmeasured risk, what a drifted clock costs), because the report
/// the tradehub prints states them verbatim.
#[must_use]
pub fn clock_result(
    venue: &str,
    read: Result<i64, ServerTimeGap>,
    local_ms: i64,
    rtt_ms: u64,
) -> PreflightLegWire {
    let (leg, reason, at_stake) = match read {
        Ok(server_ms) => {
            (ServerTimeGapWire::ServerTime { server_ms, local_ms, rtt_ms }, None, None)
        }
        Err(ServerTimeGap::Unreachable(why)) => (ServerTimeGapWire::Unreachable(why), None, None),
        Err(ServerTimeGap::NotChecked(reason)) => {
            (ServerTimeGapWire::NotChecked, Some(reason.to_string()), None)
        }
        Err(ServerTimeGap::UnmeasuredRisk { reason, at_stake }) => (
            ServerTimeGapWire::UnmeasuredRisk,
            Some(reason.to_string()),
            Some(at_stake.to_string()),
        ),
    };
    PreflightLegWire::Clock { venue: venue.to_string(), leg, reason, at_stake }
}

/// The wire's [`PreflightLegWire::Credential`] for one account's probe outcome (`label` is the account label's
/// text; `None` is the default account).
#[must_use]
pub fn credential_result(
    venue: &str,
    label: Option<&str>,
    read: Result<(), CredentialGap>,
) -> PreflightLegWire {
    let leg = match read {
        Ok(()) => CredentialGapWire::Accepted,
        Err(CredentialGap::Rejected(why)) => CredentialGapWire::Rejected(why),
        Err(CredentialGap::Unanswered { waited_ms, detail }) => {
            CredentialGapWire::Unanswered { waited_ms, detail }
        }
    };
    PreflightLegWire::Credential { venue: venue.to_string(), label: label.map(str::to_string), leg }
}

/// The wire's [`PreflightLegWire::Network`]: the word, and the numbers the report prints (the round count of an
/// `Alive` reading, the down-since stamp of a `Dead` one).
#[must_use]
pub fn network_result(reading: NetReading) -> PreflightLegWire {
    match reading {
        NetReading::Unknown => PreflightLegWire::Network {
            reading: NetReadingWire::Unknown,
            rounds: None,
            since_ms: None,
        },
        NetReading::Alive { rounds } => {
            PreflightLegWire::Network { reading: NetReadingWire::Alive, rounds, since_ms: None }
        }
        NetReading::Dead { since_ms } => PreflightLegWire::Network {
            reading: NetReadingWire::Dead(format!(
                "the network probe saw the internet down (since epoch ms {since_ms})"
            )),
            rounds: None,
            since_ms: Some(since_ms),
        },
    }
}
