//! `BudgetMode` — whether the budget only COUNTS or also REFUSES, and the one constant that says which.

use serde::Serialize;

/// What the budget does with a request it judges over its split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetMode {
    /// Count the request, WARN when it would have been refused, and admit it. Wave 1.
    Shadow,
    /// Refuse the request (and do not count it). Never built into a daemon yet.
    Enforce,
}

/// The mode this build runs. **It is [`BudgetMode::Shadow`] (D4 of wave 1's decisions): only the trade
/// dispatch spends, so the counters see orders alone, a reserve for orders would be a guess, and a
/// refusal built on a guess is a new way to lose data.** Turning enforcement on is a
/// one-line change to this constant, in a later PR with the CI box evidence;
/// `budget_tests.rs`'s `wave_one_refuses_nothing` pins the value so that change is visible in a diff
/// and cannot arrive inside an unrelated one.
pub const MODE: BudgetMode = BudgetMode::Shadow;

/// The verdict on one request: let it through, or hold it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetAdmission {
    /// The request may go to the venue.
    Admit,
    /// The request must not go to the venue now. Produced only under [`BudgetMode::Enforce`].
    Refuse,
}
