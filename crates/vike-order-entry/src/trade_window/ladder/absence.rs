//! The bookless ladder (spec §4.3, "No book ⇒ no ladder"): what the ladder draws when it has NO book
//! — the words ([`TradeWindowLadderAbsence`]), the icon from its own link's state and the quote it
//! may show beside them — and the classifier that picks those words from the state of the venue's
//! market-data status line ([`depth_absence`] and its two word tables).
//!
//! The classifier came down from vike-app-core's `orders/ladder_absence.rs` in wave 3, so the words
//! and the widget that draws them live together; before that it was extracted from `vike-app`'s
//! `main.rs` (for the DOM window the Trade window replaced) so its unit tests ran in CI. The
//! venue-correct signed-position resolver that once lived beside it is
//! `vike_model::events::PositionSide::signed_size` (one copy for the window, the dispatcher,
//! vike-client and the tradehub).
//!
//! # ⚠ The synthetic book is GONE, and what it was is worth carrying
//!
//! This module used to export `default_price`/`tick_for`/`synth_book`, and the DOM window's glue
//! (`dom_tool_content`, deleted with the DOM window) called them whenever the real
//! `(venue, symbol)` book was absent or empty. `synth_book` produced 40 gapless, perfectly symmetric levels per side around a
//! seeded price, with uniform-random sizes from a counter-seeded LCG. Its seed was the DOM's
//! per-frame repaint counter, which the DOM's `draw` (deleted with the window) incremented at its
//! top — so the entire ladder RE-ROLLED on every frame. It animated like a liquid market whose prices never moved, and
//! the only tells were a dim overlay and a `● STALE` badge. `default_price` fed the same illusion
//! from the other side: a table answering `62_800.0` for `BTCUSDT`, printed in the header in the
//! same amber and the same place a venue's real last price uses.
//!
//! The replacement is not a better placeholder. There is no placeholder: the widget draws nothing
//! ladder-shaped without a book, and this module's job is now to say WHY there is none. The
//! argument is `vike_app_core::ui::capture_seed`'s, one plane over — *"A screenshot of a fabricated
//! `CoreSnapshot` would be a picture of a code path that does not exist, which for the panel that
//! shows a human their money is the worst thing to publish."* A fabricated BOOK is that same claim
//! about the panel a human CLICKS to send an order, and unlike a screenshot it is in front of them
//! while they do it.
//!
//! Nothing else called any of the three, and no capture path depended on them: `scripts/qa_shots.sh`
//! lists `VIKE_TOOL` among its deliberate omissions, and `.trader/shots/manifest.json`'s
//! `grid-dom.png` pose is BLOCKED with a `path_to_unblock` requiring the ladder be proven LIVE
//! before capture — which a synthetic ladder defeats rather than serves.

use super::fmt_price;
use crate::trade_window::TradeWindowQuoteFace;
use egui::{Color32, CornerRadius, Rect, Stroke, StrokeKind};
use vike_model::feed_status::DataClientConnectionState;
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::icons::{self, ThemeIcon};
use vike_ui_theme::maps;
use vike_ui_theme::metrics::{RADIUS, space, stroke};
use vike_ui_theme::side::ThemeSidePair;
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;

/// The WORDS a bookless ladder shows — the glance answer, the market-data session's own status line
/// verbatim, and what an operator can do. The widget takes the no-book DECISION from the book,
/// never from this.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TradeWindowLadderAbsence<'a> {
    pub headline: &'a str,
    pub cause: &'a str,
    pub next_step: &'a str,
}

/// The icon a bookless ladder shows above its words, from its OWN link's state — the kit's three
/// renderings (spec §4.2), all of them STILL (owner decision 5). While connecting there is none:
/// the kit draws "still asking" as a spinner, and a bookless ladder must not move. The words are
/// what a screen reader hears for the icon (`icons::named`).
///
/// A bookless ladder is an empty pane, so which of the kit's panes the link's state is stays HERE and what
/// each one looks like is its row of `ui-theme.toml`'s `empty_pane` map — the tray when the link is up or
/// not known, the cloud when it is down — the rows every other empty pane is drawn from.
pub(super) fn absence_icon(
    link: DataClientConnectionState,
    t: &ThemeTokens,
) -> Option<(ThemeIcon, Color32, &'static str)> {
    let row = match link {
        DataClientConnectionState::Connecting => return None,
        DataClientConnectionState::Connected | DataClientConnectionState::Unknown => {
            &maps::empty_pane::EMPTY
        }
        DataClientConnectionState::Disconnected | DataClientConnectionState::Error => {
            &maps::empty_pane::UNREACHABLE
        }
    };
    Some((row.icon()?, row.colour.resolve(t), row.word?))
}

