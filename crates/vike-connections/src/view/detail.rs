//! The selected venue's detail pane: its badges, the meta row, the field family, one row per tier
//! (each the account row of `(venue, label, tier)`) and the venue's application row.

use vike_model::AccountTier;
use vike_ui_theme::color::faded;
use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::{space, stroke};
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::connections;
use vike_venues::credential_keys::{API_KEY, API_PASSPHRASE, API_SECRET};
use vike_venues::credential_schema::{FieldOwner, schema_for};

use super::editor::render_edit_form;
use super::marks::{tier_colour, tier_glyph};
use super::state::EditState;
use super::{ABSENT_COLOR, CONFIGURED_COLOR, CONNECTING_COLOR, ERROR_COLOR, note};
use crate::env_write::CredentialWrite;
use crate::keys::{FieldTarget, FormField, FormPlace, form_fields, has_account_form};
use crate::status::{AccountGrids, HeldFields, Presence, TierCell, VenueCredStatus};
use crate::summary::{FeedFact, TIERS, TierState, tier_state};
use vike_secrets::credentials::StoreHealth;

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

/// **The venue's credential FIELD FAMILY**, read off its row of `vike_venues::credential_schema`
/// rather than written down: the fields an account's form writes, the venue's application fields
/// when it has any, and whether the account set is the GENERIC `API_KEY` / `API_SECRET` /
/// `API_PASSPHRASE` trio or a shape read out of that bridge's own loader.
///
/// ⚠ Deliberately derived: a hand-written list of "which venues are bespoke" is the per-venue
/// roster this crate's own module docs have twice recorded rotting, and the schema is already the
/// authority. FIELD words only — no credential NAME is composed (decision 0156 item 6).
#[must_use]
pub fn field_family(venue: &str) -> String {
    let Some(schema) = schema_for(venue) else { return "no credential fields".to_string() };
    let account: Vec<&str> = schema.form(FieldOwner::Account).iter().map(|f| f.field).collect();
    if account.is_empty() {
        return "no credential fields".to_string();
    }
    let generic = account == [API_KEY, API_SECRET, API_PASSPHRASE];
    let app: Vec<&str> = schema.form(FieldOwner::Venue).iter().map(|f| f.field).collect();
    let shape = if generic { "generic" } else { "bespoke" };
    if app.is_empty() {
        format!("{} ({shape})", account.join(" · "))
    } else {
        format!("{} + app {} ({shape})", account.join(" · "), app.join(" · "))
    }
}

/// One FIELD line of a row: the field it writes, `(optional)` where the loader does without it,
/// and whether the store holds it — `stored` / `missing` (a required field not held). Presence by
/// FIELD only; no value ever.
fn field_lines(ui: &mut egui::Ui, fields: &[FormField], held: impl Fn(&FormField) -> bool) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = space::NONE;
        let text2 = Tokens::of(ui.ctx()).theme.text2;
        for field in fields {
            let optional = !field.required();
            let presence = match (held(field), optional) {
                (true, _) => "  stored",
                (false, false) => "  missing",
                (false, true) => "",
            };
            let text = if optional {
                format!("{}  (optional){presence}", field.hint())
            } else {
                format!("{}{presence}", field.hint())
            };
            ui.label(
                egui::RichText::new(text)
                    .monospace()
                    .size(role_px(ui.ctx(), TextRole::Caption))
                    .color(if optional { ABSENT_COLOR } else { text2 }),
            );
        }
    });
}

/// Whether the owner (`held`) or the deployment map holds `field` — presence only.
fn holds(grids: &AccountGrids, held: Option<&HeldFields>, field: &FormField) -> bool {
    match &field.target {
        FieldTarget::Account(k) | FieldTarget::Venue(k) => held.is_some_and(|h| h.holds(k.field)),
        FieldTarget::Deployment(name) => grids.deployment_value(name).is_some(),
    }
}

/// The name cell and the state cell every row of the pane opens with.
fn row_head(ui: &mut egui::Ui, name: &str, cell: TierState) {
    ui.add_sized(
        connections::TIER_NAME_CELL,
        egui::Label::new(
            egui::RichText::new(name)
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Strong))
                .strong(),
        )
        .selectable(false),
    );
    // An em dash rather than the rail's middle dot for a cell that takes no credential: this cell
    // has the WORDS beside it, so the mark can be the wider one that reads as "no such thing" at
    // text size.
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
}

/// A caption of a row, set through [`note`] — in `connections::NOTE_W` at most, never across
/// whatever width the window offers. ⚠ A bare `ui.label` in the row's `horizontal` layout would
/// not wrap at all (a non-wrapping horizontal layout lays a label out at its full length), and the
/// longest of these sentences — the no-account one, which names a command — would then decide how
/// wide the window is, or run past its right edge.
fn caption(ui: &mut egui::Ui, text: String, color: Option<egui::Color32>) {
    let rich = egui::RichText::new(text).monospace().size(role_px(ui.ctx(), TextRole::Caption));
    note(
        ui,
        match color {
            Some(c) => rich.color(c),
            None => rich.weak(),
        },
    );
}

