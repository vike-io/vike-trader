//! The router: `dispatch` splits out the mode switch and the close, `handle_request` maps every other verb.

use std::sync::Arc;

use vike_data::{HistStore, TsRange};
use vike_datahub_client::PROTO_VERSION;
use vike_datahub_client::proto::{Request, Response};

use crate::backfill::BackfillTable;
use crate::catalog::CatalogLane;
use crate::import::ImportLane;
use crate::md::MdHub;
use crate::seed::SeedLane;

use super::backfill::{backfill_verb, cancel_backfill_verb, no_backfill_registry};
use super::delete::delete_series_verb;
use super::features::served_features;
use super::limits::ReadCeilings;
use super::market_data::{md_update_verb, refuse_an_oversized_spec_list};
use super::range_reads::{
    LOAD_BARS, SCAN_BOOK_UPDATES, SCAN_COHORT, SCAN_DEPTH, SCAN_EQUITY, SCAN_PERP_METRICS,
    SCAN_QUOTES, SCAN_TRADES, book_stored_rows, exec_fills_verb, range_verb, row_count,
};
use super::seed_series::seed_series_verb;
use super::step::{Step, StopProbe, compute_verb_moved};
use super::venue_catalog::venue_catalog_verb;

/// The refusal a build with NO market-data plane answers [`Request::MdSubscribe`] with.
///
/// ⚠ **It is a [`Response::Error`] and the connection SURVIVES POSITIONALLY — it is deliberately
/// NOT an all-refused `MdSubscribed`.** That is why
/// `vike_datahub_client::market::MdRefusal` carries no `HubNotMounted` variant, and it is a
/// considered deviation from §4.4: "no hub" is a whole-REQUEST condition, uniform across every spec,
/// so expressing it as a PER-SPEC refusal would mode-switch a hub-less build into a heartbeat-only
/// writer that will never send a frame — worse than the refusal it replaces. This shape is
/// byte-for-byte what [`backfill_verb`] already answers for an unmounted collector table, and what
/// leg (3) of every `FEATURE_*` doc in the proto promises.
///
/// A `const` because two things must be able to name it: this refusal, and the test that proves it
/// is the same server refusing on the path a client actually takes.
/// ⚠ **IT LEADS WITH THE ENVIRONMENT VARIABLE, AND IT USED TO LEAD WITH A REBUILD THAT CANNOT BE
/// THE FIX.** The plane is armed by an OPERATOR, not by a build: `crate::datahub_cli`'s hub mount
/// carries no `#[cfg]` at all (that is the whole point of the feature-free hub), so `md.is_none()`
/// here holds if and only if `VIKE_DATAHUB_LIVE != "1"`. And `live-feeds` is an umbrella that gates
/// no code — `git grep 'feature = "live-feeds"'` finds only doc comments — so
/// `--features live-feeds` changes not one byte of the binary. Sending an operator to recompile
/// when the sole cause is an unset variable is the worst kind of accurate-sounding message. The
/// per-venue features govern a DIFFERENT refusal, `MdRefusal::VenueNotServed` /
/// `crates/vike-datahub/src/md/venues.rs`'s `missing_feature`, which is only reachable once this
/// one is not.
pub const NO_MARKET_DATA_PLANE: &str = "this datahub serves no market-data plane. It is armed by an OPERATOR, not by a build: set \
     VIKE_DATAHUB_LIVE=1 on the server and restart it. (A rebuild is a SEPARATE question and only \
     afterwards — `--features live-feeds` alone gates no code, and it is the per-venue \
     `live-<venue>` features, which imply it, that link a venue's feed; a venue this build did not \
     link is then refused BY NAME.) Nothing was subscribed and this connection is unchanged: every \
     other verb still works on it.";

