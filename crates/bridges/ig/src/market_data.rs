//! IG Lightstreamer market-data normalizers — the PURE decode layer between the TLCP codec
//! ([`crate::lightstreamer`]) and the live feed driver ([`crate::market_feed`]), the IG analogue
//! of the crypto venues' `market_data` tick normalizers.
//!
//! Two lanes, both MERGE-mode (one item per subscription, `LS_snapshot=true`):
//!
//! - **Quotes** — `MARKET:{epic}` over [`QUOTE_SCHEMA`]: best BID/OFFER (L1 only; IG's streaming
//!   API serves NO depth), plus MARKET_STATE so a closed market's nulled sides stop emissions
//!   rather than fabricating a 0/0 quote. IG stamps no epoch on this item (UPDATE_TIME is a bare
//!   HH:MM:SS), so the feed stamps receive time.
//! - **Candles** — `CHART:{epic}:{scale}` over [`CANDLE_SCHEMA`]: bid/offer OHLC folded to MID
//!   bars, mirroring the REST history convention (`crate::data::parse_prices` — one venue, one
//!   bar law). `UTM` is the candle's open epoch-ms; `CONS_END=1` marks consolidation end (the
//!   lossless close), and a `UTM` advance without one still closes the previous bar (rollover
//!   close, the hyperliquid shape) so a dropped `CONS_END` frame cannot silently lose a close.
//!
//! Both folds tolerate TLCP's delta encoding by construction: they fold into a
//! [`MergeItemState`] and read the FOLDED values, never the raw update.

use vike_model::Bar;

use crate::lightstreamer::{LsUpdate, MergeItemState};

/// `LS_group` for the L1 quote lane.
pub fn market_item(epic: &str) -> String {
    format!("MARKET:{epic}")
}

/// `LS_group` for the candle lane at an IG chart `scale` (see [`ig_scale`]).
pub fn chart_item(epic: &str, scale: &str) -> String {
    format!("CHART:{epic}:{scale}")
}

/// Quote-lane schema, in wire order. `LS_schema` is this joined with spaces.
pub const QUOTE_SCHEMA: &[&str] = &["BID", "OFFER", "UPDATE_TIME", "MARKET_STATE"];
const Q_BID: usize = 0;
const Q_OFFER: usize = 1;
const Q_STATE: usize = 3;

/// Candle-lane schema, in wire order. `LS_schema` is this joined with spaces.
pub const CANDLE_SCHEMA: &[&str] = &[
    "UTM",
    "BID_OPEN",
    "BID_HIGH",
    "BID_LOW",
    "BID_CLOSE",
    "OFR_OPEN",
    "OFR_HIGH",
    "OFR_LOW",
    "OFR_CLOSE",
    "LTV",
    "CONS_END",
];
const C_UTM: usize = 0;
/// First of the four contiguous BID OHLC slots; the four OFR slots follow at
/// `C_BID_OPEN + OHLC_WIDTH` (the layout `CandleFold::bar`'s mid fold walks).
const C_BID_OPEN: usize = 1;
/// Width of one OHLC run (open/high/low/close) — the stride between the bid and offer blocks.
const OHLC_WIDTH: usize = 4;
const C_LTV: usize = 9;
const C_CONS_END: usize = 10;

/// Map a canonical interval to an IG chart scale — the STREAMING set, deliberately narrower than
/// the REST [`crate::data::resolution`] ladder: IG's Lightstreamer serves exactly these four.
pub fn ig_scale(interval: &str) -> Option<&'static str> {
    Some(match interval {
        "1s" => "SECOND",
        "1m" => "1MINUTE",
        "5m" => "5MINUTE",
        "1h" => "HOUR",
        _ => return None,
    })
}

/// One decoded L1 quote off the `MARKET:{epic}` fold.
#[derive(Debug, Clone, PartialEq)]
pub struct IgQuote {
    pub bid: f64,
    pub offer: f64,
    /// `MARKET_STATE` as last delivered (e.g. `TRADEABLE`, `CLOSED`, `EDITS_ONLY`).
    pub market_state: Option<String>,
}

/// MERGE fold for one `MARKET:{epic}` item → an [`IgQuote`] per update that leaves BOTH sides
/// present. A one-sided/nulled book (market closed, auction) yields `None` — no side is ever
/// fabricated.
#[derive(Debug, Clone)]
pub struct QuoteFold {
    state: MergeItemState,
}

