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
//! (`market_data_venue=`, `rec_venue=`, `import_format=`) can neither satisfy nor shadow a named capability.
//!
//! The words this wire shares with another hub's (`wire_msgpack`, `gateway_status`, `market_data_venue=`) and the
//! reader of every `prefix=value` entry live one crate down, in `vike_hub_protocol::features`, and are named there.

use vike_hub_protocol::features::{value_after, values_after};

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

/// Gates the OPTION-TRADES lane of [`Request::Backfill`](super::Request::Backfill): an `interval` of
/// `vike_data::source::OPTION_TRADES_INTERVAL` (`"option-trades"`) fetches an options venue's public trade log for
/// the `symbol`'s CURRENCY and derives the hourly option chain from it, not bars. Advertised only when the
/// server's table mounts an option-trades source. The client refuses locally without it, for
/// [`FEATURE_BACKFILL_FUNDING`]'s reason: an older server refuses the label as a bar step it cannot measure.
pub const FEATURE_BACKFILL_OPTION_TRADES: &str = "backfill_option_trades";

/// Gates the MARKET-RULES lane of [`Request::Backfill`](super::Request::Backfill): an `interval` of
/// `vike_data::source::RULES_INTERVAL` (`"rules"`) fetches a prediction-market venue's archived rules texts
/// (each closed market's question, rules, resolution source and final outcome) into the `kind=text` series
/// the `symbol` names as its collector, not bars. Advertised only when the server's table mounts a
/// market-rules source. The client refuses locally without it, for [`FEATURE_BACKFILL_FUNDING`]'s reason.
pub const FEATURE_BACKFILL_MARKET_RULES: &str = "backfill_market_rules";

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
/// arm the `flags.datahub_chart_seed` row advertises nothing, so a client can name the switch that
/// is off instead of drawing an empty chart and blaming the venue.
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

/// Gates the OPTIONS-CHAIN verb ([`Request::OptionsChain`](super::Request::OptionsChain) /
/// [`Response::OptionsChain`](super::Response::OptionsChain) — one underlying's chain snapshot,
/// folded by the data daemon from the venue's public chain REST and option feeds); negotiated per
/// decision 0112.
///
/// ⚠ **A BUILD fact, advertised UNCONDITIONALLY — deliberately NOT [`FEATURE_VENUE_CATALOG`]'s
/// per-armed-lane rule.** The lane has no runtime switch: it is armed exactly when the daemon was
/// built with `options-serve`, and an unarmed build still DECODES and ANSWERS the verb with
/// [`crate::options::OptionsChainOutcome::NotArmed`], whose sentence names the rebuild. Gating the
/// string on the lane would make an unarmed modern daemon indistinguishable from an OLD one —
/// `vike_backtesthub_client::proto::FEATURE_NAMED_RUN`'s argument. So: an old server advertises nothing and the client refuses
/// LOCALLY, sending nothing ([`crate::DatahubClient::options_chain`]); an unarmed one advertises and
/// answers `NotArmed`; an armed one answers the snapshot.
pub const FEATURE_OPTIONS_CHAIN: &str = "options_chain";

/// Gates the OPTIONS-LISTING REFRESH ([`Request::OptionsRefresh`](super::Request::OptionsRefresh) /
/// [`Response::OptionsRefreshed`](super::Response::OptionsRefreshed)): read the venue's option
/// listing again now. A BUILD fact advertised unconditionally, [`FEATURE_OPTIONS_CHAIN`]'s rule: a
/// build without the options lane answers the request with an error naming the rebuild, and an older
/// server advertises nothing, so a client refuses LOCALLY ([`crate::DatahubClient::options_refresh`]).
pub const FEATURE_OPTIONS_REFRESH: &str = "options_refresh";

/// Gates the THREE TOOL-FEED verbs ([`Request::NewsFeed`](super::Request::NewsFeed),
/// [`Request::EconomicCalendar`](super::Request::EconomicCalendar) and
/// [`Request::EquityCalendar`](super::Request::EquityCalendar), with their
/// [`Response`](super::Response) twins — the desktop's News, Calendar, Earnings, Dividends and IPO
/// windows, polled by the data daemon); negotiated per decision 0112. ONE string for all three: they
/// are one lane, armed together.
///
/// ⚠ **A BUILD fact, advertised UNCONDITIONALLY — [`FEATURE_OPTIONS_CHAIN`]'s rule, for its
/// reason.** The lane has no runtime switch: it is armed exactly when the daemon was built with
/// `tool-feeds-serve`, and an unarmed build still DECODES and ANSWERS each verb with
/// [`crate::tool_feeds::DatahubPollOutcome::NotArmed`], whose sentence names the rebuild. Gating the
/// string on the lane would make an unarmed modern daemon indistinguishable from an OLD one. So: an
/// old server advertises nothing and the client refuses LOCALLY, sending nothing, BY NAME
/// ([`crate::DatahubClient::news_feed`]); an unarmed one advertises and answers `NotArmed`; an
/// armed one answers the snapshot. A keyed feed whose key the server lacks is a THIRD, different
/// fact (`NotConfigured`), not a reason to withhold this string.
pub const FEATURE_TOOL_FEEDS: &str = "tool_feeds";

