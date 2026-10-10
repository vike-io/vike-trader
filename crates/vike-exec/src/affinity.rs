//! Opt-in HFT thread-to-core pinning. No Python twin — this is Rust-runtime infrastructure.
//!
//! The live core is a dedicated-blocking-thread design (md-reader pump · single-writer core ·
//! `ExecActor`). By default the OS scheduler is free to migrate those threads across cores, which
//! adds cache-miss + wakeup jitter to the arc-swap hot hop the `p99<10µs` gate measures. Pinning
//! each latency-sensitive thread to a fixed core removes that jitter — the last HFT latency lever.
//!
//! **OFF by default.** Pinning only helps on a dedicated colocated trading host; on the desktop-GUI
//! box most users run it can *hurt* (fighting the scheduler, starving the egui/wgpu render thread).
//! So it is driven by the `VIKE_PIN_CORES` env var and, where that variable is ABSENT, by the spec a
//! composition root installed from its `config.pin_cores` settings row ([`install_pin_spec`];
//! decision 0111, phase P3 — only `vike-tradehub` installs one). Neither ⇒ every
//! `pin_current_thread` call is a no-op and the app behaves exactly as before. A SET variable
//! decides even when it is EMPTY (no pinning), which is how a throwaway copy of the daemon is kept
//! off the cores the live one pins.
//!
//! ## `VIKE_PIN_CORES` grammar
//!
//! A comma-separated list of `role:core` pairs, e.g. `VIKE_PIN_CORES=md:28,core:29,exec:30`.
//! - `role` is one of the [`Role`] labels (`md`, `core`, `exec`); unknown roles are ignored.
//! - `core` is a 0-based logical core id. An id outside the process's allowed set is skipped with
//!   a warn; an unparseable one leaves the role unpinned, silently.
//! - A role absent from the spec is never pinned (its threads float, as today).
//!
//! Each thread calls [`pin_current_thread`] with its role as the FIRST thing in its body (before the
//! hot loop). It logs its outcome ONCE at spawn (`info` on success, `warn` on a bad/unavailable
//! core) — never on the hot path.
//!
//! Note (coarse v1): all threads sharing a role pin to the SAME core. For a focused single-symbol
//! HFT deployment that is the intent; a many-symbol deployment that wants per-feed cores is a future
//! refinement of the grammar, not a change to this seam.

use std::env;
use std::sync::OnceLock;

use tracing::{info, warn};

/// The env var that drives all pinning. Absent ⇒ the installed row's spec, if any.
pub const PIN_ENV: &str = "VIKE_PIN_CORES";

/// The `config.pin_cores` settings row, as the composition root installed it ([`install_pin_spec`]).
/// Unset (nothing installed) and `Some(None)` (no row) both mean "no spec from the row".
static INSTALLED_SPEC: OnceLock<Option<String>> = OnceLock::new();

/// **Install the settings row's pinning spec — once, before any pinned thread is spawned.** The
/// library takes the value as a parameter; the binary that loaded the settings hands it over (the
/// `vike_backtest` `install_sweep_threads` shape). A second call is ignored: the first answer is the
/// one every thread already pinned by.
pub fn install_pin_spec(spec: Option<String>) {
    // `Err` = already installed; the first answer stands (documented above).
    let _ = INSTALLED_SPEC.set(spec);
}

/// Which spec decides: the variable when it is SET (even empty, even non-UTF-8 — then nothing is
/// pinned), else the installed row. Pure, so the precedence is a test.
fn choose_spec(var: Result<String, env::VarError>, installed: Option<&str>) -> Option<String> {
    match var {
        Ok(s) => Some(s),
        Err(env::VarError::NotPresent) => installed.map(str::to_string),
        Err(env::VarError::NotUnicode(_)) => None,
    }
}

/// A pinnable thread role. The string form is what appears in `VIKE_PIN_CORES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A market-data reader pump (venue quote/trade/book feed).
    MarketData,
    /// The single-writer core thread (`vt-core`) — the arc-swap hot hop.
    Core,
    /// A venue `ExecActor` command thread.
    Exec,
}

impl Role {
    /// The token matched against `VIKE_PIN_CORES` keys.
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::MarketData => "md",
            Role::Core => "core",
            Role::Exec => "exec",
        }
    }
}