/// Route one decoded request, splitting the MODE SWITCH — and the CLOSE — out from everything else.
///
/// The `md.is_some()` test is what decides between the switch and [`NO_MARKET_DATA_PLANE`], and it
/// is a RUNTIME fact rather than a cfg — the same rule `FEATURE_MARKET_DATA`'s advertisement
/// follows, so "compiled with `live-feeds`" and "armed by an operator" cannot answer differently.
///
/// The close is [`Step::Close`]: a request whose `probe` saw its client gone is answered by closing
/// the connection, never by a write. The check reads the probe's latch and does not peek, so a
/// request that never asked the probe — every verb but `Backfill` and `ImportArchive` — cannot
/// reach it.
#[allow(clippy::too_many_arguments)] // `handle_request`'s ten, which it routes
pub(super) fn dispatch(
    request: Request,
    store: &Arc<dyn HistStore + Send + Sync>,
    backfill: Option<&BackfillTable>,
    keyed: bool,
    md: Option<&Arc<MdHub>>,
    seed: Option<&SeedLane>,
    catalog: Option<&CatalogLane>,
    import: Option<&ImportLane>,
    ceilings: ReadCeilings,
    probe: &StopProbe,
) -> Step {
    match request {
        Request::MdSubscribe { specs } => match md {
            // ⚠ ALL THREE whole-REQUEST refusals answer the same way and on the same loop: no hub,
            // an over-length spec list, and no stream-connection slot. See [`Step::ModeSwitch`] for
            // why the session is opened here rather than inside the writer.
            //
            // The ORDER is the `delete_series_verb` idiom: the refusal that is about the SERVER
            // rather than about the request comes first, then the request's own cheapest check,
            // then the one that reserves something.
            Some(hub) => match refuse_an_oversized_spec_list("MdSubscribe", "specs", specs.len()) {
                Some(why) => Step::reply(Response::Error(why)),
                None => match hub.open_session() {
                    Ok(guard) => Step::ModeSwitch(guard, specs),
                    Err(msg) => Step::reply(Response::Error(msg)),
                },
            },
            None => Step::reply(Response::Error(NO_MARKET_DATA_PLANE.to_string())),
        },
        other => {
            let response = handle_request(
                other,
                store,
                backfill,
                keyed,
                md.map(|h| &**h),
                seed,
                catalog,
                import,
                ceilings,
                probe,
            );
            if probe.peer_gone() { Step::Close } else { Step::reply(response) }
        }
    }
}

