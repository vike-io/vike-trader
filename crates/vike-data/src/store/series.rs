//! Engine-free series identity + coverage types.
//!
//! [`SeriesId`] and [`SeriesCoverage`] are pure data — a `(kind, venue, symbol, interval, source)`
//! tuple and a handful of integers computed straight from the manifest file-index. They describe
//! the SHAPE of a stored series, not the DataFusion engine that reads it, so they live here rather
//! than in `vike-store` and downstream crates (e.g. `vike-app-core`'s Data-Manager inventory view)
//! can group/roll-up an inventory listing WITHOUT pulling the Arrow/DataFusion tree into their
//! build.
//! `HistoryStoreDataFusion::{list_series, series_coverage, inventory}` (in `vike-store`) produce them;
//! everything here is engine-agnostic.

use crate::store::store_kind::{StoreKindId, USER_LANE_PREFIX, refuse_an_unknown_lane};

/// Identifies one series in a store: the `(kind, venue, symbol, interval, source)` tuple its leaf
/// directory `kind=…/venue=…[/source=…]/symbol=…[/interval=…]` encodes. `interval` is `Some` for
/// bars (which sub-partition by bar step) and `None` for ticks (quotes/trades). Produced by
/// `HistoryStoreDataFusion::list_series` (which parses it straight back out of the path segments) and
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
    /// The `kind=` segment — one of the closed [`StoreKindId`] set.
    pub kind: StoreKindId,
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
    /// which is not the venue and not the vendor (`crate::store::store_kind::StoreKindCommitKey::source` is the
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
    /// `HistoryStoreDataFusion::series_dir`'s doc for the structural fact that forces the position.
    ///
    /// ⚠ **Two verbs write a `Some`:** `vike-store`'s `append_quotes_sourced` (decision 0193), for the
    /// recorder's lane of a venue that also has an archive and the holes lane of a tape, and its
    /// `append_bars_sourced`, for bars that are not the venue's own candles
    /// ([`crate::store::store_kind::AGGTRADES_LANE`]). `HistoryStore::scan_quotes_sourced_capped` and
    /// `HistoryStore::load_bars_sourced_capped` are the reads that name a lane, so `inventory` and
    /// `coverage_report` answer for the sourceless leaves and only `list_series` shows a sourced one.
    /// The one-way door of §6.3 is written out at
    /// `parse_series_id` in `vike-store`'s `datafusion_hist`.
    ///
    /// `#[serde(default)]` so an inventory persisted before the dimension existed still
    /// deserializes — the precedent `group` set on the same wire.
    #[serde(default)]
    pub source: Option<String>,
}

impl SeriesId {
    /// A per-symbol series — today's layout, and what every caller predating grouping means.
    pub fn per_symbol(
        kind: StoreKindId,
        venue: impl Into<String>,
        symbol: impl Into<String>,
        interval: Option<String>,
    ) -> Self {
        SeriesId {
            kind,
            venue: venue.into(),
            symbol: symbol.into(),
            interval,
            group: None,
            source: None,
        }
    }

