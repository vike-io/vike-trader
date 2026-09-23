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
//! - [`Request::RunParamscan`] / [`Request::RunWalkforward`] (PR-4) are the sweep and walk-forward
//!   siblings of `RunSlice`: they ship the same `spec`/`slice` DTOs plus a [`WireParamscan`] grid or a
//!   [`WireWalkforward`] split-count — and, since proto v6, the same OPTIONAL
//!   `params: Option<WireEngineParams>` cost/cash override `RunSlice` carries — and get back a
//!   [`Response::ParamscanResult`] / [`Response::WalkforwardResult`] — the ranked/stitched ANSWER, never
//!   the raw slices. Same `serve-datafusion`-only serving as `RunSlice`. These are the STUDIO
//!   verbs: the GUI holds `spec`/`slice` DTOs (a two-click picker), not a profile file.
//! - [`Request::RunParamscanProfile`] / [`Request::RunWalkforwardProfile`] (v7) are the PROFILE-shaped
//!   twins of those two, and the sweep / walk-forward siblings of [`Request::RunBacktest`]: they
//!   ship the profile's **TOML text** verbatim and get back [`Response::ParamscanReport`] /
//!   [`Response::WalkforwardReport`] as JSON TEXT. The SERVER parses with
//!   `BacktestProfile::from_toml_str` and runs `vike_backtest::harness::run_paramscan` /
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
//! own log, where the last four additions each argue why they were not such a change.
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
use vike_data::{
    CohortRow, ExecFillRow, InstrumentCoverage, PerpMetricRow, SeriesCoverage, SeriesId,
};

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
/// The `--produced-by` VALIDATOR, re-exported for the same reason as the vocabulary above — and it
/// is the half that was missing, which cost this wire a hole.
///
/// ⚠ **`vike_data::store_kind::resolve_produced_by` had exactly ONE caller in the tree** — the
/// ENGINE's local `data rm` arm (`crates/vike-backtest/src/backtest_cli.rs`'s `run_rm_series`) —
/// and `crates/vike-datahub/src/server.rs`'s `delete_series_verb` was not it: it handed the raw
/// spelling to `plan_removal`. So the SELECTOR's rules were enforced server-side (`plan_removal`
/// opens by calling `SeriesSelector::validate_shape`, a method on the shared type) while the
/// ASSERTION's were enforced nowhere on the wire. A BLANK spelling then satisfied the sweep gate
/// (`Some("")` is not `None`) *and* the provenance check (`key_matches_prefix` is `starts_with`,
/// and every key starts with the empty string), so one token turned a provenance-asserted delete
/// into a wildcard one. That resolver's own doc had described the hazard since it was written.
///
/// Importing it FROM HERE rather than from `vike_data::store_kind` is the point: one grep on this
/// file shows both ends of the wire sharing one definition of what a valid producer filter is. It
/// is also what lets `vike-cli` — which takes `vike-data` as a DEV-dependency only — name the
/// rule its own refusals enforce before a socket is dialled.
///
/// The module this comes from is dependency-free by design (`crates/vike-data/src/store_kind.rs`
/// carries no `use` statement at all and is declared OUTSIDE every `hist-datafusion` gate), so this
/// adds no package, no feature and no Arrow/DataFusion weight to the light crate.
pub use vike_data::store_kind::resolve_produced_by;
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

// The capability ceiling carried by `Request::Auth` / `Response::AuthOk`. Defined in
// `crate::node_auth` (the SHARED primitive both localhost services sign with) and re-exported here
// so `proto::Scope` resolves on this protocol exactly as it does on the tradehub node's.
pub use crate::node_auth::Scope;