/// Gates the cross-kind coverage verb ([`Request::Coverage`](super::Request::Coverage) /
/// [`Response::Coverage`](super::Response::Coverage) — split-plane spec §6 Q2, the Data Manager's
/// "Partial" column over the wire); negotiated per decision 0112.
///
/// A BUILD fact, advertised UNCONDITIONALLY by every server built from this crate's `serve`
/// (`served_features`): coverage is a plain [`vike_data::HistoryStore`] trait verb with no table to
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
/// travels INSIDE the answer ([`crate::history::HistoryChannelWire::mounted`]), so an unmounted server
/// is never mistaken for an old one. Without it the client refuses locally
/// ([`crate::DatahubClient::history_channels`]) and a caller renders its OWN compiled table under
/// [`crate::history::COMPILED_TABLE_CAPTION`] instead ([`crate::history::compiled_report`]).
pub const FEATURE_HISTORY_CHANNELS: &str = "history_channels";

/// Gates the venue sessions' history ([`Request::GatewayLinks`](super::Request::GatewayLinks) /
/// [`Response::GatewayLinks`](super::Response::GatewayLinks)); negotiated per decision 0112. A BUILD fact, advertised
/// unconditionally: an empty history is an answer, and a datahub without the string is one too old to ask.
pub const FEATURE_GATEWAY_LINKS: &str = "gateway_links";

/// Gates the relay of an operator's credential change to the gatewayhub
/// ([`Request::CredentialsChanged`](super::Request::CredentialsChanged) /
/// [`Response::CredentialsChanged`](super::Response::CredentialsChanged), decision 0287); negotiated per decision
/// 0112. A BUILD fact, advertised unconditionally
/// ([`FEATURE_GATEWAY_STATUS`](vike_hub_protocol::features::FEATURE_GATEWAY_STATUS)'s shape): a gatewayhub that is down is
/// an answer inside the reply, and a datahub without the string is one too old to relay it.
pub const FEATURE_CREDENTIALS_CHANGED: &str = "credentials_changed";

/// Gates the user's named symbol lists ([`Request::SymbolSets`](super::Request::SymbolSets),
/// [`Request::SetSymbolSet`](super::Request::SetSymbolSet),
/// [`Request::DeleteSymbolSet`](super::Request::DeleteSymbolSet)); negotiated per decision 0112. A BUILD fact,
/// advertised unconditionally: a server with no settings store answers each verb with an error in words, and a
/// datahub without the string is one too old to ask, whose client shows the built-in lists read-only.
pub const FEATURE_SYMBOL_SETS: &str = "symbol_sets";

/// Gates the venues' kept instrument lists ([`Request::InstrumentLists`](super::Request::InstrumentLists) and
/// [`Request::RefreshInstrumentLists`](super::Request::RefreshInstrumentLists)); negotiated per decision 0112. A RUNTIME
/// fact: advertised by a datahub that runs in a project folder, where the lists live (an unarmed catalog lane there
/// answers each venue's refresh with `NotArmed`). A datahub without the string keeps no list (no project, or too old
/// to ask), and a reader of its names reads them as typed.
pub const FEATURE_INSTRUMENT_LISTS: &str = "instrument_lists";

/// Gates [`Request::InstrumentListsOfVenues`](super::Request::InstrumentListsOfVenues): the kept lists of the venues a
/// reader names, not every venue's (a backtest reads a few of ~24k rows); negotiated per decision 0112, a NEW verb.
/// Advertised exactly when [`FEATURE_INSTRUMENT_LISTS`] is (the same lists, the same runtime fact). A datahub without
/// it is asked the whole lists and the client keeps the named venues' (`DatahubClient::instrument_lists_of_venues`).
pub const FEATURE_INSTRUMENT_LISTS_OF_VENUES: &str = "instrument_lists_of_venues";

/// Gates [`Request::KeptRowsOfVenues`](super::Request::KeptRowsOfVenues): the kept rows and share splits of the named
/// venues in BINARY with a version stamp per venue, answered "unchanged" for a venue whose stamp the reader holds (the
/// owner's "yes both" of 2026-11-19; `docs/decisions/0107-the-node-protocol-substrate-is-a-crate-below-both-clients.md`'s
/// amendment of that day); negotiated per decision 0112, a NEW verb. Advertised exactly when
/// [`FEATURE_INSTRUMENT_LISTS`] is (the same lists, the same runtime fact). A datahub without it is asked
/// [`FEATURE_INSTRUMENT_LISTS_OF_VENUES`]' JSON answer, as before (`crate::HistoryStoreDatahub`'s
/// `kept_instruments_and_splits_of`).
pub const FEATURE_KEPT_ROWS_STAMPED: &str = "kept_rows_stamped";

/// Gates the instruments the operator adds by hand ([`Request::OperatorInstruments`](super::Request::OperatorInstruments));
/// negotiated per decision 0112. A BUILD fact, advertised unconditionally: a server with no settings store or no
/// project answers in words, and a datahub without the string is one too old to ask.
pub const FEATURE_OPERATOR_INSTRUMENTS: &str = "operator_instruments";

/// Gates the open split warnings ([`Request::SplitWarnings`](super::Request::SplitWarnings)); negotiated per decision
/// 0112. A BUILD fact, advertised unconditionally: a datahub that stores no share series answers an empty list, and
/// a datahub without the string is one too old to ask, whose client shows no warning.
pub const FEATURE_SPLIT_WARNINGS: &str = "split_warnings";

/// Gates the live tokens of the rolling families the datahub's recorder records
/// ([`Request::LiveFamilyTokens`](super::Request::LiveFamilyTokens)); negotiated per decision 0112. A BUILD fact,
/// advertised unconditionally: a datahub that runs no recorder, or has resolved no family yet, says so inside the
/// answer, and a datahub without the string is one too old to ask.
pub const FEATURE_LIVE_FAMILY_TOKENS: &str = "live_family_tokens";

