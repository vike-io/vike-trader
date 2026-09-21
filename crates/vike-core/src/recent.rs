//! The bounded recent-events ring's ELEMENT type — a compact, un-rendered note.
//!
//! ## Why this exists (perf audit 2026-07-28, finding #2)
//!
//! Every event the `EventBus` delivers is folded into a `recent_events_cap`-bounded ring, and that
//! drain (`CoreThread::drain_delivered`) runs on the **per-message path** — `handle()` calls it
//! after every single `Ingest`, inside the `p99 core-hop < 10 µs` gate. It used to call a
//! `describe_event(&Event) -> String` that ran `format!` per delivered event: the full
//! `core::fmt` machinery plus one heap allocation on the fold thread (and the matching `free` when
//! the entry later rolled off the ring).
//!
//! The ring is only ever READ at publish, which is coalesced to `snapshot_interval` (≥16 ms) — so
//! the rendering is pure waste on the fold thread. This module stores the few fields each line
//! needs and renders LAZILY, in `CoreSnapshot::build`.
//!
//! Allocation on the fold path is now zero for the common lanes:
//! - `client_order_id` goes into a [`CompactString`], which is INLINE up to 24 bytes — a live coid
//!   is `<8-hex session><seq>` (well under that), so the copy never touches the heap.
//! - `venue`/`symbol` are already interned [`Ustr`] on the event, so they are stored by 8-byte
//!   `Copy`. (Coids are deliberately NOT interned — unbounded per-order cardinality would leak the
//!   `ustr` arena; see `vike_model::events`' interning contract.)
//! - `reason` is the one field that can exceed the inline budget; rejection/denial lines are rare
//!   next to fills and acks, and it costs the same single allocation `format!` already paid.
//!
//! ## The rendering contract
//!
//! [`RecentNote::render`] must produce the **byte-identical** string the old `format!` produced —
//! the GUI, the `vike-cli trade` event pane and several live smokes match on these lines. That is
//! pinned by `tests::render_matches_the_reference_format`, which keeps the pre-change
//! `describe_event` body verbatim as a reference implementation and asserts equality over one
//! instance of every [`vike_model::events::Event`] variant.

use compact_str::CompactString;
use ustr::Ustr;
use vike_model::events::Event;

