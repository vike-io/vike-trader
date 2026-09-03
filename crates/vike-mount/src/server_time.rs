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
//! It now decides **thresholds** as well as words, through [`ClockRisk::policy`], because a single
//! global threshold was measurably wrong — see the re-derivation in [`crate::preflight`]'s
//! module doc. The two halves in one sentence: only a venue that REJECTS an order over drift may
//! FAIL (and so degrade itself to paper), and a venue that cannot is judged against the looser
//! host-health threshold that its own server clock's wander demands.
//!
//! # Tier discipline: measure the clock that will judge us
//!
//! Every fetcher resolves the SAME demo/mainnet host its venue's `make_engine` arm binds — through
//! the same readers (`cex_mainnet_enabled`, `hl_env`, `load_aster_credentials`'s Live-then-Demo
//! chain, deribit's hardcoded testnet). This is not tidiness: hyperliquid's testnet and mainnet
//! clocks measured **-250 ms and -211 ms in one paired sample from the CI box** (2026-08-09, back to
//! back inside 600 ms), so a check pointed at the wrong tier measures a clock that will never judge
//! our orders.
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

use crate::preflight::{
    ClockPolicy, ServerTimeGap, CANARY_CLOCK_WARN_MS, DEFAULT_CLOCK_FAIL_MS, DEFAULT_CLOCK_WARN_MS,
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

/// What a drifted host clock actually COSTS at a venue — the fact each wired row must state, so the
/// report's remedy can never assert a mechanism the venue does not have, and so only the venues
/// that can lose an order are judged against the recv-window thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockRisk {
    /// Every private request is SIGNED with a timestamp and rejected outside a recv window (the
    /// signers hard-code `recv_window: 5000`). Drift far enough and EVERY order is rejected:
    /// binance `-1021`, bybit `10002`, okx `50102`.
    SignedTimestamp,
    /// The clock is bound into the order NONCE rather than a recv window. Hyperliquid accepts a
    /// nonce inside `(T - 2 days, T + 1 day)` (`docs/research/2026-07-16-hyperliquid-adapters/README.md`
    /// §9, implemented by that crate's `NonceManager`), so the rejection cliff is a DAY away, not
    /// five seconds.
    NonceWindow,
    /// Auth carries no per-request timestamp at all (deribit's `client_credentials`, ig's session
    /// token pair), so no order can be rejected for clock drift. The leg is a HOST-HEALTH canary:
    /// it still proves the box's NTP is broken, and a wrong clock still misdates every local
    /// record of the session.
    NoTimestamp,
}

impl ClockRisk {
    /// The remediation line for a measured out-of-band skew at a venue with this risk. Every one
    /// starts with the same action — the host clock is what is wrong — and differs in what it
    /// costs, which is the part an operator prioritises on.
    #[must_use]
    pub fn remedy(self) -> &'static str {
        match self {
            ClockRisk::SignedTimestamp => {
                "sync the host clock (NTP / w32tm) — this venue stamps a timestamp on every signed \
                 request and rejects one outside a 5000 ms recv window, so drift rejects ORDERS"
            }
            ClockRisk::NonceWindow => {
                "sync the host clock (NTP / w32tm) — this venue binds the clock into the order \
                 nonce (valid within roughly a day), so orders survive this drift but the host's \
                 NTP is broken"
            }
            ClockRisk::NoTimestamp => {
                "sync the host clock (NTP / w32tm) — this venue's auth stamps no timestamp, so \
                 orders are NOT at risk here; the drift misdates every local record and is evidence \
                 the host's NTP is broken"
            }
        }
    }

    /// The thresholds a reading at this venue is judged against — the fix for a global threshold
    /// that MEASUREMENT falsified (see [`crate::preflight`]'s "the thresholds are per-venue"
    /// section for the readings and the derivation).
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
    pub fn policy(self) -> ClockPolicy {
        match self {
            ClockRisk::SignedTimestamp => ClockPolicy {
                warn_ms: DEFAULT_CLOCK_WARN_MS,
                fail_ms: Some(DEFAULT_CLOCK_FAIL_MS),
                remedy: self.remedy(),
            },
            ClockRisk::NonceWindow | ClockRisk::NoTimestamp => {
                ClockPolicy { warn_ms: CANARY_CLOCK_WARN_MS, fail_ms: None, remedy: self.remedy() }
            }
        }
    }
}

