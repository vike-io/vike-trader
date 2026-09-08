//! The vike-datahub wire protocol: length-prefixed `serde_json` frames over a blocking
//! `std::net` byte stream, and the request/response schema that rides them.
//!
//! # Framing
//!
//! One frame is a big-endian `u32` byte length followed by exactly that many bytes of UTF-8 JSON.
//! [`write_frame`] serializes a value, writes the length prefix, writes the bytes, and flushes;
//! [`read_frame`] reads the 4-byte length, bounds-checks it against [`MAX_FRAME_LEN`] (so a hostile
//! or corrupt peer cannot make us pre-allocate an unbounded buffer and OOM), then reads and decodes
//! the body. serde/JSON errors map to [`io::Error`] so a caller has ONE error channel.
//!
//! No length-delimited codec crate and no async runtime are used — deliberately: the workspace
//! `deny.toml` bans a second HTTP/WS/RPC transport stack, so the transport here is bare
//! `std::net` + `serde_json`, nothing more.
//!
//! # Compute-to-data contract
//!
//! This is the "compute-to-data" (Design B) seam: a client ships a small CONFIG next to where the
//! data lives and gets back a compact ANSWER, never a raw upstream data slice.
//!
//! - [`Request::RunBacktest`] ships a profile (as its TOML text) and gets a [`Response::Report`]
//!   (as JSON text) — the heavy history stays server-side; only the profile in and the metrics
//!   summary out cross the wire.
//! - [`Request::RunSlice`] (PR-3) is the Studio's compute-to-data run: it ships the small
//!   [`WireSpec`]/[`WireSlice`]/[`WireEngineParams`] DTOs (see [`crate::wire_studio`]) and gets back a
//!   [`Response::RunResult`] — the rendered [`WireRunResult`], NOT the bars/ticks. The server runs the
//!   EXISTING `vike_studio_core::run::run_slice` next to the data. It is served ONLY by a
//!   `serve-datafusion` build; a lean build answers a `RunSlice` request with a clean
//!   [`Response::Error`] instead (the arm still decodes, so a mismatched client is never dropped).
//! - [`Request::RunSweep`] / [`Request::RunWalkforward`] (PR-4) are the sweep and walk-forward
//!   siblings of `RunSlice`: they ship the same `spec`/`slice` DTOs plus a [`WireSweep`] grid or a
//!   [`WireWalkforward`] split-count — and, since proto v6, the same OPTIONAL
//!   `params: Option<WireEngineParams>` cost/cash override `RunSlice` carries — and get back a
//!   [`Response::SweepResult`] / [`Response::WalkforwardResult`] — the ranked/stitched ANSWER, never
//!   the raw slices. Same `serve-datafusion`-only serving as `RunSlice`. These are the STUDIO
//!   verbs: the GUI holds `spec`/`slice` DTOs (a two-click picker), not a profile file.
//! - [`Request::RunSweepProfile`] / [`Request::RunWalkforwardProfile`] (v7) are the PROFILE-shaped
//!   twins of those two, and the sweep / walk-forward siblings of [`Request::RunBacktest`]: they
//!   ship the profile's **TOML text** verbatim and get back [`Response::SweepReport`] /
//!   [`Response::WalkforwardReport`] as JSON TEXT. The SERVER parses with
//!   `BacktestProfile::from_toml_str` and runs `vike_backtest::harness::run_sweep` /
//!   `harness::run_walkforward`, so the WHOLE `[engine]` surface applies — the `fee` SCHEDULE,
//!   `[engine.impact]`, `[engine.resolution]`, `[risk]`, `snap_to_properties`, tick mode,
//!   cross-venue `[[data.series]]` — none of which a `WireSlice` + `WireEngineParams` pair can
//!   carry, and the profile is parsed by ONE parser instead of re-parsed client-side. Like
//!   `RunBacktest` (and unlike the Studio verbs) they are served on EVERY build: the harness
//!   compiles against the `HistStore` trait, DataFusion-free.
//! - The READ verbs ([`Request::LoadBars`] / [`Request::ScanQuotes`] / [`Request::ScanTrades`] /
//!   [`Request::PropertiesAsOf`]) mirror the four GUI-relevant [`vike_data::HistStore`] read
//!   methods; they carry typed params and return the exact typed `vike-model` result
//!   ([`Response::Bars`] / [`Response::Quotes`] / [`Response::Trades`] / [`Response::Properties`]).
//!   These are the [`RemoteHistStore`](crate::remote::RemoteHistStore) seam: a chart's bars ARE the
//!   answer, so returning them across the wire is still "answers, not raw slices" — the results are
//!   bounded by the query, not the whole store.
//!
//! # Protocol version & the decode-vs-drop contract (PR-2)
//!
//! [`PROTO_VERSION`] tags the wire schema. A client opens with [`Request::Hello`] and the server
//! answers [`Response::Welcome`] carrying ITS version + the served-verb `features`; the CLIENT
//! compares and fails loudly (naming BOTH numbers) on a mismatch, so a stale client gets a legible
//! error instead of a silent frame desync once verbs multiply. Bump [`PROTO_VERSION`] on any change
//! to the [`Request`] / [`Response`] schema that an old peer could not DECODE — see that constant's
//! own log, where the last three additions each argue why they were not such a change.
//!
//! ⚠ **Whether the handshake is optional now depends on the SERVER's keys**
//! (`docs/decisions/0025-datahub-remote-posture.md`). On a key-LESS server it is optional exactly as
//! it always was — a normal request sent without a prior `Hello` is served unchanged, and `Hello`
//! negotiates the version rather than gating the connection. On a KEYED server (one advertising
//! [`FEATURE_AUTH`]) `Hello` is MANDATORY and must be followed by [`Request::Auth`]; every other
//! verb before [`Response::AuthOk`] is refused. `vike_datahub::server`'s module doc is the authority
//! for both arms, and its `required_scope` for which verb each scope may send.
//!
//! **Decode-vs-drop.** Reading a frame and DECODING it are split: [`read_frame_raw`] returns the
//! framed body bytes (behind the same OOM guard) WITHOUT decoding, and [`read_frame`] is that plus
//! `serde_json::from_slice`. A server MUST use the raw read so a well-framed body that fails to
//! decode into a known [`Request`] variant (an unknown/incompatible verb from a newer client) is
//! answered with [`Response::Error`] and the connection SURVIVES — never dropped. A decode error is
//! a bad *request*, not a bad *connection*; fusing decode into the read (as [`read_frame`] does)
//! makes the two indistinguishable, which is exactly the connection-killing footgun PR-2 removes.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use vike_data::{InstrumentCoverage, SeriesCoverage, SeriesId};

/// The removal vocabulary, RE-EXPORTED rather than restated.
///
/// ⚠ This exists so a consumer can CONSTRUCT a [`Request::DeleteSeries`] without a `vike-data`
/// dependency EDGE of its own. `vike-cli` is the caller that needs it and the reason it is worth a
/// paragraph: that crate's identity is being DataFusion-free, argued edge by edge in its manifest,
/// and it takes `vike-data` as a DEV-dependency only — so `vike_data::removal::SeriesSelector`
/// cannot be NAMED in its library code, while reading the FIELDS of a value handed to it needs no
/// edge at all (which is how it already consumes `SeriesId`).
///
/// A re-export adds no package and no edge: `vike-data` with DEFAULT features is already in that
/// graph through this crate. The alternative — three flattened DTOs here plus the conversions —
/// would be a second shape for the same facts, which is exactly what this wire's typed params
/// exist to avoid.
pub use vike_data::removal::{RemovalOutcome, RemovalPlan, SeriesSelector, describe_id};
use vike_model::{Bar, QuoteTick, SymbolProperties, TradeTick};

// The capability ceiling carried by `Request::Auth` / `Response::AuthOk`. Defined in
// `crate::node_auth` (the SHARED primitive both localhost services sign with) and re-exported here
// so `proto::Scope` resolves on this protocol exactly as it does on the tradehub node's.
pub use crate::node_auth::Scope;

use crate::wire_studio::{
    WireEngineParams, WireRunResult, WireSlice, WireSpec, WireSweep, WireSweepResult,
    WireWalkforward, WireWalkforwardResult,
};

