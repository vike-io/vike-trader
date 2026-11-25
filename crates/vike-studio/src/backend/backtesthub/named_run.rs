//! The NAMED RUN (`docs/decisions/0064-a-named-run-carries-no-source.md`): the Studio's second
//! backend, and the only one an OBSERVE credential reaches. The conversion that refuses each bound
//! by name ([`to_named_run_spec`]), the run ([`spawn_backtesthub_named_run`]), the Saved pane's
//! "Compare all" on this backend ([`spawn_backtesthub_named_compare`]) and the roster a picker
//! offers ([`spawn_backtesthub_named_roster`]). Every dial here is a Read dial
//! (`super`'s `connect_observe`).

use std::sync::mpsc::Receiver;

use vike_analytics::results::periods_per_year_for;
use vike_backtesthub_client::named_run::{
    NamedRunOutcome, NamedRunParam, NamedRunRefusal, NamedRunRoster, NamedRunSpec,
    validate_named_run,
};
use vike_backtesthub_client::summary::summarize;
use vike_backtesthub_client::wire_studio::TradingRecordWire;
use vike_hub_protocol::auth::HubKeys;
use vike_model::params::Params;
use vike_model::run_request::replay::ReplayKind;
use vike_studio_core::{DataSlice, RunError, StrategySpec, spawn_outcome};

use super::{
    StudioRunBackend, StudioRunView, StudioRunViewOutcome, StudioSavedCompareOutcome, compare_each,
    connect_observe,
};

/// The refusal a SEARCH-shaped action gets on [`StudioRunBackend::Named`], or `None` when it may proceed.
///
/// ⚠ **This is a BOUND, not a missing feature, and the sentence says so** —
/// `docs/decisions/0064-a-named-run-carries-no-source.md`'s decision 3 makes the single-point shape
/// the largest of that verb's bounds and a STRUCTURAL one: `NamedRunSpec` has no field a grid, a
/// trial count or a split count could occupy, which is what makes
/// `vike_backtest::search::paramscan`'s product over client-supplied arrays (capped, but each point a
/// backtest) unreachable from an Observe credential. Growing a search dimension is that record's FIRST
/// reopener.
///
/// Pure, so the Studio's whole named-backend refusal surface is testable with no socket.
pub(crate) fn named_backend_search_refusal(
    backend: &StudioRunBackend,
    action: &str,
) -> Option<String> {
    matches!(backend, StudioRunBackend::Named { .. }).then(|| {
        format!(
            "the Named backend runs ONE parameter set, so it cannot {action}: a grid, a trial count \
             and a split count are cost terms the request would be naming, and the verb is \
             read-scope precisely because it names none of them. Switch the Backend to Source, \
             which signs with this PC's device key and needs it listed at the role `control`, \
             or {action} from `vike-cli` on the daemon's own box."
        )
    })
}

/// **Run a strategy the SERVER already holds** — the Studio's second backend, and the only one an
/// OBSERVE credential can reach.
///
/// [`StudioRunBackend::Source`] needs this PC's device key listed at the role `control`, because every `Run*`
/// verb it sends is Control-scoped (decision 0288; until its cutover Source signed with a shared datahub
/// CONTROL key, decision 0083's question 1). This backend is the one an `observe` role reaches: it makes
/// the narrow case need no Write scope at all, and it is still the right choice for a device the server
/// lists at `observe`.
///
/// What it costs, stated where somebody choosing it will read it: no script, no sweep, no
/// walk-forward, one symbol, one bounded window. [`to_named_run_spec`] refuses each of those BY
/// NAME rather than silently narrowing the run.
pub(crate) fn spawn_backtesthub_named_run(
    address: String,
    keys: Option<HubKeys>,
    spec: StrategySpec,
    slice: DataSlice,
) -> Receiver<StudioRunViewOutcome> {
    spawn_outcome(move || {
        let whole = run_named_whole(&address, keys.as_ref(), &spec, &slice)?;
        // A named run's bounded window crosses whole (decision 0064), so it is summarized HERE, by the
        // summarizer the backtesthub runs over a Studio run, and its trades stay in hand.
        let periods = periods_per_year_for(Some(slice.interval.as_str()));
        let (summary, trades) = summarize(whole, periods);
        Ok(StudioRunView { summary, trades: Some(trades) })
    })
}

