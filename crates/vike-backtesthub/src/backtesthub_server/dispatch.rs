//! Request dispatch: one request to its response, and the advertised features.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_backtesthub_client::proto::{
    FEATURE_ATTACH_FUNDING, FEATURE_BUILD_STRATEGY_FILE, FEATURE_LIST_STRATEGIES,
    FEATURE_NAMED_RUN, FEATURE_PARAMSCAN_PROFILE_SUMMARY, FEATURE_RHAI_PARAMETERS,
    FEATURE_RUN_CURVE, FEATURE_RUN_FILES, FEATURE_RUN_FROM_SAVED, FEATURE_RUN_PARAMSCAN,
    FEATURE_RUN_PARAMSCAN_PROFILE, FEATURE_RUN_SUMMARY, FEATURE_RUN_WALKFORWARD,
    FEATURE_RUN_WALKFORWARD_PROFILE, FEATURE_SDK_SOURCE, FEATURE_SEARCH_METHOD,
    FEATURE_SLIPPAGE_MODEL, FEATURE_STRATEGY_FILES, FEATURE_STRATEGY_SDK, FEATURE_STUDY,
    FEATURE_STUDY_FILES, FEATURE_WALKFORWARD_SEARCH, Request, Response, STUDIO_RUNNER_SENTINEL,
};
use vike_backtesthub_client::summary::{ParamscanProfileSummaryWire, ParamscanRowSummaryWire};
use vike_data::HistoryStore;
use vike_hub_protocol::auth::FEATURE_AUTH;
use vike_hub_protocol::features::FEATURE_WIRE_MSGPACK;
use vike_hub_protocol::plane::BACKTEST_PLANE_SENTINEL;
use vike_model::Clock;

use super::StudyDirs;
use super::saved_run::{ParamscanSave, StudioRun};
use super::verbs::{
    run_backtest, run_backtest_whole, run_paramscan_profile, run_walkforward_profile,
};
use crate::named_run::NamedRunLane;
use crate::run_summary::WholeRecords;
use crate::strategy_sandbox::RustRun;
use crate::strategy_sandbox::witness::ReadWitnesses;

