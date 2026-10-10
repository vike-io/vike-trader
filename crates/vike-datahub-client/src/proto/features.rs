//! The `Welcome.features` capability vocabulary: every string a server advertises and a client
//! negotiates on, and the builder/reader pairs for the value-carrying entries.
//!
//! An additive verb is negotiated by one of these strings, never by a `PROTO_VERSION` bump, and has
//! three legs — the server advertises it exactly when it serves the verb, the client refuses
//! LOCALLY without sending when it is absent, and an older server answers the unknown variant with
//! `Response::Error` and KEEPS the connection:
//! `docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`.
//! Each constant below says only what is specific to it: WHEN it is advertised — a BUILD fact
//! (unconditional) or a RUNTIME fact (per mounted or armed lane), which differs per verb and is
//! load-bearing — and how its absence or refusal differs from the rule.
//!
//! Every capability check is whole-string equality (`f == FEATURE_*`), so a value-carrying entry
//! (`md_venue=`, `rec_venue=`, `import_format=`) can neither satisfy nor shadow a named capability.

/// Gates the backfill-on-demand verb ([`Request::Backfill`](super::Request::Backfill) /
/// [`Response::BackfillDone`](super::Response::BackfillDone), split-plane REQ-9) — the first verb
/// negotiated per decision 0112.
///
/// A RUNTIME fact: advertised only when the server holds collectors (a `backfill-serve` build with
/// a table mounted). The client refuses locally without it (`DatahubClient::backfill`).
pub const FEATURE_BACKFILL: &str = "backfill";

/// Gates the FUNDING lane of [`Request::Backfill`](super::Request::Backfill): an `interval` of
/// `vike_data::source::FUNDING_INTERVAL` (`"funding"`) fetches the funding-rate series, not bars.
/// Advertised only when the server's table mounts a funding source.
///
/// ⚠ The client must refuse locally without it: an older server does not drop such a request, it
/// refuses it with the forming-bar message ("`funding` has no bar width"), which reads as though
/// funding were unsupported by design.
pub const FEATURE_BACKFILL_FUNDING: &str = "backfill_funding";

/// Gates the operator's door onto RUNNING backfills —
/// [`Request::ListBackfills`](super::Request::ListBackfills) and
/// [`Request::CancelBackfill`](super::Request::CancelBackfill). One string for both: they read and
/// flag ONE registry (`docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md`
/// §4; `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`
/// is the scope verdict).
///
/// A RUNTIME fact, advertised exactly when [`FEATURE_BACKFILL`] is: the registry lives with the
/// mounted table. The client refuses locally without it ([`crate::DatahubClient::list_backfills`],
/// [`crate::DatahubClient::cancel_backfill`]).
///
/// ⚠ NOT the key-presence posture [`FEATURE_DELETE_SERIES`] takes: a key-less loopback datahub
/// advertises and serves both verbs, because it serves `Backfill` — 0101's verdict 1.
pub const FEATURE_BACKFILL_CANCEL: &str = "backfill_cancel";

/// Gates the CHART-GAP SEED verb ([`Request::SeedSeries`](super::Request::SeedSeries) /
/// [`Response::SeriesSeeded`](super::Response::SeriesSeeded) —
/// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`); negotiated per decision 0112.
///
/// A RUNTIME fact, advertised per ARMED LANE: a build with the collectors whose operator did not
/// set `VIKE_DATAHUB_CHART_SEED=1` advertises nothing, so a client can name the switch that is off
/// instead of drawing an empty chart and blaming the venue.
///
/// ⚠ **Its absence SUCCEEDS, writing nothing — unlike every other capability here, and that
/// difference IS the scope argument.** A server without `delete_series` REFUSES that verb; a server
/// without this one answers [`SeedDone::armed`](super::SeedDone::armed) `== false`. That is what
/// makes the `Scope::Read` classification honest: a read-scope connection may say which series a
/// chart is open on, never cause a write — whether one happens is the server's own configuration.
/// 0058's reach property 3 is that sentence, and removing it is in that record's reopen list.
pub const FEATURE_SEED_SERIES: &str = "seed_series";

