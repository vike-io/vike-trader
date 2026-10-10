//! The tier-state marks: the rail's four glyphs and their colours, the mark legend, the footnote.

use vike_ui_theme::color::faded;
use vike_ui_theme::components::{Tokens, role_px};
use vike_ui_theme::icons;
use vike_ui_theme::type_scale::TextRole;
use vike_ui_theme::value::connections;

use vike_model::accounts::account_keys::AccountLabel;

use super::ABSENT_COLOR;
use crate::keys::account_expected_key_name;
use crate::status::VenueCredStatus;
use crate::summary::{StoreHealth, TierState, tier_state};

/// ⚠ **The rail's four glyphs, and why the last two are not dots.**
///
/// The filled `●` and the hollow `○` are the two MEASUREMENTS — every key set, and none set.
/// The other two states are not measurements at all and must not be spelled with the same shape:
///
/// * [`TierState::NotConfigurable`] used to render a DIM FILLED `●`, which is a filled dot at the
///   glyph level however low its opacity. That mattered beyond aesthetics: `scripts/qa_shots.sh`'s
///   `05-connections` pose tells a human judge that on the credential-free QA root *a single
///   FILLED dot means the isolation broke — report it immediately*, and a dim `●` is what dukascopy
///   SIM/LIVE, aster SIM, hyperliquid SIM, alpaca SIM and polymarket SIM/DEMO all rendered there.
///   Every clean capture false-positived that instruction, and a tree-reading test could not tell
///   the two apart either. A middle dot is the "dim, low-opacity mark" the design asks for and is
///   the same shape no measurement uses.
/// * [`TierState::Unknown`] is a QUESTION MARK, because the store did not open and nothing about
///   this cell was measured. It is the one glyph here that is not grey.
const GLYPH_CONFIGURED: &str = "\u{25CF}";
const GLYPH_NOT_SET: &str = "\u{25CB}";
const GLYPH_NOT_CONFIGURABLE: &str = "\u{00B7}";
const GLYPH_UNKNOWN: &str = "?";

/// The rail's glyph for a tier state. (A `tier_state` row has no field for a text glyph, so the four stay
/// here with their argument above.)
pub(super) fn tier_glyph(state: TierState) -> &'static str {
    match state {
        TierState::Configured => GLYPH_CONFIGURED,
        TierState::NotSet => GLYPH_NOT_SET,
        TierState::NotConfigurable => GLYPH_NOT_CONFIGURABLE,
        TierState::Unknown => GLYPH_UNKNOWN,
    }
}

/// The colour of a tier state's mark under `t`: its `tier_state` row's `colour`, dimmed by
/// `connections::NOT_CONFIGURABLE_DIM` where the row's `flag` says it is. The one place the three renderings
/// of a tier state — the rail's dot, the detail pane's tier row and the legend — read their colour, so they
/// cannot disagree. (The dimming is a blend, not a role, so it stays here.)
pub(super) fn tier_colour(state: TierState, t: &Tokens) -> egui::Color32 {
    let row = state.row();
    let colour = row.colour.resolve(t);
    if row.flag == Some(true) { faded(colour, connections::NOT_CONFIGURABLE_DIM) } else { colour }
}

/// One rail dot for one (venue, tier) cell — CREDENTIAL PRESENCE, never a feed state. The four
/// glyphs are the four [`TierState`]s and the hover text names the exact key this cell's form
/// would write, for the account being shown.
pub(super) fn tier_dot(
    ui: &mut egui::Ui,
    row: &VenueCredStatus,
    tier: &str,
    account: &AccountLabel,
    health: &StoreHealth,
) {
    let state = tier_state(row, tier, health);
    let (glyph, color) = (tier_glyph(state), tier_colour(state, &Tokens::of(ui.ctx())));
    let hover = match state {
        TierState::NotConfigurable => {
            format!("{} has no {} tier", row.venue, tier.to_lowercase())
        }
        // ⚠ NOT the key name: naming a key beside a mark that measured nothing invites the reading
        // that the key was looked for and found missing. It was not looked for at all.
        TierState::Unknown => {
            "the credential store could not be opened — nothing about this cell was measured"
                .to_string()
        }
        // ⚠ The KEY NAME, never its value: `account_expected_key_name` composes a name out of the
        // venue, the tier and the account label and has no access to the store at all.
        _ => account_expected_key_name(&row.venue, tier, account),
    };
    // ⚠ A FIXED cell width, matching the `S  D  L` header's, so the three dots sit in columns
    // rather than drifting with each glyph's own advance — a rail whose dots do not line up is a
    // rail you cannot read down.
    ui.add_sized(
        [connections::DOT_W, connections::CHIP_H],
        egui::Label::new(
            egui::RichText::new(glyph)
                .monospace()
                .size(role_px(ui.ctx(), TextRole::Title))
                .color(color),
        )
        .selectable(false),
    )
    .on_hover_text(hover);
}