/// Map one request to its response: a request whose strategy is a Rust strategy FILE runs in a sandboxed
/// child ([`crate::strategy_sandbox::run_in_child`]: this process loads no strategy file), every other one
/// here ([`handle_request_here`]).
///
/// `client_gone` is the connection's peer-gone flag (`None`: nobody is watching). A sandboxed run reads
/// it to KILL its child (`super::connection`'s `is_watched` arms it for them).
///
/// Every backtest (book W35, the owner's word of 2026-11-16: a profile verb's run and each of the Studio's three)
/// answered by EITHER route leaves a run under `study.runs_root` (`super::saved_run`, as a study's run is saved) when
/// `study.save_backtest_runs` says this process saves (a serving daemon); a data plan saves nothing. After a request
/// that may save, `study.run_retention` (when set) comes back beside the answer, and the connection prunes the runs
/// root with it once the answer is WRITTEN (`super::saved_run::prune`, which reads every saved run's manifest): no
/// answer waits for it.
///
/// ⚠ **An IDENTICAL request is answered from the run it saved** (decision 0078's amendment of 2026-11-20,
/// `super::answer_from_saved`): keyed before it runs, answered at once from a saved run whose recorded reads still hold,
/// saving nothing; else run as here, its relay recording each read, and saved with its key. `Request::Fresh` runs its
/// request in any case, and keys nothing.
pub(crate) fn handle_request<'a>(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &'a StudyDirs,
    named_run: NamedRunLane,
    client_gone: Option<&AtomicBool>,
) -> (Response, Option<&'a vike_runs::RunRetention>) {
    let clock = vike_model::ClockLive;
    let (request, fresh) = match request {
        Request::Fresh(inner) => (*inner, true),
        request => (request, false),
    };
    // A `.rs` file is read, built and its model texts read ONCE, for its key and for its run alike.
    let rust = crate::strategy_sandbox::resolve(&request, study);
    let reusable = match rust.as_ref().map(|runs| runs.as_deref()) {
        // Only a run of ONE file is keyed: a portfolio's several are never answered from a saved run.
        Some(Ok([run])) => {
            super::answer_from_saved::reusable(&request, study, run, clock.now_ms(), fresh)
        }
        _ => None,
    };
    if let Some(reusable) = reusable.as_ref()
        && let Some(answer) = super::answer_from_saved::answer(reusable, &study.runs_root, store)
    {
        return (answer, None);
    }
    let witness = reusable.as_ref().map(|reusable| &reusable.reads);
    let kept = |response: &Response| reusable.as_ref().and_then(|reusable| reusable.kept(response));
    let mut saved =
        study.save_backtest_runs.then(|| super::saved_run::start(&request, &clock)).flatten();
    // A summarized PROFILE search (decision 0078's amendment of 2026-11-19) saves its rows as they arrive, never its
    // whole report at once (`super::saved_run::ParamscanSave`), asking the free-space floor as it writes: a search's
    // report is gigabytes, never one check's worth.
    if is_paramscan_profile_summary(&request) {
        let may_have_saved = saved.is_some();
        let room = study.disk_floor.map(|floor| {
            Box::new(move |dir: &std::path::Path| floor.refusal(dir))
                as super::saved_run::RoomCheck<'_>
        });
        let mut save = ParamscanSave::new(saved, &study.runs_root, room);
        let response =
            answer_paramscan_summary(request, store, study, client_gone, &mut save, rust, witness);
        save.keep(kept(&response));
        let response = save.finish(response, &clock);
        return (response, study.run_retention.as_ref().filter(|_| may_have_saved));
    }
    let (response, studio_run) =
        answer_leaving_run(request, store, study, named_run, client_gone, rust, witness);
    // THE one point a single backtest's trade ledger comes off its answer, saving on or off: no client sees it.
    let (response, trade_ledger) = super::saved_run::take_trade_ledger(response);
    if let Some(start) = saved.as_mut() {
        start.kept = kept(&response);
    }
    let may_have_saved = saved.is_some();
    // The free-space floor, asked only for an answer that may be saved: below it the answer goes back unsaved.
    let no_room =
        study.disk_floor.filter(|_| may_have_saved).and_then(|f| f.refusal(&study.runs_root));
    let response = match studio_run {
        // A Studio run (or a summarized `RunBacktest`) is saved as the profile run it left beside its answer, which goes
        // back as it came, a single run's summary naming the saved run, which keeps its whole curve (decision 0078's
        // amendment of 2026-11-18), or a bar run's event log in its place (its amendment of 2026-11-19).
        Some(mut run) => {
            let (curve, logged) = match &response {
                Response::RunSummary(summary) if may_have_saved => (
                    crate::run_summary::held_curve(&summary.run),
                    crate::run_summary::held_log(&summary.run),
                ),
                _ => (None, None),
            };
            // A log this process ran beside its held curve records the store's day stamps of the bars the run just
            // read, and this daemon's build; bars the store cannot pin (no stamps, a provisional day) keep the curve
            // instead (`equity_log`'s module doc). A `.rs` file's run held as its log (`logged`) was pinned in its
            // child BEFORE it chose to send no curve (`crate::strategy_sandbox`'s `pinned_log`): it is never dropped.
            let pinned = match (&curve, run.equity_log.as_mut()) {
                (Some(_), Some(log)) => {
                    let build = study.build.as_deref();
                    vike_backtest::run::equity_log::stamp(&mut log.recipe, store.as_ref(), build)
                }
                _ => true,
            };
            if !pinned {
                run.equity_log = None;
            }
            let saved_run = super::saved_run::save_studio(
                run,
                curve.as_deref(),
                logged.as_deref().map(|held| &held.log),
                saved,
                &study.runs_root,
                &clock,
                no_room,
            );
            super::saved_run::with_saved_run(response, saved_run)
        }
        None => {
            super::saved_run::save(response, trade_ledger, saved, &study.runs_root, &clock, no_room)
        }
    };
    (response, study.run_retention.as_ref().filter(|_| may_have_saved))
}

/// Whether `request` is a summarized PROFILE search: `Request::Summarized` around `Request::RunParamscanProfile`.
pub(crate) fn is_paramscan_profile_summary(request: &Request) -> bool {
    matches!(request, Request::Summarized(inner) if matches!(**inner, Request::RunParamscanProfile { .. }))
}