    /// A grouped series — one part holding many symbols (see [`SeriesId::group`]).
    pub fn grouped(kind: StoreKindId, venue: impl Into<String>, group: impl Into<String>) -> Self {
        SeriesId {
            kind,
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
    /// and that stays their answer — neither signature moves. Only a caller holding a lane says so.
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

/// The liveness key of one series, `{kind}/{venue}/{label}` ([`SeriesId::label`]): the one spelling
/// the recorder's writer, its loss report, the subscription set and the silence watchdog join on. A
/// changed spelling makes a subscription match no liveness row.
pub fn series_key(kind: impl std::fmt::Display, venue: &str, label: &str) -> String {
    format!("{kind}/{venue}/{label}")
}

/// A [`series_key`]'s `(kind, venue, label)`, or `None` for fewer than three parts. A `/` in a symbol
/// survives: the key splits into at most three parts.
pub fn parse_series_key(key: &str) -> Option<(&str, &str, &str)> {
    let mut parts = key.splitn(3, '/');
    Some((parts.next()?, parts.next()?, parts.next()?))
}

/// **Parse an INPUT name** (decision 0271), `VENUE:SYMBOL:INTERVAL` or `LANE:VENUE:SYMBOL:INTERVAL`,
/// into the bar series it reads and one bar's length in ms: the CLI's series spec (`vike-cli data hist
/// get`), with the `source=` lane before it. Three parts or four, nothing else; the lane one the store
/// declares or a user's ([`refuse_an_unknown_lane`]); the interval one with a length, because an input
/// is read as of its bars' close. Here, below both readers, because a backtest
/// (`vike_backtest::data_plan`) and a live mount (`vike-tradehub`) read the same names.
///
/// # Errors
/// The sentence naming the spelling and what it breaks.
pub fn parse_input_name(name: &str) -> Result<(SeriesId, i64), String> {
    let parts: Vec<&str> = name.split(':').collect();
    let (lane, venue, symbol, interval) = match parts.as_slice() {
        [venue, symbol, interval] => (None, *venue, *symbol, *interval),
        [lane, venue, symbol, interval] => (Some(*lane), *venue, *symbol, *interval),
        _ => {
            return Err(format!(
                "the input name {name:?} is not VENUE:SYMBOL:INTERVAL (a venue's own candles, as \
                 `vike-cli data hist get` names a series) or LANE:VENUE:SYMBOL:INTERVAL (a lane of \
                 the store, such as a CSV import's `{USER_LANE_PREFIX}<label>`)"
            ));
        }
    };
    if [venue, symbol, interval].iter().any(|p| p.is_empty() || p.trim() != *p) {
        return Err(format!(
            "the input name {name:?} has an empty or padded part: VENUE, SYMBOL and INTERVAL are \
             each a word"
        ));
    }
    let Some(interval_ms) = vike_marketdata::interval::interval_ms(interval).filter(|ms| *ms > 0)
    else {
        return Err(format!(
            "the input name {name:?} has the interval {interval:?}, which is not a bar length (a \
             count then a unit, such as 1m, 4h or 1d): an input is read as of its bars' close, so its \
             bars need one"
        ));
    };
    let id = SeriesId::per_symbol(StoreKindId::Bar, venue, symbol, Some(interval.to_string()));
    let id = match lane {
        Some(lane) => {
            refuse_an_unknown_lane(lane).map_err(|e| format!("the input name {name:?}: {e}"))?;
            id.with_source(lane)
        }
        None => id,
    };
    Ok((id, interval_ms))
}

/// The series a reader's OWN symbol is, which a ONE-PART input name reads at another bar length
/// ([`parse_input_name_for`]): the venue and symbol it trades, and the lane it reads its own bars from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeriesOwn<'a> {
    pub venue: &'a str,
    pub symbol: &'a str,
    pub lane: Option<&'a str>,
}

/// **Parse an input name as a reader with its own symbol reads it** (decision 0271): `INTERVAL` alone
/// (`"1d"`) is `own` at that bar length, in its lane; any other spelling is [`parse_input_name`]'s. The
/// one grammar a backtest (`vike_backtest::data_plan`, `own` its profile's one symbol) and a live mount
/// (`vike-tradehub`, `own` the mount's symbol) read names by. `own` is `Err(why)` for a reader that has
/// no one own symbol (a run of several symbols, of a file): a one-part name is then refused with `why`.
///
/// # Errors
/// [`parse_input_name`]'s sentence, `own`'s, or the one saying a one-part name is no bar length.
pub fn parse_input_name_for(
    name: &str,
    own: Result<SeriesOwn<'_>, String>,
) -> Result<(SeriesId, i64), String> {
    if name.contains(':') {
        return parse_input_name(name);
    }
    if vike_marketdata::interval::interval_ms(name).filter(|ms| *ms > 0).is_none() {
        return Err(format!(
            "the input name {name:?} is one part, the reader's own symbol at a bar length, and {name:?} \
             is not one (a count then a unit, such as 1m, 4h or 1d)"
        ));
    }
    let SeriesOwn { venue, symbol, lane } = own.map_err(|why| {
        format!(
            "the input name {name:?} names the reader's own symbol at another bar length, and {why}: \
             name the series in full, VENUE:SYMBOL:INTERVAL"
        )
    })?;
    let spelled = match lane {
        Some(lane) => format!("{lane}:{venue}:{symbol}:{name}"),
        None => format!("{venue}:{symbol}:{name}"),
    };
    parse_input_name(&spelled).map_err(|e| format!("the input name {name:?} reads {e}"))
}

/// **The `[inputs]` rows that do not parse** (decision 0271): the one load-time check of a remap table,
/// `"<the name the code writes>" = "<the series to read instead>"`, which both profiles run (the backtest's
/// `vike_backtest::harness::ProfileBacktest::validate`, the daemon's `vike_tradehub::config::ProfileDaemon`'s).
/// Both names of every row go through [`parse_input_name_for`] against `own`; each that does not parse is
/// `(name, read, why)`, in row order. Each profile words its own refusal from it: the backtest's sentence is
/// a refusal site its published profile reference reads from its own source.
pub fn unparsed_input_rows<'a>(
    inputs: &'a std::collections::BTreeMap<String, String>,
    own: Result<SeriesOwn<'_>, String>,
) -> Vec<(&'a str, &'a str, String)> {
    let mut out = Vec::new();
    for (name, read) in inputs {
        for spelled in [name, read] {
            if let Err(e) = parse_input_name_for(spelled, own.clone()) {
                out.push((name.as_str(), read.as_str(), e));
            }
        }
    }
    out
}

/// Cheap per-series coverage, computed from the manifest file-index (NO DataFusion scan).
/// `bytes` is the summed on-disk size of the part files; everything else comes from the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SeriesCoverage {
    pub first_timestamp: i64,
    pub last_timestamp: i64,
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
        assert_eq!(
            id,
            SeriesId::per_symbol(StoreKindId::Bar, "binance", "BTCUSDT", Some("1m".into()))
        );
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
            SeriesId::per_symbol(StoreKindId::Quote, "polymarket", "TOK", None),
            SeriesId::per_symbol(StoreKindId::Quote, "polymarket", "TOK", None).with_source("pmxt"),
            SeriesId::grouped(StoreKindId::Book, "polymarket", "btc-5m").with_source("recorder"),
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
        let base = SeriesId::per_symbol(StoreKindId::Book, "polymarket", "TOK", None);
        let archive = base.clone().with_source("pmxt");
        let recorded = base.clone().with_source("recorder");
        assert_ne!(archive, recorded);
        assert_ne!(archive, base, "a sourced id is not its own sourceless leaf");
        // Sorted enumeration order stays total and deterministic across the three.
        let mut all = vec![recorded.clone(), base.clone(), archive.clone()];
        all.sort();
        assert_eq!(all, vec![base, archive, recorded]);
    }

    /// Row 10's table: the home spells every builder's `{}/{}/{}` byte for byte, over every kind, a
    /// grouped label and a symbol with a `/`, and hands the same three fields back.
    #[test]
    fn a_series_key_is_the_one_spelling_and_parses_back() {
        let id = SeriesId::grouped(StoreKindId::Book, "polymarket", "btc-5m");
        assert_eq!(series_key(id.kind, &id.venue, id.label()), "book/polymarket/btc-5m");
        for kind in [StoreKindId::Quote, StoreKindId::Trade, StoreKindId::Book, StoreKindId::Depth]
        {
            for (venue, label) in
                [("binance", "BTCUSDT.P"), ("polymarket", "group=btc-5m"), ("oanda", "EUR/USD/x")]
            {
                let key = series_key(kind, venue, label);
                assert_eq!(key, format!("{kind}/{venue}/{label}"));
                let word = kind.to_string();
                assert_eq!(parse_series_key(&key), Some((word.as_str(), venue, label)));
                assert_eq!(key.split('/').nth(1), Some(venue), "a venue is the second field");
            }
        }
        assert_eq!(parse_series_key("depth/binance"), None, "two parts are no series key");
        assert_eq!(parse_series_key(""), None);
        assert_eq!(parse_series_key("a/b/"), Some(("a", "b", "")), "as `splitn` always read it");
    }

    /// The label is the SYMBOL-or-group answer and the source is not folded into it — stated as a
    /// test because the residual it leaves (two identical-looking rows in a UI) is declared on
    /// [`SeriesId::label`]'s own doc, and a future "helpful" widening would break every consumer
    /// that matches a label against a symbol it was handed.
    #[test]
    fn the_label_ignores_the_source() {
        let id =
            SeriesId::per_symbol(StoreKindId::Book, "polymarket", "TOK", None).with_source("pmxt");
        assert_eq!(id.label(), "TOK");
        assert_eq!(
            SeriesId::grouped(StoreKindId::Book, "polymarket", "g").with_source("pmxt").label(),
            "g"
        );
    }
}