/// Gates the raw L2 book-update read ([`Request::ScanBookUpdates`](super::Request::ScanBookUpdates)
/// / [`Response::BookUpdates`](super::Response::BookUpdates)), and documents the whole SIX-verb
/// family `docs/decisions/0084-only-the-datahub-touches-the-store.md` names:
/// [`FEATURE_SCAN_DEPTH`], [`FEATURE_SCAN_COHORT`], [`FEATURE_SCAN_PERP_METRICS`],
/// [`FEATURE_SCAN_EQUITY`] and [`FEATURE_SCAN_EXECUTION_FILLS`] point here.
///
/// ⚠ **These six exist because the wire NOT serving them kept four crates opening the store
/// directly.** 0084's verdict is one store reader, everyone else asking over the wire; a verb
/// nobody can ask for over the wire is a verb whose consumer opens the files instead.
///
/// Each is a plain [`vike_data::HistoryStore`] trait verb: a BUILD fact, advertised UNCONDITIONALLY
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

/// Gates the text-history read ([`Request::ScanTexts`](super::Request::ScanTexts)), the `kind=text`
/// series the datahub's text recorder fills. A BUILD fact, advertised unconditionally like
/// [`FEATURE_SCAN_BOOK_UPDATES`]' family (a store trait verb with no table to mount); a datahub
/// without it predates the verb, and a client asks it nothing (the read refuses locally, naming it).
pub const FEATURE_SCAN_TEXTS: &str = "scan_texts";

/// Gates the option-chain read ([`Request::ScanChain`](super::Request::ScanChain)), the `kind=chain`
/// snapshots a study reads point-in-time. Family: [`FEATURE_SCAN_BOOK_UPDATES`], whose argument it
/// joined later: before it, `crates/vike-datahub-client/src/remote.rs`'s `HistoryStoreDatahub`
/// refused every chain read, so a chain was readable by no process but the datahub.
pub const FEATURE_SCAN_CHAIN: &str = "scan_chain";

/// Gates the two SEC table reads ([`Request::ScanFinancialFacts`](super::Request::ScanFinancialFacts),
/// [`Request::ScanInstitutionalHoldings`](super::Request::ScanInstitutionalHoldings)), the
/// `kind=financial_fact` and `kind=institutional_holding` series the SEC import fills. One BUILD fact for
/// the pair (they were added together), advertised unconditionally like [`FEATURE_SCAN_TEXTS`]; a
/// datahub without it predates both verbs, and a client asks it nothing.
pub const FEATURE_SCAN_SEC_TABLES: &str = "scan_sec_tables";

/// Gates the text-score read ([`Request::ScanTextScores`](super::Request::ScanTextScores)), the
/// `kind=text_score` series the `text-scores` import fills: [`FEATURE_SCAN_TEXTS`]' twin, the same
/// unconditional build fact.
pub const FEATURE_SCAN_TEXT_SCORES: &str = "scan_text_scores";

/// Gates the data plane's read of the live strategies' published forecasts
/// ([`Request::ScanForecasts`](super::Request::ScanForecasts)), the `kind=exec_forecast` series, on an
/// Observe key. A BUILD fact like [`FEATURE_SCAN_TEXTS`], and a different string from the lane's
/// [`FEATURE_FORECASTS`], which only a server with a strategy lane advertises. A datahub without it
/// predates the verb, and a client asks it nothing (the read refuses locally, naming it).
pub const FEATURE_SCAN_FORECASTS: &str = "scan_forecasts";

/// Gates the equity-curve read ([`Request::ScanEquity`](super::Request::ScanEquity)). Family:
/// [`FEATURE_SCAN_BOOK_UPDATES`].
pub const FEATURE_SCAN_EQUITY: &str = "scan_equity";

/// Gates the Tier-2 exec-fill read ([`Request::ScanExecFills`](super::Request::ScanExecFills)).
/// Family: [`FEATURE_SCAN_BOOK_UPDATES`].
pub const FEATURE_SCAN_EXECUTION_FILLS: &str = "scan_exec_fills";

/// Gates the optional `start`/`end`/`limit` on [`Request::ScanExecFills`](super::Request::ScanExecFills)
/// — the time window the GUI's execution history reads (Q1 of
/// `docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`, reopened by the
/// owner on 2026-10-11 for that reason rather than for a series nearing its ceiling).
///
/// ⚠ **The fields are ADDITIVE on an existing variant, so this string is load-bearing, not
/// informational** — decision 0112's third leg. An older server DECODES a frame carrying them
/// (serde skips unknown fields) and answers the WHOLE series, the bound silently ignored; a client
/// that sent a window without checking would render "the last 7 days" over rows from a year ago. A
/// client without this string sends the old frame — byte-identical, the three fields are skipped
/// when `None` — and filters on its side. A BUILD fact, advertised unconditionally; the `limit` is
/// the soft whole-`ts` cap [`FEATURE_SCAN_LIMIT`] defines.
pub const FEATURE_SCAN_EXECUTION_FILLS_RANGE: &str = "scan_exec_fills_range";

/// Gates the order-lifecycle read ([`Request::ScanExecOrders`](super::Request::ScanExecOrders)),
/// `kind=execution_order` over a time window with the soft [`FEATURE_SCAN_LIMIT`] cap. A NEW verb, so
/// the three legs of decision 0112: a client without this string sends nothing and refuses
/// locally, a server that does not know the verb answers `Response::Error` and keeps the
/// connection, and a server that serves it says so here. A BUILD fact, advertised unconditionally.
pub const FEATURE_SCAN_EXECUTION_ORDERS: &str = "scan_exec_orders";

