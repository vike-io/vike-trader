//! Arbitrage over a prediction market's COMPLETE OUTCOME SET: the maths any venue that lists mutually exclusive
//! outcomes shares (Polymarket's neg-risk sets, and the other prediction markets that list the same structure), so a
//! venue bridge stays a connection and holds none of it.
//!
//! **The structure.** A set of `N` mutually exclusive outcomes, exactly one of which resolves YES. Each outcome has a
//! YES share and a NO share, each pays `$1` if it is right and `$0` if not, so over the whole set
//!
//! - one YES of EVERY outcome pays exactly `$1` at resolution (the winner's), and
//! - one NO of EVERY outcome pays exactly `$N − 1` (every loser's; the winner's NO pays nothing).
//!
//! Buy either whole set for less than it pays, net of fees and of the cost of walking each leg's book, and the profit
//! is locked: [`outcome_set_lock`] sizes and nets that on the live per-leg books.
//!
//! **Convert or resolve.** The NO set's `N − 1` is the same number everywhere; WHEN the capital comes back is the
//! venue's. A venue that can convert a full NO set into `(N − 1)` collateral at once (Polymarket's neg-risk
//! `convertPositions`) frees the money on the spot; one that cannot pays it at resolution, locking the capital until
//! then. The venue says which ([`outcome_set_lock::PayoutTiming`]); the profit's arithmetic does not change.
//!
//! **The fee.** A prediction market's taker fee is a curve in the share's price, `rate · p · (1 − p)` per share
//! (the venue passes its [`vike_model::FeeSchedule`]; a market with no fee passes `FeeSchedule::Free`). It is charged
//! at each leg's VWAP, and because `p · (1 − p)` is concave that is an UPPER bound on the exact per-level fee
//! (Jensen), so the netted edge is conservative: never a phantom arbitrage from under-charging the fee.
//!
//! **Completeness is load-bearing, and it is the CALLER's to prove.** Dropping a member of a set can only lower a Σ, so a
//! partial set always looks like free money (a real one: 7 of 33 members of one set summed to `0.039`, a "96¢
//! arbitrage" that was the missing favourite). This module cannot tell a complete set from a partial one: the venue's
//! catalog can, and refuses before it calls in here. What it does check is what it is given: at least two outcomes
//! and a token for every outcome.
//!
//! - [`outcome_set_lock`] — the lock on the LIVE books: the two locks, each leg's walk for size and VWAP, the fee, and
//!   the net profit.
//! - [`cached_marks`] — the screen on the venue's CACHED marks: the buy-all-YES edge from the sum of the YES prices,
//!   ignoring depth, a SCREEN and never a trade signal.
//!
//! The venue-specific parts are the venue's: its catalog of sets (Gamma parsing, `NegRiskSet`), its on-chain convert,
//! its fee rates and its config. This module is read-only maths: it signs and sends nothing.

pub mod cached_marks;
pub mod outcome_set_lock;
