//! The blocking datahub client: a thin wrapper over one [`TcpStream`] that speaks the [`crate::proto`]
//! request/response protocol.
//!
//! This is the "thin client" half of the compute-to-data design: it ships a small profile (as TOML
//! text) or a typed read query, and receives a compact answer — it never pulls a raw UPSTREAM data
//! slice across the wire (a read verb returns exactly the query's bounded result). One client owns
//! one connection and issues requests sequentially (request, then read its response); it is not
//! internally synchronized, so share it per thread.
//!
//! # The connect handshake (PR-2)
//!
//! [`connect`](DatahubClient::connect) performs the [`Request::Hello`] / [`Response::Welcome`]
//! version handshake before returning: it sends the client's [`PROTO_VERSION`], reads the server's
//! `Welcome`, and returns `Err` (NAMING BOTH versions) if they disagree — so a stale client fails
//! with a legible message instead of a silent frame desync on the first real request. Because every
//! caller (`vike-cli`, [`RemoteHistStore`](crate::remote::RemoteHistStore)) constructs the client
//! through `connect`, they all inherit the handshake for free. The server's advertised feature list
//! is captured and readable via [`features`](DatahubClient::features).
//!
//! # Authentication (`docs/decisions/0025-datahub-remote-posture.md`)
//!
//! A datahub server is KEYED or key-less, and the client mirrors that in two constructors:
//!
//! - [`connect`](DatahubClient::connect) — the pre-existing one, unchanged against a key-less
//!   server. Against a KEYED one (which advertises [`crate::proto::FEATURE_AUTH`]) it now fails
//!   IMMEDIATELY with an actionable message, instead of connecting fine and having every verb
//!   refused one round trip later.
//! - [`connect_authed`](DatahubClient::connect_authed) — `Hello` → `Welcome{nonce}` →
//!   `Auth{scope, mac}` → `AuthOk`, signing the per-connection nonce with the scope's key. It
//!   degrades to a plain unauthenticated connect against a key-less server (which has nothing to
//!   verify a mac with), so one keyed caller works against both; check
//!   [`authenticated_scope`](DatahubClient::authenticated_scope) when that distinction matters.
//!
//! The scope chosen at connect is the connection's ceiling for its whole life. [`Scope::Observe`]
//! reads history and catalog; [`Scope::Control`] additionally admits the `Backfill` WRITE and every
//! `Run*` verb — those COMPILE CLIENT-SUPPLIED RHAI on the server, so they are not reads whatever
//! they return. `vike_datahub::server`'s `required_scope` is the authority for that mapping.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};

use vike_data::{InstrumentCoverage, SeriesCoverage, SeriesId, TsRange};
use vike_model::{Bar, QuoteTick, SymbolProperties, TradeTick};

use crate::node_auth::{self, NodeKeys, Scope};
use crate::proto::{
    read_frame, write_frame, BackfillDone, Request, Response, FEATURE_AUTH, FEATURE_BACKFILL,
    FEATURE_COVERAGE, PROTO_VERSION,
};
use crate::wire_studio::{
    WireEngineParams, WireRunResult, WireSlice, WireSpec, WireSweep, WireSweepResult,
    WireWalkforward, WireWalkforwardResult,
};

/// A connected datahub client. Wraps the underlying [`TcpStream`]; drop it to close the connection.
#[derive(Debug)]
pub struct DatahubClient {
    stream: TcpStream,
    /// The server's advertised feature strings, captured during the [`connect`](Self::connect)
    /// handshake. Empty before/without a successful `Welcome`.
    features: Vec<String>,
    /// The scope this connection authenticated under, or `None` against a key-less server (which
    /// requires and offers no authentication). See [`authenticated_scope`](Self::authenticated_scope).
    authed: Option<Scope>,
}

