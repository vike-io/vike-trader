//! **What a preflight probe of the venue half answers**: the outcomes of a venue clock read, a
//! credential read and the network reading, as the venue half's own values (wave 2, W9b).
//!
//! These are the twins of `vike-tradehub-core`'s `preflight::ServerTimeGap`, `preflight::CredentialGap`
//! and `NetReading`, which the go/no-go report is written against and which stay in that crate. The
//! gatewayhub cannot name it, so the probes answer in these and `BridgeTrade` maps them onto the
//! wire's `ClockLeg`, `CredentialLeg` and `NetReading` (`vike_gatewayhub_client::proto`). The
//! variants and their meanings are the originals': the four outcomes of the clock leg (module doc of
//! `crates/vike-tradehub-core/src/preflight.rs`) and the "refused versus silent" split of the
//! credential leg, which is what lets only a REFUSAL stop a venue.

use vike_gatewayhub_client::proto::{
    ClockLeg, CredentialLeg, LegResult, NetReading as WireNetReading,
};

/// Why a clock read produced no number (one variant per outcome ②-④): different facts, never one
/// row. Declared variants take `&'static str`, so a runtime failure cannot fake a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerTimeGap {
    /// ② The venue PUBLISHES a clock and this attempt got none. Carries the venue's error text,
    /// never a URL or a credential.
    Unreachable(String),
    /// ③ DECLARED, nothing at stake: no clock leg, with the row's own reason.
    NotChecked(&'static str),
    /// ④ DECLARED, ORDERS at stake: no clock leg, but the venue's auth binds the clock into the
    /// order path — an unmeasured HAZARD.
    UnmeasuredRisk {
        /// Why there is no leg (the row's own sentence).
        reason: &'static str,
        /// What a drifted clock costs AT THIS VENUE, in that venue's own terms.
        at_stake: &'static str,
    },
}

/// Why a credential probe failed — only "refused us" may stop a venue; "never heard back" never does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialGap {
    /// The venue ANSWERED inside the bound and refused (or a definite transport error). Venue error
    /// text, never a key or secret.
    Rejected(String),
    /// No answer inside `CREDENTIAL_PROBE_TIMEOUT`. NOT evidence about the credentials.
    Unanswered {
        /// How long the thread actually waited, in ms.
        waited_ms: u64,
        /// What the wait was on, in operator language. Never contains a secret.
        detail: String,
    },
}

impl From<String> for CredentialGap {
    /// A bare error string is the CONFIRMED half; only the bounded runner in `startup` (which owns the
    /// clock the wait is measured on) constructs [`CredentialGap::Unanswered`].
    fn from(e: String) -> Self {
        CredentialGap::Rejected(e)
    }
}

/// The network leg's reading of a probe handle: three states and their numbers, so the report names
/// no bridge type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetReading {
    /// No round has completed: the handle's `internet_up` is still the optimistic initial value, which
    /// is not a measurement.
    Unknown,
    /// The last round observed the internet up, after `rounds` completed rounds.
    Alive {
        /// Completed probe rounds.
        rounds: u64,
    },
    /// The last round observed the internet down.
    Dead {
        /// Epoch ms the flag last went down.
        since_ms: i64,
    },
}

// ---- The wire's spelling of a probe outcome -------------------------------------------------------------

/// **The ONE mapping from a venue clock read to the wire's [`LegResult::Clock`]** (`BridgeTrade` and the
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
) -> LegResult {
    let (leg, reason, at_stake) = match read {
        Ok(server_ms) => (ClockLeg::ServerTime { server_ms, local_ms, rtt_ms }, None, None),
        Err(ServerTimeGap::Unreachable(why)) => (ClockLeg::Unreachable(why), None, None),
        Err(ServerTimeGap::NotChecked(reason)) => {
            (ClockLeg::NotChecked, Some(reason.to_string()), None)
        }
        Err(ServerTimeGap::UnmeasuredRisk { reason, at_stake }) => {
            (ClockLeg::UnmeasuredRisk, Some(reason.to_string()), Some(at_stake.to_string()))
        }
    };
    LegResult::Clock { venue: venue.to_string(), leg, reason, at_stake }
}

/// The wire's [`LegResult::Credential`] for one account's probe outcome (`label` is the account label's
/// text; `None` is the default account).
#[must_use]
pub fn credential_result(
    venue: &str,
    label: Option<&str>,
    read: Result<(), CredentialGap>,
) -> LegResult {
    let leg = match read {
        Ok(()) => CredentialLeg::Accepted,
        Err(CredentialGap::Rejected(why)) => CredentialLeg::Rejected(why),
        Err(CredentialGap::Unanswered { waited_ms, detail }) => {
            CredentialLeg::Unanswered { waited_ms, detail }
        }
    };
    LegResult::Credential { venue: venue.to_string(), label: label.map(str::to_string), leg }
}

/// The wire's [`LegResult::Network`]: the word, and the numbers the report prints (the round count of an
/// `Alive` reading, the down-since stamp of a `Dead` one).
#[must_use]
pub fn network_result(reading: NetReading) -> LegResult {
    match reading {
        NetReading::Unknown => {
            LegResult::Network { reading: WireNetReading::Unknown, rounds: None, since_ms: None }
        }
        NetReading::Alive { rounds } => LegResult::Network {
            reading: WireNetReading::Alive,
            rounds: Some(rounds),
            since_ms: None,
        },
        NetReading::Dead { since_ms } => LegResult::Network {
            reading: WireNetReading::Dead(format!(
                "the network probe saw the internet down (since epoch ms {since_ms})"
            )),
            rounds: None,
            since_ms: Some(since_ms),
        },
    }
}
