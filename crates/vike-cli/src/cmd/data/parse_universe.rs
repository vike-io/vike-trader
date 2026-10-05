//! `data hist universe` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit. The module this verb's pure half lives in is `universe`,
//! which is why this file is not simply called that.

use super::{Sub, Window, membership_window, refuse_foreign_flags, universe};

/// `universe`'s arm: the membership window is parsed here, as a usage error, because nothing is
/// forwarded. Returns the `(spec, window)` pair `parse` builds its `Args` from, and the
/// `MembershipWindow` it carries.
#[allow(clippy::type_complexity)] // the arm's `(spec, window)` pair plus the struct it builds
pub(super) fn parse(
    sub: Sub,
    partial_only: bool,
    class: bool,
    days: Option<String>,
    from: Option<String>,
    to: Option<String>,
) -> Result<(Option<String>, Option<Window>, Option<universe::MembershipWindow>), String> {
    let universe_window;
    let (spec, window) = {
        refuse_foreign_flags(
            sub,
            &[("--partial-only", partial_only)],
            "that flag annotates one series' recorded span, while a `universe` row is one \
                 INSTRUMENT's membership of a window. Whether a member's tape has holes IN it is \
                 `vike-cli data hist gaps`; this verb answers whether it was there at all",
        )?;
        // ⚠ `--class` is refused for a reason of its OWN, and it is the one this verb exists
        // to defend: every answer here is judged AS OF a window the operator wrote down, and
        // the class probe is deliberately as-of NOW ([`CLASS_AS_OF_TS`]). A class column on a
        // point-in-time membership report would be the one cell in it that changed meaning
        // between two runs over the same window — which is the survivorship defect wearing a
        // different field.
        refuse_foreign_flags(
            sub,
            &[("--class", class)],
            "every cell here is judged as of the window you named, and the recorded class is \
                 read as of NOW — so it would be the one column that answers differently on two \
                 runs over the same window. `vike-cli data hist ls --class` is where the class is",
        )?;
        // ⚠ `--days` is refused BY NAME here for a reason of its own, and not the one
        // `Sub::Export` gives. An export's `--days` is meaningless because a store's contents
        // have nothing to do with now(); a universe's would be WORSE than meaningless — it
        // would resolve, and it would resolve to a different window every day it is run. A
        // membership report is a thing two backtests are compared against, so its window has
        // to be a date somebody wrote down.
        if days.is_some() {
            return Err(
                "--days counts back from NOW, so a membership window built from it answers a \
                     different question every day it is run — and a point-in-time universe exists \
                     precisely to be re-askable. Name the dates: --from/--to (epoch-ms or \
                     YYYY-MM-DD), either of which stands alone"
                    .into(),
            );
        }
        universe_window = Some(membership_window(from.as_deref(), to.as_deref())?);
        (None, None)
    };
    Ok((spec, window, universe_window))
}
