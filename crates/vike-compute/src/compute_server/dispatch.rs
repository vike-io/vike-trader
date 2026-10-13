//! Request dispatch: one request to its response, the cross-plane refusals, the advertised features.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_data::StoreHandle;
use vike_datahub_client::proto::{Plane, Request, Response, wrong_plane_message};
use vike_datahub_client::{
    COMPUTE_PLANE_SENTINEL, FEATURE_AUTH, FEATURE_NAMED_RUN, FEATURE_SEARCH_METHOD, FEATURE_STUDY,
    FEATURE_WALKFORWARD_SEARCH, PROTO_VERSION, STUDIO_RUNNER_SENTINEL,
};

use super::StudyDirs;
use super::verbs::{run_backtest, run_paramscan_profile, run_walkforward_profile};
use vike_backtest::harness;
use vike_backtest::named_run::NamedRunLane;

/// The refusal a DATA verb gets here — the exact mirror of `vike_datahub::server`'s
/// `compute_verb_moved`, through the same one-spelling helper with the planes swapped.
fn data_verb_elsewhere(verb: &'static str) -> Response {
    Response::Error(wrong_plane_message(verb, Plane::Compute, Plane::Data))
}

/// Map one request to its response. Pure dispatch: the profile-shaped runs go straight to
/// `vike_backtest::harness`, the Studio three and the study to `vike-studio-core`'s runners, and
/// every DATA verb is answered by [`data_verb_elsewhere`].
///
/// ⚠ The DATA arms are written out one by one rather than as an `other =>` catch-all, for the
/// reason `plane_of`'s own match spells out: a catch-all would silently swallow the NEXT verb
/// somebody adds and tell its author it belongs to the data daemon.
///
/// `client_gone` is the connection's peer-gone flag; the three profile-shaped runs read it
/// (`super::connection`'s `is_watched` names them, and `run_paramscan_profile`'s doc carries the
/// contract), and `None` means nobody is watching. Every other arm ignores it — the connection
/// never arms a watch for them.
pub(super) fn handle_request(
    request: Request,
    store: &StoreHandle,
    study: &StudyDirs,
    named_run: NamedRunLane,
    client_gone: Option<&AtomicBool>,
) -> Response {
    match request {
        // The OPTIONAL version handshake: answer with THIS server's `PROTO_VERSION` and the verbs
        // it serves. Only reachable on a KEY-LESS server — a keyed one answers `Hello` inside
        // `run_handshake` and never returns here, which is why `requires_auth` is `false`.
        Request::Hello { proto_version: client_version } => {
            tracing::debug!(client_version, "vike-backtest serve: hello handshake");
            Response::Welcome {
                proto_version: PROTO_VERSION,
                features: served_features(false),
                nonce: None,
            }
        }
        // Only reachable on a KEY-LESS server. There is nothing to verify it against, so say so
        // rather than pretending: a client that signed a mac deserves to learn its key was never
        // checked, not to be told "ok".
        Request::Auth { .. } => Response::AuthDenied {
            reason: "this backtest server has no node keys configured and authenticates nothing; \
                     connect without Auth"
                .into(),
        },
        Request::Ping => Response::Pong,

        // The compute-to-data run: the profile crosses the wire, the history never does.
        Request::RunBacktest(profile_toml) => run_backtest(&profile_toml, store, client_gone),
        Request::RunParamscanProfile { profile_toml, rank_by, search } => run_paramscan_profile(
            &profile_toml,
            rank_by.as_deref(),
            search.as_ref(),
            store,
            client_gone,
        ),
        Request::RunWalkforwardProfile { profile_toml } => {
            run_walkforward_profile(&profile_toml, store, client_gone)
        }
        // The research plane's verb (ruling R1): `vike-studio-core`'s study runner, over this
        // daemon's own two directories.
        Request::RunStudy(study_req) => match vike_studio_core::run_study_local(
            &study_req,
            Arc::clone(store),
            &study.runs_root,
            study.learner_bin.as_deref(),
        ) {
            Ok(json) => Response::StudyReport(json),
            Err(msg) => Response::Error(msg),
        },
        // The strategy-roster verb. Store-independent (the roster is a compile-time `&[&str]`
        // const) and, since ruling 7, served beside the `--list` flag of this very binary, which reads
        // the same constant — one constant, two transports, one crate.
        Request::ListStrategies => {
            Response::Strategies(harness::STRATEGIES.iter().map(|s| s.to_string()).collect())
        }

        // ⚠ **THE NAMED RUN AND ITS ROSTER — the only Observe-scope `Run*` pair on this wire**
        // (`docs/decisions/0064-a-named-run-carries-no-source.md`). Everything that makes them
        // different from their neighbours above is in `vike_backtest::named_run`, deliberately: this arm is
        // a call, so there is no second place where the fence, the bounds or the arming could be
        // re-spelled slightly differently.
        //
        // ⚠ The roster is NOT `harness::STRATEGIES` one arm up, and the difference runs in BOTH
        // directions — that roster carries the simulator-only arms that sit beside the Rhai
        // compiler and therefore outside the named run's closure, and it has never carried the
        // operator's own compiled-in user strategies, which a named run DOES resolve. 0064's
        // decision 7 is the ruling; `vike_backtest::named_run::named_roster` is the one implementation.
        Request::NamedStrategies => {
            Response::NamedStrategies(vike_backtest::named_run::named_roster(named_run))
        }
        Request::RunNamed(spec) => {
            match vike_backtest::named_run::serve_named_run(&spec, store, named_run) {
                Ok(outcome) => Response::NamedRun(Box::new(outcome)),
                // A malformed request or a store failure — the two things that are not an OUTCOME of
                // running. An unarmed lane, a full slot table and an unknown name are all `Ok`.
                Err(msg) => Response::Error(msg),
            }
        }

        // The STUDIO three, through `vike-studio-core`'s `*_local` runners — the SAME entries its
        // parity test drives. `slice` is boxed in each variant (enum-size hygiene); unbox it for
        // the runner.
        Request::RunSlice { spec, slice, params } => {
            match vike_studio_core::run_slice_local(
                &spec,
                &slice,
                params.as_ref(),
                Arc::clone(store),
            ) {
                Ok(result) => Response::RunResult(result),
                Err(e) => Response::Error(e.to_error_string()),
            }
        }
        Request::RunParamscan { spec, slice, paramscan, params } => {
            match vike_studio_core::run_sweep_local(
                &spec,
                &slice,
                &paramscan,
                params.as_ref(),
                Arc::clone(store),
            ) {
                Ok(result) => Response::ParamscanResult(result),
                Err(e) => Response::Error(e.to_error_string()),
            }
        }
        Request::RunWalkforward { spec, slice, walkforward, params } => {
            match vike_studio_core::run_walkforward_local(
                &spec,
                &slice,
                &walkforward,
                params.as_ref(),
                Arc::clone(store),
            ) {
                Ok(result) => Response::WalkforwardResult(result),
                Err(e) => Response::Error(e.to_error_string()),
            }
        }

        // The DATA plane — `vike-backend datahub`'s, and refused here by name. `handle_connection`
        // refuses them one step earlier, before the scope check; these arms are the belt to that
        // braces.
        Request::LoadBars { .. } => data_verb_elsewhere("LoadBars"),
        Request::ScanQuotes { .. } => data_verb_elsewhere("ScanQuotes"),
        Request::ScanTrades { .. } => data_verb_elsewhere("ScanTrades"),
        // ...and the SIX reads `docs/decisions/0084` added beside them. Same plane, same
        // refusal: this daemon holds the COMPUTE store handle, not the data daemon's.
        Request::ScanBookUpdates { .. } => data_verb_elsewhere("ScanBookUpdates"),
        Request::ScanDepth { .. } => data_verb_elsewhere("ScanDepth"),
        Request::ScanCohort { .. } => data_verb_elsewhere("ScanCohort"),
        Request::ScanPerpMetrics { .. } => data_verb_elsewhere("ScanPerpMetrics"),
        Request::ScanEquity { .. } => data_verb_elsewhere("ScanEquity"),
        Request::ScanExecFills { .. } => data_verb_elsewhere("ScanExecFills"),
        Request::ScanExecOrders { .. } => data_verb_elsewhere("ScanExecOrders"),
        Request::PropertiesAsOf { .. } => data_verb_elsewhere("PropertiesAsOf"),
        Request::ListSeries => data_verb_elsewhere("ListSeries"),
        Request::Inventory => data_verb_elsewhere("Inventory"),
        Request::SeriesGaps { .. } => data_verb_elsewhere("SeriesGaps"),
        Request::SeriesFacts { .. } => data_verb_elsewhere("SeriesFacts"),
        Request::Coverage => data_verb_elsewhere("Coverage"),
        Request::Backfill { .. } => data_verb_elsewhere("Backfill"),
        // ...and the two verbs that list and stop the data daemon's RUNNING backfills: the registry
        // they read is that daemon's, and this one runs no backfill to name.
        Request::ListBackfills => data_verb_elsewhere("ListBackfills"),
        Request::CancelBackfill { .. } => data_verb_elsewhere("CancelBackfill"),
        // ...and the history-channels read: its overlay is the data daemon's table, store and
        // credential store, none of which this daemon holds (`docs/decisions/0102`).
        Request::HistoryChannels => data_verb_elsewhere("HistoryChannels"),
        // The archive import — the data daemon's store and the data daemon's imports directory,
        // so this daemon refuses it exactly as it refuses `Backfill`.
        Request::ImportArchive(_) => data_verb_elsewhere("ImportArchive"),
        // The chart-gap seed. Its SCOPE is unlike its neighbours here (`VerbScope::Read`,
        // `docs/decisions/0057`), and its PLANE is not: the store and the collector table it needs
        // are the data daemon's, so this daemon refuses it exactly as it refuses `Backfill`.
        Request::SeedSeries { .. } => data_verb_elsewhere("SeedSeries"),
        // The venue catalog. Same shape as the seed above and for a sharper reason: this daemon
        // links no venue bridge at all, so it holds no `CatalogProvider` to answer with
        // (`docs/decisions/0062`). Its scope is Observe and its plane is the data daemon's.
        Request::VenueCatalog { .. } => data_verb_elsewhere("VenueCatalog"),
        // The options-chain snapshot: the data daemon polls the venue and folds its feeds; this
        // daemon links no venue bridge and holds no snapshot to answer with.
        Request::OptionsChain { .. } => data_verb_elsewhere("OptionsChain"),
        // The tool feeds (news, economic calendar, equity calendars): the data daemon polls the
        // providers and holds their keys; this daemon holds neither a cache nor a credential.
        Request::NewsFeed { .. } => data_verb_elsewhere("NewsFeed"),
        Request::EconomicCalendar { .. } => data_verb_elsewhere("EconomicCalendar"),
        Request::EquityCalendar { .. } => data_verb_elsewhere("EquityCalendar"),
        Request::DeleteSeries { .. } => data_verb_elsewhere("DeleteSeries"),
        // The MARKET-DATA push lane — likewise the data daemon's. ⚠ These two arms are why this
        // file is in the wire's diff at all: `Request` is ONE schema for two daemons, so a verb
        // added for the datahub reddens this exhaustive match until it is classified. A desktop
        // that dialled `config.backtest_addr` by mistake therefore gets a NAMED refusal that says
        // which daemon serves its DOM, and keeps its connection.
        Request::MdSubscribe { .. } => data_verb_elsewhere("MdSubscribe"),
        Request::MdUpdate { .. } => data_verb_elsewhere("MdUpdate"),
    }
}