/// One venue's clock read: the `.env`/credentials map in, an ABSOLUTE epoch-ms venue stamp out.
/// `Err` is the venue's own error text — never a URL, never a credential (see
/// [`crate::preflight`]'s secrets note).
type ClockFetch = fn(&HashMap<String, String>) -> Result<i64, String>;

/// Whether reading this venue's clock needs a credential — the fact that decides whether a box
/// WITHOUT that credential may legitimately skip the read, or has just found a real fault.
///
/// It is a declared field rather than something inferred from the endpoint text, because the live
/// smoke branches on it: a keyless endpoint that fails from a box with egress is the ② this whole
/// lane exists to surface, while a credentialed one may simply have no key here. Sniffing the
/// human-facing `endpoint` string for "(public)" got deribit's row — whose label reads
/// `"(public, testnet host)"` — WRONG, reporting a genuine failure as a skipped credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockAuth {
    /// Keyless: any box with egress can read it, so a failure is always a real failure.
    Public,
    /// Needs a credential from the vars map (never a signature — these are all cheap reads).
    Credentialed,
}

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
            reason:
                "its /v1/clock needs the OAuth2 client-credentials Bearer, i.e. a second token \
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
    // orders. `crates/bridges/polymarket/src/auth.rs`'s `l2_auth_headers` signs `POLY_TIMESTAMP`
    // into every authenticated CLOB request (and `l1.rs`'s `l1_auth_headers` does the same for the
    // key-derivation handshake), so this venue's clock IS on the order path.
    //
    // ⚠ This row used to read "mounted recon-only behind a feature here" and render N/A, and it
    // could not print in any configuration where that sentence was true. `crate::startup`'s
    // `clock_venues` lists a venue only when `crate::would_mount_live_under_policy` accepts it (so
    // the deployment must also have ARMED it above `paper`), and that
    // function's `("polymarket", _)` arm needs the `polymarket` FEATURE **and** `POLY_EXEC=1`
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
            reason:
                "its CLOB is reachable only through the SOCKS egress proxy that the live mount \
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
            reason:
                "no REST surface at all (a native ForexConnect FFI session) and no make_engine \
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
    // The scaffold's insertion point sits HERE, at the end of ③, and not at the head of the
    // table: a generated row is `NotWired`, and a `NotWired` row filed under the ① divider reads
    // as a wired one. Nothing gates row ORDER (`no_clock_source_row_is_off_roster` checks
    // membership and count), so the divider structure is documentation only — which is exactly
    // why a generator must not violate it.
    // vike:new-venue:row // TODO(new-venue: {venue}): if the venue publishes a server-time endpoint, replace this with a
    // vike:new-venue:row // `ClockSource::Wired` row naming the endpoint an operator can curl, and MOVE it up under ①.
    // vike:new-venue:row // `unmeasured_risk` must be `Some(..)` the moment the venue binds a timestamp into the order path.
    // vike:new-venue:row (
    // vike:new-venue:row     "{venue}",
    // vike:new-venue:row     ClockSource::NotWired {
    // vike:new-venue:row         reason: "no server-time endpoint is wired for this bridge yet, so the clock leg \
    // vike:new-venue:row                  measures nothing on this venue",
    // vike:new-venue:row         unmeasured_risk: None,
    // vike:new-venue:row     },
    // vike:new-venue:row ),
];

/// This venue's declared row, or `None` for a string that is not on the roster.
#[must_use]
pub fn clock_source(venue: &str) -> Option<&'static ClockSource> {
    CLOCK_SOURCES.iter().find(|(v, _)| *v == venue).map(|(_, s)| s)
}

