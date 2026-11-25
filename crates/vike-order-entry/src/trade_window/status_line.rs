//! The status strip (spec §3.8): the order held for a confirm with Place and Cancel, or else the
//! last outcome the app composed.
//!
//! # ⚠ A held order's prompt is never cut where it matters
//!
//! The prompt is what the trader confirms, so its order (side, size, type, price) and what it
//! carries (`TP … / SL …`, or `no TP/SL`) are two LINES, never one line cut short: on one line in
//! a narrow strip the tail went first, and the tail was the price and then the exits — the one
//! thing a ladder click's prompt must say while TP/SL is ticked (a ladder click sends none). Its
//! slot is measured BEFORE the buttons are placed, and it is drawn before them, so a screen reader
//! reads the question before the answers. The strip is therefore two text lines tall at every
//! width ([`height`]), whether an order waits or not: a strip that grew whenever an order waited
//! would shift every ladder row under the pointer the moment a click was held. Where the two lines
//! do not fit beside the words
//! Place and Cancel (a strip under 304 pt), the ANSWERS give way first — ✓ and ✕, named Place and
//! Cancel — then the lines' size, to the Caption role, and last the two-line form itself: the
//! prompt's words wrap over as many lines as they take and the strip grows to hold them while that
//! order waits ([`height`]; W4 fix round 1, I-2). A held Stop whose trigger would fire at once
//! takes a third line, its warning, and grows the strip by it the same way; wrapped, its words keep
//! the warning colour (FW6, I2).
//!
//! # ⚠ A LIVE window's prompt does not look like a DEMO one's (FW6, I1)
//!
//! On a LIVE account the prompt's region is WASHED in the LIVE chip's own fill, the theme's accent
//! (the owner's ruling B, 2026-10-03), as a low tint over the background (`chip::tier_wash`: a
//! fill, so it takes no width from the words), and it says the word LIVE, in that fill's colour, at
//! the head of its first or second line wherever the word fits the form the prompt takes without
//! it: it never turns the answers into ✓ and ✕, never moves the words to the Caption role and never
//! makes the strip grow. The chip is the one source of that colour: the prompt reads
//! `chip::tier_colours`, so the two cannot drift. An account whose mode is not known is treated as
//! LIVE wherever safety is at stake: its prompt is washed too, and never CALLED live.
//!
//! # ⚠ "Ready" only when the ticket can send (FW6, I3 and I4)
//!
//! With no order waiting and no line from the app, the strip says why the ticket's Buy and Sell
//! send nothing (`ticket::standing_refusal`, the one source of those words) and "Ready" only when
//! they can. That sentence is never cut: on two lines of Body text where they hold it, else in the
//! Caption role, on as many lines as it takes, and the strip grows to hold them while it stands.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Color32, CornerRadius, FontId, Galley, Layout, Rect, RichText, UiBuilder, pos2};
use vike_model::AccountTier;
use vike_ui_theme::components::button::ThemeButtonAction;
use vike_ui_theme::components::chip;
use vike_ui_theme::components::{ThemeStatus, ThemeTokens};
use vike_ui_theme::icons;
use vike_ui_theme::maps::{self, ThemeMapRow};
use vike_ui_theme::metrics::{RADIUS, space};
use vike_ui_theme::type_scale::ThemeTextRole;
use vike_ui_theme::value::trade;

use super::describe::describe_parts;
use super::{
    TradeWindowAction, TradeWindowInputs, TradeWindowState, TradeWindowStatusLineKind,
    TradeWindowTicketKind, button_w, instrument_bar, row_h, text_w,
};

/// What the window says when the trader lets go of a held order: the strip's Cancel, and the
/// ticket's one-click padlock.
pub(super) const NOT_SENT: &str = "Not sent.";

/// How many text lines the strip holds: a held order's prompt (its order, then what it carries),
/// or a status line long enough to need a second.
const LINES: usize = 2;