impl Default for QuoteFold {
    fn default() -> Self {
        Self::new()
    }
}

impl QuoteFold {
    pub fn new() -> Self {
        QuoteFold { state: MergeItemState::new(QUOTE_SCHEMA.len()) }
    }

    pub fn on_update(&mut self, u: &LsUpdate) -> Option<IgQuote> {
        self.state.apply(&u.fields);
        let (bid, offer) = (self.state.get_f64(Q_BID)?, self.state.get_f64(Q_OFFER)?);
        Some(IgQuote { bid, offer, market_state: self.state.get(Q_STATE).map(str::to_string) })
    }
}

/// A candle-lane emission: the still-forming bar (conflating lane) or a final close (lossless
/// lane). One update can yield BOTH a close (of the previous candle, on rollover) and the first
/// forming frame of the next.
#[derive(Debug, Clone, PartialEq)]
pub enum CandleEvent {
    Forming(Bar),
    Closed(Bar),
}

/// MERGE fold for one `CHART:{epic}:{scale}` item → [`CandleEvent`]s. Close detection is
/// dual-sourced: `CONS_END=1` closes the current candle (exactly once — later same-candle frames
/// are suppressed), and a `UTM` advance closes a previous candle that never saw its `CONS_END`.
#[derive(Debug, Clone)]
pub struct CandleFold {
    state: MergeItemState,
    /// Open-time of the candle currently being folded.
    current_utm: Option<i64>,
    /// The last forming bar (the rollover-close candidate); `None` once its close was emitted.
    last_forming: Option<Bar>,
    /// Whether the CURRENT candle's close has already been emitted (via `CONS_END`).
    closed_emitted: bool,
}

impl Default for CandleFold {
    fn default() -> Self {
        Self::new()
    }
}

impl CandleFold {
    pub fn new() -> Self {
        CandleFold {
            state: MergeItemState::new(CANDLE_SCHEMA.len()),
            current_utm: None,
            last_forming: None,
            closed_emitted: false,
        }
    }

    pub fn on_update(&mut self, u: &LsUpdate) -> Vec<CandleEvent> {
        self.state.apply(&u.fields);
        let mut out = Vec::new();
        let Some(utm) = self.state.get_i64(C_UTM) else {
            return out;
        };
        if self.current_utm.is_some_and(|prev| prev != utm) {
            // New candle began. Close the previous one if CONS_END never arrived for it.
            if let Some(prev_bar) = self.last_forming.take() {
                out.push(CandleEvent::Closed(prev_bar));
            }
            self.closed_emitted = false;
        }
        self.current_utm = Some(utm);
        let Some(bar) = self.bar(utm) else {
            return out;
        };
        if self.closed_emitted {
            return out; // post-close frames of an already-closed candle carry nothing new
        }
        if self.state.get(C_CONS_END) == Some("1") {
            self.closed_emitted = true;
            self.last_forming = None;
            out.push(CandleEvent::Closed(bar));
        } else {
            self.last_forming = Some(bar.clone());
            out.push(CandleEvent::Forming(bar));
        }
        out
    }

