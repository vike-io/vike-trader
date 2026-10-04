//! `data hist coverage` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit.

use super::{Filter, Sub, Window, refuse_foreign_flags};

/// `coverage`'s arm: it refuses `--kind` and `--class`, for the reasons the body argues.
pub(super) fn parse(
    sub: Sub,
    filter: &Filter,
    class: bool,
) -> Result<(Option<String>, Option<Window>), String> {
    Ok({
        // ⚠ The `--gaps` refusal that used to open this arm has MOVED to the universal one in
        // [`parse`]'s flag section, because gaps is a VERB now — the unit argument it carried
        // (a gap range is epoch-ms, a coverage day is a UTC-day index) went with it. What is
        // left is the one refusal peculiar to this verb: `--kind` would filter away exactly
        // the kinds whose disagreement the report exists to show.
        refuse_foreign_flags(
            sub,
            &[("--kind", filter.kind.is_some())],
            "a coverage row IS the join across kinds, so filtering to one would leave a \
                 report that cannot show a day one kind has and another lacks — which is the \
                 whole of what it reports",
        )?;
        // ⚠ The refusal an operator is likeliest to argue with, since a class IS an instrument
        // fact and a coverage row IS an instrument — so the reason is the MEASURED one rather
        // than a taxonomy of verbs. `crates/vike-data/src/datafusion_hist/inventory.rs`'s
        // `coverage_report` enumerates only `crates/vike-data/src/coverage.rs`'s `TICK_KINDS`,
        // which excludes `bar`, so an instrument a `data hist fetch` wrote appears in NO row here at
        // all — measured on that function, not inferred. A class column on
        // this verb would be silently blind to the commonest thing a store holds, and an
        // operator reading it as "every instrument is classified" would be reading a subset.
        refuse_foreign_flags(
            sub,
            &[("--class", class)],
            "a `coverage` row is the join across the TICK kinds only — a bar-only instrument \
                 (what `vike-cli data hist fetch` writes) appears in none of them, so a class column \
                 here would answer for a subset of the store while reading like the whole of it. \
                 `vike-cli data hist ls --class` enumerates every stored series",
        )?;
        (None, None)
    })
}
