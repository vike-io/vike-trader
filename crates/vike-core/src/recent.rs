//! A recent-events line's compact CAPTURE form and its rendering contract. The ring itself holds
//! RENDERED `Arc<str>` lines: each note is rendered on the fold, at push (see below).
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
//! #887 stored the few fields each line needs and rendered LAZILY, in `CoreSnapshot::build`, on
//! the premise that the ring is only ever READ at publish, which is coalesced to
//! `snapshot_interval` (≥16 ms). **That premise is false**: publish also runs on every idle
//! transition, so on a sporadic feed it runs once per event and deferring the render only
//! relocated it. #896 moved it back onto the fold — `crates/vike-core/src/runtime/journaling.rs`'s
//! `note_event` captures, renders at once through [`RecentNote::render`] and pushes the line, and
//! carries the measurement.
//!
//! The CAPTURE allocates nothing for the common lanes (the render that follows it on the fold
//! does allocate the line):
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
    /// held as the same shared `Arc<str>` a rendered event line is.
    Text(std::sync::Arc<str>),
    /// A delivered event, captured ON the fold thread and rendered there at once (`note_event`).
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
    /// The line this note renders to. Called ON the per-message fold: `note_event`/`note_lost`
    /// (`crates/vike-core/src/runtime/journaling.rs`) render every delivered event here at push,
    /// inside the gated core hop — the module doc says why the render is not deferred to publish.
    pub fn render(&self) -> String {
        self.to_string()
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

#[path = "recent_tests.rs"]
#[cfg(test)]
mod recent_tests;