/// The thresholds and remediation text a MEASURED reading at `venue` is judged against — the
/// venue's own [`ClockRisk::policy`], so the report can never claim a recv window a venue does not
/// have, and can never demote a venue over a fault its auth cannot suffer. `None` for an unwired or
/// unknown venue (which produces no measurement, hence nothing to judge).
#[must_use]
pub fn clock_policy(venue: &str) -> Option<ClockPolicy> {
    match clock_source(venue) {
        Some(ClockSource::Wired { risk, .. }) => Some(risk.policy()),
        _ => None,
    }
}

/// The clock leg's venue read: `venue`'s own server time as an ABSOLUTE epoch-ms stamp (which is
/// what [`crate::preflight::PreflightProbes::venue_server_time_ms`] wants — not the OFFSET
/// `BinanceSpotRest::server_time_offset` returns).
///
/// The four outcomes are the module doc's ①/②/③/④. A venue that is not on the roster at all is
/// [`ServerTimeGap::Unreachable`], not a declaration: an unknown venue string is a caller bug, and
/// declaring it "nothing to check" would hide it.
pub fn venue_server_time_ms(
    venue: &str,
    vars: &HashMap<String, String>,
) -> Result<i64, ServerTimeGap> {
    match clock_source(venue) {
        Some(ClockSource::Wired { fetch, .. }) => fetch(vars).map_err(ServerTimeGap::Unreachable),
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

/// binance `/api/v3/time` on the SAME demo/mainnet host the mount binds (`BINANCE_MAINNET=1`
/// resolved through the shared `cex_mainnet_enabled`, which reads the `.env` map as well as the
/// process env).
fn binance_time(vars: &HashMap<String, String>) -> Result<i64, String> {
    let base = if crate::cex_mainnet_enabled("binance", vars) {
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

/// bybit `/v5/market/time`, demo or mainnet per `BYBIT_MAINNET` — two SEPARATE hosts.
fn bybit_time(vars: &HashMap<String, String>) -> Result<i64, String> {
    let base = if crate::cex_mainnet_enabled("bybit", vars) {
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
fn okx_time(_vars: &HashMap<String, String>) -> Result<i64, String> {
    parse_okx_time(&public_time_body("okx", vike_okx::perp::REST, vike_okx::perp::PATH_TIME)?)
}

/// aster `/fapi/v1/time` on the FUTURES host the exec client binds, tier-resolved by the SAME
/// Live-then-Demo credential chain `make_engine`'s `("aster", _)` arm uses — aster has no
/// `{VENUE}_MAINNET` flag, its tier IS which key set resolved.
fn aster_time(vars: &HashMap<String, String>) -> Result<i64, String> {
    let env = if vike_aster::signing::load_aster_credentials(Environment::Live, vars).is_some() {
        Environment::Live
    } else {
        Environment::Demo
    };
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
/// and deribit has no `{VENUE}_MAINNET` flag (`vike_bridge_core::mainnet` returns `None` for it).
/// A check pointed at `www.deribit.com` would measure a host this mount never talks to.
fn deribit_time(_vars: &HashMap<String, String>) -> Result<i64, String> {
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
/// tier `hl_env` resolves. ⚠ The two tiers' clocks genuinely differ (see the module doc), so this
/// must not be simplified to "always mainnet".
///
/// ⚠ `hl_env`'s ceiling conjunct is passed `true` — the FLAG's own answer, uncapped — and it is now
/// a DECLARED RESIDUAL rather than the deliberate choice this doc used to describe. The old
/// argument ("no policy has reached the preflight") is dead: the ceiling reaches
/// `crate::startup::clock_venues`, which walks the roster with
/// `crate::would_mount_live_under_policy`, so a venue capped to `paper` is no longer read at all.
/// What the ceiling does NOT reach is the TIER chosen inside a fetcher, because [`ClockFetch`] is a
/// `fn(&HashMap<String, String>)` in a `const` table. So under a `demo` ceiling with the mainnet
/// flag armed, this reads the MAINNET clock while the mount binds testnet — and the two tiers
/// genuinely differ (see the module doc's tier-discipline section). It stays a residual rather than
/// a fix because the read is KEYLESS and its worst verdict is a WARN: no account is touched and
/// nothing can be demoted by it. Closing it means widening [`ClockFetch`] itself, which is a change
/// to every row in [`CLOCK_SOURCES`]. `crate::startup`'s module doc declares this residual beside
/// the rest of the ceiling wiring.
fn hyperliquid_time(vars: &HashMap<String, String>) -> Result<i64, String> {
    let network = crate::hyperliquid::hl_env(vars, true).network();
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
fn ig_time(vars: &HashMap<String, String>) -> Result<i64, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preflight::REMEDY_CLOCK;

    /// A REAL captured body for each wired venue, read from
    /// `crates/vike-mount/tests/fixtures/server_time/`. Captured from the CI box on 2026-08-09 (ig's
    /// twin lives in that crate — see `crates/bridges/ig/src/rest.rs`'s `parse_server_time_ms`).
    fn fixture(name: &str) -> serde_json::Value {
        let raw = match name {
            "binance" => include_str!("../tests/fixtures/server_time/binance.json"),
            "bybit" => include_str!("../tests/fixtures/server_time/bybit.json"),
            "okx" => include_str!("../tests/fixtures/server_time/okx.json"),
            "aster" => include_str!("../tests/fixtures/server_time/aster.json"),
            "deribit" => include_str!("../tests/fixtures/server_time/deribit.json"),
            "hyperliquid" => include_str!("../tests/fixtures/server_time/hyperliquid.json"),
            other => panic!("no captured fixture for {other}"),
        };
        serde_json::from_str(raw).expect("the captured fixture is valid JSON")
    }

    /// THE completeness gate, and the reason no count of wired venues is written down anywhere:
    /// every canonical roster venue is either wired or declared, and adding a bridge crate turns
    /// this red until the new venue is classified.
    #[test]
    fn clock_sources_cover_the_roster() {
        for venue in vike_model::VENUES {
            assert!(
                clock_source(venue).is_some(),
                "{venue} is on vike_model::VENUES but has no CLOCK_SOURCES row — wire its clock or \
                 declare why it has none"
            );
        }
    }

    /// …and the other direction: no row names a venue that is not on the roster (a typo would
    /// otherwise silently classify nothing).
    #[test]
    fn no_clock_source_row_is_off_roster() {
        for (venue, _) in CLOCK_SOURCES {
            assert!(
                vike_model::VENUES.contains(venue),
                "CLOCK_SOURCES names {venue}, which is not in vike_model::VENUES"
            );
        }
        assert_eq!(CLOCK_SOURCES.len(), vike_model::VENUES.len(), "exactly one row per venue");
    }

    #[test]
    fn no_venue_is_declared_twice() {
        for (i, (venue, _)) in CLOCK_SOURCES.iter().enumerate() {
            let dupe = CLOCK_SOURCES.iter().skip(i + 1).any(|(other, _)| other == venue);
            assert!(!dupe, "{venue} has two CLOCK_SOURCES rows");
        }
    }

    /// A wired row must name the endpoint an operator can curl; a declared row must give a REASON,
    /// not a shrug. The length floor is deliberate — "n/a" would pass a non-empty check and teach
    /// nobody anything.
    #[test]
    fn every_row_says_something_useful() {
        for (venue, source) in CLOCK_SOURCES {
            match source {
                ClockSource::Wired { endpoint, auth, .. } => {
                    assert!(endpoint.contains('/'), "{venue}'s endpoint names no path: {endpoint}");
                    // The label and the declared `auth` must not contradict each other: the label
                    // is what an operator reads, the field is what the live smoke branches on, and
                    // a row whose two halves disagree is a capability table with a false row.
                    let labelled_public = endpoint.contains("(public");
                    assert_eq!(
                        labelled_public,
                        *auth == ClockAuth::Public,
                        "{venue}'s endpoint label and its declared auth disagree: {endpoint}"
                    );
                }
                ClockSource::NotWired { reason, unmeasured_risk } => {
                    assert!(
                        reason.len() >= 60,
                        "{venue}'s NotWired reason is too short to be an explanation: {reason}"
                    );
                    assert!(!reason.contains("TODO"), "{venue}'s reason is a TODO, not a reason");
                    if let Some(at_stake) = unmeasured_risk {
                        assert!(
                            at_stake.len() >= 60,
                            "{venue} declares an unmeasured risk without saying what is at stake"
                        );
                    }
                }
            }
        }
    }

    /// The remedy is per-venue BECAUSE it would otherwise be false: only the recv-window venues may
    /// mention a recv window, and only they may claim orders are rejected.
    #[test]
    fn only_recv_window_venues_claim_orders_are_rejected() {
        for (venue, source) in CLOCK_SOURCES {
            let ClockSource::Wired { risk, .. } = source else { continue };
            let remedy = risk.remedy();
            assert!(remedy.starts_with("sync the host clock"), "{venue}: {remedy}");
            let claims_recv_window = remedy.contains("recv window");
            assert_eq!(
                claims_recv_window,
                *risk == ClockRisk::SignedTimestamp,
                "{venue}'s remedy must mention a recv window iff it signs a timestamp: {remedy}"
            );
            if *risk == ClockRisk::NoTimestamp {
                assert!(
                    remedy.contains("NOT at risk"),
                    "{venue} cannot reject an order over clock drift; say so: {remedy}"
                );
            }
        }
    }

    /// THE policy rule, over the table: a venue may carry a FAIL threshold — the one that degrades
    /// it to paper — IFF a drifted clock can actually get its orders rejected. Everything else is a
    /// host-health canary that may warn and nothing more.
    #[test]
    fn only_order_rejecting_venues_can_fail_their_clock_check() {
        for (venue, source) in CLOCK_SOURCES {
            let ClockSource::Wired { risk, .. } = source else {
                assert_eq!(clock_policy(venue), None, "{venue} is unwired and judges nothing");
                continue;
            };
            let policy = risk.policy();
            assert_eq!(clock_policy(venue), Some(policy), "{venue}");
            assert_eq!(
                policy.fail_ms.is_some(),
                *risk == ClockRisk::SignedTimestamp,
                "{venue}: only a venue that REJECTS orders over drift may be degraded to paper by \
                 this leg"
            );
            assert!(policy.warn_ms > 0, "{venue} must warn somewhere");
            assert_ne!(policy.remedy, REMEDY_CLOCK, "{venue} must carry its OWN remedy");
        }
    }

    /// The canary venues warn LATER than the recv-window ones, because what a reading at them
    /// measures is dominated by the venue's own clock (hyperliquid: -220..-424 ms across 40
    /// samples from an NTP-disciplined box).
    #[test]
    fn the_canary_threshold_is_looser_than_the_recv_window_one() {
        let signed = ClockRisk::SignedTimestamp.policy();
        let nonce = ClockRisk::NonceWindow.policy();
        let none = ClockRisk::NoTimestamp.policy();
        assert!(nonce.warn_ms > signed.warn_ms);
        // The two canary risks share THRESHOLDS and differ only in words — a nonce window and an
        // absent timestamp cost the same nothing, but an operator is told which one they have.
        assert_eq!((nonce.warn_ms, nonce.fail_ms), (none.warn_ms, none.fail_ms));
        assert_ne!(nonce.remedy, none.remedy, "…and they must not say the same thing");
        assert_eq!(signed.fail_ms, Some(DEFAULT_CLOCK_FAIL_MS));
        assert_eq!(nonce.fail_ms, None);
    }

    /// ③ IS PURE: a declared venue with nothing at stake answers `NotChecked` with its reason and
    /// touches NO network — which is what lets the preflight render it without a probe, a timeout
    /// or a warning.
    #[test]
    fn a_declared_venue_reports_not_checked_without_touching_the_network() {
        let vars = HashMap::new();
        for (venue, source) in CLOCK_SOURCES {
            let ClockSource::NotWired { reason, unmeasured_risk: None } = source else { continue };
            let gap =
                venue_server_time_ms(venue, &vars).expect_err("declared venues never measure");
            assert_eq!(gap, ServerTimeGap::NotChecked(reason), "{venue}");
        }
    }

    /// ④ AND ITS ONE ROW: a declared venue whose clock IS on the order path answers the OTHER
    /// declaration, so the preflight can warn about it instead of printing "not applicable" over
    /// the roster's only order-affecting gap. Also pure — no network.
    #[test]
    fn a_declared_venue_with_orders_at_stake_reports_the_risk_not_a_shrug() {
        let vars = HashMap::new();
        let mut at_risk = 0usize;
        for (venue, source) in CLOCK_SOURCES {
            let ClockSource::NotWired { reason, unmeasured_risk: Some(at_stake) } = source else {
                continue;
            };
            at_risk += 1;
            let gap =
                venue_server_time_ms(venue, &vars).expect_err("declared venues never measure");
            assert_eq!(gap, ServerTimeGap::UnmeasuredRisk { reason, at_stake }, "{venue}");
        }
        assert!(at_risk > 0, "the table has an at-risk row, or this test proves nothing");
    }

    /// The polymarket row's PREMISE, asserted rather than asserted-about: that venue really does
    /// sign a timestamp into every authenticated request. Under the feature this reads the venue
    /// crate's own header builder, so the row cannot outlive the fact it claims.
    #[cfg(feature = "polymarket")]
    #[test]
    fn the_polymarket_row_is_at_risk_because_that_venue_signs_a_timestamp() {
        let creds = vike_polymarket::PolymarketCreds {
            secret: "cG9seW1hcmtldC1sMi1zZWNyZXQta2V5LTEyMzQ1Njc4".to_string(),
            address: "0xabc".to_string(),
            api_key: "key-1".to_string(),
            passphrase: "pass-1".to_string(),
            ..Default::default()
        };
        let headers = vike_polymarket::l2_auth_headers(&creds, 1_700_000_000, "GET", "/x", "")
            .expect("the fixture secret is valid base64url");
        assert!(
            headers.iter().any(|(k, _)| k == "POLY_TIMESTAMP"),
            "polymarket's row claims its clock is on the order path — prove it"
        );
        let Some(ClockSource::NotWired { unmeasured_risk, .. }) = clock_source("polymarket") else {
            panic!("polymarket is declared, not wired");
        };
        assert!(unmeasured_risk.is_some(), "…so its row must declare the risk, not shrug");
    }

    /// An unknown venue string is NOT silently "declared" — it is an error that names itself.
    #[test]
    fn an_unknown_venue_is_unreachable_not_declared() {
        let gap = venue_server_time_ms("not-a-venue", &HashMap::new()).expect_err("unknown venue");
        match gap {
            ServerTimeGap::Unreachable(e) => assert!(e.contains("not-a-venue"), "{e}"),
            other => panic!("an unknown venue must not read as declared: {other:?}"),
        }
    }

    /// The wired venues that need a credential to READ the clock are exactly the ones whose live
    /// gate already resolved one — so an uncredentialed box can never turn a wired row into a
    /// spurious ② "the venue did not answer". (ig is the only such row today; the assert is over
    /// the table, not over that fact.)
    #[test]
    fn a_credentialed_clock_read_reports_its_own_missing_key() {
        let vars = HashMap::new();
        let e = ig_time(&vars).expect_err("no IG credentials in an empty map");
        let (api_key_var, _, _) = vike_ig::ig_env_var_names(Environment::Demo);
        assert!(
            e.contains(&api_key_var),
            "the message must name what is missing, not the URL: {e}"
        );
        assert!(!e.contains("http"), "no URL in a report line: {e}");
    }

    // ---- the PARSE, against real captured bodies (CI, no network) ------------------------------

    /// Every wired parser against the REAL body its venue answered, with the exact stamp pinned.
    /// This is the test that was missing: the parses were inline in the fetchers, so a renamed
    /// field or a wrong unit could only be found by the `#[ignore]`d live smoke.
    #[test]
    fn every_parser_reads_its_venues_real_captured_body() {
        assert_eq!(parse_binance_shaped_time(&fixture("binance")), Ok(1_786_242_369_664));
        assert_eq!(parse_binance_shaped_time(&fixture("aster")), Ok(1_786_242_370_439));
        assert_eq!(parse_bybit_time(&fixture("bybit")), Ok(1_786_242_369_911));
        assert_eq!(parse_okx_time(&fixture("okx")), Ok(1_786_242_370_157));
        assert_eq!(parse_deribit_time(&fixture("deribit")), Ok(1_786_242_370_599));
        assert_eq!(parse_hyperliquid_time(&fixture("hyperliquid")), Ok(1_786_242_384_206));
    }

    /// THE UNIT TRAP, pinned at the two venues that ship a wrong-unit field in the SAME body: a
    /// mix-up here reports a thousand- or million-fold skew and looks authoritative. Each assert
    /// names the neighbouring field the parse must NOT read.
    #[test]
    fn no_parser_reads_a_neighbouring_field_in_the_wrong_unit() {
        // bybit: `result.timeNano` is NANOSECONDS as a string, `result.timeSecond` SECONDS.
        let bybit = fixture("bybit");
        let ns = bybit["result"]["timeNano"].as_str().expect("the capture carries timeNano");
        let secs = bybit["result"]["timeSecond"].as_str().expect("…and timeSecond");
        let read = parse_bybit_time(&bybit).expect("the ms number parses");
        assert_ne!(read.to_string(), ns, "timeNano is nanoseconds — a millionfold error");
        assert_ne!(read.to_string(), secs, "timeSecond is seconds — a thousandfold error");
        assert_eq!(read / 1_000, secs.parse::<i64>().unwrap(), "…and it agrees with them");
        assert_eq!(read, ns.parse::<i64>().unwrap() / 1_000_000);

        // deribit: `usIn`/`usOut` are MICROSECONDS beside the ms `result`.
        let deribit = fixture("deribit");
        let us_in = deribit["usIn"].as_i64().expect("the capture carries usIn");
        let read = parse_deribit_time(&deribit).expect("the ms result parses");
        assert_ne!(read, us_in, "usIn is microseconds — a thousandfold error");
        assert_eq!(read, us_in / 1_000);

        // okx: the value is a STRING inside an ARRAY, so the naive `as_i64()` reads nothing.
        let okx = fixture("okx");
        assert!(okx["data"][0]["ts"].as_i64().is_none(), "precondition: it is a string");
        assert_eq!(parse_okx_time(&okx), Ok(1_786_242_370_157));
    }

    /// A body that answered but carries no stamp is an ERROR naming the field, never a zero or a
    /// panic — and every parser is total over garbage.
    #[test]
    fn a_body_without_the_stamp_names_the_missing_field() {
        let empty = serde_json::json!({});
        assert_eq!(parse_binance_shaped_time(&empty), Err(missing("serverTime")));
        assert_eq!(parse_bybit_time(&empty), Err(missing("time")));
        assert_eq!(parse_hyperliquid_time(&empty), Err(missing("time")));
        assert_eq!(parse_okx_time(&empty), Err(missing("data[0].ts")));
        // okx with an EMPTY data array — the shape that would index out of bounds.
        assert_eq!(
            parse_okx_time(&serde_json::json!({"code":"0","data":[],"msg":""})),
            Err(missing("data[0].ts"))
        );
        // okx with the ts as a NUMBER (the shape a future API change might send): still refused
        // rather than silently mis-read.
        assert_eq!(
            parse_okx_time(&serde_json::json!({"data":[{"ts":1_786_242_370_157i64}]})),
            Err(missing("data[0].ts"))
        );
    }

    /// deribit's row measures the TESTNET host every exec spawn site binds, and its body says so.
    /// A mainnet body — `testnet: false` — is refused rather than reported as a reading against a
    /// host this mount never talks to.
    #[test]
    fn a_deribit_body_from_the_wrong_host_is_refused() {
        let mut mainnet = fixture("deribit");
        mainnet["testnet"] = serde_json::Value::Bool(false);
        let e = parse_deribit_time(&mainnet).expect_err("wrong host");
        assert!(e.contains("wrong host"), "{e}");
        // …and an absent flag is treated the same way, not as an implicit pass.
        let mut absent = fixture("deribit");
        absent.as_object_mut().expect("object").remove("testnet");
        assert!(parse_deribit_time(&absent).is_err());
    }
}