/// Gates the execution-history CATALOG of one window ([`Request::ExecutionSeries`](super::Request::ExecutionSeries)):
/// the `kind=exec_fill` / `kind=exec_order` series a time window reaches, the Tearsheet's first read, a few KB where
/// the whole [`Request::Inventory`](super::Request::Inventory) it replaces was 212 KB on the CI box (2026-11-17). A NEW
/// verb, decision 0112's three legs; a BUILD fact, advertised unconditionally. Without it the client sends nothing
/// and `HistoryStoreDatahub` folds the whole inventory on its own side.
pub const FEATURE_EXECUTION_SERIES: &str = "execution_series";

/// The RUNTIME fact "this datahub records the live executions a tradehub streams", so live executions
/// are being written into `kind=exec_fill` / `kind=execution_order` at all: advertised when the
/// [`FEATURE_EXECUTION_STREAM`] verbs are served with a writable store.
///
/// ⚠ **The word `exec_materialize` is a stored wire word and keeps its spelling**: it named the retired
/// journal materializer, and an older desktop reads it. A `Welcome.features` entry changes no schema
/// (decision 0112, verdict 4). Without it a reader cannot tell "nothing new is recorded" from "nothing
/// was traded": both are an empty or stale history. Its ABSENCE is ambiguous by construction (an older
/// build, or a store that refuses writes): a client says "this datahub records no live executions" and
/// still shows whatever rows the store holds, never refuses.
pub const FEATURE_EXECUTIONS_RECORDED: &str = "exec_materialize";

/// Gates the EXECUTION STREAM ([`Request::ExecutionStreamOpen`](super::Request::ExecutionStreamOpen) /
/// [`Response::ExecutionStreamOpened`](super::Response::ExecutionStreamOpened), then the frames of
/// [`crate::execution_stream`], and the status read
/// [`Request::ExecutionStreams`](super::Request::ExecutionStreams)); negotiated per decision 0112.
///
/// A BUILD fact, advertised by every build whose store opens for writing. An older datahub cannot decode
/// either verb, so the client refuses LOCALLY, in words and sending nothing, a datahub without it
/// ([`crate::DatahubClient::open_execution_stream`], [`crate::DatahubClient::execution_streams`]): a
/// publisher facing one sends no record, and the journal keeps them under its floor.
pub const FEATURE_EXECUTION_STREAM: &str = "execution_stream";

/// Gates the ROW CAP on a range scan — the `limit` field on
/// [`Request::LoadBars`](super::Request::LoadBars),
/// [`Request::ScanQuotes`](super::Request::ScanQuotes),
/// [`Request::ScanTrades`](super::Request::ScanTrades) and the five ranged members of 0084's
/// family. These verbs answer in ONE frame, so without a cap a range holding more than
/// [`MAX_FRAME_LEN`](vike_hub_protocol::frame::MAX_FRAME_LEN) (64 MiB) of rows is a request the server CANNOT answer —
/// measured against a Polymarket book group of ~37.5 M rows, the read
/// `crates/vike-backtesthub/src/bin/cheap_np_depth.rs` performs with `TimestampRange::all()`.
///
/// ⚠ **THE CAP IS SOFT, AND THAT IS THE CORRECTNESS PROPERTY RATHER THAN A CONVENIENCE.** A server
/// honouring it returns whole `ts` GROUPS: it stops at the last complete timestamp at or before the
/// cap, so a page may carry fewer rows than asked, or the full group that straddles it. A paging
/// client continues from `last_timestamp + 1` (there is no cursor), so a page cut mid-`ts` would leave the
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
/// `HistoryStore::load_bars_head`
/// (`docs/superpowers/specs/2026-10-01-loadbars-bounded-read-design.md`), the seven scans through
/// their `_capped` store verbs
/// (`docs/superpowers/specs/2026-10-02-remaining-whole-range-reads-design.md`). A request with NO
/// limit reads one row past its kind's ceiling and is refused BY NAME above it
/// (`vike_datahub::server`'s `ReadCeilings`).
pub const FEATURE_SCAN_LIMIT: &str = "scan_limit";

/// Gates NAMING a `source=` lane on this wire: [`Request::ScanQuotes`](super::Request::ScanQuotes)'s `source`
/// and `SeriesSelector`'s, under [`Request::DeleteSeries`](super::Request::DeleteSeries). A BUILD fact, advertised
/// by every build.
///
/// ⚠ **An older server decodes either frame and IGNORES the field (decision 0112, verdict 3), and what it does
/// instead is the danger:** the read answers the archive's rows as the lane's, and the delete DELETES THE
/// ARCHIVE. So the client refuses locally without this string and sends nothing. A request that names no lane
/// is sent to any server, since it asks for what it always asked for.
pub const FEATURE_SOURCE: &str = "source";

/// Gates NAMING a `source=` lane on [`Request::LoadBars`](super::Request::LoadBars). A BUILD fact,
/// advertised by every build.
///
/// ⚠ **Its own string, because [`FEATURE_SOURCE`] does not cover it.** A server from before this field
/// advertises that one, decodes the frame, ignores the lane and answers the venue's candles as the lane's
/// bars. So the client refuses locally without this string and sends nothing. A read that names no lane
/// goes to any server.
pub const FEATURE_BAR_SOURCE: &str = "bar_source";

