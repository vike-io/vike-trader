//! The per-venue **server-clock declaration table**: which venues the startup preflight reads a
//! clock from, which it does not, and WHY, in a row that names the venue.
//!
//! Why: the old two-arm dispatch rendered "no endpoint wired" (a permanent property of OUR code)
//! and "the venue did not answer" (a live fault) as the same WARN, so the first live mount warned
//! forever on bybit and okx, two venues that publish a keyless clock and reject a drifted signed
//! order: a warning nobody reads, and the drift check skipped across most of the roster.
//!
//! # The table, gated by the roster
//! Every venue in [`vike_model::VENUES`] has ONE clock row, its bridge's `VenueDeclaration::clock`
//! (the covered/deferred idiom of `crates/vike-bridge-core/tests/bridge_conformance.rs`'s
//! `covered_bridges()` / `DEFERRED`); [`clock_decl`] reads it from the registry. The rows moved out
//! of this module's former `CLOCK_SOURCES` (docs/decisions/0096).
//! `crates/vike-tradehub/tests/mount_roster/server_time.rs`'s `clock_sources_cover_the_roster`
//! fails until a new venue is classified; no prose states the count.
//!
//! ⚠ A row's reader is `VenueMount::server_time_ms`, whose default is `Err("no clock leg")`: a
//! bridge declaring `Wired` without overriding it is not refused at compile time; it answers ② (a
//! WARN).
//!
//! # FOUR outcomes ([`venue_server_time_ms`]; variants: [`crate::preflight::ServerTimeGap`])
//! - `Ok(ms)` — ① a measurement, judged against that venue's thresholds.
//! - `Err(Unreachable)` — ② the venue publishes a clock we read, and the read FAILED.
//! - `Err(NotChecked)` — ③ DECLARED: no clock leg, with the reason, where drift costs NOTHING.
//!   Rendered [`CheckStatus::NotApplicable`](crate::preflight::CheckStatus::NotApplicable), never a
//!   warning.
//! - `Err(UnmeasuredRisk)` — ④ DECLARED at a venue whose auth **does** bind the clock into the
//!   order path: our gap can cost an order, so it WARNs. Today only polymarket
//!   (`crates/bridges/polymarket/src/exec_plane/mount.rs`'s `PolymarketVenueMount`; folding it
//!   into ③ was a real defect, see that declaration's comment).
//!
//! # A clock check means different things per venue
//! [`ClockRisk`] exists because a 5000 ms recvWindow is TRUE on binance/bybit/okx/aster, FALSE on
//! deribit (`client_credentials`, no timestamp or nonce) and ig (session tokens), nearly-false on
//! hyperliquid (a nonce window measured in DAYS). Through [`clock_policy_of`] it decides thresholds
//! too: only a venue that REJECTS orders over drift may FAIL (degrading itself to paper); the rest
//! get the looser host-health threshold (derivation: [`crate::preflight`]'s module doc).
//!
//! # Tier discipline: measure the clock that will judge us
//! A bridge reads its clock from the SAME demo/mainnet host its mount binds, by the mount's rule
//! (deribit: the testnet host; aster: `mountable_tier_for_account`; hyperliquid: `Env::for_ceiling`
//! of decision 0095's ceiling). Hyperliquid's testnet and mainnet clocks measured **-250 ms and
//! -211 ms in one paired sample from the CI box** (2026-08-09, inside 600 ms): the wrong tier measures a
//! clock that will never judge our orders.
//!
//! # Every read is BOUNDED, and the parse is fixture-tested
//! - **[`CLOCK_READ_TIMEOUT`]**, not the shared 30 s agent (arithmetic: [`crate::preflight`]'s "the
//!   clock leg is BOUNDED" section).
//! - **Parse separated from fetch**, each bridge's `parse_server_time` tested against a REAL
//!   captured body under its `tests/fixtures/server_time/`: a units mix-up reports a thousand-fold
//!   skew while looking authoritative. The worst unit traps: deribit's bare `i64` beside MICROSECOND
//!   fields, bybit's ns STRING beside the ms NUMBER, okx's STRING inside an ARRAY
//!   (`crates/bridges/deribit/src/mount.rs`'s `DeribitVenueMount`,
//!   `crates/bridges/bybit/src/mount.rs`'s `BybitVenueMount`,
//!   `crates/bridges/okx/src/mount.rs`'s `OkxVenueMount`).

use std::collections::HashMap;
use std::time::Duration;

use vike_bridge_core::venue_mount::{ClockDecl, ClockRisk};

use crate::preflight::{
    CANARY_CLOCK_WARN_MS, ClockPolicy, DEFAULT_CLOCK_FAIL_MS, DEFAULT_CLOCK_WARN_MS, ServerTimeGap,
};