/// Upper bound on a single frame's body length (64 MiB). A declared length above this is rejected
/// by [`read_frame`] before any allocation, so a bad peer cannot drive us to OOM on a bogus prefix.
/// Comfortably larger than any real profile (in) or report (out), which are kilobytes; a bars/tick
/// answer for a chart's visible range is likewise bounded well under this.
pub const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

/// The datahub wire protocol version, negotiated by the [`Request::Hello`] / [`Response::Welcome`]
/// handshake (PR-2). A client sends its version in `Hello`; the server echoes ITS version in
/// `Welcome`; the client fails the connection with a legible, both-numbers error on a mismatch (see
/// `DatahubClient::connect`). Bump this on ANY change to the [`Request`] / [`Response`] schema so a
/// stale peer is rejected loudly rather than desyncing on the first incompatible frame.
///
/// - `1` — the PR-1/PR-2 schema (Hello/Ping/RunBacktest + the four read verbs).
/// - `2` — PR-3 added the Studio [`Request::RunSlice`] verb + its [`Response::RunResult`] answer.
/// - `3` — PR-4 added the Studio [`Request::RunSweep`] / [`Request::RunWalkforward`] verbs + their
///   [`Response::SweepResult`] / [`Response::WalkforwardResult`] answers.
/// - `4` — PR-6 added the store-metadata verbs [`Request::ListSeries`] / [`Request::Inventory`] /
///   [`Request::SeriesGaps`] + their [`Response::SeriesList`] / [`Response::Inventory`] /
///   [`Response::SeriesGaps`] answers (so `RemoteHistStore` can serve the Data-Manager catalog).
/// - `5` — added the [`Request::ListStrategies`] roster verb + its [`Response::Strategies`] answer
///   (the compiled native backtest-strategy names, so an agent can discover which strategies exist).
/// - `6` — added the OPTIONAL `params: Option<WireEngineParams>` cost/cash field to
///   [`Request::RunSweep`] / [`Request::RunWalkforward`] (the same DTO [`Request::RunSlice`] already
///   carries), so a remote sweep / walk-forward honors a profile's `[engine]` cash/fee_rate/slippage
///   instead of always running default engine params. The field is `#[serde(default)]`, so an old
///   frame that omits it decodes as `None` (backward-compatible on the wire).
/// - `7` — added the PROFILE-shaped [`Request::RunSweepProfile`] / [`Request::RunWalkforwardProfile`]
///   verbs + their [`Response::SweepReport`] / [`Response::WalkforwardReport`] answers: the sweep /
///   walk-forward siblings of `RunBacktest`, carrying the profile TOML verbatim instead of
///   re-parsed DTOs. Purely ADDITIVE — the Studio `RunSweep`/`RunWalkforward` verbs are unchanged.
/// - still `7` — the backfill-on-demand verb ([`Request::Backfill`] / [`Response::BackfillDone`])
///   was added WITHOUT a bump, negotiated per-capability through [`FEATURE_BACKFILL`] in
///   `Welcome.features` instead — see that constant for why a bump would be the wrong tool for a
///   purely additive, feature-gated verb.
/// - still `7` — the cross-kind coverage verb ([`Request::Coverage`] / [`Response::Coverage`],
///   split-plane spec §6 Q2) followed the SAME precedent: additive variants, negotiated through
///   [`FEATURE_COVERAGE`], no bump. See that constant.
/// - still `7` — the NodeKeys AUTHENTICATION handshake ([`Request::Auth`], [`Response::AuthOk`] /
///   [`Response::AuthDenied`], `Welcome.nonce`) likewise did NOT bump, and here the argument is
///   the strongest it gets: **the version is folded INTO the signed auth mac**
///   ([`crate::node_auth::sign`]), so a bump breaks the handshake against every peer not upgraded
///   in lockstep — the identical reasoning `vike_tradehub_client::proto`'s `FEATURE_STRATEGY_VERBS`
///   spells out for the node protocol. Every frame decodes cleanly in BOTH directions: the new
///   `Request::Auth` variant is additive (an old server answers `Response::Error`, never a drop),
///   and `Welcome.nonce` is `Option` + `skip_serializing_if`, so a key-less server's `Welcome`
///   bytes are unchanged and an old server's `Welcome` still decodes for a new client. A version
///   bump is the tool for a change an old peer CANNOT decode; this is not one. [`FEATURE_AUTH`] is
///   the negotiation instead.
pub const PROTO_VERSION: u32 = 7;

/// The `Welcome.features` capability string for the backfill-on-demand verb
/// ([`Request::Backfill`] / [`Response::BackfillDone`] — split-plane REQ-9).
///
/// ⚠ This verb deliberately shipped WITHOUT a [`PROTO_VERSION`] bump. A bump fails EVERY
/// old-client/new-server pair at the handshake, including the many pairs that never touch
/// backfill; the `features` list exists precisely so a purely ADDITIVE verb can be negotiated
/// per-capability instead (the forward-compat room [`Response::Welcome`] reserved). The contract
/// has three legs, each tested:
///
/// - a server advertises this string ONLY when it actually holds collectors (a `backfill-serve`
///   build with a table mounted);
/// - the CLIENT checks the advertisement and refuses locally — without sending — when it is
///   absent (`DatahubClient::backfill`);
/// - underneath both, a server that predates the verb answers the unknown variant with a clean
///   `Response::Error` rather than dropping the connection or hanging (the PR-2 framing/decode
///   split), so even a client that skips the check degrades legibly.
pub const FEATURE_BACKFILL: &str = "backfill";

/// The `Welcome.features` capability string for the cross-kind coverage verb
/// ([`Request::Coverage`] / [`Response::Coverage`] — split-plane spec §6 Q2, the Data-Manager's
/// "Partial" column over the wire).
///
/// ⚠ Same reasoning as [`FEATURE_BACKFILL`], and deliberately the same shape: purely ADDITIVE
/// variants, so a [`PROTO_VERSION`] bump would fail every old-client/new-server pair — including
/// every pair that never opens the Data Manager — to protect one column. The capability list is
/// the tool built for this.
///
/// Where the two verbs DIFFER: `backfill` is advertised per MOUNTED TABLE (a runtime fact — a
/// server can be built with the collectors and still not have wired them), while coverage is a
/// plain [`vike_data::HistStore`] trait verb like `inventory`/`series_gaps`, so EVERY build that
/// serves at all serves it and it is advertised unconditionally. The negotiation therefore answers
/// exactly one question: *is this server older than the verb?*
///
/// The three legs, each tested:
///
/// - every server built from this crate's `serve` advertises it (`served_features`);
/// - the CLIENT checks the advertisement and refuses locally — without sending — when it is absent
///   (`DatahubClient::coverage_report`), which is what lets the GUI render an honest note instead
///   of an empty column;
/// - underneath both, a server that predates the verb answers the unknown variant with a clean
///   `Response::Error` rather than dropping the connection (the PR-2 framing/decode split).
pub const FEATURE_COVERAGE: &str = "coverage";

/// The `Welcome.features` capability string a server advertises when it is KEYED — i.e. it holds
/// at least one [`crate::node_auth::NodeKeys`] scope key and therefore REQUIRES a
/// [`Request::Auth`] before it will answer any other verb
/// (`docs/decisions/0025-datahub-remote-posture.md`, the adopting PR).
///
/// It is the CLIENT's signal that authentication is mandatory on this connection, and it is what
/// makes the whole feature backward-compatible in both directions:
///
/// - **Key-LESS server (the default, and every deployment before this PR).** It does not advertise
///   this string and its `Welcome.nonce` is `None`, so its handshake bytes are IDENTICAL to the
///   pre-auth protocol and every existing client — `vike-cli backtest`, the Studio's
///   `Backend::Remote`, [`RemoteHistStore`](crate::remote::RemoteHistStore), the GUI's store branch
///   — keeps working with no change at all. Turning auth ON is writing two keys into the credential
///   store; there is no other switch.
/// - **Keyed server, old client.** The client ignores the unknown feature string and the extra
///   `nonce` field (serde skips unknown fields), sends a normal verb, and is answered with a clean
///   refusal naming what is missing rather than being dropped or desynced.
/// - **Keyed server, new client.** [`crate::DatahubClient::connect_authed`] signs the nonce; the
///   plain [`crate::DatahubClient::connect`] sees this string with no keys in hand and fails at the
///   handshake with an actionable message instead of at the first verb with a confusing one.
///
/// ⚠ Like [`FEATURE_BACKFILL`], this shipped WITHOUT a [`PROTO_VERSION`] bump, and here the
/// argument is stronger than convenience: the version is folded INTO the signed auth mac
/// ([`crate::node_auth::sign`]), so a bump would break the handshake against every peer that has
/// not been upgraded in lockstep — including for the key-less majority that never authenticates at
/// all. Capability negotiation is the designed forward-compat hook; a version bump is the tool for
/// a change an old peer cannot decode, and every frame here decodes cleanly both ways.
pub const FEATURE_AUTH: &str = "auth";

