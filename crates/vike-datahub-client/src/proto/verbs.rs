//! Per-verb tables: which scope may send a verb, what it is called, and how long its reply may stay
//! silent. Every classifier here is an EXHAUSTIVE match with no `_` arm, so a new wire verb fails to
//! COMPILE until somebody says which scope may send it and what it is called; WHICH arm was chosen is
//! pinned by this module's tests. Which hub a verb belongs to is `vike_hub_protocol::plane`'s roster, and
//! `verbs_tests.rs` holds this enum to it.

use super::Request;
#[cfg(doc)]
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::auth::VerbScope;
use vike_hub_protocol::conn::ReplyWait;

/// A request's variant name, for a log line or a refusal — never its PAYLOAD, which on the `Run*`
/// verbs is a client-supplied script and on every verb is remote text. Both daemons log refusals
/// by it; exhaustive for the same reason [`required_scope`] is.
///
/// ⚠ **It returns the WIRE tag, the one an operator greps a capture for**;
/// `crates/vike-datahub-client/src/client.rs`'s `response_kind` follows it for
/// [`Response`](super::Response).
pub fn request_kind(r: &Request) -> &'static str {
    match r {
        Request::Ping => "Ping",
        Request::LoadBars { .. } => "LoadBars",
        Request::ScanQuotes { .. } => "ScanQuotes",
        Request::ScanTrades { .. } => "ScanTrades",
        Request::ScanBookUpdates { .. } => "ScanBookUpdates",
        Request::ScanDepth { .. } => "ScanDepth",
        Request::ScanCohort { .. } => "ScanCohort",
        Request::ScanPerpMetrics { .. } => "ScanPerpMetrics",
        Request::ScanTexts { .. } => "ScanTexts",
        Request::ScanChain { .. } => "ScanChain",
        Request::ScanFinancialFacts { .. } => "ScanFinancialFacts",
        Request::ScanInstitutionalHoldings { .. } => "ScanInstitutionalHoldings",
        Request::ScanTextScores { .. } => "ScanTextScores",
        Request::ScanForecasts { .. } => "ScanForecasts",
        Request::ScanEquity { .. } => "ScanEquity",
        Request::ScanExecFills { .. } => "ScanExecFills",
        Request::ScanExecOrders { .. } => "ScanExecOrders",
        Request::PropertiesAsOf { .. } => "PropertiesAsOf",
        Request::ListSeries => "ListSeries",
        Request::Inventory => "Inventory",
        Request::ExecutionSeries { .. } => "ExecutionSeries",
        Request::SeriesGaps { .. } => "SeriesGaps",
        Request::SeriesFacts { .. } => "SeriesFacts",
        Request::SeriesDays { .. } => "SeriesDays",
        Request::Coverage => "Coverage",
        Request::Backfill { .. } => "Backfill",
        Request::ListBackfills => "ListBackfills",
        Request::CancelBackfill { .. } => "CancelBackfill",
        Request::HistoryChannels => "HistoryChannels",
        Request::GatewayStatus => "GatewayStatus",
        Request::GatewayLinks { .. } => "GatewayLinks",
        Request::CredentialsChanged { .. } => "CredentialsChanged",
        Request::ImportArchive(_) => "ImportArchive",
        Request::FetchArchive(_) => "FetchArchive",
        Request::SeedSeries { .. } => "SeedSeries",
        Request::VenueCatalog { .. } => "VenueCatalog",
        Request::OptionsChain { .. } => "OptionsChain",
        Request::OptionsRefresh { .. } => "OptionsRefresh",
        Request::NewsFeed { .. } => "NewsFeed",
        Request::EconomicCalendar { .. } => "EconomicCalendar",
        Request::EquityCalendar { .. } => "EquityCalendar",
        Request::DeleteSeries { .. } => "DeleteSeries",
        Request::MarketDataSubscribe { .. } => "MarketDataSubscribe",
        Request::MarketDataUpdate { .. } => "MarketDataUpdate",
        Request::StrategyOpen { .. } => "StrategyOpen",
        Request::StrategyFundingHistory { .. } => "StrategyFundingHistory",
        Request::StrategyInputBars { .. } => "StrategyInputBars",
        Request::StrategySymbolFacts { .. } => "StrategySymbolFacts",
        Request::StrategyForecasts { .. } => "StrategyForecasts",
        Request::CsvUpload(_) => "CsvUpload",
        Request::CsvUploadChunk { .. } => "CsvUploadChunk",
        Request::CsvUploadEnd => "CsvUploadEnd",
        Request::SymbolSets => "SymbolSets",
        Request::SetSymbolSet { .. } => "SetSymbolSet",
        Request::DeleteSymbolSet { .. } => "DeleteSymbolSet",
        Request::InstrumentLists => "InstrumentLists",
        Request::InstrumentListsOfVenues { .. } => "InstrumentListsOfVenues",
        Request::KeptRowsOfVenues { .. } => "KeptRowsOfVenues",
        Request::RefreshInstrumentLists { .. } => "RefreshInstrumentLists",
        Request::OperatorInstruments { .. } => "OperatorInstruments",
        Request::SplitWarnings => "SplitWarnings",
        Request::LiveFamilyTokens => "LiveFamilyTokens",
        Request::ExecutionStreamOpen { .. } => "ExecutionStreamOpen",
        Request::ExecutionStreams => "ExecutionStreams",
        Request::ExecutionAccountPositions { .. } => "ExecutionAccountPositions",
    }
}

