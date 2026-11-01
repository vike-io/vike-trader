//! `data hist gaps` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit.

use super::{Sub, Window, refuse_foreign_flags};

/// `gaps`'s arm: it refuses the two listing aids whose question it does not answer.
pub(super) fn parse(
    sub: Sub,
    partial_only: bool,
    class: bool,
) -> Result<(Option<String>, Option<Window>), String> {
    Ok({
        // `--partial-only` for `ls`'s reason — a gap row is one series of one kind too.
        refuse_foreign_flags(
            sub,
            &[("--partial-only", partial_only)],
            "that flag filters `coverage`'s cross-kind verdict, and a `gaps` row is ONE \
                 series of ONE kind — there is nothing for it to disagree with",
        )?;
        // ⚠ `--class` is refused here and ACCEPTED on `ls`, and the split is the whole reason
        // these are two verbs. A recorded asset class is an annotation on WHAT EXISTS; a gap
        // is a statement about what does not. Putting the two in one table would cost one
        // extra round trip per distinct (venue, symbol) on top of the one per matched SERIES
        // this verb already pays — and would answer a question the operator did not ask, in
        // the output whose whole value is that every line in it is an absence.
        refuse_foreign_flags(
            sub,
            &[("--class", class)],
            "that flag annotates what a store HOLDS, and every line here is about what it \
                 does NOT — and it costs a second round trip per instrument on top of this verb's \
                 own one per series. `vike-cli data hist ls --class` is where the class is",
        )?;
        (None, None)
    })
}