/// The strip's height in a window `width` wide: `idle_height` — and, while an order waits whose
/// prompt those two lines cannot hold whole (a seven-digit price with its exits in a 260 pt window
/// at Large text), as many lines as its words wrap to; and, while nothing waits and the app gives
/// no line, as many as the ticket's refusal takes (`refusal`: a TP/SL refused on the account's lane
/// in a 320 pt window, say); either with a point above and below them.
pub fn height(
    context: &egui::Context,
    t: &ThemeTokens,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    width: f32,
) -> f32 {
    let said = match &state.held {
        Some(held) => {
            TradeWindowStatusLinePrompt::of(context, t, held, state.held_position, inputs, width)
                .height(context)
        }
        None => {
            let says = says_w(context, t, inputs, width);
            let galley = match inputs.status {
                Some(line) => Some(status_line(context, t, line, says)),
                None => refusal(context, t, state, inputs, says),
            };
            galley.map_or(0.0, |g| g.size().y)
        }
    };
    idle_height(context, t).max(said + 2.0 * space::HAIR)
}

/// The status dot's diameter: a little over half the caption's height.
fn dot_d(t: &ThemeTokens) -> f32 {
    (t.text.px(ThemeTextRole::Caption) * trade::STATUS_DOT_RATIO).round().max(trade::STATUS_DOT_MIN)
}

/// What the strip's line leaves for its words in a strip `width` wide: the dot and its gap at the left,
/// and at the right the FEED word and its gap ([`instrument_bar::feed_badge`]).
fn says_w(
    context: &egui::Context,
    t: &ThemeTokens,
    inputs: &TradeWindowInputs<'_>,
    width: f32,
) -> f32 {
    let lead = dot_d(t) + t.metrics.gap;
    let feed = if instrument_bar::feeds_in_the_strip(context, t, inputs, width) {
        instrument_bar::feed_badge_w(context, t, inputs) + t.metrics.gap
    } else {
        0.0
    };
    (width - lead - feed).max(0.0)
}

/// Draw the strip's idle form into `band`: the status dot, `says` (a closure drawing the words into
/// the room between the dot and the FEED word), and the FEED word at the right.
fn idle(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    band: Rect,
    inputs: &TradeWindowInputs<'_>,
    dot: Color32,
    says: impl FnOnce(&mut egui::Ui, Rect),
) {
    let gap = t.metrics.gap;
    let d = dot_d(t);
    let in_strip = instrument_bar::feeds_in_the_strip(ui.ctx(), t, inputs, band.width());
    let feed_w = if in_strip { instrument_bar::feed_badge_w(ui.ctx(), t, inputs) } else { 0.0 };
    let feed_gap = if in_strip { gap } else { 0.0 };
    let dot_at = pos2(band.left() + d / 2.0, band.center().y);
    ui.painter().circle_filled(dot_at, d / 2.0, dot);
    let words = Rect::from_min_max(
        pos2(band.left() + d + gap, band.min.y),
        pos2((band.right() - feed_w - feed_gap).max(band.left() + d + gap), band.max.y),
    );
    says(ui, words);
    if in_strip {
        let feed = Rect::from_min_max(pos2(band.right() - feed_w, band.min.y), band.max);
        let mut at = child(ui, feed, Layout::right_to_left(Align::Center));
        instrument_bar::feed_badge(&mut at, inputs);
    }
}

/// The strip's least height: one control row or two lines of Body text, whichever is taller, and a
/// point above and below it — what the strip is while no order waits and the ticket can send, and
/// what the height a window opens at counts (`layout::window_size`). A floor, NOT the height of
/// every strip with no order waiting: a standing refusal its two lines cannot hold whole grows it
/// while it stands ([`height`]; a TP/SL refused on a spot lane takes three Caption lines in a
/// 320 pt window and four in a 280 pt one at Standard text, FW6). `layout::window_size` does not
/// count that growth: a window opens with TP/SL off.
pub(super) fn idle_height(context: &egui::Context, t: &ThemeTokens) -> f32 {
    let two = LINES as f32 * row_h(context, &t.font(ThemeTextRole::Body));
    t.metrics.control_height.max(two) + 2.0 * space::HAIR
}