/// The un-rendered form of one delivered [`Event`]. One variant per rendering SHAPE (not per event
/// kind) — the `kind` label rides as a `&'static str` so the rendered prefix cannot drift from the
/// variant name it came from.
#[derive(Debug, Clone, PartialEq)]
pub enum EventNote {
    /// `FillEvent {coid} {qty}@{px}`
    Fill { coid: CompactString, qty: f64, px: f64 },
    /// `{kind} {coid}` — the bare order-lifecycle lines.
    Coid { kind: &'static str, coid: CompactString },
    /// `{kind} {coid} ({reason})` — reject / deny and the two Rust-native reject twins. NOT
    /// `OrderCanceled`: it carries a `reason` but has never rendered one (see `capture`).
    CoidReason { kind: &'static str, coid: CompactString, reason: CompactString },
    /// `OrderModified {coid} qty={new_qty:?} px={new_price:?}` — note the `{:?}` on the `Option`s.
    Modified { coid: CompactString, new_qty: Option<f64>, new_price: Option<f64> },
    /// `{kind} {label}` — the position/account lane, labelled by symbol (or venue).
    Label { kind: &'static str, label: Ustr },
    /// `{kind} {label} {num}` — funding amount / liquidated qty.
    LabelNum { kind: &'static str, label: Ustr, num: f64 },
}

impl EventNote {
    /// Capture the fields this event's line needs. No formatting, no `String`; the only heap
    /// traffic possible is an over-long `reason`.
    pub fn capture(ev: &Event) -> EventNote {
        match ev {
            Event::Fill(e) => EventNote::Fill {
                coid: CompactString::new(&e.client_order_id),
                qty: e.last_qty,
                px: e.last_px,
            },
            Event::OrderSubmitted(e) => EventNote::Coid {
                kind: "OrderSubmitted",
                coid: CompactString::new(&e.client_order_id),
            },
            Event::OrderAccepted(e) => EventNote::Coid {
                kind: "OrderAccepted",
                coid: CompactString::new(&e.client_order_id),
            },
            Event::OrderRejected(e) => EventNote::CoidReason {
                kind: "OrderRejected",
                coid: CompactString::new(&e.client_order_id),
                reason: e.reason.clone(),
            },
            Event::OrderDenied(e) => EventNote::CoidReason {
                kind: "OrderDenied",
                coid: CompactString::new(&e.client_order_id),
                reason: e.reason.clone(),
            },
            Event::OrderTriggered(e) => EventNote::Coid {
                kind: "OrderTriggered",
                coid: CompactString::new(&e.client_order_id),
            },
            Event::OrderPartiallyFilled(e) => EventNote::Coid {
                kind: "OrderPartiallyFilled",
                coid: CompactString::new(&e.client_order_id),
            },
            Event::OrderFilled(e) => EventNote::Coid {
                kind: "OrderFilled",
                coid: CompactString::new(&e.client_order_id),
            },
            // NOTE the asymmetry, which is pre-existing and deliberately preserved: `OrderCanceled`
            // CARRIES a `reason` but the ring line has never shown it, unlike the reject/deny
            // lines. Rendering it here would change what the GUI and the live smokes read, so this
            // is `Coid`, not `CoidReason`. (First cut of this module got that wrong;
            // `tests::render_matches_the_reference_format` caught it.)
            Event::OrderCanceled(e) => EventNote::Coid {
                kind: "OrderCanceled",
                coid: CompactString::new(&e.client_order_id),
            },
            Event::OrderExpired(e) => EventNote::Coid {
                kind: "OrderExpired",
                coid: CompactString::new(&e.client_order_id),
            },
            Event::OrderLiquidated(e) => EventNote::Coid {
                kind: "OrderLiquidated",
                coid: CompactString::new(&e.client_order_id),
            },
            Event::OrderModified(e) => EventNote::Modified {
                coid: CompactString::new(&e.client_order_id),
                new_qty: e.new_qty,
                new_price: e.new_price,
            },
            Event::OrderCancelRejected(e) => EventNote::CoidReason {
                kind: "OrderCancelRejected",
                coid: CompactString::new(&e.client_order_id),
                reason: e.reason.clone(),
            },
            Event::OrderModifyRejected(e) => EventNote::CoidReason {
                kind: "OrderModifyRejected",
                coid: CompactString::new(&e.client_order_id),
                reason: e.reason.clone(),
            },
            Event::PositionOpened(e) => {
                EventNote::Label { kind: "PositionOpened", label: e.symbol }
            }
            Event::PositionChanged(e) => {
                EventNote::Label { kind: "PositionChanged", label: e.symbol }
            }
            Event::PositionClosed(e) => {
                EventNote::Label { kind: "PositionClosed", label: e.symbol }
            }
            Event::AccountState(e) => EventNote::Label { kind: "AccountState", label: e.venue },
            Event::Funding(e) => {
                EventNote::LabelNum { kind: "FundingEvent", label: e.symbol, num: e.amount }
            }
            Event::PositionLiquidated(e) => {
                EventNote::LabelNum { kind: "PositionLiquidated", label: e.symbol, num: e.qty }
            }
        }
    }
}

/// The rendering contract, in one place. Byte-identical to the `format!` the fold thread used to
/// run — see the module doc, and `tests::render_matches_the_reference_format` for the pin.
impl std::fmt::Display for EventNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventNote::Fill { coid, qty, px } => write!(f, "FillEvent {coid} {qty}@{px}"),
            EventNote::Coid { kind, coid } => write!(f, "{kind} {coid}"),
            EventNote::CoidReason { kind, coid, reason } => write!(f, "{kind} {coid} ({reason})"),
            EventNote::Modified { coid, new_qty, new_price } => {
                write!(f, "OrderModified {coid} qty={new_qty:?} px={new_price:?}")
            }
            EventNote::Label { kind, label } => write!(f, "{kind} {label}"),
            EventNote::LabelNum { kind, label, num } => write!(f, "{kind} {label} {num}"),
        }
    }
}

/// One entry of the bounded recent-events ring.
#[derive(Debug, Clone, PartialEq)]
pub enum RecentNote {
    /// A runtime-authored line (drift / margin / recon / refusal notes). Already a full sentence,
    /// and every one of these is minted on a COLD path, so it stays an owned `String`.
    Text(std::sync::Arc<str>),
    /// A delivered event, captured un-rendered off the fold thread.
    Event(EventNote),
    /// An event salvaged from a mid-fold handler panic (audit C4). Renders with the
    /// `LOST(panic mid-fold) ` prefix the nested `format!` produced.
    Lost(EventNote),
}

impl std::fmt::Display for RecentNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecentNote::Text(s) => f.write_str(s),
            RecentNote::Event(n) => write!(f, "{n}"),
            RecentNote::Lost(n) => write!(f, "LOST(panic mid-fold) {n}"),
        }
    }
}