    /// The folded state as a MID bar (REST-history convention: each point is `(bid + offer) / 2`).
    /// `None` until every bid/offer OHLC component has been delivered.
    fn bar(&self, utm: i64) -> Option<Bar> {
        let mut mid = [0.0f64; OHLC_WIDTH]; // open, high, low, close
        for (i, m) in mid.iter_mut().enumerate() {
            let bid = self.state.get_f64(C_BID_OPEN + i)?;
            let ofr = self.state.get_f64(C_BID_OPEN + OHLC_WIDTH + i)?;
            *m = (bid + ofr) / 2.0;
        }
        Some(Bar {
            ts: utm,
            open: mid[0],
            high: mid[1],
            low: mid[2],
            close: mid[3],
            volume: self.state.get_f64(C_LTV).unwrap_or(0.0),
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lightstreamer::{parse_frame, LsFrame};

    fn u(line: &str) -> LsUpdate {
        match parse_frame(line) {
            Some(LsFrame::Update(u)) => u,
            other => panic!("expected Update for {line:?}, got {other:?}"),
        }
    }

    #[test]
    fn quote_fold_emits_only_with_both_sides_and_tracks_deltas() {
        let mut q = QuoteFold::new();
        // Snapshot with only a bid (offer null) — no quote fabricated.
        assert_eq!(q.on_update(&u("U,1,1,1.0855|#|20:10:01|EDITS_ONLY")), None);
        // Offer arrives; bid unchanged (empty segment) — a full quote now.
        let quote = q.on_update(&u("U,1,1,|1.0857|20:10:02|TRADEABLE")).unwrap();
        assert_eq!((quote.bid, quote.offer), (1.0855, 1.0857));
        assert_eq!(quote.market_state.as_deref(), Some("TRADEABLE"));
        // A later delta moving only the bid keeps the folded offer.
        let quote = q.on_update(&u("U,1,1,1.0856|^3")).unwrap();
        assert_eq!((quote.bid, quote.offer), (1.0856, 1.0857));
        // The venue nulling a side (market close) stops emissions again.
        assert_eq!(q.on_update(&u("U,1,1,#||22:00:00|CLOSED")), None);
    }

    #[test]
    fn candle_fold_forms_then_closes_on_cons_end_exactly_once() {
        let mut c = CandleFold::new();
        // Snapshot: forming candle, all components present, CONS_END=0.
        let evs =
            c.on_update(&u("U,2,1,1700000000000|1.10|1.12|1.09|1.11|1.11|1.13|1.10|1.12|42|0"));
        assert_eq!(evs.len(), 1);
        let CandleEvent::Forming(b) = &evs[0] else { panic!("expected Forming, got {evs:?}") };
        assert_eq!(b.ts, 1_700_000_000_000);
        assert!((b.open - 1.105).abs() < 1e-12, "mid open");
        assert!((b.close - 1.115).abs() < 1e-12, "mid close");
        assert_eq!(b.volume, 42.0);

        // Intrabar delta: only BID_CLOSE/OFR_CLOSE move.
        let evs = c.on_update(&u("U,2,1,|^3|1.12||||1.13|43|"));
        assert!(matches!(&evs[0], CandleEvent::Forming(b) if (b.close - 1.125).abs() < 1e-12));

        // CONS_END=1 → the close, exactly once; a repeat frame emits nothing.
        let evs = c.on_update(&u("U,2,1,^10|1"));
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], CandleEvent::Closed(b) if (b.close - 1.125).abs() < 1e-12));
        assert!(c.on_update(&u("U,2,1,^10|1")).is_empty(), "no double close");

        // Next candle: new UTM, CONS_END back to 0 → forming (previous already closed).
        let evs =
            c.on_update(&u("U,2,1,1700000060000|1.11|1.11|1.11|1.11|1.13|1.13|1.13|1.13|1|0"));
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], CandleEvent::Forming(b) if b.ts == 1_700_000_060_000));
    }

    #[test]
    fn candle_fold_rollover_closes_a_candle_that_lost_its_cons_end() {
        let mut c = CandleFold::new();
        c.on_update(&u("U,2,1,1700000000000|1.10|1.12|1.09|1.11|1.11|1.13|1.10|1.12|5|0"));
        // The CONS_END frame was dropped; the next candle's first frame arrives directly.
        let evs = c.on_update(&u("U,2,1,1700000060000|1.11|1.11|1.11|1.11|1.13|1.13|1.13|1.13|1|"));
        assert_eq!(evs.len(), 2, "close of the lost candle, then the new forming bar: {evs:?}");
        assert!(matches!(&evs[0], CandleEvent::Closed(b) if b.ts == 1_700_000_000_000));
        assert!(matches!(&evs[1], CandleEvent::Forming(b) if b.ts == 1_700_000_060_000));
    }

    #[test]
    fn scale_map_is_the_streaming_subset() {
        assert_eq!(ig_scale("1m"), Some("1MINUTE"));
        assert_eq!(ig_scale("5m"), Some("5MINUTE"));
        assert_eq!(ig_scale("1h"), Some("HOUR"));
        assert_eq!(ig_scale("1s"), Some("SECOND"));
        for unsupported in ["15m", "4h", "1d", ""] {
            assert_eq!(ig_scale(unsupported), None, "{unsupported} is REST-only");
        }
        assert_eq!(market_item("CS.D.EURUSD.MINI.IP"), "MARKET:CS.D.EURUSD.MINI.IP");
        assert_eq!(
            chart_item("CS.D.EURUSD.MINI.IP", "1MINUTE"),
            "CHART:CS.D.EURUSD.MINI.IP:1MINUTE"
        );
    }
}