use crate::wire_studio::{
    WireEngineParams, WireParamscan, WireParamscanResult, WireRunResult, WireSlice, WireSpec,
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
/// - `3` — PR-4 added the Studio [`Request::RunParamscan`] / [`Request::RunWalkforward`] verbs + their
///   [`Response::ParamscanResult`] / [`Response::WalkforwardResult`] answers.
/// - `4` — PR-6 added the store-metadata verbs [`Request::ListSeries`] / [`Request::Inventory`] /
///   [`Request::SeriesGaps`] + their [`Response::SeriesList`] / [`Response::Inventory`] /
///   [`Response::SeriesGaps`] answers (so `RemoteHistStore` can serve the Data-Manager catalog).
/// - `5` — added the [`Request::ListStrategies`] roster verb + its [`Response::Strategies`] answer
///   (the compiled native backtest-strategy names, so an agent can discover which strategies exist).
/// - `6` — added the OPTIONAL `params: Option<WireEngineParams>` cost/cash field to
///   [`Request::RunParamscan`] / [`Request::RunWalkforward`] (the same DTO [`Request::RunSlice`] already
///   carries), so a remote sweep / walk-forward honors a profile's `[engine]` cash/fee_rate/slippage
///   instead of always running default engine params. The field is `#[serde(default)]`, so an old
///   frame that omits it decodes as `None` (backward-compatible on the wire).
/// - `7` — added the PROFILE-shaped [`Request::RunParamscanProfile`] / [`Request::RunWalkforwardProfile`]
///   verbs + their [`Response::ParamscanReport`] / [`Response::WalkforwardReport`] answers: the sweep /
///   walk-forward siblings of `RunBacktest`, carrying the profile TOML verbatim instead of
///   re-parsed DTOs. Purely ADDITIVE — the Studio [`Request::RunParamscan`] /
///   [`Request::RunWalkforward`] verbs are unchanged.
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
/// - still `7` — the MARKET-DATA push lane ([`Request::MdSubscribe`] / [`Request::MdUpdate`],
///   [`Response::MdSubscribed`] / [`Response::MdUpdated`] / [`Response::Md`]) followed the SAME
///   precedent: additive variants, negotiated through [`FEATURE_MARKET_DATA`] and the per-venue
///   [`md_venue_feature`] entries, no bump. The version is folded INTO the signed auth mac
///   ([`crate::node_auth::sign`]), so a bump breaks the handshake against every keyed peer not
///   upgraded in lockstep — and every frame decodes cleanly in BOTH directions: a server predating
///   the verbs answers [`Response::Error`] and KEEPS the connection (the PR-2 framing/decode
///   split), and no existing reply changed. See that constant.
/// - still `7` — the `search` selector on [`Request::RunParamscanProfile`] and the `"multi"` ranking
///   value, negotiated per-capability through [`FEATURE_SEARCH_METHOD`], NOT by a bump. See that
///   constant: a bump here fails the CONNECTION (strict equality inside `connect`) and reports "a
///   service could not be reached" for a service that answered, and it breaks the signed auth mac
///   against every keyed peer not upgraded in lockstep.
/// - still `7` — [`Request::RunStudy`] / [`Response::StudyReport`], negotiated through the
///   already-shipped [`FEATURE_STUDY`] rather than by a bump. That constant has carried the
///   argument since before the verb existed: a bump fails every old-client/new-server pair at the
///   handshake, and this is precisely an OLD-CLIENT/new-server situation by construction.
/// - still `7` — the `search` selector on the STUDIO [`Request::RunWalkforward`]
///   ([`crate::wire_studio::WireWindowSearch`]) and the cost-model STAMP on the three Studio
///   answers ([`crate::wire_studio::WireCostModel`]). The stamp is a pure addition to a reply and
///   needs nothing but `default` + `skip_serializing_if`; the SELECTOR is the opposite case and is
///   negotiated per-capability through [`FEATURE_WALKFORWARD_SEARCH`], NOT by a bump, for the
///   reasons [`FEATURE_SEARCH_METHOD`] already states — a bump fails the CONNECTION and breaks the
///   signed auth mac, while the thing actually at risk is a daemon silently running a FIXED walk
///   for a client that asked for a SEARCH. See that constant.
/// - still `7` — the per-venue RECORDING advertisement ([`rec_venue_feature`] /
///   [`advertised_rec_venues`]), on the SAME precedent as the per-venue [`md_venue_feature`]
///   entries beside it and for a reason stronger than theirs: **no `Request`/`Response` variant
///   and no field changed at all.** `Welcome.features` is a `Vec<String>` that already carries
///   entries this shape, so an OLD client decodes a `Welcome` carrying `rec_venue=binance`
///   byte-for-byte as it always did and ignores a string it does not know — which is the room
///   `Response::Welcome` reserved. The rule at the top of this list ("bump on ANY change to the
///   [`Request`] / [`Response`] schema") is therefore not engaged, and bumping anyway would fail
///   the CONNECTION on strict equality inside `connect` and break the signed auth mac
///   ([`crate::node_auth::sign`]) against every keyed peer not upgraded in lockstep — the cost
///   [`FEATURE_SEARCH_METHOD`] spells out.
/// - still `7`, and NOT A SCHEMA CHANGE AT ALL — the `sweep` -> `paramscan` rename moved four Rust
///   identifiers (`RunSweep`/`RunSweepProfile`/`SweepResult`/`SweepReport`) and moved ZERO bytes.
///   Each carries a `#[serde(rename = "…")]` pinning its old serialized tag, and the field key
///   inside [`Request::RunParamscan`] is still `"sweep"`.
///
///   ⚠ **The guard for all five lives OUTSIDE this file, and the reason is the incident.** It used
///   to be a `#[test]` in this very module asserting the same strings as source literals, and a
///   rename pass rewrote a pin's ARGUMENT and that test's expected string in ONE edit — a silent,
///   permanent wire break that passed 1097 tests and a full `verify-branch` and was caught only by
///   a human reading the diff. The expectation is now a pair of committed fixtures of REAL frames
///   under `fixtures/datahub_wire/`, which no edit under `crates/` can reach, replayed by
///   `crates/vike-datahub-client/tests/wire_tag_fixtures.rs` — whose third test also proves those
///   frames cover every `rename` this file declares, so a new pin cannot ship uncaptured.
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

/// The `Welcome.features` capability string for the CHART-GAP SEED verb
/// ([`Request::SeedSeries`] / [`Response::SeriesSeeded`] —
/// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`).
///
/// ⚠ Same forward-compat design as [`FEATURE_BACKFILL`]: additive variants, negotiated
/// per-capability, no [`PROTO_VERSION`] bump. And the same three legs, each tested.
///
/// ⚠ **It is advertised per ARMED LANE, a RUNTIME fact like `backfill`'s per-mounted-table rule
/// and unlike the build fact [`FEATURE_COVERAGE`] carries.** A build with the collectors compiled
/// in whose operator did not set `VIKE_DATAHUB_CHART_SEED=1` advertises nothing here, so a client
/// that reads the handshake tells the operator which switch is off instead of drawing an empty
/// chart and blaming the venue.
///
/// ⚠ **Its absence and its refusal differ from every other capability on this wire, and the
/// difference IS the scope argument.** A server that does not advertise `delete_series` REFUSES
/// the verb; a server that does not advertise this one SUCCEEDS at it and writes nothing
/// ([`SeedDone::armed`] `== false`). That is what makes the Observe classification honest rather
/// than convenient: an Observe connection's authority is to say which series a chart is open on,
/// never to cause a write — whether a write happens is a fact about the server's own configuration.
/// 0057's reach property 3 is that sentence, and removing it is in that record's reopen list.
pub const FEATURE_SEED_SERIES: &str = "seed_series";

/// The `Welcome.features` capability string for a chart-gap seed that NAMES THE INSTRUMENT'S KIND
/// — [`Request::SeedSeries`]'s `class` field, `docs/decisions/0061-an-instrument-names-its-kind.md`
/// Phase 3.
///
/// ⚠ **This shipped WITHOUT a [`PROTO_VERSION`] bump, for the reasons [`FEATURE_SEARCH_METHOD`]
/// spells out** — a bump is checked by STRICT EQUALITY inside `DatahubClient::connect`, so it kills
/// the CONNECTION and surfaces as "a service could not be reached" for a service that answered, and
/// [`crate::node_auth::sign`] folds the version into the auth mac, so it breaks the handshake
/// against every keyed peer not upgraded in lockstep.
///
/// ⚠ **And the field alone is not a guard — it is the DEFECT, in the sharpest form this wire has
/// seen.** [`Request`] has no `deny_unknown_fields`, so a daemon that predates this field DECODES
/// the frame, drops `class`, routes on the symbol alone and answers a perfectly ordinary
/// [`SeedDone`] reporting rows written. For a search selector that costs a slower search; here a
/// new client says PERPETUAL, an old daemon fetches the SPOT tape, writes it under the series the
/// chart is about to read, and reports success — **0061's measured bug, reproduced by its own
/// fix**. The client cannot tell that answer from a correct one by looking at it. So the three
/// legs, modelled on [`FEATURE_WALKFORWARD_SEARCH`] rather than on its neighbour, because this one
/// genuinely has a second parser and therefore a real server-side re-check:
///
/// - the server advertises this string **UNCONDITIONALLY** — a BUILD fact like
///   [`FEATURE_COVERAGE`], never a runtime one like [`FEATURE_SEED_SERIES`] beside it. The
///   distinction is load-bearing: `seed_series` answers *"is this box's lane armed"*, a question
///   whose answer changes with the operator's environment, while this one answers only *"is this
///   daemon older than the field"*, which a build cannot change its mind about. Advertising it
///   conditionally on the LANE would make an unarmed-but-modern daemon look like an old one, and
///   the client would then refuse to send a class to a server that understands it perfectly well;
/// - the CLIENT checks the advertisement and refuses **LOCALLY, WITHOUT SENDING**, when — and only
///   when — the request actually carries a class ([`crate::DatahubClient::seed_series_classed`]).
///   ⚠ A class-less seed must still be SENT to a daemon that does not advertise this: it asks for
///   exactly what it always asked for, and refusing it would break every chart against every older
///   daemon to guard a field the request does not use. That false-refusal trap is what
///   `an_ordinary_grid_search_is_still_sent_to_a_daemon_without_the_capability` pins for the
///   search-method sibling, and its twin here pins the same thing;
/// - the SERVER re-checks, in `crates/vike-datahub/src/server.rs`'s `seed_series_verb`, against
///   `vike_catalog::addressing_for` — the same table the bridges' own `route_target` consults. It
///   refuses a class the venue's data path cannot address, BY NAME, and it refuses a class it
///   cannot HONOUR on the spelling it was given. The client-side refusal answers "your daemon is
///   too old"; this one answers "that is not a book I can reach for that symbol", and neither is
///   reachable by the other's route.
pub const FEATURE_SEED_CLASS: &str = "seed_class";

/// The `Welcome.features` capability string for the VENUE-CATALOG verb
/// ([`Request::VenueCatalog`] / [`Response::VenueCatalog`] —
/// `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`).
///
/// ⚠ Same forward-compat design as [`FEATURE_BACKFILL`]: additive variants, negotiated
/// per-capability, no [`PROTO_VERSION`] bump. The variants are purely ADDITIVE, so no old peer can
/// fail to DECODE a frame — which is the only thing a bump is the tool for — and a server predating
/// the verb answers a clean [`Response::Error`] over a connection that SURVIVES (the
/// decode-vs-drop contract in this module's doc).
///
/// ⚠ **Advertised per SERVING LANE, a RUNTIME fact** like [`FEATURE_SEED_SERIES`]'s and unlike the
/// build fact [`FEATURE_COVERAGE`] carries. A server whose operator wrote `venue_catalog_off = true`
/// advertises nothing here, so a client that reads the handshake tells the operator which key is
/// set instead of showing an EMPTY instrument list — which for this verb would be indistinguishable
/// from the two roster venues that genuinely have no bulk list
/// ([`crate::catalog::CatalogRefusal::NoBulkList`]).
///
/// ⚠ **The DEFAULT flipped on 2026-09-16 and this paragraph said the opposite.** It read "whose
/// operator did not set `VIKE_DATAHUB_VENUE_CATALOG=1`", which was the arming;
/// `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md` made
/// the lane default-ON, so the absence of this capability is now evidence of a WRITTEN REFUSAL and
/// not of an unconfigured box. The two read identically on the wire and mean opposite things to an
/// operator, which is why the sentence [`crate::catalog::CatalogListing::describe`] renders moved
/// with it.
///
/// ⚠ **Its absence is a SUCCESS, not a refusal of the request**, the same shape
/// [`FEATURE_SEED_SERIES`] argues: such a server answers
/// [`crate::catalog::CatalogOutcome::NotArmed`] having called no venue. The difference from that
/// verb is what the switch is FOR — 0062's decision 1 establishes this is not a store write at all,
/// so it never gated a write; 0066's decision 2 then takes 0062's three arming reasons apart and
/// finds that none survives as an argument for the DEFAULT, one (cost) surviving only as the
/// argument that a refusal must exist.
pub const FEATURE_VENUE_CATALOG: &str = "venue_catalog";

/// The `Welcome.features` capability string for the NAMED-RUN verbs
/// ([`Request::RunNamed`] / [`Response::NamedRun`] and [`Request::NamedStrategies`] /
/// [`Response::NamedStrategies`] — `docs/decisions/0064-a-named-run-carries-no-source.md`).
///
/// ⚠ Same forward-compat design as [`FEATURE_BACKFILL`]: additive variants, negotiated
/// per-capability, no [`PROTO_VERSION`] bump. A bump fails every old-client/new-server pair at the
/// handshake, and nothing here is undecodable to an old peer — which is the only thing a bump is
/// the tool for.
///
/// ⚠ **A BUILD fact, like [`FEATURE_COVERAGE`] and UNLIKE [`FEATURE_SEED_SERIES`] and
/// [`FEATURE_VENUE_CATALOG`] — and the difference is deliberate rather than an inconsistency.**
/// Those two are advertised per ARMED LANE, so an unarmed server and an OLD server look identical
/// in `Welcome.features` and a client cannot tell them apart. That is tolerable where an unarmed
/// lane is a fetch the operator declined; it is not tolerable here, because
/// `docs/decisions/0064`'s decision 8 requires an unarmed server to ANSWER — *"an unarmed server
/// advertises itself as unarmed […] and the client renders the variable's name rather than an
/// empty list"*. So the CAPABILITY says "this build has the verbs" and the ARMING is carried in the
/// answer ([`crate::named_run::NamedRunOutcome::NotArmed`], [`crate::named_run::NamedRoster::armed`]),
/// which makes all three states distinguishable with one string: an old server advertises nothing,
/// an unarmed one advertises and answers `NotArmed`, an armed one runs.
///
/// The three legs, each tested:
///
/// - the compute daemon advertises it unconditionally (`vike_backtest::compute_server`'s
///   `served_features`) — this whole surface is behind that crate's `hist-replay` feature, so a
///   build that compiles the module has the arms;
/// - the CLIENT checks the advertisement and refuses locally, WITHOUT sending
///   ([`crate::DatahubClient::run_named`]), for the reason
///   [`crate::DatahubClient::run_paramscan_profile`] states: a daemon predating a field DROPS it
///   and answers a well-formed report, so there is no reply to inspect and no way to tell
///   afterwards — and a dropped WINDOW CEILING would be exactly that failure;
/// - the SERVER re-checks the arming at its own door and never trusts the advertisement.
pub const FEATURE_NAMED_RUN: &str = "named_run";

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

/// The `Welcome.features` capability string for the raw L2 book-update read
/// ([`Request::ScanBookUpdates`] / [`Response::BookUpdates`]) — and the doc for the whole SIX-verb
/// family `docs/decisions/0084-only-the-datahub-touches-the-store.md` names: its five siblings
/// [`FEATURE_SCAN_DEPTH`], [`FEATURE_SCAN_COHORT`], [`FEATURE_SCAN_PERP_METRICS`],
/// [`FEATURE_SCAN_EQUITY`] and [`FEATURE_SCAN_EXEC_FILLS`] point here rather than restating it.
///
/// ⚠ **These six exist because the wire NOT serving them is what kept four crates opening the
/// store directly.** 0084's verdict is that the store has one reader and everything else asks it
/// over the wire; the obstacle it measured is this gap — `vike-backtest` needed
/// `scan_book_updates`, the study context (through `vike-user-research`) needed four more, and
/// `vike-report` needed two. Every one of them was REFUSED by
/// `crates/vike-datahub-client/src/remote.rs` rather than merely absent, which was the correct
/// answer for a client that could not ask: a verb nobody can ask for over the wire is a verb whose
/// consumer opens the files instead, and then the wire's verb set is optional and drifts.
///
/// They take [`FEATURE_COVERAGE`]'s shape rather than [`FEATURE_BACKFILL`]'s, for that constant's
/// stated reason: these are plain [`vike_data::HistStore`] trait verbs with no table to mount and
/// nothing runtime about them, so every build that serves at all serves them and each is
/// advertised UNCONDITIONALLY. The negotiation answers exactly one question — *is this server
/// older than the verb?*
///
/// ⚠ **Six strings and not one**, although all six shipped together and probably always will. That
/// is this protocol's own convention — `load_bars` / `scan_quotes` / `scan_trades` /
/// `properties_as_of` are four strings for four verbs that also shipped together — and it is the
/// shape that survives a server which later serves a SUBSET. A single family string would have to
/// be RE-DEFINED to mean less the first time that happened, and a capability whose meaning changes
/// under a client is worse than one that is merely absent.
///
/// ⚠ **Additive, so [`PROTO_VERSION`] does NOT move**, and the reason is sharper than "new variants
/// are compatible": the version is folded into the signed auth mac, so a bump fails the connection
/// against every keyed peer not upgraded in lockstep — including every pair that never sends one of
/// these. Underneath the advertisement, a server that predates these verbs answers the unknown
/// variant with a clean `Response::Error` and KEEPS the connection (the PR-2 framing/decode split),
/// so a client that asks without checking first gets a named refusal rather than a dropped socket.
pub const FEATURE_SCAN_BOOK_UPDATES: &str = "scan_book_updates";

/// The capability string for the CONFLATING depth lane ([`Request::ScanDepth`] /
/// [`Response::Depth`]). See [`FEATURE_SCAN_BOOK_UPDATES`] for the family's whole negotiation.
pub const FEATURE_SCAN_DEPTH: &str = "scan_depth";

/// The capability string for the cohort read ([`Request::ScanCohort`] / [`Response::Cohort`]). See
/// [`FEATURE_SCAN_BOOK_UPDATES`] for the family's whole negotiation.
pub const FEATURE_SCAN_COHORT: &str = "scan_cohort";

/// The capability string for the perp-metrics read ([`Request::ScanPerpMetrics`] /
/// [`Response::PerpMetrics`]). See [`FEATURE_SCAN_BOOK_UPDATES`] for the family's whole
/// negotiation.
pub const FEATURE_SCAN_PERP_METRICS: &str = "scan_perp_metrics";

/// The capability string for the equity-curve read ([`Request::ScanEquity`] /
/// [`Response::Equity`]). See [`FEATURE_SCAN_BOOK_UPDATES`] for the family's whole negotiation.
pub const FEATURE_SCAN_EQUITY: &str = "scan_equity";

/// The capability string for the Tier-2 exec-fill read ([`Request::ScanExecFills`] /
/// [`Response::ExecFills`]). See [`FEATURE_SCAN_BOOK_UPDATES`] for the family's whole negotiation.
pub const FEATURE_SCAN_EXEC_FILLS: &str = "scan_exec_fills";

/// The `Welcome.features` capability string for the ROW CAP on a range scan — the `limit` field on
/// [`Request::LoadBars`], [`Request::ScanQuotes`], [`Request::ScanTrades`] and the five ranged
/// members of 0084's family.
///
/// ⚠ **Why a cap had to exist at all.** These verbs answer in ONE frame and carried no row bound,
/// so a scan whose range holds more rows than [`MAX_FRAME_LEN`] (64 MiB) is a request the server
/// CANNOT answer — measured against a Polymarket book group of ~37.5 M rows, which is exactly the
/// read `crates/vike-backtest/src/bin/cheap_np_depth.rs` performs with `TsRange::all()`. Without a
/// cap, routing that reader through the datahub
/// (`docs/decisions/0084-only-the-datahub-touches-the-store.md`) is not possible at all.
///
/// ⚠ **THE CAP IS SOFT, AND THAT IS THE CORRECTNESS PROPERTY RATHER THAN A CONVENIENCE.** A server
/// honouring it returns whole `ts` GROUPS: it stops at the last complete timestamp at or before
/// the cap, so a page may carry fewer rows than asked and may carry the full group that straddles
/// it. The reason is what a paging client does next — it continues from `last_ts + 1`, because the
/// response carries no cursor (see below). A server that cut a page mid-`ts` would leave the rest
/// of that timestamp's rows on the far side of the client's own continuation bound, and they would
/// be dropped SILENTLY: no error, no gap, a short answer that reads exactly like the truth. Every
/// row family on this wire sorts by `ts` first, and `scan_book_updates` REGROUPS on `(ts, seq)`, so
/// a mid-`ts` cut can also split one logical event into two partial ones.
///
/// ⚠ **The response carries no cursor, deliberately, and this is what keeps the change additive.**
/// Every reply here is a TUPLE variant — `Response::Trades(Vec<TradeTick>)` is `{"Trades":[…]}` on
/// the wire. Adding a `next` alongside the rows means a STRUCT variant
/// (`{"Trades":{"rows":[…],"next":…}}`), which is a different shape and would break every peer that
/// predates it — losing the one property that let six verbs and this cap ship without moving
/// [`PROTO_VERSION`]. A client derives its continuation from the last row's `ts` instead. The whole
/// cost of that choice is ONE trailing request per scan whose final page happened to be exactly
/// full; the alternative costs wire compatibility.
///
/// ⚠ **What this does NOT bound: the SERVER's memory.** `vike_data::HistStore`'s scan verbs return
/// the whole range as a `Vec` and take no limit, so a capped answer is a FULL materialization that
/// is then truncated — the frame is bounded, the allocation is not. On the deployed box that
/// allocation happens beside a live trading daemon. Closing it means pushing the limit down into
/// the trait and the DataFusion query, which is a separate change across every `HistStore` impl;
/// until then a client asking for an enormous range with a small `limit` has moved a failure it
/// could see into one it cannot.
pub const FEATURE_SCAN_LIMIT: &str = "scan_limit";

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

/// The `Welcome.features` capability string the COMPUTE daemon advertises when it serves the
/// compiled STUDY runner — `vike-backend study`'s wire half, and what `vike-cli research study`
/// negotiates on (ruling 16 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`).
///
/// ⚠ **THE DAEMON THAT WILL ADVERTISE IT IS `vike-backend backtest --addr`, NOT the data server.**
/// A study RUNS an engine — it opens the hist store, folds a strategy over it and drives a LightGBM
/// child process — and ruling 7 of that spec puts every verb of that shape on the COMPUTE plane,
/// which `backtest` serves. The string is declared HERE because this crate is where every
/// `FEATURE_*` a client of this protocol negotiates on already lives ([`FEATURE_AUTH`],
/// [`FEATURE_BACKFILL`], [`FEATURE_COVERAGE`], [`FEATURE_DELETE_SERIES`]) — one spelling, below
/// whichever daemon comes to serve it, rather than a second copy in the client and the server.
///
/// ⚠ **THE VERB IT GATES EXISTS since stage 7**, and this doc said the opposite for as long as it
/// did not. It is [`Request::RunStudy`], answered by [`Response::StudyReport`]. The deferral was
/// real while ruling 7's split was in flight — a `RunStudy` variant added to [`Request`] in the
/// middle of that lift would have landed two changes on top of each other — and ruling R1 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` spent it: `research` is a
/// PLANE now, and a plane whose only verb refuses on every box is a promise rather than a plane.
///
/// ⚠ **The advertisement is MOUNT-conditional, and that is the sharp half.**
/// `vike_backtest::compute_server`'s `served_features` pushes this string ONLY when a `StudyRunFn`
/// was handed to it — a runtime fact like [`FEATURE_BACKFILL`], not a build fact like
/// [`FEATURE_COVERAGE`] or its own neighbour [`FEATURE_SEARCH_METHOD`]. The reason is the layer
/// graph rather than a preference: the runner lives in `vike-studio-core`, which sits ABOVE
/// `vike-backtest`, so only a composition root that can see both (`crates/vike/src/main.rs`) can
/// inject one. `vike-backend backtest --addr` therefore serves a study and a bare
/// `cargo run -p vike-backtest --bin backtest` refuses it BY NAME.
///
/// ⚠ **A client still refuses BY NAME with nothing sent when it is absent**, and that half is
/// unchanged: [`crate::DatahubClient::run_study`] checks the advertisement before the write, and
/// `crates/vike-cli/src/cmd/study.rs` checks the same advertisement and prints the richer refusal
/// with the `vike-backend study` escape hatch filled in. The DATA daemon advertises no `study` and
/// never will, which is what `crates/vike-cli/tests/study_report_refusal_cli.rs` drives the shipped
/// binary against to prove the client's half of it.
///
/// Same forward-compat design as [`FEATURE_BACKFILL`]: capability strings, never a
/// [`PROTO_VERSION`] bump, because a bump fails every old-client/new-server pair at the handshake.
pub const FEATURE_STUDY: &str = "study";

/// The `Welcome.features` capability string for a parameter search that names its own SEARCH
/// METHOD — [`Request::RunParamscanProfile`]'s `search` field and the `rank_by` value `"multi"`.
///
/// ⚠ **This shipped WITHOUT a [`PROTO_VERSION`] bump, and here the reason is sharper than on any
/// sibling.** A bump is checked inside `DatahubClient::connect` by STRICT EQUALITY, so it kills the
/// CONNECTION — which `vike-cli backtest` reports on the CONNECT rung, as "a service could not be
/// reached". That is the wrong sentence: the service WAS reached, it answered, and it cannot do the
/// one thing that was asked. A bump also breaks the signed auth mac ([`crate::node_auth::sign`]
/// folds the version in) against every keyed peer not upgraded in lockstep.
///
/// ⚠ **And the field alone is not a guard — it is the DEFECT.** [`Request`] has no
/// `deny_unknown_fields`, so a daemon that predates this field DECODES the frame, drops `search`,
/// runs the exhaustive grid and answers a perfectly normal [`Response::ParamscanReport`]. The client
/// cannot tell a Bayesian search from a grid by looking at one. A silent downgrade of a selector is
/// the exact defect #1750 ended when it retired `--search`, so the three legs are:
///
/// - the compute daemon advertises this string UNCONDITIONALLY — a build fact like
///   [`FEATURE_COVERAGE`], not a runtime one like [`FEATURE_BACKFILL`], because
///   `vike_backtest::compute_server` is one `hist-replay`-gated module and a build that has the
///   module has the arm. ⚠ Contrast [`FEATURE_STUDY`], which the SAME `served_features` advertises
///   CONDITIONALLY, because its runner is injected from a crate above that daemon;
/// - the CLIENT checks the advertisement and refuses LOCALLY, WITHOUT SENDING, when the request
///   needs it ([`WireSearch::needs_capability`]) — the [`crate::DatahubClient::coverage_report`]
///   shape;
/// - underneath both, a server that predates the FIELD still decodes the frame and keeps the
///   connection, so the refusal is a client-side courtesy rather than the only thing standing
///   between a peer and a desync.
pub const FEATURE_SEARCH_METHOD: &str = "search_method";

/// The `Welcome.features` capability string for the STUDIO walk-forward's per-window SEARCH —
/// [`crate::wire_studio::WireWalkforward`]'s `search` field.
///
/// ⚠ **Same defect, same three legs, different verb — and it shipped WITHOUT a
/// [`PROTO_VERSION`] bump for the reasons [`FEATURE_SEARCH_METHOD`] spells out** (strict-equality
/// version check inside `DatahubClient::connect`, and the version folded into the signed auth mac
/// by [`crate::node_auth::sign`]). [`Request`] has no `deny_unknown_fields`, so a daemon predating
/// the field DECODES the frame, drops `search`, runs the FIXED walk and answers a perfectly normal
/// [`Response::WalkforwardResult`]. A caller cannot tell an optimized walk from a fixed one by
/// looking at one report, and the fixed one is not a degraded answer to the question asked — it is
/// an answer to a DIFFERENT question (were these parameters stable out of sample, rather than does
/// fit-then-trade survive out of sample). So:
///
/// - the compute daemon advertises this string **only when the STUDIO runners are MOUNTED** — a
///   MOUNT fact like [`FEATURE_STUDY`] and unlike its neighbour [`FEATURE_SEARCH_METHOD`], which is
///   unconditional because its arm is a build fact of one `hist-replay`-gated module. This one
///   rides `Request::RunWalkforward`, which `vike_backtest::compute_server` can only serve through
///   a table injected from `vike-studio-core` ABOVE it; a daemon advertising it unmounted would
///   invite a frame whose only possible answer is a refusal;
/// - the CLIENT checks the advertisement and refuses LOCALLY, WITHOUT SENDING, when the request
///   needs it ([`crate::wire_studio::WireWindowSearch::needs_capability`]) — the
///   [`crate::DatahubClient::coverage_report`] shape;
/// - underneath both, the SERVER re-checks: `vike_studio_core::wire_run`'s `run_walkforward_local`
///   parses the method itself and refuses an unknown one BY NAME rather than falling through to the
///   fixed walk. The client-side refusal answers "your daemon is too old"; this one answers "that
///   is not a search I have", and neither can be reached by the other's route.
pub const FEATURE_WALKFORWARD_SEARCH: &str = "walkforward_search";

/// The parameter-search METHODS this protocol can carry, and the ONE roster four surfaces read.
///
/// ⚠ **It lives in the PROTOCOL crate and the ENGINE reads it, which looks inverted and is not.**
/// `vike-datahub-client` is the only crate BOTH `vike-cli` (which spelling-checks `--optimizer`
/// before a dial or a spawn) and `vike-backtest` (which implements the methods) take as a normal
/// dependency — the workspace's own cure for two sides that must not disagree is a shared crate
/// BELOW both, and there is no other candidate: `crates/vike-cli/Cargo.toml` states at its
/// `vike-datahub-client` edge that the CLI carries "no concrete backend, no vike-backtest, no
/// engine crates".
///
/// Three rosters disagreed before this const: the engine binary took all four, `vike-cli
/// backtest --local` took three (`genetic` was refused during arg parsing), and the remote route
/// took one. `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §5.4 records that
/// divergence and §15.1 requires the gate that now holds it closed.
pub const SEARCH_METHODS: [&str; 4] = ["grid", "euler", "tpe", "genetic"];

/// The method a search runs when none is named: the exhaustive product. The first row of
/// [`SEARCH_METHODS`], and `vike_backtest::harness::sweep::GridSearch`'s own `Optimizer::name`.
pub const DEFAULT_SEARCH_METHOD: &str = "grid";

/// The `Welcome.features` capability string for the MARKET-DATA push lane
/// ([`Request::MdSubscribe`] / [`Request::MdUpdate`] / [`Response::Md`] — the datahub
/// market-data wire design, §4.1).
///
/// ⚠ Same forward-compat design as [`FEATURE_BACKFILL`]: additive variants, negotiated
/// per-capability, no [`PROTO_VERSION`] bump. The three legs, each tested:
///
/// - a server advertises this string ONLY when an `MdHub` is actually MOUNTED AND ARMED — a
///   RUNTIME fact like `backfill`'s per-mounted-table rule, NOT a build fact like
///   [`FEATURE_COVERAGE`]. A build carrying the plane with `VIKE_DATAHUB_LIVE` unset mounts no hub
///   and advertises nothing;
/// - the CLIENT checks the advertisement and refuses LOCALLY — without sending — when it is absent
///   (the [`crate::DatahubClient::coverage_report`] shape), which is what lets a desktop show an
///   honest note instead of a socket that stalls;
/// - underneath both, a server that predates the verb (or one built without the plane) answers the
///   unknown variant with a clean [`Response::Error`] and KEEPS the connection — the PR-2
///   framing/decode split. ⚠ On this verb that leg carries more weight than on any other: a
///   refusal is what tells the client its connection is STILL POSITIONAL and it must not start a
///   reader thread. See [`crate::market`]'s module doc for the mode-switch invariant.
pub const FEATURE_MARKET_DATA: &str = "market_data";

/// The prefix of a per-venue market-data capability entry — `md_venue=binance`.
///
/// One entry per venue the SERVER's build actually links a market-data client for, so a client
/// learns at the HANDSHAKE which venues it may name in an [`crate::market::MdSpec`] rather than
/// discovering it one [`crate::market::MdRefusal::VenueNotServed`] at a time. The
/// value-carrying-feature shape has a precedent in this protocol family:
/// `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_DATAHUB_PREFIX` / `datahub_feature` /
/// `advertised_datahub` spell `datahub=<addr>` exactly this way, and their doc states the reason
/// the builder and the reader ship as a PAIR: it "is the round-trip authority (pinned by test), so
/// the server and the client cannot drift on the spelling."
///
/// ⚠ COLLISION-SAFE by construction: every capability check in this protocol family is
/// whole-string equality ([`crate::DatahubClient::coverage_report`], `backfill`, `delete_series`
/// all do `f == FEATURE_*`), so an `md_venue=binance` entry can neither satisfy nor shadow a named
/// capability.
pub const FEATURE_MD_VENUE_PREFIX: &str = "md_venue=";

/// Build the `md_venue=<slug>` capability entry for one venue — the WRITE half of the pair
/// [`advertised_md_venues`] reads, so the two spellings cannot drift.
///
/// ⚠ A pure `format!` in the LIGHT crate. The cfg that decides WHICH venues a build advertises is
/// the server's (`crates/vike-datahub/src/md/venues.rs`'s `supported`); this crate declares no
/// feature at all and must not grow one.
pub fn md_venue_feature(slug: &str) -> String {
    format!("{FEATURE_MD_VENUE_PREFIX}{slug}")
}

/// Read every venue slug a server advertised, in ADVERTISEMENT ORDER — the READ half of
/// [`md_venue_feature`].
///
/// ⚠ It differs from its precedent (`vike_tradehub_client::proto`'s `advertised_datahub`) in
/// exactly one way, and the difference is the point: `datahub=` is ONE entry, so that reader takes
/// the first match and returns an `Option`; `md_venue=` is one entry PER VENUE, so this returns a
/// `Vec`. Each value is `str::trim`med and an EMPTY value is dropped — an empty advertisement
/// advertises nothing, the same "absence is the answer" rule the rest of this file uses.
pub fn advertised_md_venues(features: &[String]) -> Vec<String> {
    features
        .iter()
        .filter_map(|f| f.strip_prefix(FEATURE_MD_VENUE_PREFIX))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// The prefix of a per-venue RECORDING capability entry — `rec_venue=binance`.
///
/// The twin of [`FEATURE_MD_VENUE_PREFIX`] for the OTHER venue-linking plane in the data daemon,
/// and it answers a DIFFERENT question about a DIFFERENT set. `md_venue=` says which venues this
/// build can SERVE LIVE (`crates/vike-datahub/src/md/venues.rs`'s `supported`); this one says which
/// venues this build can RECORD (`crates/vike-recorder/src/venues/mod.rs`'s `supported`). The two
/// sets are genuinely different rather than incidentally so — the shipped image serves six venues
/// and records two — so a client that read one for the other would refuse a venue it could record,
/// or accept one it could not.
///
/// # The defect it exists to prevent
///
/// `subscription.venue` carries no CHECK constraint, and it cannot: the recordable set is a
/// per-BUILD fact that a schema has no way to know. So a `vike-cli data realtime record add okx:…`
/// writes a perfectly legal row that `crates/vike-datahub/src/recorder.rs`'s
/// `load_and_check_profile_row` then REFUSES at the daemon's next start — under
/// `Restart=on-failure`, which is a data wire that goes down five seconds after an operator edits a
/// profile and stays down. The advertisement is what lets a client refuse the ROW at the point of
/// the edit, with the operator watching, instead of at a restart hours later.
///
/// # ⚠ An EMPTY advertisement is not a refusal, and that is the whole of the degrade rule
///
/// A reachable server carrying NO `rec_venue=` entry is ambiguous BY CONSTRUCTION: it is either a
/// server older than this advertisement, or a build carrying no recording plane at all (the
/// `record` Cargo feature off), which records nothing. A client cannot tell those apart and must
/// not try — an absent advertisement is the same answer as an unreachable server
/// (`docs/decisions/0013-degrade-vs-refuse.md`): WARN and proceed. The refusal is available only in
/// the case that is unambiguous — the server advertised AT LEAST ONE venue and the named one is not
/// among them.
///
/// ⚠ COLLISION-SAFE by construction, for [`FEATURE_MD_VENUE_PREFIX`]'s reason verbatim: every
/// capability check in this protocol family is whole-string equality, so a `rec_venue=binance`
/// entry can neither satisfy nor shadow a named capability. It cannot be confused with an
/// `md_venue=` entry either, the two prefixes being distinct strings that share no prefix.
pub const FEATURE_REC_VENUE_PREFIX: &str = "rec_venue=";

/// Build the `rec_venue=<slug>` capability entry for one venue — the WRITE half of the pair
/// [`advertised_rec_venues`] reads, so the two spellings cannot drift.
///
/// ⚠ A pure `format!` in the LIGHT crate, [`md_venue_feature`]'s twin and for its reason: the cfg
/// that decides WHICH venues a build advertises is the server's
/// (`crates/vike-recorder/src/venues/mod.rs`'s `supported`, reached from
/// `crates/vike-datahub/src/server.rs`'s `served_features`); this crate declares no feature at all
/// and must not grow one.
pub fn rec_venue_feature(slug: &str) -> String {
    format!("{FEATURE_REC_VENUE_PREFIX}{slug}")
}

/// Read every RECORDABLE venue slug a server advertised, in ADVERTISEMENT ORDER — the READ half of
/// [`rec_venue_feature`], and [`advertised_md_venues`]' twin down to the trim-and-drop-empty rule
/// (an empty value advertises nothing, the "absence is the answer" rule the rest of this file
/// uses).
///
/// ⚠ It reads ONE prefix and therefore cannot see an `md_venue=` entry, deliberately: the two
/// planes advertise separately because their sets differ, and a reader that accepted either would
/// report a venue as recordable because it happens to be servable.
///
/// ⚠ An EMPTY result is NOT "this server records nothing" — see [`FEATURE_REC_VENUE_PREFIX`]'s
/// degrade rule, which is the one thing a caller must read before acting on this answer.
pub fn advertised_rec_venues(features: &[String]) -> Vec<String> {
    features
        .iter()
        .filter_map(|f| f.strip_prefix(FEATURE_REC_VENUE_PREFIX))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// A parameter search's SEARCH SELECTION as it crosses the wire: which method, and that method's
/// own knobs, carried as the operator's own TOKENS.
///
/// ⚠ **Strings, not typed scalars, and that is the whole point.** The one authority for what
/// `"128"` means as a `--trials` value — including the refusal text for `"abc"` — is
/// `vike_backtest::harness::search_select`, which runs on the SERVER. Typed fields here would force
/// `vike-cli` to parse and refuse the same three values with its own three messages, and `vike-cli`
/// cannot call the engine's parser: it has no `vike-backtest` dependency and its manifest says so
/// deliberately. One parser, one message, on both routes, is what these `String`s buy.
///
/// ⚠ **Not to be confused with [`crate::wire_studio::WireParamscan`]**, which is the Studio's GRID (the
/// axes and their values). This is the METHOD that walks a grid, and the grid itself still lives in
/// the profile's own `[paramscan]` table.
///
/// ⚠ **The method cannot live in the profile instead, and that is what makes this field the only
/// available home rather than a preferred one.** `vike_backtest::harness::BacktestProfile` is
/// `#[serde(deny_unknown_fields)]`, so a top-level `optimizer = "tpe"` is a hard parse error, and
/// its parameter-grid field is a raw `toml::Table` whose every key is an AXIS —
/// `[paramscan].method = "tpe"` declares an axis named `method`. There is exactly one place this
/// can go, and this is it.
///
/// ⚠ **The sibling verb carries no selector**, and `Request::RunWalkforwardProfile`'s own doc
/// argues why: a second place to say "optimize" is a second place for the two to disagree. What a
/// walked-forward SEARCH should name, and where, is ruling R7's question (walk-forward is a
/// MODIFIER over a run, not a third run kind) for the stage that owns that routing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSearch {
    /// One of [`SEARCH_METHODS`]. `None` = [`DEFAULT_SEARCH_METHOD`].
    #[serde(default)]
    pub optimizer: Option<String>,
    /// euler's halving depth. Refused under any other method, by the server.
    #[serde(default)]
    pub euler_depth: Option<String>,
    /// tpe's trial budget. Refused under any other method, by the server.
    #[serde(default)]
    pub trials: Option<String>,
    /// The reproducibility seed tpe and genetic both take. Refused under any other method.
    #[serde(default)]
    pub seed: Option<String>,
}

impl WireSearch {
    /// Nothing was selected at all — the shape a caller sends as `None` rather than as an empty
    /// struct, so an ordinary grid search's frame is byte-identical to the one shipped before this
    /// field existed.
    pub fn is_empty(&self) -> bool {
        self.optimizer.is_none()
            && self.euler_depth.is_none()
            && self.trials.is_none()
            && self.seed.is_none()
    }

    /// Whether a daemon that does not advertise [`FEATURE_SEARCH_METHOD`] would answer this
    /// selection DIFFERENTLY from what it asks for — the predicate
    /// [`crate::DatahubClient::run_paramscan_profile`] refuses on.
    ///
    /// An explicit `grid` and nothing else is `false`: an old daemon drops the field and runs the
    /// grid, which is what was asked, and refusing it would be a false refusal on the one command
    /// line that works everywhere. Anything else is `true` — including a knob written UNDER the
    /// grid, because the refusal that argv deserves (`--trials is a tpe or genetic flag`) is one an
    /// old daemon cannot produce: it drops the field and reports success.
    pub fn needs_capability(&self) -> bool {
        self.euler_depth.is_some()
            || self.trials.is_some()
            || self.seed.is_some()
            || self
                .optimizer
                .as_deref()
                .is_some_and(|m| !m.eq_ignore_ascii_case(DEFAULT_SEARCH_METHOD))
    }
}

/// One STUDY run as it crosses the wire: which compiled study, its recipe's TEXT, and the window —
/// the whole of what `vike-cli study` has been able to say since it shipped, and could not send.
///
/// ⚠ **The recipe travels as TEXT and never as a PATH**, and that is not a style choice:
/// `crates/vike-cli/src/cmd/study.rs`'s module doc argues it where the file is read — the backend
/// is typically a different machine, so a path would be resolved against ITS filesystem and the
/// recipe an author is editing would be invisible to the run they just started. The same division
/// [`Request::RunBacktest`] draws over profile TOML.
///
/// ⚠ **`from`/`to` are the operator's own strings and are NOT parsed here.** The backend owns that
/// grammar — `YYYY-MM-DD`, `YYYY-MM-DDTHH`, or bare unix SECONDS — and a second parser on the
/// client would be a second answer to "what does this string mean", which is the failure class this
/// workspace re-learns most often. Same rule, same reason as [`WireSearch`]'s string knobs one
/// struct up.
///
/// ⚠ **There is no store root and no trainer path, deliberately.** Both name paths on the BACKEND's
/// box, which the client cannot see; `cmd/study.rs`'s `parse` already refuses `--store` and
/// `--lightgbm` BY NAME for exactly that reason. They are the daemon's configuration, resolved at
/// its composition root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireStudy {
    /// The study's registry name, which is also its folder name under
    /// `<project>/user_data/research/studies/rust/`. Validated only by the backend: the registry is
    /// a compile-time const in the study host, which `vike-cli` deliberately does not link.
    pub study: String,
    /// The recipe's TEXT — one of that study folder's `.toml` files, read on the CLIENT.
    pub recipe_toml: String,
    /// The window's start, as typed.
    pub from: String,
    /// The window's end, as typed.
    pub to: String,
}

/// A client-to-server request.
///
/// Two payload styles ride here, each for a reason:
///
/// - [`Request::RunBacktest`] carries the profile as its **TOML text** (not the `vike_backtest`
///   structs): the server parses+validates it with `BacktestProfile::from_toml_str` — the same path
///   a `.toml` file takes — which keeps the wire schema decoupled from `vike-backtest`'s internal
///   serde surface, so neither crate grows derives it does not otherwise want and the protocol stays
///   stable across engine refactors. (⚠ This read "deserialize-only on the profile, serialize-only
///   on the report". The profile half still holds; `BacktestReport` gained `Deserialize` with the
///   run-artifact stage, and the DECOUPLING argument is what survives — it never depended on the
///   report being unreadable, only on the wire not being its struct.)
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
    /// to the scope that may send it. In short: [`Scope::Read`] reads history and catalog;
    /// [`Scope::Write`] additionally admits [`Request::Backfill`] (which WRITES the store) and
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
    /// `sweep` grid next to the data, and reply with [`Response::ParamscanResult`] — the ranked answer,
    /// NOT the bars/ticks. Converted to the studio/engine types at the server boundary, which runs the
    /// EXISTING `vike_studio_core::run_paramscan_slice`. Served ONLY by a `serve-datafusion` build; a lean
    /// build decodes the request but answers [`Response::Error`].
    ///
    /// ⚠ **Renamed from `RunSweep` in SOURCE ONLY.** `#[serde(rename = "RunSweep")]` pins the wire
    /// tag to the string every already-deployed peer sends and expects; the Rust name is free to
    /// say what this verb actually is (a parameter SEARCH — not the order-sweep or runtime-latch
    /// sense of the word used elsewhere in this workspace) because a Rust identifier was never on
    /// the wire. Only this attribute's STRING ARGUMENT is, and it must never change. There is no
    /// `FEATURE_*`-shaped fix available if it did: a capability lets a client OMIT something, it
    /// cannot make one enum encode under two tag strings depending on who is listening.
    #[serde(rename = "RunSweep")]
    RunParamscan {
        /// The strategy to run (Rhai source or a native registry name + its params as TOML text).
        spec: WireSpec,
        /// The data window. BOXED to keep this variant small, exactly like [`Request::RunSlice`] —
        /// serde treats `Box<T>` transparently, so the wire shape is identical to an unboxed `WireSlice`.
        slice: Box<WireSlice>,
        /// The parameter grid — each `(name, values)` axis overrides `strategy.params.<name>`.
        ///
        /// ⚠ Also renamed IN SOURCE ONLY: `#[serde(rename = "sweep")]` pins this struct-variant
        /// FIELD's own wire key, because externally-tagged serde serializes a struct variant's
        /// fields under their own names too — this is not just the outer tag.
        #[serde(rename = "sweep")]
        paramscan: WireParamscan,
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
    /// the Studio's [`Request::RunParamscan`].
    ///
    /// The server parses+validates with `BacktestProfile::from_toml_str` (the SAME path a `.toml`
    /// file takes), expands the grid and runs it next to the data with
    /// `vike_backtest::harness::run_paramscan`, and replies with [`Response::ParamscanReport`] — the
    /// server-RANKED report as JSON text. Because the whole profile crosses the wire, the whole
    /// `[engine]` applies (fee schedule included) and the client needs no profile→DTO mapping and
    /// no metric math of its own.
    ///
    /// Served on EVERY build (the harness is DataFusion-free), exactly like `RunBacktest`.
    ///
    /// ⚠ **Renamed from `RunSweepProfile` in SOURCE ONLY**, pinned to its wire tag for the same
    /// reason [`Request::RunParamscan`] is — see that variant.
    #[serde(rename = "RunSweepProfile")]
    RunParamscanProfile {
        /// The profile's TOML text, shipped verbatim. Must carry a `[paramscan]` table (the
        /// `[sweep]` spelling still loads — see `vike_backtest::harness::BacktestProfile`).
        profile_toml: String,
        /// Which ranking orders the rows — `"sharpe"` / `"return"` / `"max_dd"` / `"equity"`
        /// (`harness::RankMetric`, case-insensitive) or `"multi"`, the COMPOSITE objective.
        /// `None` = `"sharpe"`, the `backtest --rank-by` default. An unrecognized name is a
        /// [`Response::Error`], never a silent fallback. `#[serde(default)]` so a frame that omits
        /// it decodes as `None`.
        ///
        /// ⚠ **`"multi"` needs [`FEATURE_SEARCH_METHOD`] too**, and for a different reason from
        /// the `search` field below: a daemon predating that capability resolves this string
        /// through `RankMetric::from_str_ci`, whose four arms have no `multi`, so it answers a
        /// server-side error naming a four-name set this client advertises five of. One refusal
        /// for one capability beats two.
        #[serde(default)]
        rank_by: Option<String>,
        /// Which SEARCH METHOD walks the grid, and that method's own knobs (v7 + the
        /// [`FEATURE_SEARCH_METHOD`] capability). `None` = the exhaustive grid, which is what this
        /// verb has always run and what an omitted field decodes to.
        ///
        /// ⚠ **`#[serde(default)]` makes an OLD client's frame decode; it does NOT make a NEW
        /// client's frame safe against an OLD server.** A daemon predating this field drops it
        /// silently and answers a normal report, so the client must refuse before sending — see
        /// [`FEATURE_SEARCH_METHOD`], which carries the whole argument.
        #[serde(default)]
        search: Option<WireSearch>,
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
    /// Run one COMPILED study over the daemon's own hist store and leave a run behind THERE
    /// (v7 + the [`FEATURE_STUDY`] capability).
    ///
    /// ⚠ **BOXED**, like [`Request::RunSlice`]'s slice: four `String`s inline widen every
    /// [`Request`] value on every connection for a variant almost none of them carry.
    ///
    /// ⚠ **A COMPUTE verb, not a fourth plane on the wire.** Ruling R1 of
    /// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` makes `research` a fourth
    /// CLI plane; that spec's own fence says a reading which gives it its own DAEMON is reopening
    /// ruling 7. A study opens the hist store and runs an engine, so it is served where every verb
    /// of that shape is served: `vike-backend backtest --addr`.
    RunStudy(Box<WireStudy>),
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
        /// The most rows this answer may carry, or `None` for every row in the range — the
        /// pre-2026-09-22 behaviour, and what an absent field still means.
        ///
        /// ⚠ A SOFT cap, and the softness is the CORRECTNESS property: see
        /// [`FEATURE_SCAN_LIMIT`], which argues why a server that cut a page mid-`ts` would make
        /// a paging client lose rows in silence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
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
        /// The most rows this answer may carry, or `None` for every row in the range — the
        /// pre-2026-09-22 behaviour, and what an absent field still means.
        ///
        /// ⚠ A SOFT cap, and the softness is the CORRECTNESS property: see
        /// [`FEATURE_SCAN_LIMIT`], which argues why a server that cut a page mid-`ts` would make
        /// a paging client lose rows in silence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
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
        /// The most rows this answer may carry, or `None` for every row in the range — the
        /// pre-2026-09-22 behaviour, and what an absent field still means.
        ///
        /// ⚠ A SOFT cap, and the softness is the CORRECTNESS property: see
        /// [`FEATURE_SCAN_LIMIT`], which argues why a server that cut a page mid-`ts` would make
        /// a paging client lose rows in silence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read raw L2 book updates — mirrors `HistStore::scan_book_updates(venue, symbol, range)`.
    /// Answered by [`Response::BookUpdates`] (or [`Response::Error`]).
    ///
    /// ⚠ The verb `docs/decisions/0084-only-the-datahub-touches-the-store.md` measured FIRST: it is
    /// the one read `vike-backtest` could not reach over this wire, and therefore the reason that
    /// crate opened the store directly. See [`FEATURE_SCAN_BOOK_UPDATES`] for the family.
    ScanBookUpdates {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// The most rows this answer may carry, or `None` for every row in the range — the
        /// pre-2026-09-22 behaviour, and what an absent field still means.
        ///
        /// ⚠ A SOFT cap, and the softness is the CORRECTNESS property: see
        /// [`FEATURE_SCAN_LIMIT`], which argues why a server that cut a page mid-`ts` would make
        /// a paging client lose rows in silence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read the CONFLATING depth lane — mirrors `HistStore::scan_depth(venue, symbol, range)`.
    /// Answered by [`Response::Depth`] (or [`Response::Error`]).
    ///
    /// ⚠ **Its payload type is `BookUpdate`, the same as [`Self::ScanBookUpdates`], and it still
    /// gets its OWN request and reply variant rather than sharing that one.** The two verbs read
    /// DIFFERENT `kind=` partitions (`crates/vike-data/src/store_kind.rs`), so one shared reply
    /// would make a desync between them undetectable: the caller would decode the other lane's rows
    /// as its own and see plausible data rather than an error. A conflated lane misread as a
    /// lossless one is exactly the class of wrongness this protocol's desync check exists to make
    /// loud.
    ScanDepth {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// The most rows this answer may carry, or `None` for every row in the range — the
        /// pre-2026-09-22 behaviour, and what an absent field still means.
        ///
        /// ⚠ A SOFT cap, and the softness is the CORRECTNESS property: see
        /// [`FEATURE_SCAN_LIMIT`], which argues why a server that cut a page mid-`ts` would make
        /// a paging client lose rows in silence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read cohort rows — mirrors `HistStore::scan_cohort(venue, asset, range)`. Answered by
    /// [`Response::Cohort`] (or [`Response::Error`]).
    ///
    /// ⚠ The second field is an **ASSET, not a symbol** — the one verb in this family whose middle
    /// argument is not a symbol partition. The trait spells it `asset` and so does this variant,
    /// because a field named `symbol` carrying an asset is how a caller passes the wrong one.
    ScanCohort {
        /// Venue partition.
        venue: String,
        /// Asset partition — NOT a symbol; see the variant doc.
        asset: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// The most rows this answer may carry, or `None` for every row in the range — the
        /// pre-2026-09-22 behaviour, and what an absent field still means.
        ///
        /// ⚠ A SOFT cap, and the softness is the CORRECTNESS property: see
        /// [`FEATURE_SCAN_LIMIT`], which argues why a server that cut a page mid-`ts` would make
        /// a paging client lose rows in silence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read perpetual-swap metric rows — mirrors
    /// `HistStore::scan_perp_metrics(venue, symbol, range)`. Answered by [`Response::PerpMetrics`]
    /// (or [`Response::Error`]).
    ScanPerpMetrics {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// The most rows this answer may carry, or `None` for every row in the range — the
        /// pre-2026-09-22 behaviour, and what an absent field still means.
        ///
        /// ⚠ A SOFT cap, and the softness is the CORRECTNESS property: see
        /// [`FEATURE_SCAN_LIMIT`], which argues why a server that cut a page mid-`ts` would make
        /// a paging client lose rows in silence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read the stored equity curve — mirrors `HistStore::scan_equity(venue, symbol, range)`.
    /// Answered by [`Response::Equity`] (or [`Response::Error`]).
    ScanEquity {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
        /// Inclusive-range start bound (epoch-ms), `None` = unbounded.
        start: Option<i64>,
        /// Inclusive-range end bound (epoch-ms), `None` = unbounded.
        end: Option<i64>,
        /// The most rows this answer may carry, or `None` for every row in the range — the
        /// pre-2026-09-22 behaviour, and what an absent field still means.
        ///
        /// ⚠ A SOFT cap, and the softness is the CORRECTNESS property: see
        /// [`FEATURE_SCAN_LIMIT`], which argues why a server that cut a page mid-`ts` would make
        /// a paging client lose rows in silence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    /// Read the Tier-2 exec FILL log — mirrors `HistStore::scan_exec_fills(venue, symbol)`.
    /// Answered by [`Response::ExecFills`] (or [`Response::Error`]).
    ///
    /// ⚠ **No `start`/`end`, and that is the TRAIT's shape rather than an omission here**:
    /// `HistStore::scan_exec_fills` takes `(venue, symbol)` alone. Giving the wire a range the store
    /// method has no parameter for would let a caller set a bound and watch it be silently ignored —
    /// the fabricated-answer failure this family was added to stop, wearing a field instead of an
    /// empty `Ok`.
    ScanExecFills {
        /// Venue partition.
        venue: String,
        /// Symbol partition.
        symbol: String,
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
    /// **Enumerate the roster a NAMED RUN would resolve** — and say whether the lane is ARMED.
    /// Answered by [`Response::NamedStrategies`] (or [`Response::Error`]).
    ///
    /// ⚠ **It is NOT [`Request::ListStrategies`] and the two answer different rosters**, which is
    /// the whole reason it exists. That verb answers `vike_backtest::harness::STRATEGIES` — the
    /// SIMULATOR roster, including the seven arms that live in `vike-backtest` beside the Rhai
    /// compiler — while a named run resolves only through crates that cannot name `vike-script`,
    /// and it also resolves the operator's own compiled-in user strategies, which
    /// `ListStrategies` has never enumerated at all. A client naming into the dark is the
    /// empty-answer-versus-refusal lie `docs/decisions/0062`'s decision 5 fences against, one layer
    /// out, and `docs/decisions/0064`'s decision 7 is the ruling: *the roster the verb SERVES is
    /// the roster it ENUMERATES*.
    ///
    /// Store-independent like its neighbour (the roster is a compile-time const plus a build-time
    /// generated one), so every build of the compute daemon serves it. Negotiated by
    /// [`FEATURE_NAMED_RUN`]; an UNARMED server still answers, with
    /// [`crate::named_run::NamedRoster::armed`] `false`.
    NamedStrategies,
    /// **Run ONE strategy the server already holds** — one strategy, one param set, one window,
    /// one pass. Answered by [`Response::NamedRun`] (or [`Response::Error`]).
    ///
    /// ⚠ **This is `VerbScope::Read` and it is the only `Run*` verb that is**, which is the
    /// whole subject of `docs/decisions/0064-a-named-run-carries-no-source.md`. Read
    /// [`required_scope`]'s ⚠ first: every OTHER `Run*` verb is Control because of the SOURCE it
    /// can carry — a profile's `[strategy.params].src`, a `WireSpec::Rhai`, and (0064's decision 5)
    /// a `WireSpec::Native`'s `params_toml` — not because running spends CPU. This one carries
    /// [`crate::named_run::NamedParam`], which has no variant a script could occupy, and resolves
    /// through a crate that cannot name `vike-script`, so there is no compiler at the end of the
    /// path rather than a refusal in front of one.
    ///
    /// ⚠ **The cost half is the CONDITION of that classification, not a consequence of it.** 0064's
    /// decision 3 records that on this daemon *"today NOT ONE dimension a request names is
    /// bounded"* — so this verb's bounds had to be BUILT: the single-point shape (no grid, no
    /// trials, no splits — [`crate::named_run::NamedRunSpec`] has no field a search can occupy),
    /// [`crate::named_run::NAMED_RUN_MAX_BARS`] on the window, the interval set, the symbol and
    /// venue validators, and a process-wide run-slot count that REFUSES rather than queues. **Adding
    /// a search dimension here is 0064's first reopener, not a small change.**
    ///
    /// Negotiated by [`FEATURE_NAMED_RUN`]; a server whose operator has not armed the lane answers
    /// [`crate::named_run::NamedRunOutcome::NotArmed`] rather than an error.
    RunNamed(Box<crate::named_run::NamedRunSpec>),
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
    /// **A chart is open on a series the store cannot paint — SEED IT.** Answered by
    /// [`Response::SeriesSeeded`] (or [`Response::Error`]).
    ///
    /// ⚠ **This is a WRITE verb classified [`VerbScope::Read`], and that is a DECISION against a
    /// standing forward ruling**: `docs/decisions/0052`'s *What would reopen this* predicted that a
    /// recording-on-demand verb "would be a `Backfill`-class write and would take its scope".
    /// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` is the argument that the
    /// predicate is the CLASS and this verb is not in it. Read it before widening anything here;
    /// [`required_scope`]'s arm carries the short form.
    ///
    /// **The request names a SERIES AND NOTHING ELSE.** No range, no bar count, no lookback, no
    /// cadence — every other term of the cost is a server constant the request cannot move
    /// ([`crate::seed`] carries the ones both ends need, `crates/vike-datahub/src/seed.rs` the ones
    /// only the server can have). That is the whole difference from [`Request::Backfill`], which
    /// takes a client-named range and stays `Control` for exactly that reason. **Adding a parameter
    /// here is the first bullet of 0057's reopen list, not a small change.**
    ///
    /// It SEEDS; it does not MAINTAIN. One window per series per process, by construction — the
    /// ledger that bounds the process also makes a repeat free
    /// (`SeedDone::repeated`). Deepening or refreshing a series is
    /// [`Request::Backfill`]'s job, which is the same split
    /// `vike_app_core::store_bars`' module doc draws from the read side.
    ///
    /// Negotiated by [`FEATURE_SEED_SERIES`] in `Welcome.features` — NOT by a version bump — and a
    /// server whose operator has not armed the lane answers `SeriesSeeded { armed: false, .. }`
    /// rather than an error. See that constant for why those two differ.
    SeedSeries {
        /// Venue whose PUBLIC kline REST the server fetches from, in the server's own table
        /// spelling (e.g. `"binance"`). A venue with no collector is an error naming the
        /// supported set — the identical refusal [`Request::Backfill`] gives.
        venue: String,
        /// Symbol, in the venue's own spelling (e.g. `"BTCUSDT"`, `"BTC-USDT-SWAP"`). Bounded in
        /// length AND charset by [`crate::seed::validate_seed_symbol`] before it reaches a bridge,
        /// because the binance kline URL interpolates it unencoded.
        symbol: String,
        /// Bar interval (e.g. `"5m"`). Checked against [`crate::seed::SEED_INTERVALS`] at the
        /// server's door, before dispatch — see that constant, and the ⚠ in [`crate::seed`]'s
        /// module doc for the hole it stands in front of.
        interval: String,
        /// **What KIND of instrument `symbol` names**, when the caller knows — `docs/decisions/0061`
        /// Phase 3, negotiated by [`FEATURE_SEED_CLASS`].
        ///
        /// ⚠ **`default` + `skip_serializing_if` are load-bearing rather than tidiness**, the same
        /// pairing [`Response::Welcome`]'s `nonce` and [`Request::RunParamscanProfile`]'s `search`
        /// carry: a `None` frame is BYTE-IDENTICAL to the one shipped before this field existed, so
        /// every chart that names no class costs an older daemon nothing and looks to it exactly as
        /// it always did.
        ///
        /// ⚠ **It NARROWS the route; it moves no term of the cost.** The paragraph in
        /// [`required_scope`] warning that adding a parameter here is 0057's first reopen bullet is
        /// about COST — the window, the bar count, the venue set, the interval set, the rate and
        /// the per-process series cap are all server constants, and this field touches none of
        /// them. It cannot make the server fetch more, fetch for longer, or fetch a series it would
        /// otherwise refuse; it can only make it refuse one it would otherwise have fetched from
        /// the wrong book. That is the argument, and it is written here because the arm's own ⚠ is
        /// what a reviewer hits first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<vike_model::AssetClass>,
    },
    /// **LIST ONE VENUE'S INSTRUMENTS** — the verb behind the Data Manager's catalog refresh.
    /// Answered by [`Response::VenueCatalog`] (or [`Response::Error`]).
    ///
    /// ⚠ **This verb is `VerbScope::Read` and it is NOT A WRITE**, which is the distinction
    /// `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` turns on.
    /// Nothing enters the served store: no `append_*`, no commit key, no partition, no row — so it
    /// is invisible to [`Request::ListSeries`] / [`Request::Inventory`] / [`Request::Coverage`] and
    /// unreachable by [`Request::DeleteSeries`]. The durable copy of a catalog is the CLIENT's own
    /// `vike_catalog::persist` cache; the server keeps only an in-process TTL memo. **Read 0062
    /// before persisting anything here — doing so makes this a write, collapses that record's
    /// decision 1 and re-imposes 0058's four-part rule in full**, including the idempotence leg a
    /// list-REPLACING write would fail.
    ///
    /// **The request names a VENUE AND NOTHING ELSE** — no filter, no page, no asset class, no
    /// `since`. Every other term of the cost is a server constant
    /// ([`crate::catalog`] carries the ones both ends need,
    /// `crates/vike-datahub/src/catalog.rs` the ones only the server can have). That is
    /// [`Request::Backfill`]'s difference restated, and the dimension this verb DOES name is
    /// bounded by an enumerable roster (`vike_model::VENUES`) rather than by a rate standing in for
    /// one — which is the residual `docs/decisions/0058` declared for [`Request::SeedSeries`]' free
    /// symbol, and the reason this verb sits further from `Backfill` than that one does.
    /// **Adding a parameter here is the first bullet of 0062's reopen list, not a small change.**
    ///
    /// ⚠ **A CREDENTIALED venue is refused BY CONSTRUCTION** (0062's decision 3): the server's
    /// table admits only providers that read a PUBLIC endpoint, so no client can make the server
    /// authenticate as its operator. alpaca/oanda/ctrader answer
    /// [`crate::catalog::CatalogRefusal::NeedsCredentials`].
    ///
    /// Negotiated by [`FEATURE_VENUE_CATALOG`] in `Welcome.features` — NOT by a version bump — and
    /// a server whose operator has not armed the lane answers
    /// [`crate::catalog::CatalogOutcome::NotArmed`] rather than an error.
    VenueCatalog {
        /// The venue whose PUBLIC instrument endpoint the server lists, in the canonical roster
        /// spelling (e.g. `"binance"`). Bounded in length AND charset by
        /// [`crate::catalog::validate_catalog_venue`] at the server's door, before its own table is
        /// consulted. A venue with no provider in this build is a
        /// [`crate::catalog::CatalogRefusal::NotServed`] naming the supported set — and, unlike
        /// every other unknown-venue path on this wire, it is carried in the SUCCESS variant rather
        /// than as an error, because "which venues can I refresh" is a question the Data Manager
        /// asks routinely and an error is the wrong shape for a routine answer.
        venue: String,
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
    /// **Open a MARKET-DATA push stream on this connection** — the wire's one MODE SWITCH.
    ///
    /// Sent on a fresh connection and answered positionally with exactly one
    /// [`Response::MdSubscribed`], **which is the last positional frame that socket will ever
    /// carry**: after it the server never reads this direction again and writes only
    /// [`Response::Md`]. [`crate::market`]'s module doc states the whole invariant and the one
    /// exception to it — a [`Response::Error`] answer (a server predating the verb, or one whose
    /// build serves no market-data plane) has NOT switched the connection, and a client that gets
    /// one must keep using the socket positionally rather than starting a reader thread.
    ///
    /// ⚠ **`refused` is PER-SPEC and a whole-request failure is [`Response::Error`].** A build with
    /// no hub does not answer `MdSubscribed { accepted: [], refused: [all] }` — that would
    /// mode-switch a connection into a heartbeat-only writer that will never send a frame, which is
    /// worse than the refusal it replaces. See [`crate::market::MdRefusal`], which is why that enum
    /// carries no `HubNotMounted` variant.
    ///
    /// ⚠ **There is deliberately NO long-lived control connection.** A session's mutation channel is
    /// [`Request::MdUpdate`] on an ordinary short-lived connection — "dial, one request, one reply,
    /// close" — so a dead control connection is not a STATE. That is the failure mode a persistent
    /// two-socket session has: a client that cannot reach its control socket cannot unsubscribe and
    /// leaks topics until its stream drops.
    ///
    /// Classified [`VerbScope::Read`] — see [`required_scope`] and
    /// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md`.
    MdSubscribe {
        /// The subscriptions wanted. Each is accepted (possibly with a CLAMPED `depth_levels`) or
        /// refused with its own reason; `accepted` echoes the server's AUTHORITATIVE spec.
        specs: Vec<crate::market::MdSpec>,
    },
    /// **Change an existing market-data session's subscription set**, answered positionally with
    /// [`Response::MdUpdated`].
    ///
    /// ⚠ **Sent on an ORDINARY, SHORT-LIVED connection — never on the stream socket.** The stream
    /// socket's server side has left its read loop (see [`Request::MdSubscribe`]), so a frame
    /// written there is never read and the caller waits forever. This is the
    /// `crates/vike-datahub-client/src/remote.rs` `RemoteHistStore` dial-per-request shape, and it
    /// is stated in the type because sending it on the stream socket is the obvious wrong move.
    ///
    /// ⚠ `remove` matches on the KEY `(venue, symbol, lane)` and IGNORES `depth_levels` — depth is
    /// not part of a subscription's identity (`crate::market::MdSpec::key`), so a client that
    /// removes a spec it once asked 200 levels for removes the key it holds whatever depth it named.
    ///
    /// An unknown or expired `session` is [`Response::Error`], not a refusal list — same rule as
    /// above.
    MdUpdate {
        /// The session minted by the [`Response::MdSubscribed`] that opened the stream.
        session: crate::market::MdSessionId,
        /// Specs to ADD. Refusals are per-spec.
        add: Vec<crate::market::MdSpec>,
        /// Specs to REMOVE, matched on their key.
        remove: Vec<crate::market::MdSpec>,
    },
}

/// Which SERVED SURFACE a verb belongs to — the split ruling 7 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` made, expressed once,
/// BELOW both servers that have to agree about it.
///
/// It lives in this crate rather than in either server for the reason the workspace's layering rule
/// gives: *when two sides must not disagree, the cure is a shared crate BELOW both.* The data server
/// (`vike-datahub`, layer 65) and the compute server (`vike-backtest`, layer 50) cannot see each
/// other — no edge between them exists or may exist — so a table kept in one of them would be a
/// second copy in the other, and the first divergence would show up as a verb that BOTH daemons
/// refuse. Here there is one match, and it is exhaustive.
///
/// ⚠ The two servers share one [`Request`]/[`Response`] SCHEMA on purpose: same `Hello`/`Auth`/`Ping`
/// handshake, same length-prefixed frames, same [`Scope`] rules, same bind posture
/// ([`crate::bind`]). What differs is which verbs each ANSWERS — everything else is the same
/// protocol, so one client library, one fixture set and one test harness serve both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// The DATA plane — the history/catalog reads, the store write and the store removal. Served by
    /// `vike-backend datahub`.
    Data,
    /// The COMPUTE plane — the seven verbs that run an engine (or answer out of the compiled-in
    /// strategy roster). Served by `vike-backend backtest --addr`.
    Compute,
    /// Served by BOTH: the handshake frames and the liveness probe. A daemon that could not answer
    /// `Hello` could not be connected to at all, and `Ping` is how a client learns a socket is alive
    /// before it commits to a verb.
    Shared,
}

impl Plane {
    /// The command that SERVES this plane, for the refusal message a client gets when it dialled the
    /// other daemon. Naming the command (rather than a port) is the point: an operator who reads it
    /// can start the missing daemon without looking anything up.
    pub fn served_by(self) -> &'static str {
        match self {
            Plane::Data => "vike-backend datahub",
            Plane::Compute => "vike-backend backtest --addr",
            // A shared verb is never the subject of a wrong-plane refusal, so this string exists
            // only so the method is total.
            Plane::Shared => "either daemon",
        }
    }

    /// The settings key naming the address this plane's daemon is dialled at — the OTHER half of an
    /// actionable refusal: the reader needs both what to start and where their client is pointing.
    pub fn addr_key(self) -> &'static str {
        match self {
            Plane::Data => "config.datahub_addr",
            Plane::Compute => "config.backtest_addr",
            Plane::Shared => "config.datahub_addr / config.backtest_addr",
        }
    }
}

/// Classify one request onto its served surface. **The match is EXHAUSTIVE — no `_` arm,
/// deliberately** — so a new wire verb fails to COMPILE here until somebody says which daemon
/// answers it, rather than silently defaulting to whichever side a catch-all happened to pick.
///
/// The split, and the one row that is not obvious:
///
/// - **[`Plane::Data`]** — everything that answers out of the STORE: the `HistStore` reads, the
///   four catalog verbs, the [`Request::Backfill`] write and the [`Request::DeleteSeries`] removal.
///   (It said "the FOUR `HistStore` reads" until
///   `docs/decisions/0084-only-the-datahub-touches-the-store.md` added six; [`plane_of`]'s own
///   match is the roster, and it is one screen below.)
/// - **[`Plane::Compute`]** — everything that RUNS something: [`Request::RunBacktest`], the Studio
///   [`Request::RunSlice`]/[`Request::RunParamscan`]/[`Request::RunWalkforward`], the profile-shaped
///   [`Request::RunParamscanProfile`]/[`Request::RunWalkforwardProfile`], the research plane's
///   [`Request::RunStudy`] — and
///   [`Request::ListStrategies`], which is the row worth arguing. It touches no store at all: it
///   returns `vike_backtest::harness::STRATEGIES`, a compile-time const roster, and
///   [`required_scope`]'s own comment used to admit the exception rather than hide it
///   (*"`ListStrategies` joins them — it returns a compile-time const roster, NOT store contents"*).
///   A verb that needs a footnote to fit the rule is a verb in the wrong place; after the split it
///   sits beside this binary's own `--list` flag, which reads the same constant — one constant,
///   two transports, one crate.
/// - **[`Plane::Shared`]** — `Hello`, `Auth`, `Ping`.
pub fn plane_of(request: &Request) -> Plane {
    match request {
        Request::Hello { .. } | Request::Auth { .. } | Request::Ping => Plane::Shared,

        Request::RunBacktest(_)
        | Request::RunSlice { .. }
        | Request::RunParamscan { .. }
        | Request::RunWalkforward { .. }
        | Request::RunParamscanProfile { .. }
        | Request::RunWalkforwardProfile { .. }
        | Request::RunStudy(_)
        | Request::ListStrategies
        // ...and the NAMED RUN and its roster verb. Their SCOPE is unlike every neighbour here
        // (`VerbScope::Read` — `docs/decisions/0064-a-named-run-carries-no-source.md`) and their
        // PLANE is not: they run an engine over the compute daemon's store handle, and the roster
        // they enumerate is that daemon's own compiled-in one.
        | Request::NamedStrategies
        | Request::RunNamed(_) => Plane::Compute,

        Request::LoadBars { .. }
        | Request::ScanQuotes { .. }
        | Request::ScanTrades { .. }
        // The SIX tick-level and research reads (`docs/decisions/0084`). Data plane and
        // Observe scope for the same reason as their neighbours above: they are
        // `vike_data::HistStore` trait reads answered from the DATA daemon's own store handle,
        // bounded by the range the caller names, and they write nothing.
        | Request::ScanBookUpdates { .. }
        | Request::ScanDepth { .. }
        | Request::ScanCohort { .. }
        | Request::ScanPerpMetrics { .. }
        | Request::ScanEquity { .. }
        | Request::ScanExecFills { .. }
        | Request::PropertiesAsOf { .. }
        | Request::ListSeries
        | Request::Inventory
        | Request::SeriesGaps { .. }
        | Request::Coverage
        | Request::Backfill { .. }
        // The chart-gap seed — the DATA daemon's store and the DATA daemon's collector table, so it
        // sits beside `Backfill` here however differently `required_scope` treats the two.
        | Request::SeedSeries { .. }
        // The VENUE CATALOG — the DATA daemon's own provider table and the DATA daemon's venue
        // egress, so it sits here beside the other venue-touching verbs. It reaches no store, which
        // is `required_scope`'s business rather than this match's: `plane_of` answers WHICH DAEMON,
        // and the answer is the one that links the bridges.
        | Request::VenueCatalog { .. }
        | Request::DeleteSeries { .. }
        // The MARKET-DATA push lane. ⚠ `Plane::Data` is the single highest-consequence row in this
        // match: `crates/vike-datahub/src/server.rs`'s `handle_connection` asks `plane_of` BEFORE
        // the scope check, so a misclassification here would have the DATA daemon refuse its own
        // new verbs with a wrong-plane message — everything compiling, nothing working. Classified
        // here, the mirror guard in `crates/vike-backtest/src/compute_server.rs` also hands a
        // desktop that dialled `config.backtest_addr` a free, correct, NAMED refusal.
        | Request::MdSubscribe { .. }
        | Request::MdUpdate { .. } => Plane::Data,
    }
}

/// The [`Response::Error`] text a daemon answers a verb from the OTHER plane with — one spelling, so
/// the two refusals are each other's mirror and neither can drift into being less useful than the
/// other.
///
/// It names three things, because a wrong-plane dial is a CONFIGURATION mistake and each one is a
/// step of the fix: the verb, the daemon that does serve it, and the settings key whose value the
/// client dialled the wrong daemon from. It deliberately does NOT print an address — this side knows
/// only where IT is listening, and telling a client "try 127.0.0.1:7879" from a box whose tunnel maps
/// a different port is worse than saying nothing.
///
/// ⚠ It rides a [`Response::Error`] frame rather than a dropped connection, per this protocol's
/// decode-vs-drop contract: a verb the peer does not serve is a bad *request*, not a bad
/// *connection*.
pub fn wrong_plane_message(verb: &str, serves: Plane, wanted: Plane) -> String {
    format!(
        "{verb} is a {wanted:?}-plane verb and this is the {serves:?} daemon — it is served by \
         `{}`. Point the client at that daemon ({}); this connection stays open and every \
         {serves:?}-plane verb still works on it.",
        wanted.served_by(),
        wanted.addr_key(),
    )
}

/// A request's variant name, for a log line or a refusal — never its PAYLOAD, which on the `Run*`
/// verbs is a client-supplied script and on every verb is remote text.
///
/// ⚠ Moved here from `vike_datahub::server` when ruling 7 split the served surface: BOTH daemons log
/// refusals by verb name, and a second copy would be a second list to forget a variant from. The
/// match is exhaustive for the same reason [`plane_of`]'s is.
///
/// ⚠ **It returns the WIRE tag, not the Rust identifier, and the four `sweep` verbs are where the
/// two differ.** A refusal and a log line are both about a FRAME: the operator who reads them sent
/// `{"RunSweepProfile": …}` and can grep a capture for that string, while `RunParamscanProfile` —
/// the Rust identifier since the `sweep` -> `paramscan` rename — appears on no wire at all and
/// names nothing they can find. The pins on the variants ([`Request::RunParamscan`] and its three
/// siblings carry `#[serde(rename = "…")]`) are the same decision applied to the bytes; this is it
/// applied to the prose about them. `crates/vike-datahub-client/src/client.rs`'s `resp_kind`
/// follows the identical rule for [`Response`].
pub fn request_kind(r: &Request) -> &'static str {
    match r {
        Request::Hello { .. } => "Hello",
        Request::Auth { .. } => "Auth",
        Request::Ping => "Ping",
        Request::RunBacktest(_) => "RunBacktest",
        Request::RunSlice { .. } => "RunSlice",
        // ⚠ WIRE tags, not the Rust identifiers — see this function's doc.
        Request::RunParamscan { .. } => "RunSweep",
        Request::RunWalkforward { .. } => "RunWalkforward",
        Request::RunParamscanProfile { .. } => "RunSweepProfile",
        Request::RunWalkforwardProfile { .. } => "RunWalkforwardProfile",
        Request::RunStudy(_) => "RunStudy",
        Request::RunNamed(_) => "RunNamed",
        Request::NamedStrategies => "NamedStrategies",
        Request::LoadBars { .. } => "LoadBars",
        Request::ScanQuotes { .. } => "ScanQuotes",
        Request::ScanTrades { .. } => "ScanTrades",
        Request::ScanBookUpdates { .. } => "ScanBookUpdates",
        Request::ScanDepth { .. } => "ScanDepth",
        Request::ScanCohort { .. } => "ScanCohort",
        Request::ScanPerpMetrics { .. } => "ScanPerpMetrics",
        Request::ScanEquity { .. } => "ScanEquity",
        Request::ScanExecFills { .. } => "ScanExecFills",
        Request::PropertiesAsOf { .. } => "PropertiesAsOf",
        Request::ListSeries => "ListSeries",
        Request::Inventory => "Inventory",
        Request::SeriesGaps { .. } => "SeriesGaps",
        Request::ListStrategies => "ListStrategies",
        Request::Coverage => "Coverage",
        Request::Backfill { .. } => "Backfill",
        Request::SeedSeries { .. } => "SeedSeries",
        Request::VenueCatalog { .. } => "VenueCatalog",
        Request::DeleteSeries { .. } => "DeleteSeries",
        Request::MdSubscribe { .. } => "MdSubscribe",
        Request::MdUpdate { .. } => "MdUpdate",
    }
}

/// The scope a verb requires — the ONE authority for the read/write split on BOTH daemons.
///
/// ⚠ Moved here from `vike_datahub::server` by ruling 7, for the same reason [`plane_of`] lives
/// here: the compute daemon enforces the identical rule over the identical [`Scope`] values, and
/// `vike-backtest` (layer 50) cannot see `vike-datahub` (layer 65). One table, two consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbScope {
    /// A pre-auth handshake frame — [`Request::Hello`] / [`Request::Auth`]. Not a verb at all: it is
    /// how a connection GETS a scope, so it cannot require one.
    Handshake,
    /// Servable by a connection authenticated under [`Scope::Read`] or [`Scope::Write`].
    Read,
    /// Servable ONLY by a connection authenticated under [`Scope::Write`].
    Write,
}

/// Classify one request's required scope. **The match is EXHAUSTIVE — no `_` arm, deliberately** —
/// so a new wire verb fails to COMPILE here until somebody classifies it, rather than silently
/// defaulting to whichever side the catch-all happened to pick. (A default of `Observe` would leak a
/// write; a default of `Control` would silently break a read for observe clients. Neither is a
/// decision a wildcard should make.)
///
/// The split, with the reasoning that is not self-evident spelled out:
///
/// - **Reads are [`VerbScope::Read`]**: they answer from the store and change nothing.
///   [`Request::ListStrategies`] joins them — it returns a compile-time const roster, not store
///   contents, and after ruling 7 it is answered by the COMPUTE daemon, which is where a roster of
///   compiled-in strategies belongs. `Ping` is Observe rather than Handshake: a liveness probe from
///   an unauthenticated peer is a free oracle for "is this address a vike daemon", and there is no
///   reason to hand that out.
/// - **[`Request::Backfill`] is [`VerbScope::Write`]**: it WRITES the served store and spends
///   venue-API budget from that box's own IP. This is the verb
///   `docs/decisions/0025-datahub-remote-posture.md` calls "the class change" — the reason the record
///   chose authentication over tunnel-only-forever at all.
/// - **⚠ ALMOST every `Run*` verb is [`VerbScope::Write`], and the reason is the SOURCE it can
///   carry rather than the act of running.** This is the classification a reader is most likely to
///   get wrong, since these verbs *return* an answer and look like reads. They are not:
///   `RunSlice`/`RunSweep`/`RunWalkforward` carry a [`WireSpec`]`::Rhai` source that reaches
///   `StrategySpec::rhai`, and `RunBacktest`/`RunParamscanProfile`/`RunWalkforwardProfile` carry a
///   profile whose `[strategy.params].src` resolves through
///   `vike_backtest::harness::registry`'s `"rhai"` arm to the same compiler. That is remote code
///   execution BY DESIGN — it is how `vike-cli backtest --script` works — and the correct posture
///   for it is the write scope.
///
///   ⚠ **This sentence read "EVERY `Run*` verb" until 2026-09-16, and it was one verb too wide even
///   then.** [`Request::RunStudy`] compiles nothing: `vike_studio_core::wire_run`'s `study_run_fn`
///   hardcodes the Rust study tier, so the interpreted tier is unreachable over this wire, and its
///   `recipe_toml` parses to a params value no `src` reader consults. It is Control for the OTHER
///   reason — it WRITES a run directory on the server's disk — and its own arm comment says so.
///   `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 6 is the correction; the arm
///   does not move, because writing a run directory is sufficient on its own.
///
///   ⚠ **And [`Request::RunNamed`] is [`VerbScope::Read`]**, which is 0064's subject. Read the
///   predicate above literally: it names FIELDS. A verb that structurally cannot carry source is a
///   different verb, and this one carries [`crate::named_run::NamedParam`] — `i64`/`f64`/`bool`,
///   with no variant a script could occupy — resolving through a crate that cannot name
///   `vike-script`, so a `src` is UNREAD rather than refused. Its COST is bounded by the constants
///   in [`crate::named_run`] plus a server-side run-slot count, and those bounds are the CONDITION
///   of the classification rather than a consequence of it: 0064's decision 3 records that not one
///   dimension a request names is bounded on the verbs above, so the denial of service survives
///   removing the compiler entirely. **Moving this line is re-deciding that record, not a
///   reclassification.**
///
///   The cost of the Control rows, stated so nobody has to rediscover it: an Observe client cannot
///   run a SCRIPT, a SEARCH or a walk-forward, and cannot have a run persisted. It can run one
///   named strategy over one bounded window. That is intended, and it is the shape
///   `crates/vike-app-core/src/backend_registry.rs`'s `datahub_observe_key` needed — a desktop that
///   must never hold the key which compiles client-supplied Rhai.
pub fn required_scope(request: &Request) -> VerbScope {
    match request {
        Request::Hello { .. } | Request::Auth { .. } => VerbScope::Handshake,

        Request::Ping
        | Request::LoadBars { .. }
        | Request::ScanQuotes { .. }
        | Request::ScanTrades { .. }
        // The SIX tick-level and research reads (`docs/decisions/0084`). Data plane and
        // Observe scope for the same reason as their neighbours above: they are
        // `vike_data::HistStore` trait reads answered from the DATA daemon's own store handle,
        // bounded by the range the caller names, and they write nothing.
        | Request::ScanBookUpdates { .. }
        | Request::ScanDepth { .. }
        | Request::ScanCohort { .. }
        | Request::ScanPerpMetrics { .. }
        | Request::ScanEquity { .. }
        | Request::ScanExecFills { .. }
        | Request::PropertiesAsOf { .. }
        | Request::ListSeries
        | Request::Inventory
        | Request::SeriesGaps { .. }
        // Coverage is the cross-kind READ behind the Data Manager's "Partial" column (split-plane
        // spec §6 Q2) — a plain `vike_data::HistStore` trait verb like `inventory`/`series_gaps`,
        // answering what a store already holds and writing nothing.
        | Request::Coverage
        // ⚠ THE MARKET-DATA VERBS ARE OBSERVE, and the argument is not "they look like reads" —
        // `Backfill` looks like a read too and is Control. The distinction that survives scrutiny is
        // BOUNDED-BY-AN-OPERATOR-SET-CEILING versus not: a `Backfill`'s venue-budget cost is
        // unbounded per request and the CLIENT names the range, while a subscription's is bounded
        // by `MD_MAX_KEYS_PER_VENUE` and `MD_LINGER`, which no request can move. The alternative
        // forces a desktop that only wants a DOM ladder to hold a key that also compiles
        // client-supplied Rhai on the compute daemon — strictly more authority for strictly less
        // reason. ⚠ It is a DECISION, not an inherited default:
        // `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` carries the
        // argument AND the posture it inherits (on a KEY-LESS server every verb but `DeleteSeries`
        // is served to whoever reaches the loopback socket, per `docs/decisions/0050`).
        | Request::MdSubscribe { .. }
        | Request::MdUpdate { .. }
        // ⚠⚠ **THE ONE WRITE VERB ON THE OBSERVE SIDE, AND IT IS A DECISION AGAINST A STANDING
        // FORWARD RULING.** `docs/decisions/0052`'s *What would reopen this* predicted that "a
        // market-data verb that WRITES … would be a `Backfill`-class write and would take its
        // scope". `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` argues that the
        // predicate is the CLASS, and that this verb is not in it — READ IT BEFORE MOVING THIS ARM
        // OR ADDING A FIELD TO THE VARIANT. The short form, which is 0052's own rule unamended plus
        // one axis it never needed:
        //   * COST — `Backfill` is Control because the CLIENT names the range. `SeedSeries` names a
        //     series and NOTHING else; the window, the bar count, the venue set, the interval set,
        //     the rate and the per-process series cap are all server constants no request can move.
        //   * REACH — a subscription leaves nothing behind and a write does, so bounding the cost
        //     is not sufficient. This write is ADDITIVE and IDEMPOTENT (commit-key dedup, so no
        //     existing row is reachable), CONTAINED (the server's own venue x interval sets), and
        //     OPERATOR-ARMED with an unarmed server still ANSWERING the verb and writing nothing.
        // All four together, or it goes back to Control — that is 0057's decision 3 and it is the
        // constraint the NEXT write-shaped verb has to argue against.
        // The precedent that makes the alternative absurd: `MdSubscribe` above already spends the
        // same rate-limited venue budget from the same IP with NO operator opt-in, at 800 weight/min
        // steady (`crates/vike-datahub/src/md/mod.rs`'s `MD_LINGER`), where this lane's SATURATION
        // ceiling is 60/min and its steady state is zero.
        | Request::SeedSeries { .. }
        // ⚠⚠ **THE VENUE CATALOG, AND IT IS OBSERVE FOR A DIFFERENT REASON FROM ITS NEIGHBOUR
        // ABOVE — READ `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`
        // BEFORE MOVING THIS ARM OR ADDING A FIELD TO THE VARIANT.** `SeedSeries` is a WRITE that
        // earns Observe by passing 0058's four-part rule. This verb does not pass that rule; it is
        // OUTSIDE it, and the difference is load-bearing:
        //   * NOT A WRITE — nothing enters the served store. No `append_*`, no commit key, no row.
        //     It is invisible to `ListSeries`/`Inventory`/`Coverage` and unreachable by
        //     `DeleteSeries`, so 0050's "a removal takes the only copy" asymmetry has no surface
        //     here. 0058's decision 3 opens with the words "a write verb"; this is not one, so the
        //     rule is INAPPLICABLE rather than satisfied — and its additive/idempotent leg is
        //     declared VACUOUS in 0062 rather than claimed, because the list-replace happens only
        //     in the CLIENT's own on-disk cache.
        //   * COST — the request names a VENUE and nothing else, and that dimension is drawn from
        //     the gated `vike_model::VENUES` roster intersected with the server's own table. This
        //     is the one verb on this side whose single client-named dimension is bounded by an
        //     ENUMERABLE SET rather than by a rate: it CLOSES the residual 0058 declared for
        //     `SeedSeries`' free symbol.
        //   * CREDENTIALS — the server's table admits only PUBLIC-endpoint providers, so no client
        //     can make this daemon authenticate as its operator (0062 decision 3). That hazard is
        //     bounded by no rate limit, which is why it is closed by construction and not by a
        //     switch.
        // The precedent that makes the alternative absurd is unchanged from its neighbour's:
        // `MdSubscribe` above already spends the same rate-limited venue budget from the same IP
        // with NO operator opt-in at 800 weight/min steady, where this lane is 1 weight/min at
        // SATURATION against binance `fapi` and its steady state is bounded below that again by a
        // memo TTL.
        | Request::VenueCatalog { .. }
        | Request::ListStrategies
        // ⚠⚠ **THE NAMED RUN, AND IT IS THE ONLY `Run*` VERB ON THIS SIDE OF THE TABLE — READ
        // `docs/decisions/0064-a-named-run-carries-no-source.md` BEFORE MOVING THIS ARM OR ADDING
        // A FIELD TO THE VARIANT.** Its two neighbours above each earn Observe on a different
        // ground (a write that passes 0058's four-part rule; a fetch that is not a write at all);
        // this one earns it on the predicate the ⚠ at the top of this function actually states:
        //   * SOURCE — the Control rows above are Control because of what they can CARRY, not
        //     because running spends CPU. This request carries `crate::named_run::NamedParam`
        //     (`i64`/`f64`/`bool`), which has no variant a script could occupy, and it resolves
        //     through `vike_user_strategies::named_run::resolve`, whose crate cannot name
        //     `vike-script` — a dependency closure the layer gate already holds, so a `src` is
        //     UNREAD rather than refused. 0064's decision 2.
        //   * COST — and this is the leg that had to be BUILT rather than argued, because 0064's
        //     decision 3 found that on this daemon NOT ONE dimension a request names is bounded:
        //     the window is `Option<i64>` (`None` = the whole store), the grid takes an unchecked
        //     `product()` allocated before a point runs, the trial and split counts have no
        //     ceiling, and `serve` spawns one unbounded thread per connection. The denial of
        //     service therefore SURVIVES removing the compiler. So this verb is the SINGLE-POINT
        //     shape (no grid, no trials, no splits — the variant has no field a search can occupy),
        //     its window is capped by `crate::named_run::NAMED_RUN_MAX_BARS` and REFUSED rather
        //     than clamped, its interval comes from a server-owned set, its symbol and venue go
        //     through the same validators the seed and catalog verbs use, and the server holds a
        //     process-wide run-slot count that refuses rather than queues.
        //   * WRITES — nothing. 0064's decision 4: a run directory is durable shared state on the
        //     operator's disk under a runs root NOTHING prunes, so persisting is `RunStudy`, which
        //     is Control and stays there.
        // The bounds are the CONDITION of this arm, not a claim about it. Adding a search dimension
        // is 0064's first reopener.
        | Request::RunNamed(_)
        // ...and the roster it enumerates, which must be answerable to the same credential that
        // runs it (0064's decision 7) — otherwise a client names into the dark.
        | Request::NamedStrategies => VerbScope::Read,

        // The WRITE verb...
        Request::Backfill { .. }
        // ...and the DESTRUCTIVE one. Control is necessary and NOT sufficient: the data server
        // refuses it outright on a KEY-LESS server, where `Scope::Write` is a word nothing
        // enforces — see [`FEATURE_DELETE_SERIES`]. This arm is what stops an OBSERVE connection on
        // a keyed server reaching it.
        | Request::DeleteSeries { .. }
        // ...and the six that compile client-supplied Rhai — see the ⚠ above.
        | Request::RunBacktest(_)
        | Request::RunSlice { .. }
        | Request::RunParamscan { .. }
        | Request::RunWalkforward { .. }
        | Request::RunParamscanProfile { .. }
        | Request::RunWalkforwardProfile { .. }
        // A study RUNS code on the backend and WRITES a run directory there — the same
        // classification every `Run*` verb already has, and the one
        // `crates/vike-cli/src/cmd/study.rs`'s connect already negotiates under.
        | Request::RunStudy(_) => VerbScope::Write,
    }
}

/// Whether a connection authenticated under `authed` may send a verb requiring `needed`.
///
/// [`VerbScope::Handshake`] is `false` here on purpose: those frames belong to the pre-auth phase,
/// and a SECOND `Hello`/`Auth` on an established connection is a protocol error (it would be a
/// re-negotiation, and a scope that can be re-negotiated after the fact is not a ceiling).
///
/// ⚠ **[`Scope::Account`] admits NOTHING here, and the `matches!` arms are why that is deliberate
/// rather than an oversight.** That scope is the TRADEHUB node's
/// (`docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md` §3c part 2); this
/// server holds no admin key at all — `node_keys_from_vars` reads two names and leaves it empty,
/// so its handshake refuses the scope before reaching a verb. Listing it beside `Control` here
/// would grant this service's compute verbs to a capability it has no way to authenticate, which
/// is the failure the enum's own doc calls *not a superset*.
pub fn scope_admits(authed: Scope, needed: VerbScope) -> bool {
    match needed {
        VerbScope::Handshake => false,
        VerbScope::Read => matches!(authed, Scope::Read | Scope::Write),
        VerbScope::Write => matches!(authed, Scope::Write),
    }
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

/// The [`Response::SeriesSeeded`] payload: what the server DID about one [`Request::SeedSeries`].
///
/// ⚠ **Every field here is an OUTCOME, and the request carried none of them.** That asymmetry is
/// the shape `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` rests on: the client
/// names a series, the server names the window, and this struct is how the client learns what the
/// window was. A client that starts deriving a NEXT request from these numbers is building the
/// range parameter 0057's reopen list forbids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedDone {
    /// **Whether the operator armed this lane at all.** `false` = `VIKE_DATAHUB_CHART_SEED` is
    /// unset on the server: nothing was fetched, nothing was written, and this is a SUCCESS.
    ///
    /// ⚠ A client must not treat `false` as an error — it is the default configuration and the
    /// server is behaving correctly. It IS worth telling the operator about, because from the
    /// chart's side an unarmed server and an empty store look identical
    /// (`vike_app_core::chart_seed::render_chart_seed_status` is where that sentence is written).
    pub armed: bool,
    /// **Whether this series had already been seeded by this process.** `true` = the ledger
    /// answered, no token was spent and no venue was called; `rows_written` is 0 and the timestamps
    /// describe what the store already holds. Distinct from `rows_written == 0` on a fresh seed,
    /// which means the venue was asked and the window was already ingested.
    pub repeated: bool,
    /// Rows the collector wrote (0 = the window was already ingested, or `armed`/`repeated` short
    /// -circuited the fetch — all three are success).
    pub rows_written: u64,
    /// The inclusive epoch-ms window the SERVER chose (`crate::seed::seed_range`), so a client can
    /// report what it was given. `None` only when nothing was fetched.
    pub range: Option<(i64, i64)>,
    /// First bar ts now stored in that window, read back through the served handle (`None` = the
    /// window holds no bars — an honest answer for a symbol the venue does not list).
    pub first_ts: Option<i64>,
    /// Last bar ts now stored in that window.
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
    /// exact shape the `backtest --json` bin emits).
    ///
    /// ⚠ `BacktestReport` DERIVES `Deserialize` now (it gained it with the run-artifact stage, so a
    /// stored `report.json` could be read back at all), so a client that wants the typed value can
    /// `serde_json::from_str` this string TODAY — this doc said that was "a later phase" and the
    /// phase has landed. The wire stays TEXT rather than a typed DTO, which is a separate decision
    /// and unchanged: the protocol is then independent of `vike-backtest`'s internal serde surface,
    /// so a field added there does not become a frame-compatibility question.
    Report(String),
    /// Reply to [`Request::RunSlice`] (PR-3): the rendered [`WireRunResult`] — the equity curve,
    /// closed trades, and the few counters the Studio shows, built from a `BacktestResult` at the
    /// server boundary. A run FAILURE (bad script/slice/params) comes back as [`Response::Error`]
    /// (the stringified [`crate::wire_studio::WireRunError`]), not here.
    RunResult(WireRunResult),
    /// Reply to [`Request::RunParamscan`] (PR-4): the ranked [`WireParamscanResult`] — the entries
    /// plus the deflated-Sharpe / PBO scores, built next to the data. A run FAILURE (bad
    /// script/slice/params) comes back as [`Response::Error`], not here.
    ///
    /// ⚠ `#[serde(rename = "SweepResult")]` pins the wire tag; see [`Request::RunParamscan`] for why.
    #[serde(rename = "SweepResult")]
    ParamscanResult(WireParamscanResult),
    /// Reply to [`Request::RunWalkforward`] (PR-4): the stitched [`WireWalkforwardResult`] — the OOS
    /// windows + stitched equity curve + summary stats. A run FAILURE comes back as
    /// [`Response::Error`], not here.
    WalkforwardResult(WireWalkforwardResult),
    /// Reply to [`Request::RunParamscanProfile`] (v7): the RANKED sweep as **JSON text**
    /// (`serde_json::to_string(&vike_backtest::harness::ParamscanReport)` — the exact shape the
    /// `backtest --sweep --json` bin emits). Rows arrive already ordered best-first by the
    /// requested metric, each carrying its own `BacktestReport`, so a client renders
    /// server-computed stats and never re-implements a metric.
    ///
    /// Text rather than a typed DTO for the DECOUPLING reason [`Response::Report`] gives — the wire
    /// schema stays independent of `vike-backtest`'s internal serde surface — and, unlike that one,
    /// also because `vike_backtest::harness::ParamscanReport` is genuinely `Serialize`-ONLY and so
    /// cannot be read back into a typed value at all. ⚠ The two used to share one reason and no
    /// longer do: `BacktestReport` gained `Deserialize` with the run-artifact stage; `ParamscanReport`
    /// did not.
    ///
    /// ⚠ `#[serde(rename = "SweepReport")]` pins the wire tag; see [`Request::RunParamscan`] for why.
    #[serde(rename = "SweepReport")]
    ParamscanReport(String),
    /// Reply to [`Request::RunWalkforwardProfile`] (v7): the stitched
    /// `vike_backtest::walkforward::WalkForwardReport` as **JSON text** — the OOS windows, the
    /// stitched equity curve, and the three summary scalars. Text for the same reason as
    /// [`Response::ParamscanReport`].
    WalkforwardReport(String),
    /// Reply to [`Request::RunStudy`]: the run that is now on the BACKEND's disk, as JSON text.
    ///
    /// ⚠ **Text, for the same reason [`Response::ParamscanReport`] is text**: the report types are
    /// Serialize-ONLY, so a typed variant would require `Deserialize` on the whole run-manifest and
    /// study-outcome tree. One direction, one shape, and no client-side type to keep in step.
    StudyReport(String),
    /// A server-side failure (invalid profile, missing strategy, data error, …), stringified.
    Error(String),
    /// Reply to [`Request::LoadBars`]: the derived OHLCV bars, ts-ascending.
    Bars(Vec<Bar>),
    /// Reply to [`Request::ScanQuotes`]: the L1 quotes, ts-ascending.
    Quotes(Vec<QuoteTick>),
    /// Reply to [`Request::ScanTrades`]: the executed trades, ts-ascending.
    Trades(Vec<TradeTick>),
    /// Reply to [`Request::ScanBookUpdates`]: the raw L2 book updates, ts-ascending.
    BookUpdates(Vec<BookUpdate>),
    /// Reply to [`Request::ScanDepth`]: the conflating depth lane's updates, ts-ascending.
    ///
    /// ⚠ The same payload type as [`Self::BookUpdates`] and deliberately a SEPARATE variant — see
    /// [`Request::ScanDepth`] for why sharing one would make a desync between two different `kind=`
    /// partitions decode as plausible data.
    Depth(Vec<BookUpdate>),
    /// Reply to [`Request::ScanCohort`]: the cohort rows, ts-ascending.
    Cohort(Vec<CohortRow>),
    /// Reply to [`Request::ScanPerpMetrics`]: the perpetual-swap metric rows, ts-ascending.
    PerpMetrics(Vec<PerpMetricRow>),
    /// Reply to [`Request::ScanEquity`]: the equity samples, ts-ascending.
    Equity(Vec<EquitySample>),
    /// Reply to [`Request::ScanExecFills`]: the Tier-2 exec fill rows.
    ExecFills(Vec<ExecFillRow>),
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
    /// Reply to [`Request::NamedStrategies`]: the roster a NAMED RUN would resolve, plus whether the
    /// lane is armed. See [`crate::named_run::NamedRoster`] — and note `armed: false` is a SUCCESS.
    NamedStrategies(crate::named_run::NamedRoster),
    /// Reply to [`Request::RunNamed`]: what the run did, which is one of THREE outcomes and only one
    /// of them an error. See [`crate::named_run::NamedRunOutcome`].
    ///
    /// ⚠ BOXED for `clippy::large_enum_variant`'s sake, the same reason [`Response::Properties`]
    /// boxes its payload: the `Ran` arm carries a whole [`crate::wire_studio::WireRunResult`]. serde
    /// treats `Box<T>` transparently, so the wire shape is identical to an unboxed one.
    NamedRun(Box<crate::named_run::NamedRunOutcome>),
    /// Reply to [`Request::Backfill`]: the fetch ran and the rows are IN THE STORE (write-through
    /// happens before this frame is written). See [`BackfillDone`] for the field contract; a
    /// fetch/validation failure is [`Response::Error`], never a partial `BackfillDone`.
    BackfillDone(BackfillDone),
    /// Reply to [`Request::SeedSeries`]: what the server did. See [`SeedDone`] for the field
    /// contract — and note that an UNARMED server answers this variant rather than an
    /// [`Response::Error`], which [`FEATURE_SEED_SERIES`] argues is the point rather than a
    /// leniency. A validation or fetch FAILURE is [`Response::Error`], never a partial `SeedDone`.
    SeriesSeeded(SeedDone),
    /// Reply to [`Request::VenueCatalog`]: one venue's instruments, or the typed reason there are
    /// none. See [`crate::catalog::CatalogListing`].
    ///
    /// ⚠ **Every ROUTINE outcome is this variant, including the refusals** — an unarmed lane, a
    /// venue with no bulk list, a credentialed venue and a venue this build does not serve are all
    /// [`crate::catalog::CatalogOutcome`] values, not [`Response::Error`]s. That is deliberate:
    /// "which venues can I refresh" is a question the Data Manager asks every time it opens, and a
    /// stream of errors is the wrong shape for an answer it expects. [`Response::Error`] is kept
    /// for the genuinely exceptional — a malformed venue string, or a provider that failed mid
    /// fetch.
    VenueCatalog(crate::catalog::CatalogListing),
    /// Reply to [`Request::DeleteSeries`]: the plan, and what it did. See [`DeleteDone`]. A
    /// provenance REFUSAL, a key-less server and an unknown kind are all [`Response::Error`] —
    /// nothing was deleted in any of those cases, and this variant never reports a run that did not
    /// happen.
    Deleted(DeleteDone),
    /// Reply to [`Request::MdSubscribe`]. **POSITIONAL, and the LAST positional frame this socket
    /// will carry** — see [`crate::market`]'s module doc.
    MdSubscribed {
        /// The session token, for a later [`Request::MdUpdate`] on a SHORT-LIVED connection.
        session: crate::market::MdSessionId,
        /// The specs now served, echoing the server's AUTHORITATIVE form — including any CLAMPED
        /// `depth_levels`. A clamp is an acceptance with a smaller number, never a refusal, which
        /// is what lets a client that asked for 200 levels LEARN it got 50.
        accepted: Vec<crate::market::MdSpec>,
        /// The specs refused, each with its own typed reason.
        refused: Vec<(crate::market::MdSpec, crate::market::MdRefusal)>,
        /// **The server's own heartbeat period in milliseconds.**
        ///
        /// ⚠ This field is what keeps `crate::market::MD_READ_TIMEOUT` from being a COMPILE-TIME
        /// contract between two independently-deployed binaries: the client arms
        /// `max(MD_READ_TIMEOUT, 3 × heartbeat_ms)`. The ACCOUNT plane could not do this —
        /// `vike_tradehub_client::liveness`'s `OBSERVE_READ_TIMEOUT` is a constant on both sides
        /// and that module's own doc names the hazard. This wire fixed it by carrying the number.
        heartbeat_ms: u64,
    },
    /// Reply to [`Request::MdUpdate`]. **POSITIONAL**, on the short-lived control connection the
    /// request arrived on — the frames themselves go to the STREAM connection.
    MdUpdated {
        /// Newly-served specs, in the server's authoritative form.
        accepted: Vec<crate::market::MdSpec>,
        /// Refused additions, each with its own typed reason.
        refused: Vec<(crate::market::MdSpec, crate::market::MdRefusal)>,
        /// The specs this update actually RELEASED — matched on their key, so a `remove` naming a
        /// spec the session never held is silently absent here rather than an error.
        released: Vec<crate::market::MdSpec>,
    },
    /// One PUSHED market-data frame. Appears ONLY on a stream connection, and after
    /// [`Response::MdSubscribed`] it is the only variant that appears there at all — which is what
    /// makes a stream reader's `match` on this variant a complete protocol, with anything else a
    /// desync.
    ///
    /// BOXED, following [`Response::Properties`]'s own argument: serde treats `Box<T>`
    /// transparently so the wire shape is identical, and `Response` is cloned and matched on every
    /// POSITIONAL path — it should not grow for a variant only one kind of connection ever sees.
    /// (Measured from the declarations, `crate::market::MdFrame`'s largest variant is a
    /// `BookSnapshot` at ~128 B, so the box is a size win rather than a `clippy::large_enum_variant`
    /// necessity.)
    Md(Box<crate::market::MdFrame>),
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
        write_frame(&mut buf, &Request::Auth { scope: Scope::Write, mac: vec![9u8; 32] }).unwrap();
        write_frame(&mut buf, &Response::AuthOk { scope: Scope::Write }).unwrap();
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
                assert_eq!(scope, Scope::Write);
                assert_eq!(mac, vec![9u8; 32]);
            }
            other => panic!("expected Auth, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::AuthOk { scope } => assert_eq!(scope, Scope::Write),
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
            cost_model: None,
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
        let sweep = Request::RunParamscan {
            spec: WireSpec::Rhai("fn on_bar() {}".to_string()),
            slice: slice(),
            paramscan: WireParamscan { axes: vec![("fast".to_string(), vec![3.0, 5.0])] },
            params: Some(WireEngineParams {
                cash: Some(5000.0),
                fee_rate: Some(0.001),
                slippage: None,
            }),
        };
        let wf = Request::RunWalkforward {
            spec: WireSpec::Rhai("fn on_bar() {}".to_string()),
            slice: slice(),
            walkforward: WireWalkforward::fixed(4),
            params: None,
        };

        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &sweep).unwrap();
        write_frame(&mut buf, &wf).unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunParamscan { params, .. } => {
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
            Request::RunParamscan { params, .. } => {
                assert!(params.is_none(), "absent params -> None")
            }
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
            &Request::RunParamscanProfile {
                profile_toml: PROFILE.to_string(),
                rank_by: Some("max_dd".to_string()),
                search: None,
            },
        )
        .unwrap();
        write_frame(
            &mut buf,
            &Request::RunWalkforwardProfile { profile_toml: PROFILE.to_string() },
        )
        .unwrap();
        write_frame(&mut buf, &Response::ParamscanReport("{\"rows\":[]}".to_string())).unwrap();
        write_frame(&mut buf, &Response::WalkforwardReport("{\"windows\":[]}".to_string()))
            .unwrap();

        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunParamscanProfile { profile_toml, rank_by, search } => {
                assert_eq!(profile_toml, PROFILE, "the profile TOML crosses verbatim");
                assert_eq!(rank_by.as_deref(), Some("max_dd"));
                assert!(search.is_none(), "an unselected search is absent, not an empty struct");
            }
            other => panic!("expected RunParamscanProfile, got {other:?}"),
        }
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunWalkforwardProfile { profile_toml } => assert_eq!(profile_toml, PROFILE),
            other => panic!("expected RunWalkforwardProfile, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::ParamscanReport(json) => assert_eq!(json, "{\"rows\":[]}"),
            other => panic!("expected ParamscanReport, got {other:?}"),
        }
        match read_frame::<_, Response>(&mut cur).unwrap() {
            Response::WalkforwardReport(json) => assert_eq!(json, "{\"windows\":[]}"),
            other => panic!("expected WalkforwardReport, got {other:?}"),
        }
    }

    /// A v7 frame that OMITS `search` still decodes — the `#[serde(default)]` contract the v6
    /// `params` field and the `rank_by` field both already have. This is the OLD-CLIENT /
    /// NEW-SERVER direction, and it is the half a version bump would have broken.
    #[test]
    fn profile_sweep_without_search_decodes_as_none() {
        let body = br#"{"RunSweepProfile":{"profile_toml":"[data]\n"}}"#;
        match serde_json::from_slice::<Request>(body).expect("must decode without search") {
            Request::RunParamscanProfile { search, rank_by, .. } => {
                assert!(search.is_none(), "absent search -> None");
                assert!(rank_by.is_none(), "absent rank_by is unchanged");
            }
            other => panic!("expected RunParamscanProfile, got {other:?}"),
        }
    }

    /// The selector survives `write_frame` -> `read_frame` with every field populated, and the
    /// values cross as the TOKENS the operator typed — the server's parser is the one authority
    /// for what `"128"` means, so nothing is re-typed on the way.
    #[test]
    fn a_search_selector_survives_the_frame_codec() {
        let search = WireSearch {
            optimizer: Some("tpe".to_string()),
            euler_depth: None,
            trials: Some("128".to_string()),
            seed: Some("7".to_string()),
        };
        let mut buf: Vec<u8> = Vec::new();
        write_frame(
            &mut buf,
            &Request::RunParamscanProfile {
                profile_toml: "[paramscan]\nfast = [5, 10]\n".to_string(),
                rank_by: Some("multi".to_string()),
                search: Some(search.clone()),
            },
        )
        .unwrap();
        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunParamscanProfile { search: got, rank_by, .. } => {
                assert_eq!(got.as_ref(), Some(&search));
                assert_eq!(rank_by.as_deref(), Some("multi"));
            }
            other => panic!("expected RunParamscanProfile, got {other:?}"),
        }
    }

    /// ⚠ THE PREDICATE THE CLIENT REFUSES ON. An explicit `grid` needs no capability — an old
    /// daemon that drops the field runs the grid, which is exactly what was asked, so refusing it
    /// would be a false refusal. Everything else would be SILENTLY DOWNGRADED and must not be sent.
    #[test]
    fn only_a_selector_an_old_daemon_would_change_needs_the_capability() {
        let grid = WireSearch { optimizer: Some("grid".to_string()), ..WireSearch::default() };
        assert!(
            !grid.needs_capability(),
            "an explicit grid is what an old daemon would run anyway"
        );
        assert!(!WireSearch::default().needs_capability(), "an empty selector asks for nothing");
        for (label, w) in [
            (
                "a method",
                WireSearch { optimizer: Some("tpe".to_string()), ..WireSearch::default() },
            ),
            ("a budget", WireSearch { trials: Some("8".to_string()), ..WireSearch::default() }),
            ("a seed", WireSearch { seed: Some("7".to_string()), ..WireSearch::default() }),
            ("a depth", WireSearch { euler_depth: Some("2".to_string()), ..WireSearch::default() }),
        ] {
            assert!(w.needs_capability(), "{label} would be silently dropped by an old daemon");
        }
        // ⚠ A knob under the GRID needs it too: the refusal it must produce
        // (`--trials is a tpe or genetic flag`) is one an old daemon cannot produce at all — it
        // drops the field and reports success.
        let mixed = WireSearch {
            optimizer: Some("grid".to_string()),
            trials: Some("8".to_string()),
            ..WireSearch::default()
        };
        assert!(
            mixed.needs_capability(),
            "a knob under grid must reach a daemon that can refuse it"
        );
    }

    /// An EMPTY selector is the shape a caller must send as `None` — the property that keeps an
    /// ordinary grid frame byte-identical to the one shipped before this field existed.
    #[test]
    fn an_empty_selector_is_empty_and_a_written_one_is_not() {
        assert!(WireSearch::default().is_empty());
        assert!(!WireSearch { seed: Some("0".to_string()), ..WireSearch::default() }.is_empty());
    }

    /// The roster is a CONST, not a literal, and `DEFAULT_SEARCH_METHOD` is a member of it — the
    /// §15.1 one-roster gate's anchor leg. Every other surface's leg is in its own crate.
    #[test]
    fn the_default_search_method_is_in_the_roster() {
        assert!(SEARCH_METHODS.contains(&DEFAULT_SEARCH_METHOD), "{SEARCH_METHODS:?}");
        assert_eq!(SEARCH_METHODS[0], DEFAULT_SEARCH_METHOD, "the default is the first row");
    }

    /// The study request survives `write_frame` -> `read_frame` with its recipe TEXT intact — the
    /// property `vike-cli study` was built on: a PATH would be resolved against the BACKEND's
    /// filesystem, so the recipe an author is editing would be invisible to the run they started.
    #[test]
    fn a_study_request_survives_the_frame_codec() {
        let study = WireStudy {
            study: "cohort".to_string(),
            recipe_toml: "[learner]\nnum_iterations = 400\n".to_string(),
            from: "2026-04-07T05".to_string(),
            to: "1785906000".to_string(),
        };
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &Request::RunStudy(Box::new(study.clone()))).unwrap();
        let mut cur = Cursor::new(buf);
        match read_frame::<_, Request>(&mut cur).unwrap() {
            Request::RunStudy(got) => assert_eq!(*got, study),
            other => panic!("expected RunStudy, got {other:?}"),
        }
    }

    /// ⚠ THE THREE CLASSIFIERS, asserted rather than trusted to the compiler. Each of them is
    /// exhaustive, so a missing arm is a build failure — but WHICH arm was chosen is a judgement,
    /// and these are the three judgements: a study RUNS an engine (compute), it WRITES a run
    /// directory on the backend (Control, which `crates/vike-cli/src/cmd/study.rs`'s connect
    /// already negotiates under), and it names itself in a refusal.
    #[test]
    fn the_study_verb_is_a_control_scoped_compute_verb() {
        let r = Request::RunStudy(Box::new(WireStudy {
            study: "cohort".to_string(),
            recipe_toml: String::new(),
            from: "1".to_string(),
            to: "2".to_string(),
        }));
        assert_eq!(plane_of(&r), Plane::Compute);
        assert_eq!(request_kind(&r), "RunStudy");
        assert_eq!(required_scope(&r), VerbScope::Write);
    }

    /// `RunParamscanProfile` without the optional `rank_by` key still decodes (as `None` ⇒ the server's
    /// `sharpe` default) — the same `#[serde(default)]` contract the v6 `params` field has.
    #[test]
    fn profile_sweep_without_rank_by_decodes_as_none() {
        let body = br#"{"RunSweepProfile":{"profile_toml":"[data]\n"}}"#;
        match serde_json::from_slice::<Request>(body).expect("must decode without rank_by") {
            Request::RunParamscanProfile { rank_by, .. } => assert!(rank_by.is_none()),
            other => panic!("expected RunParamscanProfile, got {other:?}"),
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

    /// The `rec_venue=` pair ROUND-TRIPS, which is the whole reason the builder and the reader ship
    /// together: the server writes through one and the client reads through the other, so the
    /// spelling exists once.
    ///
    /// Modelled on `crates/vike-datahub-client/tests/market_data_negotiation.rs`'s
    /// `the_md_venue_feature_round_trips` — the twin this pair was built in the image of.
    #[test]
    fn the_rec_venue_feature_round_trips() {
        let features = vec![
            rec_venue_feature("binance"),
            FEATURE_MARKET_DATA.to_string(),
            rec_venue_feature("polymarket"),
        ];
        assert_eq!(
            advertised_rec_venues(&features),
            vec!["binance".to_string(), "polymarket".to_string()],
            "advertisement ORDER is the contract, and a named capability between two entries is \
             not one of them"
        );
    }

    /// The two per-venue prefixes describe DIFFERENT PLANES and neither reader may see the other's
    /// entries — the defect that would make a client report a venue as recordable because it
    /// happens to be servable (the shipped image serves six and records two).
    ///
    /// Also the EMPTY-value and whole-string-equality rules, which are
    /// [`FEATURE_REC_VENUE_PREFIX`]'s own claims.
    #[test]
    fn the_two_venue_planes_advertise_separately() {
        let both = vec![md_venue_feature("okx"), rec_venue_feature("binance")];
        assert_eq!(advertised_rec_venues(&both), vec!["binance".to_string()]);
        assert_eq!(advertised_md_venues(&both), vec!["okx".to_string()]);

        // An empty or blank value advertises nothing — "absence is the answer".
        assert!(advertised_rec_venues(&[FEATURE_REC_VENUE_PREFIX.to_string()]).is_empty());
        assert!(advertised_rec_venues(&["rec_venue=   ".to_string()]).is_empty());
        assert_eq!(
            advertised_rec_venues(&["rec_venue= bybit ".to_string()]),
            vec!["bybit".to_string()],
            "the value is trimmed, exactly as the md twin trims"
        );

        // A NAMED capability can neither satisfy nor shadow a per-venue entry.
        let named = vec![FEATURE_BACKFILL.to_string(), FEATURE_COVERAGE.to_string()];
        assert!(advertised_rec_venues(&named).is_empty());
        assert!(
            advertised_rec_venues(&[]).is_empty(),
            "no entries is an EMPTY answer, not a panic"
        );
    }
}