/// The whole of what a bookless ladder shows: no ladder at all, no rows, no bars, no interaction.
///
/// Rendered as real `label` widgets inside a child `Ui` rather than as `painter.text`, so every
/// line lands in the accessibility tree and the window's a11y tests can assert the words on a
/// GPU-less runner. A painted string is invisible to those tests, and an empty state nothing can
/// gate is one refactor away from becoming a fabricated ladder again.
pub(super) fn no_book(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    region: Rect,
    absence: Option<TradeWindowLadderAbsence<'_>>,
    link: DataClientConnectionState,
    quote: Option<(TradeWindowQuoteFace<'_>, Option<f64>)>,
) {
    // An inset frame: unmistakably NOT the ladder's own painted grid.
    ui.painter().rect_stroke(
        region.shrink(space::MD),
        CornerRadius::same(RADIUS),
        Stroke::new(stroke::HAIRLINE, t.theme.border),
        StrokeKind::Inside,
    );
    let a = absence.unwrap_or_default();
    let headline = if a.headline.is_empty() { NO_BOOK_HEADLINE } else { a.headline };
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(region.shrink(space::XL2))
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    child.spacing_mut().item_spacing.y = t.metrics.gap;
    // A quote's three lines stand in for the subline, and the block starts that much higher.
    let rise = trade::NO_BOOK_RISE * if quote.is_some() { 2.5 } else { 1.0 };
    child.add_space((region.height() * 0.5 - rise).max(0.0));
    if let Some((icon, colour, words)) = absence_icon(link, t) {
        let glyph = icon.rich().size(t.text.px(ThemeTextRole::Heading)).color(colour);
        icons::named(child.label(glyph), words);
    }
    child.label(
        egui::RichText::new(headline).font(t.font(ThemeTextRole::Title)).color(t.theme.text),
    );
    if let Some((q, tick)) = quote {
        let (bid_ink, ask_ink) = ThemeSidePair::BidAsk.texts(t);
        for (word, price, ink) in [("Bid", q.bid, bid_ink), ("Ask", q.ask, ask_ink)] {
            let text = format!("{word} {}", fmt_price(price, tick));
            child.label(egui::RichText::new(text).font(t.mono(ThemeTextRole::Title)).color(ink));
        }
        let (words, ink) = match q.delay {
            Some(delay) => {
                (format!("{} · {delay}", quote_age_words(q.age_ms)), ThemeStatus::Warning.color())
            }
            None => (quote_age_words(q.age_ms), t.theme.text_secondary),
        };
        child.label(egui::RichText::new(words).font(t.mono(ThemeTextRole::Body)).color(ink));
        // The age moves with no new quote, and on a quiet pair nothing else repaints the window.
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    } else {
        child.label(
            egui::RichText::new(NO_BOOK_SUBLINE)
                .font(t.font(ThemeTextRole::Body))
                .color(t.theme.text_secondary),
        );
    }
    if !a.cause.is_empty() {
        // the link's own status line, read character by character: mono
        child.label(
            egui::RichText::new(a.cause).font(t.mono(ThemeTextRole::Body)).color(t.theme.text),
        );
    }
    if !a.next_step.is_empty() {
        child.label(
            egui::RichText::new(a.next_step)
                .font(t.font(ThemeTextRole::Body))
                .color(t.theme.text_tertiary),
        );
    }
}

/// How a quote's age reads: whole seconds, whole minutes past two of them.
pub(crate) fn quote_age_words(age_ms: i64) -> String {
    let secs = age_ms.max(0) / 1000;
    if secs < 120 { format!("{secs} s old") } else { format!("{} min old", secs / 60) }
}

/// The headline a bookless ladder falls back to when the caller supplied none.
pub const NO_BOOK_HEADLINE: &str = "NO ORDER BOOK";

/// The line that states, in so many words, that nothing is being drawn — the sentence whose
/// ABSENCE let a synthetic ladder pass for a real one for as long as it did.
pub const NO_BOOK_SUBLINE: &str =
    "nothing is drawn below: no depth has been received for this venue + symbol";

/// What the Trade window knows about WHY it has no order book for its venue + symbol.
///
/// Derived from ONE fact the shell already holds: that venue's market-data status line, written by
/// `crates/vike-app-core/src/data/market_data_session/session.rs`'s `refresh_statuses` — the thread that dials,
/// subscribes and reads. This classifier adds no knowledge of its own; it picks the HEADLINE and
/// the NEXT STEP, while the status line itself goes on screen VERBATIM as the cause. That split is
/// deliberate: if a situation lands in a coarser state than it deserves, the operator still has the
/// exact words in front of them, so the worst outcome is a vague headline rather than a wrong one.
///
/// The classification reads the line's STATE, as its producer set it
/// (`vike_model::feed_status::DataClientStatus`'s `state`), the same state the window's FEED badge
/// reads, so the badge and this headline never disagree; nothing here reads the line's words, which
/// were got wrong more than once by callers who reasoned about the strings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TradeWindowLadderAbsenceCause {
    /// This venue has no market-data status producer at all — it is absent from the status map.
    /// `MarketDataSession::sync_mount` mounts a line for each venue the connected datahub ADVERTISES, so
    /// this is a venue it does not advertise, or any venue before its first handshake has landed
    /// (or a build that mounts no session).
    NoFeed,
    /// The link is down, idle, or has never resolved a datahub address.
    LinkDown,
    /// The link reported a fault.
    LinkFault,
    /// The link is dialling or subscribing.
    Dialling,
    /// The link is up and serving this venue — the book simply has not arrived for THIS symbol.
    NoSnapshot,
    /// The link is up and this venue is not being served: refused, nothing wanted, or no holder.
    /// ⚠ They are ONE variant on purpose: `refresh_statuses` sets each of them `Unknown` (the link
    /// is fine and nothing flows), so a classifier claiming to tell them apart would be guessing.
    /// The verbatim cause line DOES tell them apart, which is why it is the half that goes on screen.
    NotServed,
}

