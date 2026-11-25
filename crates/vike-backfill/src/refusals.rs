//! The refusals the operator backfill bins (`src/bin/*.rs`) and their shared harness ([`crate::cli`])
//! print to a person, declared once (`vike_utility::refusal_enum!`, decision 0301): each variant
//! carries its ONE text and its id.
//!
//! The site that refuses builds the variant and prints `.rendered()` (`error [<id>]: <text>`, the id a
//! person looks up in the release's `refusals.json`), after the bin's own `<bin>: ` prefix where it
//! prints one and before a ` (see --help)` pointer where it gives one: neither is part of a text. Code
//! and tests decide on the variant, never on its words.
//!
//! An id is `backfill.<bin>.<flag>.<words>` for one bin's own argument rule, and
//! `backfill.<family>.<words>` for a failure several bins report (a store that will not open, the
//! credential store, the settings database's address rows): ONE id per family, its cause a field.
//! Words another crate owns (a collector's [`crate::CollectError`], `vike_secrets`' and `vike_venues`'
//! refusals) ride unchanged as a field of the bin's line. Only-logged lines (`tracing`), the
//! bench-fixture loader `ingest_bench_bars` and clap's own usage errors are not here. An id is
//! published, so it stays when the text is reworded.

use vike_secrets::{CredentialScopeError, CredentialUndeclaredKey};
use vike_venues::venue_urls::UrlRefusal;

