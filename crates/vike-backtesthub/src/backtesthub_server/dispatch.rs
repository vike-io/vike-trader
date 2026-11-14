//! Request dispatch: one request to its response, and the advertised features.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vike_backtesthub_client::proto::{
    FEATURE_NAMED_RUN, FEATURE_SEARCH_METHOD, FEATURE_SLIPPAGE_MODEL, FEATURE_STRATEGY_FILES,
    FEATURE_STUDY, FEATURE_WALKFORWARD_SEARCH, PROTO_VERSION, Request, Response,
    STUDIO_RUNNER_SENTINEL,
};
use vike_data::HistoryStore;
use vike_hub_protocol::auth::FEATURE_AUTH;
use vike_hub_protocol::plane::BACKTEST_PLANE_SENTINEL;

use super::StudyDirs;
use super::verbs::{run_backtest, run_paramscan_profile, run_walkforward_profile};
use crate::named_run::NamedRunLane;

/// Map one request to its response: a request whose strategy is a Rust strategy FILE runs in a sandboxed
/// child ([`crate::strategy_sandbox::run_in_child`]: this process loads no strategy file), every other one
/// here ([`handle_request_here`]).
///
/// `client_gone` is the connection's peer-gone flag (`None`: nobody is watching). A sandboxed run reads
/// it to KILL its child (`super::connection`'s `is_watched` arms it for them).
pub(super) fn handle_request(
    request: Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    study: &StudyDirs,
    named_run: NamedRunLane,
    client_gone: Option<&AtomicBool>,
) -> Response {
    if let Some((file, source)) = crate::strategy_sandbox::rust_file_of(&request, &study.strategies)
    {
        if let Err(why) = crate::strategy_sandbox::ensure_built(&file, &source) {
            return Response::Error(vike_studio_core::RunError::Strategy(why).wire_line());
        }
        return crate::strategy_sandbox::run_in_child(&request, &source, store, client_gone);
    }
    handle_request_here(request, store, study, named_run, client_gone)
}

/// Map one request to its response IN THIS PROCESS. Pure dispatch: the profile-shaped runs go straight to
/// `vike_backtest::harness`, the Studio three and the study to `crate::studio_run`'s runners. A DATA
/// verb never reaches this match: it does not decode into this daemon's [`Request`], and
/// `super::connection`'s loop answers it by name. The sandboxed child calls this directly, with its
/// piped history as `store`.
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
        // The OPTIONAL version handshake: answer with THIS server's `PROTO_VERSION` and the verbs
        // it serves. Only reachable on a KEY-LESS server — a keyed one answers `Hello` inside
        // `run_handshake` and never returns here, which is why `requires_auth` is `false`.
        Request::Hello { proto_version: client_version } => {
            tracing::debug!(client_version, "backtesthub: hello handshake");
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
            reason: "this backtest server has no hub keys configured and authenticates nothing; \
                     connect without Auth"
                .into(),
        },
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
        // The research plane's verb (ruling R1): `vike-studio-core`'s study runner, over this
        // daemon's own two directories, through `crate::studio_run`'s entry.
        Request::RunStudy(study_request) => match crate::studio_run::run_study_local(
            &study_request,
            Arc::clone(store),
            &study.runs_root,
            study.learner_bin.as_deref(),
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

        // The STUDIO three, through `crate::studio_run`'s `*_local` runners — the SAME entries the
        // parity test (`tests/studio_wire_parity.rs`) drives. `slice` is boxed in each variant (enum-size hygiene); unbox it for
        // the runner.
        Request::RunSlice { spec, slice, params } => {
            match crate::studio_run::run_slice_local(
                &spec,
                &study.strategies,
                &slice,
                params.as_ref(),
                Arc::clone(store),
            ) {
                Ok(result) => Response::RunResult(result),
                Err(e) => Response::Error(e.wire_line()),
            }
        }
        Request::RunParamscan { spec, slice, paramscan, params } => {
            match crate::studio_run::run_paramscan_local(
                &spec,
                &study.strategies,
                &slice,
                &paramscan,
                params.as_ref(),
                Arc::clone(store),
            ) {
                Ok(result) => Response::ParamscanResult(result),
                Err(e) => Response::Error(e.wire_line()),
            }
        }
        Request::RunWalkforward { spec, slice, walkforward, params } => {
            match crate::studio_run::run_walkforward_local(
                &spec,
                &study.strategies,
                &slice,
                &walkforward,
                params.as_ref(),
                Arc::clone(store),
            ) {
                Ok(result) => Response::WalkforwardResult(result),
                Err(e) => Response::Error(e.wire_line()),
            }
        }
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
        "list_strategies".to_string(),
        "run_paramscan_profile".to_string(),
        "run_walkforward_profile".to_string(),
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
    features.push("run_paramscan".to_string());
    features.push("run_walkforward".to_string());
    // ⚠ A CAPABILITY rather than a verb name, like `FEATURE_SEARCH_METHOD` above. It says the
    // STUDIO walk-forward can carry a per-window SEARCH: a question a client cannot ask any other
    // way, because a daemon predating the field decodes the frame, drops it, and answers a normal
    // FIXED-walk report. `vike_backtesthub_client::proto`'s `FEATURE_WALKFORWARD_SEARCH` carries the
    // three legs.
    features.push(FEATURE_WALKFORWARD_SEARCH.to_string());
    // ⚠ A CAPABILITY: a Studio run's `StrategyEngineConfigWire` can carry a slippage MODEL, which a daemon
    // predating the field would drop and price as a flat fraction (`FEATURE_SLIPPAGE_MODEL`).
    features.push(FEATURE_SLIPPAGE_MODEL.to_string());
    // ⚠ A CAPABILITY too: the research plane's `RunStudy`, which belongs to a different plane in
    // the CLI's vocabulary (ruling R1 of
    // `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`) and negotiates its own
    // string, so that string means "this daemon runs a study" and nothing broader.
    features.push(FEATURE_STUDY.to_string());
    // A BUILD fact: both strategy-file verbs read the folder every build has.
    features.push(FEATURE_STRATEGY_FILES.to_string());
    if requires_auth {
        features.push(FEATURE_AUTH.to_string());
    }
    features
}