/// Gates a chart-gap seed that NAMES THE INSTRUMENT'S KIND —
/// [`Request::SeedSeries`](super::Request::SeedSeries)'s `class` field,
/// `docs/decisions/0061-an-instrument-names-its-kind.md` Phase 3. An optional field an old server
/// would silently ignore, so it has its own string (decision 0112, verdict 3).
///
/// ⚠ **The field alone is the DEFECT.** [`Request`](super::Request) has no `deny_unknown_fields`,
/// so a daemon predating `class` decodes the frame, drops it, routes on the symbol alone and
/// reports rows written: a new client says PERPETUAL, an old daemon writes the SPOT tape under the
/// series the chart is about to read — 0061's measured bug, reproduced by its own fix. So:
///
/// - the server advertises it **UNCONDITIONALLY** — a BUILD fact, unlike [`FEATURE_SEED_SERIES`]
///   beside it, because it answers only "is this daemon older than the field". Advertised per armed
///   lane, an unarmed modern daemon would look old and be refused a class it understands;
/// - the client refuses **LOCALLY, WITHOUT SENDING**, only when the request carries a class
///   ([`crate::DatahubClient::seed_series_classed`]). ⚠ A class-less seed must still be SENT to a
///   daemon without this: refusing it would break every chart against every older daemon. That
///   false-refusal trap is what
///   `an_ordinary_grid_search_is_still_sent_to_a_daemon_without_the_capability` pins for the
///   search-method sibling, and its twin here pins the same;
/// - the SERVER re-checks, in `crates/vike-datahub/src/server/seed_series.rs`'s `seed_series_verb`,
///   against `vike_catalog::addressing_for` (the table the bridges' `route_target` consults), and
///   refuses BY NAME a class the venue's data path cannot address or honour on that spelling. The
///   client refusal says "your daemon is too old"; this one says "that is not a book I can reach".
pub const FEATURE_SEED_CLASS: &str = "seed_class";

/// Gates the VENUE-CATALOG verb ([`Request::VenueCatalog`](super::Request::VenueCatalog) /
/// [`Response::VenueCatalog`](super::Response::VenueCatalog) —
/// `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md`); negotiated
/// per decision 0112.
///
/// A RUNTIME fact, advertised per SERVING LANE. The lane is ON by default
/// (`docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`), so
/// absence is evidence that the operator WROTE `venue_catalog_off = true` — not an unconfigured
/// box, and not an empty list like the two roster venues that genuinely have no bulk list
/// ([`crate::catalog::CatalogRefusal::NoBulkList`]); [`crate::catalog::CatalogListing::describe`]
/// renders that sentence.
///
/// ⚠ **Its absence is a SUCCESS, not a refusal of the request**, as for [`FEATURE_SEED_SERIES`]:
/// such a server answers [`crate::catalog::CatalogOutcome::NotArmed`] having called no venue.
/// Unlike that verb, this one is not a store write at all (0062's decision 1).
pub const FEATURE_VENUE_CATALOG: &str = "venue_catalog";

/// Gates the NAMED-RUN verbs ([`Request::RunNamed`](super::Request::RunNamed) and
/// [`Request::NamedStrategies`](super::Request::NamedStrategies) —
/// `docs/decisions/0064-a-named-run-carries-no-source.md`); negotiated per decision 0112.
///
/// ⚠ **A BUILD fact, deliberately unlike [`FEATURE_SEED_SERIES`] and [`FEATURE_VENUE_CATALOG`].**
/// Those are advertised per armed lane, so an unarmed server and an OLD one look identical. 0064's
/// decision 8 requires an unarmed server to ANSWER, so this string says "this build has the verbs"
/// and the arming rides the answer ([`crate::named_run::NamedRunOutcome::NotArmed`],
/// [`crate::named_run::NamedRoster::armed`]): an old server advertises nothing, an unarmed one
/// advertises and answers `NotArmed`, an armed one runs.
///
/// - the compute daemon advertises it unconditionally (`vike_backtest::compute_server`'s
///   `served_features`, a DEFAULT-build module);
/// - the client refuses locally, WITHOUT SENDING ([`crate::DatahubClient::run_named`]): a daemon
///   predating a field DROPS it and answers a well-formed report, and a dropped WINDOW CEILING
///   would be exactly that failure;
/// - the server re-checks the arming at its own door and never trusts the advertisement.
pub const FEATURE_NAMED_RUN: &str = "named_run";