/// Why the ticket sends nothing, laid out for a strip `width` wide, or `None` while it can send:
/// `ticket::standing_refusal` for the order the ticket sends ([`TradeWindowState::entry`]), in the
/// [`TradeWindowStatusLineMessage::Refusal`] words' colour, on the strip's two lines of Body text where they hold it,
/// else in the Caption role on as many lines as it takes — never cut (FW6, I3 and I4).
fn refusal(
    context: &egui::Context,
    t: &ThemeTokens,
    state: &TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    width: f32,
) -> Option<Arc<Galley>> {
    let why =
        super::ticket::standing_refusal(state, inputs, state.entry(), state.take_profit_stop_loss)?;
    let lay = |role| {
        let format =
            TextFormat::simple(t.font(role), TradeWindowStatusLineMessage::Refusal.words(t));
        let mut job = LayoutJob::single_section(why.clone(), format);
        job.wrap.max_width = width;
        context.fonts_mut(|f| f.layout_job(job))
    };
    let body = lay(ThemeTextRole::Body);
    Some(if body.rows.len() <= LINES { body } else { lay(ThemeTextRole::Caption) })
}

/// The app's line, laid out for a strip `width` wide: up to two lines of Body text, the rest elided (its
/// whole text on hover), a venue's rejection often being longer than a narrow strip. ⚠ A refusal is never
/// cut: an error its two lines cannot hold takes the Caption role, on as many lines as it needs, and the
/// strip grows ([`height`]), because a door's refusal lists the books to choose from in its tail.
fn status_line(
    context: &egui::Context,
    t: &ThemeTokens,
    line: super::TradeWindowStatusLine<'_>,
    width: f32,
) -> Arc<Galley> {
    let colour = TradeWindowStatusLineMessage::from(line.kind).words(t);
    let lay = |role, rows| {
        let format = TextFormat::simple(t.font(role), colour);
        let mut job = LayoutJob::single_section(line.text.to_string(), format);
        job.wrap.max_width = width;
        job.wrap.max_rows = rows;
        context.fonts_mut(|f| f.layout_job(job))
    };
    let body = lay(ThemeTextRole::Body, LINES);
    if line.kind == TradeWindowStatusLineKind::Error && body.elided {
        lay(ThemeTextRole::Caption, usize::MAX)
    } else {
        body
    }
}

/// What the strip's line says, which decides the colour of its dot and of its words: each kind is a row of
/// `ui-theme.toml`'s `strip` map (`vike_ui_theme::maps::strip`), whose `colour` is the dot and whose `text`
/// is the words, so which colour a kind wears is a line of the TOML and nothing here. The line the app
/// composed is one of the first three ([`TradeWindowStatusLineKind`], `TradeWindowStatusLineMessage::from`); the other two are the strip's own
/// states, when the ticket sends nothing and when it can.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TradeWindowStatusLineMessage {
    /// A note from the app that asks nothing of the trader.
    Info,
    /// An order the venue filled.
    Ok,
    /// A failure or a refusal.
    Error,
    /// The ticket's own standing refusal, said where "Ready" would be.
    Refusal,
    /// Nothing waits and the ticket can send.
    Ready,
}

impl TradeWindowStatusLineMessage {
    /// The row of the `strip` map this message wears. Exhaustive: a message without a row does not compile.
    fn row(self) -> &'static ThemeMapRow {
        match self {
            TradeWindowStatusLineMessage::Info => &maps::strip::INFO,
            TradeWindowStatusLineMessage::Ok => &maps::strip::OK,
            TradeWindowStatusLineMessage::Error => &maps::strip::ERROR,
            TradeWindowStatusLineMessage::Refusal => &maps::strip::REFUSAL,
            TradeWindowStatusLineMessage::Ready => &maps::strip::READY,
        }
    }

    /// The status dot's colour: the row's `colour`.
    fn dot(self, t: &ThemeTokens) -> Color32 {
        self.row().colour.resolve(t)
    }

    /// The words' colour: the row's `text`.
    fn words(self, t: &ThemeTokens) -> Color32 {
        self.row().text.resolve(t)
    }
}