/// Gates [`Request::LoadBars`](super::Request::LoadBars)' `columns`: the reply as one BINARY body
/// (`super::bar_columns`, [`super::BAR_COLUMNS_MAGIC`]) instead of `Response::Bars` JSON. A BUILD fact,
/// advertised by every build; decision 0107's amendment of 2026-11-18 is the ruling.
///
/// ⚠ **Its own string, because an older server decodes the frame and IGNORES the field** (decision 0112,
/// verdict 3) and answers JSON. A reader that sent it anyway would still read that reply, so the danger is
/// smaller than [`FEATURE_BAR_SOURCE`]'s; the client refuses locally all the same, so "asked for columns"
/// always means "the server said it answers them".
pub const FEATURE_BAR_COLUMNS: &str = "bar_columns";

/// Gates the tick-level scans' `columns` ([`Request::ScanQuotes`](super::Request::ScanQuotes),
/// `ScanTrades`, `ScanBookUpdates`, `ScanDepth`, `ScanPerpMetrics`): the reply as one BINARY body
/// (`super::bar_columns`, the bar reply's codec through each row's table) instead of JSON. A BUILD fact,
/// advertised by every build.
///
/// ⚠ **Its own string, because an older server decodes the frame and IGNORES the field** (decision 0112,
/// verdict 3) and answers JSON, and because [`FEATURE_BAR_COLUMNS`] says nothing about these verbs: a server
/// that answers bar columns may predate the tick ones. The client asks only a server that advertises it, so
/// "asked for columns" always means "the server said it answers them".
pub const FEATURE_TICK_COLUMNS: &str = "tick_columns";

/// Gates the `columns` field of every other row-shaped scan ([`Request::ScanCohort`](super::Request::ScanCohort),
/// `ScanTexts`, `ScanChain`, `ScanFinancialFacts`, `ScanInstitutionalHoldings`, `ScanTextScores`, `ScanEquity`,
/// `ScanExecFills`, `ScanExecOrders`, `ScanForecasts` and `StrategyForecasts`): the reply as one BINARY body
/// (`super::bar_columns`, the same codec through each row's table, its strings as run-coded text columns) instead of
/// JSON (the owner's "JSON only at the edges" of 2026-11-19). A BUILD fact, advertised by every build.
///
/// ⚠ **Its own string**, for [`FEATURE_TICK_COLUMNS`]' reason: an older server decodes the frame and IGNORES the
/// field, and a server that answers tick columns may predate these.
pub const FEATURE_SCAN_COLUMNS: &str = "scan_columns";

/// Gates [`Request::LoadBars`](super::Request::LoadBars)' `resample_to`: the bars of a range folded by the
/// datahub into a coarser interval (`vike_marketdata::resample`), so a chart zoomed out over years reads
/// a thousand candles instead of a million rows. A BUILD fact, advertised by every build.
///
/// ⚠ **Its own string, because an older server decodes the frame and IGNORES the field** (decision 0112,
/// verdict 3) and answers the range's STORED rows, which over a zoomed-out range is refused at the bar
/// ceiling or crosses as hundreds of megabytes. So the client refuses locally without it and sends nothing;
/// the desktop then reads as it did before the field (`vike_app_core::data::stored`).
pub const FEATURE_BAR_RESAMPLE: &str = "bar_resample";

/// Gates [`Request::SeriesFacts`](super::Request::SeriesFacts) — one series' coverage plus the
/// commit keys that produced it.
///
/// ⚠ **The SEVENTH verb of 0084's gap, which that record did not price**: it was INHERENT to
/// `HistoryStoreDataFusion`, not on the `HistoryStore` trait 0084 measured. Without it a wire-routed run's
/// reproducibility record (`crates/vike-backtesthub/src/backtest_cli/run_record.rs`'s
/// `collect_data_fingerprint`) is written as `null` and still reads GREEN.
///
/// A BUILD fact, advertised unconditionally ([`FEATURE_COVERAGE`]'s shape).
pub const FEATURE_SERIES_FACTS: &str = "series_facts";

/// Gates [`Request::SeriesDays`](super::Request::SeriesDays): the UTC days a bar read of a range reads, each
/// with a stamp of its stored parts (`HistoryStore::series_days`), so the backtesthub keeps the bars of past
/// days and reads a day again only when its parts changed. A NEW verb, decision 0112's three legs; a BUILD
/// fact, advertised unconditionally. Without it the client sends nothing and keeps nothing.
pub const FEATURE_SERIES_DAYS: &str = "series_days";

/// Gates a `kind` on [`Request::SeriesDays`](super::Request::SeriesDays): the server stamps a TICK read's days
/// (quotes, trades, book updates) over every layout that read takes rows from (`HistoryStore::series_days`), so the
/// backtesthub keeps the ticks of past days as it keeps their bars. An ADDITIVE field, decision 0112's three legs: an
/// older server would skip the unknown field and answer the BAR days, so a client sends a `kind` only where this is
/// advertised, and otherwise sends nothing and keeps no tick. A BUILD fact, advertised unconditionally.
pub const FEATURE_SERIES_DAYS_KINDS: &str = "series_days_kinds";

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

/// Gates the ARCHIVE DOWNLOAD verb ([`Request::FetchArchive`](super::Request::FetchArchive) /
/// [`Response::ArchiveFetched`](super::Response::ArchiveFetched), the owner's ruling of 2026-11-13).
/// Advertised beside [`FEATURE_ARCHIVE_IMPORT`] only by a server whose import lane MOUNTS a download:
/// a build that carries one and a `venue_url` row naming the vendor's bucket. Whether the AWS key is
/// stored is asked per request, so a key stored after start needs no restart. The client refuses
/// locally without this string ([`crate::DatahubClient::fetch_archive`]).
pub const FEATURE_ARCHIVE_FETCH: &str = "archive_fetch";