/// The `Welcome.features` capability string for the DESTRUCTIVE store verb
/// ([`Request::DeleteSeries`] / [`Response::Deleted`]).
///
/// ⚠ **Advertised ONLY by a KEYED server, and that is a POSTURE decision rather than a capability
/// one.** Every other feature here answers "is this server new enough / built with the collectors";
/// this one answers "does this server have any way to say no". `docs/decisions/0025-datahub-remote-posture.md`
/// records the reason its whole argument for authentication turned on the BACKFILL write verb:
/// *"A surface with a write verb and no way to say 'reads yes, writes no' cannot be handed even a
/// trusted LAN."* A delete is that argument's sharper form — `Backfill` writes rows that can be
/// re-fetched, a delete destroys the only copy — and the default datahub is KEY-LESS, where the
/// `Hello` handshake *"informs, it does not gate"* and every verb is served to whoever reaches the
/// socket.
///
/// So the rule is unconditional and has three legs, each tested:
///
/// - a KEY-LESS server never advertises this string and REFUSES the verb outright, whatever the
///   request says — there is no flag, no override and no `--allow` for it;
/// - a KEYED server advertises it, and `crates/vike-datahub/src/server.rs`'s `required_scope` puts
///   it in the CONTROL scope, so an Observe connection cannot reach it either;
/// - the CLIENT checks the advertisement and refuses LOCALLY — without sending — when it is absent,
///   so an operator learns "that server has no keys" rather than "unknown request".
///
/// ⚠ Like [`FEATURE_BACKFILL`], this shipped WITHOUT a [`PROTO_VERSION`] bump: the variants are
/// purely ADDITIVE, a server that predates them answers a clean [`Response::Error`], and a bump
/// would fail every old-client/new-server pair at the handshake — including every pair that will
/// never delete anything.
pub const FEATURE_DELETE_SERIES: &str = "delete_series";

