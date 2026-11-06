//! IG Lightstreamer market-data normalizers — the PURE decode layer between the TLCP codec
//! ([`crate::lightstreamer`]) and the live feed driver ([`crate::market_feed`]), the IG analogue
//! of the crypto venues' `market_data` tick normalizers.
//!
//! Two lanes, both MERGE-mode (one item per subscription, `LS_snapshot=true`):
//!
//! - **Quotes** — `PRICE:{accountId}:{epic}` on the `Pricing` data adapter ([`QUOTE_ADAPTER`]) over
//!   [`QUOTE_SCHEMA`]: the top of IG's ladder, best bid/ask (L1 only; the lower tiers are not
//!   asked for), plus the venue's dealing flag (`DLG_FLAG`) so a CLOSED market stops emissions rather
//!   than fabricating a stale quote. ⚠ This replaced `market_item` (`MARKET:{epic}`), which IG
//!   decommissioned (every ask is `REQERR,1,21,Invalid group` since 2026-05; measured on the demo
//!   gateway 2026-10-19; decision 0045 still cites the retired `market_item` by name, so the token
//!   stays in this file's docs). The item is NAMED AFTER THE ACCOUNT, and the data adapter is not optional: the
//!   `DEFAULT` adapter ACKs the same item and then streams NOTHING, and `PRICE` does not validate
//!   field names (a misspelt one arrives as a silent null), so a SUBOK proves neither the adapter
//!   nor the fields; only a populated value does. `TIMESTAMP` is UTC epoch ms but the feed still
//!   stamps receive time, as it did when `MARKET`'s `UPDATE_TIME` was a bare HH:MM:SS.
//! - **Candles** — `CHART:{epic}:{scale}` over [`CANDLE_SCHEMA`]: bid/offer OHLC folded to MID
//!   bars, mirroring the REST history convention (`crate::data::parse_prices` — one venue, one
//!   bar law). `UTM` is the candle's open epoch-ms; `CONS_END=1` marks consolidation end (the
//!   lossless close), and a `UTM` advance without one still closes the previous bar (rollover
//!   close, the hyperliquid shape) so a dropped `CONS_END` frame cannot silently lose a close.
//!
//! Both folds tolerate TLCP's delta encoding by construction: they fold into a
//! [`MergeItemState`] and read the FOLDED values, never the raw update.

use vike_marketdata::Bar;

use crate::lightstreamer::{LsUpdate, MergeItemState};

/// `LS_group` for the L1 quote lane: the item is named after the ACCOUNT the session is logged in as
/// (`IgSession::account_id`, also the `LS_user`), then the epic.
pub fn price_item(account_id: &str, epic: &str) -> String {
    format!("PRICE:{account_id}:{epic}")
}

/// The data adapter that serves [`price_item`]. ⚠ Not `DEFAULT`: that adapter ACKs the item and streams nothing.
pub const QUOTE_ADAPTER: &str = "Pricing";

/// `LS_group` for the candle lane at an IG chart `scale` (see [`ig_scale`]).
pub fn chart_item(epic: &str, scale: &str) -> String {
    format!("CHART:{epic}:{scale}")
}

/// The data adapter that serves [`chart_item`] (`CHART` is not deprecated and stays where it always was).
pub const CANDLE_ADAPTER: &str = "DEFAULT";

