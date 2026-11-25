//! The router: `dispatch` splits out the mode switch and the close, `handle_request` maps every other verb.

use std::sync::Arc;

use vike_data::{HistoryStore, TimestampRange};
use vike_datahub_client::HistoryStoreDatahub;
use vike_datahub_client::proto::{
    BAR_COLUMNS, BOOK_UPDATE_COLUMNS, BarColumnsBody, CHAIN_COLUMNS, COHORT_COLUMNS,
    COLUMNS_EMPTY_LEN, ColumnRow, ColumnsMagic, DEPTH_COLUMNS, EQUITY_COLUMNS,
    EXECUTION_FILL_COLUMNS, EXECUTION_ORDER_COLUMNS, FINANCIAL_FACT_COLUMNS, FORECAST_COLUMNS,
    INSTITUTIONAL_HOLDING_COLUMNS, PERP_METRIC_COLUMNS, QUOTE_COLUMNS, Request, Response,
    TEXT_COLUMNS, TEXT_SCORE_COLUMNS, TRADE_COLUMNS, columns_empty_len, columns_row_len,
    compress_columns, encode_columns_into,
};
use vike_hub_protocol::frame::{BodyCodec, reply_body_of_msgpack};

use crate::backfill::BackfillTable;
use crate::import::ImportLane;
use crate::market_data::MarketDataHub;
use crate::seed::SeedLane;

use super::RESAMPLE_SCAN_CEILING;
use super::backfill::{backfill_verb, cancel_backfill_verb, no_backfill_registry};
use super::bar_resample::resampled_bars;
use super::connection::{ConnectionLimits, ServerLanes};
use super::delete::delete_series_verb;
use super::gateway_links::gateway_links_verb;
use super::gateway_status::{credentials_changed_verb, gateway_status_verb};
use super::market_data::{market_data_update_verb, refuse_an_oversized_spec_list};
use super::options_chain::{options_chain_verb, options_refresh_verb};
use super::range_reads::{
    FrameBudget, LOAD_BARS, RangeVerb, SCAN_BOOK_UPDATES, SCAN_CHAIN, SCAN_COHORT, SCAN_DEPTH,
    SCAN_EQUITY, SCAN_EXECUTION_FILLS, SCAN_EXECUTION_ORDERS, SCAN_FINANCIAL_FACTS, SCAN_FORECASTS,
    SCAN_INSTITUTIONAL_HOLDINGS, SCAN_PERP_METRICS, SCAN_QUOTES, SCAN_TEXT_SCORES, SCAN_TEXTS,
    SCAN_TRADES, STRATEGY_FORECASTS, STRATEGY_FUNDING_HISTORY, STRATEGY_INPUT_BARS, bars_head,
    book_stored_rows, range_rows, range_verb, row_count,
};
use super::seed_series::seed_series_verb;
use super::series_gaps::series_gaps_verb;
use super::step::{Step, StopProbe};
use super::strat::strategy_open_verb;
use super::symbol_sets::{delete_symbol_set_verb, set_symbol_set_verb, symbol_sets_verb};
use super::tool_feeds::{economic_calendar_verb, equity_calendar_verb, news_feed_verb};
use super::venue_catalog::venue_catalog_verb;

/// The refusal a build with NO market-data plane answers [`Request::MarketDataSubscribe`] with.
///
/// ⚠ **It is a [`Response::Error`] and the connection SURVIVES POSITIONALLY — it is deliberately
/// NOT an all-refused `MarketDataSubscribed`.** That is why
/// `vike_datahub_client::market::MarketDataRefusal` carries no `HubNotMounted` variant, and it is a
/// considered deviation from §4.4: "no hub" is a whole-REQUEST condition, uniform across every spec,
/// so expressing it as a PER-SPEC refusal would mode-switch a hub-less build into a heartbeat-only
/// writer that will never send a frame — worse than the refusal it replaces. This shape is
/// byte-for-byte what [`backfill_verb`] already answers for an unmounted collector table, and what
/// leg (3) of every `FEATURE_*` doc in the proto promises.
///
/// A `const` because two things must be able to name it: this refusal, and the test that proves it
/// is the same server refusing on the path a client actually takes.
/// ⚠ **IT LEADS WITH THE SETTINGS ROW, AND IT USED TO LEAD WITH A REBUILD THAT CANNOT BE THE FIX.** The plane is armed by an OPERATOR, not by a build: `crate::datahub_cli`'s hub mount
/// carries no `#[cfg]` at all (that is the whole point of the feature-free hub), so `market_data.is_none()`
/// here holds if and only if the `flags.datahub_live` row is off.
pub const NO_MARKET_DATA_PLANE: &str = "this datahub serves no market-data plane. It is armed by an OPERATOR, not by a build: run \
     `vike-cli config set flags.datahub_live true` on the server and restart it. Nothing was subscribed and this connection is unchanged: every \
     other verb still works on it.";