/// A client-to-server request.
///
/// Two payload styles ride here, each for a reason:
///
/// - [`Request::RunBacktest`] carries the profile as its **TOML text** (not the `vike_backtest`
///   structs): the server parses+validates it with `BacktestProfile::from_toml_str` — the same path
///   a `.toml` file takes — which keeps the wire schema decoupled from `vike-backtest`'s internal
///   serde surface (deserialize-only on the profile, serialize-only on the report), so neither crate
///   grows derives it does not otherwise want and the protocol stays stable across engine refactors.
/// - The READ verbs carry **typed params** and mirror [`vike_data::HistStore`]'s read methods
///   one-for-one. The `(venue, symbol, interval, ts)` params are plain scalars/strings; the
///   inclusive-range bound is carried as decomposed `start` / `end` `Option<i64>` fields rather than
///   a `vike_data::TsRange` because `TsRange` does not derive serde in vike-data — the server rebuilds
///   the `TsRange` from the two fields. The results are the `vike-model` value types (`Bar` /
///   `QuoteTick` / `TradeTick` / `SymbolProperties`), which already derive serde in vike-model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// The version-handshake opener: declare the client's [`PROTO_VERSION`]. Answered by
    /// [`Response::Welcome`] (the server's version, its served-verb feature list, and — on a KEYED
    /// server — the per-connection auth `nonce`).
    ///
    /// ⚠ **Whether it is OPTIONAL now depends on the server's keys**, and this is the one contract
    /// this PR flips (`docs/decisions/0025-datahub-remote-posture.md`):
    ///
    /// - **Key-LESS server** (no [`crate::node_auth::NodeKeys`] configured — the default, and every
    ///   deployment before this PR): unchanged. A normal request sent without a prior `Hello` is
    ///   served exactly as before; `Hello` negotiates the version, it does not gate the connection.
    /// - **KEYED server**: `Hello` is MANDATORY and must be the FIRST frame, followed by
    ///   [`Request::Auth`]. Any other verb before [`Response::AuthOk`] is refused with
    ///   [`Response::AuthDenied`] and the connection closes.
    Hello {
        /// The client's [`PROTO_VERSION`].
        proto_version: u32,
    },
    /// Answer a KEYED server's nonce challenge (the second and last pre-auth frame): `mac` is the
    /// HMAC over `(key_for(scope), Welcome.nonce, proto_version, scope)` under the DATAHUB domain
    /// separator, per [`crate::node_auth::sign`]. Answered by [`Response::AuthOk`] or
    /// [`Response::AuthDenied`].
    ///
    /// `scope` is the capability ceiling for the whole connection, and it is bound INTO the mac —
    /// see `vike_datahub::server`'s `required_scope`, which is the ONE authority mapping each verb
    /// to the scope that may send it. In short: [`Scope::Observe`] reads history and catalog;
    /// [`Scope::Control`] additionally admits [`Request::Backfill`] (which WRITES the store) and
    /// every `Run*` verb (which COMPILES client-supplied Rhai on the server).
    ///
    /// Sent only against a server advertising [`FEATURE_AUTH`]; a key-less server has nothing to
    /// verify it against and answers [`Response::AuthDenied`] saying so.
    Auth {
        /// The capability ceiling this client is authenticating under.
        scope: Scope,
        /// The HMAC-SHA256 tag over the connection's nonce challenge (raw bytes; serialized as a
        /// JSON array of `u8`).
        mac: Vec<u8>,
    },
    /// Liveness probe — answered by [`Response::Pong`].
    Ping,
    /// Run one backtest, given the profile as its **TOML text**. The server parses+validates it via
    /// `BacktestProfile::from_toml_str`, runs it next to the data, and replies with
    /// [`Response::Report`] (or [`Response::Error`]).
    RunBacktest(String),
    /// The Studio compute-to-data run (PR-3): resolve `spec`, load `slice`, backtest it, and reply
    /// with [`Response::RunResult`] — the rendered answer, NOT the bars/ticks. The `spec`/`slice`/
    /// `params` DTOs (see [`crate::wire_studio`]) are converted to the studio/engine types at the
    /// server boundary, which runs the EXISTING `vike_studio_core::run::run_slice`. Served ONLY by a
    /// `serve-datafusion` build; a lean build decodes the request but answers [`Response::Error`].
    RunSlice {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: WireSpec,
        /// The data window (venue + symbols + interval + range + bars-vs-ticks). BOXED to keep this
        /// variant small: [`WireSlice`] is the largest field (~112 bytes), so an inline `RunSlice`
        /// would exceed `clippy::large_enum_variant`'s 200-byte bar (the same reason
        /// [`Response::Properties`] boxes `SymbolProperties`). serde treats `Box<T>` transparently,
        /// so the wire shape is identical to an unboxed `WireSlice` — the box is in-memory only.
        slice: Box<WireSlice>,
        /// Cost/cash overrides; `None` = every engine field takes `EngineParams::default()`.
        params: Option<WireEngineParams>,
    },
    /// The Studio SWEEP compute-to-data run (PR-4): resolve `spec`, load `slice`, run the parameter
    /// `sweep` grid next to the data, and reply with [`Response::SweepResult`] — the ranked answer,
    /// NOT the bars/ticks. Converted to the studio/engine types at the server boundary, which runs the
    /// EXISTING `vike_studio_core::run_sweep_slice`. Served ONLY by a `serve-datafusion` build; a lean
    /// build decodes the request but answers [`Response::Error`].
    RunSweep {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: WireSpec,
        /// The data window. BOXED to keep this variant small, exactly like [`Request::RunSlice`] —
        /// serde treats `Box<T>` transparently, so the wire shape is identical to an unboxed `WireSlice`.
        slice: Box<WireSlice>,
        /// The parameter grid — each `(name, values)` axis overrides `strategy.params.<name>`.
        sweep: WireSweep,
        /// Cost/cash overrides applied to EVERY grid point; `None` = every engine field takes
        /// `EngineParams::default()` (the pre-v6 behavior). `#[serde(default)]` so an older frame
        /// that omits it still decodes (as `None`).
        #[serde(default)]
        params: Option<WireEngineParams>,
    },
    /// The Studio WALK-FORWARD compute-to-data run (PR-4): resolve `spec`, load `slice`, walk it
    /// forward over `walkforward.n_splits` anchored out-of-sample windows next to the data, and reply
    /// with [`Response::WalkforwardResult`]. Runs the EXISTING
    /// `vike_studio_core::run_walkforward_slice` at the boundary. Served ONLY by a `serve-datafusion`
    /// build; a lean build decodes the request but answers [`Response::Error`].
    RunWalkforward {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: WireSpec,
        /// The data window; BOXED to keep this variant small, exactly like [`Request::RunSlice`].
        slice: Box<WireSlice>,
        /// The walk-forward config (number of anchored OOS splits).
        walkforward: WireWalkforward,
        /// Cost/cash overrides applied to every OOS window; `None` = every engine field takes
        /// `EngineParams::default()` (the pre-v6 behavior). `#[serde(default)]` so an older frame
        /// that omits it still decodes (as `None`).
        #[serde(default)]
        params: Option<WireEngineParams>,
    },
    /// Run a parameter SWEEP from a profile's own `[sweep]` table, given the profile as its **TOML
    /// text** (v7) — the sweep sibling of [`Request::RunBacktest`], and the profile-shaped twin of
    /// the Studio's [`Request::RunSweep`].
    ///
    /// The server parses+validates with `BacktestProfile::from_toml_str` (the SAME path a `.toml`
    /// file takes), expands the grid and runs it next to the data with
    /// `vike_backtest::harness::run_sweep`, and replies with [`Response::SweepReport`] — the
    /// server-RANKED report as JSON text. Because the whole profile crosses the wire, the whole
    /// `[engine]` applies (fee schedule included) and the client needs no profile→DTO mapping and
    /// no metric math of its own.
    ///
    /// Served on EVERY build (the harness is DataFusion-free), exactly like `RunBacktest`.
    RunSweepProfile {
        /// The profile's TOML text, shipped verbatim. Must carry a `[sweep]` table.
        profile_toml: String,
        /// Which `harness::RankMetric` ranks the rows — `"sharpe"` / `"return"` / `"max_dd"` /
        /// `"equity"`, case-insensitive (`RankMetric::from_str_ci`). `None` = `"sharpe"`, the
        /// `backtest --rank-by` default. An unrecognized name is a [`Response::Error`], never a
        /// silent fallback. `#[serde(default)]` so a frame that omits it decodes as `None`.
        #[serde(default)]
        rank_by: Option<String>,
    },
    /// Run an anchored WALK-FORWARD from a profile's own `[walkforward]` table, given the profile
    /// as its **TOML text** (v7) — the walk-forward sibling of [`Request::RunBacktest`], and the
    /// profile-shaped twin of the Studio's [`Request::RunWalkforward`].
    ///
    /// The server parses+validates the profile, then runs `vike_backtest::harness::run_walkforward`
    /// (bar mode, ONE series, `WalkMode::Anchored`) next to the data and replies with
    /// [`Response::WalkforwardReport`] — the stitched report as JSON text. Served on EVERY build.
    RunWalkforwardProfile {
        /// The profile's TOML text, shipped verbatim. Must carry a `[walkforward]` table (the
        /// split count lives IN the profile — there is no wire override).
        profile_toml: String,
    },
    /// Read derived OHLCV bars — mirrors `HistStore::load_bars(venue, symbol, interval, range)`.
    /// Answered by [`Response::Bars`] (or [`Response::Error`]).
    LoadBars {
        /// Venue partition (e.g. `"binance"`).
        venue: String,
        /// Symbol partition (e.g. `"BTCUSDT"`).
        symbol: String,
        /// Bar interval (e.g. `"1d"`).
        interval: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded — the low half of a
        /// `vike_data::TsRange`, decomposed because `TsRange` is not serde in vike-data.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
    },
    /// Read L1 quotes — mirrors `HistStore::scan_quotes(venue, symbol, range)`. Answered by
    /// [`Response::Quotes`] (or [`Response::Error`]).
    ScanQuotes {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
    },
    /// Read executed trades — mirrors `HistStore::scan_trades(venue, symbol, range)`. Answered by
    /// [`Response::Trades`] (or [`Response::Error`]).
    ScanTrades {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
    },
    /// Point-in-time properties lookup — mirrors `HistStore::properties_as_of(venue, symbol, ts)`.
    /// Answered by [`Response::Properties`] (or [`Response::Error`]).
    PropertiesAsOf {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// As-of instant (epoch-ms): the most-recent properties observed at or before this.
        ts: i64,
    },
    /// Enumerate every stored series — mirrors `HistStore::list_series()`. Answered by
    /// [`Response::SeriesList`] (or [`Response::Error`]). (PR-6: store-metadata verbs.)
    ListSeries,
    /// Every stored series with its cheap coverage — mirrors `HistStore::inventory()`. Answered by
    /// [`Response::Inventory`] (or [`Response::Error`]).
    Inventory,
    /// The gap ranges in one series' recorded span — mirrors `HistStore::series_gaps(id)`. Answered
    /// by [`Response::SeriesGaps`] (or [`Response::Error`]). `id` rides whole (`SeriesId` is serde).
    SeriesGaps {
        /// The series whose day-coverage gaps to report.
        id: SeriesId,
    },
    /// The CROSS-KIND coverage report — mirrors `HistStore::coverage_report()`. Answered by
    /// [`Response::Coverage`] (or [`Response::Error`]). Carries no payload: like
    /// [`Request::Inventory`] it is a whole-store manifest fold, and the Data Manager renders all of
    /// it at once (the "Partial" column is per-instrument, so a per-instrument verb would be N round
    /// trips to render one screen).
    ///
    /// Answers, not slices: this is day INDICES per kind per instrument — a fold of the file index,
    /// the same cost class as [`Request::Inventory`] — never rows. Negotiated by
    /// [`FEATURE_COVERAGE`] in `Welcome.features`, NOT by a version bump (see that constant); a
    /// server that predates the verb answers a clean [`Response::Error`], and the client refuses
    /// before sending.
    Coverage,
    /// Enumerate the compiled NATIVE backtest-strategy roster (`vike_backtest::harness::STRATEGIES`)
    /// — the names a profile's `strategy.name` can resolve WITH DEFAULT PARAMS. Answered by
    /// [`Response::Strategies`] (or [`Response::Error`]). Note the `"rhai"` SCRIPT arm is
    /// deliberately NOT in the roster (it needs a `src` param), so it never appears here. This verb
    /// carries no payload — it is store-independent (the roster is a compile-time const), so every
    /// build serves it without DataFusion, exactly like the store-metadata verbs.
    ListStrategies,
    /// Backfill-on-demand (split-plane REQ-9, §3 "This requires one new wire verb"): fetch
    /// `(venue, symbol, interval)` klines over `[start, end]` from the venue's public REST INTO
    /// the server's store, write-through BEFORE the reply, and answer
    /// [`Response::BackfillDone`] — so N clients asking for the same range cost the venue ONE
    /// fetch, and a fetched range is never lost ("History is fetched by the backend, once, into
    /// the store — clients request, never fetch").
    ///
    /// v1 is SYNCHRONOUS-per-request with a BOUNDED range — `start`/`end` are REQUIRED epoch-ms
    /// bounds (unlike [`Request::LoadBars`]'s optional pair), because an unbounded fetch is
    /// unbounded venue I/O with no one waiting on the other end of it. No job id, no progress
    /// stream: the `vike-backfill` collectors already page and pace internally (1000/req paging,
    /// per-venue pacers), so the server just runs one inline and the reply IS the completion
    /// event. A job queue is YAGNI until a consumer needs one — the Data Manager renders
    /// per-request status today, and a range too big for one request is the CLIENT's chunking
    /// decision, exactly as it is for the fat GUI's `maybe_spawn_backfill` today.
    ///
    /// Served ONLY by a `backfill-serve` server with a collector table mounted; negotiated by
    /// [`FEATURE_BACKFILL`] in `Welcome.features` — NOT by a version bump (see that constant).
    /// Any other build answers a clean [`Response::Error`] naming what is missing.
    Backfill {
        /// Venue whose PUBLIC kline REST the server fetches from, and the store partition the
        /// rows land in (e.g. `"binance"`). An unsupported venue is an error naming the
        /// supported set.
        venue: String,
        /// Symbol, in the venue's own spelling (e.g. `"BTCUSDT"`).
        symbol: String,
        /// Bar interval (e.g. `"1h"`).
        interval: String,
        /// Inclusive range start (epoch-ms). REQUIRED — see above.
        start: i64,
        /// Inclusive range end (epoch-ms). REQUIRED — see above.
        end: i64,
    },
    /// **DELETE stored series, IRREVERSIBLY** — the one verb on this wire that destroys data.
    /// Answered by [`Response::Deleted`] (or [`Response::Error`]).
    ///
    /// It always computes and returns the PLAN — which series matched, what they hold, and who
    /// wrote them — and `dry_run` decides only whether the second half happens. There is no
    /// preview-token dance on the wire: a caller that wants one runs `dry_run: true` first and
    /// compares, and the surfaces that need a bound confirmation (the CLI's typed line, the MCP
    /// tool's `preview_token`) build it on their own side where the human or the agent is.
    ///
    /// ⚠ Served ONLY by a KEYED server, and refused OUTRIGHT by a key-less one whatever the request
    /// says — see [`FEATURE_DELETE_SERIES`], which carries the whole argument. On a keyed server it
    /// additionally requires the CONTROL scope, like every other write here.
    DeleteSeries {
        /// Which series to remove — the four identity dimensions, with an OMITTED one as the only
        /// wildcard. The server intersects it with its own enumeration; nothing here builds a path.
        selector: SeriesSelector,
        /// The commit-key prefix EVERY key of EVERY matched series must carry. `None` skips the
        /// assertion, which the server permits only for a fully-named single series — a SWEEP with
        /// no assertion is refused, because that is where "by name" stops being a name.
        produced_by: Option<String>,
        /// `true` computes and returns the plan and deletes nothing.
        dry_run: bool,
    },
}