/// Parse `VIKE_PIN_CORES`-style text into the core id assigned to `role`, if any.
///
/// Pure (takes the spec string, does no env read or pinning) so it is unit-testable. Returns `None`
/// when the spec is empty, `role` is absent, or its value is not a valid `usize`.
pub fn core_for_role(spec: &str, role: Role) -> Option<usize> {
    let key = role.as_str();
    spec.split(',')
        .filter_map(|pair| pair.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
}

/// Pin the CURRENT thread to the core assigned to `role` by `VIKE_PIN_CORES` — or, where that is
/// unset, by the installed `config.pin_cores` spec — if any.
///
/// No-op (and silent) when neither names `role` — the default. When one does name a core, pins and
/// logs the outcome ONCE. Safe to call from any thread; call it before the thread's work loop.
/// `label` is a short thread identifier for the log line (e.g. `"binance"`).
pub fn pin_current_thread(role: Role, label: &str) {
    let installed = INSTALLED_SPEC.get().and_then(Option::as_deref);
    let spec = match choose_spec(env::var(PIN_ENV), installed) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return, // no spec, or an empty one ⇒ pinning disabled: the default desktop path
    };
    let Some(core_id) = core_for_role(&spec, role) else {
        return; // spec present but this role isn't listed ⇒ leave the thread floating
    };

    // core_affinity enumerates the cores the process is actually allowed to run on; index into that
    // rather than assuming a dense 0..N so a cgroup/taskset-restricted process can't pin off-set.
    let available = core_affinity::get_core_ids().unwrap_or_default();
    match available.iter().find(|c| c.id == core_id) {
        Some(target) if core_affinity::set_for_current(*target) => {
            info!(role = role.as_str(), label, core = core_id, "pinned thread to core");
        }
        Some(_) => {
            warn!(
                role = role.as_str(),
                label,
                core = core_id,
                "core pin call failed; thread floats"
            );
        }
        None => {
            warn!(
                role = role.as_str(),
                label,
                core = core_id,
                available = available.len(),
                "requested core not in the process's allowed set; thread floats"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_role_to_core() {
        let spec = "md:28,core:29,exec:30";
        assert_eq!(core_for_role(spec, Role::MarketData), Some(28));
        assert_eq!(core_for_role(spec, Role::Core), Some(29));
        assert_eq!(core_for_role(spec, Role::Exec), Some(30));
    }

    #[test]
    fn absent_role_is_none() {
        assert_eq!(core_for_role("exec:3", Role::MarketData), None);
        assert_eq!(core_for_role("", Role::Core), None);
    }

    #[test]
    fn tolerates_whitespace_and_case() {
        assert_eq!(core_for_role(" MD : 7 , CORE:8 ", Role::MarketData), Some(7));
        assert_eq!(core_for_role(" MD : 7 , CORE:8 ", Role::Core), Some(8));
    }

    #[test]
    fn bad_value_is_skipped_not_panicked() {
        assert_eq!(core_for_role("md:notanum,core:5", Role::MarketData), None);
        assert_eq!(core_for_role("md:notanum,core:5", Role::Core), Some(5));
    }

    /// Decision 0111, phase P3: the variable still wins over the settings row — even SET TO EMPTY,
    /// which is how a throwaway probe of the daemon is kept off the live daemon's cores — and the
    /// row answers only where the variable is absent.
    #[test]
    fn the_variable_beats_the_installed_row_and_the_row_fills_its_absence() {
        let row = Some("core:5");
        assert_eq!(choose_spec(Ok("core:1".to_string()), row), Some("core:1".to_string()));
        assert_eq!(choose_spec(Ok(String::new()), row), Some(String::new()), "empty still decides");
        assert_eq!(choose_spec(Err(env::VarError::NotPresent), row), Some("core:5".to_string()));
        assert_eq!(choose_spec(Err(env::VarError::NotPresent), None), None, "neither: no pinning");
        let not_unicode = env::VarError::NotUnicode(std::ffi::OsString::from("x"));
        assert_eq!(choose_spec(Err(not_unicode), row), None, "an unreadable variable pins nothing");
    }

    #[test]
    fn last_matching_key_wins_is_irrelevant_first_wins() {
        // `find` returns the first match — a deterministic, documented rule.
        assert_eq!(core_for_role("core:1,core:2", Role::Core), Some(1));
    }
}