impl From<TradeWindowStatusLineKind> for TradeWindowStatusLineMessage {
    fn from(kind: TradeWindowStatusLineKind) -> Self {
        match kind {
            TradeWindowStatusLineKind::Info => TradeWindowStatusLineMessage::Info,
            TradeWindowStatusLineKind::Ok => TradeWindowStatusLineMessage::Ok,
            TradeWindowStatusLineKind::Error => TradeWindowStatusLineMessage::Error,
        }
    }
}

/// What a run of the prompt's words is, which decides its colour: ONE source for the colour of
/// those words in each of the prompt's forms, two lines or wrapped (FW6, I2: the wrapped form
/// painted a Stop's warning in the text colour).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TradeWindowStatusLinePromptTone {
    /// The order, and what it carries.
    Plain,
    /// A Stop's warning that it triggers at once (`ticket::stop_warning`).
    Warning,
    /// The word LIVE on a LIVE account's prompt: the LIVE chip's own fill.
    Live,
}

impl TradeWindowStatusLinePromptTone {
    fn colour(self, t: &ThemeTokens) -> Color32 {
        match self {
            TradeWindowStatusLinePromptTone::Plain => t.theme.text,
            TradeWindowStatusLinePromptTone::Warning => ThemeStatus::Warning.color(),
            TradeWindowStatusLinePromptTone::Live => chip::tier_colours(AccountTier::Live, t).0,
        }
    }
}

/// A line of a prompt's words — its runs of one tone each, read one after the other — as read.
fn said(line: &[(String, TradeWindowStatusLinePromptTone)]) -> String {
    line.iter().map(|(run, _)| run.as_str()).collect()
}

/// How a held order's prompt is laid out in a strip `width` wide — measured, never cut.
///
/// Two lines, its order then what it carries, beside the words Place and Cancel where both fit;
/// else beside ✓ and ✕ (named Place and Cancel); else in the Caption role; else its words wrapped
/// over as many Caption lines as they take, and the strip grows to hold them ([`height`]) while that
/// order waits. The last is rare (a seven-digit price with its exits, Large text, a window under
/// 280 pt) and is the one time a held order moves the window's other regions; a ladder there loses
/// rows at its foot. A press held across it cannot land on another button: the ticket's order
/// buttons are keyed on what they do (`super::keyed`, beside `draw` in the window's own module).
///
/// A held Stop whose trigger is on the wrong side of the market NOW adds a line, the very words the
/// ticket wrote before the click (`ticket::stop_warning`; the owner's decision of 2026-10-03,
/// round 2, item 9), and the strip grows by it while that order waits. Its words keep the warning
/// colour in every form, wrapped too ([`TradeWindowStatusLinePromptTone`]; FW6, I2). What the node says of the order
/// ([`TradeWindowInputs::held_says`]) is a line the same way, whole however long: a door's refusal lists
/// the books to choose from in its tail.
///
/// On a LIVE account the prompt says the word LIVE (FW6, I1) in the form the prompt takes without
/// it — beside the same answers, in the same text role, in a strip no taller — so the word moves
/// nothing: in front of its first line where that line still fits, else in front of its second
/// (what the order carries), else, for a one-line prompt (a Close, a Reverse), on a line of its
/// own above it, which the strip's two lines hold; and in the wrapped form as its first word, where
/// the words then wrap to no more lines. Where it fits none of these, the word is left out and the
/// wash alone marks the prompt. (A first version let the word move the answers to ✓ and ✕, which
/// the look at 320 pt showed beside a DEMO prompt that kept its words.)
struct TradeWindowStatusLinePrompt {
    /// ✓ and ✕ in place of the words.
    icons: bool,
    font: FontId,
    /// Each line's words, in runs of one tone each.
    lines: Vec<Vec<(String, TradeWindowStatusLinePromptTone)>>,
}

