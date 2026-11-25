//! **The preflight's venue legs as the gatewayhub measured them, and the fold's reads over them.**
//!
//! The port splits the preflight at the unit `PreflightProbes` already has (`preflight_plan`, then one
//! read per clock, per credential and one network reading), and so does the wire: `PreflightQuery`
//! names a [`PreflightStep`] and the daemon only MEASURES, one read per question. The SCHEDULE stays
//! the keep half's `run_preflight`: it re-reads a clock while the reading's ±rtt/2 band straddles a
//! threshold (the tightest round trip wins) and interleaves the clock and credential budgets between
//! reads, and each of its reads is one question here. This file is the pure half: the answers of a
//! question, read as the fold's four outcomes.
//!
//! Everything the gatewayhub could not say is a WARN row in the report, **never a green one**: a
//! gatewayhub that does not answer makes every leg `did not answer`, and a venue it returned no leg
//! for is `Unreachable`.
//!
//! # What crosses
//!
//! * **The clock.** A reading is the venue's own stamp. The daemon and the tradehub share a host (the
//!   gatewayhub binds loopback only), and the daemon read the venue inside the bracket the fold
//!   takes around the question, so the fold's midpoint correction, its round trip and its resample
//!   judge the reading exactly as they judged an in-process one. The round trip the fold prints is
//!   the question's (the daemon's venue read plus a loopback dial), so it reads a few ms wider.
//! * **The thresholds** are the tradehub's judgement over the venue's declared [`ClockRisk`]
//!   (only a signed-timestamp venue may FAIL); the remedy sentence is the bridge's own, carried.
//! * **Declared gaps** (`NotChecked`, `UnmeasuredRisk`) carry the row's own reason (and what a drifted
//!   clock costs), which the report prints.
//! * **The network reading** carries its round count and its down-since stamp.
//! * **Reasons are `&'static str` in the fold** (a runtime failure must not be able to fake a
//!   declaration), so a reason that crossed the wire is interned once per distinct text, at most
//!   [`MAX_INTERNED`] of them; the roster's declared sentences are a few dozen.
//!
//! The report never prints an `Identity` leg: the in-process preflight never produced one (the
//! credential probe records the book at the daemon, as it did in process).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use vike_gatewayhub_client::proto::{
    CredentialGapWire, NetReadingWire, PreflightLegWire, PreflightPlanWire, ServerTimeGapWire,
};
use vike_model::{ClockRisk, CredentialGap, ServerTimeGap};
use vike_tradehub_core::preflight::{
    CANARY_CLOCK_WARN_MS, DEFAULT_CLOCK_FAIL_MS, DEFAULT_CLOCK_WARN_MS, PreflightClockPolicy,
};

/// What this backend knows about its preflight plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Asking {
    /// The gatewayhub answered the plan: each read is asked of it, one at a time.
    Up,
    /// There is no gatewayhub to ask (or its plan did not come): every read says why none.
    Failed(String),
}

/// The thresholds the tradehub judges a declared clock risk by, with the bridge's remedy sentence.
/// Only [`ClockRisk::SignedTimestamp`] carries a FAIL (it can reject an order over drift); a
/// nonce-window venue's cliff is a day away and a no-timestamp venue's auth stamps nothing, so for
/// them a FAIL would stop a venue over a fault it does not have; those warn later (the canary line).
pub(super) fn clock_policy_of(class: ClockRisk, remedy: &str) -> PreflightClockPolicy {
    let remedy = intern(remedy);
    match class {
        ClockRisk::SignedTimestamp => PreflightClockPolicy {
            warn_ms: DEFAULT_CLOCK_WARN_MS,
            fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
            remedy,
        },
        ClockRisk::NonceWindow | ClockRisk::NoTimestamp => {
            PreflightClockPolicy { warn_ms: CANARY_CLOCK_WARN_MS, fail_ms: None, remedy }
        }
    }
}

/// The per-venue clock policies of a plan.
pub(super) fn policies_of(lists: &PreflightPlanWire) -> HashMap<String, PreflightClockPolicy> {
    lists
        .clock_risks
        .iter()
        .map(|(venue, info)| (venue.clone(), clock_policy_of(info.class, &info.remedy)))
        .collect()
}

/// The clock read of a query that got no answer.
pub(super) fn clock_unanswered(why: &str) -> ServerTimeGap {
    ServerTimeGap::Unreachable(format!("the gatewayhub did not answer the preflight query: {why}"))
}

/// The credential read of a query that got no answer: a silence, which proves nothing about the keys.
pub(super) fn credential_unanswered(why: &str) -> CredentialGap {
    CredentialGap::Unanswered {
        waited_ms: 0,
        detail: format!("the gatewayhub did not answer the preflight query: {why}"),
    }
}

/// The text of a declared gap whose reason the gatewayhub did not state.
const NO_REASON: &str = "the venue declares no clock endpoint (the gatewayhub stated no reason)";
/// The text of a declared unmeasured risk whose cost the gatewayhub did not state.
const NO_AT_STAKE: &str =
    "its auth binds the clock into the order path, so a drifted clock can cost an order";