/// Classify one request's [`ReplyWait`]. **EXHAUSTIVE — no `_` arm, deliberately**: a new verb
/// fails to COMPILE here until somebody says who bounds its work, so it cannot silently inherit a
/// bound that clips it or none that would park its caller.
///
/// - **[`ReplyWait::Unbounded`]** — [`Request::Backfill`] (the collector runs inline over the
///   client's window, hours for a fine interval), [`Request::ImportArchive`] (holds the connection
///   until the last day of up to `IMPORT_MAX_DAYS` is stored), [`Request::CsvUploadEnd`] (stores
///   every day of the file the client sent, a decade of daily commits for a long one) and
///   [`Request::OperatorInstruments`] (one venue lookup per symbol the client names, each waiting its
///   turn in the venue's read cap) and [`Request::RefreshInstrumentLists`] (every venue's list in turn, ig's paced
///   by its account's read cap).
/// - **[`ReplyWait::Bounded`]** — everything else.
pub fn reply_wait(request: &Request) -> ReplyWait {
    match request {
        Request::Backfill { .. }
        | Request::ImportArchive(_)
        // ...and the archive DOWNLOAD: up to `FETCH_MAX_DAYS` files of a vendor bucket, each a GET
        // with its retries.
        | Request::FetchArchive(_)
        // The client names how many instruments are looked up, each paced by the venue's own read cap.
        | Request::OperatorInstruments { .. }
        // Every venue's list, one after another, and ig's alone is ~17 reads paced to a quarter of its account's read
        // cap (about three minutes): the answer comes when the last list is kept.
        | Request::RefreshInstrumentLists { .. }
        | Request::CsvUploadEnd => ReplyWait::Unbounded,

        // An upload's open checks and its chunk acknowledgements: answered at once.
        Request::CsvUpload(_)
        | Request::CsvUploadChunk { .. }

        | Request::Ping
        // The store reads: each capped by the server's per-kind row ceiling (`ReadCeilings`), the
        // metadata ones manifest-only.
        | Request::LoadBars { .. }
        | Request::ScanQuotes { .. }
        | Request::ScanTrades { .. }
        | Request::ScanBookUpdates { .. }
        | Request::ScanDepth { .. }
        | Request::ScanCohort { .. }
        | Request::ScanPerpMetrics { .. }
        | Request::ScanTexts { .. }
        | Request::ScanChain { .. }
        | Request::ScanFinancialFacts { .. }
        | Request::ScanInstitutionalHoldings { .. }
        | Request::ScanTextScores { .. }
        | Request::ScanForecasts { .. }
        | Request::ScanEquity { .. }
        | Request::ScanExecFills { .. }
        | Request::ScanExecOrders { .. }
        | Request::PropertiesAsOf { .. }
        | Request::ListSeries
        | Request::Inventory
        | Request::ExecutionSeries { .. }
        | Request::SeriesGaps { .. }
        | Request::SeriesFacts { .. }
        | Request::SeriesDays { .. }
        | Request::Coverage
        | Request::DeleteSeries { .. }
        // Answered from server state: the registry (a cancel flags and answers at once), the
        // overlay, the compiled rosters, the folded options snapshot, the market-data hub.
        | Request::ListBackfills
        | Request::CancelBackfill { .. }
        | Request::HistoryChannels
        | Request::OptionsChain { .. }
        // ...and the options listing's refresh: one gatewayhub round trip, whose venue read the
        // venue agent's own timeout bounds.
        | Request::OptionsRefresh { .. }
        // ...and the gatewayhub's state: a cache, or a wait the server caps in seconds.
        | Request::GatewayStatus
        // ...and the session history: one bounded read of the settings store.
        | Request::GatewayLinks { .. }
        // ...and the relay of a credential change: one gatewayhub round trip, whose teardown and lease end run
        // under the gatewayhub's own join and fence bounds.
        | Request::CredentialsChanged { .. }
        // ...and the three tool-feed snapshots: cache reads, the first poll never holds the reply.
        | Request::NewsFeed { .. }
        | Request::EconomicCalendar { .. }
        | Request::EquityCalendar { .. }
        | Request::MarketDataSubscribe { .. }
        | Request::MarketDataUpdate { .. }
        // ...and the strategy lane's open: answered from server state, the one reply before the switch.
        | Request::StrategyOpen { .. }
        // ...and the execution stream's open (one cursor read, the one reply before the switch) and its status
        // read (one bounded read of the settings store).
        | Request::ExecutionStreamOpen { .. }
        | Request::ExecutionStreams
        // ...and the accounts' position snapshots: one bounded read of the settings store.
        | Request::ExecutionAccountPositions { .. }
        // ...and its store reads (stored funding, an input's bars, the published forecasts): each under its
        // ceiling, like `LoadBars`.
        | Request::StrategyFundingHistory { .. }
        | Request::StrategyInputBars { .. }
        | Request::StrategyForecasts { .. }
        // ...and its symbol-facts read: one store read, else one keyless catalog search under the gatewayhub's resolve
        // bound.
        | Request::StrategySymbolFacts { .. }
        // ...and the user's symbol lists: one bounded read or write of one table of the settings store.
        | Request::SymbolSets
        | Request::SetSymbolSet { .. }
        | Request::DeleteSymbolSet { .. }
        // ...and the kept instrument lists: a read of the server's files.
        | Request::InstrumentLists
        | Request::InstrumentListsOfVenues { .. }
        | Request::KeptRowsOfVenues { .. }
        // ...and the open split warnings: the server's memory and one table of its settings store.
        | Request::SplitWarnings
        // ...and the recorded families' live tokens: the recorder's memory, no venue call.
        | Request::LiveFamilyTokens
        // Venue fetches whose SIZE the server fixes: one `SEED_BARS` window, one venue's catalog
        // request set, each request under the bridges' own HTTP timeout.
        | Request::SeedSeries { .. }
        | Request::VenueCatalog { .. } => ReplyWait::Bounded,
    }
}