/// Gates the cross-kind coverage verb ([`Request::Coverage`](super::Request::Coverage) /
/// [`Response::Coverage`](super::Response::Coverage) — split-plane spec §6 Q2, the Data Manager's
/// "Partial" column over the wire); negotiated per decision 0112.
///
/// A BUILD fact, advertised UNCONDITIONALLY by every server built from this crate's `serve`
/// (`served_features`): coverage is a plain [`vike_data::HistStore`] trait verb with no table to
/// mount, so the negotiation answers exactly one question — *is this server older than the verb?*
/// The client refuses locally without it (`DatahubClient::coverage_report`), so the GUI renders an
/// honest note instead of an empty column.
pub const FEATURE_COVERAGE: &str = "coverage";

/// Gates the HISTORY-CHANNELS read
/// ([`Request::HistoryChannels`](super::Request::HistoryChannels) /
/// [`Response::HistoryChannels`](super::Response::HistoryChannels) —
/// `docs/superpowers/specs/2026-10-02-history-channels-step2-design.md` §2,
/// `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md` the scope verdict);
/// negotiated per decision 0112.
///
/// A BUILD fact, advertised UNCONDITIONALLY by every data daemon
/// (`crates/vike-datahub/src/server/features.rs`'s `served_features`): whether a lane is mounted
/// travels INSIDE the answer ([`crate::history::ChannelReport::mounted`]), so an unmounted server
/// is never mistaken for an old one. Without it the client refuses locally
/// ([`crate::DatahubClient::history_channels`]) and a caller renders its OWN compiled table under
/// [`crate::history::COMPILED_TABLE_CAPTION`] instead ([`crate::history::compiled_report`]).
pub const FEATURE_HISTORY_CHANNELS: &str = "history_channels";

/// Gates the raw L2 book-update read ([`Request::ScanBookUpdates`](super::Request::ScanBookUpdates)
/// / [`Response::BookUpdates`](super::Response::BookUpdates)), and documents the whole SIX-verb
/// family `docs/decisions/0084-only-the-datahub-touches-the-store.md` names:
/// [`FEATURE_SCAN_DEPTH`], [`FEATURE_SCAN_COHORT`], [`FEATURE_SCAN_PERP_METRICS`],
/// [`FEATURE_SCAN_EQUITY`] and [`FEATURE_SCAN_EXEC_FILLS`] point here.
///
/// ⚠ **These six exist because the wire NOT serving them kept four crates opening the store
/// directly.** 0084's verdict is one store reader, everyone else asking over the wire; a verb
/// nobody can ask for over the wire is a verb whose consumer opens the files instead.
///
/// Each is a plain [`vike_data::HistStore`] trait verb: a BUILD fact, advertised UNCONDITIONALLY
/// ([`FEATURE_COVERAGE`]'s shape), negotiated per decision 0112. ⚠ **Six strings, not one**,
/// although they shipped together: one string per verb is this protocol's convention, and it
/// survives a server that later serves a SUBSET — a family string would have to be re-defined to
/// mean less, and a capability whose meaning changes under a client is worse than one that is
/// absent.
pub const FEATURE_SCAN_BOOK_UPDATES: &str = "scan_book_updates";

/// Gates the CONFLATING depth lane ([`Request::ScanDepth`](super::Request::ScanDepth)). Family:
/// [`FEATURE_SCAN_BOOK_UPDATES`].
pub const FEATURE_SCAN_DEPTH: &str = "scan_depth";

