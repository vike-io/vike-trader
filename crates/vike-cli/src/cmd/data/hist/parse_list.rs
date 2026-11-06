//! `data hist ls` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit.

use super::{Sub, Window, refuse_foreign_flags};

/// `ls`'s arm: one refusal, for the flag that filters `coverage`'s cross-kind verdict.
pub(super) fn parse(
    sub: Sub,
    partial_only: bool,
) -> Result<(Option<String>, Option<Window>), String> {
    Ok({
        // `--partial-only` is a verdict about the CROSS-KIND join, and a per-series listing has
        // no such verdict to filter on — each row here is one kind, which can never disagree
        // with itself.
        refuse_foreign_flags(
            sub,
            &[("--partial-only", partial_only)],
            "that flag filters `coverage`'s cross-kind verdict, and an `ls` row is ONE \
                 series of ONE kind — there is nothing for it to disagree with",
        )?;
        (None, None)
    })
}
