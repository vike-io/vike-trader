//! `resolve` — the VENUE-FREE family resolvers. An explicit list is
//! `vike_data::live::FixedSymbols`; this file holds the glob over a venue's own instrument list,
//! which works for any venue implementing `vike_catalog::CatalogProvider`. It lived in
//! `venues/binance.rs` until 2026-09-28 although nothing in it was binance-specific.
use std::collections::BTreeSet;

use vike_catalog::{CatalogProvider, Instrument};
use vike_data::live::SymbolResolver;

/// Match a symbol against a family glob supporting a leading and/or trailing `*`.
///
/// Deliberately NOT a regex: a family key is something a customer types into a TOML file and should
/// be able to predict the meaning of. `*USDT` / `BTC*` / `*USDT.P` / `*` cover the real cases
/// (quote-asset, base-asset, perp-suffix, everything) and a literal with no `*` matches exactly one
/// symbol, which is how a one-instrument family degenerates to naming it.
pub fn glob_matches(pattern: &str, symbol: &str) -> bool {
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        // `*x*` — contains. Bare `*` and `**` degenerate to "contains empty" = everything.
        (Some(rest), Some(_)) => symbol.contains(rest.strip_suffix('*').unwrap_or(rest)),
        (Some(suffix), None) => symbol.ends_with(suffix),
        (None, Some(prefix)) => symbol.starts_with(prefix),
        (None, None) => symbol == pattern,
    }
}

/// Render a family PATTERN into a group NAME that can be a directory.
///
/// ⚠ **This is the OTHER half of [`glob_matches`], and confusing the two is the bug it prevents.**
/// That function takes the operator's glob and decides what the feed SUBSCRIBES to. This one takes
/// the same glob and decides what the store is asked to CREATE. A family key is free text and is
/// conventionally a glob (`*USDT.P`, `BTC*`, or a bare `*` for every listing); a group name is a
/// `group=…` path component, and `vike_model::store_path::PATH_HOSTILE_IN_A_SYMBOL` lists what may
/// not appear in one — `*` and `?` among them, because Windows refuses a directory carrying either.
///
/// ⚠ **The mapping is deliberately LOSSY-BUT-STABLE, not an escape.** `*` is DROPPED as an affix
/// and becomes `all` when it is the whole pattern, so `*USDT.P` renders `USDT.P` and `BTC*` renders
/// `BTC`. An escape (`%2A`, `_star_`) would round-trip, and round-tripping is not wanted: the
/// pattern is kept verbatim on [`CatalogGlob`]'s `pattern`, where the matching happens, so nothing
/// needs to recover it from a directory name. A group name only has to be a stable, readable label.
///
/// ⚠ Two patterns CAN collide — `*BTC` and `BTC*` both render `BTC`. Accepted rather than solved: a
/// profile naming both is recording one venue's instruments into one group twice, which is a
/// profile mistake and not something a name renderer should be catching. What is NOT accepted is a
/// name no directory can hold, which is what this function exists for.
///
/// ⚠ An empty result would be worse than any collision — a bare `*` must not render `""`, because
/// that is a `group=` component with no value. Hence the `all` fallback.
pub fn group_name_for(pattern: &str) -> String {
    let trimmed = pattern.trim_matches('*');
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !vike_model::store_path::PATH_HOSTILE_IN_A_SYMBOL.contains(c))
        .collect();
    if cleaned.is_empty() { "all".to_string() } else { cleaned }
}

/// A STATIC family: a glob matched against the venue's instrument list, resolved ONCE and cached
/// — a listing moves on the order of weeks, so re-fetching per tick would be a REST call a minute
/// for an answer that does not change. A restart re-resolves.
///
/// ⚠ The pattern and the group are DIFFERENT strings: the glob is kept verbatim for matching, the
/// group is [`group_name_for`]'s path-safe rendering (see that function).
pub struct CatalogGlob {
    pattern: String,
    group: String,
    provider: Box<dyn CatalogProvider>,
    /// Whole instruments, not symbols: the venue's own `asset_class` survives to here
    /// (`docs/decisions/0061` Phase 2's drop seam) and dies only at `desired`'s return type.
    resolved: Option<Vec<Instrument>>,
}

impl CatalogGlob {
    pub fn new(pattern: &str, provider: Box<dyn CatalogProvider>) -> Self {
        Self {
            pattern: pattern.to_string(),
            group: group_name_for(pattern),
            provider,
            resolved: None,
        }
    }
}

impl SymbolResolver for CatalogGlob {
    fn group(&self) -> Option<&str> {
        Some(&self.group)
    }

    fn desired(&mut self, _now_ms: i64) -> Result<BTreeSet<String>, String> {
        if self.resolved.is_none() {
            // `Err`, never an empty set: an empty set would unsubscribe every live stream
            // because the instrument list happened to time out.
            let venue = self.provider.venue().to_string();
            let list = self
                .provider
                .list_instruments()
                .map_err(|e| format!("{venue} instrument list: {e}"))?;
            self.resolved = Some(list);
        }
        Ok(self
            .resolved
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter(|i| glob_matches(&self.pattern, &i.raw_symbol))
            .map(|i| i.raw_symbol.clone())
            .collect())
    }
}

#[path = "resolve_tests.rs"]
#[cfg(test)]
mod resolve_tests;