/// Gates the cohort read ([`Request::ScanCohort`](super::Request::ScanCohort)). Family:
/// [`FEATURE_SCAN_BOOK_UPDATES`].
pub const FEATURE_SCAN_COHORT: &str = "scan_cohort";

/// Gates the perp-metrics read ([`Request::ScanPerpMetrics`](super::Request::ScanPerpMetrics)).
/// Family: [`FEATURE_SCAN_BOOK_UPDATES`].
pub const FEATURE_SCAN_PERP_METRICS: &str = "scan_perp_metrics";

/// Gates the equity-curve read ([`Request::ScanEquity`](super::Request::ScanEquity)). Family:
/// [`FEATURE_SCAN_BOOK_UPDATES`].
pub const FEATURE_SCAN_EQUITY: &str = "scan_equity";

/// Gates the Tier-2 exec-fill read ([`Request::ScanExecFills`](super::Request::ScanExecFills)).
/// Family: [`FEATURE_SCAN_BOOK_UPDATES`].
pub const FEATURE_SCAN_EXEC_FILLS: &str = "scan_exec_fills";

/// Gates the ROW CAP on a range scan — the `limit` field on
/// [`Request::LoadBars`](super::Request::LoadBars),
/// [`Request::ScanQuotes`](super::Request::ScanQuotes),
/// [`Request::ScanTrades`](super::Request::ScanTrades) and the five ranged members of 0084's
/// family. These verbs answer in ONE frame, so without a cap a range holding more than
/// [`MAX_FRAME_LEN`](super::MAX_FRAME_LEN) (64 MiB) of rows is a request the server CANNOT answer —
/// measured against a Polymarket book group of ~37.5 M rows, the read
/// `crates/vike-backtest/src/bin/cheap_np_depth.rs` performs with `TsRange::all()`.
///
/// ⚠ **THE CAP IS SOFT, AND THAT IS THE CORRECTNESS PROPERTY RATHER THAN A CONVENIENCE.** A server
/// honouring it returns whole `ts` GROUPS: it stops at the last complete timestamp at or before the
/// cap, so a page may carry fewer rows than asked, or the full group that straddles it. A paging
/// client continues from `last_ts + 1` (there is no cursor), so a page cut mid-`ts` would leave the
/// rest of that timestamp's rows behind its continuation bound — dropped SILENTLY, a short answer
/// that reads exactly like the truth. Every row family sorts by `ts` first, and `scan_book_updates`
/// REGROUPS on `(ts, seq)`, so a mid-`ts` cut can also split one logical event into two.
///
/// ⚠ **No cursor, deliberately: that is what keeps the change additive.** Every reply is a TUPLE
/// variant (`{"Trades":[…]}`); a `next` beside the rows would make it a STRUCT variant, a shape
/// every older peer fails to decode. The whole cost is one trailing request per scan whose final
/// page happened to be exactly full.
///
/// In the SERVER's memory every range verb pushes the limit into the store: `LoadBars` through
/// `HistStore::load_bars_head`
/// (`docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`), the seven scans through
/// their `_capped` store verbs
/// (`docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`). A request with NO
/// limit reads one row past its kind's ceiling and is refused BY NAME above it
/// (`vike_datahub::server`'s `ReadCeilings`).
pub const FEATURE_SCAN_LIMIT: &str = "scan_limit";

/// Gates [`Request::SeriesFacts`](super::Request::SeriesFacts) — one series' coverage plus the
/// commit keys that produced it.
///
/// ⚠ **The SEVENTH verb of 0084's gap, which that record did not price**: it was INHERENT to
/// `DataFusionHist`, not on the `HistStore` trait 0084 measured. Without it a wire-routed run's
/// reproducibility record (`crates/vike-backtest/src/backtest_cli/run_record.rs`'s
/// `collect_data_fingerprint`) is written as `null` and still reads GREEN.
///
/// A BUILD fact, advertised unconditionally ([`FEATURE_COVERAGE`]'s shape).
pub const FEATURE_SERIES_FACTS: &str = "series_facts";