impl TradeWindowStatusLinePrompt {
    /// The prompt for `held` — a Close or a Reverse asked against a position of signed size
    /// `position` (`TradeWindowState`'s record of it) — in the window `inputs` draws.
    fn of(
        context: &egui::Context,
        t: &ThemeTokens,
        held: &TradeWindowAction,
        position: Option<f64>,
        inputs: &TradeWindowInputs<'_>,
        width: f32,
    ) -> TradeWindowStatusLinePrompt {
        let (order, carries) = describe_parts(held, inputs.grid, position);
        let warning = match held {
            TradeWindowAction::Place {
                side,
                order_type: TradeWindowTicketKind::Stop,
                price: Some(p),
                ..
            } => super::ticket::stop_warning(*side, *p, inputs),
            _ => None,
        };
        let plain = |words: String| vec![(words, TradeWindowStatusLinePromptTone::Plain)];
        let mut lines = match &carries {
            Some(c) => vec![plain(format!("{order},")), plain(format!("{c}?"))],
            None => vec![plain(format!("{order}?"))],
        };
        lines.extend(warning.map(|w| vec![(w, TradeWindowStatusLinePromptTone::Warning)]));
        lines.extend(
            inputs
                .held_says
                .map(|says| vec![(says.to_string(), TradeWindowStatusLinePromptTone::Warning)]),
        );
        let two = TradeWindowStatusLinePrompt::beside(context, t, &lines, width);
        if inputs.mode != Some(AccountTier::Live) {
            return two.unwrap_or_else(|| {
                TradeWindowStatusLinePrompt::wrapped(context, t, &lines, width)
            });
        }
        // ⚠ LIVE: the word, in the form the prompt takes without it, wherever it fits there.
        let word = chip::tier_label(AccountTier::Live);
        let front = |at: usize| {
            let mut fronted = lines.clone();
            fronted[at].insert(0, (format!("{word} "), TradeWindowStatusLinePromptTone::Live));
            fronted
        };
        let fronted = front(0);
        match two {
            Some(two) => {
                // The first line, else the second (what the order carries), else, above a
                // one-line prompt, a line of its own.
                let mut tries = vec![fronted];
                if lines.get(1).is_some_and(|l| {
                    l.iter().all(|(_, tone)| *tone == TradeWindowStatusLinePromptTone::Plain)
                }) {
                    tries.push(front(1));
                }
                if lines.len() < LINES {
                    let mut above =
                        vec![vec![(word.to_string(), TradeWindowStatusLinePromptTone::Live)]];
                    above.extend(lines.iter().cloned());
                    tries.push(above);
                }
                let fit = |l: &Vec<_>| {
                    TradeWindowStatusLinePrompt::fits(context, t, l, two.icons, &two.font, width)
                };
                match tries.into_iter().find(fit) {
                    Some(lines) => TradeWindowStatusLinePrompt { lines, ..two },
                    None => two,
                }
            }
            None => {
                let without = TradeWindowStatusLinePrompt::wrapped(context, t, &lines, width);
                let with = TradeWindowStatusLinePrompt::wrapped(context, t, &fronted, width);
                if with.lines.len() <= without.lines.len() { with } else { without }
            }
        }
    }

    /// The first of the two-line forms — beside the words Place and Cancel in the Body role, beside
    /// ✓ and ✕ in the Body role, beside ✓ and ✕ in the Caption role — that holds every one of
    /// `lines` whole; `None` where none does.
    fn beside(
        context: &egui::Context,
        t: &ThemeTokens,
        lines: &[Vec<(String, TradeWindowStatusLinePromptTone)>],
        width: f32,
    ) -> Option<TradeWindowStatusLinePrompt> {
        let (body, caption) = (t.font(ThemeTextRole::Body), t.font(ThemeTextRole::Caption));
        [(false, body.clone()), (true, body), (true, caption)]
            .into_iter()
            .find(|(icons, font)| {
                TradeWindowStatusLinePrompt::fits(context, t, lines, *icons, font, width)
            })
            .map(|(icons, font)| TradeWindowStatusLinePrompt { icons, font, lines: lines.to_vec() })
    }

    /// Whether every one of `lines` fits, in `font`, beside the answers (✓ and ✕ where `icons`).
    fn fits(
        context: &egui::Context,
        t: &ThemeTokens,
        lines: &[Vec<(String, TradeWindowStatusLinePromptTone)>],
        icons: bool,
        font: &FontId,
        width: f32,
    ) -> bool {
        let room = width - answers_w(context, t, icons) - t.metrics.gap;
        lines.iter().all(|l| text_w(context, &said(l), font, t) <= room)
    }