/// Gates [`ImportSpec::replace_holed`](crate::archive::ImportSpec::replace_holed), the import's option to
/// replace a holed day another producer holds (decision 0268, point 4). Advertised beside
/// [`FEATURE_ARCHIVE_IMPORT`] by a server that knows the field.
///
/// ⚠ **An older server decodes the frame and IGNORES the field (decision 0112, verdict 3)**, and runs a
/// plain import: the operator would believe days were replaced that were left as they were. So the client
/// refuses locally without this string and sends nothing. A request that does not ask is sent to any
/// server, and the field is left off its frame.
pub const FEATURE_IMPORT_REPLACE_HOLED: &str = "import_replace_holed";

/// Gates [`ImportTarget::columns`](crate::archive::ImportTarget::columns), a `csv-bars` file's column
/// mapping, and the reserved `user` venue (decision 0272). Advertised beside [`FEATURE_ARCHIVE_IMPORT`]
/// by a server that knows both.
///
/// ⚠ **An older server IGNORES the field (decision 0112, verdict 3)** and reads the file by its
/// header's names, so a mapped file would be read another way or refused for a column the user named.
/// The client refuses locally without this string and sends nothing.
pub const FEATURE_CSV_COLUMNS: &str = "import_csv_columns";

/// **A user's CSV sent from his PC into a `user-*` lane** (`Request::CsvUpload`, decision 0272).
/// Advertised only by a server whose import lane carries `csv-bars` AND whose operator armed it with
/// the `flags.datahub_csv_upload` row: the advertisement is the arming, so "can serve" and "armed"
/// cannot answer differently. A client refuses locally without it, naming the row.
pub const FEATURE_CSV_UPLOAD: &str = "csv_upload";

/// The prefix of a per-format ARCHIVE IMPORT entry — `import_format=dukascopy-bi5` — one per format
/// the server's MOUNTED import registry holds, beside [`FEATURE_ARCHIVE_IMPORT`]. Written and read
/// by a PAIR ([`import_format_feature`] / [`advertised_import_formats`]), the
/// [`FEATURE_MARKET_DATA_VENUE_PREFIX`](vike_hub_protocol::features::FEATURE_MARKET_DATA_VENUE_PREFIX) shape, so the two
/// ends cannot drift on the spelling.
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
/// nothing). It reads ONE prefix, so it never sees a `market_data_venue=` or a `rec_venue=` entry.
pub fn advertised_import_formats(features: &[String]) -> Vec<String> {
    values_after(features, FEATURE_IMPORT_FORMAT_PREFIX)
}

/// Gates the MARKET-DATA push lane ([`Request::MarketDataSubscribe`](super::Request::MarketDataSubscribe) /
/// [`Request::MarketDataUpdate`](super::Request::MarketDataUpdate) / [`Response::MarketData`](super::Response::MarketData) — the
/// datahub market-data wire design, §4.1); negotiated per decision 0112.
///
/// A RUNTIME fact: advertised ONLY when a `MarketDataHub` is MOUNTED AND ARMED — a build carrying the
/// plane with `flags.datahub_live` off advertises nothing. The client refuses locally without it,
/// so a desktop shows an honest note instead of a socket that stalls.
///
/// ⚠ The old-server leg carries more weight here than on any other verb: a
/// [`Response::Error`](super::Response::Error) answer is what tells the client its connection is
/// STILL POSITIONAL and it must not start a reader thread. See [`crate::market`]'s module doc for
/// the mode-switch invariant.
pub const FEATURE_MARKET_DATA: &str = "market_data";

/// Gates the market-data plane's QUOTE lane ([`vike_model::StreamLabel::Quotes`],
/// [`crate::market::MarketDataFrame::Quote`]); negotiated per decision 0112. Advertised with
/// [`FEATURE_MARKET_DATA`], by every daemon whose hub is mounted.
///
/// ⚠ **The client's local refusal is the whole leg 3 here.** A server older than the string cannot
/// decode a spec naming the lane, so it answers the WHOLE request with
/// [`Response::Error`](super::Response::Error): one quote spec would cost every other spec of that
/// subscribe. So `crate::client::DatahubClient::market_data_subscribe` and `market_data_update` refuse such a spec
/// before anything is sent.
pub const FEATURE_MARKET_DATA_LANE_QUOTES: &str = "market_data_lane_quotes";

/// The prefix of a per-venue RECORDING entry — `rec_venue=binance`: which venues this build can
/// RECORD (`crates/vike-datahub/src/recording.rs`'s `supported`), where
/// [`FEATURE_MARKET_DATA_VENUE_PREFIX`](vike_hub_protocol::features::FEATURE_MARKET_DATA_VENUE_PREFIX)
/// says which it can SERVE LIVE (`crates/vike-datahub/src/market_data/venues.rs`'s `supported`). The two
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
/// ⚠ A pure `format!`, like
/// [`market_data_venue_feature`](vike_hub_protocol::features::market_data_venue_feature): the cfg that decides WHICH venues a build
/// advertises is the server's (`crates/vike-datahub/src/recording.rs`'s `supported`, reached from
/// `crates/vike-datahub/src/server/features.rs`'s `served_features`).
pub fn rec_venue_feature(slug: &str) -> String {
    format!("{FEATURE_REC_VENUE_PREFIX}{slug}")
}

/// Every RECORDABLE venue slug a server advertised, in ADVERTISEMENT ORDER — the READ half of
/// [`rec_venue_feature`], trimmed with empty values dropped ([`values_after`]). It reads
/// ONE prefix, deliberately: a reader that also accepted `market_data_venue=` would report a venue as
/// recordable because it happens to be servable.
///
/// ⚠ An EMPTY result is NOT "this server records nothing" — read [`FEATURE_REC_VENUE_PREFIX`]'s
/// degrade rule before acting on this answer.
pub fn advertised_rec_venues(features: &[String]) -> Vec<String> {
    values_after(features, FEATURE_REC_VENUE_PREFIX)
}