/// The verbs THIS daemon answers, advertised in the [`Response::Welcome`] handshake — the compute
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
/// `"run_sweep_profile"`/`"run_sweep"`, so the doc that asserted "unchanged" was the one that
/// changed, and it named two strings that appear on no wire. A capability string is a NEGOTIATED
/// TOKEN, not an identifier: an older client compares it literally, so renaming it would refuse
/// every peer that shipped before the rename. The authority is the code below, never this list;
/// if the two disagree, the code is right. `crates/vike-datahub/src/server/features.rs`'s `served_features`
/// carries the same frozen spellings in its own removal note.
pub(crate) fn served_features(requires_auth: bool) -> Vec<String> {
    let mut features = vec![
        // ⚠ The PLANE SENTINEL, pushed through the shared constant rather than spelled here: a
        // client (`vike_datahub_client::DatahubClient::connect_authed_on`) REFUSES to sign a key
        // toward a server whose `Welcome` it cannot place on a plane, so a literal that drifted
        // from the client's copy would refuse every keyed Studio Run. The value is the frozen
        // token it always was — `"backtest"`. `crates/vike-compute/tests/compute_plane.rs`'s
        // `a_keyed_compute_daemon_is_the_plane_a_compute_key_may_sign_toward` holds this daemon's
        // real `Welcome` to that classification.
        COMPUTE_PLANE_SENTINEL.to_string(),
        "list_strategies".to_string(),
        "run_sweep_profile".to_string(),
        "run_walkforward_profile".to_string(),
        // ⚠ A CAPABILITY, not a verb name, and the first entry here that is not one. It says
        // `RunParamscanProfile` can carry a search METHOD (and can rank by the composite objective) —
        // a question a client cannot ask any other way, because a daemon predating the field
        // decodes the frame, drops it and answers a normal report. UNCONDITIONAL, like
        // `FEATURE_COVERAGE` and unlike `FEATURE_BACKFILL`: this whole module is a DEFAULT build of
        // this crate (the 2026-09-27 feature collapse retired the `hist-replay` feature that used to
        // gate it), so a build that has the crate has the arm.
        // `vike_datahub_client::proto`'s `FEATURE_SEARCH_METHOD` carries the three legs.
        FEATURE_SEARCH_METHOD.to_string(),
        // ⚠ UNCONDITIONAL, and it is a BUILD fact for the same reason `FEATURE_SEARCH_METHOD` above
        // is one: this whole module is a DEFAULT build, so a build that compiles this file has
        // both named-run arms. It is deliberately NOT conditioned on the ARMING, and that is
        // `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 8: an unarmed server
        // must ANSWER, and if the capability rode the arming then an OLD server and an unarmed one
        // would be indistinguishable in `Welcome.features` — one silence for two different
        // sentences an operator needs to tell apart. The arming rides in the ANSWER instead
        // (`NamedRunOutcome::NotArmed`, `NamedRoster::armed`), which makes all three states
        // distinguishable with one string.
        FEATURE_NAMED_RUN.to_string(),
    ];
    // The sentinel a client reads to learn the Studio runners are here — shared for the same
    // reason as `COMPUTE_PLANE_SENTINEL` above; its value is the frozen `"run_slice"`.
    features.push(STUDIO_RUNNER_SENTINEL.to_string());
    features.push("run_sweep".to_string());
    features.push("run_walkforward".to_string());
    // ⚠ A CAPABILITY rather than a verb name, like `FEATURE_SEARCH_METHOD` above. It says the
    // STUDIO walk-forward can carry a per-window SEARCH: a question a client cannot ask any other
    // way, because a daemon predating the field decodes the frame, drops it, and answers a normal
    // FIXED-walk report. `vike_datahub_client::proto`'s `FEATURE_WALKFORWARD_SEARCH` carries the
    // three legs.
    features.push(FEATURE_WALKFORWARD_SEARCH.to_string());
    // ⚠ A CAPABILITY too: the research plane's `RunStudy`, which belongs to a different plane in
    // the CLI's vocabulary (ruling R1 of
    // `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`) and negotiates its own
    // string, so that string means "this daemon runs a study" and nothing broader.
    features.push(FEATURE_STUDY.to_string());
    if requires_auth {
        features.push(FEATURE_AUTH.to_string());
    }
    features
}
