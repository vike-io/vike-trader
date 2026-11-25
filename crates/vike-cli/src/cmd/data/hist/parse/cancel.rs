//! `data history cancel` — the arm of the per-subcommand `match` in `super` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `fetch` for how the pieces fit.

use super::super::{Window, check_spec};

/// `cancel`'s arm: one spec, exactly as the fetch was asked for.
pub(in crate::cmd::data::hist) fn parse(
    spec: Option<String>,
) -> Result<(Option<String>, Option<Window>), String> {
    Ok({
        let spec = spec.ok_or(
            "cancel needs a spec: VENUE:SYMBOL:INTERVAL, exactly as the fetch was asked for — \
                 e.g. `vike-cli data history cancel oanda:EURUSD:5s`. `vike-cli data history running` \
                 lists every running fetch with its spec ready to paste",
        )?;
        check_spec(&spec)?;
        (Some(spec), None)
    })
}
