//! The injected runner seams: the Studio dispatch table and the study runner, over wire DTOs only.

use vike_datahub_client::WireStudy;
#[cfg(doc)]
use vike_datahub_client::proto::Response;
use vike_datahub_client::wire_studio::{
    WireEngineParams, WireParamscan, WireParamscanResult, WireRunError, WireRunResult, WireSlice,
    WireSpec, WireWalkforward, WireWalkforwardResult,
};

use super::StoreHandle;
#[cfg(doc)]
use super::verbs::study_verb_unmounted;

/// The `RunSlice` runner, injected. `(spec, slice, params, store) -> the rendered answer`.
///
/// Every parameter and both result types are `Wire*` DTOs from `vike-datahub-client`, which is the
/// property that makes the whole seam work: this crate can name them (layer 25 < 30) while it
/// cannot name `vike_studio_core::DataSlice`/`StrategySpec`/`RunError` (layer 35 > 30). The real
/// implementation is `vike_studio_core::wire_run`'s `run_slice_local` — the SAME entry its parity
/// test drives, so "run locally" and "run over this wire" stay one computation.
pub type StudioSliceFn = Box<
    dyn Fn(
            &WireSpec,
            &WireSlice,
            Option<&WireEngineParams>,
            StoreHandle,
        ) -> Result<WireRunResult, WireRunError>
        + Send
        + Sync,
>;

/// The `RunSweep` runner, injected — [`StudioSliceFn`] plus the parameter grid.
pub type StudioParamscanFn = Box<
    dyn Fn(
            &WireSpec,
            &WireSlice,
            &WireParamscan,
            Option<&WireEngineParams>,
            StoreHandle,
        ) -> Result<WireParamscanResult, WireRunError>
        + Send
        + Sync,
>;

/// The `RunWalkforward` runner, injected — [`StudioSliceFn`] plus the split count.
pub type StudioWalkforwardFn = Box<
    dyn Fn(
            &WireSpec,
            &WireSlice,
            &WireWalkforward,
            Option<&WireEngineParams>,
            StoreHandle,
        ) -> Result<WireWalkforwardResult, WireRunError>
        + Send
        + Sync,
>;

/// The three STUDIO runners, mounted together or not at all.
///
/// Together, because they are one capability from a client's point of view: a Studio pointed at a
/// daemon that could run a slice but not a sweep would have to discover the difference one verb at
/// a time. `vike_studio_core::wire_run`'s `studio_run_table` is the production constructor and the
/// only place the three real runners are named; a test may build one from its own closures, which
/// is how the roundtrip suite drives the verbs without linking the studio tree.
pub struct StudioRunTable {
    slice: StudioSliceFn,
    paramscan: StudioParamscanFn,
    walkforward: StudioWalkforwardFn,
}

impl StudioRunTable {
    /// A table from three explicit runners — the ONLY constructor, so a mount is always a
    /// deliberate act by a composition root that could name the real ones.
    pub fn new(
        slice: StudioSliceFn,
        paramscan: StudioParamscanFn,
        walkforward: StudioWalkforwardFn,
    ) -> Self {
        Self { slice, paramscan, walkforward }
    }

    /// The `RunSlice` runner.
    pub fn slice(&self) -> &StudioSliceFn {
        &self.slice
    }

    /// The `RunParamscan` runner.
    pub fn paramscan(&self) -> &StudioParamscanFn {
        &self.paramscan
    }

    /// The `RunWalkforward` runner.
    pub fn walkforward(&self) -> &StudioWalkforwardFn {
        &self.walkforward
    }
}

/// The STUDY runner, injected. `(request, store) -> the run's own JSON document`.
///
/// ⚠ **Injected for the same reason [`StudioRunTable`] is, and it is the only shape available.**
/// `vike_studio_core::study_dispatch`'s `run_study_plan` sits at layer 35 and DEPENDS on this crate
/// at 30, so this crate can neither call it nor name its types — only a composition root that can
/// see both may hand it down. `crates/vike/src/main.rs`'s `backtest_main` does; the standalone
/// `src/bin/backtest.rs` passes `None` and this daemon then refuses the verb by name
/// ([`study_verb_unmounted`]).
///
/// ⚠ **A SEPARATE seam rather than a fourth runner on [`StudioRunTable`]**, whose own doc argues
/// its three are ONE capability. A study negotiates its own capability string
/// (`vike_datahub_client::proto`'s `FEATURE_STUDY`), belongs to a different plane in the CLI's
/// vocabulary (ruling R1 of
/// `docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md`), and can be mounted
/// independently — folding it in would make that string mean "the Studio runners are mounted",
/// which is a sentence nobody could read off the wire.
///
/// The answer is JSON TEXT because [`Response::StudyReport`] is: the run manifest and study outcome
/// are Serialize-only, and one direction is all this verb needs.
pub type StudyRunFn = Box<dyn Fn(&WireStudy, StoreHandle) -> Result<String, String> + Send + Sync>;

/// How a composition root NAMES the study runner without resolving the daemon's own directories.
///
/// ⚠ **A FACTORY rather than a built [`StudyRunFn`], and `crates/vike-ops/tests/wiring/multicall_gate.rs`
/// is why.** The `vike-backend` dispatcher may perform exactly ONE `std::env::vars()` sweep and ONE
/// `current_dir()` and then hand over — `the_dispatcher_starts_nothing` fails a PR that calls
/// `state_path::` there at all, because any project-relative resolution in the dispatcher is the
/// second walk in another costume. So the root passes the FUNCTION ITEM
/// (`vike_studio_core::study_run_fn`), and `crate::backtest_cli`'s `--addr` arm — which already
/// owns the walk that resolves this daemon's settings — supplies the runs root and the pinned
/// trainer it resolved.
///
/// A plain `fn` pointer, not a boxed closure: the root captures nothing, and a function item is
/// what makes that visible at the call site.
pub type StudyRunFactory = fn(std::path::PathBuf, Option<std::path::PathBuf>) -> StudyRunFn;
