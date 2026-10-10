//! The texts the sequence refuses with: the seal.

/// **The seal disposition, as a function so a test can drive both sides of it.**
///
/// `refuses` is whether this root took
/// [`SettingsLoad::LoadAndRefuseUnsoundSeal`](crate::SettingsLoad::LoadAndRefuseUnsoundSeal);
/// `seal` is [`vike_config::Settings::seal_refusal`]. Extracted from [`boot`](crate::boot) because
/// the alternative is a test that plants a sealed SQLite store to exercise two branches of an
/// `if`, and because a seam nothing can call is a seam nothing can prove:
/// `crates/vike-cli/tests/seal_enforcement.rs` had a library-level predecessor that returned `Ok`
/// while every real binary exited 1.
pub(crate) fn seal_gate(refuses: bool, seal: Option<&str>) -> Result<(), String> {
    let Some(why) = seal else { return Ok(()) };
    if !refuses {
        return Ok(());
    }
    Err(format!(
        "REFUSING TO START: this box's settings seal is unsound, and this process places or gates \
         orders.\n\n{why}\n\nThe values this process resolved are not the ones the box was sealed \
         with, so an unreadable \
         row resolves to the compiled-in default, which for `policy.max_notional_per_order` is NO \
         SIZE CAP. Starting anyway would trade uncapped while an operator believed a ceiling was \
         armed.\n\nDiagnose with `vike-cli config check`, which names the row. The repair is \
         restoring `<project>/settings/db/vike.db` from this box's nightly backup — no command \
         repairs it."
    ))
}

#[cfg(test)]
mod seal_gate_tests;
