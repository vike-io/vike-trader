//! `readmit` — on boot, tell each engine again which symbols it trades beyond the one it was started
//! on, with fresh facts (decision 0173, item 8; decision 0174).
//!
//! A restart restores positions and resting orders for EVERY symbol (and, with no mark, every symbol an
//! earlier run admitted, held or flat: marks are market data, never restored), and the engine's own
//! `restore_runtime_state` puts back the symbols a previous run admitted on demand (in
//! `extra_symbols`, with the on-demand grid row it had then), so the snapshot LISTS them. What it
//! cannot restore is a venue's CURRENT tick and lot, or the price feed of the symbol: [`targets`] reads the symbols off the journal's restored engines and [`readmit`] sends one
//! `AdmitSymbol` for each (a refresh), on the core's command lane, BEFORE the reconcile driver is
//! spawned: the lane is FIFO, so every admission folds ahead of the first `ReconcileReports`.
//!
//! The facts come from the same [`SymbolFacts`] the control door uses, on the boot thread, so a venue
//! that cannot answer costs a logged error per symbol and nothing else: the symbol keeps the grid the
//! journal restored, and the door refreshes it on the next order. Nothing here refuses a start.
//!
//! A LANE TWIN's own symbol (`binance#PERP`'s `BTCUSDT.P`) is the engine's mounted symbol, so [`targets`]
//! never lists it and [`readmit_and_price`] never prices it, and no strategy mounts it either:
//! [`twin_targets`] names the twins this boot built and [`warm_twins`] opens their price lanes, each on a
//! short-lived thread the boot does not wait for.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_core::RETIRED_ROUTE_KEY_SUFFIX;
use vike_exec::{Command, EngineSnapshot, SymbolAdmission};
use vike_gatewayhub_client::proto::FACTS_BUSY;
use vike_model::accounts::account_keys::{AccountLabel, route_key_of};
use vike_secrets::LANE_TWINS;
use vike_secrets::account_directory::AccountDirectory;

use crate::server::symbol_admission::{SymbolFacts, with_confirmed_leverage};

/// Restate `sent`, the admission just sent for a symbol, with the leverage the daemon's commit confirmed
/// for it (`SymbolFacts::confirmed_leverage`): a SECOND `AdmitSymbol`, the symbol's own margin rate, on the
/// same lane. Nothing confirmed, or `sent` already says it, sends nothing. Answers what is now the latest
/// admission the engine holds for the symbol, so a later commit restates against it and not against the
/// first.
fn restate_leverage(
    facts: &dyn SymbolFacts,
    sent: SymbolAdmission,
    send: &mut impl FnMut(Command),
) -> SymbolAdmission {
    match with_confirmed_leverage(facts, &sent) {
        Some(second) => {
            tracing::info!(
                route_key = %second.route_key, symbol = %second.symbol,
                im_requirement = ?second.im_requirement,
                "boot: restating the re-admitted symbol with its own margin rate (the venue holds \
                 the account's leverage for it)"
            );
            send(Command::AdmitSymbol(Box::new(second.clone())));
            second
        }
        None => sent,
    }
}

/// The `why` of a target the pass did not ask because its budget was spent ([`Pace`]).
pub const OVER_BUDGET: &str = "the boot's re-admission budget was spent before this symbol's turn";

/// One symbol an engine must be told about again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The account's engine (`ExecutionEngine::route_key`).
    pub route_key: String,
    /// Its venue.
    pub venue: String,
    /// The symbol, spelled as the engine holds it.
    pub symbol: String,
}

