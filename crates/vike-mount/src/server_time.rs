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
//! [`CLOCK_SOURCES`] carries ONE row per venue in [`vike_model::VENUES`] — the same
//! covered/deferred partition idiom as `crates/vike-bridge-core/tests/bridge_conformance.rs`'s
//! `covered_bridges()` / `DEFERRED` split. `clock_sources_cover_the_roster` iterates the canonical
//! roster, so **adding a venue fails this crate's tests until that venue is classified**, and no
//! prose (here or anywhere) states how many venues are wired: the rows are the count, and
//! [`ClockSource::Wired`] carries its own reader as a function pointer, so a "wired" row cannot
//! exist without the code that reads it.
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
//!   today (polymarket), and collapsing it into ③ was a real defect: see that row's own comment.
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
//! Every fetcher resolves the SAME demo/mainnet host its venue's `make_engine` arm binds — through
//! the same ceiling (decision 0095: `ceiling_selects_mainnet`/`Env::for_ceiling`), aster's
//! `mountable_tier_for_account` Live-then-Demo chain, deribit's hardcoded testnet. This is not
//! tidiness: hyperliquid's testnet
//! and mainnet clocks measured **-250 ms and -211 ms in one paired sample from the CI box** (2026-08-09,
//! back to back inside 600 ms), so a check pointed at the wrong tier measures a clock that will
//! never judge our orders.
//!
//! # Every read is BOUNDED, and the parse is fixture-tested
//!
//! Two properties this module owns rather than inherits:
//!
//! - **[`CLOCK_READ_TIMEOUT`]**, not the shared 30 s agent. A preflight that can park a mount for
//!   minutes is worse than the warning it replaces — [`crate::preflight`]'s "the clock leg is
//!   BOUNDED" section carries the arithmetic and the leg-wide budget that bounds the rest.
//! - **The parse is separated from the fetch** (`parse_*`), and each one is tested against a REAL
//!   captured body under `crates/vike-mount/tests/fixtures/server_time/`. The units genuinely
//!   differ — okx is a STRING inside an ARRAY, deribit a bare `i64` beside MICROSECOND fields,
//!   bybit a ns STRING next to the ms NUMBER we want — and a units mix-up reports a
//!   thousand-fold skew while looking authoritative. Before those fixtures the only test that
//!   could catch a renamed field was the `#[ignore]`d live smoke.

use std::collections::HashMap;
use std::time::Duration;

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_bridge_core::venue_mount::{ClockAuth, ClockDecl, ClockRisk};

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

/// One venue's clock read: the vars map and whether the ceiling permits the LIVE tier for this
/// venue's default account, in; an ABSOLUTE epoch-ms venue stamp out. `Err` is the venue's own
/// error text — never a URL, never a credential (see [`crate::preflight`]'s secrets note).
type ClockFetch = fn(&HashMap<String, String>, bool) -> Result<i64, String>;

/// Whether the preflight reads this venue's clock, and what it means.
///
/// [`ClockSource::Wired`] holds its own reader, so a row cannot CLAIM an endpoint the crate does
/// not read — the "wired" set is proven by construction rather than asserted by a test.
pub enum ClockSource {
    /// Preflight reads this venue's clock. `endpoint` is the operator-facing name of the read
    /// (method + path + how it authenticates); `risk` is what a drift costs here.
    Wired {
        /// e.g. `"GET /v5/market/time (public)"` — what to curl when reproducing a reading.
        endpoint: &'static str,
        /// Whether this read needs a credential (see [`ClockAuth`]).
        auth: ClockAuth,
        /// What a measured skew actually threatens at this venue.
        risk: ClockRisk,
        /// The read itself.
        fetch: ClockFetch,
    },
    /// No clock leg for this venue, DECLARED with its reason and with what the absence COSTS.
    NotWired {
        /// One sentence, in operator language, saying what is missing and why. The evidence behind
        /// it belongs in the row's doc comment, not in this string — this text lands in a log line.
        reason: &'static str,
        /// `None` — a drifted clock costs this venue NOTHING (its auth stamps no timestamp, or
        /// nothing here can mount it live at all), so the row is ③: NOT-APPLICABLE, never a
        /// warning.
        ///
        /// `Some(what_is_at_stake)` — this venue DOES bind the clock into the order path, so the
        /// missing leg is an unmeasured HAZARD (outcome ④) and the row WARNs. The string says what
        /// is at stake in that venue's own terms, because [`ClockRisk`]'s wording is CEX-specific
        /// and inheriting it would ship a false mechanism.
        unmeasured_risk: Option<&'static str>,
    },
}

