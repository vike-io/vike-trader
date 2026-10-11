//! The third-party data-provider keys the equity-calendar fetches need.

use std::collections::HashMap;

#[cfg(doc)]
use super::fetchers::spawn_tool_fetchers;

/// The env-var / store key holding the Finnhub key the weekly EARNINGS fetch needs.
pub const FINNHUB_API_KEY: &str = "FINNHUB_API_KEY";
/// The env-var / store key holding the FMP key the weekly DIVIDENDS fetch needs.
pub const FMP_API_KEY: &str = "FMP_API_KEY";

/// The third-party data-provider keys the equity-calendar fetches need — resolved by the BINARY
/// and handed to [`spawn_tool_fetchers`], never read here.
///
/// These are NOT venue credentials: they buy read-only market metadata (Finnhub earnings, FMP
/// dividends) for the calendar day-strip. Absent, those two counts are simply blank and the third
/// (Nasdaq IPOs, keyless) still lands — nothing about trading changes.
///
/// # Where they come from
///
/// The process environment first, the credential store second — the SAME store every venue
/// credential lives in (the settings database), resolved by the same `vike_secrets` walk. Both tiers arrive as
/// PARAMETERS ([`ToolApiKeys::resolve`]) because this is a library: only the binary reads global
/// configuration state.
///
/// ⚠ **A CWD-relative `./.env` used to be the second tier and is deliberately gone.** It was the
/// last location outside `<project>/settings/`, and CWD-relative is the worst kind — the same
/// binary reads a different file depending on the directory it was launched from, so a key that
/// works from the repo root vanishes when the app is started from anywhere else. There is no
/// legacy fallback: move the `FINNHUB_API_KEY=` / `FMP_API_KEY=` line into the credential store
/// (or export the variable), and the panel fills in again.
///
/// Resolved ONCE, at startup: a key added to the store afterwards needs a restart. The fetch loop
/// re-read it every 30 minutes before, which is not a property worth a per-cycle store read on a
/// background thread — the Connections tool is where live credential state is shown.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ToolApiKeys {
    /// `FINNHUB_API_KEY` — the weekly earnings calendar.
    pub finnhub: Option<String>,
    /// `FMP_API_KEY` — the weekly dividends calendar.
    pub fmp: Option<String>,
}

impl ToolApiKeys {
    /// Resolve both keys from an already-loaded process environment and credential store.
    ///
    /// PURE — no environment read, no file read, so the precedence below is unit-testable without
    /// mutating process-global state (`set_var` is unsound under threads; the repo's rule).
    pub fn resolve(env: &HashMap<String, String>, store: &HashMap<String, String>) -> Self {
        Self {
            finnhub: layered_key(env, store, FINNHUB_API_KEY),
            fmp: layered_key(env, store, FMP_API_KEY),
        }
    }
}

/// Redacts. A key is a secret, and this type is the only place one is now NAMED rather than living
/// as an anonymous local — so it gets the same manual `Debug` every credential type here has.
impl std::fmt::Debug for ToolApiKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown = |v: &Option<String>| if v.is_some() { "<set>" } else { "<unset>" };
        f.debug_struct("ToolApiKeys")
            .field("finnhub", &shown(&self.finnhub))
            .field("fmp", &shown(&self.fmp))
            .finish()
    }
}

/// One key, process env FIRST and credential store second.
///
/// A present-but-BLANK value falls THROUGH to the next tier rather than winning, at both tiers —
/// preserving the pre-injection behaviour exactly, and the right rule for this knob: an empty
/// `FINNHUB_API_KEY=` cannot mean "no key on purpose", because absent already means that and both
/// spellings produce the identical blank column. (`vike_polymarket`'s egress resolution makes
/// the OPPOSITE call for a proxy knob's legacy credential row, where an empty value genuinely meant
/// "direct" — and it reads no environment at all since decision 0095: an empty `POLY_SOCKS_PROXY`
/// variable now REFUSES startup, saying what the blank used to mean.)
fn layered_key(
    env: &HashMap<String, String>,
    store: &HashMap<String, String>,
    key: &str,
) -> Option<String> {
    [env, store]
        .into_iter()
        .filter_map(|m| m.get(key))
        .map(|v| v.trim())
        .find(|v| !v.is_empty())
        .map(str::to_string)
}

#[path = "api_key_tests.rs"]
#[cfg(test)]
mod api_key_tests;