/// The symbols the restored `engines` trade beyond the symbol each was started on and its DECLARED
/// extras, in first-seen order and without repeats: every symbol an earlier run ADMITTED (even though the
/// snapshot lists it in `extra_symbols`, and whether or not the engine still holds a position or a live
/// order in it), then any other symbol a position or a live order names. A STOPPED engine's tombstone
/// (a route key ending in [`RETIRED_ROUTE_KEY_SUFFIX`], which the core journals a stopped primary
/// under once a newer engine of its account replaced it, decision 0169) is not an engine of this boot:
/// the boot restore SKIPS that snapshot, so it holds no lease and has nothing to be told. It yields no
/// target, said once at `info`. No running engine can carry the suffix: a label is `[A-Z0-9]+`
/// (`vike_model::accounts::account_keys::AccountLabel::parse`), so `#stopped` is never one.
pub fn targets(engines: &[EngineSnapshot]) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    for snap in engines {
        let route_key = snap.route_key.clone().unwrap_or_else(|| snap.venue.clone());
        if route_key.ends_with(RETIRED_ROUTE_KEY_SUFFIX) {
            tracing::info!(
                %route_key, symbols = ?snap.extra_symbols,
                "boot: `{route_key}` is the journal's record of an engine that was stopped and \
                 replaced (decision 0169); it runs no engine and holds no lease, so none of its \
                 symbols is re-admitted"
            );
            continue;
        }
        let held = snap
            .account
            .positions
            .iter()
            .filter(|(_, entry)| entry.size != 0.0)
            .map(|((_, symbol, _), _)| symbol.as_str())
            .chain(
                snap.registry
                    .iter()
                    .filter(|(_, order)| !order.status.is_terminal())
                    .map(|(_, order)| order.request.symbol.as_str()),
            );
        // EVERY symbol an earlier run admitted, held or not: a flat symbol with no resting order is
        // restored into the engine too (`restore_runtime_state`) but with no mark, and nothing but this
        // pass subscribes its price (v0.1.66 on the CI box: a restored flat ETHUSDT was denied
        // `no-reference-price` for ever). Then the symbols a position or a live order names.
        let admitted = snap
            .extra_symbols
            .iter()
            .filter(|s| snap.limits.grid_by_symbol.get(s.as_str()).is_some_and(|g| g.on_demand))
            .map(String::as_str);
        for symbol in admitted.chain(held) {
            // KNOWN to the mount is the engine's own symbol and a symbol DECLARED at assembly (in
            // `extra_symbols` with a grid row that is not on-demand, or none). An ADMITTED symbol is
            // in `extra_symbols` too (the snapshot lists it and `restore_runtime_state` restores it)
            // but its row is on-demand: it still wants fresh facts and a price subscription.
            let on_demand = snap.limits.grid_by_symbol.get(symbol).is_some_and(|g| g.on_demand);
            let known = symbol == snap.symbol
                || (!on_demand && snap.extra_symbols.iter().any(|s| s == symbol));
            let seen = out.iter().any(|t| t.route_key == route_key && t.symbol == symbol);
            if !symbol.is_empty() && !known && !seen {
                out.push(Target {
                    route_key: route_key.clone(),
                    venue: snap.venue.clone(),
                    symbol: symbol.to_string(),
                });
            }
        }
    }
    out
}

/// What a restored (journaled) engine HOLDS: its route key, venue, open positions and live orders.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredHold {
    pub route_key: String,
    pub venue: String,
    /// `(symbol, size)` of every non-flat position.
    pub positions: Vec<(String, f64)>,
    /// How many orders were live (not terminal) in the journal.
    pub live_orders: usize,
}

/// The restored `engines` that hold a position or a live order, in journal order. A STOPPED engine's tombstone
/// ([`RETIRED_ROUTE_KEY_SUFFIX`]) is the journal's record of an engine that was replaced: its state is not an
/// engine of this boot and is left out, as for [`targets`]. Read BEFORE `apply_to` consumes the restore.
pub fn restored_exposure(engines: &[EngineSnapshot]) -> Vec<RestoredHold> {
    engines
        .iter()
        .filter_map(|snap| {
            let route_key = snap.route_key.clone().unwrap_or_else(|| snap.venue.clone());
            if route_key.ends_with(RETIRED_ROUTE_KEY_SUFFIX) {
                return None;
            }
            let positions: Vec<(String, f64)> = snap
                .account
                .positions
                .iter()
                .filter(|(_, entry)| entry.size != 0.0)
                .map(|((_, symbol, _), entry)| (symbol.to_string(), entry.size))
                .collect();
            let live_orders =
                snap.registry.iter().filter(|(_, order)| !order.status.is_terminal()).count();
            (!positions.is_empty() || live_orders > 0).then(|| RestoredHold {
                route_key,
                venue: snap.venue.clone(),
                positions,
                live_orders,
            })
        })
        .collect()
}

