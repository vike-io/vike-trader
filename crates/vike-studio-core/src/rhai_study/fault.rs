//! How a TYPED [`StudyError`] survives a trip through the interpreter.
//!
//! # The problem, stated exactly
//!
//! [`vike_user_research::StudyError`] has three variants and the split is by WHO an operator has to
//! go and talk to — that type's own doc says so. Two of them carry evidence a caller acts on
//! MECHANICALLY: `Data` carries `vike_data::DataError` itself, so a caller can tell a query fault
//! from an I/O one without parsing a sentence (and
//! `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md`'s item 3 wants a study that
//! asks for a window the store does not hold to STOP, naming the range), and `NoLearner` is the one
//! failure a UI can render as a host ceiling rather than a stack of prose.
//!
//! A Rhai host function can only fail as a `Box<EvalAltResult>`. The obvious implementation raises
//! `e.to_string()` and rebuilds a `StudyError::Study(msg)` on the way out — which would silently
//! flatten both typed variants into a sentence, for every study written in this tier. The Rust tier
//! keeps them; a tier that quietly did not would be exactly the "widening the contract to match a
//! limitation" this work was told not to do.
//!
//! # ⚠ Why the value cannot simply ride inside the Rhai error
//!
//! `EvalAltResult::ErrorRuntime` carries a `Dynamic`, which would be the natural home — but a
//! `Dynamic` requires `Clone + Send + Sync + 'static` and `vike_data::DataError` derives `Debug`
//! ALONE (`crates/vike-data/src/hist.rs`). So the typed value cannot travel INSIDE the interpreter
//! at all, and the fix has to be a side channel.
//!
//! # The side channel, and why it is a LOG rather than a slot
//!
//! Every raise appends the typed error to this log and embeds the entry's index in the message it
//! raises ([`FAULT_SENTINEL`]). On the way out, [`recover`] reads the LAST sentinel in the rendered
//! error and returns that entry.
//!
//! A single-slot version was the first shape and is wrong, because Rhai has `try`/`catch`: a study
//! that CATCHES a store fault and then fails for an unrelated reason would report the caught fault
//! as its cause. The sentinel makes the association explicit — a rendered error carrying no
//! sentinel is not one of ours, and a caught-then-discarded fault leaves an entry nothing points
//! at. This is the same shape `crates/vike-script/src/engine.rs`'s `register_user_form` uses when
//! it reads a `RhaiIndicator`'s fault back out rather than letting it read as a warm-up NaN.

use std::sync::{Arc, Mutex};

use vike_user_research::StudyError;

/// The marker a raised fault carries so [`recover`] can find its typed twin. Deliberately ugly and
/// deliberately not a word: it is appended to a message a script author may see, and it must not
/// read as advice.
pub(super) const FAULT_SENTINEL: &str = "[vike-study-fault:";

/// The typed faults raised during ONE `run` call, in raise order. `Option` because a `StudyError`
/// is not `Clone` and [`recover`] takes it out.
pub(super) type FaultLog = Arc<Mutex<Vec<Option<StudyError>>>>;

/// A fresh, empty log.
pub(super) fn new_log() -> FaultLog {
    Arc::new(Mutex::new(Vec::new()))
}

/// Record `e` and hand back the Rhai error to raise in its place. The rendered message is the
/// error's own `Display` plus the sentinel, so a script author reading the interpreter's output
/// sees the real sentence and a caller can still find the typed value.
pub(super) fn raise(faults: &FaultLog, e: StudyError) -> Box<rhai::EvalAltResult> {
    let mut log = faults.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let index = log.len();
    let msg = format!("{e} {FAULT_SENTINEL}{index}]");
    log.push(Some(e));
    msg.into()
}