/// Advertised when the server is KEYED — it holds at least one
/// [`vike_node_proto::auth::NodeKeys`] scope key and therefore REQUIRES a
/// [`Request::Auth`](super::Request::Auth) before it answers any other verb
/// (`docs/decisions/0025-datahub-remote-posture.md`). Negotiated per decision 0112, and
/// backward-compatible both ways:
///
/// - **Key-LESS server (the default).** No string, and `Welcome.nonce` is `None`, so its handshake
///   bytes are IDENTICAL to the pre-auth protocol and every existing client works unchanged.
///   Turning auth ON is writing two keys into the credential store; there is no other switch.
/// - **Keyed server, old client.** The client ignores the unknown string and the `nonce` field,
///   sends a normal verb, and gets a clean refusal naming what is missing.
/// - **Keyed server, new client.** [`crate::DatahubClient::connect_authed`] signs the nonce; the
///   plain [`crate::DatahubClient::connect`] fails at the handshake with an actionable message
///   instead of at the first verb with a confusing one.
pub const FEATURE_AUTH: &str = "auth";

/// Gates the DESTRUCTIVE store verb ([`Request::DeleteSeries`](super::Request::DeleteSeries) /
/// [`Response::Deleted`](super::Response::Deleted)); negotiated per decision 0112.
///
/// ⚠ **Advertised ONLY by a KEYED server — a POSTURE decision, not a capability one.** Every other
/// string here answers "is this server new enough / built with it"; this one answers "does this
/// server have any way to say no". `docs/decisions/0025-datahub-remote-posture.md`: *"A surface
/// with a write verb and no way to say 'reads yes, writes no' cannot be handed even a trusted
/// LAN."* A delete is that argument's sharper form (it destroys the only copy), and the default
/// datahub is KEY-LESS. So, each leg tested:
///
/// - a KEY-LESS server never advertises it and REFUSES the verb outright — no flag, no override,
///   no `--allow`;
/// - a KEYED server advertises it, and `required_scope` puts it in the `Scope::Write` scope, so a
///   read-scope connection cannot reach it either;
/// - the client refuses LOCALLY when it is absent, so an operator learns "that server has no keys"
///   rather than "unknown request".
pub const FEATURE_DELETE_SERIES: &str = "delete_series";

/// Gates the ARCHIVE IMPORT verb ([`Request::ImportArchive`](super::Request::ImportArchive) /
/// [`Response::ArchiveImported`](super::Response::ArchiveImported) —
/// `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §2.5); negotiated per
/// decision 0112.
///
/// A RUNTIME fact: advertised ONLY when an import lane is MOUNTED — a datahub with no project
/// directory above it has no imports root and advertises nothing, however it was built. The client
/// checks this string AND the named format's [`import_format_feature`] entry, and refuses LOCALLY
/// when either is absent ([`crate::DatahubClient::import_archive`]).
///
/// ⚠ **This string says WHETHER the server imports; the per-format entries say WHAT.** A format the
/// server does not register is refused BY NAME before a frame is written — which matters more here
/// than anywhere, because this verb steers server-side filesystem reads.
pub const FEATURE_ARCHIVE_IMPORT: &str = "archive_import";

/// The prefix of a per-format ARCHIVE IMPORT entry — `import_format=dukascopy-bi5` — one per format
/// the server's MOUNTED import registry holds, beside [`FEATURE_ARCHIVE_IMPORT`]. Written and read
/// by a PAIR ([`import_format_feature`] / [`advertised_import_formats`]), the
/// [`FEATURE_MD_VENUE_PREFIX`] shape, so the two ends cannot drift on the spelling.
///
/// ⚠ **An empty list beside an advertised [`FEATURE_ARCHIVE_IMPORT`] is a server that imports
/// nothing**, and the client refuses every format: the two shipped together, so — unlike
/// `rec_venue=` — absence here is unambiguous and the refusal is safe.
pub const FEATURE_IMPORT_FORMAT_PREFIX: &str = "import_format=";