vike_utility::refusal_enum! {
    /// A refusal a backfill bin prints to a person: this module's doc.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum BackfillBinRefusal with ids BackfillBinRefusalId {
        // ---- the shared harness (`crate::cli`) and the families several bins report ----
        /// A `crate::cli::RETIRED_VARIABLES` name is set; `set` names each with its replacement.
        RetiredVariables { set: String }
            => "backfill.env.retired_variables":
               "refusing to start — decision 0111 retired these process variables, and a set one would be \
                ignored in silence:\n{set}\nUnset them and run again. Nothing was read and nothing was written.",
        /// The settings database exists and would not read its `venue_url` rows.
        AddressRowsUnreadable { why: String }
            => "backfill.address.settings_unreadable":
               "the settings database would not read its address rows: {why}",
        /// The vendor's `live` `history` `rest` address row answered no address.
        AddressRefused { why: UrlRefusal }
            => "backfill.address.refused":
               "{why}",
        /// `$VIKE_NO_LIVE_KEYS` holds a word other than `true`/`false`, so no credential store opened.
        CredentialScope { why: CredentialScopeError }
            => "backfill.credentials.scope_refused":
               "{why}",
        /// The bin asked for a credential its own scope never declared: a defect in the bin.
        CredentialUndeclared { key: CredentialUndeclaredKey }
            => "backfill.credentials.undeclared":
               "credential scope defect: {key}",
        /// The hist store at `root` would not open.
        StoreOpenFailed { root: String, why: String }
            => "backfill.store.open_failed":
               "open store {root}: {why}",
        /// `--symbols` named no symbol.
        SymbolsEmpty
            => "backfill.symbols.empty":
               "--symbols produced no entries",
        /// `--from`/`--to` is not a pair of calendar days, or runs backwards.
        DayRangeInvalid { from: String, to: String }
            => "backfill.day_range.invalid":
               "bad --from/--to (want YYYY-MM-DD) or --from after --to: {from}..{to}",
        /// The `--tokens-file` cannot be read.
        TokensFileUnreadable { path: String, why: String }
            => "backfill.tokens_file.unreadable":
               "failed to read --tokens-file {path}: {why}",
        /// The vendor collector's fetch-and-ingest failed.
        RunFailed { why: String }
            => "backfill.run.failed":
               "backfill failed: {why}",
        /// The owned scratch directory under `<project>/tmp` cannot be created.
        ScratchDirFailed { why: String }
            => "backfill.scratch.create_failed":
               "create scratch dir: {why}",
        /// The system clock reads before 1970.
        ClockBeforeEpoch { why: String }
            => "backfill.clock.before_epoch":
               "system clock is before the unix epoch: {why}",

        // ---- databento_backfill ----
        /// A `--schema` Databento's ingest does not map.
        DatabentoSchemaUnknown { schema: String }
            => "backfill.databento.schema.unknown":
               "unknown schema '{schema}': want trades|ohlcv-1m|ohlcv-1h|ohlcv-1d|mbp-1|mbp-10",
        /// `DATABENTO_API_KEY` is not in the credential store.
        DatabentoApiKeyMissing
            => "backfill.databento.api_key.missing":
               "DATABENTO_API_KEY not set in the credential store (vike-cli secrets set \
                DATABENTO_API_KEY, value on stdin)",

        // ---- tardis_backfill ----
        /// `--start`/`--end` is not a `YYYY-MM-DD` day.
        TardisRangeNotADate
            => "backfill.tardis.range.not_a_date":
               "--start/--end must be YYYY-MM-DD",
        /// A `--data-type` Tardis' ingest does not map.
        TardisDataTypeUnknown { data_type: String }
            => "backfill.tardis.data_type.unknown":
               "unknown data-type '{data_type}': trades|quotes|incremental_book_L2|bars",
        /// An `--interval` the shared interval vocabulary refuses (`0m`, a second spelling such as `01m`).
        TardisIntervalInvalid { interval: String }
            => "backfill.tardis.interval.invalid":
               "--interval '{interval}' is not a valid interval (e.g. 1m, 5m, 1h, 1d)",
        /// The trades-to-bars resample after the ingest failed.
        TardisResampleFailed { why: String }
            => "backfill.tardis.resample.failed":
               "bars resample failed: {why}",

        // ---- eod_backfill ----
        /// A `--source` no EOD provider answers to; `have` lists the ones that do.
        EodSourceUnknown { source: String, have: String }
            => "backfill.eod.source.unknown":
               "unknown --source '{source}' (have: {have})",

        // ---- exec_trade_backfill ----
        /// The `--file` export cannot be read.
        ExecTradeFileUnreadable { file: String, why: String }
            => "backfill.exec_trade.file.unreadable":
               "reading {file}: {why}",
        /// The `--file` export is not a Binance `myTrades` JSON.
        ExecTradeFileUnparsable { file: String, why: String }
            => "backfill.exec_trade.file.unparsable":
               "parsing {file}: {why}",
        /// The store refused the parsed fills.
        ExecTradeAppendFailed { why: String }
            => "backfill.exec_trade.append_failed":
               "appending: {why}",

        // ---- events_api_backfill ----
        /// Neither `--tokens` nor `--tokens-file` named a token: an events-API pull is per token.
        EventsApiNoTokens
            => "backfill.events_api.tokens.none":
               "no tokens given — need --tokens T1,T2,... and/or --tokens-file PATH",
        /// A `--kind` the events API does not serve.
        EventsApiKindUnknown
            => "backfill.events_api.kind.unknown":
               "unknown --kind (want book|trade|quote|all)",
        /// A day of the range that has no UTC window.
        EventsApiDateInvalid { date: String, why: String }
            => "backfill.events_api.date.invalid":
               "bad date {date}: {why}",

        // ---- vike_archive_backfill ----
        /// `--gaps` with `--from`/`--to`.
        ArchiveGapsWithRange
            => "backfill.vike_archive.gaps.with_range":
               "--gaps takes the day list from the store's coverage report; drop --from/--to",
        /// Neither `--from` and `--to` nor `--gaps`.
        ArchiveRangeRequired
            => "backfill.vike_archive.range.required":
               "either --from and --to, or --gaps, is required",
        /// `--gaps` could not read the store's coverage report.
        ArchiveCoverageUnreadable { why: String }
            => "backfill.vike_archive.gaps.coverage_unreadable":
               "--gaps: reading coverage: {why}",
        /// A `--kind` the archive does not serve.
        ArchiveKindUnknown { kind: String }
            => "backfill.vike_archive.kind.unknown":
               "unknown --kind '{kind}': want book|trade|quote|all",
        /// `--asset` without `--tenor`.
        ArchiveAssetWithoutTenor
            => "backfill.vike_archive.asset.without_tenor":
               "--asset given without --tenor (both or neither)",
        /// `--tenor` without `--asset`.
        ArchiveTenorWithoutAsset
            => "backfill.vike_archive.tenor.without_asset":
               "--tenor given without --asset (both or neither)",
        /// `--file` with `--dry-run`.
        ArchiveFileWithDryRun
            => "backfill.vike_archive.file.with_dry_run":
               "--file and --dry-run are mutually exclusive: the file is already local, there is \
                nothing to plan",
        /// `--file` over a range of other than one day.
        ArchiveFileMultiDay { days: usize, from: String, to: String }
            => "backfill.vike_archive.file.multi_day":
               "--file requires exactly one day (--from == --to); got a {days}-day range \
                [{from}, {to}] — one local Parquet file is one date, not a range",
        /// `--file` with the default `all` kind.
        ArchiveFileKindAll
            => "backfill.vike_archive.file.kind_all":
               "--file requires an explicit --kind book|trade|quote (not the default 'all') — one \
                local Parquet file is one stream, not the archive's three-stream set",
        /// `--file` with `--asset`/`--tenor`, which steer only a remote fetch.
        ArchiveFileWithFamily { asset: String, tenor: String }
            => "backfill.vike_archive.file.with_family":
               "--file and --asset/--tenor are mutually exclusive: --file names the local Parquet \
                file outright, so there is no URL left for the family target (--asset {asset} \
                --tenor {tenor}) to resolve, and asset/tenor are not store partition keys (rows land \
                under venue=polymarket at symbol=<token_id> read from the file itself) — drop \
                --asset/--tenor, or drop --file to fetch the family file over HTTP",
        /// The dry run's `manifest.json` fetch failed.
        ArchiveManifestFetchFailed { why: String }
            => "backfill.vike_archive.manifest.fetch_failed":
               "dry-run: fetch manifest.json failed: {why}",
        /// The `--file` local ingest failed.
        ArchiveFileIngestFailed { why: String }
            => "backfill.vike_archive.file.ingest_failed":
               "local-file ingest failed: {why}",

        // ---- vikedata_backfill ----
        /// `--days` with `--start`/`--end`.
        VikedataDaysWithRange
            => "backfill.vikedata.days.with_range":
               "--days and --start/--end are two different run shapes; pass one",
        /// A `--days` that is not a whole number.
        VikedataDaysNotANumber { days: String }
            => "backfill.vikedata.days.not_a_number":
               "--days '{days}' is not a whole number",
        /// `--days 0`.
        VikedataDaysZero
            => "backfill.vikedata.days.zero":
               "--days must be at least 1",
        /// A `--start` that is not a window boundary.
        VikedataStartNotADate { start: String }
            => "backfill.vikedata.start.not_a_date":
               "--start '{start}' is not a date",
        /// An `--end` that is not a window boundary.
        VikedataEndNotADate { end: String }
            => "backfill.vikedata.end.not_a_date":
               "--end '{end}' is not a date",
        /// The cohort window refused the `--start`/`--end` pair (its words are the collector's).
        VikedataWindowRefused { why: String }
            => "backfill.vikedata.window.refused":
               "{why}",
        /// `--start` without `--end`.
        VikedataStartNeedsEnd
            => "backfill.vikedata.start.needs_end":
               "--start needs --end",
        /// `--end` without `--start`.
        VikedataEndNeedsStart
            => "backfill.vikedata.end.needs_start":
               "--end needs --start",
        /// No run shape at all.
        VikedataNoRunShape
            => "backfill.vikedata.no_run_shape":
               "pass --schedule, or --days N, or --start DATE --end DATE",
        /// `--catch-up` without `--schedule`.
        VikedataCatchUpNeedsSchedule
            => "backfill.vikedata.catch_up.needs_schedule":
               "--catch-up is the SCHEDULED shape's look-back; it needs --schedule. A \
                manual run names its own window with --days or --start/--end.",
        /// An `--axis` that names no ladder axis.
        VikedataAxisUnknown { axis: String }
            => "backfill.vikedata.axis.unknown":
               "unknown --axis '{axis}': size|pnl|tier",
        /// A `--grading` that names no grading.
        VikedataGradingUnknown { grading: String }
            => "backfill.vikedata.grading.unknown":
               "unknown --grading '{grading}': realized|realized-pit|unrealized",
        /// `--schedule` with a typed window boundary (`--days`, `--start` or `--end`): the timer's safety.
        VikedataScheduleWithBoundary { flag: &'static str }
            => "backfill.vikedata.schedule.with_boundary":
               "--schedule and {flag} are two different run shapes; pass one. --schedule derives \
                whole COMPLETE UTC days off a fixed grid precisely so no boundary is typed — a \
                typed one slides with the clock, and two firings over overlapping hours are two \
                commit keys, so the shared hours land TWICE.",
        /// `--schedule` with `--axis` or `--grading`: the polled ladder set is a reviewed table.
        VikedataScheduleWithLadder { flag: &'static str }
            => "backfill.vikedata.schedule.with_ladder":
               "--schedule polls the DECLARED ladder set \
                (crates/vike-backfill/src/vikedata/schedule.rs's POLLED), so {flag} has no meaning \
                here: a schedule that retyped it would be a second copy of a decision that carries \
                a written reason for every ladder it leaves out. Use --axis/--grading with \
                --start/--end for an ad-hoc read.",
        /// A `--catch-up` that is not a whole number; `max` is the look-back ceiling.
        VikedataCatchUpNotANumber { catch_up: String, max: u32 }
            => "backfill.vikedata.catch_up.not_a_number":
               "--catch-up '{catch_up}' is not a whole number (1..={max} days of look-back)",
        /// The schedule refused the `--catch-up` (its words are the collector's).
        VikedataScheduleRefused { why: String }
            => "backfill.vikedata.schedule.refused":
               "{why}",
        /// `--panel` with a ladder or schedule flag.
        VikedataPanelWithLadder
            => "backfill.vikedata.panel.with_ladder":
               "--panel takes only --start/--end — it fetches no ladder and may not ride the schedule",
        /// `--panel` without both `--start` and `--end`.
        VikedataPanelNeedsRange
            => "backfill.vikedata.panel.needs_range":
               "--panel needs --start and --end",
        /// A `--panel` boundary that is neither a date nor unix seconds.
        VikedataPanelNotADate
            => "backfill.vikedata.panel.not_a_date":
               "--start/--end must be dates or unix seconds",
        /// A `--panel` window whose end is not after its start.
        VikedataPanelEndNotAfterStart
            => "backfill.vikedata.panel.end_not_after_start":
               "--end must be after --start",
        /// The cohort API key is not in the credential store: absent is a refusal, never a fallback.
        VikedataApiKeyMissing { key: &'static str }
            => "backfill.vikedata.api_key.missing":
               "{key} is not in the credential store, so this run is REFUSED. An \
                unauthenticated fetch 403s part-way through the window and would store a partial \
                batch under a commit key claiming the whole one.",
        /// The hourly asset panel fetch failed.
        VikedataPanelFetchFailed { exchange: String, asset: String, why: String }
            => "backfill.vikedata.panel.fetch_failed":
               "panel fetch {exchange}/{asset}: {why}",
        /// The store refused the panel's rows.
        VikedataPanelAppendFailed { exchange: String, asset: String, why: String }
            => "backfill.vikedata.panel.append_failed":
               "append perp_metrics {exchange}/{asset}: {why}",
        /// One ladder's window failed; the other ladders still run.
        VikedataLadderFailed { ladder: String, start: i64, end: i64, why: String }
            => "backfill.vikedata.ladder.failed":
               "backfill failed: ladder={ladder} window=[{start}, {end}]: {why}",
    }
}
