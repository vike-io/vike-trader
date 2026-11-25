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
//! #887 stored the few fields each line needs and rendered LAZILY, in `snapshot::build`, on
//! the premise that the ring is only ever READ at publish, which is coalesced to
//! `snapshot_interval` (≥16 ms). **That premise is false**: publish also runs on every idle
//! transition, so on a sporadic feed it runs once per event and deferring the render only
//! relocated it. #896 moved it back onto the fold — `crates/vike-core/src/runtime/journaling.rs`'s
//! `note_event` captures, renders at once through [`RecentNote::render`] and pushes the line, and
//! carries the measurement.
//!
//! The CAPTURE allocates nothing for the common lanes (the render that follows it on the fold
//! does allocate the line):
//! - `order_client_id` goes into a [`CompactString`], which is INLINE up to 24 bytes — a live order client id
//!   is `<8-hex session><seq>` (well under that), so the copy never touches the heap.
//! - `venue`/`symbol` are already interned [`Symbol`] on the event, so they are stored by 8-byte
//!   `Copy`. (Order client ids are deliberately NOT interned — unbounded per-order cardinality would leak the
//!   never-freed `Symbol` table; see `vike_model::events`' interning contract.)
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
use vike_marketdata::Symbol;
use vike_model::events::Event;

/// The un-rendered form of one delivered [`Event`]. One variant per rendering SHAPE (not per event
/// kind) — the `kind` label rides as a `&'static str` so the rendered prefix cannot drift from the
/// variant name it came from.
#[derive(Debug, Clone, PartialEq)]
pub enum EventNote {
    /// `FillEvent {order_client_id} {qty}@{px}`
    Fill { order_client_id: CompactString, qty: f64, price: f64 },
    /// `{kind} {order_client_id}` — the bare order-lifecycle lines.
    OrderClientId { kind: &'static str, order_client_id: CompactString },
    /// `{kind} {order_client_id} ({reason})` — reject / deny and the two Rust-native reject twins. NOT
    /// `OrderCanceled`: it carries a `reason` but has never rendered one (see `capture`).
    OrderClientIdReason {
        kind: &'static str,
        order_client_id: CompactString,
        reason: CompactString,
    },
    /// `OrderModified {order_client_id} qty={new_qty:?} px={new_price:?}` — note the `{:?}` on the `Option`s.
    Modified { order_client_id: CompactString, new_qty: Option<f64>, new_price: Option<f64> },
    /// `{kind} {label}` — the position/account lane, labelled by symbol (or venue).
    Label { kind: &'static str, label: Symbol },
    /// `{kind} {label} {num}` — funding amount / liquidated qty.
    LabelNumber { kind: &'static str, label: Symbol, number: f64 },
}

impl EventNote {
    /// Capture the fields this event's line needs. No formatting, no `String`; the only heap
    /// traffic possible is an over-long `reason`. Every `kind` is the event's own wire tag
    /// (`Event::wire_tag`), so a line cannot name an event another way than the wire does.
    pub fn capture(event: &Event) -> EventNote {
        match event {
            Event::Fill(e) => EventNote::Fill {
                order_client_id: CompactString::new(&e.order_client_id),
                qty: e.last_qty,
                price: e.last_price,
            },
            Event::OrderSubmitted(e) => EventNote::OrderClientId {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
            },
            Event::OrderAccepted(e) => EventNote::OrderClientId {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
            },
            Event::OrderRejected(e) => EventNote::OrderClientIdReason {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
                reason: e.reason.clone(),
            },
            Event::OrderDenied(e) => EventNote::OrderClientIdReason {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
                reason: e.reason.clone(),
            },
            Event::OrderTriggered(e) => EventNote::OrderClientId {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
            },
            Event::OrderPartiallyFilled(e) => EventNote::OrderClientId {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
            },
            Event::OrderFilled(e) => EventNote::OrderClientId {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
            },
            // NOTE the asymmetry, which is pre-existing and deliberately preserved: `OrderCanceled`
            // CARRIES a `reason` but the ring line has never shown it, unlike the reject/deny
            // lines. Rendering it here would change what the GUI and the live smokes read, so this
            // is `OrderClientId`, not `OrderClientIdReason`. (First cut of this module got that wrong;
            // `tests::render_matches_the_reference_format` caught it.)
            Event::OrderCanceled(e) => EventNote::OrderClientId {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
            },
            Event::OrderExpired(e) => EventNote::OrderClientId {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
            },
            Event::OrderLiquidated(e) => EventNote::OrderClientId {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
            },
            Event::OrderModified(e) => EventNote::Modified {
                order_client_id: CompactString::new(&e.order_client_id),
                new_qty: e.new_qty,
                new_price: e.new_price,
            },
            Event::OrderCancelRejected(e) => EventNote::OrderClientIdReason {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
                reason: e.reason.clone(),
            },
            Event::OrderModifyRejected(e) => EventNote::OrderClientIdReason {
                kind: event.wire_tag(),
                order_client_id: CompactString::new(&e.order_client_id),
                reason: e.reason.clone(),
            },
            Event::PositionOpened(e) => {
                EventNote::Label { kind: event.wire_tag(), label: e.symbol }
            }
            Event::PositionChanged(e) => {
                EventNote::Label { kind: event.wire_tag(), label: e.symbol }
            }
            Event::PositionClosed(e) => {
                EventNote::Label { kind: event.wire_tag(), label: e.symbol }
            }
            Event::AccountState(e) => EventNote::Label { kind: event.wire_tag(), label: e.venue },
            Event::Funding(e) => {
                EventNote::LabelNumber { kind: event.wire_tag(), label: e.symbol, number: e.amount }
            }
            Event::PositionLiquidated(e) => {
                EventNote::LabelNumber { kind: event.wire_tag(), label: e.symbol, number: e.qty }
            }
            // Its tag is `FeeAdjustmentEvent`: the line said `FeeAdjustment` until B1.22.
            Event::FeeAdjustment(e) => EventNote::LabelNumber {
                kind: event.wire_tag(),
                label: e.symbol,
                number: e.extra_commission,
            },
        }
    }
}

/// The rendering contract, in one place. Byte-identical to the `format!` the fold thread used to
/// run — see the module doc, and `tests::render_matches_the_reference_format` for the pin.
impl std::fmt::Display for EventNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventNote::Fill { order_client_id, qty, price: px } => {
                write!(f, "FillEvent {order_client_id} {qty}@{px}")
            }
            EventNote::OrderClientId { kind, order_client_id } => {
                write!(f, "{kind} {order_client_id}")
            }
            EventNote::OrderClientIdReason { kind, order_client_id, reason } => {
                write!(f, "{kind} {order_client_id} ({reason})")
            }
            EventNote::Modified { order_client_id, new_qty, new_price } => {
                write!(f, "OrderModified {order_client_id} qty={new_qty:?} px={new_price:?}")
            }
            EventNote::Label { kind, label } => write!(f, "{kind} {label}"),
            EventNote::LabelNumber { kind, label, number } => write!(f, "{kind} {label} {number}"),
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
