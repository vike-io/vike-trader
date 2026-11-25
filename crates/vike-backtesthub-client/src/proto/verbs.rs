//! Per-verb tables: which scope may send a verb, what it is called, and how long its reply may stay
//! silent. Every classifier here is an EXHAUSTIVE match with no `_` arm, so a new wire verb fails to
//! COMPILE until somebody says which scope may send it and what it is called; WHICH arm was chosen is
//! pinned by this module's tests. Which hub a verb belongs to is `vike_hub_protocol::plane`'s roster, and
//! `verbs_tests.rs` holds this enum to it.

use super::Request;
use vike_hub_protocol::auth::VerbScope;
use vike_hub_protocol::conn::ReplyWait;

/// A request's variant name, for a log line or a refusal — never its PAYLOAD, which on the `Run*`
/// verbs is a client-supplied script and on every verb is remote text. The daemon logs refusals by it;
/// exhaustive for the same reason [`required_scope`] is.
///
/// ⚠ **It returns the WIRE tag, not the Rust identifier**, and the four `sweep` verbs are where the
/// two differ: an operator greps a capture for `RunSweepProfile`, while `RunParamscanProfile`
/// appears on no wire. The tag pins on [`Request::RunParamscan`] and its three siblings are the
/// same decision applied to the bytes; `crates/vike-backtesthub-client/src/client.rs`'s `response_kind`
/// follows it for [`Response`](super::Response).
pub fn request_kind(r: &Request) -> &'static str {
    match r {
        Request::Ping => "Ping",
        Request::RunBacktest(_) => "RunBacktest",
        Request::RunSlice { .. } => "RunSlice",
        // ⚠ WIRE tags, not the Rust identifiers — see this function's doc.
        Request::RunParamscan { .. } => "RunParamscan",
        Request::RunWalkforward { .. } => "RunWalkforward",
        Request::RunParamscanProfile { .. } => "RunParamscanProfile",
        Request::RunWalkforwardProfile { .. } => "RunWalkforwardProfile",
        Request::RunStudy(_) => "RunStudy",
        Request::RunNamed(_) => "RunNamed",
        Request::NamedStrategies => "NamedStrategies",
        Request::ListStrategies => "ListStrategies",
        Request::ListStrategyFiles => "ListStrategyFiles",
        Request::ReadStrategyFile { .. } => "ReadStrategyFile",
        Request::SaveStrategyFile { .. } => "SaveStrategyFile",
        Request::BuildStrategyFile { .. } => "BuildStrategyFile",
        Request::Summarized(_) => "Summarized",
        Request::ReadTrades { .. } => "ReadTrades",
        Request::ReadPoint { .. } => "ReadPoint",
        Request::ReadCurve { .. } => "ReadCurve",
        Request::ReadParamscanRows { .. } => "ReadParamscanRows",
        Request::ListStudies => "ListStudies",
        Request::ListRuns { .. } => "ListRuns",
        Request::ReadRunArtifact { .. } => "ReadRunArtifact",
        Request::ListSdks => "ListSdks",
        Request::ReadSdkFile { .. } => "ReadSdkFile",
        Request::Fresh(_) => "Fresh",
    }
}

/// Whether the backtesthub may answer `request` from an identical run it saved (decision 0078's amendment of
/// 2026-11-20): a profile's run verb, plain or summarized, or a Studio run SUMMARIZED. The whole Studio verbs are
/// not: a desktop from before 2026-11-16 sends them and could show no "answered from saved run". What
/// [`crate::BacktesthubClient::set_fresh`] wraps in [`Request::Fresh`]; the backtesthub asks it too.
pub fn may_answer_from_saved(request: &Request) -> bool {
    match request {
        Request::RunBacktest(_)
        | Request::RunParamscanProfile { .. }
        | Request::RunWalkforwardProfile { .. } => true,
        Request::Summarized(inner) => matches!(
            **inner,
            Request::RunBacktest(_)
                | Request::RunParamscanProfile { .. }
                | Request::RunSlice { .. }
                | Request::RunParamscan { .. }
                | Request::RunWalkforward { .. }
        ),
        _ => false,
    }
}