/// Quote-lane schema, in wire order. `LS_schema` is this joined with spaces.
pub const QUOTE_SCHEMA: &[&str] = &["BIDPRICE1", "ASKPRICE1", "TIMESTAMP", "DLG_FLAG"];
const Q_BID: usize = 0;
const Q_OFFER: usize = 1;
const Q_STATE: usize = 3;
/// The `DLG_FLAG` words of a market nothing can be dealt in NOW, whose price is stale or indicative: IG's
/// streaming reference lists nine words and defines none, so the readings are those of the REST
/// `marketStatus` table whose names they echo (`CLOSED` closed, `SUSPEND` suspended, `EDIT` "open for edits"
/// only, `AUCTION` "in auction mode", `AUCTIONNOEDIT` "in no-edits mode"; `CALL` has no REST twin and means
/// dealing by phone). A quote is emitted only where a deal can be done, opening or closing: `DEAL`,
/// `DEALNOEDIT` ("deals but not edits") and `CLOSINGSONLY` ("closings only allowed": the price a held
/// position is marked and exited on). The words are DOCUMENTED, not measured (the probe ran in the FX
/// week: only `DEAL` was seen), and a word IG has not documented does not silence the lane.
const NOT_QUOTABLE: &[&str] = &["CLOSED", "CALL", "SUSPEND", "EDIT", "AUCTION", "AUCTIONNOEDIT"];

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

/// One decoded L1 quote off the `PRICE:{accountId}:{epic}` fold.
#[derive(Debug, Clone, PartialEq)]
pub struct IgQuote {
    pub bid: f64,
    pub offer: f64,
    /// `DLG_FLAG` as last delivered, trimmed (IG's dealing word: `DEAL`, `EDIT`, `CALL`, `AUCTION`, ...).
    /// ⚠ Was `MARKET_STATE` (`TRADEABLE`, `CLOSED`, ...) on the decommissioned `MARKET` item: a
    /// different vocabulary, and nothing reads it.
    pub market_state: Option<String>,
}

