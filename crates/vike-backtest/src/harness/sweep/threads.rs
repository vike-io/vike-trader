//! The bounded sweep pool: its concurrency cap, the two env knobs behind it, and the one rayon entry.

#[cfg(doc)]
use super::{ParamscanExec, map_bounded};

/// The env var that pins a process back to [`ParamscanExec::Sequential`].
pub const SWEEP_SEQUENTIAL_ENV: &str = "VIKE_SWEEP_SEQUENTIAL";

/// The env var that caps how many sweep points run CONCURRENTLY (see [`sweep_threads`]).
pub const SWEEP_THREADS_ENV: &str = "VIKE_SWEEP_THREADS";

/// The default concurrency cap when [`SWEEP_THREADS_ENV`] is unset or unparsable. SMALL ON
/// PURPOSE, and deliberately NOT the core count: each concurrent point materializes its OWN copy of
/// the data slice (see the module doc's memory note), so this number IS the peak-RSS multiplier.
// ⚠ `pub(crate)` rather than private since stage 5:
// `crates/vike-backtest/src/backtest_cli/search_flags.rs`'s `parse_keep_trials` REFUSES
// `--keep-trials series` citing this cap, and a refusal that typed the number instead would be a
// second spelling of it two files away — the shape
// `crates/vike-ops/tests/docs/one_authority_gate.rs` exists to stop. Not `pub`: the value is this
// crate's own spending decision and no consumer has any business reading it.
pub(crate) const DEFAULT_SWEEP_THREADS: usize = 4;

/// **The CALLER-OWNED cap** — the process-wide handle [`install_sweep_threads`] fills and
/// [`sweep_threads`] reads first. `OnceLock`, so it is written once by a composition root and never
/// changes under a running sweep.
pub(super) static INSTALLED_SWEEP_THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// **Hand this process's sweep pool the cap its settings resolved** — the seam that makes
/// `preferences.sweep_threads` a setting rather than a declaration.
///
/// ⚠ **Why this shape and not a parameter.** The pool is entered from three front doors in two
/// crates (`optimize.rs`'s fan-out, `walkforward/runner.rs`, and the public [`map_bounded`]
/// vike-studio-core takes), none of which carries a settings value and none of which a
/// composition root calls directly — so threading the number would mean a parameter on every
/// public sweep entry point plus the private glue under it. `vike_ops::settings`' module doc files
/// this under its family-2 STEP-2 items and names `init(spec)` + `OnceLock` as the shape; this is
/// that shape, for the one knob, with the root owning the read.
///
/// **`n` is the RESOLVED preference**, i.e. `vike_config::Preferences::sweep_threads` after
/// `apply_env` — which already folds `VIKE_SWEEP_THREADS` over the file value. That is why
/// [`sweep_threads`] consults this BEFORE the environment rather than after: consulting the
/// variable again would be a second authority over a value the loader has already decided, and on
/// a box that sets both it would answer the same thing twice or, after a future precedence change,
/// differently. `None` installs nothing, so a process whose file and environment both say nothing
/// keeps the compiled-in fallback.
///
/// Returns whether THIS call installed the value. A second call with a different number is a
/// composition-root bug (two roots in one process), so it is refused rather than honoured —
/// silently changing a bound under a pool that may already be built is the worse failure.
pub fn install_sweep_threads(n: Option<usize>) -> bool {
    match n {
        Some(n) if n > 0 => INSTALLED_SWEEP_THREADS.set(n).is_ok(),
        // A zero is refused by `vike_config::Preferences::apply` long before it reaches here (it
        // would install a pool that never runs a point); treating it as "install nothing" keeps
        // this function total for a caller that built its own value.
        _ => false,
    }
}

/// How many sweep points run concurrently: the cap a composition root
/// [`install_sweep_threads`]-ed, else `VIKE_SWEEP_THREADS` when it parses to a POSITIVE integer,
/// else `min(DEFAULT_SWEEP_THREADS, available_parallelism())`.
///
/// ⚠ The installed value wins over the environment, and that is not a precedence inversion: what a
/// root installs IS the resolved `env > file > default` answer (see [`install_sweep_threads`]).
/// The direct read below is what keeps a process that installs NOTHING — a one-shot `backtest` run
/// loads no settings at all — working exactly as it did before this seam existed.
///
/// Never returns zero — rayon reads `num_threads(0)` as "use the default", i.e. one worker per
/// logical core, which is exactly the unbounded shape this cap exists to prevent.
pub fn sweep_threads() -> usize {
    if let Some(n) = INSTALLED_SWEEP_THREADS.get() {
        return *n;
    }
    match std::env::var(SWEEP_THREADS_ENV).ok().and_then(|v| v.trim().parse::<usize>().ok()) {
        Some(n) if n > 0 => n,
        _ => default_sweep_threads(),
    }
}

/// The compiled-in fallback — `min(DEFAULT_SWEEP_THREADS, available_parallelism())`, with neither
/// lever consulted. Split out of [`sweep_threads`] so the cap can be asserted independently of the
/// process-wide handle: `install_sweep_threads` writes a `OnceLock`, test order is undefined, and a
/// default-cap test reading [`sweep_threads`] would otherwise pass or fail by scheduling.
pub(super) fn default_sweep_threads() -> usize {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    DEFAULT_SWEEP_THREADS.min(cores)
}

/// Run `op` on a sweep-sized rayon pool ([`sweep_threads`] workers) rather than the GLOBAL pool —
/// the ONE place the sweep/euler lanes enter rayon, so the memory multiplier stays bounded.
///
/// Pool construction can fail (the OS refusing a thread spawn). That is a resource condition, not a
/// correctness one, so it is logged and `op` runs on the caller's current pool instead of aborting
/// the sweep: results are identical either way (order-preserving collect) — only the concurrency
/// bound is lost. It is never `unwrap`ped.
///
/// [`map_bounded`] is the public front door for callers OUTSIDE this crate (vike-studio-core's own
/// sweep) — they get the same bounded pool and the same order-preserving collect without taking a
/// rayon dependency of their own, so rayon is entered from exactly one place in the workspace.
pub(crate) fn install_bounded<T: Send>(op: impl FnOnce() -> T + Send) -> T {
    match rayon::ThreadPoolBuilder::new().num_threads(sweep_threads()).build() {
        Ok(pool) => pool.install(op),
        Err(e) => {
            tracing::warn!(
                threads = sweep_threads(),
                error = %e,
                "sweep thread pool build failed; falling back to the ambient rayon pool"
            );
            op()
        }
    }
}
