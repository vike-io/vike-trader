//! The selected venue's detail pane: its badges, the meta row, the key family, one row per tier.

use std::collections::HashMap;
use vike_ui_theme::color::faded;
use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::connections;

use super::editor::render_edit_form;
use super::marks::{tier_colour, tier_glyph};
use super::state::EditState;
use super::{ABSENT_COLOR, CONFIGURED_COLOR, CONNECTING_COLOR, ERROR_COLOR};
use crate::env_write::CredentialWrite;
use crate::keys::{account_fields, credential_fields};
use crate::status::VenueCredStatus;
use crate::summary::{FeedFact, StoreHealth, TIERS, TierState, tier_state};

/// The detail pane's second chip, when the venue has a feed producer at all. ⚠ A BUILD fact — see
/// [`venue_detail`], where the sentence this badge may not contradict is argued at the call.
const FEED_PRODUCER_BADGE: &str = "feed producer in this build";

/// A small outlined badge — the detail pane's `Venue` / [`FEED_PRODUCER_BADGE`] chips.
///
/// ⚠ **Deliberately left WRAPPING**, unlike [`rail_chips`]'s chips, and the difference is the
/// failure mode rather than the mechanism. `Frame::show` builds its content `Ui` through
/// `Ui::new_child` with no layout override, so a badge near the end of [`venue_detail`]'s wrapped
/// row does inherit `main_wrap` and can wrap its own text — but the worst that costs is a taller
/// row with every character still on screen, and it does not COMPOUND (the badges are two, not
/// fourteen). Forcing `TextWrapMode::Extend` here without also measuring and allocating the
/// badge's true width would trade that graceful degradation for silent clipping off the right
/// edge, which is the trade this pane has already been bitten by once.
fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(
            stroke::HAIRLINE,
            faded(color, connections::BADGE_OUTLINE_STRENGTH),
        ))
        .corner_radius(connections::BADGE_RADIUS)
        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::HAIR as i8))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Caption))
                    .color(color),
            );
        });
}

/// One `LABEL  value` cell of the detail pane's meta row. ⚠ Wrapping for the same reason
/// [`badge`] is — three cells, not fourteen, and a wrapped cell loses nothing.
fn meta_cell(ui: &mut egui::Ui, label: &str, value: &str, color: egui::Color32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = space::HAIR;
        ui.label(
            egui::RichText::new(label)
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Caption))
                .color(ABSENT_COLOR),
        );
        ui.label(
            egui::RichText::new(value)
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Body))
                .color(color),
        );
    });
}

/// The venue's KEY FAMILY, derived from [`crate::keys::edit_fields`] rather than written down.
///
/// Two halves, both computed: the longest common `_`-terminated prefix of every key name this
/// venue's forms write, and whether those forms are the GENERIC `{VENUE}_{TIER}_API_*` trio or a
/// shape read out of that bridge's own config loader. ⚠ Deliberately derived: a hand-written list
/// of "which venues are bespoke" is the per-venue roster this crate's own module docs have twice
/// recorded rotting, and `edit_fields` is already the authority.
///
/// ⚠ **`credential_fields`, not [`crate::keys::edit_fields`], and the difference is not cosmetic.** The
/// venue-wide attribution tail is not part of any venue's key family: including it would make
/// binance's LIVE form four fields where the generic trio is three (so every mechanised venue would
/// read `bespoke`), and polymarket's `POLY_LIVE_*` family would share only `POLY` with
/// `POLYMARKET_BUILDER_CODE` — no `_` boundary — so [`common_key_prefix`] would fall back to
/// printing one whole key name where a prefix belongs.
#[must_use]
pub fn key_family(venue: &str) -> String {
    let keys: Vec<String> = TIERS
        .iter()
        .flat_map(|tier| credential_fields(venue, tier).into_iter().map(|(_, key)| key))
        .collect();
    if keys.is_empty() {
        return "no configurable tier".to_string();
    }
    let v = venue.to_uppercase();
    let generic = TIERS.iter().all(|tier| {
        let fields = credential_fields(venue, tier);
        if fields.is_empty() {
            return true;
        }
        let want = [
            format!("{v}_{tier}_API_KEY"),
            format!("{v}_{tier}_API_SECRET"),
            format!("{v}_{tier}_API_PASSPHRASE"),
        ];
        fields.len() == want.len() && fields.iter().zip(&want).all(|((_, k), w)| k == w)
    });
    let shape = if generic { "generic" } else { "bespoke" };
    format!("{}* ({shape})", common_key_prefix(&keys))
}

