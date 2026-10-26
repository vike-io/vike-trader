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
/// - EVERY restored engine is at least as strict as the profile's state and some is STRICTER, so there
///   is nothing to tighten and sending it would loosen that engine (the journal's state wins).
///
/// `Some(state)` otherwise: a `reducing` or `halted` profile over a journal that restored nothing, or
/// over at least one engine LOOSER than the profile's word. It reaches EVERY engine of the core,
/// because the command is node-wide. The states are NOT always uniform any more: an account-scoped
/// halt (`trade account-state <book> halted`) is journaled, so a restore in which one engine was
/// `Halted` and another `Active`, under a profile of `reducing`, MUST tighten the `Active` one — it
/// used to send nothing and leave it `Active` despite the profile. The engine that was stricter than
/// the word is put back by [`send_initial_trading_state`], which knows the route keys.
///
/// A residual, same side as before: `restored_engines` lists every engine the journal HOLDS,
/// including one this boot builds no engine for (the core skips that snapshot with a `RESTORE ENGINE
/// SKIPPED` note). Its state still counts here, so a `reducing` profile over a stale `Halted`
/// snapshot of an unmounted venue as the ONLY restored engine sends nothing — the profile fails to
/// TIGHTEN, never to loosen. A `halted` profile is unaffected: nothing is stricter than it.
#[must_use]
pub fn initial_trading_state_to_send(
    profile: &RunProfile,
    journaled: impl IntoIterator<Item = TradingState>,
) -> Option<TradingState> {
    let wanted = profile.guards.trading_state();
    if wanted == TradingState::Active {
        return None;
    }
    let word = wanted.strictness();
    let restored: Vec<u8> = journaled.into_iter().map(TradingState::strictness).collect();
    // Everything restored at least as strict as the word and something stricter: nothing to tighten,
    // and sending the word would loosen that engine, so the journal's state stands.
    let journal_stands = restored.iter().all(|&s| s >= word) && restored.iter().any(|&s| s > word);
    (!journal_stands).then_some(wanted)
}

/// `(route key, trading state)` of every engine the boot restore is about to put back
/// (`vike_core::replay::BootRestore::restored_engines`), in the shape [`send_initial_trading_state`]
/// reads. The route key is the engine's own, which a default account leaves implicit (it is the venue).
#[must_use]
pub fn restored_trading_states(
    restored: &[vike_exec::EngineSnapshot],
) -> Vec<(String, TradingState)> {
    restored
        .iter()
        .map(|snap| {
            (snap.route_key.clone().unwrap_or_else(|| snap.venue.clone()), snap.trading_state)
        })
        .collect()
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
    journaled: &[(String, TradingState)],
    mut send: impl FnMut(Command),
) -> Option<TradingState> {
    let wanted = profile.guards.trading_state();
    let sent = initial_trading_state_to_send(profile, journaled.iter().map(|(_, s)| *s));
    match sent {
        Some(state) => {
            tracing::warn!(
                state = ?state,
                "run profile `guards.initial_trading_state`: this process STARTS in {state:?} (a \
                 `SetTradingState` command is sent to the core before any feed is wired). Resume \
                 with a `SetTradingState(Active)`; the key applies again at every start"
            );
            send(Command::SetTradingState(state));
            // The word is node-wide, so it also brought an engine that restored STRICTER (an account-scoped
            // halt) down to it: put each back, by its route key. An account the core does not run is refused
            // by the core with a note and, being refused, never journaled.
            //
            // ⚠ **Two records, so a short transient**: until the second folds (a few messages, ahead of the
            // first feed message) that engine is at the profile's word, `Reducing` at least, which is still
            // closing-only; a crash between the two journaled records restores it at the word. The other
            // order cannot work (the node-wide word overwrites every account's state), and a single record
            // would need a command of its own whose first use closes the same journal door. These records
            // are written only for a NON-UNIFORM restore (an account-scoped halt, or an engine the link
            // dead-man had halted, restored with no overlay) and are the THIRD trigger of the journal's
            // format-21 stamp, after `trade account-state` and the account watcher's hold.
            for (route_key, restored) in journaled {
                if restored.strictness() > state.strictness() {
                    send(Command::SetAccountTradingState {
                        route_key: route_key.clone(),
                        state: *restored,
                    });
                }
            }
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
