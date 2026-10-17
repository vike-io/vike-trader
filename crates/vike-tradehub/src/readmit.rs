//! `readmit` — on boot, tell each engine again which symbols it trades beyond the one it was started
//! on, with fresh facts (decision 0173, item 8; decision 0174).
//!
//! A restart restores positions and resting orders for EVERY symbol, and the engine's own
//! `restore_runtime_state` puts back the symbols a previous run admitted on demand (in
//! `extra_symbols`, with the on-demand grid row it had then), so the snapshot LISTS them. What it
//! cannot restore is a venue's CURRENT tick and lot, or the price feed of the symbol: [`targets`] reads the symbols off the journal's restored engines and [`readmit`] sends one
//! `AdmitSymbol` for each (a refresh), on the core's command lane, BEFORE the reconcile driver is
//! spawned: the lane is FIFO, so every admission folds ahead of the first `ReconcileReports`.
//!
//! The facts come from the same [`SymbolFacts`] the control door uses, on the boot thread, so a venue
//! that cannot answer costs a logged error per symbol and nothing else: the symbol keeps the grid the
//! journal restored, and the door refreshes it on the next order. Nothing here refuses a start.

use vike_exec::{Command, EngineSnapshot};

use crate::server::symbol_admission::SymbolFacts;

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

/// The symbols the restored `engines` hold a position or a live order in, other than the symbol each
/// was started on and its DECLARED extras, in first-seen order and without repeats. A symbol an
/// earlier run ADMITTED is a target even though the snapshot lists it in `extra_symbols`.
pub fn targets(engines: &[EngineSnapshot]) -> Vec<Target> {
    let mut out: Vec<Target> = Vec::new();
    for snap in engines {
        let route_key = snap.route_key.clone().unwrap_or_else(|| snap.venue.clone());
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
        for symbol in held {
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

/// Send an `AdmitSymbol` for every target the facts source can answer, through `send` (the core's
/// lossless command door), and report the rest. The failures are said at `error`.
pub fn readmit(
    targets: &[Target],
    facts: &dyn SymbolFacts,
    mut send: impl FnMut(Command),
) -> Readmitted {
    let mut done = Readmitted::default();
    for t in targets {
        match facts.admission(&t.route_key, &t.venue, &t.symbol) {
            Ok(admission) => {
                send(Command::AdmitSymbol(Box::new(admission)));
                facts.admitted(&t.route_key, &t.venue, &t.symbol);
                done.sent += 1;
                done.admitted.push(t.clone());
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
pub fn readmit_and_price(
    targets: &[Target],
    facts: &dyn SymbolFacts,
    send: impl FnMut(Command),
) -> (Readmitted, Vec<Missed>) {
    // The engines these targets belong to, as of NOW: a retry works for them and for no later engine.
    let generation = |t: &Target| facts.generation(&t.route_key);
    let generations: Vec<u64> = targets.iter().map(generation).collect();
    let done = readmit(targets, facts, send);
    let mut missed: Vec<Missed> = targets
        .iter()
        .zip(&generations)
        .filter(|(t, _)| {
            done.failed.iter().any(|(rk, sym, _)| *rk == t.route_key && *sym == t.symbol)
        })
        .map(|(t, &generation)| Missed { target: t.clone(), told: false, generation })
        .collect();
    for t in &done.admitted {
        if let Err(why) = facts.price(&t.route_key, &t.venue, &t.symbol, true) {
            tracing::error!(
                route_key = %t.route_key, symbol = %t.symbol,
                "boot: no price feed for the re-admitted `{}` ({why}); an opening order on it \
                 is refused until one exists",
                t.symbol
            );
            let at = targets.iter().position(|x| x == t).map_or(0, |i| generations[i]);
            missed.push(Missed { target: t.clone(), told: true, generation: at });
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
    mut send: impl FnMut(Command),
) -> Vec<Missed> {
    let mut left = Vec::new();
    for m in missed {
        let t = &m.target;
        let current = || facts.generation(&t.route_key) == m.generation;
        let superseded = || {
            tracing::warn!(
                route_key = %t.route_key, symbol = %t.symbol,
                "boot retry: `{}`'s engine was stopped or started anew since the boot; the symbol is \
                 dropped (its engine admits through the door)",
                t.route_key
            );
        };
        if !current() {
            superseded();
            continue;
        }
        if !m.told {
            let admission = match facts.admission(&t.route_key, &t.venue, &t.symbol) {
                Ok(admission) => admission,
                Err(why) => {
                    tracing::error!(
                        route_key = %t.route_key, symbol = %t.symbol,
                        "boot retry: could not refresh `{}` in `{}`'s engine yet ({why})",
                        t.symbol, t.route_key
                    );
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
            send(Command::AdmitSymbol(Box::new(admission)));
            facts.admitted(&t.route_key, &t.venue, &t.symbol);
        }
        if !current() {
            superseded();
            continue;
        }
        // The daemon's commit, with its outcome: `admitted` swallows a stalled commit (a warning), and a
        // price that succeeds on a probe lookup would then count the symbol healed while the reconcile
        // client was never told it. Idempotent, so a symbol just admitted above may be told twice.
        if let Err(why) = facts.tell_daemon(&t.route_key, &t.venue, &t.symbol) {
            tracing::error!(
                route_key = %t.route_key, symbol = %t.symbol,
                "boot retry: the gatewayhub was not told `{}` is admitted yet ({why}); its restored \
                 orders stay HELD",
                t.symbol
            );
            left.push(Missed { told: true, ..m.clone() });
            continue;
        }
        if !current() {
            superseded();
            continue;
        }
        if let Err(why) = facts.price(&t.route_key, &t.venue, &t.symbol, true) {
            tracing::error!(
                route_key = %t.route_key, symbol = %t.symbol,
                "boot retry: no price feed for the re-admitted `{}` yet ({why})",
                t.symbol
            );
            left.push(Missed { told: true, ..m.clone() });
        }
    }
    left
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
