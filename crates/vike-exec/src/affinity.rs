//! Opt-in HFT thread-to-core pinning. No Python twin — this is Rust-runtime infrastructure.
//!
//! The live core is a dedicated-blocking-thread design (md-reader pump · single-writer core ·
//! `ExecActor`). By default the OS scheduler is free to migrate those threads across cores, which
//! adds cache-miss + wakeup jitter to the arc-swap hot hop the `p99<10µs` gate measures. Pinning
//! each latency-sensitive thread to a fixed core removes that jitter — the last HFT latency lever.
//!
//! **OFF by default.** Pinning only helps on a dedicated colocated trading host; on the desktop-GUI
//! box most users run it can *hurt* (fighting the scheduler, starving the egui/wgpu render thread).
//! So it is driven entirely by the `VIKE_PIN_CORES` env var — absent ⇒ every `pin_current_thread`
//! call is a no-op and the app behaves exactly as before.
//!
//! ## `VIKE_PIN_CORES` grammar
//!
//! A comma-separated list of `role:core` pairs, e.g. `VIKE_PIN_CORES=md:28,core:29,exec:30`.
//! - `role` is one of the [`Role`] labels (`md`, `core`, `exec`); unknown roles are ignored.
//! - `core` is a 0-based logical core id. Out-of-range / unparseable ids are skipped with a warn.
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

use tracing::{info, warn};

/// The env var that drives all pinning. Absent ⇒ pinning fully disabled.
pub const PIN_ENV: &str = "VIKE_PIN_CORES";

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

/// Pin the CURRENT thread to the core assigned to `role` by `VIKE_PIN_CORES`, if any.
///
/// No-op (and silent) when the env var is unset or does not name `role` — the default. When it does
/// name a core, pins and logs the outcome ONCE. Safe to call from any thread; call it before the
/// thread's work loop. `label` is a short thread identifier for the log line (e.g. `"binance"`).
pub fn pin_current_thread(role: Role, label: &str) {
    let spec = match env::var(PIN_ENV) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => return, // unset/empty ⇒ pinning disabled: the default desktop path
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

    #[test]
    fn last_matching_key_wins_is_irrelevant_first_wins() {
        // `find` returns the first match — a deterministic, documented rule.
        assert_eq!(core_for_role("core:1,core:2", Role::Core), Some(1));
    }
}