/// EVERY venue on [`vike_model::VENUES`], classified. The completeness test below iterates the
/// canonical roster, so a new bridge crate cannot ship without a row here.
pub const CLOCK_SOURCES: &[(&str, ClockSource)] = &[
    // ---- ① wired: a real number is obtainable ------------------------------------------------
    (
        "binance",
        ClockSource::Wired {
            endpoint: "GET /api/v3/time (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::SignedTimestamp,
            fetch: binance_time,
        },
    ),
    (
        "bybit",
        ClockSource::Wired {
            endpoint: "GET /v5/market/time (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::SignedTimestamp,
            fetch: bybit_time,
        },
    ),
    (
        "okx",
        ClockSource::Wired {
            endpoint: "GET /api/v5/public/time (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::SignedTimestamp,
            fetch: okx_time,
        },
    ),
    (
        "aster",
        ClockSource::Wired {
            endpoint: "GET /fapi/v1/time (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::SignedTimestamp,
            fetch: aster_time,
        },
    ),
    // Deribit's own auth carries no timestamp — see `ClockRisk::NoTimestamp` and the endpoint's
    // doc in `vike_deribit::transport`'s `PATH_TIME`. Wired anyway: it is keyless, it answers in
    // ~50 ms from the CI box, and it is the only venue that tells us WHICH HOST answered.
    (
        "deribit",
        ClockSource::Wired {
            endpoint: "GET /api/v2/public/get_time (public, testnet host)",
            auth: ClockAuth::Public,
            risk: ClockRisk::NoTimestamp,
            fetch: deribit_time,
        },
    ),
    (
        "hyperliquid",
        ClockSource::Wired {
            endpoint: "POST /info {\"type\":\"exchangeStatus\"} (public)",
            auth: ClockAuth::Public,
            risk: ClockRisk::NonceWindow,
            fetch: hyperliquid_time,
        },
    ),
    // IG is the one CREDENTIALED read here, and it is cheap: the API key alone, no `POST /session`
    // login, no session to expire. The key is guaranteed present whenever this row can fire — the
    // clock leg only runs for a venue `would_mount_live_under_policy` accepted, and IG's live gate
    // IS that key. ⚠ That predicate is CEILING-AWARE: an ig capped to `paper` is never read here at
    // all, which is the point — this is the ONE credentialed read in the table.
    (
        "ig",
        ClockSource::Wired {
            endpoint: "GET /session/encryptionKey (X-IG-API-KEY only)",
            auth: ClockAuth::Credentialed,
            risk: ClockRisk::NoTimestamp,
            fetch: ig_time,
        },
    ),
    // ---- ③ declared, nothing at stake: no clock leg, with the reason -------------------------
    // OANDA publishes a `time` field, and it is NOT a clock. Measured from the CI box 2026-08-08 over
    // four consecutive `GET /v3/accounts/{id}/pricing` calls: the fractions were .300679626,
    // .300892984, .300157847 and .300407597 while the whole seconds went 30 → 31 → 33 → 35 against
    // local seconds 30 → 32 → 33 → 34. It is the pricing publication tick on a 1 s grid at a fixed
    // .300 phase, so the derived "skew" swung from -884 ms to +17 ms on a disciplined host. The
    // other two endpoints this adapter calls (`/v3/accounts`, `/v3/accounts/{id}/summary`) carry no
    // time field at all, and OANDA's Bearer auth stamps no timestamp on a request.
    (
        "oanda",
        ClockSource::NotWired {
            reason: "its only time field is the pricing snapshot's publication tick, quantized to \
                     a 1 s grid — a check over it would flap across a ±900 ms band on a healthy \
                     host",
            unmeasured_risk: None,
        },
    ),
    // Alpaca's `GET /v1/clock` exists and reads well (+~110 ms, sub-second, stable over three the CI box
    // reps on 2026-08-08) but is HTTP 401 without the OAuth2 client-credentials Bearer minted by
    // `crates/bridges/alpaca/src/auth.rs`'s `TokenSource`. Wiring it would put a second token
    // exchange in front of a measurement whose own risk row is `NoTimestamp` — alpaca's Bearer auth
    // stamps nothing — so the leg would cost a network lifecycle to catch a fault it cannot prevent.
    // ⚠ Its `timestamp` is RFC3339 with a NUMERIC US/Eastern offset (never `Z`), which follows US
    // DST: a parser that assumes UTC is wrong by four hours in August and five in December.
    (
        "alpaca",
        ClockSource::NotWired {
            reason: "its /v1/clock needs the OAuth2 client-credentials Bearer, i.e. a second token \
                     exchange before any measurement, and alpaca stamps no timestamp on a request",
            unmeasured_risk: None,
        },
    ),
    // cTrader is the strongest declared row in the table: the absence is a property of the
    // PUBLISHED SCHEMA, not of our wiring. `crates/bridges/ctrader/proto/OpenApiCommonMessages.proto`
    // and `OpenApiMessages.proto` contain no message, field or rpc matching /server ?time/;
    // `ProtoHeartbeatEvent` carries exactly one field (`payload_type`); and `ProtoOaSpotEvent`'s
    // optional `timestamp` is a TICK stamp (hours or days stale on a closed market) that
    // `crates/bridges/ctrader/src/conn.rs`'s `write_command` does not even request.
    (
        "ctrader",
        ClockSource::NotWired {
            reason: "the Open API protobuf schema publishes no server time at all — the heartbeat \
                     carries none and the only timestamp on the wire is a market tick",
            unmeasured_risk: None,
        },
    ),
    // IBKR is feature-gated here (a default build has no `("ibkr", _)` arm at all), and neither
    // backend offers a public clock: the socket API's time request rides the authenticated TWS
    // socket and the CP Gateway is a LOCAL process, so a "server time" read there would largely be
    // this host comparing itself against itself.
    (
        "ibkr",
        ClockSource::NotWired {
            reason: "feature-gated here, and both backends put the clock behind an authenticated \
                     TWS socket / local CP-Gateway session this pre-mount step does not open",
            unmeasured_risk: None,
        },
    ),
    // ⚠ Polymarket is outcome ④ — the ONE row whose gap is genuinely OURS and genuinely costs
    // orders. `crates/bridges/polymarket/src/exec_plane/auth.rs`'s `l2_auth_headers` signs `POLY_TIMESTAMP`
    // into every authenticated CLOB request (and `l1.rs`'s `l1_auth_headers` does the same for the
    // key-derivation handshake), so this venue's clock IS on the order path.
    //
    // ⚠ This row used to read "mounted recon-only behind a feature here" and render N/A, and it
    // could not print in any configuration where that sentence was true. `crate::startup`'s
    // `clock_venues` lists a venue only when `crate::would_mount_live_under_policy` accepts it (so
    // the deployment must also have ARMED it above `paper`), and that
    // function's `("polymarket", _)` arm needs the `polymarket` FEATURE **and** `flags.poly_exec`
    // **and** resolvable keys — i.e. the row can only ever be emitted for a mount that is about to
    // run the LIVE exec client, never for a recon-only one. So the old text was false exactly when
    // it appeared, and it dressed the roster's one order-affecting gap as "nothing to see here".
    //
    // The remaining reason is the true one: the CLOB is geo-blocked from the hosts this preflight
    // runs on and is reachable only through the SOCKS egress proxy that
    // `vike_polymarket::live_mount_from_vars` builds well AFTER this step. A future lane that moves
    // the proxy earlier should make this a `Wired` row.
    (
        "polymarket",
        ClockSource::NotWired {
            reason: "its CLOB is reachable only through the SOCKS egress proxy that the live mount \
                     builds after this step, so there is nothing this preflight can read yet",
            unmeasured_risk: Some(
                "polymarket signs POLY_TIMESTAMP into every authenticated CLOB request, so a \
                 drifted host clock is on the ORDER path here and this leg does not measure it",
            ),
        },
    ),
    // FXCM and dukascopy reach neither `make_engine` arm nor a REST surface, so nothing this
    // preflight guards can ever mount them live (`would_mount_live` is `false` for both, which is
    // what actually keeps these rows from ever firing).
    (
        "fxcm",
        ClockSource::NotWired {
            reason: "no REST surface at all (a native ForexConnect FFI session) and no make_engine \
                     arm, so nothing here mounts it live",
            unmeasured_risk: None,
        },
    ),
    (
        "dukascopy",
        ClockSource::NotWired {
            reason: "execution is a JForex Java sidecar over stdio with no REST API, and \
                     make_engine has no arm for it",
            unmeasured_risk: None,
        },
    ),
];