/// **A restored engine that holds something but was NOT rebuilt this boot is said at ERROR and listed NOT
/// RUNNING with what it holds** (review r2, finding 5). The boot restore puts a journaled engine's state back
/// only onto an engine the boot built under the same route key; a lane twin whose figure was cleared (no row
/// derives it) or an account deactivated since is not built, its journaled position and resting orders are
/// restored onto nothing, and the core says so at WARN only. The venue still holds them, with nothing watching.
/// `running` are the route keys of the engines the core holds; `listed` the NOT RUNNING entries the boot made
/// (an account the rows name but could not start keeps its reason, which now also names what it holds; any
/// other gets an entry of its own). Returns the route keys it flagged.
pub fn flag_unmanaged_exposure(
    holds: &[RestoredHold],
    running: &[String],
    listed: &mut Vec<vike_exec::NotRunningAccount>,
) -> Vec<String> {
    let mut flagged = Vec::new();
    for hold in holds.iter().filter(|h| !running.contains(&h.route_key)) {
        let named: Vec<String> =
            hold.positions.iter().map(|(symbol, size)| format!("{symbol} {size}")).collect();
        let mut held = Vec::new();
        if !named.is_empty() {
            held.push(format!("position open at venue: {}", named.join(", ")));
        }
        if hold.live_orders > 0 {
            held.push(format!("{} order(s) resting", hold.live_orders));
        }
        let held = held.join("; ");
        tracing::error!(
            route_key = %hold.route_key,
            "BOOT: the journal restored `{}` holding {held}, but this boot built NO engine under that key (no \
             account row runs it now), so nothing manages them: they are still at the venue. The account is \
             listed NOT RUNNING with what it holds. Flatten it at the venue (or state its row again so it runs \
             and the next restart restores onto it)",
            hold.route_key
        );
        match listed.iter_mut().find(|nr| nr.route_key == hold.route_key) {
            Some(entry) => {
                entry.reason = format!("{}; restored state is NOT managed: {held}", entry.reason)
            }
            None => listed.push(vike_exec::NotRunningAccount {
                venue: hold.venue.clone(),
                account: vike_model::accounts::account_keys::label_of_route_key(
                    &hold.venue,
                    &hold.route_key,
                )
                .filter(|label| !label.is_default()),
                route_key: hold.route_key.clone(),
                tier: None,
                reason: format!(
                    "no account runs under this key; restored state is NOT managed: {held}"
                ),
            }),
        }
        flagged.push(hold.route_key.clone());
    }
    flagged
}

/// The lane twins this boot built an engine for, each as a [`Target`] on the twin's OWN mounted symbol
/// (`vike_secrets::LANE_TWINS`' `symbol`). A twin is listed only when the rows DERIVE it
/// (`AccountDirectory::is_derived_lane_twin`: an account that states no leverage, a spot-only one, derives
/// none) and its route key is in `running` (`AssembledRuntime::live_venues`, the engines that came up: a twin
/// whose start was refused has no lease to price over). [`targets`] never lists these: the symbol is the
/// engine's own, and a twin has no strategy mount whose feed would price it.
#[must_use]
pub fn twin_targets(accounts: &AccountDirectory, running: &HashSet<String>) -> Vec<Target> {
    LANE_TWINS
        .iter()
        .filter(|row| accounts.is_derived_lane_twin(row.venue, row.label))
        .filter_map(|row| {
            let route_key = route_key_of(row.venue, &AccountLabel::parse(row.label).ok()?);
            running.contains(&route_key).then(|| Target {
                route_key,
                venue: row.venue.to_string(),
                symbol: row.symbol.to_string(),
            })
        })
        .collect()
}

/// **Open the price lane of each lane twin's own symbol at the boot** ([`twin_targets`]), announced as
/// `pricing <venue> <k>/<n>` on the `starting` stream like the pass's own pricing steps. `warm` is the
/// lane's opening (`LeaseFacts::warm_price`: the facts look-up, the datahub subscribe), and it runs on a
/// short-lived `twin-price-warm` thread per twin, the thread `account_follow`'s hot start uses, which this
/// function does NOT wait for: a lease or a datahub that never answers must not hold `ready`, and a lane
/// that did not open costs a warning there and the door's own ask at the first order, as before. The
/// handles come back for a test to join; the boot drops them.
///
/// Not a [`Missed`] of the retry thread on purpose: that retry TELLS the daemon the symbol is admitted, which
/// a twin's own symbol is not.
pub fn warm_twins(
    twins: &[Target],
    pace: &Pace<'_>,
    warm: impl Fn(&Target) + Send + Sync + 'static,
) -> Vec<std::thread::JoinHandle<()>> {
    let warm = Arc::new(warm);
    let mut started = Vec::new();
    for (i, t) in twins.iter().enumerate() {
        pace.say("pricing", t, i + 1, twins.len());
        let spawned = std::thread::Builder::new().name("twin-price-warm".into()).spawn({
            let (t, warm) = (t.clone(), Arc::clone(&warm));
            move || (*warm)(&t)
        });
        match spawned {
            Ok(handle) => started.push(handle),
            Err(e) => tracing::warn!(
                error = %e, route_key = %t.route_key, symbol = %t.symbol,
                "boot: the lane twin's price lane could not be opened (no thread); its first order asks"
            ),
        }
    }
    started
}