/// Classify one request's [`ReplyWait`]. **EXHAUSTIVE — no `_` arm, deliberately**: a new verb fails to
/// COMPILE here until somebody says who bounds its work, so it cannot silently inherit a bound that
/// clips it or none that would park its caller.
///
/// - **[`ReplyWait::Unbounded`]** — every verb that runs an engine: the six Rhai/profile `Run*` verbs,
///   [`Request::RunStudy`] and [`Request::RunNamed`] (its bar window is capped, the engine's cost per
///   bar is not); and [`Request::BuildStrategyFile`], whose answer is on the far side of a `cargo build`
///   (minutes on a cold dependency tree).
/// - **[`ReplyWait::Bounded`]** — `Ping`, the two rosters, the strategy-file list and read, the held
///   runs' reads, the study list, the runs page and a run file's page, and the SDK list and an SDK
///   file.
pub fn reply_wait(request: &Request) -> ReplyWait {
    match request {
        Request::RunBacktest(_)
        | Request::RunSlice { .. }
        | Request::RunParamscan { .. }
        | Request::RunWalkforward { .. }
        | Request::RunParamscanProfile { .. }
        | Request::RunWalkforwardProfile { .. }
        | Request::RunStudy(_)
        | Request::RunNamed(_)
        | Request::BuildStrategyFile { .. } => ReplyWait::Unbounded,

        Request::Ping
        | Request::ListStrategies
        | Request::NamedStrategies
        | Request::ListStrategyFiles
        | Request::ReadStrategyFile { .. }
        | Request::SaveStrategyFile { .. }
        // A held run's page or point: memory the daemon already holds (a curve page, or one read of a
        // saved run's curve file).
        | Request::ReadTrades { .. }
        | Request::ReadPoint { .. }
        | Request::ReadCurve { .. }
        | Request::ReadParamscanRows { .. }
        // The server's study folder, a page of its runs and a 64 KiB page of one run's file: a scan
        // and a read, never a run.
        | Request::ListStudies
        | Request::ListRuns { .. }
        | Request::ReadRunArtifact { .. }
        // The server's SDK list, and one SDK file (capped) or folder: a scan and a read.
        | Request::ListSdks
        | Request::ReadSdkFile { .. } => ReplyWait::Bounded,

        // The summarized run, and the run asked fresh, wait as long as the run they wrap.
        Request::Summarized(inner) | Request::Fresh(inner) => reply_wait(inner),
    }
}