/// **A summarized profile search's answer**, its rows handed to `save` as they arrive: in a sandboxed child for a `.rs`
/// strategy file (`crate::strategy_sandbox::run_paramscan_summary_in_child`, its rows streamed up one a frame), in this
/// process for every other one ([`paramscan_summary_here`]). `rust` is the request's `.rs` file as
/// `crate::strategy_sandbox::resolve` resolved it.
fn answer_paramscan_summary(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    client_gone: Option<&AtomicBool>,
    save: &mut ParamscanSave<'_>,
    rust: Option<Result<Vec<RustRun>, String>>,
    witness: Option<&ReadWitnesses>,
) -> Response {
    match rust {
        None => paramscan_summary_here(request, store, study, client_gone, save),
        Some(Err(why)) => Response::Error(vike_studio_core::RunError::Strategy(why).wire_line()),
        Some(Ok(runs)) => crate::strategy_sandbox::run_paramscan_summary_in_child(
            &request,
            &runs,
            store,
            client_gone,
            save,
            witness,
        ),
    }
}

/// [`answer_paramscan_summary`] in THIS process: the search's rows summarized ([`paramscan_summary_parts`]), each
/// row's whole JSON written by `save`, the summaries held here for `Request::ReadParamscanRows`.
fn paramscan_summary_here(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    client_gone: Option<&AtomicBool>,
    save: &mut ParamscanSave<'_>,
) -> Response {
    let mut rows = Vec::new();
    let parts = paramscan_summary_parts(request, store, study, client_gone, |row, json| {
        save.row(Some(&json));
        rows.push(row);
        Ok(())
    });
    match parts {
        Ok((summary, tail)) => {
            save.tail(tail);
            crate::run_summary::hold_paramscan(summary, rows)
        }
        Err(answer) => *answer,
    }
}

/// **A summarized profile search run here** (in this process, or in a `.rs` file's sandboxed child,
/// `crate::strategy_sandbox::serve_child`): `request`'s ranked report (`super::verbs::paramscan_report`), its rows
/// handed to `each` best first (`crate::run_summary::summarize_paramscan`), then its header and its tail; or the answer
/// it gives instead of a search (a refusal, a data plan, the search's error), which goes back as it came.
pub(crate) fn paramscan_summary_parts(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    client_gone: Option<&AtomicBool>,
    each: impl FnMut(ParamscanRowSummaryWire, String) -> std::io::Result<()>,
) -> Result<(ParamscanProfileSummaryWire, String), Box<Response>> {
    let Request::Summarized(inner) = request else {
        return Err(Box::new(Response::Error("not a summarized search".to_string())));
    };
    let Request::RunParamscanProfile { profile_toml, rank_by, search } = *inner else {
        return Err(Box::new(Response::Error("not a summarized search".to_string())));
    };
    let report = super::verbs::paramscan_report(
        &profile_toml,
        &study.strategies,
        rank_by.as_deref(),
        search.as_ref(),
        store,
        client_gone,
    )?;
    crate::run_summary::summarize_paramscan(report, each)
        .map_err(|why| Box::new(Response::Error(why)))
}

/// [`handle_request`]'s answer with nothing saved: the sandboxed child for a Rust strategy FILE, this
/// process for every other request. A study's one backtest (`crate::harness_sim`) asks here, because
/// it is a step of the study's run, which is saved as a whole.
pub(crate) fn answer_unsaved(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    named_run: NamedRunLane,
    client_gone: Option<&AtomicBool>,
) -> Response {
    let rust = crate::strategy_sandbox::resolve(&request, study);
    answer_leaving_run(request, store, study, named_run, client_gone, rust, None).0
}

/// [`answer_unsaved`]'s answer and, for a Studio run, the run it leaves for the saver (`super::saved_run::StudioRun`),
/// from whichever route ran it: `rust` is the request's `.rs` file as `crate::strategy_sandbox::resolve` resolved it
/// (`None`: this process runs it), its child's reads recorded with their witnesses in `witness` when given.
fn answer_leaving_run(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    named_run: NamedRunLane,
    client_gone: Option<&AtomicBool>,
    rust: Option<Result<Vec<RustRun>, String>>,
    witness: Option<&ReadWitnesses>,
) -> (Response, Option<StudioRun>) {
    let runs = match rust {
        Some(Ok(runs)) => Some(runs),
        Some(Err(why)) => {
            return (Response::Error(vike_studio_core::RunError::Strategy(why).wire_line()), None);
        }
        None => None,
    };
    // A summarized run runs the request it wraps as that request alone would, and THIS process holds its
    // trades (`crate::run_summary`). A `.rs` file's child summarizes it there and streams what is held,
    // point by point, so its pipe carries no whole answer.
    if let Request::Summarized(inner) = request {
        if let Some(runs) = runs {
            let summarized = Request::Summarized(inner);
            return crate::strategy_sandbox::run_summarized_in_child(
                &summarized,
                &runs,
                store,
                client_gone,
                study.build.as_deref(),
                witness,
            );
        }
        let mut left = None;
        let response = crate::run_summary::summarized(*inner, &study.strategies.dir, |inner| {
            let (response, run, held) =
                answer_whole_here(inner, store, study, named_run, client_gone);
            left = run;
            (response, held)
        });
        return (response, left);
    }
    if let Some(runs) = runs {
        return crate::strategy_sandbox::run_in_child(&request, &runs, store, client_gone, witness);
    }
    answer_here(request, store, study, named_run, client_gone)
}