/// Dial the backtesthub as OBSERVE, ship the named run, and answer its whole result, or the refusal
/// in words.
fn run_named_whole(
    address: &str,
    keys: Option<&HubKeys>,
    spec: &StrategySpec,
    slice: &DataSlice,
) -> Result<TradingRecordWire, RunError> {
    let named = to_named_run_spec(spec, slice)?;
    let mut client = connect_observe(address, keys)?;
    match client.run_named(&named).map_err(RunError::Data)? {
        // ⚠ `report_json` is DISCARDED here, and deliberately. The wire carries it beside the curve
        // so a client need not re-implement sharpe/return/max_dd — but the Studio summarizes a named
        // run with the summarizer the backtesthub runs over a Source run
        // (`vike_backtesthub_client::summary::summarize`), so the two backends print one rendering of
        // one run. The field is the right shape for a consumer that has no metrics of its own
        // (`vike-cli`), which is why it is on the wire.
        NamedRunOutcome::Ran { result, .. } => Ok(*result),
        // ⚠ NOT an error on the wire, and it must not become a silent empty result here either: the
        // operator of THAT box has not armed the lane, and the fix is on their side. The note names
        // the variable and the command — the teaching-refusal shape.
        NamedRunOutcome::NotArmed => {
            Err(RunError::Data(NamedRunRoster::unarmed_note().to_string()))
        }
        NamedRunOutcome::Refused(NamedRunRefusal::UnknownStrategy { known }) => {
            Err(RunError::Strategy(format!(
                "this backend does not hold a strategy called {:?}. It can run: {}. \
                 A named run resolves only strategies compiled into the SERVER — shipping your own \
                 source is the Control-scope path: the Source backend here, signed with this PC's \
                 device key at the role `control`, or a strategy file under strategies/ named \
                 with `[strategy] file` in the profile of `vike-cli backtest run`.",
                named.strategy,
                known.join(", ")
            )))
        }
        NamedRunOutcome::Refused(NamedRunRefusal::NoSlot { limit }) => {
            Err(RunError::Data(format!(
                "this backend is already running its maximum of {limit} named runs and REFUSES rather \
             than queueing — a queued run is a connection held open for an unbounded time. Try \
             again in a moment."
            )))
        }
    }
}

/// Ask the backtesthub which strategies it would run, and whether the lane is armed — the roster
/// a picker offers, which by 0064's decision 7 must be the roster the verb actually SERVES.
///
/// ⚠ Not `list_strategies`: that answers the daemon's SIMULATOR roster, which both over- and
/// under-states what a named run can resolve. See `Request::NamedStrategies`.
///
/// ⚠ **BLOCKING — call it from [`spawn_backtesthub_named_roster`], never from a frame.** The verb itself
/// is trivial (a compile-time const plus a build-time generated roster, no store read, no engine),
/// and the first draft of this feature reasoned from exactly that and put the call on the egui
/// update loop. The cost that matters is not the VERB, it is the DIAL:
/// `vike_hub_protocol::limits::CLIENT_CONNECT_TIMEOUT` (which `vike_datahub_client` dials with) is ten seconds PER RESOLVED ADDRESS, and its own module
/// doc records that name resolution above it is unbounded — so a stale tunnel or a dark IPv6 route
/// is a frozen GUI, not a slow one.
pub(crate) fn backtesthub_named_roster(
    address: &str,
    keys: Option<&HubKeys>,
) -> Result<NamedRunRoster, String> {
    connect_observe(address, keys).map_err(|e| e.to_string())?.named_strategies()
}

/// [`backtesthub_named_roster`] on a worker thread — the shape every other dial in this module takes,
/// and the one the UI must use.
pub(crate) fn spawn_backtesthub_named_roster(
    address: String,
    keys: Option<HubKeys>,
) -> Receiver<Result<NamedRunRoster, String>> {
    spawn_outcome(move || backtesthub_named_roster(&address, keys.as_ref()))
}

/// "Compare all" on the [`StudioRunBackend::Named`] backend: one NAMED run per saved row, so a row this
/// backend cannot run (a script, a strategy file) reports that run's refusal as its result.
pub(crate) fn spawn_backtesthub_named_compare(
    address: String,
    keys: Option<HubKeys>,
    rows: Vec<(String, StrategySpec)>,
    slice: DataSlice,
) -> Receiver<StudioSavedCompareOutcome> {
    spawn_outcome(move || {
        compare_each(rows, |spec| {
            // A named run's bounded window crosses whole (decision 0064) and is summarized HERE, as a
            // Named Run is ([`spawn_backtesthub_named_run`]): a Compare-all row is a summary on either
            // backend (decision 0078's amendment of 2026-11-18).
            let whole = run_named_whole(&address, keys.as_ref(), spec, &slice)?;
            Ok(summarize(whole, periods_per_year_for(Some(slice.interval.as_str()))).0)
        })
    })
}