/// The prefix of the datahub's BUILD entry — `build=<sha256>`, the sha256 of the program the datahub runs, hashed once
/// at its start (decision 0078's amendment of 2026-11-20, the owner's yes to stating it on the wire): what the
/// backtesthub keys an identical run's answer from a saved run with, so a datahub that changed its code answers none
/// of the runs it served before. ONE entry, read by [`advertised_build`] and written by [`build_feature`].
///
/// ⚠ **ABSENT is a datahub that states no build**: one older than the entry, or one still hashing itself in the first
/// moment after its start. A reader falls back to what it did before (the backtesthub: the advertised features), and
/// never refuses on it. Collision-safe like every value entry here: no named capability starts `build=`.
pub const FEATURE_BUILD_PREFIX: &str = "build=";

/// Build the `build=<sha256>` entry — the WRITE half of [`advertised_build`].
pub fn build_feature(build: &str) -> String {
    format!("{FEATURE_BUILD_PREFIX}{build}")
}

/// The build a datahub advertised ([`FEATURE_BUILD_PREFIX`]), trimmed; `None` when it states none or an empty one.
pub fn advertised_build(features: &[String]) -> Option<String> {
    value_after(features, FEATURE_BUILD_PREFIX)
}

/// Gates the STRATEGY LANE ([`Request::StrategyOpen`](super::Request::StrategyOpen) /
/// [`Response::StrategyOpened`](super::Response::StrategyOpened), then the tube's frames - wave 2 of
/// `vike-gatewayhub`); negotiated per decision 0112.
///
/// ⚠ **A RUNTIME fact, and the only capability here that is ABSENT-by-default and structural**:
/// advertised ONLY by a KEYED datahub that holds the strategy-lane key (`VIKE_DATAHUB_TRADE_KEY`,
/// [`Scope::Trade`](vike_hub_protocol::auth::Scope::Trade)) and has the lane mounted. The absence of the
/// key is the switch and it fails CLOSED: a key-less datahub, or one without that key, serves no lane
/// and refuses the verb, the way [`FEATURE_DELETE_SERIES`] is withheld from a server that cannot say
/// no. The client refuses LOCALLY, naming the string, before it signs anything toward a daemon that
/// does not advertise it (`HandshakeFrame::Auth { scope: Trade }` is never sent to a datahub that predates the
/// lane: an old datahub answers an unknown scope with an undecodable-request refusal).
pub const FEATURE_STRATEGY_LANE: &str = "strategy_lane";

/// Gates `Quotes` subscriptions on the strategy lane. One string per lane family beside
/// [`FEATURE_STRATEGY_LANE`], so a client refuses a lane the daemon does not serve BY NAME rather than
/// be refused per subscription.
pub const FEATURE_STRATEGY_LANE_QUOTES: &str = "strategy_lane_quotes";

/// Gates bar subscriptions (`Bars`) on the strategy lane. See [`FEATURE_STRATEGY_LANE_QUOTES`].
pub const FEATURE_STRATEGY_LANE_BARS: &str = "strategy_lane_bars";

/// Gates the CEX tick pump (`Ticks`) on the strategy lane. See [`FEATURE_STRATEGY_LANE_QUOTES`].
pub const FEATURE_STRATEGY_LANE_TICKS: &str = "strategy_lane_ticks";

/// Gates the folded and raw book lanes (`Book`, `Depth`) on the strategy lane. See
/// [`FEATURE_STRATEGY_LANE_QUOTES`].
pub const FEATURE_STRATEGY_LANE_BOOK: &str = "strategy_lane_book";

/// Gates the tube's DELIVERY CLASS on a strategy-lane `Sub` (the `class` field, the `Delivery` frame and
/// its marker): the frozen token `stream_class`, the gatewayhub's own string for the same tube feature
/// (`vike_gatewayhub_client::proto::FEATURE_GATEWAY_STREAM_CLASS`; this crate cannot name that one). A peer
/// that does not advertise it ignores a `class` it is sent, so the client sends none.
pub const FEATURE_STREAM_CLASS: &str = "stream_class";

/// Gates the tube's FUNDING frames on the strategy lane (the venue's public funding rate and next
/// settlement, `StreamDown::Funding`): a client that sees the string asks for them with `StreamUp::Funding`
/// at the start of its session, and a session that never asks is sent none. The token `stream_funding`, the
/// gatewayhub's own string for the same tube feature (`vike_gatewayhub_client::proto::FEATURE_GATEWAY_STREAM_FUNDING`;
/// this crate cannot name that one).
pub const FEATURE_STREAM_FUNDING: &str = "stream_funding";

/// Gates BINARY TICK BODIES on the strategy lane (decision 0167's amendment of 2026-11-19, the owner's yes): a
/// client that sees the string asks with `StreamUp::BinaryTicks` at the start of its session, and from then on
/// the datahub writes that session's quote, trade, book delta, mark, candle-close tick and depth frames as
/// `vike_market_stream::binary_ticks` bodies (the same values, a fixed little-endian layout with a version
/// byte); every other frame stays JSON. A session that never asks (an older tradehub) is written JSON only, and
/// a client facing a datahub without the string (an older one) never asks and reads JSON. The receiving engine
/// reads either body on every frame, by its first byte. Advertised with [`FEATURE_STRATEGY_LANE`]; the
/// gatewayhub has no such string, so its streams stay JSON.
pub const FEATURE_STREAM_BINARY_TICKS: &str = "stream_binary_ticks";

