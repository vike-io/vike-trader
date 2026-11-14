//! `data hist running` — the arm of the per-subcommand `match` in `super::parse` that holds its
//! grammar.
//!
//! Moved here verbatim from that function (code-layout phase 2, task 11); see
//! `parse_fetch` for how the pieces fit.

use super::Window;

/// `running`'s arm: it takes no spec, so what is left to decide is the one positional.
pub(super) fn parse(spec: Option<String>) -> Result<(Option<String>, Option<Window>), String> {
    // The running-door arm refused every store, window and listing flag above; what is left
    // to decide is the one positional.
    Ok({
        if let Some(extra) = &spec {
            return Err(format!(
                "'{extra}': `running` takes no spec — it lists EVERY fetch the datahub is \
                     running. To stop the ones on one series, `vike-cli data hist cancel {extra}`"
            ));
        }
        (None, None)
    })
}