/// What [`readmit`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Readmitted {
    /// Admissions sent.
    pub sent: usize,
    /// The targets those admissions were for, so their prices can be subscribed once the feeds are up.
    pub admitted: Vec<Target>,
    /// `(route key, symbol, why)` for each symbol the facts source could not answer.
    pub failed: Vec<(String, String, String)>,
}

/// How long the boot's whole pass ([`readmit_and_price`]) may take before it hands the rest to the retry
/// thread. Each symbol is a gatewayhub round trip with a venue HTTP call behind it, bounded one at a time
/// (`LeaseFacts::begin_boot`) but not in sum, so N slow-but-successful lookups used to add up without
/// limit while `ready` waited. The deploy helper's window follows the `starting` lines, so the pass must
/// also end on its own.
pub const READMIT_BUDGET: Duration = Duration::from_secs(60);

/// How the boot's pass reports itself and bounds itself: [`Pace::default`] does neither.
#[derive(Clone, Copy)]
pub struct Pace<'a> {
    /// Told `readmitting <venue> <k>/<n>` (then `pricing <venue> <k>/<n>`) before each symbol, so the
    /// `{"kind":"starting"}` stream moves with the pass instead of going quiet until its end.
    progress: Option<&'a dyn Fn(&str)>,
    /// The pass's whole budget, from `started`; `None` = unbounded.
    budget: Option<Duration>,
    started: Instant,
    now: fn() -> Instant,
    /// How a busy gatewayhub is waited out ([`BUSY_PAUSE`]); the tests move their fake clock instead.
    sleep: fn(Duration),
}

impl Default for Pace<'_> {
    fn default() -> Self {
        Pace {
            progress: None,
            budget: None,
            started: Instant::now(),
            now: Instant::now,
            sleep: std::thread::sleep,
        }
    }
}

impl<'a> Pace<'a> {
    /// A pass that reports to `progress` and ends after `budget`, counted from now.
    #[must_use]
    pub fn new(budget: Duration, progress: &'a dyn Fn(&str)) -> Self {
        Self::with_clock(budget, progress, Instant::now, std::thread::sleep)
    }

    /// [`Pace::new`] on the clock `now` and the wait `sleep` (the tests').
    #[must_use]
    fn with_clock(
        budget: Duration,
        progress: &'a dyn Fn(&str),
        now: fn() -> Instant,
        sleep: fn(Duration),
    ) -> Self {
        Pace { progress: Some(progress), budget: Some(budget), started: now(), now, sleep }
    }

    fn say(&self, what: &str, t: &Target, k: usize, n: usize) {
        if let Some(progress) = self.progress {
            progress(&format!("{what} {} {k}/{n}", t.venue));
        }
    }

    fn spent(&self) -> bool {
        self.budget
            .is_some_and(|budget| (self.now)().saturating_duration_since(self.started) >= budget)
    }
}

/// Say which targets the budget cut off, at `warn` (one line naming them: they are not failures, the retry
/// thread asks again).
fn say_over_budget(pace: &Pace<'_>, phase: &str, left: &[&Target]) {
    let names: Vec<String> = left.iter().map(|t| format!("{} {}", t.route_key, t.symbol)).collect();
    tracing::warn!(
        symbols = ?names,
        "boot: the {phase} pass spent its {:?} budget; {} symbols are left to the retry thread \
         ({})",
        pace.budget.unwrap_or_default(),
        names.len(),
        names.join(", ")
    );
}

/// How long the boot waits before it asks again after the gatewayhub answered [`FACTS_BUSY`]: one token of
/// the venue's lookup bucket comes back per 500 ms (`vike_gatewayhub::trade::adapters::symbol_facts`'s
/// `LOOKUP_REFILL`; this crate links no gatewayhub, so the figure is restated here).
pub const BUSY_PAUSE: Duration = Duration::from_millis(500);

/// How many times the boot waits [`BUSY_PAUSE`] for ONE symbol before it leaves the symbol to the retry
/// thread: the bucket's burst (10) spent by a long restore refills in a second, while a venue whose
/// budget is per minute (ig, ibkr) would otherwise hold the whole pass.
pub const MAX_PACED_ASKS: u32 = 6;

/// Whether `why` is the gatewayhub's "not now" for a per-venue request budget ([`FACTS_BUSY`]).
fn is_busy(why: &str) -> bool {
    why.contains(FACTS_BUSY)
}