/// The legend's own text size. Named because [`legend_w`] lays the entries out at it to MEASURE
/// the run, and [`mark_legend_items`] draws them at it — a run measured at one size and drawn at
/// another is a run that does not land where the measurement said it would.
const LEGEND_TEXT: TextRole = TextRole::Caption;

/// What a legend entry shows: a presence glyph (text) or an icon (the edit pencil).
#[derive(Clone, Copy)]
enum Mark {
    Glyph(&'static str),
    Icon(icons::Icon),
}

impl Mark {
    /// The legend entry `<mark> = <word>` as ONE paragraph at the legend's size, the mark in the
    /// face it draws from: a presence glyph in the mono text, an icon in the icon family.
    fn entry(
        self,
        style: &egui::Style,
        px: f32,
        word: &str,
        color: egui::Color32,
    ) -> egui::text::LayoutJob {
        match self {
            Mark::Glyph(g) => {
                let mut job = egui::text::LayoutJob::default();
                egui::RichText::new(legend_text(g, word))
                    .monospace()
                    .size(px)
                    .color(color)
                    .append_to(&mut job, style, egui::FontSelection::Default, egui::Align::Center);
                job
            }
            Mark::Icon(i) => i.before(
                style,
                egui::RichText::new(format!("= {word}")).monospace().size(px).color(color),
            ),
        }
    }
}

/// **The mark legend, as data** — one row per mark: the mark, the word, the colour, the hover.
///
/// A function rather than an inline array because the run is MEASURED before it is drawn (see
/// [`legend_w`]), and two copies of this table are two chances for the measurement to describe a
/// legend the panel does not render.
fn legend_entries(ui: &egui::Ui) -> [(Mark, &'static str, egui::Color32, &'static str); 5] {
    let tok = Tokens::of(ui.ctx());
    let text2 = tok.theme.text2;
    [
        (
            Mark::Glyph(GLYPH_CONFIGURED),
            "configured",
            tier_colour(TierState::Configured, &tok),
            "every key that tier's form writes is set in this store, for the account selected to \
             the left",
        ),
        (
            Mark::Glyph(GLYPH_NOT_SET),
            "not set",
            tier_colour(TierState::NotSet, &tok),
            "the tier exists for this venue and no key for it is in the store",
        ),
        (
            Mark::Glyph(GLYPH_NOT_CONFIGURABLE),
            "no such tier",
            tier_colour(TierState::NotConfigurable, &tok),
            "this venue has no such tier — nothing to configure, which is why the mark is not a dot",
        ),
        (
            Mark::Glyph(GLYPH_UNKNOWN),
            "not measured",
            tier_colour(TierState::Unknown, &tok),
            "the credential store could not be opened — nothing about that cell was measured",
        ),
        (
            Mark::Icon(icons::EDIT),
            "edit",
            text2,
            "opens a masked form for that tier; the fields start empty and an existing secret is \
             never shown",
        ),
    ]
}

/// One legend entry's rendered text. ⚠ `<glyph> = <word>`, and the `=` is load-bearing rather than
/// decorative — see [`mark_legend_items`], where the whole argument is.
fn legend_text(glyph: &str, word: &str) -> String {
    format!("{glyph} = {word}")
}

/// The width the whole legend run occupies, laid out with the real painter at [`LEGEND_TEXT`] and
/// spaced with the gap the row it is going into actually uses.
///
/// This is what lets the legend be RIGHT-ALIGNED opposite the account chips on one row without a
/// flex container: [`account_strip`] asks how wide the run is, and pads to it when the row has the
/// room. When it does not, nothing is padded and the run simply wraps onto the next line like any
/// other item in a `horizontal_wrapped` — which is the 400pt arm.
pub(super) fn legend_w(ui: &egui::Ui, gap: f32) -> f32 {
    let entries = legend_entries(ui);
    let text: f32 = entries
        .iter()
        .map(|(mark, word, _, _)| {
            let job = mark.entry(
                ui.style(),
                role_px(ui.ctx(), LEGEND_TEXT),
                word,
                egui::Color32::PLACEHOLDER,
            );
            ui.painter().layout_job(job).size().x
        })
        .sum();
    text + gap * (entries.len() as f32 - 1.0)
}

/// **The compact one-line legend, drawn INLINE on the account-strip row.**
///
/// ⚠⚠ **This used to be a full-width TEXT WALL and that is what it was reported as.** It was two
/// stacked rows of its own under the strip: this run, and then a ~250-character SENTENCE as a
/// wrapping `Label`. In a window free to size to its content that sentence laid out on one line
/// and took the window with it — MEASURED on the owner's capture, a ~2000pt window around a
/// ~700pt panel, with the legend band running edge to edge across the top of it. The run is now
/// one line on a row that already exists, and the sentence is [`rail_footnote`], set in a column.
/// `view`'s module doc (`view.rs`) carries the sizing mechanism.
///
/// ⚠ **The run right-aligns against the ROW's right edge, which is now the window's**, since the
/// panel stopped capping its own width. That is the design's `space-between`: the account chips at
/// one end of the strip and the key at the other. It is not anchored to a measure — a legend
/// floating in the middle of a wide bar, ending where nothing else ends, is worse than one at the
/// edge.
///
/// ⚠ **`<glyph> = <word>`, and the `=` is load-bearing rather than decorative** — the approved
/// design spells these entries `● configured`, and that spelling may not be copied. The DETAIL
/// pane's own status cell is `<glyph> <word>` for the SAME two words (`tier_row`, over
/// `TierState::label`). Spelled identically, a legend entry and a cell CLAIMING to have measured
/// something would be the same accessibility node text, and
/// `crates/vike-connections/tests/panel/a11y_detail.rs`'s
/// `an_unreadable_store_says_so_and_renders_no_measurement` — whose whole job is to catch a cell
/// that claims a measurement nothing took — could no longer tell them apart. Its `status_cells`
/// helper says the same thing from the other side. The separator is what keeps that gate exact
/// rather than approximate, and two characters at 10pt is what it costs.
///
/// ⚠ Adds plain `Label`s straight to the caller's row and opens NO container of its own. The row
/// is `horizontal_wrapped`, whose layout carries `main_wrap: true`, and a child `Ui` that inherits
/// it is the 48× vertical blow-up [`rail_chips`] documents.
pub(super) fn mark_legend_items(ui: &mut egui::Ui) {
    for (mark, word, color, hover) in legend_entries(ui) {
        let job = mark.entry(ui.style(), role_px(ui.ctx(), LEGEND_TEXT), word, color);
        ui.add(egui::Label::new(job).selectable(false)).on_hover_text(hover);
    }
}

/// The rail footer's heading and its sentence — the long half of the old legend, moved out of the
/// panel-wide band and into the rail COLUMN, which is where the approved design puts it.
const FOOTNOTE_TITLE: &str = "About these marks";
const FOOTNOTE: &str = "The three per venue are Sim · Demo · Live CREDENTIAL PRESENCE in this store — never whether a \
     venue is reachable or armed. That is the detail pane's own Status (live feed) row, from a \
     different producer.";

/// **The rail's own footer**, under the venue rows: the sentence that says what the marks are.
///
/// ⚠ **It is a node on the accessibility tree and may not become a hover.**
/// `crates/vike-connections/tests/panel/a11y_detail.rs`'s
/// `the_feed_status_is_labelled_and_an_absent_producer_is_not_unknown` reads `CREDENTIAL PRESENCE`
/// off the tree, because the half of this panel that keeps two facts apart — credential presence
/// and live feed state — is the half a hover would hide.
///
/// ⚠ **It wraps at whatever column it is given**, which is the whole move: in the two-column arm
/// that is the rail's ~190pt and the sentence is seven short lines filling the rail's own foot; in
/// the narrow arm the caller sets it in [`connections::NOTE_W`] at the FOOT of the panel. Neither is a
/// full-width band, and neither can decide how wide the window is.
pub(super) fn rail_footnote(ui: &mut egui::Ui) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(FOOTNOTE_TITLE)
                .monospace()
                .size(role_px(ui.ctx(), LEGEND_TEXT))
                .strong()
                .color(ABSENT_COLOR),
        )
        .selectable(false),
    );
    ui.add(egui::Label::new(
        egui::RichText::new(FOOTNOTE).monospace().size(role_px(ui.ctx(), LEGEND_TEXT)).weak(),
    ));
}