/// This venue's declared row, or `None` for a string that is not on the roster.
#[must_use]
pub fn clock_source(venue: &str) -> Option<&'static ClockSource> {
    CLOCK_SOURCES.iter().find(|(v, _)| *v == venue).map(|(_, s)| s)
}

/// A venue's clock row as the contract declares it, from its registry row.
#[must_use]
pub fn clock_decl(registry: &'static [crate::VenueRow], venue: &str) -> Option<ClockDecl> {
    match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => Some(row.declaration().clock),
        Some(crate::VenueRow::FeatureAbsent { .. }) => Some(crate::registry::ABSENT_CLOCK),
        Some(crate::VenueRow::Legacy(_)) | None => clock_source(venue).map(|s| match *s {
            ClockSource::Wired { endpoint, auth, risk, .. } => {
                ClockDecl::Wired { endpoint, auth, risk }
            }
            ClockSource::NotWired { reason, unmeasured_risk } => {
                ClockDecl::NotWired { reason, unmeasured_risk }
            }
        }),
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
/// `FeatureAbsent` row is declared `NotChecked`; a `Legacy` row — or a venue the registry does not
/// carry — keeps the legacy table's answer.
pub fn venue_server_time_ms(
    registry: &'static [crate::VenueRow],
    venue: &str,
    vars: &HashMap<String, String>,
    live_permitted: bool,
) -> Result<i64, ServerTimeGap> {
    let row = match crate::row_of(registry, venue) {
        Some(crate::VenueRow::Mount(row)) => *row,
        Some(crate::VenueRow::FeatureAbsent { .. }) => {
            return Err(ServerTimeGap::NotChecked(crate::registry::ABSENT_CLOCK_REASON));
        }
        Some(crate::VenueRow::Legacy(_)) | None => {
            return legacy_server_time_ms(venue, vars, live_permitted);
        }
    };
    match row.declaration().clock {
        ClockDecl::Wired { .. } => {
            let account = vike_model::account_keys::AccountLabel::Default;
            let process = vike_bridge_core::venue_mount::ProcessFacts::default();
            let inputs = vike_bridge_core::venue_mount::MountInputs {
                account: &account,
                secrets: vars,
                settings: crate::contract::settings_of(None, venue),
                live_permitted,
                accounts: crate::contract::unread_directory(),
                process: &process,
            };
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

/// The legacy table's answer — the four outcomes as `CLOCK_SOURCES` states them. A venue that is
/// not on the roster at all is [`ServerTimeGap::Unreachable`], not a declaration: an unknown venue
/// string is a caller bug, and declaring it "nothing to check" would hide it.
fn legacy_server_time_ms(
    venue: &str,
    vars: &HashMap<String, String>,
    live_permitted: bool,
) -> Result<i64, ServerTimeGap> {
    match clock_source(venue) {
        Some(ClockSource::Wired { fetch, .. }) => {
            fetch(vars, live_permitted).map_err(ServerTimeGap::Unreachable)
        }
        Some(ClockSource::NotWired { reason, unmeasured_risk: None }) => {
            Err(ServerTimeGap::NotChecked(reason))
        }
        Some(ClockSource::NotWired { reason, unmeasured_risk: Some(at_stake) }) => {
            Err(ServerTimeGap::UnmeasuredRisk { reason, at_stake })
        }
        None => Err(ServerTimeGap::Unreachable(format!("{venue} is not on the venue roster"))),
    }
}

/// The shared blocking `ureq` transport for every clock read — same stack as every adapter, but on
/// an agent bounded by [`CLOCK_READ_TIMEOUT`] instead of the 30 s default. `UreqTransport::with_agent`
/// is the existing seam for exactly this (it is how the polymarket adapter injects its proxied
/// agent).
fn bounded_transport(venue: &'static str) -> UreqTransport {
    UreqTransport::with_agent(
        venue,
        vike_bridge_core::http::blocking_agent_with_timeout(CLOCK_READ_TIMEOUT),
    )
}

/// `GET {base}{path}` over that bounded transport, with the venue's own error text on failure.
fn public_time_body(
    venue: &'static str,
    base: &str,
    path: &str,
) -> Result<serde_json::Value, String> {
    bounded_transport(venue).public(base, path, &[]).map_err(|e| e.to_string())
}

/// The one shared failure message: the endpoint answered, but not with the field we parse.
fn missing(field: &str) -> String {
    format!("{field} missing from the server-time response")
}

/// `{"serverTime": <epoch ms>}` — binance's shape, and aster's (the venue is binance-API-shaped).
/// Split from the fetch so the field name is gated by a fixture test rather than by a live call.
fn parse_binance_shaped_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("serverTime").and_then(serde_json::Value::as_i64).ok_or_else(|| missing("serverTime"))
}

/// binance `/api/v3/time` on the SAME demo/mainnet host the mount binds — decision 0095: the
/// ceiling alone chooses it (`ceiling_selects_mainnet("binance") && live_permitted`).
fn binance_time(_vars: &HashMap<String, String>, live_permitted: bool) -> Result<i64, String> {
    let base = if crate::ceiling_selects_mainnet("binance") && live_permitted {
        vike_binance::spot::MAINNET_REST
    } else {
        vike_binance::spot::DEMO_REST
    };
    parse_binance_shaped_time(&public_time_body("binance", base, vike_binance::spot::PATH_TIME)?)
}

/// bybit's v5 envelope. ⚠ The stamp is the TOP-LEVEL `"time"` NUMBER; `result.timeNano` is a
/// NANOSECOND string and `result.timeSecond` a SECONDS string, so reading the wrong one is off by
/// a factor of a million or a thousand while still looking like a plausible integer.
fn parse_bybit_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("time").and_then(serde_json::Value::as_i64).ok_or_else(|| missing("time"))
}

/// bybit `/v5/market/time`, demo or mainnet per the ceiling (decision 0095) — two SEPARATE hosts.
fn bybit_time(_vars: &HashMap<String, String>, live_permitted: bool) -> Result<i64, String> {
    let base = if crate::ceiling_selects_mainnet("bybit") && live_permitted {
        vike_bybit::perp::MAINNET_REST
    } else {
        vike_bybit::perp::DEMO_REST
    };
    parse_bybit_time(&public_time_body("bybit", base, vike_bybit::perp::PATH_TIME)?)
}

/// okx's envelope. ⚠ `data[0].ts` is epoch ms as a STRING inside an ARRAY — `as_i64()` reads
/// `None`, and an empty `data` array must not panic.
fn parse_okx_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("data")
        .and_then(|d| d.get(0))
        .and_then(|row| row.get("ts"))
        .and_then(serde_json::Value::as_str)
        .and_then(|ts| ts.parse::<i64>().ok())
        .ok_or_else(|| missing("data[0].ts"))
}

/// okx `/api/v5/public/time`. Demo and mainnet share [`vike_okx::perp::REST`], so there is no tier
/// to resolve.
fn okx_time(_vars: &HashMap<String, String>, _live_permitted: bool) -> Result<i64, String> {
    parse_okx_time(&public_time_body("okx", vike_okx::perp::REST, vike_okx::perp::PATH_TIME)?)
}

/// aster `/fapi/v1/time` on the FUTURES host the exec client binds, tier-resolved by the SAME
/// `vike_aster::signing::mountable_tier_for_account` chain `make_engine`'s `("aster", _)` arm
/// calls — aster has no mainnet switch, its tier IS which key set resolved under this ceiling.
fn aster_time(vars: &HashMap<String, String>, live_permitted: bool) -> Result<i64, String> {
    let env = vike_aster::signing::mountable_tier_for_account(
        &crate::AccountLabel::Default,
        vars,
        live_permitted,
    )
    .map(|(env, _)| env)
    .unwrap_or(Environment::Demo);
    let base = vike_aster::urls::urls_for(env).fapi_rest;
    parse_binance_shaped_time(&public_time_body("aster", base, vike_aster::perp::PATH_TIME)?)
}

/// deribit's JSON-RPC envelope. ⚠ The stamp is a BARE top-level `result` i64 in epoch MS, sitting
/// beside `usIn`/`usOut`/`usDiff` in MICROSECONDS — the nearest wrong field is a thousand-fold out.
/// The `"testnet"` boolean is a free correctness assert no other venue offers: it proves the host
/// that ANSWERED is the tier we bound, rather than trusting the URL constant.
fn parse_deribit_time(body: &serde_json::Value) -> Result<i64, String> {
    if body.get("testnet").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(
            "the deribit host answered testnet=false, but every exec spawn site binds testnet — \
             this reading is against the wrong host"
                .to_string(),
        );
    }
    body.get("result").and_then(serde_json::Value::as_i64).ok_or_else(|| missing("result"))
}