/// The admission of `t`, waiting out the gatewayhub's per-venue lookup bucket instead of failing on it: a
/// restore of more than the bucket's burst of symbols on a per-symbol venue (bybit, binance spot) is paced
/// to the refill rate, at `warn` ("paced, retrying"). Bounded by [`MAX_PACED_ASKS`] and by `pace`'s budget.
fn admission_paced(
    pace: &Pace<'_>,
    facts: &dyn SymbolFacts,
    t: &Target,
) -> Result<vike_exec::SymbolAdmission, String> {
    let mut paused = 0;
    loop {
        match facts.admission(&t.route_key, &t.venue, &t.symbol) {
            Err(why) if is_busy(&why) && paused < MAX_PACED_ASKS && !pace.spent() => {
                paused += 1;
                tracing::warn!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot: the gatewayhub is rate-limiting `{}`'s facts lookups ({why}); paced, \
                     retrying in {BUSY_PAUSE:?} ({paused}/{MAX_PACED_ASKS})",
                    t.symbol
                );
                (pace.sleep)(BUSY_PAUSE);
            }
            other => return other,
        }
    }
}

/// Send an `AdmitSymbol` for every target the facts source can answer, through `send` (the core's
/// lossless command door), and report the rest. A request budget that stays spent past
/// [`MAX_PACED_ASKS`] waits is said at `warn` (the retry thread asks again); every other failure is said at
/// `error`. Past `pace`'s budget
/// the remaining targets are not asked: they come back in `failed` (`why` = [`OVER_BUDGET`]) so the caller
/// treats them as missed and the retry thread admits them.
///
/// ⚠ **Each symbol's commit (`facts.admitted`) is the daemon's leverage settle on a venue that posts one**
/// (three or four signed reads on a symbol the venue already holds at the account's figure; a write only when
/// it does not): seconds, where it used to be one round trip. There is no separate count to cap: the pass is
/// bounded by `pace`'s [`READMIT_BUDGET`] and by `MAX_EXTRA_SYMBOLS` per account, and what it does not reach
/// goes to the retry thread, whose restored resting orders stay HELD until the symbol is told, as for any
/// miss. A commit that times out (the boot's bound) is UNCONFIRMED and nothing more: no figure is kept, no
/// second `AdmitSymbol` is sent, and the symbol runs at the engine's own rate until a commit that completes.
pub fn readmit(
    targets: &[Target],
    facts: &dyn SymbolFacts,
    pace: &Pace<'_>,
    mut send: impl FnMut(Command),
) -> Readmitted {
    let mut done = Readmitted::default();
    for (i, t) in targets.iter().enumerate() {
        if pace.spent() {
            let left: Vec<&Target> = targets[i..].iter().collect();
            say_over_budget(pace, "readmit", &left);
            done.failed.extend(
                left.iter()
                    .map(|t| (t.route_key.clone(), t.symbol.clone(), OVER_BUDGET.to_string())),
            );
            break;
        }
        pace.say("readmitting", t, i + 1, targets.len());
        match admission_paced(pace, facts, t) {
            Ok(admission) => {
                send(Command::AdmitSymbol(Box::new(admission.clone())));
                facts.admitted(&t.route_key, &t.venue, &t.symbol);
                restate_leverage(facts, admission, &mut send);
                done.sent += 1;
                done.admitted.push(t.clone());
            }
            Err(why) if is_busy(&why) => {
                tracing::warn!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot: `{}` in `{}`'s engine is not refreshed yet ({why}); paced, left to the \
                     retry thread",
                    t.symbol, t.route_key
                );
                done.failed.push((t.route_key.clone(), t.symbol.clone(), why));
            }
            Err(why) => {
                tracing::error!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot: could not refresh `{}` in `{}`'s engine ({why}); it keeps the grid the \
                     journal restored and has no price feed until an order on it asks again",
                    t.symbol, t.route_key
                );
                done.failed.push((t.route_key.clone(), t.symbol.clone(), why));
            }
        }
    }
    done
}

