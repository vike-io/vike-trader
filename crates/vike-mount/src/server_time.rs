//! The per-venue **server-clock declaration table**: which venues the startup preflight reads a
//! clock from, which ones it does not, and — in every case — WHY, in a row that names the venue.
//!
//! # The defect this exists to close
//!
//! Before this module, [`crate::startup`]'s clock dispatch was a two-arm match with a catch-all:
//! binance, then `other => Err("no public server-time endpoint wired for {other}")`. Two facts came
//! out of it wearing the same clothes — **"this adapter has no endpoint wired"** (a permanent
//! property of OUR code) and **"the venue did not answer"** (a live problem that should stop an
//! operator) — and [`crate::preflight::check_clock_skew`] rendered both as the same WARN. The first
//! live `vike-tradehub` mount printed exactly that, twice:
//!
//! ```text
//! [WARN] clock_skew (bybit): server time unavailable: no public server-time endpoint wired for bybit
//! [WARN] clock_skew (okx):   server time unavailable: no public server-time endpoint wired for okx
//! ```
//!
//! …on two venues that both publish a keyless server clock and both reject a signed order whose
//! timestamp is outside their recv window. A warning that fires forever on a healthy box is a
//! warning nobody reads, which is how the check that exists to catch a drifted clock came to be
//! skipped on most of the roster behind a line that looks like noise.
//!
//! # The table IS the answer, and the roster gates it
//!
//! Every venue in [`vike_model::VENUES`] has ONE clock row — its bridge's
//! `VenueDeclaration::clock` — the same covered/deferred partition idiom as
//! `crates/vike-bridge-core/tests/bridge_conformance.rs`'s `covered_bridges()` / `DEFERRED` split.
//! `crates/vike-tradehub/tests/mount_roster/server_time.rs`'s `clock_sources_cover_the_roster`
//! iterates the canonical roster through the real registry, so **adding a venue fails that test
//! until the venue is classified**, and no prose (here or anywhere) states how many venues are
//! wired: the rows are the count. This module's `CLOCK_SOURCES` table held one row per venue until
//! the venue mount contract moved each row into its bridge's `VenueDeclaration::clock`
//! (docs/decisions/0096); [`clock_decl`] reads the registry.
//!
//! ⚠ That table's `Wired` row carried its reader as a function pointer, so a "wired" row could not
//! exist without the code that read it. A contract row's reader is its bridge's
//! `VenueMount::server_time_ms`, which has a default — `Err("no clock leg")` — so a bridge that
//! declares `Wired` and does not override it is not refused at compile time: its read answers
//! outcome ② below, a failed read that WARNs, rather than passing in silence.
//!
//! # FOUR outcomes, not two
//!
//! [`venue_server_time_ms`] returns `Result<i64, ServerTimeGap>`, and the error variants are the
//! whole point (see [`crate::preflight::ServerTimeGap`]):
//!
//! - `Ok(ms)` — ① a measurement. The preflight compares it against that venue's thresholds.
//! - `Err(Unreachable)` — ② this venue publishes a clock we read, and the read FAILED. A real
//!   problem, reported as such.
//! - `Err(NotChecked)` — ③ a DECLARED row: no clock leg for this venue, plus the reason, at a venue
//!   where a drifted clock costs NOTHING. Rendered
//!   [`CheckStatus::NotApplicable`](crate::preflight::CheckStatus::NotApplicable), never a warning,
//!   because it is a permanent property rather than a fault.
//! - `Err(UnmeasuredRisk)` — ④ a DECLARED row at a venue whose auth **does** bind the clock into
//!   the order path. The gap is OURS and it can cost an order, so it WARNs. Exactly one row is ④
//!   today — polymarket's, which its bridge declares since the venue mount contract
//!   (`crates/bridges/polymarket/src/exec_plane/mount.rs`'s `PolymarketVenueMount`) — and
//!   collapsing it into ③ was a real defect: see that declaration's own comment.
//!
//! # A clock check does not mean the same thing at every venue
//!
//! [`ClockRisk`] is the second half of each wired row, and it exists because the remedy text used
//! to assert "signed requests stamp it against a 5000 ms recvWindow" for whatever venue the row
//! belonged to. That is TRUE on binance/bybit/okx/aster and FALSE on deribit (whose
//! `client_credentials` auth carries no timestamp and no nonce), on ig (session tokens), and
//! nearly-false on hyperliquid (the clock feeds a nonce whose window is measured in DAYS).
//!
//! It now decides **thresholds** as well as words, through [`clock_policy_of`], because a single
//! global threshold was measurably wrong — see the re-derivation in [`crate::preflight`]'s
//! module doc. The two halves in one sentence: only a venue that REJECTS an order over drift may
//! FAIL (and so degrade itself to paper), and a venue that cannot is judged against the looser
//! host-health threshold that its own server clock's wander demands.
//!
//! # Tier discipline: measure the clock that will judge us
//!
//! A contract row's bridge reads its clock from the SAME demo/mainnet host its mount binds, by the
//! rule that mount uses (deribit's, the testnet host every exec spawn site binds; aster's, the
//! `mountable_tier_for_account` Live-then-Demo chain its mount calls; hyperliquid's, the
//! `Env::for_ceiling` mapping of decision 0095's ceiling). The legacy fetchers `CLOCK_SOURCES`
//! held followed the same rule until the last of them moved into its bridge. This is not tidiness:
//! hyperliquid's testnet and mainnet clocks measured **-250 ms and -211 ms in one paired sample
//! from the CI box** (2026-08-09, back to back inside 600 ms), so a check pointed at the wrong tier
//! measures a clock that will never judge our orders.
//!
//! # Every read is BOUNDED, and the parse is fixture-tested
//!
//! Two properties this module owns rather than inherits:
//!
//! - **[`CLOCK_READ_TIMEOUT`]**, not the shared 30 s agent. A preflight that can park a mount for
//!   minutes is worse than the warning it replaces — [`crate::preflight`]'s "the clock leg is
//!   BOUNDED" section carries the arithmetic and the leg-wide budget that bounds the rest.
//! - **The parse is separated from the fetch**, and each one is tested against a REAL captured
//!   body. Every such parse this module held lives in its venue's bridge now: `parse_server_time`
//!   beside that bridge's `VenueMount::server_time_ms`, its fixture under the bridge's own
//!   `tests/fixtures/server_time/`. The units genuinely differ from venue to venue, and a units
//!   mix-up reports a thousand-fold skew while looking authoritative. Before those fixtures the
//!   only test that could catch a renamed field was the `#[ignore]`d live smoke. (The three worst
//!   unit traps moved with their captured bodies into their bridges: deribit's, a bare `i64` beside
//!   MICROSECOND fields; bybit's, a ns STRING next to the ms NUMBER we want; okx's, a STRING inside
//!   an ARRAY — `crates/bridges/deribit/src/mount.rs`'s `DeribitVenueMount`,
//!   `crates/bridges/bybit/src/mount.rs`'s `BybitVenueMount`,
//!   `crates/bridges/okx/src/mount.rs`'s `OkxVenueMount`.)