    /// `lines`' words, whole, as many to a line as fit beside ✓ and ✕ in the Caption role, each
    /// word in the tone it had: the order's, the warning's, LIVE's.
    fn wrapped(
        context: &egui::Context,
        t: &ThemeTokens,
        lines: &[Vec<(String, TradeWindowStatusLinePromptTone)>],
        width: f32,
    ) -> TradeWindowStatusLinePrompt {
        let caption = t.font(ThemeTextRole::Caption);
        let room = width - answers_w(context, t, true) - t.metrics.gap;
        let words = lines
            .iter()
            .flatten()
            .flat_map(|(run, tone)| run.split_whitespace().map(move |w| (w, *tone)));
        let mut out: Vec<Vec<(String, TradeWindowStatusLinePromptTone)>> = Vec::new();
        for (word, tone) in words {
            match out.last_mut() {
                Some(line)
                    if text_w(context, &format!("{} {word}", said(line)), &caption, t) <= room =>
                {
                    match line.last_mut() {
                        Some((run, last)) if *last == tone => {
                            run.push(' ');
                            run.push_str(word);
                        }
                        _ => line.push((format!(" {word}"), tone)),
                    }
                }
                _ => out.push(vec![(word.to_string(), tone)]),
            }
        }
        TradeWindowStatusLinePrompt { icons: true, font: caption, lines: out }
    }

    /// How tall its lines are.
    fn height(&self, context: &egui::Context) -> f32 {
        self.lines.len() as f32 * row_h(context, &self.font)
    }
}

/// How wide the answers are drawn, a gap apart: the words, or ✓ and ✕.
fn answers_w(context: &egui::Context, t: &ThemeTokens, icons: bool) -> f32 {
    let gap = t.metrics.gap;
    if icons {
        let glyph = egui::FontId::new(t.text.px(ThemeTextRole::Strong), icons::family());
        let icon_w = |i: icons::ThemeIcon| {
            text_w(context, i.rich().text(), &glyph, t) + 2.0 * t.metrics.padding
        };
        icon_w(icons::CHECK) + gap + icon_w(icons::CLOSE)
    } else {
        button_w(context, t, PLACE) + gap + button_w(context, t, CANCEL)
    }
}