/// The longest `_`-terminated common prefix of a venue's key names — `BINANCE_`, `POLY_LIVE_`,
/// `CTRADER_`. Falls back to the whole first key when the set shares no `_` boundary at all.
fn common_key_prefix(keys: &[String]) -> String {
    let first = keys[0].as_str();
    let mut end = first.len();
    for key in &keys[1..] {
        let shared =
            first.as_bytes().iter().zip(key.as_bytes()).take_while(|(a, b)| a == b).count();
        end = end.min(shared);
    }
    let head = &first[..end];
    match head.rfind('_') {
        Some(i) => head[..=i].to_string(),
        None => first.to_string(),
    }
}

/// ONE TIER ROW of the detail pane: the tier name, its state IN WORDS, the exact `.env` key names
/// the form writes (with `(optional)` carried through from [`edit_fields`]'s own UI labels), and
/// the edit affordance.
///
/// ⚠ **A tier that does not exist says so in words** — `not configurable`, plus the sentence
/// naming the venue — and offers no button. The old grid rendered it as a hollow ring
/// indistinguishable from "unset", which pointed an operator at a form that does not exist.
///
/// ⚠ **The key NAMES are rendered; no value ever is.** The strings on this row come from
/// [`account_fields`], which composes them out of the venue, the tier and the account label. This
/// function is never handed the store.
fn tier_row(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    tier: &str,
    health: &StoreHealth,
    state: &mut EditState,
) {
    let account = state.account.clone();
    let fields = account_fields(&row.venue, tier, &account);
    let cell = tier_state(row, tier, health);
    let mut open = false;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::MD;
        ui.add_sized(
            connections::TIER_NAME_CELL,
            egui::Label::new(
                egui::RichText::new(title_tier(tier))
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Strong))
                    .strong(),
            )
            .selectable(false),
        );
        // An em dash rather than the rail's middle dot for a tier that does not exist: this cell has the
        // WORDS beside it, so the mark can be the wider one that reads as "no such thing" at text size.
        let glyph = if cell == TierState::NotConfigurable { "\u{2014}" } else { tier_glyph(cell) };
        let color = tier_colour(cell, &Tokens::of(ui.ctx()));
        ui.add_sized(
            connections::TIER_STATE_CELL,
            egui::Label::new(
                egui::RichText::new(format!("{glyph} {}", cell.label()))
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .color(color),
            )
            .selectable(false),
        );
        if cell == TierState::NotConfigurable {
            ui.label(
                egui::RichText::new(format!(
                    "{} has no {} tier — nothing to configure here",
                    row.venue,
                    tier.to_lowercase()
                ))
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Caption))
                .weak(),
            );
            return;
        }
        // ⚠ The UNKNOWN cell keeps its ✏ and its key names: the write is an in-place upsert of
        // NAMED keys and is a separate act from the read that failed, so refusing the form here
        // would remove the one affordance that might be the operator's way out. What is removed is
        // any claim about what is stored — the glyph above says `unknown`, and this line says why.
        if cell == TierState::Unknown {
            ui.label(
                egui::RichText::new("the store could not be opened — this cell was not measured")
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Caption))
                    .color(CONNECTING_COLOR),
            );
        }
        if icons::named(
            ui.small_button(icons::EDIT.rich().size(role_px(ui.ctx(), TextRole::Caption))),
            "edit credentials",
        )
        .clicked()
        {
            open = true;
        }
        // The exact key names, spelled out — this is the mockup's whole reason for a detail pane:
        // a hover tooltip could name ONE key, and most venues' forms write two or four.
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::NONE;
            let text2 = Tokens::of(ui.ctx()).theme.text2;
            for (label, key) in &fields {
                let optional = label.ends_with("(optional)");
                let text = if optional { format!("{key}  (optional)") } else { key.clone() };
                ui.label(
                    egui::RichText::new(text)
                        .monospace()
                        .size(role_px(ui.ctx(), TextRole::Caption))
                        .color(if optional { ABSENT_COLOR } else { text2 }),
                );
            }
        });
    });
    if open {
        state.open(&row.venue, tier);
    }
}

/// `SIM` → `Sim`. The tier strings are the store's spelling; the detail pane reads better in
/// title case, and the account/tier vocabulary elsewhere in this module already lowercases them.
fn title_tier(tier: &str) -> String {
    let mut c = tier.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
        None => String::new(),
    }
}