/// The typed error behind a rendered Rhai failure, or `Study(rendered)` when this failure was not
/// one of ours (a genuine script error: a typo, a division by zero, the operation ceiling).
///
/// Reads the LAST sentinel, because a fault raised inside a `catch` block is the one that actually
/// stopped the study.
pub(super) fn recover(faults: &FaultLog, rendered: String) -> StudyError {
    let Some(index) = last_index(&rendered) else { return StudyError::Study(rendered) };
    let mut log = faults.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match log.get_mut(index).and_then(Option::take) {
        Some(e) => e,
        // Unreachable in practice: an index is only ever embedded by `raise`, which pushes first.
        // Spelled as the rendered message rather than a panic, because losing the typed variant is
        // survivable and aborting a run over an accounting slip is not.
        None => StudyError::Study(rendered),
    }
}

/// The index in the LAST `[vike-study-fault:N]` marker of `rendered`, if any.
fn last_index(rendered: &str) -> Option<usize> {
    let at = rendered.rfind(FAULT_SENTINEL)? + FAULT_SENTINEL.len();
    let rest = &rendered[at..];
    let end = rest.find(']')?;
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_error() -> StudyError {
        StudyError::Data(vike_data::DataError::Io("disk went away".into()))
    }

    /// THE property this module exists for: a store failure raised through the interpreter comes
    /// back as `StudyError::Data`, not as a sentence.
    #[test]
    fn a_typed_fault_survives_the_round_trip() {
        let log = new_log();
        let raised = raise(&log, data_error()).to_string();
        assert!(raised.contains("disk went away"), "the sentence is still readable: {raised}");
        assert!(matches!(recover(&log, raised), StudyError::Data(_)));
    }

    #[test]
    fn the_no_learner_variant_survives_too() {
        let log = new_log();
        let raised = raise(&log, StudyError::NoLearner("a 2-column matrix".into())).to_string();
        match recover(&log, raised) {
            StudyError::NoLearner(what) => assert_eq!(what, "a 2-column matrix"),
            other => panic!("expected NoLearner, got {other}"),
        }
    }

    /// A script error that is not ours keeps its own words and becomes the author's problem, which
    /// is the correct variant for it.
    #[test]
    fn an_ordinary_script_error_is_not_mistaken_for_a_typed_fault() {
        let log = new_log();
        let e = recover(&log, "Variable not found: xy (line 3)".into());
        assert!(matches!(e, StudyError::Study(ref m) if m.contains("xy")), "{e}");
    }

    /// The reason this is a LOG and not a slot: a study can `catch` a fault and carry on. The
    /// failure that actually stopped it is the LAST one raised, and a caught-then-discarded entry
    /// must not be reported as the cause.
    #[test]
    fn the_last_fault_wins_so_a_caught_one_is_not_reported_as_the_cause() {
        let log = new_log();
        let _caught = raise(&log, data_error());
        let stopped = raise(&log, StudyError::NoLearner("the second".into())).to_string();
        match recover(&log, stopped) {
            StudyError::NoLearner(what) => assert_eq!(what, "the second"),
            other => panic!("expected the SECOND fault, got {other}"),
        }
    }

    /// ...and a study that caught a fault and then failed on its OWN mistake reports the mistake,
    /// because the interpreter's message carries no sentinel of ours.
    #[test]
    fn a_caught_fault_does_not_hijack_a_later_unrelated_script_error() {
        let log = new_log();
        let _caught = raise(&log, data_error());
        let e = recover(&log, "Function not found: nope () (line 9)".into());
        assert!(matches!(e, StudyError::Study(ref m) if m.contains("nope")), "{e}");
    }

    /// A malformed or forged marker degrades to the rendered sentence rather than panicking or
    /// indexing something it did not raise — a script CAN put this text in a string of its own.
    #[test]
    fn a_forged_or_out_of_range_marker_degrades_to_the_message() {
        let log = new_log();
        for forged in [
            format!("{FAULT_SENTINEL}99]"),
            format!("{FAULT_SENTINEL}not-a-number]"),
            format!("{FAULT_SENTINEL}0"),
        ] {
            assert!(matches!(recover(&log, forged.clone()), StudyError::Study(_)), "{forged}");
        }
    }
}
