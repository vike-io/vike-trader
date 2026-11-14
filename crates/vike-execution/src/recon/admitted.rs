//! What a pass may claim about the symbols a reconcile client reads BEYOND the one it was built on.
//!
//! An account's engine is admitted symbols on demand (decision 0173): the engine holds the mounted
//! symbol plus up to `MAX_EXTRA_SYMBOLS` admitted ones, and a restart restores positions and resting
//! orders for all of them. [`super::ReconcileClient::admit_symbol`] is how a client hears which symbols the
//! engine admitted; the bounded list it keeps is the bridges' shared one
//! (`crates/vike-bridge-core/src/reconcile_admitted.rs`'s `ReconcileAdmittedSymbols`). This module holds
//! the one rule the core applies to that list's answers ([`admitted_throughout`]).

/// **What a pass may claim its client read**: the symbols [`super::ReconcileClient::admitted_symbols`]
/// listed BEFORE the fetch and still lists AFTER it, in the first answer's order. One told only after
/// the fetch began was not in it, and one forgotten meanwhile (the daemon behind the client was
/// replaced) may not have been: claiming either lets the restore planner read the venue's silence as
/// "gone" for a symbol nobody fetched. Both `None` (a client with no list) is `None`; one answer
/// `None` and the other not is read as an empty list.
pub fn admitted_throughout(
    before: Option<Vec<String>>,
    after: Option<Vec<String>>,
) -> Option<Vec<String>> {
    if before.is_none() && after.is_none() {
        return None;
    }
    let after = after.unwrap_or_default();
    Some(before.unwrap_or_default().into_iter().filter(|s| after.contains(s)).collect())
}

#[cfg(test)]
#[path = "admitted_tests.rs"]
mod admitted_tests;