/// Draw the strip into `ui`, the band [`height`] sized. Public so it is live code before the
/// window's `draw` calls it.
pub fn strip(
    ui: &mut egui::Ui,
    t: &ThemeTokens,
    state: &mut TradeWindowState,
    inputs: &TradeWindowInputs<'_>,
    actions: &mut Vec<TradeWindowAction>,
) {
    let band = ui.available_rect_before_wrap();
    let gap = t.metrics.gap;
    if let Some(held) = state.held.clone() {
        // The answers' slot first, measured, so the prompt's is what is left of it — and the
        // answers give way before the prompt does ([`TradeWindowStatusLinePrompt`]; W4 fix round 1, I-2).
        let TradeWindowStatusLinePrompt { icons: as_icons, font, lines } =
            TradeWindowStatusLinePrompt::of(
                ui.ctx(),
                t,
                &held,
                state.held_position,
                inputs,
                band.width(),
            );
        let split = (band.max.x - answers_w(ui.ctx(), t, as_icons)).max(band.min.x);
        let ask = Rect::from_min_max(band.min, pos2((split - gap).max(band.min.x), band.max.y));
        let mut prompt = child(ui, ask, Layout::top_down(Align::Min));
        // ⚠ A LIVE account's prompt — and one whose mode nobody reported, treated as LIVE wherever
        // safety is at stake — is washed in the LIVE chip's own fill: a fill under the words, so
        // it takes none of their room (FW6, I1).
        if matches!(inputs.mode, Some(AccountTier::Live) | None) {
            let wash = chip::tier_wash(AccountTier::Live, t);
            prompt.painter().rect_filled(ask, CornerRadius::same(RADIUS), wash);
        }
        prompt.spacing_mut().item_spacing.y = space::NONE;
        let line_h = row_h(ui.ctx(), &font);
        prompt.add_space(((band.height() - lines.len() as f32 * line_h) / 2.0).max(0.0));
        for line in lines {
            let mut words = LayoutJob::default();
            for (run, tone) in &line {
                words.append(run, 0.0, TextFormat::simple(font.clone(), tone.colour(t)));
            }
            prompt.add(egui::Label::new(words).truncate());
        }
        let answers = Rect::from_min_max(pos2(split, band.min.y), band.max);
        let mut answers = child(ui, answers, Layout::left_to_right(Align::Center));
        answers.spacing_mut().item_spacing.x = gap;
        let key = ui.scope_id();
        let (place, cancel) = if as_icons {
            (ThemeButtonAction::primary(icons::CHECK), ThemeButtonAction::secondary(icons::CLOSE))
        } else {
            (ThemeButtonAction::primary(PLACE), ThemeButtonAction::secondary(CANCEL))
        };
        let placed = answer(&mut answers, key.with("place"), place, as_icons.then_some(PLACE));
        if placed.clicked() {
            actions.push(held);
            state.held = None;
        }
        let cancelled =
            answer(&mut answers, key.with("cancel"), cancel, as_icons.then_some(CANCEL));
        if cancelled.clicked() {
            state.held = None;
            actions.push(TradeWindowAction::Note {
                kind: TradeWindowStatusLineKind::Info,
                text: NOT_SENT.to_string(),
            });
        }
    } else if let Some(line) = inputs.status {
        let message = TradeWindowStatusLineMessage::from(line.kind);
        idle(ui, t, band, inputs, message.dot(t), |ui, words| {
            let galley = status_line(ui.ctx(), t, line, words.width());
            let mut says = child(ui, words, Layout::top_down(Align::Min));
            says.add_space(((band.height() - galley.size().y) / 2.0).max(0.0));
            says.add(egui::Label::new(galley));
        });
    } else if let Some(galley) =
        refusal(ui.ctx(), t, state, inputs, says_w(ui.ctx(), t, inputs, band.width()))
    {
        // Why the ticket sends nothing, whole, where "Ready" would have said it can (FW6, I3, I4).
        idle(ui, t, band, inputs, TradeWindowStatusLineMessage::Refusal.dot(t), |ui, words| {
            let mut says = child(ui, words, Layout::top_down(Align::Min));
            says.add_space(((band.height() - galley.size().y) / 2.0).max(0.0));
            says.add(egui::Label::new(galley));
        });
    } else {
        let words = if state.one_click {
            "Ready · one-click trading is on"
        } else {
            "Ready · every order asks first"
        };
        idle(ui, t, band, inputs, TradeWindowStatusLineMessage::Ready.dot(t), |ui, room| {
            let mut says = child(ui, room, Layout::left_to_right(Align::Center));
            says.add(
                egui::Label::new(
                    RichText::new(words)
                        .font(t.font(ThemeTextRole::Body))
                        .color(TradeWindowStatusLineMessage::Ready.words(t)),
                )
                .truncate(),
            );
        });
    }
}

/// The answers' words, and the names the icon answers carry.
const PLACE: &str = "Place";
const CANCEL: &str = "Cancel";

/// Add an answer under `key` (the ticket's `keyed` rule: a click belongs to what the button does,
/// not to where it sits), named `name` when it shows only an icon.
fn answer(
    ui: &mut egui::Ui,
    key: egui::Id,
    button: ThemeButtonAction<'_>,
    name: Option<&str>,
) -> egui::Response {
    let r = ui.scope_builder(UiBuilder::new().scope_id(key), |ui| ui.add(button)).inner;
    match name {
        Some(words) => icons::named(r, words),
        None => r,
    }
}

/// A child of `ui` laid out in `rect` and clipped to it.
fn child(ui: &mut egui::Ui, rect: Rect, layout: Layout) -> egui::Ui {
    let mut c = ui.new_child(UiBuilder::new().max_rect(rect).layout(layout));
    c.shrink_clip_rect(rect);
    c
}

#[path = "status_line_tests.rs"]
#[cfg(test)]
mod status_line_tests;