/// The [`Response::Deleted`] payload: what one [`Request::DeleteSeries`] planned, and — unless it
/// was a dry run — what it then did.
///
/// The PLAN is always present, including on a dry run and including when nothing matched. The
/// OUTCOME is `None` for a dry run, and a `Some` whose `failed` list is non-empty is a partial
/// success: one broken series is one skipped series, the rest went, and the caller exits non-zero
/// off it. A provenance REFUSAL is not this shape at all — it is a [`Response::Error`], because
/// nothing was deleted and the request did not happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteDone {
    /// What the selector matched, with each series' coverage and provenance.
    pub plan: RemovalPlan,
    /// What was removed. `None` iff the request was a dry run.
    pub outcome: Option<RemovalOutcome>,
}

/// The [`Response::BackfillDone`] payload: what one [`Request::Backfill`] actually did.
///
/// `rows_written` is the collector's own count — `0` for a range the store had already ingested
/// (the collectors are idempotent by commit key), which is a SUCCESS, not a failure (failures are
/// [`Response::Error`]). `first_ts`/`last_ts` are read BACK from the store over the requested
/// range after the write — the write-through proof, and the client's seam bookkeeping for
/// stitching segment 2 to its live tail — `None` when the range holds no bars at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillDone {
    /// Rows the collector wrote (0 = the window was already ingested — still success).
    pub rows_written: u64,
    /// First bar ts now stored in the requested range (`None` = the range holds no bars).
    pub first_ts: Option<i64>,
    /// Last bar ts now stored in the requested range (`None` = the range holds no bars).
    pub last_ts: Option<i64>,
}

