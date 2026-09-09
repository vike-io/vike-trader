//! Feature-free series identity + coverage types.
//!
//! [`SeriesId`] and [`SeriesCoverage`] are pure data — a `(kind, venue, symbol, interval)` tuple and
//! a handful of integers computed straight from the manifest file-index. They describe the SHAPE of
//! a stored series, not the DataFusion engine that reads it, so they live in the crate's base (no
//! `hist-datafusion`) and downstream crates (e.g. `vike-app-core`'s Data-Manager inventory view) can
//! group/roll-up an inventory listing WITHOUT pulling the Arrow/DataFusion tree into their build.
//! `DataFusionHist::{list_series, series_coverage, inventory}` (feature `hist-datafusion`) produce
//! them; everything here is engine-agnostic.

/// Identifies one series in a store: the `(kind, venue, symbol, interval)` tuple its leaf directory
/// `kind=…/venue=…/symbol=…[/interval=…]` encodes. `interval` is `Some` for bars (which sub-partition
/// by bar step) and `None` for ticks (quotes/trades). Produced by
/// [`crate::DataFusionHist::list_series`] (which parses it straight back out of the path segments) and
/// fed to `compact_series` / `apply_retention` — so `interval.as_deref()` is what those want.
///
/// `Ord` is derived so `list_series` can return a stable, sorted enumeration (deterministic
/// maintenance order + easy test assertions). `Serialize`/`Deserialize` (like [`SeriesCoverage`])
/// so the metadata verbs (`list_series`/`inventory`/`series_gaps`) can ride the datahub RPC wire —
/// it is pure `(kind, venue, symbol, interval)` data, not an engine type.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct SeriesId {
    /// Data kind: `bar` | `quote` | `trade`.
    pub kind: String,
    /// Venue slug (e.g. `binance`, `okx`).
    pub venue: String,
    /// Instrument symbol (e.g. `BTCUSDT`) for a PER-SYMBOL series; **empty** for a grouped one —
    /// see [`SeriesId::group`].
    pub symbol: String,
    /// Bar step (`Some("1m")`) for bars; `None` for tick series.
    pub interval: Option<String>,
    /// `Some(g)` iff this is a GROUPED series (`group=g` in the path), which holds MANY symbols in
    /// one part — told apart by the row-level symbol column — instead of one series per symbol.
    ///
    /// `symbol` is EMPTY in that case, deliberately: a consumer that scans by `id.symbol` then
    /// fails visibly rather than silently scanning `""`. The two are alternatives, not a pair —
    /// exactly one of `symbol`/`group` is meaningful for any given series.
    ///
    /// `#[serde(default)]` so an inventory persisted before grouping existed still deserializes.
    #[serde(default)]
    pub group: Option<String>,
}

impl SeriesId {
    /// A per-symbol series — today's layout, and what every caller predating grouping means.
    pub fn per_symbol(
        kind: impl Into<String>,
        venue: impl Into<String>,
        symbol: impl Into<String>,
        interval: Option<String>,
    ) -> Self {
        SeriesId {
            kind: kind.into(),
            venue: venue.into(),
            symbol: symbol.into(),
            interval,
            group: None,
        }
    }

    /// A grouped series — one part holding many symbols (see [`SeriesId::group`]).
    pub fn grouped(
        kind: impl Into<String>,
        venue: impl Into<String>,
        group: impl Into<String>,
    ) -> Self {
        SeriesId {
            kind: kind.into(),
            venue: venue.into(),
            symbol: String::new(),
            interval: None,
            group: Some(group.into()),
        }
    }

    /// How this series is named on disk and in a UI: its group if grouped, else its symbol.
    pub fn label(&self) -> &str {
        self.group.as_deref().unwrap_or(&self.symbol)
    }
}

/// Cheap per-series coverage, computed from the manifest file-index (NO DataFusion scan).
/// `bytes` is the summed on-disk size of the part files; everything else comes from the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeriesCoverage {
    pub first_ts: i64,
    pub last_ts: i64,
    pub rows: u64,
    pub bytes: u64,
    pub parts: usize,
    pub dates: usize,
}