impl DatahubClient {
    /// Connect to a datahub server at `addr` (e.g. `"127.0.0.1:7878"`) and perform the
    /// [`Request::Hello`] / [`Response::Welcome`] version handshake (PR-2).
    ///
    /// Returns `Err` on a transport failure OR a protocol-version mismatch — the latter an
    /// [`io::ErrorKind::InvalidData`] whose message NAMES BOTH versions (client and server), so a
    /// stale binary fails legibly rather than desyncing on its first request.
    ///
    /// ⚠ Against a KEYED server (one advertising [`FEATURE_AUTH`]) this fails HERE, with an
    /// actionable message, rather than succeeding and having every verb refused later. Use
    /// [`connect_authed`](Self::connect_authed) for those. Against a key-less server — the default,
    /// and every deployment before authentication existed — behaviour is completely unchanged,
    /// which is what keeps `vike-cli backtest`, the Studio's `Backend::Remote` and
    /// [`RemoteHistStore`](crate::remote::RemoteHistStore) working untouched.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        let mut client = Self { stream, features: Vec::new(), authed: None };
        client.handshake()?;
        if client.requires_auth() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this datahub REQUIRES authentication (it advertises the `auth` feature) but no \
                 node keys were supplied. Connect with `DatahubClient::connect_authed`, and set \
                 VIKE_DATAHUB_OBSERVE_KEY / VIKE_DATAHUB_CONTROL_KEY in the credential store \
                 (`vike-cli secrets path` prints where that is)",
            ));
        }
        Ok(client)
    }

    /// Connect and AUTHENTICATE under `scope`, signing the server's per-connection nonce challenge
    /// with the matching key from `keys` (`docs/decisions/0025-datahub-remote-posture.md`).
    ///
    /// The exchange is `Hello` → `Welcome{nonce}` → `Auth{scope, mac}` → `AuthOk`, with the mac
    /// computed by [`node_auth::sign`](crate::node_auth::sign) over the DATAHUB domain separator —
    /// so the tag binds the key, the scope, the protocol version AND this one connection's nonce,
    /// and cannot be replayed onto another connection or against the tradehub node.
    ///
    /// ⚠ **A KEY-LESS server is not an error here — it is a successful unauthenticated connect.**
    /// A server that advertises no [`FEATURE_AUTH`] has no key to verify a mac against, so sending
    /// `Auth` would earn a refusal for doing the right thing; the client detects the absent
    /// advertisement, sends nothing, and returns a working connection with
    /// [`authenticated_scope`](Self::authenticated_scope) `== None`. That is what lets ONE caller
    /// (a GUI, `vike-cli`) hold keys and still work against a local key-less dev server without
    /// branching. A caller that must be sure it authenticated checks `authenticated_scope`.
    ///
    /// Fails with [`io::ErrorKind::PermissionDenied`] on a refused mac — a wrong key, a key this
    /// server does not hold for that scope, or a protocol-version skew (the version is signed).
    pub fn connect_authed<A: ToSocketAddrs>(
        addr: A,
        keys: &NodeKeys,
        scope: Scope,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        let mut client = Self { stream, features: Vec::new(), authed: None };
        let nonce = client.handshake()?;
        if !client.requires_auth() {
            // Key-less server: nothing to authenticate against. See the ⚠ above.
            return Ok(client);
        }
        let Some(nonce) = nonce else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "datahub handshake: server advertises `auth` but its Welcome carried no nonce",
            ));
        };
        let mac = node_auth::sign(
            node_auth::DATAHUB_DOMAIN,
            keys.key_for(scope),
            &nonce,
            PROTO_VERSION,
            scope,
        );
        write_frame(&mut client.stream, &Request::Auth { scope, mac })?;
        match read_frame::<_, Response>(&mut client.stream)? {
            Response::AuthOk { scope: granted } => {
                client.authed = Some(granted);
                Ok(client)
            }
            Response::AuthDenied { reason } => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("datahub auth denied ({scope:?}): {reason}"),
            )),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("datahub auth: expected AuthOk/AuthDenied, got {}", resp_kind(&other)),
            )),
        }
    }

    /// Open the connection with the version handshake: send our [`PROTO_VERSION`] in a
    /// [`Request::Hello`], read the server's [`Response::Welcome`], fail (naming both versions) on a
    /// mismatch, and otherwise store the server's advertised `features`.
    ///
    /// Returns the server's auth `nonce` — `Some` on a KEYED server, `None` on a key-less one.
    fn handshake(&mut self) -> io::Result<Option<[u8; 32]>> {
        write_frame(&mut self.stream, &Request::Hello { proto_version: PROTO_VERSION })?;
        match read_frame::<_, Response>(&mut self.stream)? {
            Response::Welcome { proto_version, features, nonce } => {
                check_proto_version(proto_version)
                    .map_err(|msg| io::Error::new(io::ErrorKind::InvalidData, msg))?;
                self.features = features;
                Ok(nonce)
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("datahub handshake: expected Welcome, got {}", resp_kind(&other)),
            )),
        }
    }

    /// The server's advertised feature strings, learned during the [`connect`](Self::connect)
    /// handshake (e.g. `"backtest"`, `"load_bars"`). Empty until a successful handshake.
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// Whether the connected server REQUIRES authentication — i.e. it advertises
    /// [`FEATURE_AUTH`]. The capability-negotiation half of the no-version-bump design: a client
    /// learns this from the `Welcome` rather than from a protocol number.
    pub fn requires_auth(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_AUTH)
    }

    /// The [`Scope`] this connection authenticated under, or `None` on a key-less server (where no
    /// authentication took place and every verb is served). A caller that must be certain it is
    /// talking to an authenticated connection checks this, not merely that
    /// [`connect_authed`](Self::connect_authed) returned `Ok`.
    pub fn authenticated_scope(&self) -> Option<Scope> {
        self.authed
    }

    /// Liveness probe: send [`Request::Ping`] and assert the server answers [`Response::Pong`].
    /// A non-`Pong` reply is an [`io::ErrorKind::InvalidData`] protocol error.
    pub fn ping(&mut self) -> io::Result<()> {
        write_frame(&mut self.stream, &Request::Ping)?;
        match read_frame::<_, Response>(&mut self.stream)? {
            Response::Pong => Ok(()),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected Pong, got {}", resp_kind(&other)),
            )),
        }
    }

    /// Ship a backtest profile (its TOML text) to the server, run it there, and return the report
    /// as JSON text. Parse it with `serde_json::from_str` once `BacktestReport` grows `Deserialize`
    /// (a later phase); for now it is the same JSON the `backtest --json` bin emits.
    ///
    /// Both a transport failure and a server-side [`Response::Error`] surface through the ONE
    /// `Err(String)` channel, so a caller has a single place to handle "no report". Any other reply
    /// (a protocol desync) is likewise reported as `Err`.
    pub fn run_backtest(&mut self, profile_toml: &str) -> Result<String, String> {
        let request = Request::RunBacktest(profile_toml.to_string());
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Report(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Report, got {}", resp_kind(&other))),
        }
    }

    /// Ship a Studio `RunSlice` (PR-3): resolve `spec`, load `slice`, backtest it server-side, and
    /// return the rendered [`WireRunResult`] — the answer, NOT the bars/ticks. `params` is the
    /// optional cost/cash override (`None` = every engine field takes `EngineParams::default()`).
    ///
    /// Both a transport failure and a server-side [`Response::Error`] (a bad script/slice/params, or
    /// a lean server that lacks `serve-datafusion`) surface through the ONE `Err(String)` channel —
    /// the error string is kind-first (`"compile: …"` / `"data: …"` / `"strategy: …"`), so a caller
    /// can classify the failure. Any other reply (a protocol desync) is likewise `Err`.
    pub fn run_slice(
        &mut self,
        spec: WireSpec,
        slice: WireSlice,
        params: Option<WireEngineParams>,
    ) -> Result<WireRunResult, String> {
        // `slice` is boxed in the variant to keep the enum small (serde-transparent — see the proto).
        let request = Request::RunSlice { spec, slice: Box::new(slice), params };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::RunResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected RunResult, got {}", resp_kind(&other))),
        }
    }

    /// Ship a Studio `RunSweep` (PR-4): resolve `spec`, load `slice`, run the parameter `sweep` grid
    /// server-side, and return the ranked [`WireSweepResult`] — the answer, NOT the bars/ticks. The
    /// server runs the EXISTING `vike_studio_core::run_sweep_slice` next to the data. `params` is the
    /// optional cost/cash override applied to EVERY grid point (`None` = every engine field takes
    /// `EngineParams::default()`, the pre-v6 behavior).
    ///
    /// Both a transport failure and a server-side [`Response::Error`] (a bad script/slice/params, or a
    /// lean server that lacks `serve-datafusion`) surface through the ONE `Err(String)` channel — the
    /// error string is kind-first. Any other reply (a protocol desync) is likewise `Err`.
    pub fn run_sweep(
        &mut self,
        spec: WireSpec,
        slice: WireSlice,
        sweep: WireSweep,
        params: Option<WireEngineParams>,
    ) -> Result<WireSweepResult, String> {
        // `slice` is boxed in the variant to keep the enum small (serde-transparent — see the proto).
        let request = Request::RunSweep { spec, slice: Box::new(slice), sweep, params };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::SweepResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SweepResult, got {}", resp_kind(&other)))
            }
        }
    }

    /// Ship a Studio `RunWalkforward` (PR-4): resolve `spec`, load `slice`, walk it forward over
    /// `walkforward.n_splits` anchored OOS windows server-side, and return the stitched
    /// [`WireWalkforwardResult`]. The server runs the EXISTING
    /// `vike_studio_core::run_walkforward_slice` next to the data. `params` is the optional cost/cash
    /// override applied to every OOS window (`None` = every engine field takes
    /// `EngineParams::default()`, the pre-v6 behavior).
    ///
    /// Both a transport failure and a server-side [`Response::Error`] (a tick/multi-symbol slice, or a
    /// lean server that lacks `serve-datafusion`) surface through the ONE `Err(String)` channel —
    /// kind-first. Any other reply (a protocol desync) is likewise `Err`.
    pub fn run_walkforward(
        &mut self,
        spec: WireSpec,
        slice: WireSlice,
        walkforward: WireWalkforward,
        params: Option<WireEngineParams>,
    ) -> Result<WireWalkforwardResult, String> {
        // `slice` is boxed in the variant to keep the enum small (serde-transparent — see the proto).
        let request = Request::RunWalkforward { spec, slice: Box::new(slice), walkforward, params };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::WalkforwardResult(result) => Ok(result),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected WalkforwardResult, got {}",
                resp_kind(&other)
            )),
        }
    }

    /// Ship a profile (its TOML text) to be run as a PARAMETER SWEEP over its own `[sweep]` table
    /// (proto v7) and return the RANKED report as JSON text — the sweep sibling of
    /// [`run_backtest`](Self::run_backtest).
    ///
    /// `rank_by` names the server-side `harness::RankMetric` (`"sharpe"` / `"return"` / `"max_dd"` /
    /// `"equity"`, case-insensitive); `None` = the `sharpe` default. Rows come back ALREADY ordered
    /// best-first, each carrying its own `BacktestReport`, so a caller renders server-computed stats
    /// rather than re-implementing a metric.
    ///
    /// Unlike [`run_sweep`](Self::run_sweep) (the Studio DTO verb) this carries the WHOLE profile,
    /// so the whole `[engine]` applies — the `fee` schedule included — and a LEAN server serves it
    /// too. Both a transport failure and a server-side [`Response::Error`] surface through the ONE
    /// `Err(String)` channel.
    pub fn run_sweep_profile(
        &mut self,
        profile_toml: &str,
        rank_by: Option<&str>,
    ) -> Result<String, String> {
        let request = Request::RunSweepProfile {
            profile_toml: profile_toml.to_string(),
            rank_by: rank_by.map(str::to_string),
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::SweepReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SweepReport, got {}", resp_kind(&other)))
            }
        }
    }

    /// Ship a profile (its TOML text) to be WALKED FORWARD over its own `[walkforward].n_splits`
    /// anchored out-of-sample windows (proto v7) and return the stitched report as JSON text — the
    /// walk-forward sibling of [`run_backtest`](Self::run_backtest).
    ///
    /// The split count lives IN the profile (there is no wire override), so one file describes the
    /// whole run. Server-side this is bar-mode + single-series only; a tick or multi-symbol profile
    /// comes back as a clean [`Response::Error`] on the ONE `Err(String)` channel, like every other
    /// failure.
    pub fn run_walkforward_profile(&mut self, profile_toml: &str) -> Result<String, String> {
        let request = Request::RunWalkforwardProfile { profile_toml: profile_toml.to_string() };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::WalkforwardReport(json) => Ok(json),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected WalkforwardReport, got {}",
                resp_kind(&other)
            )),
        }
    }

    /// Read derived OHLCV bars for `(venue, symbol, interval)` in `range` — the RPC twin of
    /// `HistStore::load_bars`. A transport failure or a server-side [`Response::Error`] surfaces
    /// through the ONE `Err(String)` channel.
    pub fn load_bars(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, String> {
        let request = Request::LoadBars {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start: range.start,
            end: range.end,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Bars(bars) => Ok(bars),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Bars, got {}", resp_kind(&other))),
        }
    }

    /// Read L1 quotes for `(venue, symbol)` in `range` — the RPC twin of `HistStore::scan_quotes`.
    pub fn scan_quotes(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, String> {
        let request = Request::ScanQuotes {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Quotes(quotes) => Ok(quotes),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Quotes, got {}", resp_kind(&other))),
        }
    }

    /// Read executed trades for `(venue, symbol)` in `range` — the RPC twin of
    /// `HistStore::scan_trades`.
    pub fn scan_trades(
        &mut self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, String> {
        let request = Request::ScanTrades {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start: range.start,
            end: range.end,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Trades(trades) => Ok(trades),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Trades, got {}", resp_kind(&other))),
        }
    }

    /// Point-in-time properties for `(venue, symbol)` at or before `ts` — the RPC twin of
    /// `HistStore::properties_as_of`. `Ok(None)` = nothing recorded at or before `ts`.
    pub fn properties_as_of(
        &mut self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, String> {
        let request =
            Request::PropertiesAsOf { venue: venue.to_string(), symbol: symbol.to_string(), ts };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            // Unbox on the way out — the box was a server-side enum-size optimization only.
            Response::Properties(props) => Ok(props.map(|b| *b)),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Properties, got {}", resp_kind(&other)))
            }
        }
    }

    /// Enumerate every stored series (`HistStore::list_series` over RPC — PR-6 store metadata).
    pub fn list_series(&mut self) -> Result<Vec<SeriesId>, String> {
        write_frame(&mut self.stream, &Request::ListSeries).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::SeriesList(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesList, got {}", resp_kind(&other)))
            }
        }
    }

    /// Every stored series with its cheap coverage (`HistStore::inventory` over RPC — PR-6).
    pub fn inventory(&mut self) -> Result<Vec<(SeriesId, SeriesCoverage)>, String> {
        write_frame(&mut self.stream, &Request::Inventory).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Inventory(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Inventory, got {}", resp_kind(&other))),
        }
    }

    /// The gap ranges in one series' recorded span (`HistStore::series_gaps` over RPC — PR-6).
    pub fn series_gaps(&mut self, id: &SeriesId) -> Result<Vec<(i64, i64)>, String> {
        let request = Request::SeriesGaps { id: id.clone() };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::SeriesGaps(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected SeriesGaps, got {}", resp_kind(&other)))
            }
        }
    }

    /// The CROSS-KIND coverage report (`HistStore::coverage_report` over RPC — split-plane spec
    /// §6 Q2): every instrument with its `trade`/`quote`/`book`/`depth` day sets lined up, which is
    /// what the Data Manager's "Partial" column folds
    /// ([`InstrumentCoverage::partial_days`](vike_data::InstrumentCoverage::partial_days)).
    ///
    /// ⚠ CAPABILITY-CHECKED, NOT VERSION-CHECKED, exactly like [`Self::backfill`]: the verb shipped
    /// without a `PROTO_VERSION` bump, so the version handshake cannot protect it. The method
    /// refuses CLIENT-SIDE — sending nothing — unless the server's `Welcome` advertised
    /// [`FEATURE_COVERAGE`]. Unlike backfill, EVERY server built from this protocol's `serve`
    /// advertises it (it is a plain trait verb, not a mounted table), so a refusal here means
    /// exactly one thing: the peer predates the verb. The caller renders that as an honest note,
    /// never an empty column — an empty `Ok` would say "nothing is partial", which is a different
    /// and possibly false fact.
    pub fn coverage_report(&mut self) -> Result<Vec<InstrumentCoverage>, String> {
        if !self.features.iter().any(|f| f == FEATURE_COVERAGE) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_COVERAGE}` (advertised: {:?}) — \
                 nothing was sent. The cross-kind coverage report needs a newer server; this verb \
                 is capability-negotiated, not version-gated.",
                self.features
            ));
        }
        write_frame(&mut self.stream, &Request::Coverage).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Coverage(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Coverage, got {}", resp_kind(&other))),
        }
    }

    /// Enumerate the compiled native backtest-strategy roster server-side
    /// (`vike_backtest::harness::STRATEGIES` over RPC) — the names a profile's `strategy.name` can
    /// resolve. A transport failure or a server-side [`Response::Error`] surfaces through the ONE
    /// `Err(String)` channel; any other reply (a protocol desync) is likewise `Err`.
    pub fn list_strategies(&mut self) -> Result<Vec<String>, String> {
        write_frame(&mut self.stream, &Request::ListStrategies).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::Strategies(v) => Ok(v),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected Strategies, got {}", resp_kind(&other)))
            }
        }
    }

    /// Backfill-on-demand (split-plane REQ-9): ask the SERVER to fetch `(venue, symbol,
    /// interval)` klines over the inclusive `[start_ms, end_ms]` epoch-ms range from the venue's
    /// public REST into ITS store, and return the [`BackfillDone`] outcome once the rows are
    /// written — "History is fetched by the backend, once, into the store — clients request,
    /// never fetch". v1 is synchronous per request over a BOUNDED range (see
    /// [`Request::Backfill`] for why there is no job/progress story yet).
    ///
    /// ⚠ CAPABILITY-CHECKED, NOT VERSION-CHECKED: this verb shipped without a `PROTO_VERSION`
    /// bump, so the version handshake cannot protect it. The method refuses CLIENT-SIDE — sending
    /// nothing — unless the server's `Welcome` advertised [`FEATURE_BACKFILL`], which only a
    /// `backfill-serve` build with collectors mounted does. (Even a raw caller that skips this
    /// check degrades legibly: an old server answers the unknown variant with a clean
    /// `Response::Error` — the PR-2 framing/decode split.) Both a transport failure and a
    /// server-side [`Response::Error`] surface through the ONE `Err(String)` channel.
    pub fn backfill(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<BackfillDone, String> {
        if !self.features.iter().any(|f| f == FEATURE_BACKFILL) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_BACKFILL}` (advertised: {:?}) — \
                 nothing was sent. Backfill needs a server built with `--features backfill-serve` \
                 (or a newer server; this verb is capability-negotiated, not version-gated).",
                self.features
            ));
        }
        let request = Request::Backfill {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start: start_ms,
            end: end_ms,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::BackfillDone(done) => Ok(done),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected BackfillDone, got {}", resp_kind(&other)))
            }
        }
    }
}

/// The variant NAME of a response (no payload) — for protocol-desync error messages, so we never
/// stringify a whole (possibly large) `Bars`/`Quotes`/`Trades` payload just to report "unexpected
/// variant".
fn resp_kind(r: &Response) -> &'static str {
    match r {
        Response::Welcome { .. } => "Welcome",
        Response::AuthOk { .. } => "AuthOk",
        Response::AuthDenied { .. } => "AuthDenied",
        Response::Pong => "Pong",
        Response::Report(_) => "Report",
        Response::RunResult(_) => "RunResult",
        Response::SweepResult(_) => "SweepResult",
        Response::WalkforwardResult(_) => "WalkforwardResult",
        Response::SweepReport(_) => "SweepReport",
        Response::WalkforwardReport(_) => "WalkforwardReport",
        Response::Error(_) => "Error",
        Response::Bars(_) => "Bars",
        Response::Quotes(_) => "Quotes",
        Response::Trades(_) => "Trades",
        Response::Properties(_) => "Properties",
        Response::SeriesList(_) => "SeriesList",
        Response::Inventory(_) => "Inventory",
        Response::SeriesGaps(_) => "SeriesGaps",
        Response::Coverage(_) => "Coverage",
        Response::Strategies(_) => "Strategies",
        Response::BackfillDone(_) => "BackfillDone",
    }
}

/// Compare the server's advertised protocol version against the client's [`PROTO_VERSION`],
/// producing the legible mismatch message (which NAMES BOTH versions) or `Ok(())` on a match.
///
/// Split out as a pure function so the version-compare contract is unit-testable without a socket;
/// the `handshake` wraps the `Err` message into an [`io::ErrorKind::InvalidData`] error.
fn check_proto_version(server_version: u32) -> Result<(), String> {
    if server_version == PROTO_VERSION {
        Ok(())
    } else {
        Err(format!(
            "datahub protocol version mismatch: client speaks {PROTO_VERSION}, \
             server speaks {server_version}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_proto_version_accepts_a_matching_server() {
        assert!(check_proto_version(PROTO_VERSION).is_ok());
    }

    /// A mismatched server version yields an error naming BOTH the client's and the server's number,
    /// so a stale binary reports something a human can act on.
    #[test]
    fn check_proto_version_flags_a_mismatch_naming_both() {
        let server_version = PROTO_VERSION + 1;
        let err = check_proto_version(server_version).unwrap_err();
        assert!(err.contains(&PROTO_VERSION.to_string()), "names the client version: {err}");
        assert!(err.contains(&server_version.to_string()), "names the server version: {err}");
    }
}