/// Classify one request's required scope. **EXHAUSTIVE — no `_` arm, deliberately**: a default of
/// `Read` would leak a write, and a default of `Write` would silently break a read for read-scope
/// clients. Neither is a decision a wildcard should make.
///
/// - **[`Request::ListStrategies`] is [`VerbScope::Read`]** (the folder's paths, answered by the
///   compute daemon), and so are [`Request::ListStrategyFiles`] and [`Request::ReadStrategyFile`]:
///   they describe and read that folder, and compile and write nothing. `Ping` is `Read`, not served before the handshake: an unauthenticated liveness probe would be a free
///   oracle for "is this address a vike daemon".
/// - **⚠ ALMOST every `Run*` verb is [`VerbScope::Write`], for the SOURCE it can carry rather than
///   the act of running** — the classification a reader most often gets wrong, since these verbs
///   return answers and look like reads. `RunSlice`/`RunSweep`/`RunWalkforward` carry a
///   [`StrategySpecWire`](crate::wire_studio::StrategySpecWire)`::Rhai` source that reaches `StrategySpec::rhai`,
///   and `RunBacktest`/`RunParamscanProfile`/`RunWalkforwardProfile` carry a profile whose Rhai
///   strategy reaches the same compiler (`ProfileStrategy::build`,
///   `crates/vike-backtest/src/run/strategy_builder.rs`): remote code execution BY DESIGN
///   (the Studio's Source backend), whose posture is the write scope. [`Request::RunStudy`] compiles nothing
///   that the client supplies (a Rhai study is a file of the server's own study folder, run inside the
///   daemon; a Rust one a file it builds and runs sandboxed: `vike_backtesthub`'s `studio_run::run_study_local`)
///   and is `Write` for the OTHER reason: it WRITES a run directory on the server's disk
///   (`docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 6).
///
///   ⚠ **And [`Request::RunNamed`] is [`VerbScope::Read`]**, which is 0064's subject. The predicate
///   above names FIELDS: this verb carries [`crate::named_run::NamedRunParam`] — `i64`/`f64`/`bool`,
///   with no variant a script could occupy — and its server half resolves no name (plan 2b), so a
///   `src` is UNREAD rather than refused. Its COST bounds (the constants in
///   [`crate::named_run`] plus a server-side run-slot count) are the CONDITION of the
///   classification, not a consequence: 0064's decision 3 found that the denial of service survives
///   removing the compiler. **Moving this line is re-deciding that record, not a
///   reclassification.**
///
///   The cost of the `Write` rows: a `Read` client cannot run a SCRIPT, a SEARCH or a walk-forward,
///   and cannot have a run persisted; it can run one named strategy over one bounded window — the
///   shape a device whose role is `observe` needs (decision 0288: the role, never a held key,
///   decides who may compile client-supplied Rhai).
pub fn required_scope(request: &Request) -> VerbScope {
    match request {
        Request::Ping
        | Request::ListStrategies
        | Request::ListStrategyFiles
        | Request::ReadStrategyFile { .. }
        // The server's study folder, by NAME (each study's tier and recipe file names, no text): it
        // describes a folder, as the strategy-file list does. Running one is `RunStudy`, Write.
        | Request::ListStudies
        // The versioned strategy SDK's source (decision 0296, verdict 3): the SDKs the server holds and
        // one of their files, read-only. Shipped source that compiles and writes nothing, as the
        // strategy-file read above is.
        | Request::ListSdks
        | Request::ReadSdkFile { .. }
        // ⚠⚠ **THE NAMED RUN, THE ONLY `Run*` VERB ON THIS SIDE — READ
        // `docs/decisions/0064-a-named-run-carries-no-source.md` BEFORE MOVING THIS ARM OR ADDING
        // A FIELD TO THE VARIANT.** SOURCE: this function's doc (its server half resolves no name
        // and reads no params table, `vike_backtesthub::named_run`'s module doc). COST,
        // BUILT rather than argued (0064's decision 3): the SINGLE-POINT shape (no grid, no trials,
        // no splits), a window capped by `crate::named_run::NAMED_RUN_MAX_BARS` and REFUSED rather
        // than clamped, a server-owned interval set, the seed/catalog symbol and venue validators,
        // and a process-wide run-slot count that refuses rather than queues. WRITES: nothing —
        // persisting is `RunStudy`, which stays Write (decision 4). These bounds are the CONDITION
        // of this arm; adding a search dimension is 0064's first reopener.
        | Request::RunNamed(_)
        // ...and the roster it enumerates, answerable to the same credential that runs it (0064's
        // decision 7) — otherwise a client names into the dark.
        | Request::NamedStrategies => VerbScope::Read,

        // The six that compile client-supplied Rhai — see the ⚠ in this function's doc.
        Request::RunBacktest(_)
        | Request::RunSlice { .. }
        | Request::RunParamscan { .. }
        | Request::RunWalkforward { .. }
        | Request::RunParamscanProfile { .. }
        | Request::RunWalkforwardProfile { .. }
        // A study RUNS code on the backend and WRITES a run directory there;
        // `crates/vike-cli/src/cmd/research/study.rs`'s connect already negotiates under this scope.
        | Request::RunStudy(_)
        // WRITES a strategy FILE: code a later run, or a live mount at its next start, executes.
        // The daemon also refuses it on a connection that authenticated nothing (no hub keys).
        | Request::SaveStrategyFile { .. }
        // Hands source to `cargo` on the server (through the builder this daemon alone dials), whose
        // artifact a later run loads. Refused, like the save, by a daemon with no hub keys.
        | Request::BuildStrategyFile { .. }
        // A summarized run's trades, points, whole curve and a search's rows: only a client that may run one
        // reads one back.
        | Request::ReadTrades { .. }
        | Request::ReadPoint { .. }
        | Request::ReadCurve { .. }
        | Request::ReadParamscanRows { .. }
        // A saved run's list and its files: only a client that may run something reads results back,
        // as a held run's trades and curve above.
        | Request::ListRuns { .. }
        | Request::ReadRunArtifact { .. } => VerbScope::Write,

        // The summarized run, and the run asked fresh, need what the run they wrap needs.
        Request::Summarized(inner) | Request::Fresh(inner) => required_scope(inner),
    }
}

#[path = "verbs_tests.rs"]
#[cfg(test)]
mod verbs_tests;