/// One venue's clock read, as the fold's four outcomes, from the legs one `Clock` question answered.
pub(super) fn server_time_of(venue: &str, legs: &[PreflightLegWire]) -> Result<i64, ServerTimeGap> {
    let found = legs.iter().find_map(|leg| match leg {
        PreflightLegWire::Clock { venue: v, leg, reason, at_stake } if v == venue => {
            Some((leg, reason, at_stake))
        }
        _ => None,
    });
    match found {
        Some((ServerTimeGapWire::ServerTime { server_ms, .. }, _, _)) => Ok(*server_ms),
        Some((ServerTimeGapWire::Unreachable(why), _, _)) => {
            Err(ServerTimeGap::Unreachable(why.clone()))
        }
        Some((ServerTimeGapWire::NotChecked, reason, _)) => {
            Err(ServerTimeGap::NotChecked(intern(reason.as_deref().unwrap_or(NO_REASON))))
        }
        Some((ServerTimeGapWire::UnmeasuredRisk, reason, at_stake)) => {
            Err(ServerTimeGap::UnmeasuredRisk {
                reason: intern(reason.as_deref().unwrap_or(NO_REASON)),
                at_stake: intern(at_stake.as_deref().unwrap_or(NO_AT_STAKE)),
            })
        }
        None => Err(ServerTimeGap::Unreachable(format!(
            "the gatewayhub returned no clock reading for {venue}"
        ))),
    }
}

/// One venue's credential read: the worst of its accounts' legs (a refusal beats a silence beats an
/// acceptance), so a venue with two accounts is stopped when ANY of them is refused.
pub(super) fn authed_read_of(venue: &str, legs: &[PreflightLegWire]) -> Result<(), CredentialGap> {
    let mut seen = false;
    let mut silence: Option<CredentialGap> = None;
    for leg in legs {
        let PreflightLegWire::Credential { venue: v, leg, .. } = leg else { continue };
        if v != venue {
            continue;
        }
        seen = true;
        match leg {
            CredentialGapWire::Accepted => {}
            CredentialGapWire::Rejected(why) => return Err(CredentialGap::Rejected(why.clone())),
            CredentialGapWire::Unanswered { waited_ms, detail } => {
                silence.get_or_insert(CredentialGap::Unanswered {
                    waited_ms: *waited_ms,
                    detail: detail.clone(),
                });
            }
        }
    }
    if !seen {
        return Err(CredentialGap::Rejected(format!(
            "no credential probe was planned for {venue}"
        )));
    }
    silence.map_or(Ok(()), Err)
}

/// The network reading from the legs a `Network` question answered. Absence (nothing asked, or the
/// query failed) is `Unknown`, which the check renders as a WARN, never a pass. `now_ms` stands in for
/// a down-since stamp a daemon did not give; an up reading without a round count stays without one.
pub(super) fn net_reading_of(legs: &[PreflightLegWire], now_ms: i64) -> vike_model::NetReading {
    use vike_model::NetReading as Fold;
    let found = legs.iter().find_map(|leg| match leg {
        PreflightLegWire::Network { reading, rounds, since_ms } => {
            Some((reading, rounds, since_ms))
        }
        _ => None,
    });
    match found {
        None | Some((NetReadingWire::Unknown, _, _)) => Fold::Unknown,
        Some((NetReadingWire::Alive, rounds, _)) => Fold::Alive { rounds: *rounds },
        Some((NetReadingWire::Dead(why), _, since_ms)) => {
            tracing::warn!(why = %why, "the gatewayhub's network probe reads the internet DOWN");
            Fold::Dead { since_ms: since_ms.unwrap_or(now_ms) }
        }
    }
}

/// How many distinct reason texts are kept for the process's lifetime.
const MAX_INTERNED: usize = 64;
/// The longest reason text kept, in bytes (a sentence, not a payload).
const MAX_REASON_BYTES: usize = 1_024;
/// What a reason reads as once [`MAX_INTERNED`] others have been kept.
const OVERFLOW: &str =
    "(the gatewayhub's reason is not shown: this process keeps at most 64 distinct ones)";

/// The reason texts kept so far.
static INTERNED: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// `text` as a `&'static str`, kept once per distinct text (see the module doc).
fn intern(text: &str) -> &'static str {
    intern_in(&INTERNED, MAX_INTERNED, text)
}

/// [`intern`] over a given table and cap. A text past [`MAX_REASON_BYTES`] is cut at a character boundary.
fn intern_in(table: &Mutex<Vec<&'static str>>, cap: usize, text: &str) -> &'static str {
    let mut end = text.len().min(MAX_REASON_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let text = &text[..end];
    let mut kept = table.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(known) = kept.iter().find(|known| **known == text) {
        return known;
    }
    if kept.len() >= cap {
        return OVERFLOW;
    }
    let leaked: &'static str = Box::leak(text.to_owned().into_boxed_str());
    kept.push(leaked);
    leaked
}

#[path = "legs_tests.rs"]
#[cfg(test)]
mod legs_tests;