/// Route one decoded request, splitting the MODE SWITCH — and the CLOSE — out from everything else.
///
/// The `market_data.is_some()` test is what decides between the switch and [`NO_MARKET_DATA_PLANE`], and it
/// is a RUNTIME fact rather than a cfg — the same rule `FEATURE_MARKET_DATA`'s advertisement
/// follows, so "can serve" and "armed by an operator" cannot answer differently.
///
/// The close is [`Step::Close`]: a request whose `probe` saw its client gone is answered by closing
/// the connection, never by a write. The check reads the probe's latch and does not peek, so a
/// request that never asked the probe — every verb but `Backfill` and `ImportArchive` — cannot
/// reach it.
///
/// `codec` is the form the request came in (`FEATURE_WIRE_MSGPACK`): the connection loop writes every value in it, and
/// the replies this router answers as held BYTES (the listings) are made in it here.
#[expect(clippy::too_many_arguments)]
pub(super) fn dispatch(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    backfill: Option<&BackfillTable>,
    keyed: bool,
    market_data: Option<&Arc<MarketDataHub>>,
    seed: Option<&SeedLane>,
    lanes: &ServerLanes,
    import: Option<&ImportLane>,
    facts: ConnectionLimits,
    probe: &StopProbe,
    codec: BodyCodec,
) -> Step {
    match request {
        Request::MarketDataSubscribe { specs } => match market_data {
            // ⚠ ALL THREE whole-REQUEST refusals answer the same way and on the same loop: no hub,
            // an over-length spec list, and no stream-connection slot. See [`Step::ModeSwitch`] for
            // why the session is opened here rather than inside the writer.
            //
            // The ORDER is the `delete_series_verb` idiom: the refusal that is about the SERVER
            // rather than about the request comes first, then the request's own cheapest check,
            // then the one that reserves something.
            Some(hub) => {
                match refuse_an_oversized_spec_list("MarketDataSubscribe", "specs", specs.len()) {
                    Some(why) => Step::reply(Response::Error(why)),
                    None => match hub.open_session() {
                        Ok(guard) => Step::ModeSwitch(guard, specs),
                        Err(msg) => Step::reply(Response::Error(msg)),
                    },
                }
            }
            None => Step::reply(Response::Error(NO_MARKET_DATA_PLANE.to_string())),
        },
        // The STRATEGY LANE's mode switch: the other one. After whether a hub is mounted at all, `keyed`
        // is the second thing its door looks at (a key-less server serves no lane whatever is mounted).
        Request::StrategyOpen { venue, network } => {
            strategy_open_verb(&venue, network.as_deref(), keyed, lanes.strat.as_ref())
        }
        // A user's CSV upload reads its own chunks off this socket: the connection loop runs it.
        Request::CsvUpload(spec) => Step::Upload(Box::new(spec)),
        // A bar read answered as COLUMNS (`FEATURE_BAR_COLUMNS`, decision 0107's amendment of 2026-11-18): the
        // JSON arm's rows, cut by the same rules to the frame the columns take (`columns_step`). A refusal is the
        // JSON arm's refusal frame, id and words. The connection loop holds the read's range-read place.
        // ...and a bar read RESAMPLED to a coarser interval (`FEATURE_BAR_RESAMPLE`): the range's stored rows
        // folded by `resample` in `super::bar_resample`, answered whole as JSON or as the columns it asked for.
        Request::LoadBars {
            venue,
            symbol,
            interval,
            start,
            end,
            limit,
            source,
            columns,
            resample_to,
        } if columns.is_some() || resample_to.is_some() => {
            let range = TimestampRange { start, end };
            let rows = match resample_to {
                Some(to) => resampled_bars(
                    &interval,
                    range,
                    limit,
                    &to,
                    HistoryStoreDatahub::BAR_PAGE_ROWS as usize,
                    RESAMPLE_SCAN_CEILING,
                    |page, n| {
                        bars_head(&**store, &venue, &symbol, &interval, page, source.as_deref())(n)
                    },
                ),
                None => range_rows(
                    &LOAD_BARS,
                    &format!("{venue}:{symbol}:{interval}"),
                    range,
                    limit,
                    facts.ceilings.bars,
                    FrameBudget { bytes: facts.ceilings.frame_bytes, envelope: COLUMNS_EMPTY_LEN },
                    bars_head(&**store, &venue, &symbol, &interval, range, source.as_deref()),
                    row_count,
                    |r| r.timestamp,
                    columns_row_len,
                ),
            };
            match (rows, columns) {
                (Err(refusal), _) => Step::reply(Response::Refused(refusal.into())),
                // A resampled read that asked for no columns: the JSON reply, whole.
                (Ok(rows), None) => Step::reply(Response::Bars(rows)),
                (Ok(rows), Some(body)) => columns_step(rows, &BAR_COLUMNS, body),
            }
        }
        // A live strategy input's bars answered as COLUMNS (`FEATURE_INPUT_BAR_COLUMNS`): `handle_request`'s JSON arm's
        // rows, cut by the same rules to the frame the columns take, written as `LoadBars`' columns are.
        Request::StrategyInputBars {
            venue,
            symbol,
            interval,
            source,
            start,
            end,
            columns: Some(body),
        } => {
            let range = TimestampRange { start: Some(start), end: Some(end) };
            scan_columns(
                &STRATEGY_INPUT_BARS,
                &format!("{venue}:{symbol}:{interval}"),
                range,
                None,
                facts.ceilings.bars,
                facts.ceilings.frame_bytes,
                bars_head(&**store, &venue, &symbol, &interval, range, source.as_deref()),
                row_count,
                |r| r.timestamp,
                (&BAR_COLUMNS, body),
            )
        }
        // The store's listing and the kept lists, answered in msgpack as the bytes their values write, held while their
        // sources hold (`super::series_list`, `crate::instrument_lists::instrument_lists_msgpack`); a JSON ask is
        // answered the value, as `handle_request`'s arms write it.
        Request::ListSeries => super::series_list::LISTED.step(&**store, codec),
        Request::InstrumentLists => instrument_lists_step(lanes, None, codec),
        Request::InstrumentListsOfVenues { venues } => {
            instrument_lists_step(lanes, Some(venues.as_slice()), codec)
        }
        // The kept rows of named venues in BINARY, stamped (`crate::instrument_lists::kept_rows_verb`): bytes, not a
        // value; a refusal is the JSON error.
        Request::KeptRowsOfVenues { venues, known } => {
            match crate::instrument_lists::kept_rows_verb(
                lanes.instruments.as_deref(),
                lanes.settings_store.as_ref(),
                &venues,
                &known,
            ) {
                Ok(body) => Step::Bytes(body),
                Err(why) => Step::reply(Response::Error(why)),
            }
        }
        // The tick-level scans answered as COLUMNS (`FEATURE_TICK_COLUMNS`): the JSON arm's read (the same
        // store call as `handle_request`'s arm of the verb), its rows cut by the same rules to the frame the
        // columns take, a refusal in the same words. Each its own magic: a depth reply is never a book one.
        Request::ScanQuotes { venue, symbol, start, end, limit, source, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_QUOTES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.quotes,
                facts.ceilings.frame_bytes,
                |n| match source.as_deref() {
                    Some(lane) => {
                        store.scan_quotes_sourced_capped(&venue, lane, &symbol, range, Some(n))
                    }
                    None => store.scan_quotes_capped(&venue, &symbol, range, Some(n)),
                },
                row_count,
                |r| r.timestamp,
                (&QUOTE_COLUMNS, body),
            )
        }
        Request::ScanTrades { venue, symbol, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_TRADES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.trades,
                facts.ceilings.frame_bytes,
                |n| store.scan_trades_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.timestamp,
                (&TRADE_COLUMNS, body),
            )
        }
        Request::ScanBookUpdates { venue, symbol, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_BOOK_UPDATES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.book_levels,
                facts.ceilings.frame_bytes,
                |n| store.scan_book_updates_capped(&venue, &symbol, range, Some(n)),
                book_stored_rows,
                |r| r.timestamp,
                (&BOOK_UPDATE_COLUMNS, body),
            )
        }
        Request::ScanDepth { venue, symbol, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_DEPTH,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.book_levels,
                facts.ceilings.frame_bytes,
                |n| store.scan_depth_capped(&venue, &symbol, range, Some(n)),
                book_stored_rows,
                |r| r.timestamp,
                (&DEPTH_COLUMNS, body),
            )
        }
        Request::ScanPerpMetrics { venue, symbol, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_PERP_METRICS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.perp_metrics,
                facts.ceilings.frame_bytes,
                |n| store.scan_perp_metrics_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                (&PERP_METRIC_COLUMNS, body),
            )
        }
        // Every other row-shaped scan answered as COLUMNS (`FEATURE_SCAN_COLUMNS`), under the tick scans' rule: the
        // JSON arm's read and ceiling, its rows cut to the frame the columns take, a refusal in the same words.
        Request::ScanCohort { venue, asset, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_COHORT,
                &format!("{venue}:{asset}"),
                range,
                limit,
                facts.ceilings.cohort,
                facts.ceilings.frame_bytes,
                |n| store.scan_cohort_capped(&venue, &asset, range, Some(n)),
                row_count,
                |r| r.ts,
                (&COHORT_COLUMNS, body),
            )
        }
        Request::ScanTexts { text_kind, collector, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_TEXTS,
                &format!("{text_kind}:{collector}"),
                range,
                limit,
                facts.ceilings.texts,
                facts.ceilings.frame_bytes,
                |n| store.scan_texts_capped(&text_kind, &collector, range, Some(n)),
                row_count,
                |r| r.received_ms,
                (&TEXT_COLUMNS, body),
            )
        }
        Request::ScanChain { venue, underlying, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_CHAIN,
                &format!("{venue}:{underlying}"),
                range,
                limit,
                facts.ceilings.chain,
                facts.ceilings.frame_bytes,
                |n| store.scan_chain_capped(&venue, &underlying, range, Some(n)),
                row_count,
                |r| r.ts,
                (&CHAIN_COLUMNS, body),
            )
        }
        Request::ScanFinancialFacts { venue, symbol, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_FINANCIAL_FACTS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.financial_facts,
                facts.ceilings.frame_bytes,
                |n| store.scan_financial_facts_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.filed_ms,
                (&FINANCIAL_FACT_COLUMNS, body),
            )
        }
        Request::ScanInstitutionalHoldings {
            venue,
            symbol,
            start,
            end,
            limit,
            columns: Some(body),
        } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_INSTITUTIONAL_HOLDINGS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.institutional_holdings,
                facts.ceilings.frame_bytes,
                |n| store.scan_institutional_holdings_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.filed_ms,
                (&INSTITUTIONAL_HOLDING_COLUMNS, body),
            )
        }
        Request::ScanTextScores {
            text_kind,
            model_name,
            start,
            end,
            limit,
            columns: Some(body),
        } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_TEXT_SCORES,
                &format!("{text_kind}:{model_name}"),
                range,
                limit,
                facts.ceilings.text_scores,
                facts.ceilings.frame_bytes,
                |n| store.scan_text_scores_capped(&text_kind, &model_name, range, Some(n)),
                row_count,
                |r| r.received_ms,
                (&TEXT_SCORE_COLUMNS, body),
            )
        }
        Request::ScanForecasts { venue, symbol, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_FORECASTS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.forecasts,
                facts.ceilings.frame_bytes,
                |n| store.scan_forecasts_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                (&FORECAST_COLUMNS, body),
            )
        }
        Request::StrategyForecasts { venue, symbol, start, end, columns: Some(body) } => {
            let range = TimestampRange { start: Some(start), end: Some(end) };
            scan_columns(
                &STRATEGY_FORECASTS,
                &format!("{venue}:{symbol}"),
                range,
                None,
                facts.ceilings.forecasts,
                facts.ceilings.frame_bytes,
                |n| store.scan_forecasts_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                (&FORECAST_COLUMNS, body),
            )
        }
        Request::ScanEquity { venue, symbol, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_EQUITY,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.equity,
                facts.ceilings.frame_bytes,
                |n| store.scan_equity_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.timestamp,
                (&EQUITY_COLUMNS, body),
            )
        }
        Request::ScanExecFills { venue, symbol, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_EXECUTION_FILLS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.execution_fills,
                facts.ceilings.frame_bytes,
                |n| store.scan_execution_fills_head(&venue, &symbol, range, n),
                row_count,
                |r| r.ts,
                (&EXECUTION_FILL_COLUMNS, body),
            )
        }
        Request::ScanExecOrders { venue, symbol, start, end, limit, columns: Some(body) } => {
            let range = TimestampRange { start, end };
            scan_columns(
                &SCAN_EXECUTION_ORDERS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                facts.ceilings.execution_orders,
                facts.ceilings.frame_bytes,
                |n| store.scan_execution_orders_head(&venue, &symbol, range, n),
                row_count,
                |r| r.ts,
                (&EXECUTION_ORDER_COLUMNS, body),
            )
        }
        // The EXECUTION STREAM's open: the connection loop runs it, since its session owns the socket. A datahub that
        // mounts no lane (no settings database to keep a cursor in) refuses here, positionally.
        Request::ExecutionStreamOpen { journal_id, tradehub, oldest_sequence, head_sequence } => {
            match lanes.execution_stream {
                Some(_) => Step::ExecutionStream(Box::new(
                    crate::execution_stream::ExecutionStreamOpening {
                        journal_id,
                        tradehub,
                        oldest_sequence,
                        head_sequence,
                    },
                )),
                None => Step::reply(Response::Error(
                    crate::execution_stream::NO_EXECUTION_STREAM.to_string(),
                )),
            }
        }
        other => {
            let response = handle_request(
                other,
                store,
                backfill,
                keyed,
                market_data.map(|h| &**h),
                seed,
                lanes,
                import,
                facts,
                probe,
            );
            if probe.peer_gone() { Step::Close } else { Step::reply(response) }
        }
    }
}