use std::collections::HashMap;
use std::time::Duration;

use vike_bridge_core::venue_mount::{ClockDecl, ClockRisk};

use crate::preflight::{
    CANARY_CLOCK_WARN_MS, ClockPolicy, DEFAULT_CLOCK_FAIL_MS, DEFAULT_CLOCK_WARN_MS, ServerTimeGap,
};

/// The per-read ceiling for EVERY clock fetch below — deliberately NOT the shared
/// `vike_bridge_core::http::blocking_agent`'s 30 s global timeout, which is sized for an order
/// round trip that must not be abandoned, not for a pre-mount canary nobody is waiting on.
///
/// **3 s = 4x the slowest healthy read ever measured here** (binance's demo host, 757 ms total
/// including DNS + TLS on a cold connection; the CI box, 2026-08-09 — the same run is pinned in
/// [`crate::preflight`]'s `MEASURED_HEALTHY_READINGS`). A read that overruns it produces outcome ②
/// (a WARN that degrades nothing), so the cost of being too tight is one noisy line, while the cost
/// of the 30 s agent was a mount that could stall for minutes.
pub const CLOCK_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// The thresholds a reading at a venue with this risk is judged against — was `ClockRisk::policy`
/// until `ClockRisk` moved into `vike_bridge_core::venue_mount` (it cannot name `ClockPolicy`). It
/// is the fix for a global threshold that MEASUREMENT falsified (see [`crate::preflight`]'s "the
/// thresholds are per-venue" section for the readings and the derivation).
///
/// Two facts, both of them properties of the venue rather than of the host:
///
/// - **Only [`ClockRisk::SignedTimestamp`] carries a FAIL.** A clock FAIL degrades its venue to
///   paper ([`crate::preflight::PreflightReport::venue_disposition`]), which is defensible
///   exactly where drift rejects orders and indefensible everywhere else — deribit and ig
///   cannot reject an order over a clock at all, and hyperliquid's cliff is a DAY away, so a
///   `fail_ms` there would demote a venue for a fault it does not have.
/// - **The canary venues warn later**, at [`CANARY_CLOCK_WARN_MS`], because what we measure at
///   them is dominated by THEIR clock, not ours: hyperliquid's testnet node read between -220
///   and -424 ms across 40 samples from an NTP-disciplined the CI box inside three minutes, against
///   the 12-37 ms the four CEX venues read in the same window.
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