/// [`handle_request_here`]'s answer and, for a Studio run (`Request::RunSlice`, `RunParamscan`, `RunWalkforward`), the
/// run it leaves for this daemon's saver (`super::saved_run::StudioRun`). What [`answer_leaving_run`] and the sandboxed
/// child (`crate::strategy_sandbox::serve_child`, which sends the run up in its own frame) ask.
pub(crate) fn answer_here(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    named_run: NamedRunLane,
    client_gone: Option<&AtomicBool>,
) -> (Response, Option<StudioRun>) {
    match request {
        request @ (Request::RunSlice { .. }
        | Request::RunParamscan { .. }
        | Request::RunWalkforward { .. }) => {
            let (response, run, _) = studio_three(request, study, store);
            (response, run)
        }
        request => (handle_request_here(request, store, study, named_run, client_gone), None),
    }
}

/// [`answer_here`] for the request a `Request::Summarized` wraps, in this process or in a `.rs` file's sandboxed child
/// (`crate::strategy_sandbox::serve_child`): a `RunBacktest` answers its run's WHOLE result
/// (`super::verbs::run_backtest_whole`, decision 0078's amendment of 2026-11-18) for `crate::run_summary` to summarize,
/// where the plain verb answers the report; every other request answers as [`answer_here`] does. Third, the full
/// records behind a run's answer, which its summary reads (`crate::run_summary::WholeRecords`).
pub(crate) fn answer_whole_here(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    named_run: NamedRunLane,
    client_gone: Option<&AtomicBool>,
) -> (Response, Option<StudioRun>, WholeRecords) {
    match request {
        Request::RunBacktest(profile_toml) => {
            run_backtest_whole(&profile_toml, &study.strategies, store, client_gone)
        }
        request @ (Request::RunSlice { .. }
        | Request::RunParamscan { .. }
        | Request::RunWalkforward { .. }) => studio_three(request, study, store),
        request => {
            let (response, run) = answer_here(request, store, study, named_run, client_gone);
            (response, run, WholeRecords::default())
        }
    }
}

/// **The STUDIO three**, through `crate::studio_run`'s runners: the SAME computations the parity test
/// (`tests/studio_wire_parity.rs`) drives as `run_slice_local`, `run_paramscan_local` and `run_walkforward_local`,
/// each with the run it leaves for the saver and the records behind its answer. `slice` is boxed in each variant
/// (enum-size hygiene); unbox it for the runner.
fn studio_three(
    request: Request,
    study: &StudyDirs,
    store: &Arc<dyn HistoryStore + Send + Sync>,
) -> (Response, Option<StudioRun>, WholeRecords) {
    let (files, store) = (&study.strategies, Arc::clone(store));
    let answered = match request {
        Request::RunSlice { spec, slice, params } => {
            crate::studio_run::run_slice_leaving_run(&spec, files, &slice, params.as_ref(), store)
                .map(|(result, run, held)| (Response::RunResult(result), run, held))
        }
        Request::RunParamscan { spec, slice, paramscan, params } => {
            crate::studio_run::run_paramscan_leaving_run(
                &spec,
                files,
                &slice,
                &paramscan,
                params.as_ref(),
                store,
            )
            .map(|(result, run, held)| (Response::ParamscanResult(result), run, held))
        }
        Request::RunWalkforward { spec, slice, walkforward, params } => {
            crate::studio_run::run_walkforward_leaving_run(
                &spec,
                files,
                &slice,
                &walkforward,
                params.as_ref(),
                store,
            )
            .map(|(result, run)| {
                (Response::WalkforwardResult(Box::new(result)), run, WholeRecords::default())
            })
        }
        other => {
            let kind = vike_backtesthub_client::proto::request_kind(&other);
            return (
                Response::Error(format!("{kind} is not one of the Studio's three runs")),
                None,
                WholeRecords::default(),
            );
        }
    };
    answered.unwrap_or_else(|e| (Response::Error(e.wire_line()), None, WholeRecords::default()))
}