/// Classify one request's required scope. **EXHAUSTIVE — no `_` arm, deliberately**: a default of
/// `Read` would leak a write, and a default of `Write` would silently break a read for read-scope
/// clients. Neither is a decision a wildcard should make.
///
/// - **Reads are [`VerbScope::Read`]**: they answer from the store and change nothing. `Ping` is
///   `Read`, not served before the handshake: an unauthenticated liveness probe would be a free
///   oracle for "is this address a vike daemon".
/// - **[`Request::Backfill`] is [`VerbScope::Write`]**: it WRITES the served store and spends
///   venue-API budget from that box's own IP — the "class change" of
///   `docs/decisions/0025-datahub-remote-posture.md`, the reason that record chose authentication.
/// - **[`Request::ImportArchive`] is [`VerbScope::Write`]** too: it writes the store from files on
///   the server's own disk, the client names the window, and a wrong day spends its commit key for
///   good. Its arm carries the argument.
/// - **[`Request::CancelBackfill`] is [`VerbScope::Write`] and [`Request::ListBackfills`] is
///   [`VerbScope::Read`]** —
///   `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`.
///   The cancel writes no row and is still no read: it ends work ANOTHER connection started. The
///   list answers from server state and changes nothing.
/// - **[`Request::HistoryChannels`] is [`VerbScope::Read`]** —
///   `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md`.
pub fn required_scope(request: &Request) -> VerbScope {
    match request {
        Request::Ping
        | Request::LoadBars { .. }
        | Request::ScanQuotes { .. }
        | Request::ScanTrades { .. }
        // The SIX tick-level and research reads (`docs/decisions/0084`): `vike_data::HistoryStore`
        // trait reads, bounded by the range the caller names, writing nothing.
        | Request::ScanBookUpdates { .. }
        | Request::ScanDepth { .. }
        | Request::ScanCohort { .. }
        | Request::ScanPerpMetrics { .. }
        // ...and the option-chain read that joined them for the studies: a trait read, writing nothing.
        | Request::ScanChain { .. }
        | Request::ScanEquity { .. }
        | Request::ScanExecFills { .. }
        | Request::ScanExecOrders { .. }
        // The text history: the same kind of trait read, over the received time.
        | Request::ScanTexts { .. }
        // The SEC tables: the same kind of trait read, over the knowable time.
        | Request::ScanFinancialFacts { .. }
        | Request::ScanInstitutionalHoldings { .. }
        | Request::ScanTextScores { .. }
        // The live strategies' published forecasts (`kind=exec_forecast`), an account kind read like
        // `ScanExecFills` beside it: the strategy lane's `StrategyForecasts` read under the same ceiling, on
        // the Observe key a research study's host holds.
        | Request::ScanForecasts { .. }
        | Request::PropertiesAsOf { .. }
        | Request::ListSeries
        | Request::Inventory
        | Request::ExecutionSeries { .. }
        | Request::SeriesGaps { .. }
        | Request::SeriesFacts { .. }
        // ...and a read's stamped days: one manifest read per series, no row (a tick read's: one per layout).
        | Request::SeriesDays { .. }
        // The cross-kind READ behind the Data Manager's "Partial" column: a plain `HistoryStore` verb.
        | Request::Coverage
        // ⚠ THE MARKET-DATA VERBS ARE READ-SCOPE — a DECISION
        // (`docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md`), not "they look
        // like reads": `Backfill` looks like one too and is Write. The line is BOUNDED BY AN
        // OPERATOR-SET CEILING: a `Backfill`'s venue cost is unbounded and the CLIENT names the
        // range, while a subscription's is bounded by `MARKET_DATA_MAX_KEYS_PER_VENUE` and `MARKET_DATA_LINGER`,
        // which no request moves. Write would force a desktop that only wants a DOM ladder to hold
        // the key that compiles client-supplied Rhai. (On a KEY-LESS server every verb but
        // `DeleteSeries` is served to whoever reaches the loopback socket, per
        // `docs/decisions/0050`.)
        | Request::MarketDataSubscribe { .. }
        | Request::MarketDataUpdate { .. }
        // ⚠⚠ **THE ONE WRITE VERB ON THE READ SIDE, A DECISION AGAINST A STANDING FORWARD RULING.**
        // `docs/decisions/0052`'s reopen list predicted a market-data verb that WRITES would take
        // `Backfill`'s scope; `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` argues
        // the predicate is the CLASS and this verb is not in it — READ IT BEFORE MOVING THIS ARM OR
        // ADDING A FIELD TO THE VARIANT. The short form:
        //   * COST — `SeedSeries` names a series and NOTHING else; the window, bar count, venue
        //     set, interval set, rate and per-process series cap are server constants.
        //   * REACH — the write is ADDITIVE and IDEMPOTENT (commit-key dedup, no existing row is
        //     reachable), CONTAINED (the server's own venue x interval sets), and OPERATOR-ARMED
        //     with an unarmed server still ANSWERING and writing nothing.
        // All four together, or it goes back to Write (0058's decision 3) — the constraint the NEXT
        // write-shaped verb has to argue against. A write that stays on this side joins
        // `writes_under_read_scope`, the per-write re-check's list (decision 0288 item 4). `MarketDataSubscribe` above already spends the same venue
        // budget with NO opt-in at 800 weight/min steady (`crates/vike-datahub/src/market_data/mod.rs`'s
        // `MARKET_DATA_LINGER`); this lane saturates at 60/min and idles at zero.
        | Request::SeedSeries { .. }
        // ⚠⚠ **THE VENUE CATALOG, READ-SCOPE FOR A DIFFERENT REASON — READ
        // `docs/decisions/0062-a-venue-catalog-fetch-is-an-observe-verb-and-not-a-write.md` BEFORE
        // MOVING THIS ARM OR ADDING A FIELD TO THE VARIANT.** It does not pass 0058's rule; it is
        // OUTSIDE it:
        //   * NOT A WRITE — nothing enters the served store (no `append_*`, no commit key, no row),
        //     so 0058's rule is INAPPLICABLE rather than satisfied; the list-replace happens only
        //     in the CLIENT's own on-disk cache.
        //   * COST — the request names a VENUE and nothing else, drawn from the gated
        //     `vike_model::VENUES` roster intersected with the server's table: bounded by an
        //     ENUMERABLE SET rather than a rate, which closes the residual 0058 declared for
        //     `SeedSeries`' free symbol.
        //   * CREDENTIALS — the server's table admits only PUBLIC-endpoint providers, so no client
        //     can make this daemon authenticate as its operator (0062 decision 3) — closed by
        //     construction, not by a switch.
        | Request::VenueCatalog { .. }
        // ⚠⚠ **THE OPTIONS-CHAIN SNAPSHOT, READ-SCOPE FOR THE CATALOG'S REASON** — and read the
        // catalog's arm above first, because this one leans on it:
        //   * NOT A WRITE — it answers from the server's in-memory snapshot. The datahub's OWN
        //     chain recorder (off by default, an OPERATOR's row) may write `kind=chain` from the
        //     same poll, but it records on the server's cadence whether or not anybody asks, so no
        //     request causes, sizes or times a write.
        //   * COST — the request SELECTS from what the server holds: a venue and an underlying from
        //     the lane's own enumerable sets, and at most `MAX_OPTIONS_EXPIRIES` dates, each one
        //     omitted rather than fetched when the server does not hold it. The poll cadence, the
        //     expiry and strike windows and the 100 ms stream focus set are server constants — the
        //     `docs/decisions/0052` bound — and no request moves one.
        //   * CREDENTIALS — every venue call the lane makes is to a PUBLIC keyless endpoint.
        // A parameter that reaches the venue (a strike window, a focus instrument, a cadence) is the
        // change that moves this arm.
        | Request::OptionsChain { .. }
        // ⚠ THE OPTIONS LISTING'S REFRESH, READ-SCOPE FOR THE CHAIN'S REASON WITH ONE DIFFERENCE: it
        // DOES reach the venue (a listing read), so its cost is bounded by the server instead of by
        // the request: the gatewayhub reads the listing no sooner than a minute after its last read
        // and answers the listing it holds inside that minute, whoever asks how often. NOT A WRITE
        // (no store row), no credential (the listing is a keyless public read), and the request
        // names a venue and nothing else. A parameter that sizes or times the venue read is the
        // change that moves this arm to Write.
        | Request::OptionsRefresh { .. }
        // ⚠⚠ **THE THREE TOOL-FEED SNAPSHOTS, READ-SCOPE FOR THE OPTIONS CHAIN'S REASON** — read the
        // arm above first, because these lean on it:
        //   * NOT A WRITE — they answer from the server's in-memory caches; the polls run on the
        //     server's cadence whether or not anybody asks (the first request STARTS them, and that
        //     is the whole of a request's influence on the timing).
        //   * COST — `NewsFeed` and `EconomicCalendar` name NOTHING; `EquityCalendar` names a Monday
        //     from the small enumerable set `validate_equity_week` accepts. The cadences, the
        //     provider URLs and the row bounds are server constants — the `docs/decisions/0052`
        //     bound — and no request moves one.
        //   * CREDENTIALS — two of the five providers are KEYED (Finnhub, FMP). The key is spent on
        //     the SERVER's schedule and never per request, no request input reaches the provider,
        //     and no reply carries the value (`NotConfigured` names the key's NAME). That is the
        //     line `docs/decisions/0062` (decision 3) and `docs/decisions/0097` draw: an Observe
        //     client must not be able to make the daemon authenticate as its operator on the
        //     client's timing or terms, and here it cannot.
        // A parameter that reaches a provider (a symbol, a window, a cadence, a source) is the
        // change that moves these arms.
        | Request::NewsFeed { .. }
        | Request::EconomicCalendar { .. }
        | Request::EquityCalendar { .. }
        // ⚠ THE LIST OF RUNNING BACKFILLS — `docs/decisions/0101`'s verdict 2: it answers from the
        // server's own registry and changes nothing. A read-scope key learns what the operator is
        // fetching and nothing it can act on; a field naming a credential or an account would be
        // 0101's reopener.
        | Request::ListBackfills
        // ⚠⚠ THE HISTORY-CHANNELS READ — READ
        // `docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md` BEFORE MOVING THIS
        // ARM OR ADDING A FIELD TO THE VARIANT. Not a write (0058 inapplicable); the server bounds
        // the whole cost (the request names nothing — 0052); it calls no venue (0062 decision 3);
        // and it never USES a credentialed lane's token, only reports its presence as a WORD, so
        // 0097's reopener does not fire. A parameter, a venue call or a value is 0102's reopener.
        | Request::HistoryChannels
        // ⚠ THE GATEWAYHUB'S STATE, READ-SCOPE FOR THE HISTORY CHANNELS' REASON. Not a write. The request
        // names nothing, and the cost is the server's: one dial at a time and one per
        // `crates/vike-datahub/src/server/gateway_status.rs`'s `GATEWAY_STATUS_TTL`, so a read loop
        // cannot become a dial loop (0052). `GatewayStatus` calls no venue and reads no credential (0062
        // decision 3, 0097). A parameter or a venue call is the change that moves this arm.
        | Request::GatewayStatus
        // The session history: one bounded read of one table of the settings store, no dial and no venue, and
        // what it answers is what `trade health` already shows of a session (`LINK_SESSIONS_READ_LIMIT` bounds it).
        | Request::GatewayLinks { .. }
        // ⚠⚠ **A USER'S CSV UPLOAD, THE SECOND WRITE ON THE READ SIDE** — READ
        // `docs/decisions/0272-a-users-csv-is-uploaded-into-a-user-lane-under-the-observe-key.md`
        // BEFORE MOVING THESE ARMS OR ADDING A FIELD TO `crate::upload::CsvUploadSpec`. It passes
        // `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md`'s four legs, all together:
        //   * ADDITIVE — it only CREATES a `user-<label>` series that holds no row; a series holding
        //     rows (whoever wrote them) is refused before a byte of the file is sent, so no request
        //     alters or removes a row, and the lane is composed by the server from the label, so no
        //     request reaches a venue's own series.
        //   * IDEMPOTENT — a repeat is refused and writes nothing.
        //   * OPERATOR-ARMED — the `flags.datahub_csv_upload` row, off by default; unarmed, the server
        //     advertises no `csv_upload` and refuses having read nothing.
        //   * BOUNDED — venue, interval and format words are enumerable, the bytes capped per file
        //     (the `csv-bars` format's `MAX_FILE_BYTES`) and one upload or import runs at a time; the
        //     free symbol and label are bounded by a COUNT, the server's cap on user series (0058's
        //     declared residual, the chart seed's symbol).
        // A field that names a source, an existing series to add to, or a path is the change that
        // moves these arms to Write.
        | Request::CsvUpload(_)
        | Request::CsvUploadChunk { .. }
        // The user's symbol lists, READ: one bounded read of one table of the settings store, no venue, no credential.
        | Request::SymbolSets
        // The venues' kept instrument lists, READ: the server's files and one table of its settings store, no venue call.
        | Request::InstrumentLists
        | Request::InstrumentListsOfVenues { .. }
        | Request::KeptRowsOfVenues { .. }
        // The open split warnings, READ: the server's memory and one table of its settings store, no vendor call; a
        // warning writes nothing (the refresh is the one writer of a split row).
        | Request::SplitWarnings
        // The recorded families' live tokens, READ: what the recorder holds in memory; the request names nothing and
        // asks no venue (decision 0052's rule: the server bounds the whole cost).
        | Request::LiveFamilyTokens
        // The execution streams' cursors and gaps, READ: one bounded read of the settings store; the request names
        // nothing and writes nothing.
        | Request::ExecutionStreams
        // The accounts' position snapshots, READ: one bounded read of the settings store, a time and nothing else.
        | Request::ExecutionAccountPositions { .. }
        | Request::CsvUploadEnd => VerbScope::Read,

        // The WRITE verb...
        Request::Backfill { .. }
        // ⚠⚠ ...and the verb that STOPS it — READ
        // `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`
        // BEFORE MOVING THIS ARM. It writes no row, but it ends work another connection started
        // (the twin of `record rm`, which 0081 ruled Write), and its whole effect falls on OTHER
        // connections — a read-scope key could stop the operator's backfills while it cannot start
        // one. It DESTROYS nothing, so it takes `Backfill`'s posture, not `DeleteSeries`': served
        // on a key-less LOOPBACK datahub (0101, verdict 1).
        | Request::CancelBackfill { .. }
        // ⚠⚠ ...and the ARCHIVE IMPORT, the second store write — READ
        // `docs/superpowers/specs/2026-09-30-archive-import-lane-design.md` §2.2 BEFORE MOVING
        // THIS ARM. Write for the reasons the two Read-scope writes above are NOT:
        //   * IT IS A WRITE — quote rows, and bars derived from them, into the SERVED store, which
        //     every reader sees and only `DeleteSeries` removes.
        //   * 0058'S BOUNDED LEG FAILS — the CLIENT names the window (`Backfill`'s reason); a
        //     per-request day cap bounds one request, never the total.
        //   * IT STEERS SERVER-SIDE FILESYSTEM READS, and its failure is IRREVERSIBLE: a mis-scaled
        //     day spends its commit key, and the only remedy is deleting the whole series.
        // It removes nothing, so it takes `Backfill`'s posture: served on a key-less LOOPBACK
        // datahub (`bind_decision` refuses a key-less non-loopback bind), Write-only on a keyed
        // one. A desktop holds a read-scope key, so the CLI is this verb's door.
        | Request::ImportArchive(_)
        // ...and the archive DOWNLOAD that feeds it (the owner's ruling of 2026-11-13). Write for
        // `ImportArchive`'s reasons and one of its own, the bill: it spends the OPERATOR's AWS
        // account (a Requester-Pays bucket bills every LIST and GET to the key's owner), the client
        // names the window, and it writes files under the server's imports root.
        | Request::FetchArchive(_)
        // ...and the user's symbol lists, SAVED or REMOVED (the owner's ruling of 2026-11-12): one row of the server's
        // settings database by name. Write, because a list is the operator's own record and an Observe key may read it
        // but not replace or remove it.
        | Request::SetSymbolSet { .. }
        | Request::DeleteSymbolSet { .. }
        // ...and the venues' instrument lists REFRESHED NOW (the owner's ruling of 2026-11-12, `docs/decisions/0062`'s
        // amendment): a venue fetch whose answer the server KEEPS, replacing the list every reader sees, so a write;
        // `VenueCatalog`, which keeps nothing, stays an Observe verb.
        | Request::RefreshInstrumentLists { .. }
        // ...and the instruments the operator adds or removes BY HAND: rows of the server's settings database that every
        // reader of the kept lists then reads, and an add spends the operator's own venue session for its lookup. Write
        // even for an empty change (the read of the rows), so an Observe key reaches no part of the verb.
        | Request::OperatorInstruments { .. }
        // ...and the relay of an operator's credential change to the gatewayhub (decision 0287): it carries a row id and
        // no value and writes nothing, but it BOUNCES that account's trading session (its adapter is rebuilt and its
        // lease ends once), which an Observe key must not be able to do.
        | Request::CredentialsChanged { .. }
        // ...and the DESTRUCTIVE one). Write is necessary and NOT sufficient: a KEY-LESS server
        // refuses it outright (see `FEATURE_DELETE_SERIES`); this arm stops a READ-scope connection
        // on a keyed server reaching it.
        | Request::DeleteSeries { .. } => VerbScope::Write,

        // ⚠⚠ THE STRATEGY LANE AND THE EXECUTION STREAM, THE ONLY VERBS OF THEIR OWN SCOPE. `Trade` is not a
        // superset of `Read` or `Write` and neither is one of it (`scope_admits`): the key that opens the tradehub's price
        // path is not the key that reads the store or runs a backfill, and a control key cannot open
        // the lane. COST is bounded by the SL_* ceilings (a per-session spec cap, a per-venue and a
        // total key cap that does NOT draw on the market-data plane's), never by the request.
        Request::StrategyOpen { .. }
        // ...and the lane's TWO store reads, both under the server's bar ceiling, no write and no venue call:
        // the `interval=funding` series (decision 0262's amendment of 2026-11-08, amending 0052's "the one
        // verb of its own scope"), which a restarted paper engine catches up the settlements it missed from,
        // and an input's bar series (decision 0271's live half: the venue's own candles or one `source=`
        // lane), which a live strategy mount's declared input reads. They are the lane's because the
        // tradehub holds the lane's key and no Observe key; a write or a venue fetch is the change that
        // moves this arm (the symbol-facts read below made that move for one bounded search, 2026-11-16). The
        // third store read, the published forecasts (`kind=exec_forecast`), is the tradehub's model drift rule's
        // (book W9): the series its own live strategies published, read back the same way.
        | Request::StrategyFundingHistory { .. }
        | Request::StrategyInputBars { .. }
        | Request::StrategyForecasts { .. }
        // ...and the one read that may call a venue on the lane's key: a paper account's symbol facts (decision 0285's
        // amendment of 2026-11-16), the store's recorded row else ONE keyless catalog search the datahub makes on its own
        // Read key: the Read scope's own cost (the venue's universe, the gatewayhub's resolve bound), never a window
        // and never a write.
        | Request::StrategySymbolFacts { .. }
        // ⚠⚠ ...and the EXECUTION STREAM's open, the one WRITE on the trade key: the tradehub's own executions into
        // the store. This key, because it is the one datahub key the tradehub holds; and this key alone, so an
        // operator's Write key (a backfill's) cannot inject an execution, and an Observe key cannot either. COST is
        // the tradehub's own journal: the request names no window, and each frame is bounded by its ceiling.
        | Request::ExecutionStreamOpen { .. } => VerbScope::Trade,
    }
}

/// **Whether a [`VerbScope::Read`] verb WRITES**: the chart gap seed (`SeedSeries`, decision 0058) and a
/// user's CSV upload, its chunks and its end (decision 0272), the two read-side writes [`required_scope`]
/// argues. A hub re-reads a device's access row before each, as before every [`VerbScope::Write`] verb
/// (decision 0288 item 4), so a revoked device's open connection is refused its next one. Every other
/// Read verb is a read or a view (a market-data subscription lives until its connection ends, 0288's
/// Q2) and is never re-checked. ⚠ A new write on the Read side joins this list, or a revoke misses it.
pub fn writes_under_read_scope(request: &Request) -> bool {
    matches!(
        request,
        Request::SeedSeries { .. }
            | Request::CsvUpload(_)
            | Request::CsvUploadChunk { .. }
            | Request::CsvUploadEnd
    )
}

#[path = "verbs_tests.rs"]
#[cfg(test)]
mod verbs_tests;
