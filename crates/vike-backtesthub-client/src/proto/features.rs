//! The `Welcome.features` strings a backtesthub advertises and its client negotiates on.
//!
//! An additive verb is negotiated by one of these strings, never by a `PROTO_VERSION` bump, and has
//! three legs: the server advertises it exactly when it serves the verb, the client refuses LOCALLY
//! without sending when it is absent, and an older server answers the unknown variant with
//! `Response::Error` and KEEPS the connection:
//! `docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`.
//!
//! The plane sentinels and the `auth` string both hubs push are `vike_hub_protocol`'s.

/// The capability the backtesthub pushes beside `run_sweep` and `run_walkforward` in
/// `crates/vike-backtesthub/src/backtesthub_server/dispatch.rs`'s `served_features`: the Studio runners
/// are here. Its value is the frozen token `run_slice`.
pub const STUDIO_RUNNER_SENTINEL: &str = "run_slice";

/// Gates the NAMED-RUN verbs ([`Request::RunNamed`](super::Request::RunNamed) and
/// [`Request::NamedStrategies`](super::Request::NamedStrategies) —
/// `docs/decisions/0064-a-named-run-carries-no-source.md`); negotiated per decision 0112.
///
/// ⚠ **A BUILD fact, deliberately unlike the data daemon's armed lanes** (`FEATURE_SEED_SERIES`,
/// `FEATURE_VENUE_CATALOG` in `vike_datahub_client::proto`). Those are advertised per armed lane, so an
/// unarmed server and an OLD one look identical. 0064's decision 8 requires an unarmed server to ANSWER,
/// so this string says "this build has the verbs" and the arming rides the answer
/// ([`crate::named_run::NamedRunOutcome::NotArmed`], [`crate::named_run::NamedRunRoster::armed`]): an old
/// server advertises nothing, an unarmed one advertises and answers `NotArmed`, an armed one runs.
///
/// - the backtesthub advertises it unconditionally (`vike_backtesthub::backtesthub_server`'s
///   `served_features`, a DEFAULT-build module);
/// - the client refuses locally, WITHOUT SENDING ([`crate::BacktesthubClient::run_named`]): a daemon
///   predating a field DROPS it and answers a well-formed report, and a dropped WINDOW CEILING would be
///   exactly that failure;
/// - the server re-checks the arming at its own door and never trusts the advertisement.
pub const FEATURE_NAMED_RUN: &str = "named_run";

/// Gates the compiled STUDY runner ([`Request::RunStudy`](super::Request::RunStudy) /
/// [`Response::StudyReport`](super::Response::StudyReport)) — `vike-backend study`'s wire half and what
/// `vike-cli research study` negotiates on (ruling 16 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`); negotiated per decision 0112.
///
/// ⚠ **The backtesthub (`vike-backend backtesthub --addr`) advertises it, never the data server**: a
/// study runs an engine, and ruling 7 of that spec puts every such verb on the backtest plane.
///
/// ⚠ **Unconditional on a daemon of this tree, and still a negotiated capability.**
/// `vike_backtesthub::backtesthub_server`'s `served_features` pushes it on every daemon now: the runner is
/// `vike-studio-core`'s and the server calls it directly. It was a MOUNT fact — pushed ONLY when a
/// `StudyRunFn` was handed in by a composition root, so `vike-backend backtesthub --addr` served a study
/// and a bare backtesthub bin refused it BY NAME — and an OLDER peer still behaves that way, which is why
/// the client keeps checking for the string. The client refuses BY NAME with nothing sent when it is
/// absent ([`crate::BacktesthubClient::run_study`]); `crates/vike-cli/src/cmd/study.rs` prints the richer
/// refusal with the `vike-backend study` escape hatch, and
/// `crates/vike-cli/tests/shipped_cli/study_report_refusal_cli.rs` drives it against the data daemon.
pub const FEATURE_STUDY: &str = "study";