/// `Request::InstrumentLists` (`venues` `None`) or `Request::InstrumentListsOfVenues` asked in `codec`: the bytes of
/// its reply from the held rows in a msgpack form, the value to a JSON ask.
fn instrument_lists_step(lanes: &ServerLanes, venues: Option<&[String]>, codec: BodyCodec) -> Step {
    let (dir, settings) = (lanes.instruments.as_deref(), lanes.settings_store.as_ref());
    if codec == BodyCodec::Json {
        return Step::reply(crate::instrument_lists::instrument_lists_verb(dir, settings, venues));
    }
    match crate::instrument_lists::instrument_lists_msgpack(dir, settings, venues) {
        Ok(msgpack) => reply_body_of_msgpack(codec, &msgpack).map_or_else(
            || Step::reply(crate::instrument_lists::instrument_lists_verb(dir, settings, venues)),
            Step::Bytes,
        ),
        Err(response) => Step::Reply(response),
    }
}

/// A range read's rows answered as COLUMNS under `magic`: a plain page encoded in ONE pass straight into its frame
/// behind four reserved bytes, its rows dropped ([`Step::Columns`]), an LZ4 one built and compressed here ([`Step::Bytes`]) and answered plain when LZ4
/// cannot shrink it (the client reads either, and plain fits the frame by construction).
fn columns_step<R: ColumnRow>(
    rows: Vec<R>,
    magic: &'static ColumnsMagic,
    body: BarColumnsBody,
) -> Step {
    match body {
        BarColumnsBody::Plain => {
            let mut frame = vec![0u8; 4];
            encode_columns_into(&mut frame, magic, &rows);
            Step::Columns(frame)
        }
        BarColumnsBody::Lz4 => {
            let mut plain = Vec::new();
            encode_columns_into(&mut plain, magic, &rows);
            drop(rows);
            let packed = compress_columns(magic, &plain);
            Step::Bytes(if packed.len() < plain.len() { packed } else { plain })
        }
    }
}