/// deribit `/api/v2/public/get_time` on the TESTNET host — unconditionally, because
/// `crates/bridges/deribit/src/exec.rs` hardcodes `TESTNET_REST`/`TESTNET_WS` at every spawn site
/// and deribit's network is never the ceiling (`ceiling_selects_mainnet` excludes it). A check
/// pointed at `www.deribit.com` would measure a host this mount never talks to.
fn deribit_time(_vars: &HashMap<String, String>, _live_permitted: bool) -> Result<i64, String> {
    parse_deribit_time(&public_time_body(
        "deribit",
        vike_deribit::transport::TESTNET_REST,
        vike_deribit::transport::PATH_TIME,
    )?)
}

/// hyperliquid's `exchangeStatus` body: `{"specialStatuses":null,"time":<epoch ms>}`.
fn parse_hyperliquid_time(body: &serde_json::Value) -> Result<i64, String> {
    body.get("time").and_then(serde_json::Value::as_i64).ok_or_else(|| missing("time"))
}

/// hyperliquid `POST /info {"type":"exchangeStatus"}` — keyless, over the crate's OWN transport
/// (which already owns the IP-weight gate) on an agent bounded by [`CLOCK_READ_TIMEOUT`], on the
/// tier the ceiling resolves. ⚠ The two tiers' clocks genuinely differ (see the module doc), so
/// this must not be simplified to "always mainnet" — the ceiling reaches the tier chosen here too
/// (decision 0095): `Env::for_ceiling(live_permitted)` is the exact fold `make_engine`'s
/// `("hyperliquid", _)` arm applies, so this can never read a tier the mount does not bind.
fn hyperliquid_time(_vars: &HashMap<String, String>, live_permitted: bool) -> Result<i64, String> {
    let network = vike_hyperliquid::config::Env::for_ceiling(live_permitted).network();
    let body = vike_hyperliquid::transport::HyperliquidTransport::with_agent(
        network,
        vike_bridge_core::http::blocking_agent_with_timeout(CLOCK_READ_TIMEOUT),
    )
    .info(&serde_json::json!({ "type": "exchangeStatus" }))
    .map_err(|e| e.to_string())?;
    parse_hyperliquid_time(&body)
}

/// ig `GET /session/encryptionKey` with the API key only — no login, no session. The tier mirrors
/// the `("ig", _)` mount arm, which resolves `Environment::Demo` and nothing else. The parse (and
/// its own fixture test) lives in that crate, beside the request that produces the body.
fn ig_time(vars: &HashMap<String, String>, _live_permitted: bool) -> Result<i64, String> {
    let Some(cfg) = vike_ig::load_ig_config_from(Environment::Demo, vars) else {
        // The variable NAME comes from IG's own naming authority rather than a literal: it cannot
        // rot against that crate, and this file then states no env-shaped string it does not read
        // (`crates/vike-ops/tests/settings_registry.rs`'s literal harvest scores one as an
        // undeclared read — and the read here is `load_ig_config_from`'s, over the caller's map).
        let (api_key_var, _, _) = vike_ig::ig_env_var_names(Environment::Demo);
        return Err(format!("{api_key_var} is not set, so there is no key to read the clock with"));
    };
    vike_ig::IgSession::server_time_ms(&cfg, CLOCK_READ_TIMEOUT).map_err(|e| e.to_string())
}

#[path = "server_time_tests.rs"]
#[cfg(test)]
mod server_time_tests;