/// The boot's whole pass over `targets`: [`readmit`], then the price subscription of each symbol it
/// admitted. Returns what it did and the targets still MISSING: the ones whose facts the source could
/// not answer, and the ones admitted whose price feed could not be had (a failed `AdmitSymbol` commit
/// makes the price ask fail too). A symbol missing here stays as the restore left it, and its restored
/// resting orders are HELD, not adopted, until the symbol is told ([`retry_missed`] heals a transient
/// miss; an order on the symbol does too).
///
/// `pace` bounds the WHOLE pass, both loops: past its budget the symbols not yet asked (or not yet priced)
/// are left to the retry thread, said once at `warn`, and each `progress` call moves the start's
/// `starting` stream.
pub fn readmit_and_price(
    targets: &[Target],
    facts: &dyn SymbolFacts,
    pace: &Pace<'_>,
    send: impl FnMut(Command),
) -> (Readmitted, Vec<Missed>) {
    // The engines these targets belong to, as of NOW: a retry works for them and for no later engine.
    let generation = |t: &Target| facts.generation(&t.route_key);
    let generations: Vec<u64> = targets.iter().map(generation).collect();
    let done = readmit(targets, facts, pace, send);
    let mut missed: Vec<Missed> = targets
        .iter()
        .zip(&generations)
        .filter(|(t, _)| {
            done.failed.iter().any(|(rk, sym, _)| *rk == t.route_key && *sym == t.symbol)
        })
        .map(|(t, &generation)| Missed { target: t.clone(), told: false, generation })
        .collect();
    let generation_of =
        |t: &Target| targets.iter().position(|x| x == t).map_or(0, |i| generations[i]);
    for (i, t) in done.admitted.iter().enumerate() {
        if pace.spent() {
            let left: Vec<&Target> = done.admitted[i..].iter().collect();
            say_over_budget(pace, "price", &left);
            missed.extend(left.into_iter().map(|t| Missed {
                target: t.clone(),
                told: true,
                generation: generation_of(t),
            }));
            break;
        }
        pace.say("pricing", t, i + 1, done.admitted.len());
        if let Err(why) = facts.price(&t.route_key, &t.venue, &t.symbol, true) {
            tracing::error!(
                route_key = %t.route_key, symbol = %t.symbol,
                "boot: no price feed for the re-admitted `{}` ({why}); an opening order on it \
                 is refused until one exists",
                t.symbol
            );
            missed.push(Missed { target: t.clone(), told: true, generation: generation_of(t) });
        }
    }
    (done, missed)
}

/// A symbol the boot's pass could not finish, and where it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missed {
    /// The symbol.
    pub target: Target,
    /// Whether it WAS admitted and told (only its price feed is missing): a retry then subscribes the
    /// price and sends no second `AdmitSymbol`.
    pub told: bool,
    /// The account's engine generation at the boot ([`SymbolFacts::generation`]): once it moves the
    /// engine this symbol belonged to is gone, and the retry drops the symbol.
    pub generation: u64,
}

/// One retry round over `missed`: for each symbol, the facts look and the admission (unless it was already
/// told), then the price subscription. The account's engine generation is checked before the round's
/// work on a symbol and again immediately before each send, `admitted` and `price`: a symbol whose
/// engine was stopped or started anew since the boot is dropped untouched (the new engine's own start
/// admits through the door's gates). Returns what is STILL missing.
pub fn retry_round(
    missed: &[Missed],
    facts: &dyn SymbolFacts,
    send: impl FnMut(Command),
) -> Vec<Missed> {
    round(missed, facts, send, true).0
}

/// [`retry_round`] for a caller that repeats it on a schedule of its own (the rate timer's heal of a restored
/// position, `LeaseFacts::refresh_held_rates`): the same work, but a failure is said at `debug` and RETURNED (the
/// first reason a symbol was left missing, in words) so the caller says it once, when the state changes, instead
/// of this round saying it at `error` every time it is repeated.
pub fn retry_round_quiet(
    missed: &[Missed],
    facts: &dyn SymbolFacts,
    send: impl FnMut(Command),
) -> (Vec<Missed>, Option<String>) {
    round(missed, facts, send, false)
}

