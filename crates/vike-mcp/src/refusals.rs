//! The one table of the tool names this server no longer serves, and the refusal each one earns
//! (`vike_client::refusals::RenamedToolRefusal`, declared there so the workspace's `refusals.json`
//! walks it, and shown through the macro's `rendered()`).

use vike_client::refusals::RenamedToolRefusal;

/// Every tool name this server no longer serves, and the name it serves that tool under now. None
/// of them is on the roster, so `tools/list` never shows one.
pub const RENAMED_TOOLS: &[(&str, &str)] =
    &[("modify", "modify_order"), ("market_exit", "close_all")];

/// The refusal for a retired tool name; `None` for any other name.
#[must_use]
pub fn renamed(name: &str) -> Option<RenamedToolRefusal> {
    RENAMED_TOOLS
        .iter()
        .find(|(was, _)| *was == name)
        .map(|&(was, now)| RenamedToolRefusal::Renamed { was, now })
}