/// A server-to-client response. The `Report`/`Bars`/`Quotes`/`Trades`/`Properties` variants are the
/// compute-to-data answers; `Error` carries a server-side failure message so a client never has to
/// guess why a request did not produce its answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Reply to [`Request::Hello`]: the server's [`PROTO_VERSION`] and its advertised `features` —
    /// the verbs this server answers (e.g. `"backtest"`, `"load_bars"`, `"scan_quotes"`,
    /// `"scan_trades"`, `"properties_as_of"`). The client compares the version and fails loudly on a
    /// mismatch; `features` is forward-compat room for a client to learn what a server serves.
    ///
    /// On a KEYED server it additionally carries `nonce` — 32 fresh random bytes for THIS
    /// connection — and advertises [`FEATURE_AUTH`].
    Welcome {
        /// The server's [`PROTO_VERSION`].
        proto_version: u32,
        /// The verbs this server answers (advertised capability strings).
        features: Vec<String>,
        /// The per-connection auth challenge on a KEYED server: 32 fresh random bytes the client
        /// signs in [`Request::Auth`]. Freshly minted per connection, which is what makes a
        /// captured transcript unreplayable against a later one.
        ///
        /// ⚠ `None` on a key-less server, and the serde attributes are load-bearing rather than
        /// tidiness: `skip_serializing_if` means a key-less server's `Welcome` is BYTE-IDENTICAL to
        /// the pre-auth protocol's (the field is simply not on the wire), and `default` means an
        /// old server's `Welcome` still decodes for a new client. That pair is the whole
        /// backward-compatibility proof, and
        /// `crates/vike-datahub/tests/auth_roundtrip.rs`'s
        /// `a_keyless_servers_welcome_is_byte_identical_to_the_pre_auth_protocol` is what holds it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nonce: Option<[u8; 32]>,
    },
    /// Reply to an accepted [`Request::Auth`]: the connection is authenticated, and `scope` is its
    /// capability ceiling for the rest of its life (it is never re-negotiated — a second `Auth` is
    /// refused).
    AuthOk {
        /// The scope the connection authenticated under.
        scope: Scope,
    },
    /// Reply to a REFUSED [`Request::Auth`], or to any verb sent to a KEYED server before one
    /// succeeded. The connection closes after this frame.
    ///
    /// ⚠ `reason` is deliberately coarse (`"bad mac"`, `"not authenticated: expected Hello
    /// first"`) — it never distinguishes "no such key" from "wrong key", and never echoes a key,
    /// a mac or a nonce. The server-side detail goes to the OPERATOR's log, which is the party
    /// entitled to it.
    AuthDenied {
        /// A short, non-informative-to-an-attacker refusal reason.
        reason: String,
    },
    /// Reply to [`Request::Ping`].
    Pong,
    /// The backtest metrics summary as **JSON text** (`serde_json::to_string(&BacktestReport)` — the
    /// exact shape the `backtest --json` bin emits). The client renders it, or `from_str`s it back
    /// into a typed report once `BacktestReport` grows `Deserialize` (a later phase).
    Report(String),
    /// Reply to [`Request::RunSlice`] (PR-3): the rendered [`WireRunResult`] — the equity curve,
    /// closed trades, and the few counters the Studio shows, built from a `BacktestResult` at the
    /// server boundary. A run FAILURE (bad script/slice/params) comes back as [`Response::Error`]
    /// (the stringified [`crate::wire_studio::WireRunError`]), not here.
    RunResult(WireRunResult),
    /// Reply to [`Request::RunSweep`] (PR-4): the ranked [`WireSweepResult`] — the sweep entries plus
    /// the deflated-Sharpe / PBO scores, built next to the data. A run FAILURE (bad script/slice/
    /// params) comes back as [`Response::Error`], not here.
    SweepResult(WireSweepResult),
    /// Reply to [`Request::RunWalkforward`] (PR-4): the stitched [`WireWalkforwardResult`] — the OOS
    /// windows + stitched equity curve + summary stats. A run FAILURE comes back as
    /// [`Response::Error`], not here.
    WalkforwardResult(WireWalkforwardResult),
    /// Reply to [`Request::RunSweepProfile`] (v7): the RANKED sweep as **JSON text**
    /// (`serde_json::to_string(&vike_backtest::harness::SweepReport)` — the exact shape the
    /// `backtest --sweep --json` bin emits). Rows arrive already ordered best-first by the
    /// requested metric, each carrying its own `BacktestReport`, so a client renders
    /// server-computed stats and never re-implements a metric. Text (not a typed DTO) for the same
    /// reason [`Response::Report`] is: `SweepReport` is `Serialize`-only, keeping the wire schema
    /// decoupled from vike-backtest's internal serde surface.
    SweepReport(String),
    /// Reply to [`Request::RunWalkforwardProfile`] (v7): the stitched
    /// `vike_backtest::walkforward::WalkForwardReport` as **JSON text** — the OOS windows, the
    /// stitched equity curve, and the three summary scalars. Text for the same reason as
    /// [`Response::SweepReport`].
    WalkforwardReport(String),
    /// A server-side failure (invalid profile, missing strategy, data error, …), stringified.
    Error(String),
    /// Reply to [`Request::LoadBars`]: the derived OHLCV bars, ts-ascending.
    Bars(Vec<Bar>),
    /// Reply to [`Request::ScanQuotes`]: the L1 quotes, ts-ascending.
    Quotes(Vec<QuoteTick>),
    /// Reply to [`Request::ScanTrades`]: the executed trades, ts-ascending.
    Trades(Vec<TradeTick>),
    /// Reply to [`Request::PropertiesAsOf`]: the point-in-time properties, or `None`.
    ///
    /// BOXED deliberately: `SymbolProperties` is ~140 bytes and is DOCUMENTED to grow, so an inline
    /// `Option<SymbolProperties>` would be by far the largest variant and could trip
    /// `clippy::large_enum_variant` (and it would keep growing toward the threshold). serde treats
    /// `Box<T>` / `Option<Box<T>>` transparently, so the wire shape is identical to an unboxed
    /// `Option<SymbolProperties>` — the box is a pure in-memory size optimization.
    Properties(Option<Box<SymbolProperties>>),
    /// Reply to [`Request::ListSeries`] (PR-6): every stored series id, sorted.
    SeriesList(Vec<SeriesId>),
    /// Reply to [`Request::Inventory`] (PR-6): every stored series paired with its cheap coverage.
    Inventory(Vec<(SeriesId, SeriesCoverage)>),
    /// Reply to [`Request::SeriesGaps`] (PR-6): the inclusive epoch-ms gap ranges (empty = none).
    SeriesGaps(Vec<(i64, i64)>),
    /// Reply to [`Request::Coverage`]: the cross-kind report, one entry per instrument, in the
    /// store's own (sorted) order. The value type is [`vike_data::InstrumentCoverage`] itself, not a
    /// wire DTO — see that module's "Why these types derive serde" — so a remote caller folds
    /// `partial_days()` over exactly the bytes a local caller folds.
    Coverage(Vec<InstrumentCoverage>),
    /// Reply to [`Request::ListStrategies`]: the compiled native backtest-strategy names (the
    /// `vike_backtest::harness::STRATEGIES` roster, in its declared order).
    Strategies(Vec<String>),
    /// Reply to [`Request::Backfill`]: the fetch ran and the rows are IN THE STORE (write-through
    /// happens before this frame is written). See [`BackfillDone`] for the field contract; a
    /// fetch/validation failure is [`Response::Error`], never a partial `BackfillDone`.
    BackfillDone(BackfillDone),
    /// Reply to [`Request::DeleteSeries`]: the plan, and what it did. See [`DeleteDone`]. A
    /// provenance REFUSAL, a key-less server and an unknown kind are all [`Response::Error`] —
    /// nothing was deleted in any of those cases, and this variant never reports a run that did not
    /// happen.
    Deleted(DeleteDone),
}

/// Serialize `msg` to JSON, write a big-endian `u32` length prefix, write the body, and flush.
///
/// Errors: a serialization failure or an over-[`MAX_FRAME_LEN`] body both surface as
/// [`io::ErrorKind::InvalidData`]; the underlying `write`/`flush` I/O errors pass through.
pub fn write_frame<W: Write>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let bytes =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame body exceeds u32 length"))?;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame body of {len} bytes exceeds MAX_FRAME_LEN {MAX_FRAME_LEN}"),
        ));
    }
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Read one length-prefixed frame and return its BODY BYTES — WITHOUT decoding.
///
/// Reads the 4-byte big-endian length, rejects a length above [`MAX_FRAME_LEN`] BEFORE allocating
/// (the OOM guard), then reads exactly that many bytes and returns them. A clean end-of-stream
/// surfaces as [`io::ErrorKind::UnexpectedEof`] (from `read_exact`), which callers treat as a closed
/// connection.
///
/// This is the lower half of [`read_frame`], split out (PR-2) so a server can separate FRAMING from
/// DECODING: a well-framed body that then fails to decode into a known [`Request`] is a bad
/// *request* — answer it with [`Response::Error`] and keep the connection — not a bad *connection*.
/// If decode were fused into the read (as in [`read_frame`]), that decode error would be
/// indistinguishable from a transport fault and would drop the connection, the exact footgun PR-2
/// removes.
pub fn read_frame_raw<R: Read>(r: &mut R) -> io::Result<Vec<u8>> {
    read_frame_raw_capped(r, MAX_FRAME_LEN)
}

/// [`read_frame_raw`] with a CALLER-CHOSEN ceiling instead of [`MAX_FRAME_LEN`].
///
/// The guard is the same one and it still fires BEFORE the allocation — this only lets a server
/// spend less trust on a peer it has not authenticated yet. [`MAX_FRAME_LEN`] is 64 MiB because a
/// legitimate *answer* (a chart's worth of bars) can be large; a legitimate *handshake* frame is a
/// few hundred bytes, so accepting 64 MiB of it means an unauthenticated peer can make the server
/// allocate 64 MiB per connection by sending four bytes. A caller that knows the phase can say so.
///
/// `max_len` is clamped to [`MAX_FRAME_LEN`]: this is a way to ask for LESS trust, never more, so a
/// larger value cannot widen the global OOM guard.
pub fn read_frame_raw_capped<R: Read>(r: &mut R, max_len: u32) -> io::Result<Vec<u8>> {
    let cap = max_len.min(MAX_FRAME_LEN);
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf);
    if len > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("declared frame length {len} exceeds the {cap}-byte cap for this phase"),
        ));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// Read one length-prefixed frame and decode it as `T` — [`read_frame_raw`] plus