/// The per-read ceiling for EVERY clock fetch, NOT `vike_bridge_core::http::blocking_agent`'s 30 s
/// (sized for an order round trip, not a pre-mount canary): the 30 s agent could stall a mount for
/// minutes.
///
/// **3 s = 4x the slowest healthy read ever measured here** (binance demo, 757 ms with cold DNS +
/// TLS; the CI box, 2026-08-09; pinned in [`crate::preflight`]'s `MEASURED_HEALTHY_READINGS`). An
/// overrun is outcome ② (a WARN that degrades nothing).
pub const CLOCK_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// The thresholds a reading at a venue with this risk is judged against (a global threshold was
/// falsified by MEASUREMENT: [`crate::preflight`]'s "the thresholds are per-venue" section). Both
/// rules are properties of the venue, not the host:
///
/// - **Only [`ClockRisk::SignedTimestamp`] carries a FAIL**, which degrades the venue to paper
///   ([`crate::preflight::PreflightReport::venue_disposition`]): deribit and ig cannot reject an
///   order over a clock and hyperliquid's cliff is a DAY away, so a `fail_ms` there demotes a
///   venue for a fault it does not have.
/// - **The canary venues warn later** ([`CANARY_CLOCK_WARN_MS`]): we measure THEIR clock there.
///   Hyperliquid testnet read -220 to -424 ms across 40 samples from an NTP-disciplined the CI box in
///   three minutes, against 12-37 ms for the four CEX venues in the same window.
#[must_use]
pub fn clock_policy_of(risk: ClockRisk) -> ClockPolicy {
    match risk {
        ClockRisk::SignedTimestamp => ClockPolicy {
            warn_ms: DEFAULT_CLOCK_WARN_MS,
            fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
            remedy: risk.remedy(),
        },
        ClockRisk::NonceWindow | ClockRisk::NoTimestamp => {
            ClockPolicy { warn_ms: CANARY_CLOCK_WARN_MS, fail_ms: None, remedy: risk.remedy() }
        }
    }
}

/// A venue's declared clock row; `None` for a venue the registry does not carry.
#[must_use]
pub fn clock_decl(registry: &'static [crate::VenueRow], venue: &str) -> Option<ClockDecl> {
    match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => Some(row.declaration().clock),
        Some(crate::VenueRow::FeatureAbsent { .. }) => Some(crate::registry::ABSENT_CLOCK),
        None => None,
    }
}

/// [`clock_policy_of`] over `venue`'s own declared risk, so the report never claims a recv window
/// the venue lacks nor demotes it over a fault its auth cannot suffer. `None` for an unwired or
/// unknown venue (nothing measured to judge).
#[must_use]
pub fn clock_policy(registry: &'static [crate::VenueRow], venue: &str) -> Option<ClockPolicy> {
    match clock_decl(registry, venue) {
        Some(ClockDecl::Wired { risk, .. }) => Some(clock_policy_of(risk)),
        _ => None,
    }
}

/// `venue`'s server time as an ABSOLUTE epoch-ms stamp (what
/// [`crate::preflight::PreflightProbes::venue_server_time_ms`] wants, not an offset); the module
/// doc's outcomes ①-④. Read through the bridge's `server_time_ms` on the DEFAULT account's inputs;
/// a `FeatureAbsent` row is `NotChecked`. An unknown venue is [`ServerTimeGap::Unreachable`]: a
/// caller bug that "nothing to check" would hide. With no policy; the startup preflight reads
/// `venue_server_time_ms_under_policy`.
pub fn venue_server_time_ms(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    live_permitted: bool,
) -> Result<i64, ServerTimeGap> {
    venue_server_time_ms_under_policy(registry, venue, vars, live_permitted, None)
}

/// [`venue_server_time_ms`] under this deployment's policy — the read the startup preflight makes.
///
/// ⚠ The clock is read on the SAME inputs the mount is handed ([`crate::contract::inputs_for`],
/// [`crate::contract::process_facts`]); else a bridge whose host depends on a setting or the state
/// directory is measured at one host and mounted at another.
pub(crate) fn venue_server_time_ms_under_policy(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    live_permitted: bool,
    policy: Option<&crate::MountPolicy>,
) -> Result<i64, ServerTimeGap> {
    let row = match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => *row,
        Some(crate::VenueRow::FeatureAbsent { .. }) => {
            return Err(ServerTimeGap::NotChecked(crate::registry::ABSENT_CLOCK_REASON));
        }
        None => {
            return Err(ServerTimeGap::Unreachable(format!("{venue} is not on the venue roster")));
        }
    };
    match row.declaration().clock {
        ClockDecl::Wired { .. } => {
            let account = vike_model::accounts::account_keys::AccountLabel::Default;
            let process = crate::contract::process_facts();
            let inputs =
                crate::contract::inputs_for(row, &account, vars, live_permitted, policy, &process);
            row.server_time_ms(&inputs, CLOCK_READ_TIMEOUT).map_err(ServerTimeGap::Unreachable)
        }
        ClockDecl::NotWired { reason, unmeasured_risk: None } => {
            Err(ServerTimeGap::NotChecked(reason))
        }
        ClockDecl::NotWired { reason, unmeasured_risk: Some(at_stake) } => {
            Err(ServerTimeGap::UnmeasuredRisk { reason, at_stake })
        }
    }
}

// hyperliquid's `hyperliquid_time` now lives at `crates/bridges/hyperliquid/src/mount.rs`'s
// `server_time_ms`.

#[path = "server_time_tests.rs"]
#[cfg(test)]
mod server_time_tests;