/// Classify the Trade window's depth absence from the STATE of that venue's market-data status
/// line.
///
/// `None` ⇒ the venue is absent from the status map entirely (no producer), which is a different
/// statement from a line whose producer reports no state yet: that one reads `Unknown`
/// ([`TradeWindowLadderAbsenceCause::NotServed`]), as the FEED badge reads it (`FEED ?`), never
/// "link down".
pub fn depth_absence(status: Option<DataClientConnectionState>) -> TradeWindowLadderAbsenceCause {
    let Some(state) = status else {
        return TradeWindowLadderAbsenceCause::NoFeed;
    };
    match state {
        DataClientConnectionState::Connected => TradeWindowLadderAbsenceCause::NoSnapshot,
        DataClientConnectionState::Connecting => TradeWindowLadderAbsenceCause::Dialling,
        DataClientConnectionState::Disconnected => TradeWindowLadderAbsenceCause::LinkDown,
        DataClientConnectionState::Error => TradeWindowLadderAbsenceCause::LinkFault,
        DataClientConnectionState::Unknown => TradeWindowLadderAbsenceCause::NotServed,
    }
}

/// The glance answer for one absence. Exhaustive with no `_` arm, so a new variant cannot silently
/// inherit another's words.
pub fn depth_absence_headline(a: TradeWindowLadderAbsenceCause) -> &'static str {
    match a {
        TradeWindowLadderAbsenceCause::NoFeed => "NO DEPTH FEED FOR THIS VENUE",
        TradeWindowLadderAbsenceCause::LinkDown => "NO ORDER BOOK — MARKET-DATA LINK DOWN",
        TradeWindowLadderAbsenceCause::LinkFault => "NO ORDER BOOK — MARKET-DATA LINK FAULT",
        TradeWindowLadderAbsenceCause::Dialling => "NO ORDER BOOK YET — CONNECTING",
        TradeWindowLadderAbsenceCause::NoSnapshot => {
            "NO ORDER BOOK YET — AWAITING THE FIRST SNAPSHOT"
        }
        TradeWindowLadderAbsenceCause::NotServed => {
            "NO ORDER BOOK — THIS VENUE IS NOT BEING SERVED"
        }
    }
}

/// What an operator can actually do about one absence. Exhaustive for the same reason.
///
/// ⚠ Every one of these names the DATAHUB, never the tradehub. The observe connection the shell's
/// status bar reports carries no book at all — `vike_tradehub_client::wire::CoreSnapshotWire` and
/// `vike_execution::CoreSnapshot` each have no book, depth, quote or tape field — so "reconnect the
/// backend" would be advice that cannot work, offered at the exact moment somebody is trying to
/// find out why their ladder is blank.
pub fn depth_absence_next_step(a: TradeWindowLadderAbsenceCause) -> &'static str {
    match a {
        TradeWindowLadderAbsenceCause::NoFeed => {
            "The datahub this desktop is connected to advertises no market-data feed for it — or \
             has not answered yet. Switch the window to a venue the datahub serves (Connections \
             lists them), or check the DATAHUB address."
        }
        TradeWindowLadderAbsenceCause::LinkDown => {
            "Check the DATAHUB address in Connections. It is a different connection from the \
             tradehub the status bar reports."
        }
        TradeWindowLadderAbsenceCause::LinkFault => {
            "Connections carries this venue's full message, and the log carries the server's own \
             words."
        }
        TradeWindowLadderAbsenceCause::Dialling => {
            "Nothing to do — the ladder fills when the first snapshot lands."
        }
        TradeWindowLadderAbsenceCause::NoSnapshot => {
            "The link is serving this venue. If it persists, the datahub may have no depth stream \
             for this symbol."
        }
        TradeWindowLadderAbsenceCause::NotServed => {
            "The datahub is reachable but streams no depth here — it may have refused the key, or \
             nothing has subscribed yet. Connections carries its exact words."
        }
    }
}