/// **`(StrategySpec, DataSlice)` → [`NamedRunSpec`], or the ONE bound that refused it, BY NAME.**
///
/// Pure, so the whole of this backend's refusal surface is testable with no socket and no store —
/// and every arm names the dimension it hit rather than narrowing the run to fit. That is
/// `docs/decisions/0062`'s decision 5 applied here: answering a different question than the one
/// asked, and reporting it as the answer, is the lie these bounds exist to avoid.
pub(crate) fn to_named_run_spec(
    spec: &StrategySpec,
    slice: &DataSlice,
) -> Result<NamedRunSpec, RunError> {
    let (name, params) = match spec {
        // ⚠ THE RECORD, in one arm. A named run has no field a script could occupy — this is not a
        // check that could be relaxed, it is the shape of `NamedRunSpec`.
        StrategySpec::Rhai(_) | StrategySpec::RhaiWithParameters { .. } => {
            return Err(RunError::Strategy(
                "the Named backend runs strategies the SERVER already holds, so it carries no \
                 source — there is no field on the request a script could occupy. Switch the \
                 Strategy source to Strategy files and pick a name off this backend's \
                 roster, or send the script through the Source backend. Shipping source to a \
                 server is the \
                 Control-scope path, which Source takes with this PC's device key at the role \
                 `control` (docs/decisions/0064-a-named-run-carries-no-source.md is why a named run \
                 carries none)."
                    .to_string(),
            ));
        }
        // A named run builds nothing, so an SDK picked for the file would be read by nothing: refused, by name.
        StrategySpec::File { sdk: Some(sdk), .. } => {
            return Err(RunError::Strategy(format!(
                "the Named backend runs strategies the SERVER already holds and builds nothing, so \
                 it cannot run on SDK {sdk}: clear the SDK, or run through the Source backend"
            )));
        }
        StrategySpec::File { file: name, params, sdk: None } => (name, params),
        // A strategy file resolves through the LOADER (an already-built artifact named by sha), not
        // through this daemon's compiled-in roster (empty since plan 2b deleted the compiled
        // strategies) — the exact roster `Request::NamedStrategies`
        // enumerates. So a strategy file's `name` cannot be looked up here any more than a Rhai script's
        // can, and for the same reason: the Named backend runs strategies the SERVER already
        // holds, and a runtime-loaded strategy file is not one of them.
        StrategySpec::StrategyFile { .. } => {
            return Err(RunError::Strategy(
                "the Named backend runs strategies the SERVER already holds by NAME, and a \
                 Rust strategy you build in Studio is not on that roster — it is resolved by sha \
                 from an artifact this daemon was never handed. Switch the Strategy source to \
                 Strategy files and pick a name off this backend's roster, or use the \
                 Source backend once the strategy has been built."
                    .to_string(),
            ));
        }
    };
    if slice.kind != ReplayKind::Bar {
        return Err(RunError::Data(
            "the Named backend runs the BAR lane only: its window ceiling is counted in BARS, so a \
             tick slice is a dimension that bound cannot see. Pick a bar slice, or use the Source \
             backend."
                .to_string(),
        ));
    }
    let [symbol] = slice.symbols.as_slice() else {
        return Err(RunError::Data(format!(
            "the Named backend runs ONE symbol and this slice names {}. A symbol list is a cost \
             term the request would be naming, which is the thing this verb's classification rests \
             on not doing.",
            slice.symbols.len()
        )));
    };
    // ⚠ BOTH bounds required, and an absent one is the shape that means "the whole store"
    // everywhere else on this wire — which is precisely the unbounded window 0064's decision 3
    // found nothing bounding on the verbs that do carry it.
    let (Some(start), Some(end)) = (slice.range.start, slice.range.end) else {
        return Err(RunError::Data(
            "the Named backend needs BOTH ends of its window: an open bound means `the whole \
             store`, and an unbounded window is the cost term this verb may not carry. Set a From \
             and a To on the slice."
                .to_string(),
        ));
    };
    let params = named_params(params)?;
    let named = NamedRunSpec {
        strategy: name.clone(),
        params,
        venue: slice.venue.clone(),
        symbol: symbol.clone(),
        interval: slice.interval.clone(),
        start,
        end,
    };
    // The SAME validator the server runs at its own door, so the sentence an operator reads here is
    // the sentence the server would have answered with — and no frame is written for a request that
    // cannot be served.
    validate_named_run(&named).map_err(RunError::Data)?;
    Ok(named)
}

/// A native strategy's `Params` → the wire's [`NamedRunParam`] list, each knob through
/// `NamedRunParam::from_param` (the encode beside the server's decode), whose refusal of a string knob
/// arrives here as the strategy error it is.
fn named_params(params: &Params) -> Result<Vec<(String, NamedRunParam)>, RunError> {
    params
        .into_iter()
        .map(|(key, value)| {
            NamedRunParam::from_param(key, value)
                .map(|p| (key.clone(), p))
                .map_err(RunError::Strategy)
        })
        .collect()
}