/// MERGE fold for one `PRICE:{accountId}:{epic}` item → an [`IgQuote`] per update that leaves BOTH
/// sides present and sane (finite, a positive bid, an ask not below it) and the market in a dealable
/// state ([`NOT_QUOTABLE`]). A one-sided/nulled book yields `None` — no side is ever fabricated. When
/// the flag flips back to a dealable word the first quote can carry the price folded before the close
/// until the next update corrects it. ⚠ `PRICE` sends an update whenever `TIMESTAMP`
/// moves, price or not (about five a second on EURGBP, most of them repeating the last bid/ask),
/// and each yields a quote, as every `MARKET` update did.
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
        // The mounted lane forwards every quote to the core unchanged, so this is the one place a bad
        // price is stopped: finite, a positive bid, an ask that is not below it (`parse` accepts `0`,
        // `-1`, `NaN` and `inf`).
        if !(bid.is_finite() && offer.is_finite() && bid > 0.0 && offer >= bid) {
            return None;
        }
        if !self.dealable() {
            return None;
        }
        let market_state = self.state.get(Q_STATE).map(|s| s.trim().to_string());
        Some(IgQuote { bid, offer, market_state })
    }

    /// Does the `DLG_FLAG` as last folded say a deal can be done now? `true` while no flag has been delivered
    /// and for a word IG has not documented (the fail-open of [`NOT_QUOTABLE`]). The mid-session stall watch
    /// (`crates/bridges/ig/src/market_feed.rs`'s `Silence`) reads it: a lane whose updates stopped is only
    /// DEAD while the flag says the market is open, and a closed market legitimately sends nothing.
    pub fn dealable(&self) -> bool {
        !self.state.get(Q_STATE).map(str::trim).is_some_and(|s| NOT_QUOTABLE.contains(&s))
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
    use crate::lightstreamer::{LsFrame, parse_frame};
    use std::assert_matches;

    fn u(line: &str) -> LsUpdate {
        match parse_frame(line) {
            Some(LsFrame::Update(u)) => u,
            other => panic!("expected Update for {line:?}, got {other:?}"),
        }
    }

    #[test]
    fn quote_fold_emits_only_with_both_sides_and_tracks_deltas() {
        let mut q = QuoteFold::new();
        // Snapshot with only a bid (ask null) — no quote fabricated.
        assert_eq!(q.on_update(&u("U,1,1,1.0855|#|1792433944062|EDIT")), None);
        // Ask arrives; bid unchanged (empty segment) — a full quote now. IG pads DLG_FLAG to 13
        // characters ("DEAL" + 9 spaces); the fold reads it trimmed.
        let quote = q.on_update(&u("U,1,1,|1.0857|1792433945062|DEAL         ")).unwrap();
        assert_eq!((quote.bid, quote.offer), (1.0855, 1.0857));
        assert_eq!(quote.market_state.as_deref(), Some("DEAL"));
        // A later delta moving only the bid keeps the folded offer.
        let quote = q.on_update(&u("U,1,1,1.0856|^3")).unwrap();
        assert_eq!((quote.bid, quote.offer), (1.0856, 1.0857));
        // An update that moves only TIMESTAMP (most of them, measured) still yields the same quote.
        let quote = q.on_update(&u("U,1,1,||1792433946062|")).unwrap();
        assert_eq!((quote.bid, quote.offer), (1.0856, 1.0857));
        // A CLOSED market stops emissions with both sides still folded, and re-opening resumes.
        assert_eq!(q.on_update(&u("U,1,1,||1792433947062|CLOSED       ")), None);
        assert!(q.on_update(&u("U,1,1,||1792433948062|DEAL")).is_some());
        // The venue nulling a side stops emissions again.
        assert_eq!(q.on_update(&u("U,1,1,#||")), None);
    }

    /// IG's streaming reference lists nine `DLG_FLAG` words and defines none; the nearest definitions are the
    /// REST `marketStatus` table's, whose names they echo (`DEAL` is TRADEABLE "open for trades", `DEALNOEDIT`
    /// "deals but not edits allowed", `CLOSINGSONLY` "closings only allowed", `EDIT` "open for edits",
    /// `AUCTION` "in auction mode", `AUCTIONNOEDIT` "in no-edits mode", `SUSPEND` suspended, `CLOSED` closed;
    /// `CALL` has no REST twin and means dealing by phone). A quote is usable only where a deal can be done
    /// NOW, opening or closing: `DEAL`, `DEALNOEDIT` and `CLOSINGSONLY` (a closing price is the one a held
    /// position is marked and exited on). Every other word is a market that cannot be dealt in, whose price is
    /// stale or indicative, and emits nothing. The padding is IG's (13 characters).
    #[test]
    fn only_a_flag_that_can_deal_yields_a_quote() {
        for (flag, quotes) in [
            ("DEAL", true),
            ("DEALNOEDIT", true),
            ("CLOSINGSONLY", true),
            ("CLOSED", false),
            ("CALL", false),
            ("SUSPEND", false),
            ("EDIT", false),
            ("AUCTION", false),
            ("AUCTIONNOEDIT", false),
        ] {
            let mut q = QuoteFold::new();
            let quote = q.on_update(&u(&format!("U,1,1,1.0855|1.0857|1792433944062|{flag:<13}")));
            assert_eq!(quote.is_some(), quotes, "{flag}");
            // the stall watch reads the same verdict off the fold, without a price in it
            assert_eq!(q.dealable(), quotes, "{flag}: dealable() is the flag's verdict");
            if let Some(quote) = quote {
                assert_eq!(
                    quote.market_state.as_deref(),
                    Some(flag),
                    "the flag rides along, trimmed"
                );
            }
        }
        // no flag at all (null, or not yet delivered) is not a verdict: both sides present still quote
        let mut q = QuoteFold::new();
        assert!(q.dealable(), "nothing delivered yet is not a closed market");
        assert!(q.on_update(&u("U,1,1,1.0855|1.0857|1792433944062|#")).is_some());
        assert!(q.dealable());
        // a word IG has not documented is not a reason to go silent either
        assert!(q.on_update(&u("U,1,1,||1792433945062|NEWWORD")).is_some());
        assert!(q.dealable());
    }

    /// The mounted path forwards every quote the lane emits to the core unchanged, so the fold is the one
    /// place a bad price is stopped: finite, a positive bid, and an ask that is not below it (a locked
    /// quote, bid == ask, is a legal book).
    #[test]
    fn a_price_that_is_not_a_price_yields_no_quote() {
        for (bid, ask) in [
            ("0", "1.0857"),
            ("-1.0855", "1.0857"),
            ("NaN", "1.0857"),
            ("inf", "inf"),
            ("1.0855", "NaN"),
            ("1.0855", "0"),
            ("1.0860", "1.0857"),
        ] {
            let mut q = QuoteFold::new();
            let line = format!("U,1,1,{bid}|{ask}|1792433944062|DEAL");
            assert_eq!(q.on_update(&u(&line)), None, "{bid} / {ask}");
        }
        let mut q = QuoteFold::new();
        let locked =
            q.on_update(&u("U,1,1,1.0857|1.0857|1792433944062|DEAL")).expect("locked quotes");
        assert_eq!((locked.bid, locked.offer), (1.0857, 1.0857));
        // and a bad price folded in does not poison the next good one
        let mut q = QuoteFold::new();
        assert_eq!(q.on_update(&u("U,1,1,0|1.0857|1|DEAL")), None);
        assert!(q.on_update(&u("U,1,1,1.0855|^3")).is_some());
    }

    /// The quote lane asks for exactly what the demo gateway served on 2026-10-19: the account-named
    /// `PRICE` item from the `Pricing` adapter, never the decommissioned `MARKET` item (`REQERR 21`)
    /// and never `DEFAULT` (which ACKs `PRICE` and then streams nothing).
    #[test]
    fn quote_lane_item_adapter_and_fields_are_the_measured_ones() {
        assert_eq!(price_item("ACC1", "CS.D.EURGBP.MINI.IP"), "PRICE:ACC1:CS.D.EURGBP.MINI.IP");
        assert_eq!((QUOTE_ADAPTER, CANDLE_ADAPTER), ("Pricing", "DEFAULT"));
        assert_eq!(QUOTE_SCHEMA.join(" "), "BIDPRICE1 ASKPRICE1 TIMESTAMP DLG_FLAG");
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
        assert_matches!(&evs[0], CandleEvent::Forming(b) if (b.close - 1.125).abs() < 1e-12);

        // CONS_END=1 → the close, exactly once; a repeat frame emits nothing.
        let evs = c.on_update(&u("U,2,1,^10|1"));
        assert_eq!(evs.len(), 1);
        assert_matches!(&evs[0], CandleEvent::Closed(b) if (b.close - 1.125).abs() < 1e-12);
        assert!(c.on_update(&u("U,2,1,^10|1")).is_empty(), "no double close");

        // Next candle: new UTM, CONS_END back to 0 → forming (previous already closed).
        let evs =
            c.on_update(&u("U,2,1,1700000060000|1.11|1.11|1.11|1.11|1.13|1.13|1.13|1.13|1|0"));
        assert_eq!(evs.len(), 1);
        assert_matches!(&evs[0], CandleEvent::Forming(b) if b.ts == 1_700_000_060_000);
    }

    #[test]
    fn candle_fold_rollover_closes_a_candle_that_lost_its_cons_end() {
        let mut c = CandleFold::new();
        c.on_update(&u("U,2,1,1700000000000|1.10|1.12|1.09|1.11|1.11|1.13|1.10|1.12|5|0"));
        // The CONS_END frame was dropped; the next candle's first frame arrives directly.
        let evs = c.on_update(&u("U,2,1,1700000060000|1.11|1.11|1.11|1.11|1.13|1.13|1.13|1.13|1|"));
        assert_eq!(evs.len(), 2, "close of the lost candle, then the new forming bar: {evs:?}");
        assert_matches!(&evs[0], CandleEvent::Closed(b) if b.ts == 1_700_000_000_000);
        assert_matches!(&evs[1], CandleEvent::Forming(b) if b.ts == 1_700_000_060_000);
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
        assert_eq!(
            chart_item("CS.D.EURUSD.MINI.IP", "1MINUTE"),
            "CHART:CS.D.EURUSD.MINI.IP:1MINUTE"
        );
    }
}