impl RecentNote {
    /// The line this note renders to. Called at COALESCED PUBLISH cadence only (>=16 ms), never on
    /// the per-message fold.
    pub fn render(&self) -> String {
        self.to_string()
    }

    /// Render ONCE, collapsing the note into its rendered form so later publishes cannot re-render
    /// it. **This is the method the publish path must use.** [`Self::render`] is cheap per CALL,
    /// but publish renders the WHOLE ring, so a note surviving K publishes was formatted K times —
    /// turning "format once per event" into "format once per entry per publish".
    ///
    /// Measured, not theorised: with per-call rendering the fold thread spent ~13x more time in
    /// float formatting than the pre-change build (perf on the latency box: `RecentNote::render` ->
    /// `EventNote::fmt` -> `float_to_decimal` -> `grisu`), and the p99 core-hop went
    /// 541 ns -> 14,498 ns. The `format!`-per-event this module set out to remove was CHEAPER than
    /// re-formatting a 64-entry ring on every publish.
    ///
    /// Collapsing preserves the module's real win: a note that rolls off the ring before any
    /// publish is never rendered AT ALL — something the old render-at-push design could not do.
    /// Returns a SHARED handle, not a copy: publish clones the `Arc` (a refcount bump), never the
    /// bytes. That matters far more than the 16ms snapshot cadence suggests — the runtime ALSO
    /// publishes whenever the core is about to go idle (`if self.dirty`, immediately before
    /// `blocking_recv`), so on a venue whose events arrive sporadically it publishes PER EVENT.
    /// Measured: 100k events x a 64-entry ring was ~6.4M `String` allocations per run.
    pub fn render_once(&mut self) -> std::sync::Arc<str> {
        if !matches!(self, RecentNote::Text(_)) {
            let line: std::sync::Arc<str> = self.to_string().into();
            *self = RecentNote::Text(line);
        }
        match self {
            RecentNote::Text(s) => std::sync::Arc::clone(s),
            // unreachable: the branch above collapsed every other variant to `Text`.
            _ => unreachable!("render_once just collapsed this note to Text"),
        }
    }

    /// Does the rendered line contain `pat`? DIAGNOSTIC/TEST convenience — it RENDERS first, so it
    /// belongs nowhere near the fold path.
    pub fn contains(&self, pat: &str) -> bool {
        self.render().contains(pat)
    }

    /// Does the rendered line start with `pat`? Same diagnostic-only caveat as [`Self::contains`].
    pub fn starts_with(&self, pat: &str) -> bool {
        self.render().starts_with(pat)
    }
}

impl From<String> for RecentNote {
    fn from(s: String) -> Self {
        RecentNote::Text(s.into())
    }
}