/// The body of [`retry_round`] / [`retry_round_quiet`]: `loud` = a failure is said at `warn`/`error`; the reason
/// of the first symbol left missing is returned either way.
fn round(
    missed: &[Missed],
    facts: &dyn SymbolFacts,
    mut send: impl FnMut(Command),
    loud: bool,
) -> (Vec<Missed>, Option<String>) {
    let mut left = Vec::new();
    let mut first_why: Option<String> = None;
    for m in missed {
        let t = &m.target;
        let current = || facts.generation(&t.route_key) == m.generation;
        let superseded = || {
            if loud {
                tracing::warn!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot retry: `{}`'s engine was stopped or started anew since the boot; the symbol is \
                     dropped (its engine admits through the door)",
                    t.route_key
                );
            } else {
                tracing::debug!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "retry: `{}`'s engine was stopped or started anew; the symbol is dropped",
                    t.route_key
                );
            }
        };
        if !current() {
            superseded();
            continue;
        }
        // The admission this symbol was last sent with in THIS round, which a commit's confirmed leverage
        // restates (`restate_leverage`).
        let mut latest: Option<SymbolAdmission> = None;
        if !m.told {
            let admission = match facts.admission(&t.route_key, &t.venue, &t.symbol) {
                Ok(admission) => admission,
                Err(why) => {
                    if !loud {
                        tracing::debug!(
                            route_key = %t.route_key, symbol = %t.symbol,
                            "retry: could not refresh `{}` in `{}`'s engine yet ({why})",
                            t.symbol, t.route_key
                        );
                    } else if is_busy(&why) {
                        tracing::warn!(
                            route_key = %t.route_key, symbol = %t.symbol,
                            "boot retry: `{}` in `{}`'s engine is rate-limited ({why}); paced, \
                             retrying",
                            t.symbol, t.route_key
                        );
                    } else {
                        tracing::error!(
                            route_key = %t.route_key, symbol = %t.symbol,
                            "boot retry: could not refresh `{}` in `{}`'s engine yet ({why})",
                            t.symbol, t.route_key
                        );
                    }
                    first_why.get_or_insert(why);
                    left.push(m.clone());
                    continue;
                }
            };
            if !current() {
                superseded();
                continue;
            }
            // ⚠ The generation check above and this send are not atomic: `send` may block on the
            // core's lossless lane, and a command that lands late folds into whichever engine exists
            // under the key at dequeue. Only reachable if this thread stalls in `send` across a whole
            // stop, settle and start of the account (`crates/vike-tradehub/CLAUDE.md`); not redesigned.
            send(Command::AdmitSymbol(Box::new(admission.clone())));
            facts.admitted(&t.route_key, &t.venue, &t.symbol);
            latest = Some(restate_leverage(facts, admission, &mut send));
        }
        if !current() {
            superseded();
            continue;
        }
        // The daemon's commit, with its outcome: `admitted` swallows a stalled commit (a warning), and a
        // price that succeeds on a probe lookup would then count the symbol healed while the reconcile
        // client was never told it. Idempotent, so a symbol just admitted above may be told twice.
        if let Err(why) = facts.tell_daemon(&t.route_key, &t.venue, &t.symbol) {
            if loud {
                tracing::error!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot retry: the gatewayhub was not told `{}` is admitted yet ({why}); its restored \
                     orders stay HELD",
                    t.symbol
                );
            } else {
                tracing::debug!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "retry: the gatewayhub was not told `{}` is admitted yet ({why})",
                    t.symbol
                );
            }
            first_why.get_or_insert(why);
            left.push(Missed { told: true, ..m.clone() });
            continue;
        }
        if !current() {
            superseded();
            continue;
        }
        // The commit just made the venue hold the figure again, so what it confirmed now is what the
        // engine should carry: restate against the admission sent above, or, for a symbol whose first
        // admission went out in an earlier pass, against the facts the source holds. A source that
        // confirmed nothing is not asked a thing.
        if facts.confirmed_leverage(&t.route_key, &t.symbol).is_some()
            && let Some(base) =
                latest.or_else(|| facts.admission(&t.route_key, &t.venue, &t.symbol).ok())
        {
            restate_leverage(facts, base, &mut send);
        }
        if !current() {
            superseded();
            continue;
        }
        if let Err(why) = facts.price(&t.route_key, &t.venue, &t.symbol, true) {
            if loud {
                tracing::error!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "boot retry: no price feed for the re-admitted `{}` yet ({why})",
                    t.symbol
                );
            } else {
                tracing::debug!(
                    route_key = %t.route_key, symbol = %t.symbol,
                    "retry: no price feed for the re-admitted `{}` yet ({why})",
                    t.symbol
                );
            }
            first_why.get_or_insert(why);
            left.push(Missed { told: true, ..m.clone() });
        }
    }
    (left, first_why)
}

/// How long [`retry_missed`] waits before each attempt: the boot's bound has passed by then, so a
/// gatewayhub that was slow once has had time. About eight minutes in all.
pub const RETRY_PAUSES: [std::time::Duration; 5] = [
    std::time::Duration::from_secs(5),
    std::time::Duration::from_secs(15),
    std::time::Duration::from_secs(45),
    std::time::Duration::from_secs(120),
    std::time::Duration::from_secs(300),
];

/// Say, loudly, which symbols the boot could not re-admit and what that costs while it stands.
pub fn say_missed(missed: &[Missed]) {
    let names: Vec<String> =
        missed.iter().map(|m| format!("{} {}", m.target.route_key, m.target.symbol)).collect();
    tracing::error!(
        symbols = ?names,
        "boot: these symbols were NOT re-admitted ({}): the reconcile client was not told them, so \
         any restored resting order on them is HELD (not adopted, not declared gone) and HALT or \
         cancel-all may not reach it. The boot asks again on a backoff (about eight minutes); an order on the \
         symbol admits it too, and so does a restart",
        names.join(", ")
    );
}

