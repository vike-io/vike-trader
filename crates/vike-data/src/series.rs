//! Feature-free series identity + coverage types.
//!
//! [`SeriesId`] and [`SeriesCoverage`] are pure data — a `(kind, venue, symbol, interval, source)`
//! tuple and a handful of integers computed straight from the manifest file-index. They describe
//! the SHAPE of a stored series, not the DataFusion engine that reads it, so they live in the
//! crate's base (no `hist-datafusion`) and downstream crates (e.g. `vike-app-core`'s Data-Manager
//! inventory view) can group/roll-up an inventory listing WITHOUT pulling the Arrow/DataFusion tree
//! into their build.
//! `DataFusionHist::{list_series, series_coverage, inventory}` (feature `hist-datafusion`) produce
//! them; everything here is engine-agnostic.

/// Identifies one series in a store: the `(kind, venue, symbol, interval, source)` tuple its leaf
/// directory `kind=…/venue=…[/source=…]/symbol=…[/interval=…]` encodes. `interval` is `Some` for
/// bars (which sub-partition by bar step) and `None` for ticks (quotes/trades). Produced by
/// [`crate::DataFusionHist::list_series`] (which parses it straight back out of the path segments) and
/// fed to `compact_series` / `apply_retention` — so `interval.as_deref()` is what those want.
///
/// `Ord` is derived so `list_series` can return a stable, sorted enumeration (deterministic
/// maintenance order + easy test assertions). `Serialize`/`Deserialize` (like [`SeriesCoverage`])
/// so the metadata verbs (`list_series`/`inventory`/`series_gaps`) can ride the datahub RPC wire —
/// it is pure `(kind, venue, symbol, interval, source)` data, not an engine type. Both `group` and
/// `source` carry `#[serde(default)]`, so an id persisted or sent before either dimension existed
/// still deserializes.
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
    /// `Some(s)` iff this series' leaf carries a `source=s` segment — the LANE the rows came down,
    /// which is not the venue and not the vendor (`crate::store_kind::CommitKey::source` is the
    /// vocabulary).
    ///
    /// **`None` is a real value, not a missing one: it means the source was not recorded.** That is
    /// what lets the dimension exist with no migration — every leaf in every store today is
    /// sourceless, reads unchanged and stays that way until a producer is deliberately scoped
    /// (`docs/superpowers/specs/2026-09-07-store-source-dimension-design.md` §6.1). It sits beside
    /// [`SeriesId::interval`] and [`SeriesId::group`], which mean the same kind of thing.
    ///
    /// ⚠ **Unlike `group`, this is NOT an alternative to `symbol`.** A sourced series still has a
    /// symbol or a group; the source is a further dimension above both, which is why the segment
    /// sits under `venue=` and ABOVE `symbol=`/`group=` — see
    /// `crate::DataFusionHist::series_dir`'s doc for the structural fact that forces the position.
    ///
    /// ⚠ **Nothing in this tree writes a `Some` yet.** The identity half ships ahead of any
    /// producer on purpose, so that the window between "a build can READ a sourced leaf" and "a
    /// store CONTAINS one" is as wide as the owner wants it (§6.3, and the one-way door it names is
    /// written out at `crate::datafusion_hist`'s `parse_series_id`).
    ///
    /// `#[serde(default)]` so an inventory persisted before the dimension existed still
    /// deserializes — the precedent `group` set on the same wire.
    #[serde(default)]
    pub source: Option<String>,
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
            source: None,
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
            source: None,
        }
    }

    /// The same series, addressed inside one SOURCE lane (see [`SeriesId::source`]).
    ///
    /// A builder rather than two more constructors, because every existing call site of
    /// [`SeriesId::per_symbol`] and [`SeriesId::grouped`] across the workspace means "sourceless"
    /// and that stays their answer — neither signature moves. Only a caller holding a lane says so,
    /// and today that is tests alone: no producer is scoped, so nothing under a crate's `src/`
    /// calls this.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// How this series is named on disk and in a UI: its group if grouped, else its symbol.
    ///
    /// ⚠ **The source is deliberately NOT folded in here.** Two sources of one series share a
    /// label, so a UI that renders only this shows two identical rows — the residual §6.5 names for
    /// the datahub wire, which this method is the local twin of. Widening the label to disambiguate
    /// them would break every consumer that matches a label against a symbol it was given.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **Absence is a valid value, and the wire proves it.** An id serialized before the dimension
    /// existed carries no `source` key at all; `#[serde(default)]` is what turns that into `None`
    /// rather than into a decode error, which is the whole reason this field could be added to a
    /// type that rides the datahub RPC wire without a `PROTO_VERSION` bump (the design's §6.5).
    #[test]
    fn an_id_persisted_before_the_dimension_existed_still_decodes() {
        let legacy = r#"{"kind":"bar","venue":"binance","symbol":"BTCUSDT","interval":"1m"}"#;
        let id: SeriesId = serde_json::from_str(legacy).expect("a pre-source id must decode");
        assert_eq!(id, SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".into())));
        assert_eq!(id.source, None);
        assert_eq!(id.group, None, "the `group` precedent this field copies still holds");

        // ...and the ANTI-VACUITY control: a payload that DOES carry one is not silently dropped
        // on the way in, which is what a `#[serde(skip)]` or a missing field would look like from
        // the assertion above alone.
        let sourced = r#"{"kind":"bar","venue":"binance","symbol":"BTCUSDT","interval":"1m",
                          "group":null,"source":"recorder"}"#;
        let id: SeriesId = serde_json::from_str(sourced).expect("a sourced id must decode");
        assert_eq!(id.source.as_deref(), Some("recorder"));
    }

    /// A round trip through JSON is lossless in both states, so an inventory that crosses the wire
    /// and comes back addresses the same leaf.
    #[test]
    fn a_source_survives_a_round_trip_and_so_does_its_absence() {
        for id in [
            SeriesId::per_symbol("quote", "polymarket", "TOK", None),
            SeriesId::per_symbol("quote", "polymarket", "TOK", None).with_source("pmxt"),
            SeriesId::grouped("book", "polymarket", "btc-5m").with_source("recorder"),
        ] {
            let back: SeriesId =
                serde_json::from_str(&serde_json::to_string(&id).unwrap()).unwrap();
            assert_eq!(back, id);
        }
    }

    /// **Two lanes of one series are two DIFFERENT ids** — the property the whole dimension rests
    /// on, and the one a derived `PartialEq`/`Hash`/`Ord` gives for free ONLY while the field is
    /// actually part of the struct. A `#[serde(skip)]` or a hand-written `PartialEq` that forgot it
    /// would make the archive's copy and the recorder's compare EQUAL, at which point
    /// `list_series` dedups them away and the two can never be told apart again.
    #[test]
    fn two_lanes_of_one_series_are_two_ids() {
        let base = SeriesId::per_symbol("book", "polymarket", "TOK", None);
        let archive = base.clone().with_source("pmxt");
        let recorded = base.clone().with_source("recorder");
        assert_ne!(archive, recorded);
        assert_ne!(archive, base, "a sourced id is not its own sourceless leaf");
        // Sorted enumeration order stays total and deterministic across the three.
        let mut all = vec![recorded.clone(), base.clone(), archive.clone()];
        all.sort();
        assert_eq!(all, vec![base, archive, recorded]);
    }

    /// The label is the SYMBOL-or-group answer and the source is not folded into it — stated as a
    /// test because the residual it leaves (two identical-looking rows in a UI) is declared on
    /// [`SeriesId::label`]'s own doc, and a future "helpful" widening would break every consumer
    /// that matches a label against a symbol it was handed.
    #[test]
    fn the_label_ignores_the_source() {
        let id = SeriesId::per_symbol("book", "polymarket", "TOK", None).with_source("pmxt");
        assert_eq!(id.label(), "TOK");
        assert_eq!(SeriesId::grouped("book", "polymarket", "g").with_source("pmxt").label(), "g");
    }
}
