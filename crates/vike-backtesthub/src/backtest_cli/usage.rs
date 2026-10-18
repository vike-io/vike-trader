//! `backtest`'s operator-facing spellings that more than one surface must agree on: the words that
//! are SUBCOMMANDS, and the optimizer-listing flag.
//!
//! ⚠ **The usage PAGES are not here any more.** `USAGE`, `DATA_USAGE` and `TRIALS_USAGE` were three
//! hand-written copies of what the parser accepted; the text `--help` prints is generated from the
//! `clap` types in `args.rs` (decision 0155), whose doc comments are the per-flag prose.

use super::{DATA_SUBCOMMAND, TRIALS_SUBCOMMAND};

/// The optimizer-listing spelling, typed ONCE: it is the long name `args.rs`'s
/// `RunArgs::list_optimizers` derives, and `the_optimizer_listing_flag_is_the_spelling_the_vocabulary_declares`
/// holds it against `vike_model::run_request::flag_vocab`'s row and against the `clap` command.
/// Two surfaces name this flag and neither may spell it differently.
#[cfg(test)]
pub(super) const LIST_OPTIMIZERS_FLAG: &str = "--list-optimizers";

/// The words that are SUBCOMMANDS here and therefore can never be read as the positional profile:
/// `(word, what follows it)`. Ruling 12's `data` and stage 5's `trials` collide with ruling 14's
/// positional profile on exactly one token each, and the router resolves that by routing only when
/// the word comes FIRST. This table is the OTHER half — the refusal for the case that reaches
/// `args.rs`'s `profile_from_args`.
pub(super) const SUBCOMMANDS: &[(&str, &str)] = &[
    (DATA_SUBCOMMAND, "<fetch|fetch-starter|seed-demo|export|rm|repair>"),
    (TRIALS_SUBCOMMAND, "<id>"),
];
