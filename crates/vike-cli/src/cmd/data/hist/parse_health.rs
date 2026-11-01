//! `data hist health` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar (the verb `Sub::TapeHealth` names).
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit. The module this verb's pure half lives in is
//! `tape_health`.

use super::{Sub, Window, refuse_foreign_flags};

/// `health`'s arm: it refuses the absence flag and the class probe, for the reasons the body
/// argues.
pub(super) fn parse(
    sub: Sub,
    partial_only: bool,
    class: bool,
) -> Result<(Option<String>, Option<Window>), String> {
    Ok({
        // The refusal is about the DISTINCTION this verb exists to draw, not about tidiness.
        // `--partial-only` reports ABSENCE — a day one kind lacks — and absence is what the
        // sibling verbs already answer correctly. This one reports what is PRESENT and
        // impossible. Accepting an absence flag here would put the two units in one output
        // under one heading.
        refuse_foreign_flags(
            sub,
            &[("--partial-only", partial_only)],
            "that flag reports what is MISSING, and this verb reports what is PRESENT and \
                 self-contradictory — a gap is absence, which `vike-cli data hist gaps` and \
                 `vike-cli data hist coverage` already answer. Mixing the two puts two different \
                 meanings of `wrong` in one table",
        )?;
        // ⚠ A SEPARATE refusal from the pair above, because the reason is not the absence/
        // presence split — it is this verb's declared property that NO ROW CROSSES THE WIRE
        // (see [`execute_tape_health`]): every finding is arithmetic over the coverage numbers
        // one `inventory()` already carried. A class probe is a second round trip per
        // instrument, and an unclassified instrument is not a contradiction in its own catalog.
        refuse_foreign_flags(
            sub,
            &[("--class", class)],
            "every finding here is folded from the ONE inventory this verb already fetched, \
                 and a recorded class is a second round trip per instrument that no finding is \
                 derived from — an instrument naming no class is a wiring gap, not a catalog that \
                 contradicts itself. `vike-cli data hist ls --class` is where the class is",
        )?;
        (None, None)
    })
}