/// The DETAIL pane for the selected venue: name, badges, the meta row, then one row per tier with
/// the inline editor expanding under whichever row was opened.
pub(super) fn venue_detail(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    feed: FeedFact,
    health: &StoreHealth,
    state: &mut EditState,
    creds: CredentialWrite<'_>,
    readable: &HashMap<String, String>,
) {
    // `tok`, not `t`: the tier filter below names its closure parameter `t`.
    let tok = Tokens::of(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new(&row.venue)
                .monospace()
                .strong()
                .size(role_px(ui.ctx(), TextRole::Title))
                .color(tok.theme.text),
        );
        ui.add_space(space::SM);
        badge(ui, "Venue", ABSENT_COLOR);
        // ⚠ **A BUILD FACT, spelled as one, in the colour of one.** It renders exactly when the
        // binary handed this widget a feed-status handle for this venue, and never otherwise —
        // `FeedFact::NoProducer` is the absent case and most of the roster is in it.
        //
        // ⚠ It used to read `Streaming feed` in the CONNECTING amber, and both halves were wrong
        // beside the row underneath. `has_producer()` is true for every `FeedFact::State`,
        // `Disconnected` included, so `Streaming feed` could sit a few pixels above
        // `Status (live feed)  Disconnected` — a present-tense claim of activity contradicting the
        // measurement next to it — and the amber implied a live state this badge never reads.
        // The name now asserts only what it knows (a producer EXISTS in this build) and the muted
        // colour leaves the live half entirely to the labelled `Status` cell below, which is the
        // only thing here that reads the feed.
        if feed.has_producer() {
            badge(ui, FEED_PRODUCER_BADGE, ABSENT_COLOR);
        }
    });
    ui.add_space(space::SM);

    let tiers: Vec<String> = TIERS
        .iter()
        .filter(|t| tier_state(row, t, health) != TierState::NotConfigurable)
        .map(|t| title_tier(t))
        .collect();
    let tiers = if tiers.is_empty() { "none".to_string() } else { tiers.join(" · ") };
    let feed_color = feed.row().colour.resolve(&tok);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::XL2;
        // ⚠ `Status` is the LIVE FEED's, and the label below the pane says so: it is a different
        // fact from the dots, with a different producer. See `crate::summary`'s module doc.
        meta_cell(ui, "Status (live feed)", feed.label(), feed_color);
        meta_cell(ui, "Tiers", &tiers, tok.theme.text2);
        meta_cell(ui, "Key family", &key_family(&row.venue), tok.theme.text2);
    });
    ui.add_space(space::MD);
    // ⚠ **THE RULE THAT MAKES THE PANE A PANE.** Its twin is the one under the rail's
    // `Venue  S D L` header, and the symmetry is the point: a `Separator` in a top-down layout
    // takes `available_size_before_wrap().x`, so this spans the DETAIL COLUMN — which, since that
    // column became the design's `1fr`, is the whole width left over from the rail.
    //
    // That is not decoration. Every other widget in this pane is left-packed and narrow, so
    // without something that occupies the column the pane's `min_rect` is its longest key name
    // and the pane reads as a card floating in the corner of a wide window, however much room it
    // was actually handed. MEASURED on the live capture that reopened this: a maximized 2560pt
    // window in which every rule stopped a third of the way across.
    ui.separator();
    ui.add_space(space::XS);

    for tier in TIERS {
        tier_row(ui, row, tier, health, state);
        // The inline editor expands UNDER the tier row it belongs to, so the heading, the fields
        // and the row that opened them are one block rather than a form at the foot of the pane.
        if state.target.as_ref().is_some_and(|(v, t)| v == &row.venue && t == tier) {
            render_edit_form(ui, state, creds, readable);
        }
        // ⚠ …and so does the VERDICT, which is the second half of the same argument. It used to be
        // rendered by [`connections_ui`] after the rail/detail block, where the two-column arm's
        // full-height rail put it below the window floor: `save failed: …` reached no pixel, and a
        // Save that did not happen looked exactly like a button that did nothing. Drawn here it is
        // in the operator's eye line, immediately under the row whose ✏ they clicked — on the
        // SUCCESS path too, where the form has closed and this is the only thing that says so.
        if let Some(v) = &state.message
            && v.venue == row.venue
            && v.tier == tier
        {
            let color = if v.is_error { ERROR_COLOR } else { CONFIGURED_COLOR };
            ui.label(
                egui::RichText::new(v.text.as_str())
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Body))
                    .color(color),
            );
        }
        ui.add_space(space::XS);
    }
}

#[path = "detail_tests.rs"]
#[cfg(test)]
mod detail_tests;
