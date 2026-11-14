//! `DeadManAction`: what a dead-man switch does when it trips.
//!
//! ONE type for the core's two switches (the SILENCE switch, `vike_core::DeadManSilenceConfig`, and the
//! CONNECTION-state link dead-man, `vike_core::DeadManLinkConfig`) and for the `policy.deadman_action`
//! row that picks it (`vike_config::Policy::deadman_action`). It was a vike-core enum, a vike-config
//! file-spelling twin and a mapping between the two in the composition root; it sits here because
//! vike-config is below vike-core and both name this crate. The switches themselves stay in vike-core.

/// What the dead-man does when it trips.
///
/// Serde `snake_case`, so the file spellings are `"cancel_all_and_halt"` and `"cancel_all"`; an
/// unknown spelling fails at deserialization naming the legal two (in this declaration order).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeadManAction {
    /// Cancel every resting order AND engage HALT: set `trading_state = Halted` on every engine (the
    /// in-process `RiskGate` then denies every new order) and, when a sentinel path is configured
    /// (`vike_core::DeadManSilenceConfig::halt_file`), write that HALT file so the venue adapter's own
    /// submit thread (`ExecutionClientActor`) also refuses new orders — the SAME sentinel the manual
    /// kill-switch uses, so new-order refusal is consistent across process boundaries.
    ///
    /// **The default**: an outage that pulled the book is an incident, and a strategy that re-quotes
    /// the moment data resumes — into a market it has not seen for a minute — is the wrong first
    /// thing to happen after one. An operator un-halts.
    ///
    /// ⚠ That is the SILENCE switch's halt (`deadman_timeout_ms`). The CONNECTION-state link
    /// dead-man (`link_deadman_grace_ms`) shares this action but halts only the engines of the venue
    /// whose link died and writes no sentinel, released when its links are Live again
    /// (`vike_core::CoreThread::sweep_link_deadman`, decision 0180).
    #[default]
    CancelAllAndHalt,
    /// Cancel every resting order. Trading state is left as-is, so a strategy could legitimately
    /// re-quote once data recovers — the switch just pulls the currently-exposed orders. The lighter
    /// action, for a maker whose whole job is to be re-quoting and whose operator has decided a
    /// re-quote after an outage is acceptable.
    CancelAll,
}

impl DeadManAction {
    /// Whether this action engages HALT (in-process trading-state + the cross-process sentinel).
    #[must_use]
    pub fn engages_halt(self) -> bool {
        matches!(self, DeadManAction::CancelAllAndHalt)
    }

    /// The file spelling, for messages that name the legal set.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            DeadManAction::CancelAllAndHalt => "cancel_all_and_halt",
            DeadManAction::CancelAll => "cancel_all",
        }
    }
}