/// Map one request to its response IN THIS PROCESS. Pure dispatch: the profile-shaped runs go straight to
/// `vike_backtest`, the Studio three and the study to `crate::studio_run`'s runners. A DATA
/// verb never reaches this match: it does not decode into this daemon's [`Request`], and
/// `super::connection`'s loop answers it by name. The sandboxed child calls it through [`answer_here`], with
/// its piped history as `store`.
///
/// `client_gone` is the connection's peer-gone flag; the three profile-shaped runs read it
/// (`super::connection`'s `is_watched` names them, and `run_paramscan_profile`'s doc carries the
/// contract), and `None` means nobody is watching. Every other arm ignores it — the connection
/// never arms a watch for them.
pub(crate) fn handle_request_here(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    named_run: NamedRunLane,
    client_gone: Option<&AtomicBool>,
) -> Response {
    match request {
        Request::Ping => Response::Pong,

        // The compute-to-data run: the profile crosses the wire, the history never does.
        Request::RunBacktest(profile_toml) => {
            run_backtest(&profile_toml, &study.strategies, store, client_gone)
        }
        Request::RunParamscanProfile { profile_toml, rank_by, search } => run_paramscan_profile(
            &profile_toml,
            &study.strategies,
            rank_by.as_deref(),
            search.as_ref(),
            store,
            client_gone,
        ),
        Request::RunWalkforwardProfile { profile_toml } => {
            run_walkforward_profile(&profile_toml, &study.strategies, store, client_gone)
        }
        // The research plane's verb (ruling R1): `vike-study-runner`'s study runner, over this
        // daemon's own two directories, through `crate::studio_run`'s entry.
        Request::RunStudy(study_request) => match crate::studio_run::run_study_local(
            &study_request,
            Arc::clone(store),
            &study.runs_root,
            study.learner_bin.as_deref(),
            study.neural_trainer_bin.as_deref(),
            &study.strategies.dir,
        ) {
            Ok(json) => Response::StudyReport(json),
            Err(msg) => Response::Error(msg),
        },
        // The strategy roster IS the folder: every file the scan of this daemon's `strategies/`
        // lists, broken ones included (a run of one is refused with its reason). Store-independent,
        // and the same scan the `--list` flag of this very binary prints.
        Request::ListStrategies => {
            Response::Strategies(crate::backtest_cli::strategy_roster(&study.strategies.dir))
        }
        // The same folder with each file's states, and one file's text: what a Studio lists and
        // opens (`FEATURE_STRATEGY_FILES`).
        Request::ListStrategyFiles => Response::StrategyFiles(study.strategies.listing()),
        Request::ReadStrategyFile { file } => match study.strategies.read_text(&file) {
            Ok(text) => Response::StrategyFileText { file, text },
            Err(msg) => Response::Error(msg),
        },
        // The owner's ruling of 2026-11-13: the backtesthub writes the strategy files (the builder
        // only compiles). Admitted only on a Control connection (`super::connection` refuses it on a
        // daemon that authenticates nothing).
        Request::SaveStrategyFile { file, text } => {
            match study.strategies.save_text(&file, &text) {
                Ok((sha256, file)) => Response::StrategyFileSaved { sha256, file },
                Err(msg) => Response::Error(msg),
            }
        }
        // A Studio's Build (decision 0288's amendment of 2026-11-16): forwarded to the strategy builder,
        // which no person's key reaches any more. Admitted only on a Control connection, as the save is.
        Request::BuildStrategyFile { file, text, sdk } => {
            match crate::strategy_sandbox::build_text(&file, &text, sdk) {
                Ok(sha256) => Response::StrategyFileBuilt { sha256 },
                Err(msg) => Response::Error(msg),
            }
        }

        // ⚠ **THE NAMED RUN AND ITS ROSTER — the only Observe-scope `Run*` pair on this wire**
        // (`docs/decisions/0064-a-named-run-carries-no-source.md`). Everything that makes them
        // different from their neighbours above is in `crate::named_run`, deliberately: this arm is
        // a call, so there is no second place where the fence, the bounds or the arming could be
        // re-spelled slightly differently.
        //
        // ⚠ The roster is NOT the folder scan one arm up: a named run resolves the operator's
        // compiled-in user strategies, which no file lists. 0064's decision 7 is the ruling;
        // `crate::named_run::named_roster` is the one implementation.
        Request::NamedStrategies => {
            Response::NamedStrategies(crate::named_run::named_roster(named_run))
        }
        Request::RunNamed(spec) => {
            match crate::named_run::serve_named_run(&spec, store, named_run) {
                Ok(outcome) => Response::NamedRun(Box::new(outcome)),
                // A malformed request or a store failure — the two things that are not an OUTCOME of
                // running. An unarmed lane, a full slot table and an unknown name are all `Ok`.
                Err(msg) => Response::Error(msg),
            }
        }

        // The STUDIO three, through [`studio_three`] (their runs left for the saver are dropped here).
        request @ (Request::RunSlice { .. }
        | Request::RunParamscan { .. }
        | Request::RunWalkforward { .. }) => studio_three(request, study, store).0,

        // A summarized PROFILE search answered here saves nothing (`handle_request` saves a served one's rows).
        request if is_paramscan_profile_summary(&request) => {
            let mut nothing_saved = ParamscanSave::new(None, &study.runs_root, None);
            paramscan_summary_here(request, store, study, client_gone, &mut nothing_saved)
        }
        // The SUMMARIZED runs (decision 0078's amendments of 2026-11-16 and 2026-11-18), `crate::run_summary`:
        // the wrapped run answered in this process, then summarized, its trades and curve held for the reads.
        Request::Summarized(inner) => {
            crate::run_summary::summarized(*inner, &study.strategies.dir, |inner| {
                let (response, _, held) =
                    answer_whole_here(inner, store, study, named_run, client_gone);
                (response, held)
            })
        }
        Request::ReadTrades { run, point, page, page_size } => {
            crate::run_summary::trades_page(&run, point, page, page_size)
        }
        Request::ReadPoint { run, point } => crate::run_summary::point_summary(&run, point),
        Request::ReadParamscanRows { run, page, page_size } => {
            crate::run_summary::paramscan_rows_page(&run, page, page_size)
        }
        // A held single run's whole curve, or a saved run's (its id): kept, or rebuilt from its event log over the
        // bars `store` holds, page by page.
        Request::ReadCurve { run, page, page_size } => {
            let rebuild = crate::run_summary::CurveRebuild { store, build: study.build.as_deref() };
            crate::run_summary::curve_page(&run, page, page_size, &study.runs_root, Some(&rebuild))
        }

        // The server's studies and saved runs, as the Studio's Research pane reads them (the owner's
        // ruling of 2026-11-19: studies run only on the server). `crate::study_files` checks every name
        // a client sends against this server's own listings.
        Request::ListStudies => Response::Studies(crate::study_files::study_listing(
            &crate::study_sandbox::studies_root(),
        )),
        Request::ListRuns { page, page_size } => {
            Response::Runs(crate::study_files::runs_page(&study.runs_root, page, page_size))
        }
        Request::ReadRunArtifact { run_id, file, offset, length } => {
            match crate::study_files::run_artifact(&study.runs_root, &run_id, &file, offset, length)
            {
                Ok(artifact) => Response::RunArtifact(artifact),
                Err(msg) => Response::Error(msg),
            }
        }

        // The versioned strategy SDK's source, read-only (decision 0296, verdict 3): the SDKs under this
        // daemon's project, and one file or folder of one, every path checked by `crate::sdk_source`.
        Request::ListSdks => match crate::sdk_source::listing(study.project_root.as_deref()) {
            Ok(listing) => Response::Sdks(listing),
            Err(msg) => Response::Error(msg),
        },
        Request::ReadSdkFile { sdk, path } => {
            match crate::sdk_source::read(study.project_root.as_deref(), &sdk, &path) {
                Ok(file) => Response::SdkFile(file),
                Err(msg) => Response::Error(msg),
            }
        }
        // A run asked fresh, answered here, is its run: nothing here answers from a saved run (`handle_request` does).
        Request::Fresh(inner) => handle_request_here(*inner, store, study, named_run, client_gone),
    }
}

