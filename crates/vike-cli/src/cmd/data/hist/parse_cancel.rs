//! `data hist cancel` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit.

use super::{Window, check_spec};

/// `cancel`'s arm: one spec, exactly as the fetch was asked for.
pub(super) fn parse(spec: Option<String>) -> Result<(Option<String>, Option<Window>), String> {
    Ok({
        let spec = spec.ok_or(
            "cancel needs a spec: VENUE:SYMBOL:INTERVAL, exactly as the fetch was asked for — \
                 e.g. `vike-cli data hist cancel oanda:EUR_USD:5s`. `vike-cli data hist running` \
                 lists every running fetch with its spec ready to paste",
        )?;
        check_spec(&spec)?;
        (Some(spec), None)
    })
}