/// **Heal a transient boot miss without a restart**: ask again for `missed` after each of `pauses`
/// (`wait` sleeps and answers `false` when the process is stopping), through `attempt`, which runs the
/// normal [`readmit_and_price`] at the runtime bound (the boot window is over) and answers what is STILL
/// missing, or `None` when the facts source is gone. Ends the moment nothing is missing. Returns what
/// is still missing, said at `error` when it is not empty.
pub fn retry_missed(
    mut missed: Vec<Missed>,
    pauses: &[std::time::Duration],
    deadline: std::time::Instant,
    mut wait: impl FnMut(std::time::Duration) -> bool,
    mut attempt: impl FnMut(&[Missed]) -> Option<Vec<Missed>>,
) -> Vec<Missed> {
    let mut stopped = false;
    for &pause in pauses {
        if missed.is_empty() || std::time::Instant::now() >= deadline {
            break;
        }
        if !wait(pause) {
            stopped = true;
            break;
        }
        let Some(left) = attempt(&missed) else {
            stopped = true;
            break;
        };
        let healed = missed.len().saturating_sub(left.len());
        if healed > 0 {
            tracing::warn!(
                healed,
                left = left.len(),
                "boot: re-admitted symbols the boot had missed"
            );
        }
        missed = left;
    }
    if !missed.is_empty() && !stopped {
        let names: Vec<String> =
            missed.iter().map(|m| format!("{} {}", m.target.route_key, m.target.symbol)).collect();
        tracing::error!(
            symbols = ?names,
            "boot: STILL not re-admitted ({}): restored resting orders on them stay HELD and HALT or \
             cancel-all may not reach them until an order on the symbol admits it or the node restarts",
            names.join(", ")
        );
    }
    missed
}

/// **The retry thread's loop**, apart from the thread so a test can drive it and count: [`retry_missed`]
/// whose waits watch `stop` (slices of 250 ms; `false` = the process is stopping) and whose attempts
/// upgrade `facts` (a dropped registry ends it) and hand the live registry and the round to `run`.
fn drive(
    missed: Vec<Missed>,
    facts: &std::sync::Weak<crate::remote_backend::facts::LeaseFacts>,
    stop: &std::sync::atomic::AtomicBool,
    pauses: &[std::time::Duration],
    deadline: std::time::Instant,
    mut run: impl FnMut(&crate::remote_backend::facts::LeaseFacts, &[Missed]) -> Vec<Missed>,
) -> Vec<Missed> {
    use std::sync::atomic::Ordering::Relaxed;
    retry_missed(
        missed,
        pauses,
        deadline,
        |pause| {
            let until = std::time::Instant::now() + pause;
            while std::time::Instant::now() < until {
                if stop.load(Relaxed) {
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(250).min(pause));
            }
            !stop.load(Relaxed)
        },
        |round| {
            let facts = facts.upgrade()?;
            Some(run(&facts, round))
        },
    )
}

/// **The boot's retry thread** (`boot-readmit-retry`): [`retry_missed`] over [`retry_round`], wired to the
/// process. It holds the facts registry by `Weak` (a dropped registry ends it), sleeps in slices that
/// watch `stop` (the process's stop flag), sends through `send` (the core's lossless command door), and
/// opens a bounded round on the registry for each attempt (`LeaseFacts::begin_round`: the boot's bound
/// on every gatewayhub round trip of THIS thread, a silent lease paid once per round and per route key).
/// The thread returns what was still missing.
///
/// ⚠ The schedule's deadline (every pause, two bounds per round, a minute) is checked only BEFORE each
/// wait, so the thread can overrun it by one pause (at most the last, 300 s) plus one round, and a round
/// with K silent route keys costs about K bounds. The price subscription (`subscribe_ticks` on the
/// datahub dial) is outside the round bound. It is a bounded schedule of five rounds, not a hard stop.
///
/// # Errors
/// The thread could not be spawned.
pub fn spawn_retry(
    missed: Vec<Missed>,
    facts: &std::sync::Arc<crate::remote_backend::facts::LeaseFacts>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pauses: &'static [std::time::Duration],
    send: impl FnMut(Command) + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<Vec<Missed>>> {
    let bound = facts.round_bound();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(60)
        + pauses.iter().sum::<std::time::Duration>()
        + bound * 2 * u32::try_from(pauses.len()).unwrap_or(u32::MAX);
    let weak = std::sync::Arc::downgrade(facts);
    let mut send = send;
    std::thread::Builder::new().name("boot-readmit-retry".into()).spawn(move || {
        drive(missed, &weak, &stop, pauses, deadline, |facts, round| {
            let _bounded = facts.begin_round();
            retry_round(round, facts, &mut send)
        })
    })
}

#[cfg(test)]
#[path = "readmit_tests.rs"]
mod readmit_tests;