impl From<&str> for RecentNote {
    fn from(s: &str) -> Self {
        RecentNote::Text(s.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;
    use vike_model::events::*;

    /// The pre-change `describe_event` body, VERBATIM. This is the oracle: it is what the GUI /
    /// `vike-cli trade` event pane / live smokes were reading before the ring went lazy, so the
    /// lazy renderer is only correct if it reproduces this character for character.
    fn describe_event_reference(ev: &Event) -> String {
        match ev {
            Event::Fill(e) => {
                format!("FillEvent {} {}@{}", e.client_order_id, e.last_qty, e.last_px)
            }
            Event::OrderSubmitted(e) => format!("OrderSubmitted {}", e.client_order_id),
            Event::OrderAccepted(e) => format!("OrderAccepted {}", e.client_order_id),
            Event::OrderRejected(e) => {
                format!("OrderRejected {} ({})", e.client_order_id, e.reason)
            }
            Event::OrderDenied(e) => format!("OrderDenied {} ({})", e.client_order_id, e.reason),
            Event::OrderTriggered(e) => format!("OrderTriggered {}", e.client_order_id),
            Event::OrderPartiallyFilled(e) => {
                format!("OrderPartiallyFilled {}", e.client_order_id)
            }
            Event::OrderFilled(e) => format!("OrderFilled {}", e.client_order_id),
            Event::OrderCanceled(e) => format!("OrderCanceled {}", e.client_order_id),
            Event::OrderExpired(e) => format!("OrderExpired {}", e.client_order_id),
            Event::OrderLiquidated(e) => format!("OrderLiquidated {}", e.client_order_id),
            Event::OrderModified(e) => {
                format!(
                    "OrderModified {} qty={:?} px={:?}",
                    e.client_order_id, e.new_qty, e.new_price
                )
            }
            Event::OrderCancelRejected(e) => {
                format!("OrderCancelRejected {} ({})", e.client_order_id, e.reason)
            }
            Event::OrderModifyRejected(e) => {
                format!("OrderModifyRejected {} ({})", e.client_order_id, e.reason)
            }
            Event::PositionOpened(e) => format!("PositionOpened {}", e.symbol),
            Event::PositionChanged(e) => format!("PositionChanged {}", e.symbol),
            Event::PositionClosed(e) => format!("PositionClosed {}", e.symbol),
            Event::AccountState(e) => format!("AccountState {}", e.venue),
            Event::Funding(e) => format!("FundingEvent {} {}", e.symbol, e.amount),
            Event::PositionLiquidated(e) => format!("PositionLiquidated {} {}", e.symbol, e.qty),
        }
    }

    fn fill() -> FillEvent {
        FillEvent {
            trade_id: "t1".into(), // a source literal — `TradeId: From<&'static str>`
            client_order_id: "deadbeef12".to_string(),
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.5,
            last_px: 100.25,
            commission: 0.0,
            commission_asset: "".into(),
            liquidity_side: LiquiditySide::Maker,
            ts: 7,
            mark_price: None,
            position_side: PositionSide::Both,
        }
    }

    /// One instance of EVERY `Event` variant. A new variant makes the exhaustive `match` in
    /// `EventNote::capture` fail to compile, and this list is where its line gets pinned.
    fn every_variant() -> Vec<Event> {
        let coid = || "deadbeef12".to_string();
        vec![
            Event::Fill(fill()),
            Event::OrderSubmitted(OrderSubmitted { client_order_id: coid(), ts: 1 }),
            Event::OrderAccepted(OrderAccepted {
                client_order_id: coid(),
                venue_order_id: Some(CompactString::new("v1")),
                ts: 1,
            }),
            Event::OrderRejected(OrderRejected {
                client_order_id: coid(),
                reason: CompactString::new("insufficient balance"),
                ts: 1,
            }),
            Event::OrderDenied(OrderDenied {
                client_order_id: coid(),
                reason: CompactString::new("RiskGate: max_order_qty"),
                ts: 1,
            }),
            Event::OrderTriggered(OrderTriggered { client_order_id: coid(), ts: 1 }),
            Event::OrderPartiallyFilled(OrderPartiallyFilled {
                client_order_id: coid(),
                fill: fill(),
                ts: 1,
            }),
            Event::OrderFilled(OrderFilled { client_order_id: coid(), fill: fill(), ts: 1 }),
            // NON-EMPTY reason on purpose: `OrderCanceled` carries one but the line has never
            // shown it, and an empty fixture would let a wrongly-added `({reason})` render as
            // `"OrderCanceled c ()"` -> caught, but a CORRECT-looking empty tail could also hide a
            // missing one. A real reason makes both directions loud.
            Event::OrderCanceled(OrderCanceled {
                client_order_id: coid(),
                reason: CompactString::new("user requested"),
                ts: 1,
            }),
            Event::OrderExpired(OrderExpired { client_order_id: coid(), ts: 1 }),
            Event::OrderLiquidated(OrderLiquidated {
                client_order_id: coid(),
                liq_price: 9.0,
                ts: 1,
            }),
            // both Option arms of the `{:?}` rendering
            Event::OrderModified(OrderModified {
                client_order_id: coid(),
                venue_order_id: None,
                new_qty: Some(2.0),
                new_price: None,
                ts: 1,
            }),
            Event::OrderModified(OrderModified {
                client_order_id: coid(),
                venue_order_id: None,
                new_qty: None,
                new_price: Some(101.5),
                ts: 1,
            }),
            Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: coid(),
                reason: CompactString::new("network error: timed out"),
                ts: 1,
            }),
            Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: coid(),
                reason: CompactString::new("modify rejected"),
                ts: 1,
            }),
            Event::PositionOpened(PositionOpened {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                qty: 1.0,
                avg_px: 100.0,
                ts: 1,
                mark_price: None,
            }),
            Event::PositionChanged(PositionChanged {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                qty: 2.0,
                avg_px: 100.0,
                realized_pnl: 0.0,
                ts: 1,
                mark_price: None,
            }),
            Event::PositionClosed(PositionClosed {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                realized_pnl: 5.0,
                ts: 1,
            }),
            Event::AccountState(AccountState {
                venue: "sim".into(),
                balances: vec![("USDT".to_string(), 10.0)],
                ts: 1,
                route_key: None,
            }),
            Event::Funding(FundingEvent {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                funding_rate: 0.0001,
                amount: -0.25,
                mark_price: None,
                ts: 1,
                route_key: None,
            }),
            Event::PositionLiquidated(PositionLiquidated {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                position_side: PositionSide::Both,
                qty: 3.0,
                liq_price: 50.0,
                fee: 0.1,
                ts: 1,
                trade_id: CompactString::new("l1"),
                route_key: None,
            }),
        ]
    }

    /// THE contract: capture-then-render is byte-identical to the eager `format!` it replaced,
    /// for every event variant (and both `Option` arms of the `OrderModified` line).
    #[test]
    fn render_matches_the_reference_format() {
        for ev in every_variant() {
            let want = describe_event_reference(&ev);
            let got = RecentNote::Event(EventNote::capture(&ev)).render();
            assert_eq!(got, want, "lazy render drifted from the reference for {ev:?}");
        }
    }

    /// The salvaged-event line keeps its prefix, byte-identically to the old nested `format!`.
    #[test]
    fn lost_note_keeps_the_panic_prefix() {
        for ev in every_variant() {
            let want = format!("LOST(panic mid-fold) {}", describe_event_reference(&ev));
            assert_eq!(RecentNote::Lost(EventNote::capture(&ev)).render(), want);
        }
    }

    /// A runtime-authored note is passed through verbatim.
    #[test]
    fn text_notes_round_trip_verbatim() {
        let s = "DRIFT position sim/BTCUSDT[BOTH]: local 1 vs venue 0";
        assert_eq!(RecentNote::from(s).render(), s);
        assert_eq!(RecentNote::from(s.to_string()).render(), s);
    }

    /// The allocation claim the module doc makes: a live-shaped coid fits `CompactString`'s inline
    /// budget, so capturing a fill's line never touches the heap for the id.
    #[test]
    fn a_live_shaped_coid_stays_inline() {
        // `<8-hex session><seq>` — the `ClientOrderIdGenerator` wire form, at an absurd seq.
        let coid = CompactString::new("deadbeef18446744073709551615");
        assert!(coid.len() > 24, "precondition: this one is deliberately over the inline budget");
        assert!(coid.is_heap_allocated(), "…and therefore heap");
        // A realistic one is not.
        let real = CompactString::new("deadbeef1234567");
        assert!(!real.is_heap_allocated(), "a live coid must stay inline (no fold-path malloc)");
    }
}