/// Map one request to its response. Pure dispatch — the read verbs call the store method directly,
/// mapping `Ok` to the matching typed response and any [`vike_data::DataError`] to
/// [`Response::Error`], so a client always learns the outcome.
///
/// ⚠ Since ruling 7 this daemon serves the DATA plane only: the seven COMPUTE verbs still decode
/// (one schema, two daemons) and are answered by [`compute_verb_moved`]. `handle_connection`
/// refuses them one step earlier, before the scope check, so these arms are the belt to that
/// braces — reached by any future caller of this function that does not run the connection loop's
/// guard first.
///
/// `probe` is the request's [`StopProbe`]; the `Backfill` and `ImportArchive` arms hand it on (the
/// `CancelBackfill` arm reads its peer address, for the log line, and asks it nothing).
#[allow(clippy::too_many_arguments)] // seven it always took, the read ceilings, the import lane, the stop probe
fn handle_request(
    request: Request,
    store: &Arc<dyn HistStore + Send + Sync>,
    backfill: Option<&BackfillTable>,
    keyed: bool,
    md: Option<&MdHub>,
    seed: Option<&SeedLane>,
    catalog: Option<&CatalogLane>,
    import: Option<&ImportLane>,
    ceilings: ReadCeilings,
    probe: &StopProbe,
) -> Response {
    match request {
        // The OPTIONAL version handshake (PR-2): answer with THIS server's `PROTO_VERSION` and the
        // verbs it serves. It is not a gate — a client may skip it and send a normal request; the
        // CLIENT compares the version and fails loudly on a mismatch (see `DatahubClient::connect`).
        Request::Hello { proto_version: client_version } => {
            tracing::debug!(client_version, "vike-datahub: hello handshake");
            Response::Welcome {
                proto_version: PROTO_VERSION,
                // `false`: this arm is only reached on a KEY-LESS server (a keyed one answers
                // `Hello` inside `run_handshake` and never returns here), so `FEATURE_AUTH` is
                // never advertised from it and `nonce` is `None` — which is exactly what makes the
                // key-less `Welcome` byte-identical to the pre-auth protocol's.
                features: served_features(
                    backfill.is_some(),
                    backfill.is_some_and(BackfillTable::has_funding),
                    false,
                    md,
                    seed.is_some(),
                    catalog.is_some(),
                    import,
                ),
                nonce: None,
            }
        }
        // Only reachable on a KEY-LESS server (a keyed one consumes `Auth` in `run_handshake`).
        // There is nothing to verify it against, so say so rather than pretending: a client that
        // signed a mac deserves to learn its key was never checked, not to be told "ok".
        Request::Auth { .. } => Response::AuthDenied {
            reason: "this datahub has no node keys configured and authenticates nothing; \
                     connect without Auth"
                .into(),
        },
        Request::Ping => Response::Pong,
        // ⚠ THE SEVEN COMPUTE VERBS ARE NO LONGER SERVED HERE — ruling 7 of
        // `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`. They moved to
        // `vike-backend backtest --addr` (`vike_backtest::compute_server`), and these arms are the
        // half of that split which faces a client that has not moved with them.
        //
        // They still DECODE — the `Request` schema lives in the shared client crate and always
        // will, because both daemons speak ONE protocol — so a client that sends one here gets a
        // named `Response::Error` and KEEPS ITS CONNECTION, per this protocol's decode-vs-drop
        // contract. It is a bad *request*, not a bad *connection*: the same peer's `LoadBars` on
        // the next frame still works.
        //
        // ⚠ SEVEN ARMS RATHER THAN ONE `other =>` CATCH-ALL, deliberately, and for the reason
        // `plane_of`'s own match spells out: a catch-all would silently swallow the NEXT verb
        // somebody adds, answering "that moved to the backtest daemon" about a verb that moved
        // nowhere. Written out, a new variant fails to compile here until its author classifies it.
        Request::RunBacktest(_) => compute_verb_moved("RunBacktest"),
        Request::RunSlice { .. } => compute_verb_moved("RunSlice"),
        Request::RunParamscan { .. } => compute_verb_moved("RunSweep"),
        Request::RunWalkforward { .. } => compute_verb_moved("RunWalkforward"),
        Request::RunParamscanProfile { .. } => compute_verb_moved("RunSweepProfile"),
        Request::RunWalkforwardProfile { .. } => compute_verb_moved("RunWalkforwardProfile"),
        // Ruling R1's research-plane verb — the EIGHTH compute verb, refused by name like the
        // seven ruling 7 moved. This daemon will never serve it: a study RUNS an engine.
        Request::RunStudy(_) => compute_verb_moved("RunStudy"),
        Request::ListStrategies => compute_verb_moved("ListStrategies"),
        // ...and the NAMED RUN pair (`docs/decisions/0064-a-named-run-carries-no-source.md`), the
        // NINTH and TENTH compute verbs. ⚠ Their SCOPE is Observe, which is unlike every other
        // `Run*` verb and is what the record turns on — but scope is not plane, and this daemon
        // holds no strategy roster and no engine to answer them with.
        Request::RunNamed(_) => compute_verb_moved("RunNamed"),
        Request::NamedStrategies => compute_verb_moved("NamedStrategies"),
        // ---- every RANGE read: `LoadBars`, the two L1 scans, and the six of `docs/decisions/0084` --
        //
        // ⚠ **Each one goes through `range_verb`, and each one's store read is BUDGETED** — the
        // head of what the reply can carry, never the client's range. `range_verb`'s doc carries
        // the two arms and why every reply within the ceiling is byte-identical to the old one.
        //
        // ⚠ **TWO caps, and they do different jobs — neither is redundant.** The budgeted read is
        // what bounds the ALLOCATION (a whole-range depth scan is ~27 GB for the live store's worst
        // single day); `cap_to_whole_ts` inside `range_verb` then bounds the FRAME to the `limit`. A
        // store that inherits a trait DEFAULT narrows nothing, so the frame cap is the only one that
        // fires there — which is exactly why it stays.
        Request::LoadBars { venue, symbol, interval, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &LOAD_BARS,
                &format!("{venue}:{symbol}:{interval}"),
                range,
                limit,
                ceilings.bars,
                ceilings.frame_bytes,
                |n| store.load_bars_head(&venue, &symbol, &interval, range, n),
                row_count,
                |r| r.ts,
                Response::Bars,
            )
        }
        Request::ScanQuotes { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_QUOTES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.quotes,
                ceilings.frame_bytes,
                |n| store.scan_quotes_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Quotes,
            )
        }
        Request::ScanTrades { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_TRADES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.trades,
                ceilings.frame_bytes,
                |n| store.scan_trades_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Trades,
            )
        }
        //
        // ⚠ Plain `HistStore` TRAIT calls, exactly like the three above and like the store-metadata
        // verbs below — no `serve-datafusion` split, no mounted table, nothing runtime. A store
        // that does not hold the kind answers through its own error channel and rides
        // `Response::Error` like any other store failure; this daemon does not translate that into
        // an empty success, because "the store holds none" and "this store cannot answer" are the
        // two facts 0084's whole reader argument turns on.
        Request::ScanBookUpdates { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_BOOK_UPDATES,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.book_levels,
                ceilings.frame_bytes,
                |n| store.scan_book_updates_capped(&venue, &symbol, range, Some(n)),
                book_stored_rows,
                |r| r.ts,
                Response::BookUpdates,
            )
        }
        Request::ScanDepth { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_DEPTH,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.book_levels,
                ceilings.frame_bytes,
                |n| store.scan_depth_capped(&venue, &symbol, range, Some(n)),
                book_stored_rows,
                |r| r.ts,
                Response::Depth,
            )
        }
        // ⚠ `asset`, not `symbol` — the trait's own spelling for this one verb, carried through the
        // wire variant so the two cannot be transposed at either end.
        Request::ScanCohort { venue, asset, start, end, limit } => {
            let range = TsRange { start, end };
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
        Request::ScanPerpMetrics { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
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
        Request::ScanEquity { venue, symbol, start, end, limit } => {
            let range = TsRange { start, end };
            range_verb(
                &SCAN_EQUITY,
                &format!("{venue}:{symbol}"),
                range,
                limit,
                ceilings.equity,
                ceilings.frame_bytes,
                |n| store.scan_equity_capped(&venue, &symbol, range, Some(n)),
                row_count,
                |r| r.ts,
                Response::Equity,
            )
        }
        // ⚠ No range: `HistStore::scan_exec_fills` takes none, and the variant carries none — so
        // this verb has only the no-`limit` arm, read as a head of `ceiling + 1`.
        Request::ScanExecFills { venue, symbol } => {
            exec_fills_verb(store, &venue, &symbol, ceilings.exec_fills, ceilings.frame_bytes)
        }
        Request::PropertiesAsOf { venue, symbol, ts } => {
            match store.properties_as_of(&venue, &symbol, ts) {
                // Box the payload — `Response::Properties` boxes `SymbolProperties` to keep the enum
                // small (see the proto); serde treats the box transparently on the wire.
                Ok(props) => Response::Properties(props.map(Box::new)),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        // Store-metadata verbs (PR-6). Unlike the `Run*` verbs these need NO `serve-datafusion` split:
        // they call `HistStore` TRAIT methods (the real manifest walk on a DataFusion backend, the
        // in-memory catalog fold on the `MemHistStore` double; a store WITHOUT the catalog verbs
        // refuses, and that refusal rides `Response::Error` like any other store error rather than
        // being served as a fabricated empty catalog), so a lean build compiles + answers them
        // without pulling DataFusion. The catalog is tiny (no Parquet scan), so shipping it whole
        // is safe.
        Request::ListSeries => match store.list_series() {
            Ok(series) => Response::SeriesList(series),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Inventory => match store.inventory() {
            Ok(inv) => Response::Inventory(inv),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::SeriesFacts { id } => match store.series_facts(&id) {
            Ok(facts) => Response::SeriesFacts(Box::new(facts)),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::SeriesGaps { id } => match store.series_gaps(&id) {
            Ok(gaps) => Response::SeriesGaps(gaps),
            Err(e) => Response::Error(e.to_string()),
        },
        // The cross-kind coverage report (split-plane spec §6 Q2) — the FOURTH store-metadata verb,
        // and served exactly like its three siblings above: a `HistStore` TRAIT call, so no
        // `serve-datafusion` split and no second store handle. That the trait carries it is the
        // whole reason this arm is one line: had the report stayed a concrete `DataFusionHist`
        // method, serving it would have meant threading a SECOND, concrete store through `serve`
        // (the `BackfillTable` shape) purely to reach a manifest fold the trait can express.
        Request::Coverage => match store.coverage_report() {
            Ok(report) => Response::Coverage(report),
            Err(e) => Response::Error(e.to_string()),
        },
        // Backfill-on-demand (split-plane REQ-9). The arm always DECODES (the schema lives in the
        // client crate) so a mismatched client is never dropped; whether it SERVES depends on the
        // mounted table — `None` (a default or plain `serve-datafusion` build) is a clean refusal
        // naming the missing feature, the recorder's `missing_feature` idiom.
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
            crate::history::history_channels_verb(store, backfill, vike_model::now_ms())
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
        Request::VenueCatalog { venue } => venue_catalog_verb(&venue, catalog),
        // The DESTRUCTIVE verb. `keyed` is the FIRST thing it looks at — see `delete_series_verb`.
        Request::DeleteSeries { selector, produced_by, dry_run } => {
            delete_series_verb(&selector, produced_by.as_deref(), dry_run, keyed, store)
        }
        // ⚠ THE BELT TO `handle_connection`'s BRACES. `dispatch` intercepts `MdSubscribe` before
        // this function is reached, so this arm is for any FUTURE caller that does not run the
        // connection loop's guard first — the same reasoning the `compute_verb_moved` arms above
        // already carry. It must never mode-switch anything: this path has no socket to hand over.
        Request::MdSubscribe { .. } => Response::Error(
            "MdSubscribe is a connection MODE SWITCH and is handled by the connection loop; this \n             path cannot answer it"
                .into(),
        ),
        // The registry mutation. It runs on an ORDINARY short-lived connection; the FRAMES it
        // affects go to the stream connection that owns the session.
        Request::MdUpdate { session, add, remove } => md_update_verb(session, &add, &remove, md),
    }
}