/// The verbs THIS daemon answers, advertised in the handshake's `Welcome` — the compute
/// half of the split, and the exact complement of `vike_datahub::server`'s `served_features`.
///
/// Every daemon advertises the Studio verbs, `FEATURE_WALKFORWARD_SEARCH` and `FEATURE_STUDY`: the
/// runners are `vike-studio-core`'s and this crate calls them directly, so there is no mount to
/// be absent (it WAS a runtime fact while the runners were injected, and a daemon built without
/// them advertised none).
///
/// ⚠ The strings are the ones the data daemon used to advertise, unchanged — `backtest`,
/// `list_strategies`, `run_sweep_profile`, `run_walkforward_profile`, `run_slice`, `run_sweep`,
/// `run_walkforward`. A client's feature check is therefore the SAME string against a different
/// address, which is what makes ruling 7 a change of address for a client rather than a change of
/// protocol.
///
/// ⚠ **`run_sweep_profile` and `run_sweep` keep the OLD spelling on purpose, and this sentence is
/// not a guard** — the `sweep` -> `paramscan` rename pass rewrote this very paragraph to
/// `run_paramscan_profile`/`run_paramscan` while the code nine lines down still pushed
/// `"run_paramscan_profile"`/`"run_sweep"`, so the doc that asserted "unchanged" was the one that
/// changed, and it named two strings that appear on no wire. A capability string is a NEGOTIATED
/// TOKEN, not an identifier: an older client compares it literally, so renaming it would refuse
/// every peer that shipped before the rename. The authority is the code below, never this list;
/// if the two disagree, the code is right. `crates/vike-datahub/src/server/features.rs`'s `served_features`
/// carries the same frozen spellings in its own removal note.
pub(crate) fn served_features(requires_auth: bool) -> Vec<String> {
    let mut features = vec![
        // ⚠ The PLANE SENTINEL, pushed through the shared constant rather than spelled here: a
        // client (`vike_backtesthub_client::BacktesthubClient::connect_authed_on`) REFUSES to sign a key
        // toward a server whose `Welcome` it cannot place on a plane, so a literal that drifted
        // from the client's copy would refuse every keyed Studio Run. The value is the frozen
        // token it always was — `"backtest"`. `crates/vike-backtesthub/tests/backtest_plane.rs`'s
        // `a_keyed_backtesthub_is_the_plane_a_backtest_key_may_sign_toward` holds this daemon's
        // real `Welcome` to that classification.
        BACKTEST_PLANE_SENTINEL.to_string(),
        FEATURE_LIST_STRATEGIES.to_string(),
        FEATURE_RUN_PARAMSCAN_PROFILE.to_string(),
        FEATURE_RUN_WALKFORWARD_PROFILE.to_string(),
        // ⚠ A CAPABILITY, not a verb name, and the first entry here that is not one. It says
        // `RunParamscanProfile` can carry a search METHOD (and can rank by the composite objective) —
        // a question a client cannot ask any other way, because a daemon predating the field
        // decodes the frame, drops it and answers a normal report. UNCONDITIONAL, like
        // `FEATURE_COVERAGE` and unlike `FEATURE_BACKFILL`: this whole module is a DEFAULT build of
        // this crate (the 2026-09-27 feature collapse retired the `hist-replay` feature that used to
        // gate it), so a build that has the crate has the arm.
        // `vike_backtesthub_client::proto`'s `FEATURE_SEARCH_METHOD` carries the three legs.
        FEATURE_SEARCH_METHOD.to_string(),
        // ⚠ UNCONDITIONAL, and it is a BUILD fact for the same reason `FEATURE_SEARCH_METHOD` above
        // is one: this whole module is a DEFAULT build, so a build that compiles this file has
        // both named-run arms. It is deliberately NOT conditioned on the ARMING, and that is
        // `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8: an unarmed server
        // must ANSWER, and if the capability rode the arming then an OLD server and an unarmed one
        // would be indistinguishable in `Welcome.features` — one silence for two different
        // sentences an operator needs to tell apart. The arming rides in the ANSWER instead
        // (`NamedRunOutcome::NotArmed`, `NamedRunRoster::armed`), which makes all three states
        // distinguishable with one string.
        FEATURE_NAMED_RUN.to_string(),
    ];
    // The sentinel a client reads to learn the Studio runners are here — shared for the same
    // reason as `BACKTEST_PLANE_SENTINEL` above; its value is the frozen `"run_slice"`.
    features.push(STUDIO_RUNNER_SENTINEL.to_string());
    features.push(FEATURE_RUN_PARAMSCAN.to_string());
    features.push(FEATURE_RUN_WALKFORWARD.to_string());
    // ⚠ A CAPABILITY rather than a verb name, like `FEATURE_SEARCH_METHOD` above. It says the
    // STUDIO walk-forward can carry a per-window SEARCH: a question a client cannot ask any other
    // way, because a daemon predating the field decodes the frame, drops it, and answers a normal
    // FIXED-walk report. `vike_backtesthub_client::proto`'s `FEATURE_WALKFORWARD_SEARCH` carries the
    // three legs.
    features.push(FEATURE_WALKFORWARD_SEARCH.to_string());
    // ⚠ A CAPABILITY: a Studio run's `StrategyEngineConfigWire` can carry a slippage MODEL, which a daemon
    // predating the field would drop and price as a flat fraction (`FEATURE_SLIPPAGE_MODEL`).
    features.push(FEATURE_SLIPPAGE_MODEL.to_string());
    // ⚠ CAPABILITIES (2026-11-20): a Studio run's funding switch, which a daemon predating it would drop and charge a
    // perpetual's funding (`FEATURE_ATTACH_FUNDING`), and a Rhai buffer's parameters, a spec variant it cannot decode
    // (`FEATURE_RHAI_PARAMETERS`).
    features.push(FEATURE_ATTACH_FUNDING.to_string());
    features.push(FEATURE_RHAI_PARAMETERS.to_string());
    // ⚠ A CAPABILITY: a Studio run's file spec and a Build can name the SDK to build against, which a daemon
    // predating the field would drop and build the file's own SDK (`FEATURE_STRATEGY_SDK`).
    features.push(FEATURE_STRATEGY_SDK.to_string());
    // ⚠ A CAPABILITY too: the research plane's `RunStudy`, which belongs to a different plane in
    // the CLI's vocabulary (ruling R1 of
    // `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`) and negotiates its own
    // string, so that string means "this daemon runs a study" and nothing broader.
    features.push(FEATURE_STUDY.to_string());
    // A BUILD fact: both strategy-file verbs read the folder every build has.
    features.push(FEATURE_STRATEGY_FILES.to_string());
    // A BUILD fact too: a daemon that reaches no builder answers the Build with a refusal in words.
    features.push(FEATURE_BUILD_STRATEGY_FILE.to_string());
    // A BUILD fact: every daemon summarizes a Studio run and holds its trades (`crate::run_summary`).
    features.push(FEATURE_RUN_SUMMARY.to_string());
    // A BUILD fact too: every daemon holds a summarized single run's whole curve, pages it, and summarizes a
    // `RunBacktest` (decision 0078's amendment of 2026-11-18).
    features.push(FEATURE_RUN_CURVE.to_string());
    // A BUILD fact too: every daemon summarizes a profile search, holds its rows and saves it as its rows arrive
    // (decision 0078's amendment of 2026-11-19).
    features.push(FEATURE_PARAMSCAN_PROFILE_SUMMARY.to_string());
    // BUILD facts: every daemon lists its study folder and runs a study of either tier by name and
    // recipe file, and pages its saved runs and their files (`crate::study_files`).
    features.push(FEATURE_STUDY_FILES.to_string());
    features.push(FEATURE_RUN_FILES.to_string());
    // A BUILD fact: every daemon answers the SDK source verbs, a daemon handed no SDK reader with a
    // refusal in words (`crate::sdk_source`).
    features.push(FEATURE_SDK_SOURCE.to_string());
    // A BUILD fact: every daemon answers an identical run from the run it saved, and reads `Fresh` (decision 0078's
    // amendment of 2026-11-20); a daemon that saves nothing answers none so, which a client cannot tell from a miss.
    features.push(FEATURE_RUN_FROM_SAVED.to_string());
    // A BUILD fact: every daemon reads a msgpack request and answers it in msgpack (`connection.rs`'s loop,
    // `vike_backtesthub_client::msgpack_body::write_reply`); a JSON request is answered JSON.
    features.push(FEATURE_WIRE_MSGPACK.to_string());
    if requires_auth {
        features.push(FEATURE_AUTH.to_string());
    }
    features
}