/// Gates the strategy lane's STORED FUNDING read
/// ([`Request::StrategyFundingHistory`](super::Request::StrategyFundingHistory), answered with
/// [`Response::Bars`](super::Response::Bars)): one perpetual's settlements out of the store's `interval=funding`
/// series, on the lane's own key, which a restarted paper engine catches up from (decision 0262's amendment of
/// 2026-11-08). Advertised with [`FEATURE_STRATEGY_LANE`] (a keyed datahub with the lane mounted). An older
/// datahub cannot decode the verb, so the client refuses LOCALLY without the string and sends nothing
/// ([`crate::DatahubClient::strategy_funding_history`]).
pub const FEATURE_FUNDING_HISTORY: &str = "funding_history";

/// Gates the strategy lane's STORED INPUT-BARS read
/// ([`Request::StrategyInputBars`](super::Request::StrategyInputBars), answered with
/// [`Response::Bars`](super::Response::Bars)): one bar series a live strategy mount's input reads, the venue's
/// own candles or one `source=` lane of them, on the lane's own key (decision 0271's live half). Advertised with
/// [`FEATURE_STRATEGY_LANE`], beside [`FEATURE_FUNDING_HISTORY`]. An older datahub cannot decode the verb, so the
/// client refuses LOCALLY without the string and sends nothing ([`crate::DatahubClient::strategy_input_bars`]).
pub const FEATURE_INPUT_BARS: &str = "input_bars";

/// Gates [`Request::StrategyInputBars`](super::Request::StrategyInputBars)' `columns`: a live strategy input's bars
/// answered as the bar COLUMNS body ([`FEATURE_BAR_COLUMNS`]' reply, `super::bar_columns`) instead of
/// `Response::Bars` (the owner's "JSON only at the edges" of 2026-11-19). Advertised beside [`FEATURE_INPUT_BARS`].
///
/// ⚠ **Its own string, because an older server decodes the frame and IGNORES the field** (decision 0112, verdict
/// 3) and answers `Response::Bars`: the client asks for columns only of a server that says it answers them.
pub const FEATURE_INPUT_BAR_COLUMNS: &str = "input_bar_columns";

/// Gates the strategy lane's SYMBOL-FACTS read
/// ([`Request::StrategySymbolFacts`](super::Request::StrategySymbolFacts), answered with
/// [`Response::Properties`](super::Response::Properties)): one symbol's facts for a tradehub paper account that holds no
/// trade lease (the owner's ruling of 2026-11-16, decision 0285's amendment). Advertised with
/// [`FEATURE_STRATEGY_LANE`], beside [`FEATURE_INPUT_BARS`]. An older datahub cannot decode the verb, so the client
/// refuses LOCALLY without the string and sends nothing ([`crate::DatahubClient::strategy_symbol_facts`]).
pub const FEATURE_SYMBOL_FACTS: &str = "symbol_facts";

/// Gates the strategy lane's STORED FORECASTS read
/// ([`Request::StrategyForecasts`](super::Request::StrategyForecasts), answered with
/// [`Response::Forecasts`](super::Response::Forecasts)): the `kind=exec_forecast` series the datahub
/// stores from the live strategies' published forecasts (the execution stream), on the lane's own key, which the tradehub's model
/// drift rule reads. Advertised with [`FEATURE_STRATEGY_LANE`], beside [`FEATURE_INPUT_BARS`]. An older datahub
/// cannot decode the verb, so the client refuses LOCALLY without the string and sends nothing
/// ([`crate::DatahubClient::strategy_forecasts`]).
pub const FEATURE_FORECASTS: &str = "forecasts";

/// The prefix of a per-venue STRATEGY-LANE entry - `strategy_venue=binance` - one per venue the lane
/// serves, so a client learns at the HANDSHAKE which venues it may open a stream for. Separate from
/// [`FEATURE_MARKET_DATA_VENUE_PREFIX`](vike_hub_protocol::features::FEATURE_MARKET_DATA_VENUE_PREFIX) on purpose:
/// the market-data plane serves keyless venues only, and the
/// lane serves what the tradehub trades on (credentialed venues join it as the gatewayhub grows their
/// stream sessions). Written and read by a PAIR ([`strategy_venue_feature`] /
/// [`advertised_strategy_venues`]), the `market_data_venue=` shape.
///
/// ⚠ COLLISION-SAFE by construction: every capability check is whole-string equality, so a
/// `strategy_venue=` entry can neither satisfy nor shadow a named capability, nor a `market_data_venue=` one.
pub const FEATURE_STRATEGY_VENUE_PREFIX: &str = "strategy_venue=";

/// Build the `strategy_venue=<slug>` entry - the WRITE half of [`advertised_strategy_venues`].
///
/// ⚠ A pure `format!`: which venues a build serves on the lane is the SERVER's
/// (`crates/vike-datahub/src/feeds/venues.rs`'s `strategy_venues`); this crate names none.
pub fn strategy_venue_feature(slug: &str) -> String {
    format!("{FEATURE_STRATEGY_VENUE_PREFIX}{slug}")
}

/// Every venue slug the strategy lane serves, in ADVERTISEMENT ORDER - the READ half of
/// [`strategy_venue_feature`], trimmed with empty values dropped. It reads ONE prefix, deliberately: a
/// reader that also accepted `market_data_venue=` would report a venue as served on the lane because the market
/// data plane happens to serve it.
pub fn advertised_strategy_venues(features: &[String]) -> Vec<String> {
    values_after(features, FEATURE_STRATEGY_VENUE_PREFIX)
}