/// Why a cell takes no credential, in words — the paper tier, a venue with no form, no account
/// row (and how to create it), or more than one row with no single active one.
fn not_configurable_sentence(
    row: &VenueCredStatus,
    tier: AccountTier,
    state: &EditState,
) -> String {
    let venue = &row.venue;
    // ⚠ Kept to one short line: it stands on every venue's paper row, and a sentence that wraps
    // differently at two window widths makes the panel's height track the window's.
    if tier == AccountTier::Paper {
        return "paper: the simulator reads no credential".to_string();
    }
    if !has_account_form(venue) {
        return format!("{venue} has no credential fields — nothing to configure here");
    }
    let label = state.account.text();
    match row.cell(tier) {
        TierCell::Ambiguous(ids) => {
            let ids: Vec<String> = ids.iter().map(i64::to_string).collect();
            format!(
                "more than one {tier} account row answers here (rows {}) and none is the one \
                 active row — give each a label; no form is offered for a guess",
                ids.join(", ")
            )
        }
        _ => {
            let (whose, flag) = match label {
                Some(l) => (format!(" labelled {l}"), format!(" --label {l}")),
                None => (String::new(), String::new()),
            };
            format!(
                "no {tier} account{whose} for {venue} — create it with its tier first \
                 (`vike-cli secrets account add --venue {venue} --tier {tier}{flag}`): a \
                 credential is attached to an account and never creates one"
            )
        }
    }
}

/// ONE TIER ROW of the detail pane: the tier, its state IN WORDS, the account row the cell is, the
/// FIELDS its form writes with their presence, and the edit affordance.
///
/// ⚠ **A cell that takes no credential says so in words** — `not configurable`, plus the sentence
/// naming why (the paper tier, no account row and how to create it, an ambiguous pair) — and
/// offers no button. A credential write never creates an account (decision 0156 item 6), so a
/// form here would be a Save the store refuses.
///
/// ⚠ **Field words and presence are rendered; no value ever is.** This function reads
/// [`HeldFields::holds`] and nothing that carries a value.
fn tier_row(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    tier: AccountTier,
    health: &StoreHealth,
    state: &mut EditState,
    grids: &AccountGrids,
) {
    let cell = tier_state(row, tier, health);
    let fields = form_fields(&row.venue, FormPlace::Tier(tier));
    let mut open: Option<i64> = None;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::MD;
        row_head(ui, &title_tier(tier), cell);
        match cell {
            TierState::NotConfigurable => {
                caption(ui, not_configurable_sentence(row, tier, state), None);
            }
            // ⚠ The UNKNOWN cell offers NO form: a credential is written to an account ROW, and the
            // rows could not be read, so there is no row to address. What is removed is any claim
            // about what is stored — the glyph says `unknown`, and this line says why.
            TierState::Unknown => caption(
                ui,
                "the store could not be opened — this cell was not measured, and no form is \
                 offered: a credential is written to an account row, and the rows were not read"
                    .to_string(),
                Some(CONNECTING_COLOR),
            ),
            TierState::Configured | TierState::NotSet => {
                let TierCell::Account(account) = row.cell(tier) else { return };
                if icons::named(
                    ui.small_button(icons::EDIT.rich().size(role_px(ui.ctx(), TextRole::Caption))),
                    "edit credentials",
                )
                .clicked()
                {
                    open = Some(account.id);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = space::NONE;
                    let inactive = if account.active { "" } else { " · INACTIVE" };
                    caption(ui, format!("account {}{inactive}", account.id), None);
                    if account.presence() == Presence::Partial {
                        caption(
                            ui,
                            format!(
                                "partial: {} of {} required fields stored",
                                account.held_required.len(),
                                account.held_required.len() + account.missing_required.len()
                            ),
                            Some(CONNECTING_COLOR),
                        );
                    }
                    let held = grids.account_fields(account.id);
                    field_lines(ui, &fields, |f| holds(grids, held, f));
                });
            }
        }
    });
    if let Some(id) = open {
        state.open(&row.venue, FormPlace::Tier(tier), Some(id));
    }
}

/// **The venue's APPLICATION row** — the fields the venue holds once for every account
/// (`FieldOwner::Venue`: cTrader's OAuth client pair), shown ONCE per venue and never per tier.
/// Drawn only for a venue that has such fields.
///
/// Its state reads the venue's REQUIRED application fields; under an unreadable store it is
/// unknown and offers no form, like a tier cell.
fn venue_app_row(
    ui: &mut egui::Ui,
    venue: &str,
    health: &StoreHealth,
    state: &mut EditState,
    grids: &AccountGrids,
) {
    let fields = form_fields(venue, FormPlace::VenueApp);
    let held = grids.venue_fields(venue);
    let cell = if !health.is_readable() {
        TierState::Unknown
    } else if fields.iter().filter(|f| f.required()).all(|f| holds(grids, held, f)) {
        TierState::Configured
    } else {
        TierState::NotSet
    };
    let mut open = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::MD;
        row_head(ui, "App", cell);
        if cell == TierState::Unknown {
            caption(
                ui,
                "the store could not be opened — this row was not measured".to_string(),
                Some(CONNECTING_COLOR),
            );
            return;
        }
        if icons::named(
            ui.small_button(icons::EDIT.rich().size(role_px(ui.ctx(), TextRole::Caption))),
            "edit credentials",
        )
        .clicked()
        {
            open = true;
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::NONE;
            caption(
                ui,
                format!("the {venue} application — one registration every account shares"),
                None,
            );
            field_lines(ui, &fields, |f| holds(grids, held, f));
        });
    });
    if open {
        state.open(venue, FormPlace::VenueApp, None);
    }
}