/// A scan answered as COLUMNS: [`range_rows`] over the verb's read, fitted to the frame the columns
/// take ([`columns_row_len`]), then [`columns_step`]; a refusal is the JSON arm's refusal frame, id and words,
/// since both arms cut and refuse through [`range_rows`].
fn scan_columns<R: ColumnRow>(
    what: &RangeVerb,
    series: &str,
    range: TimestampRange,
    limit: Option<u32>,
    ceiling: usize,
    frame_bytes: usize,
    read: impl FnOnce(usize) -> Result<Vec<R>, vike_data::HistoryStoreError>,
    stored_rows: impl Fn(&[R]) -> usize,
    ts_of: impl Fn(&R) -> i64,
    (magic, body): (&'static ColumnsMagic, BarColumnsBody),
) -> Step {
    let frame = FrameBudget { bytes: frame_bytes, envelope: columns_empty_len::<R>() };
    match range_rows(
        what,
        series,
        range,
        limit,
        ceiling,
        frame,
        read,
        stored_rows,
        ts_of,
        columns_row_len,
    ) {
        Ok(rows) => columns_step(rows, magic, body),
        Err(refusal) => Step::reply(Response::Refused(refusal.into())),
    }
}

/// A store failure as a read answers it: the refusal frame of its kind (`DatahubRefusal`'s
/// `impl From<&HistoryStoreError>`), worded as the error's own `Display`.
pub(super) fn store_refused(error: &vike_data::HistoryStoreError) -> Response {
    Response::Refused(vike_datahub_client::refusals::DatahubRefusal::from(error).into())
}

/// Map one request to its response. Pure dispatch — the read verbs call the store method directly,
/// mapping `Ok` to the matching typed response and any [`vike_data::HistoryStoreError`] to
/// its refusal frame ([`store_refused`]), so a client always learns the outcome.
///
/// ⚠ Since ruling 7 this daemon serves the DATA plane only: a COMPUTE verb never reaches this match. It
/// does not decode into this daemon's [`Request`], and `handle_connection` answers it by name.
///
/// `probe` is the request's [`StopProbe`]; the `Backfill` and `ImportArchive` arms hand it on (the
/// `CancelBackfill` arm reads its peer address, for the log line, and asks it nothing).
fn handle_request(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    backfill: Option<&BackfillTable>,
    keyed: bool,
    market_data: Option<&MarketDataHub>,
    seed: Option<&SeedLane>,
    lanes: &ServerLanes,
    import: Option<&ImportLane>,
    facts: ConnectionLimits,
    probe: &StopProbe,
) -> Response {
    let ceilings = facts.ceilings;
    match request {
        Request::Ping => Response::Pong,
        // ---- every RANGE read: `LoadBars`, the two L1 scans, and the six of `docs/decisions/0084` --
        //
        // ⚠ **Each one goes through `range_verb`, and each one's store read is BUDGETED** — the
        // head of what the reply can carry, never the client's range. `range_verb`'s doc carries
        // the two arms and why every reply within the ceiling is byte-identical to the old one.
        //
        // ⚠ **TWO caps, and they do different jobs — neither is redundant.** The budgeted read is
        // what bounds the ALLOCATION (a whole-range depth scan is ~27 GB for the live store's worst
        // single day); `cap_to_whole_timestamp` inside `range_verb` then bounds the FRAME to the `limit`. A
        // store that inherits a trait DEFAULT narrows nothing, so the frame cap is the only one that
        // fires there — which is exactly why it stays.
        // `columns` and `resample_to` are `None` here: `dispatch` answers either before this match.
        Request::LoadBars { venue, symbol, interval, start, end, limit, source, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &LOAD_BARS,
                &format!("{venue}:{symbol}:{interval}"),
                range,
                limit,
                ceilings.bars,
                ceilings.frame_bytes,
                bars_head(&**store, &venue, &symbol, &interval, range, source.as_deref()),
                row_count,
                |r| r.timestamp,
                Response::Bars,
            )
        }
        // `columns` is `None` on every scan here: `dispatch` answers the columns first.
        Request::ScanQuotes { venue, symbol, start, end, limit, source, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_QUOTES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.quotes,
                ceilings.frame_bytes,
                |n| match source.as_deref() {
                    Some(lane) => {
                        store.scan_quotes_sourced_capped(&venue, lane, &symbol, range, Some(n))
                    }
                    None => store.scan_quotes_capped(&venue, &symbol, range, Some(n)),
                },
                row_count,
                |r| r.timestamp,
                Response::Quotes,
            )
        }
        Request::ScanTrades { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_TRADES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.trades,
                ceilings.frame_bytes,
                |n| store.scan_trades_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.timestamp,
                Response::Trades,
            )
        }
        //
        // ⚠ Plain `HistoryStore` TRAIT calls, exactly like the three above and like the store-metadata
        // verbs below — no `datafusion-serve` split, no mounted table, nothing runtime. A store
        // that does not hold the kind answers through its own error channel and rides
        // `store_refused` like any other store failure; this daemon does not translate that into
        // an empty success, because "the store holds none" and "this store cannot answer" are the
        // two facts 0084's whole reader argument turns on.
        Request::ScanBookUpdates { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_BOOK_UPDATES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.book_levels,
                ceilings.frame_bytes,
                |n| store.scan_book_updates_capped(&venue, &symbol, range, Some(n)),
                book_stored_rows,
                |r| r.timestamp,
                Response::BookUpdates,
            )
        }
        Request::ScanDepth { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_DEPTH,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.book_levels,
                ceilings.frame_bytes,
                |n| store.scan_depth_capped(&venue, &symbol, range, Some(n)),
                book_stored_rows,
                |r| r.timestamp,
                Response::Depth,
            )
        }
        // ⚠ `asset`, not `symbol` — the trait's own spelling for this one verb, carried through the
        // wire variant so the two cannot be transposed at either end.
        Request::ScanCohort { venue, asset, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_COHORT,
                &format!("{venue}:{asset}"),
                range,
                limit,
                ceilings.cohort,
                ceilings.frame_bytes,
                |n| store.scan_cohort_capped(&venue, &asset, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Cohort,
            )
        }
        Request::ScanPerpMetrics { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_PERP_METRICS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.perp_metrics,
                ceilings.frame_bytes,
                |n| store.scan_perp_metrics_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::PerpMetrics,
            )
        }
        // The text history: the range is over the RECEIVED time (the series' `ts`), so a range that
        // ends at a decision time is the point-in-time read.
        Request::ScanTexts { text_kind, collector, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_TEXTS,
                &format!("{text_kind}:{collector}"),
                range,
                limit,
                ceilings.texts,
                ceilings.frame_bytes,
                |n| store.scan_texts_capped(&text_kind, &collector, range, Some(n)),
                row_count,
                |r| r.received_ms,
                Response::Texts,
            )
        }
        Request::ScanChain { venue, underlying, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_CHAIN,
                &format!("{venue}:{underlying}"),
                range,
                limit,
                ceilings.chain,
                ceilings.frame_bytes,
                |n| store.scan_chain_capped(&venue, &underlying, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Chain,
            )
        }
        // The SEC tables: the range is over the KNOWABLE time (the series' `ts`), so a range that
        // ends at a decision time is the point-in-time read.
        Request::ScanFinancialFacts { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_FINANCIAL_FACTS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.financial_facts,
                ceilings.frame_bytes,
                |n| store.scan_financial_facts_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.filed_ms,
                Response::FinancialFacts,
            )
        }
        Request::ScanInstitutionalHoldings { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_INSTITUTIONAL_HOLDINGS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.institutional_holdings,
                ceilings.frame_bytes,
                |n| store.scan_institutional_holdings_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.filed_ms,
                Response::InstitutionalHoldings,
            )
        }
        // The text scores: the same clock as `ScanTexts` (the scored text's received time).
        Request::ScanTextScores { text_kind, model_name, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_TEXT_SCORES,
                &format!("{text_kind}:{model_name}"),
                range,
                limit,
                ceilings.text_scores,
                ceilings.frame_bytes,
                |n| store.scan_text_scores_capped(&text_kind, &model_name, range, Some(n)),
                row_count,
                |r| r.received_ms,
                Response::TextScores,
            )
        }
        // The live strategies' published forecasts on the data plane: `StrategyForecasts`' read and ceiling
        // below, on the Observe key, with the scans' soft `limit`. The range is over the DECISION time.
        Request::ScanForecasts { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_FORECASTS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.forecasts,
                ceilings.frame_bytes,
                |n| store.scan_forecasts_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Forecasts,
            )
        }
        Request::ScanEquity { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_EQUITY,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.equity,
                ceilings.frame_bytes,
                |n| store.scan_equity_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.timestamp,
                Response::Equity,
            )
        }
        // A WINDOWLESS request (every client older than `scan_execution_fills_range` sends one) is the
        // no-`limit` arm over `TimestampRange::all()`: the whole series, or refused by name above the
        // ceiling — the answer this verb always gave. A window and a `limit` page like every
        // other range verb.
        Request::ScanExecFills { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_EXECUTION_FILLS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.execution_fills,
                ceilings.frame_bytes,
                |n| store.scan_execution_fills_head(&venue, &symbol, range, n),
                row_count,
                |r| r.ts,
                Response::ExecFills,
            )
        }
        Request::ScanExecOrders { venue, symbol, start, end, limit, .. } => {
            let range = TimestampRange { start, end };
            range_verb(
                &SCAN_EXECUTION_ORDERS,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.execution_orders,
                ceilings.frame_bytes,
                |n| store.scan_execution_orders_head(&venue, &symbol, range, n),
                row_count,
                |r| r.ts,
                Response::ExecOrders,
            )
        }
        Request::PropertiesAsOf { venue, symbol, timestamp: ts } => {
            match store.properties_as_of(&venue, &symbol, ts) {
                // Box the payload — `Response::Properties` boxes `SymbolProperties` to keep the enum
                // small (see the proto); serde treats the box transparently on the wire.
                Ok(properties) => Response::Properties(properties.map(Box::new)),
                Err(e) => store_refused(&e),
            }
        }
        // Store-metadata verbs (PR-6). Unlike the `Run*` verbs these need NO `datafusion-serve` split:
        // they call `HistoryStore` TRAIT methods (the real manifest walk on a DataFusion backend, the
        // in-memory catalog fold on the `HistoryStoreMemory` double; a store WITHOUT the catalog verbs
        // refuses, and that refusal rides `store_refused` like any other store error rather than
        // being served as a fabricated empty catalog), so a lean build compiles + answers them
        // without pulling DataFusion. The catalog is tiny (no Parquet scan), so shipping it whole
        // is safe. `dispatch` answers it as the bytes this value writes (`super::series_list`).
        Request::ListSeries => match store.list_series() {
            Ok(series) => Response::SeriesList(series),
            Err(e) => store_refused(&e),
        },
        Request::Inventory => match store.inventory() {
            Ok(inventory) => Response::Inventory(inventory),
            Err(e) => store_refused(&e),
        },
        // ...and its execution-history window (the Tearsheet's catalog), folded from the same inventory.
        Request::ExecutionSeries { venue, symbol, start, end } => {
            let range = TimestampRange { start, end };
            match store.execution_series(venue.as_deref(), symbol.as_deref(), range) {
                Ok(answer) => Response::ExecutionSeries(answer),
                Err(e) => store_refused(&e),
            }
        }
        Request::SeriesFacts { id } => match store.series_facts(&id) {
            Ok(facts) => Response::SeriesFacts(Box::new(facts)),
            Err(e) => store_refused(&e),
        },
        // ...and a read's stamped days (the backtesthub's bar and tick cache), from the same manifests: a frame with no
        // `kind` is a bar read's, as every frame was before the tick kinds; the store refuses a read of no stamped
        // shape in words.
        Request::SeriesDays { venue, source, symbol, interval, start, end, kind } => {
            let range = TimestampRange { start, end };
            let kind = kind.unwrap_or(vike_model::StoreKindId::Bar);
            let (source, interval) = (source.as_deref(), interval.as_deref());
            match store.series_days(kind, &venue, source, &symbol, interval, range) {
                Ok(days) => Response::SeriesDays(days),
                Err(e) => store_refused(&e),
            }
        }
        // ...judged against the series' trading calendar, so a closed market's days are no gap.
        Request::SeriesGaps { id } => {
            series_gaps_verb(store.as_ref(), &id, lanes.instruments.as_deref())
        }
        // The cross-kind coverage report (split-plane spec §6 Q2) — the FOURTH store-metadata verb,
        // and served exactly like its three siblings above: a `HistoryStore` TRAIT call, so no
        // `datafusion-serve` split and no second store handle. That the trait carries it is the
        // whole reason this arm is one line: had the report stayed a concrete `HistoryStoreDataFusion`
        // method, serving it would have meant threading a SECOND, concrete store through `serve`
        // (the `BackfillTable` shape) purely to reach a manifest fold the trait can express.
        Request::Coverage => match store.coverage_report() {
            Ok(report) => Response::Coverage(report),
            Err(e) => store_refused(&e),
        },
        // Backfill-on-demand (split-plane REQ-9). The arm always DECODES (the schema lives in the
        // client crate) so a mismatched client is never dropped; whether it SERVES depends on the
        // mounted table — `None` (a default or plain `datafusion-serve` build) is a clean refusal
        // naming the missing feature.
        Request::Backfill { venue, symbol, interval, start, end } => {
            backfill_verb(&venue, &symbol, &interval, (start, end), backfill, store, probe)
        }
        // The operator's door onto RUNNING backfills — the registry the mounted table keeps. Both
        // decode on every build; a server with no table runs no backfill and answers the capability
        // refusal (`no_backfill_registry`). Neither asks this request's probe: they run nothing.
        Request::ListBackfills => match backfill {
            Some(table) => Response::RunningBackfills(table.running()),
            None => Response::Error(no_backfill_registry("ListBackfills")),
        },
        Request::CancelBackfill { venue, symbol, interval } => {
            cancel_backfill_verb(&venue, &symbol, &interval, backfill, probe.peer)
        }
        // The HISTORY-CHANNELS read (`docs/decisions/0102`). Served on EVERY build: whether a lane
        // is mounted travels inside the answer, so a server with no table answers too and says so
        // row by row. It asks the table a lookup and its credential probe a word, and calls no
        // collector — `crate::history`'s module doc carries what it must never do.
        Request::HistoryChannels => {
            // The one venue history channel a download serves is Dukascopy's archive; the SEC
            // downloads are no venue's.
            #[cfg(feature = "backfill-serve")]
            let archive_download = import.is_some_and(|lane| {
                lane.downloads_into(vike_data::archive_day::DUKASCOPY_BI5_FORMAT)
            });
            #[cfg(not(feature = "backfill-serve"))]
            let archive_download = false;
            crate::history::history_channels_verb(
                store,
                backfill,
                archive_download,
                vike_model::now_ms(),
            )
        }
        // The gatewayhub's state. Decodes on every build and never touches `store`.
        Request::GatewayStatus => gateway_status_verb(lanes.gateway_status.as_ref()),
        // The venue sessions' history, read from the settings store. Never touches `store`, the market-data one.
        Request::GatewayLinks { since_ms, route, venue } => gateway_links_verb(
            lanes.settings_store.as_ref(),
            since_ms,
            route.as_deref(),
            venue.as_deref(),
        ),
        // An operator's credential change, relayed to the gatewayhub (decision 0287). Never touches `store`; a
        // gatewayhub that is down, or no link at all, is an answer in words.
        Request::CredentialsChanged { account } => {
            credentials_changed_verb(lanes.credentials_relay.as_ref(), account)
        }
        // The ARCHIVE IMPORT (`docs/decisions/0100`). It DECODES on every build — the schema lives
        // in the client crate — so a client that sends it is answered rather than dropped; whether
        // it SERVES depends on the MOUNTED lane, and `None` answers the capability refusal having
        // touched nothing (`served_features` withholds `archive_import` for the same reason, which
        // is what a well-behaved client reads first). The request's stop probe goes with it: the
        // import asks it between DAYS, and a client that went away ends the request at that
        // boundary with a close, as a stopped `Backfill` does.
        Request::ImportArchive(spec) => {
            crate::import::import_archive_verb(&spec, import, &|| probe.should_stop())
        }
        // The ARCHIVE DOWNLOAD that feeds the import (the owner's ruling of 2026-11-13): decodes on
        // every build, served only by a lane that mounts a download; the stop probe is asked between
        // files.
        Request::FetchArchive(spec) => {
            crate::import::fetch_archive_verb(&spec, import, &|| probe.should_stop())
        }
        // The CHART-GAP SEED. Decodes on every build like its neighbour above; whether it FETCHES
        // depends on the ARMED lane, and an unarmed one answers a successful no-op rather than a
        // refusal — `seed_series_verb`'s own doc carries why those two differ here and nowhere else
        // on this wire.
        Request::SeedSeries { venue, symbol, interval, class } => {
            seed_series_verb(&venue, &symbol, &interval, class, backfill, seed, store)
        }
        // The VENUE CATALOG. Decodes on every build like both neighbours above, and — unlike either
        // — it never touches `store`, which is the whole of `docs/decisions/0062`'s decision 1 and
        // why this arm passes no store handle at all. An unarmed lane answers a successful
        // `NotArmed`; a venue this build cannot serve answers a successful `NotServed`. See
        // `venue_catalog_verb` for why so much of this verb's surface is SUCCESS rather than error.
        Request::VenueCatalog { venue } => venue_catalog_verb(&venue, lanes.catalog.as_deref()),
        // The OPTIONS CHAIN. Decodes on every build and, like the catalog, never touches `store`:
        // it answers from the mounted lane's in-memory snapshot, and an unarmed build answers a
        // successful `NotArmed`. `options_chain_verb` carries the gate order.
        Request::OptionsChain { venue, underlying, expiries } => {
            options_chain_verb(&venue, &underlying, &expiries, lanes.options.as_deref())
        }
        // The options LISTING's refresh: the lane's source asks the gatewayhub to read the venue's
        // listing again (the gatewayhub bounds how often), and answers what it lists now.
        Request::OptionsRefresh { venue } => options_refresh_verb(&venue, lanes.options.as_deref()),
        // The TOOL FEEDS. Decode on every build and, like the options chain, never touch `store`: they
        // answer from the mounted lane's in-memory caches, and an unarmed build answers a successful
        // `NotArmed`. A request names no URL, key or cadence — only an optional generation and, for
        // the equity calendar, a validated Monday — so no client input reaches a provider
        // (`equity_calendar_verb` carries the gate order).
        Request::NewsFeed { if_generation } => {
            news_feed_verb(if_generation, lanes.tool_feeds.as_deref())
        }
        Request::EconomicCalendar { if_generation } => {
            economic_calendar_verb(if_generation, lanes.tool_feeds.as_deref())
        }
        Request::EquityCalendar { week_start, if_generation } => {
            equity_calendar_verb(&week_start, if_generation, lanes.tool_feeds.as_deref())
        }
        // The DESTRUCTIVE verb. `keyed` is the FIRST thing it looks at — see `delete_series_verb`.
        Request::DeleteSeries { selector, produced_by, dry_run } => {
            delete_series_verb(&selector, produced_by.as_deref(), dry_run, keyed, store)
        }
        // ⚠ THE BELT TO `handle_connection`'s BRACES. `dispatch` intercepts `MarketDataSubscribe` before
        // this function is reached, so this arm is for any FUTURE caller that does not run the
        // connection loop's guard first. It must never mode-switch anything: this path has no socket to hand over.
        Request::MarketDataSubscribe { .. } => Response::Error(
            "MarketDataSubscribe is a connection MODE SWITCH and is handled by the connection loop; this \n             path cannot answer it"
                .into(),
        ),
        // The registry mutation. It runs on an ORDINARY short-lived connection; the FRAMES it
        // affects go to the stream connection that owns the session.
        Request::MarketDataUpdate { session, add, remove } => market_data_update_verb(session, &add, &remove, market_data),
        // The belt to `dispatch`'s braces for the strategy lane's mode switch, as `MarketDataSubscribe`'s
        // above: this path has no socket to hand over.
        Request::StrategyOpen { .. } => Response::Error(
            "StrategyOpen is a connection MODE SWITCH and is handled by the connection loop; this path \
             cannot answer it"
                .into(),
        ),
        // The belt to `dispatch`'s braces for the upload, as `StrategyOpen`'s above.
        Request::CsvUpload(_) => Response::Error(
            "CsvUpload is run by the connection loop; this path cannot answer it".into(),
        ),
        // A chunk or an end with no upload open on this connection: one that was refused, or ended.
        Request::CsvUploadChunk { .. } | Request::CsvUploadEnd => Response::Refused(
            vike_datahub_client::refusals::DatahubRefusal::UploadNotOpen.into(),
        ),
        // The strategy lane's first store read: the `interval=funding` series of one perpetual, under the bar
        // ceiling (`LoadBars`' read, the series fixed by the verb, so this verb reads no other series).
        Request::StrategyFundingHistory { venue, symbol, start, end } => {
            let range = TimestampRange { start: Some(start), end: Some(end) };
            let interval = vike_data::source::FUNDING_INTERVAL;
            range_verb(
                &STRATEGY_FUNDING_HISTORY,
                &format!("{venue}:{symbol}:{interval}"),
                range,
                None,
                ceilings.bars,
                ceilings.frame_bytes,
                |n| store.load_bars_head(&venue, &symbol, interval, range, n),
                row_count,
                |r| r.timestamp,
                Response::Bars,
            )
        }
        // ...and its second: one bar series a live strategy's input names (decision 0271's live half), the
        // venue's own candles or one `source=` lane, EXACTLY `LoadBars`' read under the bar ceiling.
        // (`dispatch` answers the columns read; this is the JSON one.)
        Request::StrategyInputBars { venue, symbol, interval, source, start, end, .. } => {
            let range = TimestampRange { start: Some(start), end: Some(end) };
            range_verb(
                &STRATEGY_INPUT_BARS,
                &format!("{venue}:{symbol}:{interval}"),
                range,
                None,
                ceilings.bars,
                ceilings.frame_bytes,
                bars_head(&**store, &venue, &symbol, &interval, range, source.as_deref()),
                row_count,
                |r| r.timestamp,
                Response::Bars,
            )
        }
        // ...and a paper account's symbol facts (decision 0285's amendment of 2026-11-16): the store's row, else one
        // keyless catalog search on this datahub's own Read key.
        Request::StrategySymbolFacts { venue, symbol } => super::strat::strategy_symbol_facts_verb(
            &venue,
            &symbol,
            keyed,
            store.as_ref(),
            lanes.keyless_lookup.as_ref(),
            lanes.instruments.as_deref(),
        ),
        // ...and its third: the forecasts the live strategies published (`kind=exec_forecast`, book W9), which the
        // tradehub's model drift rule reads back. The range is over the DECISION time, so one ending at a
        // decision time is the point-in-time read.
        Request::StrategyForecasts { venue, symbol, start, end, .. } => {
            let range = TimestampRange { start: Some(start), end: Some(end) };
            range_verb(
                &STRATEGY_FORECASTS,
                &format!("{venue}:{symbol}"),
                range,
                None,
                ceilings.forecasts,
                ceilings.frame_bytes,
                |n| store.scan_forecasts_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Forecasts,
            )
        }
        // The user's symbol lists, one table of the settings store (`symbol_sets`' module doc). Never touch `store`.
        Request::SymbolSets => symbol_sets_verb(lanes.settings_store.as_ref()),
        Request::SetSymbolSet { set } => set_symbol_set_verb(lanes.settings_store.as_ref(), &set),
        Request::DeleteSymbolSet { name } => {
            delete_symbol_set_verb(lanes.settings_store.as_ref(), &name)
        }
        // The venues' KEPT instrument lists (`crate::instrument_lists`): the read, and refresh-now through the catalog
        // lane's own gates. Neither writes `store`; the refresh LISTS its series, whose share splits it fills from
        // Yahoo (`crate::stock_splits`, the one caller: no timer fills them). `dispatch` answers the two reads as the
        // bytes these values write (`instrument_lists_step`).
        Request::InstrumentLists => crate::instrument_lists::instrument_lists_verb(
            lanes.instruments.as_deref(),
            lanes.settings_store.as_ref(),
            None,
        ),
        Request::InstrumentListsOfVenues { venues } => {
            crate::instrument_lists::instrument_lists_verb(
                lanes.instruments.as_deref(),
                lanes.settings_store.as_ref(),
                Some(venues.as_slice()),
            )
        }
        Request::RefreshInstrumentLists { venue } => crate::stock_splits::with_yahoo_splits(
            crate::instrument_lists::refresh_instrument_lists_verb(
                lanes.instruments.as_deref(),
                lanes.settings_store.as_ref(),
                lanes.catalog.as_deref(),
                venue.as_deref(),
            ),
            store.as_ref(),
            lanes.settings_store.as_ref(),
            lanes.instruments.as_deref(),
        ),
        // The open split warnings (`crate::split_watch`): the watch's memory, less what a split row covers now. Never
        // touches `store`, writes nothing.
        Request::SplitWarnings => Response::SplitWarnings(
            lanes.split_watch.as_deref().map_or_else(Vec::new, crate::split_watch::SplitWatch::open),
        ),
        // Every journal's execution-stream cursor, its gaps, and whether a tradehub streams it now: rows of the
        // settings store. Never touch `store`.
        Request::ExecutionStreams => {
            crate::execution_stream::execution_streams_verb(lanes.execution_stream.as_deref())
        }
        // Every account's latest position snapshot at or before a time: rows of the settings store. Never touch
        // `store`.
        Request::ExecutionAccountPositions { at_or_before_ms } => {
            crate::execution_stream::account_positions_verb(
                lanes.execution_stream.as_deref(),
                at_or_before_ms,
            )
        }
        // The belt to `dispatch`'s braces for the binary kept rows, as `StrategyOpen`'s above.
        Request::KeptRowsOfVenues { .. } => Response::Error(
            "KeptRowsOfVenues is answered in binary by the connection loop; this path cannot answer it".into(),
        ),
        // The belt to `dispatch`'s braces for the execution stream's open, as `StrategyOpen`'s above.
        Request::ExecutionStreamOpen { .. } => Response::Error(
            "ExecutionStreamOpen is run by the connection loop; this path cannot answer it".into(),
        ),
        // The live tokens of the rolling families the recorder records: its memory. Never touch `store`.
        Request::LiveFamilyTokens => {
            crate::live_family_tokens::live_family_tokens_verb(lanes.live_family_tokens.as_ref())
        }
        // The instruments the operator adds by hand: one table of the settings store, folded into the kept lists.
        // Never touch `store`.
        Request::OperatorInstruments { venue, add, remove, stated } => {
            super::operator_instruments::operator_instruments_verb(
                &super::operator_instruments::OperatorInstrumentsDoors {
                    instruments: lanes.instruments.as_deref(),
                    settings: lanes.settings_store.as_ref(),
                    lookup: lanes.instrument_lookup.as_ref(),
                },
                &venue,
                &add,
                &remove,
                stated.as_ref(),
            )
        }
    }
}
