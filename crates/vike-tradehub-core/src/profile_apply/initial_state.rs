//! `guards.initial_trading_state`: whether a freshly spawned core is SENT the profile's trading
//! state, and which.
//!
//! The state lives on the ENGINE, not in `vike_core::CoreConfig`, so it cannot be an assignment in
//! [`crate::apply_guards_and_sinks`]. The daemon (`vike-tradehub`, on both its arms) calls
//! [`send_initial_trading_state`] once the core is up and BEFORE any feed is wired: it sends
//! `Command::SetTradingState` down the same lossless command lane a wire `SetTradingState` takes —
//! so it folds ahead of the first market message, and the gate state it asks for is the one an
//! operator's `SetTradingState` sets (every engine of the core, the primary and the extras).
//!
//! # The two rules this is for
//!
//! 1. **`active` — the default — sends NOTHING.** A profile that never names the key, or names
//!    `active`, must behave byte-identically to a daemon that never heard of the key: no command is
//!    journaled, no engine is touched.
//! 2. **A profile never LOOSENS a stricter state the journal restored.** `vike_core`'s boot restore
//!    puts each engine's journaled state back "only when it is STRICTER than the boot's", and "only
//!    a `SetTradingState` command loosens it" — which is what this would be if a profile saying
//!    `reducing` were sent over an engine restored `Halted`. So the profile's state is sent only
//!    when it is at least as strict as every restored engine's; otherwise the journal's state
//!    stands, nothing is sent, and the daemon says so.
//!
//! Applied at EVERY start, so `halted` here means "start halted", not "halt once": an operator who
//! resumes trading with a `SetTradingState(Active)` is halted again by the next restart until the
//! key is rewritten (`vike-cli config bootstrap-run <name> --guards.initial_trading_state active`).
//! It is NOT the HALT sentinel file (`docs/ops/kill-switches.md` section C), which stops venue
//! submits across processes: this is the in-process gate state.

use vike_config::RunProfile;
use vike_exec::Command;
use vike_model::TradingState;

/// The state to send the freshly spawned core as `Command::SetTradingState`, or `None` when nothing
/// is to be sent.
///
/// `journaled` is the `trading_state` of every engine the boot restore is about to put back
/// (`vike_core::replay::BootRestore::restored_engines`, read BEFORE `apply_to` consumes it); an
/// empty iterator means nothing was restored.
///
/// `None` in exactly two cases:
///
/// - the profile's state is `active` (the default — nothing to send), or
/// - some restored engine is STRICTER than the profile's state, so sending it would loosen that
///   engine (the journal's state wins).
///
/// `Some(state)` otherwise: a `reducing` or `halted` profile over a journal that restored nothing,
/// or nothing stricter. It reaches EVERY engine of the core, because the command is uniform: a
/// restore in which one engine was `Halted` and another `Active`, under a profile of `reducing`,
/// therefore sends nothing and the `Active` engine stays `Active` — the declared residual of one
/// command for many engines, and the safe side of it (nothing is loosened). In practice the states
/// are uniform: every trip that sets one (the dead-man, the drawdown latch, an operator's command)
/// sets it on every engine.
///
/// A second residual, same side: `restored_engines` lists every engine the journal HOLDS, including
/// one this boot builds no engine for (the core skips that snapshot with a `RESTORE ENGINE SKIPPED`
/// note). Its stricter state still counts here, so a `reducing` profile over a stale `Halted`
/// snapshot of an unmounted venue sends nothing — the profile fails to TIGHTEN, never to loosen. A
/// `halted` profile is unaffected: nothing is stricter than it.
#[must_use]
pub fn initial_trading_state_to_send(
    profile: &RunProfile,
    journaled: impl IntoIterator<Item = TradingState>,
) -> Option<TradingState> {
    let wanted = profile.guards.trading_state();
    if wanted == TradingState::Active {
        return None;
    }
    let strictest_restored =
        journaled.into_iter().map(TradingState::strictness).max().unwrap_or_default();
    (wanted.strictness() >= strictest_restored).then_some(wanted)
}

/// Decide with [`initial_trading_state_to_send`], and SEND the answer through `send` (the daemon
/// passes `|c| handle.send_command(c)`, the lossless lane). Returns the state sent, `None` when
/// nothing was.
///
/// Both outcomes that matter are said out loud, at `warn!` because they are the operator's to
/// notice: a daemon that starts `Halted` or `Reducing` trades nothing (or nothing new) until
/// someone resumes it, and a profile that asked for a state the journal's stricter one overrode is
/// a profile that does not say what the process is doing. The default (`active`) is silent.
pub fn send_initial_trading_state(
    profile: &RunProfile,
    journaled: &[TradingState],
    send: impl FnOnce(Command),
) -> Option<TradingState> {
    let wanted = profile.guards.trading_state();
    let sent = initial_trading_state_to_send(profile, journaled.iter().copied());
    match sent {
        Some(state) => {
            tracing::warn!(
                state = ?state,
                "run profile `guards.initial_trading_state`: this process STARTS in {state:?} (a \
                 `SetTradingState` command is sent to the core before any feed is wired). Resume \
                 with a `SetTradingState(Active)`; the key applies again at every start"
            );
            send(Command::SetTradingState(state));
        }
        None if wanted != TradingState::Active => {
            tracing::warn!(
                asked = ?wanted,
                restored = ?journaled,
                "run profile `guards.initial_trading_state` asks for {wanted:?}, but the journal \
                 restored a STRICTER state; it stands (a profile never loosens a restored halt — \
                 only a `SetTradingState` command does)"
            );
        }
        None => {}
    }
    sent
}