/// Build the `import_format=<id>` entry — the WRITE half of [`advertised_import_formats`].
///
/// ⚠ A pure `format!`: which formats a server advertises is decided by the SERVER's import
/// registry; this crate names no format and must not grow a list of them.
pub fn import_format_feature(id: &str) -> String {
    format!("{FEATURE_IMPORT_FORMAT_PREFIX}{id}")
}

/// Every archive format id a server advertised, in ADVERTISEMENT ORDER — the READ half of
/// [`import_format_feature`]. Values are trimmed and an empty one is dropped (it advertises
/// nothing). It reads ONE prefix, so it never sees an `md_venue=` or a `rec_venue=` entry.
pub fn advertised_import_formats(features: &[String]) -> Vec<String> {
    features
        .iter()
        .filter_map(|f| f.strip_prefix(FEATURE_IMPORT_FORMAT_PREFIX))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// Gates the compiled STUDY runner ([`Request::RunStudy`](super::Request::RunStudy) /
/// [`Response::StudyReport`](super::Response::StudyReport)) — `vike-backend study`'s wire half and
/// what `vike-cli research study` negotiates on (ruling 16 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`); negotiated per
/// decision 0112.
///
/// ⚠ **The COMPUTE daemon (`vike-backend backtest --addr`) advertises it, never the data server**:
/// a study runs an engine, and ruling 7 of that spec puts every such verb on the compute plane. It
/// is declared HERE because every `FEATURE_*` a client of this protocol negotiates on lives here —
/// one spelling, below whichever daemon serves it.
///
/// ⚠ **A MOUNT fact, unlike its neighbour [`FEATURE_SEARCH_METHOD`].**
/// `vike_backtest::compute_server`'s `served_features` pushes it ONLY when a `StudyRunFn` was
/// handed in: the runner lives in `vike-studio-core`, ABOVE `vike-backtest`, so only a composition
/// root that sees both (`crates/vike/src/main.rs`) can inject one. `vike-backend backtest --addr`
/// therefore serves a study and a bare `cargo run -p vike-backtest --bin backtest` refuses it BY
/// NAME.
///
/// The client refuses BY NAME with nothing sent when it is absent
/// ([`crate::DatahubClient::run_study`]); `crates/vike-cli/src/cmd/study.rs` prints the richer
/// refusal with the `vike-backend study` escape hatch, and
/// `crates/vike-cli/tests/study_report_refusal_cli.rs` drives it against the data daemon.
pub const FEATURE_STUDY: &str = "study";

/// Gates a parameter search that names its own SEARCH METHOD —
/// [`Request::RunParamscanProfile`](super::Request::RunParamscanProfile)'s `search` field and the
/// `rank_by` value `"multi"`. An optional field an old server would silently ignore, so it has its
/// own string (decision 0112, verdict 3).
///
/// ⚠ **The field alone is the DEFECT.** [`Request`](super::Request) has no `deny_unknown_fields`,
/// so a daemon predating `search` decodes the frame, drops it, runs the exhaustive grid and answers
/// a normal [`Response::ParamscanReport`](super::Response::ParamscanReport). The client cannot tell
/// a Bayesian search from a grid by looking at one — the silent downgrade #1750 ended when it
/// retired `--search`. So:
///
/// - the compute daemon advertises it UNCONDITIONALLY — a BUILD fact, because
///   `vike_backtest::compute_server` is a DEFAULT-build module and every build that links the crate
///   has the arm. ⚠ Contrast [`FEATURE_STUDY`], which the SAME `served_features` advertises
///   CONDITIONALLY, because its runner is injected from a crate above that daemon;
/// - the client refuses LOCALLY, WITHOUT SENDING, when the request needs it
///   ([`WireSearch::needs_capability`](super::WireSearch::needs_capability));
/// - a server predating the FIELD still decodes the frame and keeps the connection, so the refusal
///   is a client-side courtesy rather than the only guard against a desync.
pub const FEATURE_SEARCH_METHOD: &str = "search_method";

/// Gates the STUDIO walk-forward's per-window SEARCH — [`crate::wire_studio::WireWalkforward`]'s
/// `search` field. Same defect, same three legs as [`FEATURE_SEARCH_METHOD`], different verb: a
/// daemon predating the field runs the FIXED walk and answers a normal
/// [`Response::WalkforwardResult`](super::Response::WalkforwardResult) — not a degraded answer but
/// an answer to a DIFFERENT question (were these parameters stable out of sample, rather than does
/// fit-then-trade survive out of sample). So:
///
/// - the compute daemon advertises it **only when the STUDIO runners are MOUNTED** — a MOUNT fact
///   like [`FEATURE_STUDY`], unlike [`FEATURE_SEARCH_METHOD`]: it rides `Request::RunWalkforward`,
///   which `vike_backtest::compute_server` serves only through a table injected from
///   `vike-studio-core` ABOVE it;
/// - the client refuses LOCALLY, WITHOUT SENDING, when the request needs it
///   ([`crate::wire_studio::WireWindowSearch::needs_capability`]);
/// - the SERVER re-checks: `vike_studio_core::wire_run`'s `run_walkforward_local` parses the method
///   itself and refuses an unknown one BY NAME rather than falling through to the fixed walk.
pub const FEATURE_WALKFORWARD_SEARCH: &str = "walkforward_search";

/// The parameter-search METHODS this protocol can carry, and the ONE roster four surfaces read.
///
/// ⚠ **It lives in the PROTOCOL crate and the ENGINE reads it, which looks inverted and is not.**
/// This crate is the only one BOTH `vike-cli` (which spelling-checks `--optimizer` before a dial or
/// a spawn) and `vike-backtest` (which implements the methods) take as a normal dependency — the
/// shared crate BELOW two sides that must not disagree; `crates/vike-cli/Cargo.toml` carries "no
/// concrete backend, no vike-backtest, no engine crates".
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §5.4 records the three
/// rosters that disagreed before it, and §15.1 requires the gate that now holds it closed.
pub const SEARCH_METHODS: [&str; 4] = ["grid", "euler", "tpe", "genetic"];

/// The method a search runs when none is named: the exhaustive product. The first row of
/// [`SEARCH_METHODS`], and `vike_backtest::harness::sweep::GridSearch`'s own `Optimizer::name`.
pub const DEFAULT_SEARCH_METHOD: &str = "grid";

/// Gates the MARKET-DATA push lane ([`Request::MdSubscribe`](super::Request::MdSubscribe) /
/// [`Request::MdUpdate`](super::Request::MdUpdate) / [`Response::Md`](super::Response::Md) — the
/// datahub market-data wire design, §4.1); negotiated per decision 0112.
///
/// A RUNTIME fact: advertised ONLY when an `MdHub` is MOUNTED AND ARMED — a build carrying the
/// plane with `VIKE_DATAHUB_LIVE` unset advertises nothing. The client refuses locally without it,
/// so a desktop shows an honest note instead of a socket that stalls.
///
/// ⚠ The old-server leg carries more weight here than on any other verb: a
/// [`Response::Error`](super::Response::Error) answer is what tells the client its connection is
/// STILL POSITIONAL and it must not start a reader thread. See [`crate::market`]'s module doc for
/// the mode-switch invariant.
pub const FEATURE_MARKET_DATA: &str = "market_data";

/// The prefix of a per-venue market-data entry — `md_venue=binance` — one per venue the SERVER's
/// build links a market-data client for, so a client learns at the HANDSHAKE which venues it may
/// name in an [`crate::market::MdSpec`] rather than one
/// [`crate::market::MdRefusal::VenueNotServed`] at a time.
///
/// Written and read by a PAIR ([`md_venue_feature`] / [`advertised_md_venues`]) so the two ends
/// cannot drift on the spelling — the precedent is
/// `crates/vike-tradehub-client/src/proto.rs`'s `FEATURE_DATAHUB_PREFIX` / `datahub_feature` /
/// `advertised_datahub`, whose `datahub=<addr>` is spelled exactly this way.
///
/// ⚠ COLLISION-SAFE by construction: every capability check in this protocol family is whole-string
/// equality, so an `md_venue=binance` entry can neither satisfy nor shadow a named capability.
pub const FEATURE_MD_VENUE_PREFIX: &str = "md_venue=";

/// Build the `md_venue=<slug>` entry — the WRITE half of [`advertised_md_venues`].
///
/// ⚠ A pure `format!`: the cfg that decides WHICH venues a build advertises is the server's
/// (`crates/vike-datahub/src/md/venues.rs`'s `supported`); this crate declares no feature at all
/// and must not grow one.
pub fn md_venue_feature(slug: &str) -> String {
    format!("{FEATURE_MD_VENUE_PREFIX}{slug}")
}

/// Every venue slug a server advertised, in ADVERTISEMENT ORDER — the READ half of
/// [`md_venue_feature`]. A `Vec` where its precedent `advertised_datahub` returns an `Option`:
/// `datahub=` is ONE entry, `md_venue=` one PER VENUE. Each value is trimmed and an EMPTY value is
/// dropped — an empty advertisement advertises nothing.
pub fn advertised_md_venues(features: &[String]) -> Vec<String> {
    features
        .iter()
        .filter_map(|f| f.strip_prefix(FEATURE_MD_VENUE_PREFIX))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// The prefix of a per-venue RECORDING entry — `rec_venue=binance`: which venues this build can
/// RECORD (`crates/vike-datahub/src/recording.rs`'s `supported`), where [`FEATURE_MD_VENUE_PREFIX`]
/// says which it can SERVE LIVE (`crates/vike-datahub/src/md/venues.rs`'s `supported`). The two
/// sets genuinely differ — the shipped image serves six venues and records two — so a client that
/// read one for the other would refuse a venue it could record, or accept one it could not.
///
/// It exists because `subscription.venue` carries no CHECK constraint (the recordable set is a
/// per-BUILD fact): a `vike-cli data realtime record add okx:…` row that
/// `crates/vike-datahub/src/recorder/profile.rs`'s `load_and_check_profile_row` REFUSES at the
/// daemon's next start takes the data wire down under `Restart=on-failure`. The advertisement lets
/// a client refuse the ROW at the edit, with the operator watching.
///
/// ⚠ **An EMPTY advertisement is not a refusal.** No `rec_venue=` entry means either a server older
/// than this advertisement or a build without the recording plane (the `record` Cargo feature off);
/// a client cannot tell them apart and must not try. It is the same answer as an unreachable server
/// (`docs/decisions/0013-degrade-vs-refuse.md`): WARN and proceed. Refuse only when the server
/// advertised AT LEAST ONE venue and the named one is not among them.
pub const FEATURE_REC_VENUE_PREFIX: &str = "rec_venue=";

/// Build the `rec_venue=<slug>` entry — the WRITE half of [`advertised_rec_venues`].
///
/// ⚠ A pure `format!`, like [`md_venue_feature`]: the cfg that decides WHICH venues a build
/// advertises is the server's (`crates/vike-datahub/src/recording.rs`'s `supported`, reached from
/// `crates/vike-datahub/src/server/features.rs`'s `served_features`).
pub fn rec_venue_feature(slug: &str) -> String {
    format!("{FEATURE_REC_VENUE_PREFIX}{slug}")
}

/// Every RECORDABLE venue slug a server advertised, in ADVERTISEMENT ORDER — the READ half of
/// [`rec_venue_feature`], trimmed with empty values dropped like [`advertised_md_venues`]. It reads
/// ONE prefix, deliberately: a reader that also accepted `md_venue=` would report a venue as
/// recordable because it happens to be servable.
///
/// ⚠ An EMPTY result is NOT "this server records nothing" — read [`FEATURE_REC_VENUE_PREFIX`]'s
/// degrade rule before acting on this answer.
pub fn advertised_rec_venues(features: &[String]) -> Vec<String> {
    features
        .iter()
        .filter_map(|f| f.strip_prefix(FEATURE_REC_VENUE_PREFIX))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}