/// `paper` → `Paper`. The tier's own word (`AccountTier::as_str`) in title case: the detail pane
/// reads better that way, and the account/tier vocabulary elsewhere in this module lowercases it.
fn title_tier(tier: AccountTier) -> String {
    let word = tier.as_str();
    let mut c = word.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// The verdict of the last Save under the row it belongs to, if it is this row's.
fn verdict_under(ui: &mut egui::Ui, state: &EditState, venue: &str, place: FormPlace) {
    // ⚠ The VERDICT is drawn under the row it belongs to. It used to be rendered by
    // [`connections_ui`] after the rail/detail block, where the two-column arm's full-height rail
    // put it below the window floor: `save failed: …` reached no pixel, and a Save that did not
    // happen looked exactly like a button that did nothing. Drawn here it is in the operator's eye
    // line, immediately under the row whose ✏ they clicked — on the SUCCESS path too, where the
    // form has closed and this is the only thing that says so.
    if let Some(v) = &state.message
        && v.venue == venue
        && v.place == place
    {
        let color = if v.is_error { ERROR_COLOR } else { CONFIGURED_COLOR };
        ui.label(
            egui::RichText::new(v.text.as_str())
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Body))
                .color(color),
        );
    }
}

/// Whether `place`'s form of `venue` is the one open.
fn is_open(state: &EditState, venue: &str, place: FormPlace) -> bool {
    state.target.as_ref().is_some_and(|t| t.venue == venue && t.place == place)
}

/// The DETAIL pane for the selected venue: name, badges, the meta row, then one row per tier with
/// the inline editor expanding under whichever row was opened, then the venue's application row.
pub(super) fn venue_detail(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    feed: FeedFact,
    health: &StoreHealth,
    state: &mut EditState,
    creds: CredentialWrite<'_>,
    grids: &AccountGrids,
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

    // The tiers a credential can go to for this account — its rows (or, under an unreadable
    // store, the tiers whose rows were not measured).
    let tiers: Vec<String> = TIERS
        .iter()
        .filter(|t| tier_state(row, **t, health) != TierState::NotConfigurable)
        .map(|t| title_tier(*t))
        .collect();
    let tiers = if tiers.is_empty() { "none".to_string() } else { tiers.join(" · ") };
    let feed_color = feed.row().colour.resolve(&tok);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::XL2;
        // ⚠ `Status` is the LIVE FEED's, and the label below the pane says so: it is a different
        // fact from the dots, with a different producer. See `crate::summary`'s module doc.
        meta_cell(ui, "Status (live feed)", feed.label(), feed_color);
        meta_cell(ui, "Tiers", &tiers, tok.theme.text2);
        meta_cell(ui, "Fields", &field_family(&row.venue), tok.theme.text2);
    });
    ui.add_space(space::MD);
    // ⚠ **THE RULE THAT MAKES THE PANE A PANE.** Its twin is the one under the rail's
    // `Venue  P D L` header, and the symmetry is the point: a `Separator` in a top-down layout
    // takes `available_size_before_wrap().x`, so this spans the DETAIL COLUMN — which, since that
    // column became the design's `1fr`, is the whole width left over from the rail.
    //
    // That is not decoration. Every other widget in this pane is left-packed and narrow, so
    // without something that occupies the column the pane's `min_rect` is its longest field line
    // and the pane reads as a card floating in the corner of a wide window, however much room it
    // was actually handed. MEASURED on the live capture that reopened this: a maximized 2560pt
    // window in which every rule stopped a third of the way across.
    ui.separator();
    ui.add_space(space::XS);

    for tier in TIERS {
        let place = FormPlace::Tier(tier);
        tier_row(ui, row, tier, health, state, grids);
        // The inline editor expands UNDER the tier row it belongs to, so the heading, the fields
        // and the row that opened them are one block rather than a form at the foot of the pane.
        if is_open(state, &row.venue, place) {
            render_edit_form(ui, state, creds, grids);
        }
        verdict_under(ui, state, &row.venue, place);
        ui.add_space(space::XS);
    }

    // The venue's application credentials: ONE row per venue, after its tiers, for a venue that
    // has any.
    if !form_fields(&row.venue, FormPlace::VenueApp).is_empty() {
        venue_app_row(ui, &row.venue, health, state, grids);
        if is_open(state, &row.venue, FormPlace::VenueApp) {
            render_edit_form(ui, state, creds, grids);
        }
        verdict_under(ui, state, &row.venue, FormPlace::VenueApp);
        ui.add_space(space::XS);
    }
}

#[path = "detail_tests.rs"]
#[cfg(test)]
mod detail_tests;