/// `serde_json::from_slice`.
///
/// A clean end-of-stream surfaces as [`io::ErrorKind::UnexpectedEof`]; an over-[`MAX_FRAME_LEN`]
/// length or a decode failure both surface as [`io::ErrorKind::InvalidData`], so a caller has ONE
/// error channel. NOTE the fusion: a server that must keep the connection alive across an
/// undecodable body should read with [`read_frame_raw`] and decode separately, so a decode error is
/// not mistaken for a transport fault (see that function's docs and the module-level decode-vs-drop
/// contract).
pub fn read_frame<R: Read, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let body = read_frame_raw(r)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// `Hello` and `Welcome` (the PR-2 handshake pair) survive `write_frame` -> `read_frame`
    /// unchanged over an in-memory buffer — the version + feature-list fields round-trip.
    #[test]
    fn hello_and_welcome_survive_the_frame_codec() {
        let features = vec!["backtest".to_string(), "load_bars".to_string()];
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &Request::Hello { proto_version: PROTO_VERSION }).unwrap();
        write_frame(
            &mut buf,
            &Response::Welcome {
                proto_version: PROTO_VERSION,
                features: features.clone(),
                nonce: None,
            },
        )
        .unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::Hello { proto_version } => assert_eq!(proto_version, PROTO_VERSION),
            other => panic!("expected Hello, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::Welcome { proto_version, features: got, nonce } => {
                assert_eq!(proto_version, PROTO_VERSION);
                assert_eq!(got, features);
                assert_eq!(nonce, None);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    /// The AUTH frames survive the codec, and `Welcome.nonce` round-trips in BOTH shapes.
    ///
    /// ⚠ The load-bearing half is the ABSENT one: a key-less server's `Welcome` must encode with no
    /// `nonce` key at all (not `"nonce":null`), which is what makes it byte-identical to the
    /// pre-auth protocol's frame and every old client's decode unaffected. Asserted on the JSON
    /// text, because that is the only place the difference between "absent" and "present and null"
    /// is visible.
    #[test]
    fn the_auth_frames_survive_the_codec_and_an_absent_nonce_is_absent_on_the_wire() {
        let keyless = serde_json::to_string(&Response::Welcome {
            proto_version: PROTO_VERSION,
            features: vec!["load_bars".to_string()],
            nonce: None,
        })
        .unwrap();
        assert!(
            !keyless.contains("nonce"),
            "a key-less Welcome must carry no nonce field: {keyless}"
        );

        let nonce = [7u8; 32];
        let mut buf: Vec<u8> = Vec::new();
        write_frame(
            &mut buf,
            &Response::Welcome {
                proto_version: PROTO_VERSION,
                features: vec![FEATURE_AUTH.to_string()],
                nonce: Some(nonce),
            },
        )
        .unwrap();
        write_frame(&mut buf, &Request::Auth { scope: Scope::Control, mac: vec![9u8; 32] })
            .unwrap();
        write_frame(&mut buf, &Response::AuthOk { scope: Scope::Control }).unwrap();
        write_frame(&mut buf, &Response::AuthDenied { reason: "bad mac".to_string() }).unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::Welcome { nonce: got, features, .. } => {
                assert_eq!(got, Some(nonce));
                assert_eq!(features, vec![FEATURE_AUTH.to_string()]);
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::Auth { scope, mac } => {
                assert_eq!(scope, Scope::Control);
                assert_eq!(mac, vec![9u8; 32]);
            }
            other => panic!("expected Auth, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::AuthOk { scope } => assert_eq!(scope, Scope::Control),
            other => panic!("expected AuthOk, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::AuthDenied { reason } => assert_eq!(reason, "bad mac"),
            other => panic!("expected AuthDenied, got {other:?}"),
        }
    }

    /// ⚠ FORWARD compatibility, the other half of the no-version-bump argument: an OLD server's
    /// `Welcome` — one written before `nonce` existed, i.e. with no such field — must still decode
    /// for a NEW client. That is what `#[serde(default)]` buys, and without it a new binary could
    /// not talk to any deployed datahub at all.
    #[test]
    fn an_old_servers_welcome_still_decodes_for_a_new_client() {
        let old =
            format!(r#"{{"Welcome":{{"proto_version":{PROTO_VERSION},"features":["backtest"]}}}}"#);
        match serde_json::from_str::<Response>(&old).expect("an old Welcome must still decode") {
            Response::Welcome { proto_version, features, nonce } => {
                assert_eq!(proto_version, PROTO_VERSION);
                assert_eq!(features, vec!["backtest".to_string()]);
                assert_eq!(nonce, None, "an absent nonce decodes as None, not an error");
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    /// The PR-3 `RunSlice` request and `RunResult` response survive `write_frame` -> `read_frame`
    /// over an in-memory buffer — including the boxed `slice` field (serde-transparent) and the
    /// embedded [`WireSpec`]/[`WireEngineParams`]/[`WireRunResult`] DTOs.
    #[test]
    fn run_slice_and_run_result_survive_the_frame_codec() {
        use crate::wire_studio::WireSliceKind;

        let request = Request::RunSlice {
            spec: WireSpec::Native {
                name: "buy_hold".to_string(),
                params_toml: "size = 1.0\n".to_string(),
            },
            slice: Box::new(WireSlice {
                venue: "binance".to_string(),
                symbols: vec!["BTCUSDT".to_string()],
                interval: "1m".to_string(),
                start: None,
                end: Some(100_000),
                kind: WireSliceKind::Bars,
            }),
            params: Some(WireEngineParams { cash: Some(5000.0), fee_rate: None, slippage: None }),
        };
        let result = WireRunResult {
            equity_curve: vec![1000.0, 1010.0],
            equity_ts: vec![1, 2],
            final_equity: 1010.0,
            n_trades: 0,
            per_symbol_pnl: vec![],
            trades: vec![],
            stale_deferrals: 0,
            session_deferrals: 0,
        };

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &request).unwrap();
        write_frame(&mut buf, &Response::RunResult(result.clone())).unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunSlice { spec, slice, params } => {
                assert!(matches!(spec, WireSpec::Native { .. }));
                assert_eq!(slice.venue, "binance"); // the box round-trips as a plain WireSlice
                assert_eq!(slice.end, Some(100_000));
                assert_eq!(params.and_then(|p| p.cash), Some(5000.0));
            }
            other => panic!("expected RunSlice, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::RunResult(got) => assert_eq!(got, result),
            other => panic!("expected RunResult, got {other:?}"),
        }
    }

    /// The v6 `RunSweep` / `RunWalkforward` requests carry the OPTIONAL `params` cost/cash field and
    /// survive `write_frame` -> `read_frame` unchanged (including the `Some(WireEngineParams)`).
    #[test]
    fn run_sweep_and_walkforward_carry_engine_params_over_the_frame_codec() {
        use crate::wire_studio::WireSliceKind;

        let slice = || {
            Box::new(WireSlice {
                venue: "binance".to_string(),
                symbols: vec!["BTCUSDT".to_string()],
                interval: "1m".to_string(),
                start: None,
                end: None,
                kind: WireSliceKind::Bars,
            })
        };
        let sweep = Request::RunSweep {
            spec: WireSpec::Rhai("fn on_bar() {}".to_string()),
            slice: slice(),
            sweep: WireSweep { axes: vec![("fast".to_string(), vec![3.0, 5.0])] },
            params: Some(WireEngineParams {
                cash: Some(5000.0),
                fee_rate: Some(0.001),
                slippage: None,
            }),
        };
        let wf = Request::RunWalkforward {
            spec: WireSpec::Rhai("fn on_bar() {}".to_string()),
            slice: slice(),
            walkforward: WireWalkforward { n_splits: 4 },
            params: None,
        };

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &sweep).unwrap();
        write_frame(&mut buf, &wf).unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunSweep { params, .. } => {
                assert_eq!(params.and_then(|p| p.cash), Some(5000.0));
            }
            other => panic!("expected RunSweep, got {other:?}"),
        }
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunWalkforward { params, .. } => assert!(params.is_none()),
            other => panic!("expected RunWalkforward, got {other:?}"),
        }
    }

    /// A pre-v6 frame that OMITS `params` still decodes — `#[serde(default)]` maps the absent field
    /// to `None`, so the field is backward-compatible on the wire.
    #[test]
    fn run_sweep_without_params_field_decodes_as_none() {
        // A RunSweep body written WITHOUT the `params` key (the pre-v6 shape).
        let body = br#"{"RunSweep":{"spec":{"Rhai":"fn on_bar() {}"},"slice":{"venue":"binance","symbols":["BTCUSDT"],"interval":"1m","start":null,"end":null,"kind":"Bars"},"sweep":{"axes":[]}}}"#;
        let req: Request = serde_json::from_slice(body).expect("pre-v6 frame must still decode");
        match req {
            Request::RunSweep { params, .. } => assert!(params.is_none(), "absent params -> None"),
            other => panic!("expected RunSweep, got {other:?}"),
        }
    }

    /// The v7 PROFILE-shaped sweep / walk-forward verbs and their JSON-text answers survive
    /// `write_frame` -> `read_frame` — the profile TOML crosses VERBATIM (byte-for-byte the text the
    /// client read off disk), which is the whole point of the verb.
    #[test]
    fn profile_sweep_and_walkforward_survive_the_frame_codec() {
        const PROFILE: &str = "[data]\nvenue = \"binance\"\n\n[sweep]\nfast = [5, 10]\n";

        let mut buf: Vec<u8> = Vec::new();
        write_frame(
            &mut buf,
            &Request::RunSweepProfile {
                profile_toml: PROFILE.to_string(),
                rank_by: Some("max_dd".to_string()),
            },
        )
        .unwrap();
        write_frame(
            &mut buf,
            &Request::RunWalkforwardProfile { profile_toml: PROFILE.to_string() },
        )
        .unwrap();
        write_frame(&mut buf, &Response::SweepReport("{\"rows\":[]}".to_string())).unwrap();
        write_frame(&mut buf, &Response::WalkforwardReport("{\"windows\":[]}".to_string()))
            .unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunSweepProfile { profile_toml, rank_by } => {
                assert_eq!(profile_toml, PROFILE, "the profile TOML crosses verbatim");
                assert_eq!(rank_by.as_deref(), Some("max_dd"));
            }
            other => panic!("expected RunSweepProfile, got {other:?}"),
        }
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunWalkforwardProfile { profile_toml } => assert_eq!(profile_toml, PROFILE),
            other => panic!("expected RunWalkforwardProfile, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::SweepReport(json) => assert_eq!(json, "{\"rows\":[]}"),
            other => panic!("expected SweepReport, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::WalkforwardReport(json) => assert_eq!(json, "{\"windows\":[]}"),
            other => panic!("expected WalkforwardReport, got {other:?}"),
        }
    }

    /// `RunSweepProfile` without the optional `rank_by` key still decodes (as `None` ⇒ the server's
    /// `sharpe` default) — the same `#[serde(default)]` contract the v6 `params` field has.
    #[test]
    fn profile_sweep_without_rank_by_decodes_as_none() {
        let body = br#"{"RunSweepProfile":{"profile_toml":"[data]\n"}}"#;
        match serde_json::from_slice::<Request>(body).expect("must decode without rank_by") {
            Request::RunSweepProfile { rank_by, .. } => assert!(rank_by.is_none()),
            other => panic!("expected RunSweepProfile, got {other:?}"),
        }
    }

    /// The `ListStrategies` request (a unit verb) and `Strategies` response survive
    /// `write_frame` -> `read_frame` over an in-memory buffer — the roster list round-trips.
    #[test]
    fn list_strategies_and_strategies_survive_the_frame_codec() {
        let roster = vec!["buy_hold".to_string(), "grid".to_string()];
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &Request::ListStrategies).unwrap();
        write_frame(&mut buf, &Response::Strategies(roster.clone())).unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::ListStrategies => {}
            other => panic!("expected ListStrategies, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::Strategies(got) => assert_eq!(got, roster),
            other => panic!("expected Strategies, got {other:?}"),
        }
    }

    /// The raw read returns a well-framed body WITHOUT decoding, so an undecodable body is
    /// recoverable (a server answers it with `Response::Error`); `read_frame` on the SAME bytes still
    /// errors on decode. This is the decode-vs-drop split the server relies on.
    #[test]
    fn read_frame_raw_separates_framing_from_decode() {
        // Valid JSON, but not a known `Request` variant: externally-tagged unit variants serialize
        // as JSON strings, and `"Bogus"` is not one of them, so it frames fine yet fails to decode.
        let body: &[u8] = b"\"Bogus\"";
        let mut framed: Vec<u8> = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
        framed.extend_from_slice(body);

        // raw: returns the body bytes verbatim, no decode
        let mut cur = Cursor::new(framed.clone());
        let raw = read_frame_raw(&mut cur).unwrap();
        assert_eq!(raw.as_slice(), body, "raw read returns the framed body untouched");

        // fused: the same bytes fail to decode into a `Request`
        let mut cur2 = Cursor::new(framed);
        let err = read_frame::<_, Request>(&mut cur2).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "an unknown variant fails to decode");
    }

    /// `read_frame_raw` keeps `read_frame`'s OOM guard: an oversized declared length is rejected
    /// before any body allocation.
    #[test]
    fn read_frame_raw_rejects_an_oversized_length() {
        let mut framed = (MAX_FRAME_LEN + 1).to_be_bytes().to_vec();
        framed.push(0); // a trailing byte — the guard must fire on the length, not on a short read
        let mut cur = Cursor::new(framed);
        let err = read_frame_raw(&mut cur).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    /// The `Backfill` request and `BackfillDone` response (the split-plane backfill-on-demand
    /// verb) survive `write_frame` -> `read_frame` over an in-memory buffer. NOTE this verb
    /// shipped WITHOUT a `PROTO_VERSION` bump: it is negotiated through the [`FEATURE_BACKFILL`]
    /// capability string in `Response::Welcome`, the designed forward-compat hook — see that
    /// constant's doc for the whole story.
    #[test]
    fn backfill_and_backfill_done_survive_the_frame_codec() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(
            &mut buf,
            &Request::Backfill {
                venue: "binance".to_string(),
                symbol: "BTCUSDT".to_string(),
                interval: "1h".to_string(),
                start: 1_000,
                end: 2_000,
            },
        )
        .unwrap();
        let done = BackfillDone { rows_written: 5, first_ts: Some(1_000), last_ts: Some(1_900) };
        write_frame(&mut buf, &Response::BackfillDone(done.clone())).unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::Backfill { venue, symbol, interval, start, end } => {
                assert_eq!(venue, "binance");
                assert_eq!(symbol, "BTCUSDT");
                assert_eq!(interval, "1h");
                assert_eq!(start, 1_000);
                assert_eq!(end, 2_000);
            }
            other => panic!("expected Backfill, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::BackfillDone(got) => assert_eq!(got, done),
            other => panic!("expected BackfillDone, got {other:?}"),
        }
    }

    /// The no-rows outcome (an already-ingested or empty range) round-trips with BOTH ts fields
    /// `None` — distinguishable from a failure, which is `Response::Error`, never a zeroed
    /// `BackfillDone`.
    #[test]
    fn backfill_done_no_rows_shape_round_trips() {
        let done = BackfillDone { rows_written: 0, first_ts: None, last_ts: None };
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &Response::BackfillDone(done.clone())).unwrap();
        let mut cur = Cursor::new(buf);
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::BackfillDone(got) => assert_eq!(got, done),
            other => panic!("expected BackfillDone, got {other:?}"),
        }
    }

    /// The `Coverage` request and a NON-EMPTY `Coverage` response survive the frame codec with
    /// every field intact — the `BTreeMap<String, KindDays>` and its day vectors included.
    ///
    /// The payload is built through `vike_data::coverage::join_coverage` rather than by hand, so
    /// this pins the REAL shape the store produces (every `TICK_KINDS` entry present, absent kinds
    /// as empty rows) rather than a hand-written approximation of it. Like `Backfill` above, this
    /// verb shipped WITHOUT a `PROTO_VERSION` bump — see [`FEATURE_COVERAGE`].
    #[test]
    fn coverage_request_and_a_non_empty_report_survive_the_frame_codec() {
        let report = vike_data::coverage::join_coverage(&[
            (
                vike_data::SeriesId::per_symbol("trade", "binance", "BTCUSDT", None),
                vec![19_000, 19_001, 19_003],
            ),
            (
                vike_data::SeriesId::per_symbol("quote", "binance", "BTCUSDT", None),
                vec![19_000, 19_001],
            ),
        ]);
        assert!(!report.is_empty(), "the fixture must carry a real entry, not an empty vec");

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &Request::Coverage).unwrap();
        write_frame(&mut buf, &Response::Coverage(report.clone())).unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::Coverage => {}
            other => panic!("expected Coverage, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::Coverage(got) => {
                assert_eq!(got, report, "the report survives the codec whole");
                // ...and the DERIVED answer the Data Manager renders survives with it: the gap in
                // the trade days and the quote kind's shorter span both have to make it across for
                // this to hold.
                assert_eq!(
                    got[0].partial_days(),
                    report[0].partial_days(),
                    "the partial-day fold is identical on both sides of the codec"
                );
            }
            other => panic!("expected Coverage, got {other:?}"),
        }
    }
}