/// Gates a parameter search that names its own SEARCH METHOD —
/// [`Request::RunParamscanProfile`](super::Request::RunParamscanProfile)'s `search` field and the
/// `rank_by` value `"multi"`. An optional field an old server would silently ignore, so it has its own
/// string (decision 0112, verdict 3).
///
/// ⚠ **The field alone is the DEFECT.** [`Request`](super::Request) has no `deny_unknown_fields`, so a
/// daemon predating `search` decodes the frame, drops it, runs the exhaustive grid and answers a normal
/// [`Response::ParamscanReport`](super::Response::ParamscanReport). The client cannot tell a Bayesian
/// search from a grid by looking at one — the silent downgrade #1750 ended when it retired `--search`.
/// So:
///
/// - the backtesthub advertises it UNCONDITIONALLY — a BUILD fact, because
///   `vike_backtesthub::backtesthub_server` is a DEFAULT-build module and every build that links the crate
///   has the arm. ⚠ [`FEATURE_STUDY`] and the walk-forward one below are advertised unconditionally too now;
///   they were MOUNT facts, conditional on a runner injected from a crate above that daemon, until it was called directly;
/// - the client refuses LOCALLY, WITHOUT SENDING, when the request needs it
///   ([`SearchSelectionWire::needs_capability`](super::SearchSelectionWire::needs_capability));
/// - a server predating the FIELD still decodes the frame and keeps the connection, so the refusal
///   is a client-side courtesy rather than the only guard against a desync.
pub const FEATURE_SEARCH_METHOD: &str = "search_method";

/// Gates the STUDIO walk-forward's per-window SEARCH — [`crate::wire_studio::WalkforwardWire`]'s
/// `search` field. Same defect, same three legs as [`FEATURE_SEARCH_METHOD`], different verb: a
/// daemon predating the field runs the FIXED walk and answers a normal
/// [`Response::WalkforwardResult`](super::Response::WalkforwardResult) — not a degraded answer but
/// an answer to a DIFFERENT question (were these parameters stable out of sample, rather than does
/// fit-then-trade survive out of sample). So:
///
/// - the backtesthub advertises it on EVERY daemon now (it was a MOUNT fact — pushed only when the STUDIO runners were MOUNTED —
///   and an OLDER peer still behaves that way): it rides `Request::RunWalkforward`,
///   which `vike_backtesthub::backtesthub_server` serves through
///   `vike-studio-core`'s runner, called directly;
/// - the client refuses LOCALLY, WITHOUT SENDING, when the request needs it
///   ([`crate::wire_studio::WalkforwardWindowSearchWire::needs_capability`]);
/// - the SERVER re-checks: `vike_studio_core::wire_run`'s `run_walkforward_local` parses the method
///   itself and refuses an unknown one BY NAME rather than falling through to the fixed walk.
pub const FEATURE_WALKFORWARD_SEARCH: &str = "walkforward_search";

/// Gates a Studio run's SLIPPAGE MODEL — [`crate::wire_studio::StrategyEngineConfigWire`]'s
/// `slippage_model` and `slippage_tick` (the one `[engine.slippage]` table, the owner's ruling of
/// 2026-11-10). An optional field an old server would silently ignore (decision 0112, verdict 3): a
/// daemon predating it drops the model and runs the value as a flat FRACTION, so two ticks would be
/// priced as 200% of the price. So:
///
/// - the backtesthub advertises it unconditionally, a BUILD fact (`served_features`);
/// - the client refuses LOCALLY, WITHOUT SENDING, when the params need it
///   ([`crate::wire_studio::StrategyEngineConfigWire::needs_capability`]);
/// - the server re-checks nothing it need not: the profile table's own check judges the words.
pub const FEATURE_SLIPPAGE_MODEL: &str = "slippage_model";

/// Gates the strategy-FILE verbs a Studio lists, opens and saves a file with (the owner's ruling of
/// 2026-11-13: the backtesthub manages the strategy files):
/// [`Request::ListStrategyFiles`](super::Request::ListStrategyFiles) (each file of the daemon's
/// `strategies/` folder with its states),
/// [`Request::ReadStrategyFile`](super::Request::ReadStrategyFile) (one file's text) and
/// [`Request::SaveStrategyFile`](super::Request::SaveStrategyFile) (write one); negotiated per
/// decision 0112.
///
/// - the backtesthub advertises it unconditionally, a BUILD fact (`served_features`): the verbs
///   serve the folder every build has (a key-less daemon refuses the SAVE in words);
/// - the client refuses LOCALLY, WITHOUT SENDING, when it is absent
///   ([`crate::BacktesthubClient::list_strategy_files`],
///   [`crate::BacktesthubClient::read_strategy_file`],
///   [`crate::BacktesthubClient::save_strategy_file`]);
/// - an older daemon answers either verb `Error` and keeps the connection. Its `ListStrategies` is
///   no fallback: before plan 2b that verb named compiled strategies, which no file names.
pub const FEATURE_STRATEGY_FILES: &str = "strategy_files";