/// A venue's clock row as the contract declares it, from its registry row — or `None` for a venue
/// the registry does not carry.
#[must_use]
pub fn clock_decl(registry: &'static [crate::VenueRow], venue: &str) -> Option<ClockDecl> {
    match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => Some(row.declaration().clock),
        Some(crate::VenueRow::FeatureAbsent { .. }) => Some(crate::registry::ABSENT_CLOCK),
        None => None,
    }
}

/// The thresholds and remediation text a MEASURED reading at `venue` is judged against —
/// [`clock_policy_of`] over the venue's own declared risk, so the report can never claim a recv
/// window a venue does not have, and can never demote a venue over a fault its auth cannot suffer.
/// `None` for an unwired or unknown venue (which produces no measurement, hence nothing to judge).
#[must_use]
pub fn clock_policy(registry: &'static [crate::VenueRow], venue: &str) -> Option<ClockPolicy> {
    match clock_decl(registry, venue) {
        Some(ClockDecl::Wired { risk, .. }) => Some(clock_policy_of(risk)),
        _ => None,
    }
}

/// The clock leg's venue read: `venue`'s own server time as an ABSOLUTE epoch-ms stamp (which is
/// what [`crate::preflight::PreflightProbes::venue_server_time_ms`] wants — not the OFFSET
/// `BinanceSpotRest::server_time_offset` returns).
///
/// The four outcomes are the module doc's ①/②/③/④. A contract row reads its clock through its
/// bridge's `server_time_ms`, on the DEFAULT account's inputs under this venue's ceiling; a
/// `FeatureAbsent` row is declared `NotChecked`. A venue the registry does not carry is
/// [`ServerTimeGap::Unreachable`], not a declaration: an unknown venue string is a caller bug, and
/// declaring it "nothing to check" would hide it.
///
/// With no policy: `venue_server_time_ms_under_policy` is the read the startup preflight makes.
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
/// ⚠ A contract row's clock is read on the SAME inputs its mount is handed
/// ([`crate::contract::inputs_for`] and [`crate::contract::process_facts`]): the policy's venue
/// settings and account table, and the process facts. A bridge whose clock host depends on a
/// setting or on the state directory would otherwise be measured at one host and mounted at
/// another.
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

// hyperliquid's clock read, `hyperliquid_time` (keyless `POST /info {"type":"exchangeStatus"}` on
// the tier the ceiling resolves), and its parse `parse_hyperliquid_time` moved into that bridge
// with the venue mount contract (docs/decisions/0096): `crates/bridges/hyperliquid/src/mount.rs`'s
// `server_time_ms`, with the parse pinned against the captured body beside it.

#[path = "server_time_tests.rs"]
#[cfg(test)]
mod server_time_tests;
