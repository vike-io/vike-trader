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
//! (the owner's ruling B, 2026-10-03), as a low tint over the background (`chip::Mode::wash`: a
//! fill, so it takes no width from the words), and it says the word LIVE, in that fill's colour, at
//! the head of its first or second line wherever the word fits the form the prompt takes without
//! it: it never turns the answers into ✓ and ✕, never moves the words to the Caption role and never
//! makes the strip grow. The chip is the one source of that colour: the prompt reads
//! `chip::Mode::colours`, so the two cannot drift. An account whose mode is not known is treated as
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
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::chip::Mode;
use vike_ui_theme::components::{Status, Tokens};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::RADIUS;
use vike_ui_theme::type_scale::TextRole;

use super::{
    AccountMode, OrderType, StatusKind, TradeAction, TradeInputs, TradeState, button_w,
    describe_parts, instrument, row_h, text_w,
};

/// What the window says when the trader lets go of a held order: the strip's Cancel, and the
/// ticket's one-click padlock.
pub(super) const NOT_SENT: &str = "Not sent.";

/// How many text lines the strip holds: a held order's prompt (its order, then what it carries),
/// or a status line long enough to need a second.
const LINES: usize = 2;

/// The strip's height in a window `width` wide: [`idle_height`] — and, while an order waits whose
/// prompt those two lines cannot hold whole (a seven-digit price with its exits in a 260 pt window
/// at Large text), as many lines as its words wrap to; and, while nothing waits and the app gives
/// no line, as many as the ticket's refusal takes (`refusal`: a TP/SL refused on the account's lane
/// in a 320 pt window, say); either with a point above and below them.
pub fn height(
    ctx: &egui::Context,
    t: &Tokens,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    width: f32,
) -> f32 {
    let said = match &state.held {
        Some(held) => Ask::of(ctx, t, held, state.held_position, inputs, width).height(ctx),
        None if inputs.status.is_none() => {
            refusal(ctx, t, state, inputs, says_w(ctx, t, inputs, width))
                .map_or(0.0, |g| g.size().y)
        }
        None => 0.0,
    };
    idle_height(ctx, t).max(said + 2.0)
}

/// The status dot's diameter: a little over half the caption's height.
fn dot_d(t: &Tokens) -> f32 {
    (t.text.px(TextRole::Caption) * 0.6).round().max(5.0)
}

/// What the strip's line leaves for its words in a strip `width` wide: the dot and its gap at the left,
/// and at the right the FEED word and its gap ([`instrument::feed_badge`]).
fn says_w(ctx: &egui::Context, t: &Tokens, inputs: &TradeInputs<'_>, width: f32) -> f32 {
    let lead = dot_d(t) + t.metrics.gap;
    let feed = if instrument::feeds_in_the_strip(ctx, t, inputs, width) {
        instrument::feed_badge_w(ctx, t, inputs) + t.metrics.gap
    } else {
        0.0
    };
    (width - lead - feed).max(0.0)
}

/// Draw the strip's idle form into `band`: the status dot, `says` (a closure drawing the words into
/// the room between the dot and the FEED word), and the FEED word at the right.
fn idle(
    ui: &mut egui::Ui,
    t: &Tokens,
    band: Rect,
    inputs: &TradeInputs<'_>,
    dot: Color32,
    says: impl FnOnce(&mut egui::Ui, Rect),
) {
    let gap = t.metrics.gap;
    let d = dot_d(t);
    let in_strip = instrument::feeds_in_the_strip(ui.ctx(), t, inputs, band.width());
    let feed_w = if in_strip { instrument::feed_badge_w(ui.ctx(), t, inputs) } else { 0.0 };
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
        instrument::feed_badge(&mut at, inputs);
    }
}

/// The strip's least height: one control row or two lines of Body text, whichever is taller, and a
/// point above and below it — what the strip is while no order waits and the ticket can send, and
/// what the height a window opens at counts (`layout::window_size`). A floor, NOT the height of
/// every strip with no order waiting: a standing refusal its two lines cannot hold whole grows it
/// while it stands ([`height`]; a TP/SL refused on a spot lane takes three Caption lines in a
/// 320 pt window and four in a 280 pt one at Standard text, FW6). `layout::window_size` does not
/// count that growth: a window opens with TP/SL off.
pub(super) fn idle_height(ctx: &egui::Context, t: &Tokens) -> f32 {
    let two = LINES as f32 * row_h(ctx, &t.font(TextRole::Body));
    t.metrics.control_h.max(two) + 2.0
}

/// Why the ticket sends nothing, laid out for a strip `width` wide, or `None` while it can send:
/// `ticket::standing_refusal` for the order the ticket sends ([`TradeState::entry`]), in the
/// strip's Info colour, on the strip's two lines of Body text where they hold it, else in the
/// Caption role on as many lines as it takes — never cut (FW6, I3 and I4).
fn refusal(
    ctx: &egui::Context,
    t: &Tokens,
    state: &TradeState,
    inputs: &TradeInputs<'_>,
    width: f32,
) -> Option<Arc<Galley>> {
    let why = super::ticket::standing_refusal(state, inputs, state.entry(), state.tpsl)?;
    let lay = |role| {
        let format = TextFormat::simple(t.font(role), t.theme.text2);
        let mut job = LayoutJob::single_section(why.clone(), format);
        job.wrap.max_width = width;
        ctx.fonts_mut(|f| f.layout_job(job))
    };
    let body = lay(TextRole::Body);
    Some(if body.rows.len() <= LINES { body } else { lay(TextRole::Caption) })
}

/// What a run of the prompt's words is, which decides its colour: ONE source for the colour of
/// those words in each of the prompt's forms, two lines or wrapped (FW6, I2: the wrapped form
/// painted a Stop's warning in the text colour).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    /// The order, and what it carries.
    Plain,
    /// A Stop's warning that it triggers at once (`ticket::stop_warning`).
    Warning,
    /// The word LIVE on a LIVE account's prompt: the LIVE chip's own fill.
    Live,
}

impl Tone {
    fn colour(self, t: &Tokens) -> Color32 {
        match self {
            Tone::Plain => t.theme.text,
            Tone::Warning => Status::Warning.color(),
            Tone::Live => Mode::Live.colours(t).0,
        }
    }
}

/// A line of a prompt's words — its runs of one tone each, read one after the other — as read.
fn said(line: &[(String, Tone)]) -> String {
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
/// colour in every form, wrapped too ([`Tone`]; FW6, I2).
///
/// On a LIVE account the prompt says the word LIVE (FW6, I1) in the form the prompt takes without
/// it — beside the same answers, in the same text role, in a strip no taller — so the word moves
/// nothing: in front of its first line where that line still fits, else in front of its second
/// (what the order carries), else, for a one-line prompt (a Close, a Reverse), on a line of its
/// own above it, which the strip's two lines hold; and in the wrapped form as its first word, where
/// the words then wrap to no more lines. Where it fits none of these, the word is left out and the
/// wash alone marks the prompt. (A first version let the word move the answers to ✓ and ✕, which
/// the look at 320 pt showed beside a DEMO prompt that kept its words.)
struct Ask {
    /// ✓ and ✕ in place of the words.
    icons: bool,
    font: FontId,
    /// Each line's words, in runs of one tone each.
    lines: Vec<Vec<(String, Tone)>>,
}

impl Ask {
    /// The prompt for `held` — a Close or a Reverse asked against a position of signed size
    /// `position` (`TradeState`'s record of it) — in the window `inputs` draws.
    fn of(
        ctx: &egui::Context,
        t: &Tokens,
        held: &TradeAction,
        position: Option<f64>,
        inputs: &TradeInputs<'_>,
        width: f32,
    ) -> Ask {
        let (order, carries) = describe_parts(held, inputs.grid, position);
        let warning = match held {
            TradeAction::Place { side, order_type: OrderType::Stop, price: Some(p), .. } => {
                super::ticket::stop_warning(*side, *p, inputs)
            }
            _ => None,
        };
        let plain = |words: String| vec![(words, Tone::Plain)];
        let mut lines = match &carries {
            Some(c) => vec![plain(format!("{order},")), plain(format!("{c}?"))],
            None => vec![plain(format!("{order}?"))],
        };
        lines.extend(warning.map(|w| vec![(w, Tone::Warning)]));
        let two = Ask::beside(ctx, t, &lines, width);
        if inputs.mode != AccountMode::Live {
            return two.unwrap_or_else(|| Ask::wrapped(ctx, t, &lines, width));
        }
        // ⚠ LIVE: the word, in the form the prompt takes without it, wherever it fits there.
        let word = Mode::Live.label();
        let front = |at: usize| {
            let mut fronted = lines.clone();
            fronted[at].insert(0, (format!("{word} "), Tone::Live));
            fronted
        };
        let fronted = front(0);
        match two {
            Some(two) => {
                // The first line, else the second (what the order carries), else, above a
                // one-line prompt, a line of its own.
                let mut tries = vec![fronted];
                if lines.get(1).is_some_and(|l| l.iter().all(|(_, tone)| *tone == Tone::Plain)) {
                    tries.push(front(1));
                }
                if lines.len() < LINES {
                    let mut above = vec![vec![(word.to_string(), Tone::Live)]];
                    above.extend(lines.iter().cloned());
                    tries.push(above);
                }
                let fit = |l: &Vec<_>| Ask::fits(ctx, t, l, two.icons, &two.font, width);
                match tries.into_iter().find(fit) {
                    Some(lines) => Ask { lines, ..two },
                    None => two,
                }
            }
            None => {
                let without = Ask::wrapped(ctx, t, &lines, width);
                let with = Ask::wrapped(ctx, t, &fronted, width);
                if with.lines.len() <= without.lines.len() { with } else { without }
            }
        }
    }

    /// The first of the two-line forms — beside the words Place and Cancel in the Body role, beside
    /// ✓ and ✕ in the Body role, beside ✓ and ✕ in the Caption role — that holds every one of
    /// `lines` whole; `None` where none does.
    fn beside(
        ctx: &egui::Context,
        t: &Tokens,
        lines: &[Vec<(String, Tone)>],
        width: f32,
    ) -> Option<Ask> {
        let (body, caption) = (t.font(TextRole::Body), t.font(TextRole::Caption));
        [(false, body.clone()), (true, body), (true, caption)]
            .into_iter()
            .find(|(icons, font)| Ask::fits(ctx, t, lines, *icons, font, width))
            .map(|(icons, font)| Ask { icons, font, lines: lines.to_vec() })
    }

    /// Whether every one of `lines` fits, in `font`, beside the answers (✓ and ✕ where `icons`).
    fn fits(
        ctx: &egui::Context,
        t: &Tokens,
        lines: &[Vec<(String, Tone)>],
        icons: bool,
        font: &FontId,
        width: f32,
    ) -> bool {
        let room = width - answers_w(ctx, t, icons) - t.metrics.gap;
        lines.iter().all(|l| text_w(ctx, &said(l), font, t) <= room)
    }

    /// `lines`' words, whole, as many to a line as fit beside ✓ and ✕ in the Caption role, each
    /// word in the tone it had: the order's, the warning's, LIVE's.
    fn wrapped(ctx: &egui::Context, t: &Tokens, lines: &[Vec<(String, Tone)>], width: f32) -> Ask {
        let caption = t.font(TextRole::Caption);
        let room = width - answers_w(ctx, t, true) - t.metrics.gap;
        let words = lines
            .iter()
            .flatten()
            .flat_map(|(run, tone)| run.split_whitespace().map(move |w| (w, *tone)));
        let mut out: Vec<Vec<(String, Tone)>> = Vec::new();
        for (word, tone) in words {
            match out.last_mut() {
                Some(line)
                    if text_w(ctx, &format!("{} {word}", said(line)), &caption, t) <= room =>
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
        Ask { icons: true, font: caption, lines: out }
    }

    /// How tall its lines are.
    fn height(&self, ctx: &egui::Context) -> f32 {
        self.lines.len() as f32 * row_h(ctx, &self.font)
    }
}

/// How wide the answers are drawn, a gap apart: the words, or ✓ and ✕.
fn answers_w(ctx: &egui::Context, t: &Tokens, icons: bool) -> f32 {
    let gap = t.metrics.gap;
    if icons {
        let glyph = egui::FontId::new(t.text.px(TextRole::Strong), icons::family());
        let icon_w = |i: icons::Icon| text_w(ctx, i.rich().text(), &glyph, t) + 2.0 * t.metrics.pad;
        icon_w(icons::CHECK) + gap + icon_w(icons::CLOSE)
    } else {
        button_w(ctx, t, PLACE) + gap + button_w(ctx, t, CANCEL)
    }
}

/// Draw the strip into `ui`, the band [`height`] sized. Public so it is live code before the
/// window's `draw` calls it.
pub fn strip(
    ui: &mut egui::Ui,
    t: &Tokens,
    state: &mut TradeState,
    inputs: &TradeInputs<'_>,
    actions: &mut Vec<TradeAction>,
) {
    let band = ui.available_rect_before_wrap();
    let gap = t.metrics.gap;
    if let Some(held) = state.held.clone() {
        // The answers' slot first, measured, so the prompt's is what is left of it — and the
        // answers give way before the prompt does ([`Ask`]; W4 fix round 1, I-2).
        let Ask { icons: as_icons, font, lines } =
            Ask::of(ui.ctx(), t, &held, state.held_position, inputs, band.width());
        let split = (band.max.x - answers_w(ui.ctx(), t, as_icons)).max(band.min.x);
        let ask = Rect::from_min_max(band.min, pos2((split - gap).max(band.min.x), band.max.y));
        let mut prompt = child(ui, ask, Layout::top_down(Align::Min));
        // ⚠ A LIVE account's prompt — and one whose mode nobody reported, treated as LIVE wherever
        // safety is at stake — is washed in the LIVE chip's own fill: a fill under the words, so
        // it takes none of their room (FW6, I1).
        if matches!(inputs.mode, AccountMode::Live | AccountMode::Unknown) {
            let wash = Mode::Live.wash(t);
            prompt.painter().rect_filled(ask, CornerRadius::same(RADIUS), wash);
        }
        prompt.spacing_mut().item_spacing.y = 0.0;
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
        let key = ui.id();
        let (place, cancel) = if as_icons {
            (ActionButton::primary(icons::CHECK), ActionButton::secondary(icons::CLOSE))
        } else {
            (ActionButton::primary(PLACE), ActionButton::secondary(CANCEL))
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
            actions.push(TradeAction::Note { kind: StatusKind::Info, text: NOT_SENT.to_string() });
        }
    } else if let Some(line) = inputs.status {
        let (col, dot) = match line.kind {
            StatusKind::Info => (t.theme.text2, t.theme.text3),
            StatusKind::Ok => (Status::Ok.color(), Status::Ok.color()),
            StatusKind::Error => (Status::Error.color(), Status::Error.color()),
        };
        idle(ui, t, band, inputs, dot, |ui, words| {
            // Up to two lines, the rest elided (its whole text on hover): a venue's rejection is
            // often longer than a narrow strip, and its cause is its tail.
            let mut job = LayoutJob::single_section(
                line.text.to_string(),
                TextFormat::simple(t.font(TextRole::Body), col),
            );
            job.wrap.max_width = words.width();
            job.wrap.max_rows = LINES;
            let galley = ui.painter().layout_job(job);
            let mut says = child(ui, words, Layout::top_down(Align::Min));
            says.add_space(((band.height() - galley.size().y) / 2.0).max(0.0));
            says.add(egui::Label::new(galley));
        });
    } else if let Some(galley) =
        refusal(ui.ctx(), t, state, inputs, says_w(ui.ctx(), t, inputs, band.width()))
    {
        // Why the ticket sends nothing, whole, where "Ready" would have said it can (FW6, I3, I4).
        idle(ui, t, band, inputs, Status::Warning.color(), |ui, words| {
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
        idle(ui, t, band, inputs, t.theme.text3, |ui, room| {
            let mut says = child(ui, room, Layout::left_to_right(Align::Center));
            says.add(
                egui::Label::new(
                    RichText::new(words).font(t.font(TextRole::Body)).color(t.theme.text2),
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
    button: ActionButton<'_>,
    name: Option<&str>,
) -> egui::Response {
    let r = ui.scope_builder(UiBuilder::new().id(key), |ui| ui.add(button)).inner;
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

#[cfg(test)]
mod tests {
    use egui::accesskit::Role;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use vike_model::{L2Book, VenueCaps};
    use vike_ui_theme::appearance::Appearance;
    use vike_ui_theme::metrics::Density;
    use vike_ui_theme::type_scale::TextSize;

    use super::*;
    use crate::trade::{AccountMode, Grid, OrderType, Origin, Tradable};

    struct Fixture {
        state: TradeState,
        emitted: Vec<TradeAction>,
    }

    fn buy() -> TradeAction {
        TradeAction::Place {
            side: 1,
            order_type: OrderType::Limit,
            price: Some(99.5),
            qty: 0.01,
            reduce_only: false,
            exits: None,
            origin: Origin::Ticket,
        }
    }

    /// The strip alone, a 320 pt window's width, holding `held` for a confirm.
    fn harness(held: TradeAction) -> Harness<'static, Fixture> {
        harness_in(Appearance::default(), held, 320.0)
    }

    /// [`harness`] under the appearance `look`, in a window `width` wide (the strip 16 pt narrower).
    fn harness_in(look: Appearance, held: TradeAction, width: f32) -> Harness<'static, Fixture> {
        harness_with(look, Some(held), width, "")
    }

    /// The strip under `look`, `width` wide, holding `held` (or nothing) with the depth link `source`.
    fn harness_with(
        look: Appearance,
        held: Option<TradeAction>,
        width: f32,
        source: &'static str,
    ) -> Harness<'static, Fixture> {
        let book = L2Book::new(0.1);
        let state = TradeState { held, ..TradeState::default() };
        Harness::builder().with_size(egui::vec2(width, 120.0)).build_ui_state(
            move |ui, f: &mut Fixture| {
                if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &look) {
                    return;
                }
                let inputs = TradeInputs {
                    venue: "binance",
                    venue_label: "Binance",
                    product: "",
                    account: None,
                    symbol: "BTCUSDT",
                    base: "BTC",
                    quote: "USDT",
                    mode: AccountMode::Live,
                    tradable: Tradable::Yes,
                    grid: Grid { tick: 0.1, lot: 0.001, min_qty: 0.001 },
                    book: &book,
                    last: Some(99.5),
                    stale: false,
                    source,
                    absence: None,
                    orders: &[],
                    orders_why: None,
                    position: None,
                    buying_power: None,
                    caps: VenueCaps::UNSUPPORTED,
                    bracket_why: None,
                    bracket_wire: false,
                    matches: &[],
                    recent: &[],
                    accounts: &[],
                    accounts_why: None,
                    unconnected: &[],
                    tape: &[],
                    status: None,
                };
                let t = Tokens::of(ui.ctx());
                strip(ui, &t, &mut f.state, &inputs, &mut f.emitted);
            },
            Fixture { state, emitted: Vec::new() },
        )
    }

    /// The window names its own depth link at the right end of an idle strip (the v3 design's bar
    /// carries only the price): a link nobody reported on reads `FEED ?`, never `FEED DOWN`, and a
    /// live one `FEED UP`. A held order's prompt takes the strip's room instead (Place and Cancel
    /// sit where the word would).
    #[test]
    fn an_idle_strip_names_the_depth_link_and_a_held_one_does_not() {
        let unknown = harness_with(Appearance::default(), None, 600.0, "");
        let mut unknown = unknown;
        unknown.run();
        assert!(unknown.query_by_label("FEED ?").is_some());
        assert!(unknown.query_by_label("FEED DOWN").is_none());
        let mut live = harness_with(
            Appearance::default(),
            None,
            600.0,
            "datahub 127.0.0.1:7878 — 1/1 stream(s) live",
        );
        live.run();
        assert!(live.query_by_label("FEED UP").is_some());
        let mut held = harness_with(Appearance::default(), Some(buy()), 600.0, "");
        held.run();
        assert!(held.query_by_label_contains("FEED").is_none(), "the prompt has the room");
    }

    /// A strip too narrow to carry the word leaves it to the bar's last row ([`feeds_in_the_strip`]).
    #[test]
    fn a_narrow_strip_leaves_the_feed_word_to_the_bar() {
        let mut h = harness_with(Appearance::default(), None, 240.0, "");
        h.run();
        assert!(h.query_by_label_contains("FEED").is_none());
    }

    /// Place sends the held order exactly as it was held, and the strip lets go of it.
    #[test]
    fn place_sends_the_held_order_once() {
        let mut h = harness(buy());
        h.run();
        assert!(h.query_by_label_contains("Buy").is_some(), "the prompt names the order");
        h.get_by_label("Place").click();
        h.run();
        assert_eq!(h.state().emitted, [buy()]);
        assert!(h.state().state.held.is_none());
    }

    /// W2 review minor 2, the W3 fix-round re-review and W4 fix round 1 (I-2): at every strip a
    /// supported window gives — 244 pt (the fit sweep's 260 pt window), 264 pt (the ticket-alone
    /// window's 280) and 304 pt (the bottom-panel window's 320) — in every density and text size, a
    /// held order's prompt is cut NOWHERE: not its side, size or price, and not what it carries
    /// (`TP … / SL …`, a ticket order's protection, or `no TP/SL`, the one thing a ladder click's
    /// prompt must say while TP/SL is ticked). Read off what is PAINTED: a galley the kit elided
    /// carries `elided`, where the accessibility tree would still hold the whole text. And the
    /// prompt is read BEFORE its answers.
    #[test]
    fn a_held_prompt_keeps_its_price_and_its_exits_at_every_strip_width() {
        let place = |price, exits| TradeAction::Place {
            side: -1,
            order_type: OrderType::Limit,
            price: Some(price),
            qty: 0.012,
            reduce_only: false,
            exits,
            origin: Origin::Ladder,
        };
        let exits = |take_profit, stop_loss| Some(crate::trade::Exits { take_profit, stop_loss });
        // The fit scene's price with its exits and without, and an index's eight-digit price with
        // its exits — the prompt two lines cannot hold whole in a narrow strip at Large text.
        let prompts = [
            (65_432.1, exits(65_105.9, 65_759.3), "65,432.1", "TP 65,105.9 / SL 65,759.3"),
            (65_432.1, None, "65,432.1", "no TP/SL"),
            (
                12_345_678.9,
                exits(12_283_950.5, 12_382_715.9),
                "12,345,678.9",
                "TP 12,283,950.5 / SL 12,382,715.9",
            ),
        ];
        let mut cut = Vec::new();
        for strip_w in [244.0, 264.0, 304.0] {
            for density in Density::ALL {
                for text_size in TextSize::ALL {
                    let look = Appearance { density, text_size, ..Appearance::default() };
                    for (price, exits, priced, carries) in prompts {
                        let mut h = harness_in(look, place(price, exits), strip_w + 16.0);
                        h.run();
                        let what = format!("{strip_w} pt, {density:?}/{text_size:?}");
                        for c in &h.output().shapes {
                            if let egui::Shape::Text(t) = &c.shape
                                && t.galley.elided
                            {
                                cut.push(format!(
                                    "{what}: {:?} elided at {:.1} pt",
                                    t.galley.text(),
                                    t.galley.rect.width()
                                ));
                            }
                        }
                        let order: Vec<String> = h
                            .root()
                            .children_recursive()
                            .filter_map(|n| {
                                let a = n.accesskit_node();
                                a.value()
                                    .or_else(|| a.label())
                                    .filter(|_| matches!(a.role(), Role::Label | Role::Button))
                            })
                            .collect();
                        // The prompt is every label read before Place, its lines joined as one text.
                        let place_at = order.iter().position(|w| w == "Place");
                        let cancel_at = order.iter().position(|w| w == "Cancel");
                        let asked = order[..place_at.unwrap_or(0)].join(" ");
                        assert!(
                            asked.contains(priced) && asked.contains(carries),
                            "{what}: the price and {carries:?} are read before Place: {order:?}"
                        );
                        assert!(place_at < cancel_at, "{what}: then Place, then Cancel: {order:?}");
                    }
                }
            }
        }
        assert!(cut.is_empty(), "{}", cut.join("\n"));
    }

    /// A measurement for the report, not a check: at the narrowest strip (304 pt), in every look,
    /// the room the prompt has left of Place and Cancel, how wide each of its two lines is, and how
    /// wide the old ONE-line prompt was. Run it with
    /// `cargo test -p vike-panels --lib print_prompt_room -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement for the report: the held prompt's room at the narrowest strip"]
    fn print_prompt_room() {
        const STRIP_W: f32 = 304.0;
        let place = |exits| TradeAction::Place {
            side: 1,
            order_type: OrderType::Limit,
            price: Some(65_432.1),
            qty: 0.012,
            reduce_only: false,
            exits,
            origin: Origin::Ladder,
        };
        let bracket = Some(crate::trade::Exits { take_profit: 65_759.6, stop_loss: 65_236.1 });
        let grid = Grid { tick: 0.1, lot: 0.001, min_qty: 0.001 };
        let mut h = Harness::builder().build_ui_state(
            move |ui, out: &mut Vec<String>| {
                if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                    return;
                }
                out.clear();
                for density in Density::ALL {
                    for text_size in TextSize::ALL {
                        let a = Appearance { density, text_size, ..Appearance::default() };
                        let t = Tokens::from_appearance(&a);
                        let w = |s: &str| {
                            let font = t.font(TextRole::Body);
                            ui.painter().layout_no_wrap(s.to_string(), font, t.theme.text).size().x
                        };
                        let answers =
                            button_w(ui.ctx(), &t, "Place") + button_w(ui.ctx(), &t, "Cancel");
                        let room = STRIP_W - answers - 2.0 * t.metrics.gap;
                        for exits in [bracket, None] {
                            let (order, carries) = describe_parts(&place(exits), grid, None);
                            let carries = carries.unwrap_or_default();
                            let one_line = format!("{order}, {carries}?");
                            out.push(format!(
                                "{density:?}/{text_size:?}: room {room:.1} pt; {:?} {:.1}, {:?} \
                                 {:.1}; one line {:.1}",
                                format!("{order},"),
                                w(&format!("{order},")),
                                format!("{carries}?"),
                                w(&format!("{carries}?")),
                                w(&one_line)
                            ));
                        }
                    }
                }
            },
            Vec::new(),
        );
        h.run();
        for line in h.state() {
            println!("{line}");
        }
    }

    /// W4 fix round 2, N4: where the words do not fit (a 244 pt strip at Comfortable density and
    /// Large text), the answers are ✓ and ✕ — and they ANSWER: ✓ is named Place and sends the held
    /// order, ✕ is named Cancel and sends nothing and says so.
    #[test]
    fn the_check_and_cross_answers_place_and_cancel() {
        let look = Appearance {
            density: Density::Comfortable,
            text_size: TextSize::Standard,
            ..Appearance::default()
        };
        let held = TradeAction::Place {
            side: -1,
            order_type: OrderType::Limit,
            price: Some(65_432.1),
            qty: 0.012,
            reduce_only: false,
            exits: Some(crate::trade::Exits { take_profit: 65_105.9, stop_loss: 65_759.3 }),
            origin: Origin::Ladder,
        };
        let painted = |h: &Harness<'static, Fixture>| -> Vec<String> {
            let mut out = Vec::new();
            for c in &h.output().shapes {
                if let egui::Shape::Text(t) = &c.shape {
                    out.push(t.galley.text().to_string());
                }
            }
            out
        };
        let mut h = harness_in(look, held.clone(), 260.0);
        h.run();
        let words = painted(&h);
        let check = icons::CHECK.rich().text().to_string();
        let cross = icons::CLOSE.rich().text().to_string();
        assert!(words.contains(&check) && words.contains(&cross), "{words:?}");
        assert!(!words.iter().any(|w| w == "Place" || w == "Cancel"), "icons, not words");
        // F2: each glyph is painted ON the button it answers with — the check on the one named
        // Place, the cross on the one named Cancel. A cross on the button that places would draw a
        // refusal on the one control that sends.
        let glyph_at = |h: &Harness<'static, Fixture>, glyph: &str| -> egui::Pos2 {
            let at: Vec<egui::Pos2> = h
                .output()
                .shapes
                .iter()
                .filter_map(|c| match &c.shape {
                    egui::Shape::Text(t) if t.galley.text() == glyph => {
                        Some(t.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .collect();
            match at.as_slice() {
                [one] => *one,
                other => panic!("one {glyph:?} painted, found {other:?}"),
            }
        };
        let (place, cancel) = (h.get_by_label("Place").rect(), h.get_by_label("Cancel").rect());
        assert!(place.contains(glyph_at(&h, &check)), "the check is on Place: {place:?}");
        assert!(cancel.contains(glyph_at(&h, &cross)), "the cross is on Cancel: {cancel:?}");
        h.get_by_label("Place").click();
        h.run();
        assert_eq!(h.state().emitted, std::slice::from_ref(&held), "✓ places the held order");
        assert!(h.state().state.held.is_none());

        let mut h = harness_in(look, held, 260.0);
        h.run();
        h.get_by_label("Cancel").click();
        h.run();
        assert_eq!(
            h.state().emitted,
            [TradeAction::Note { kind: StatusKind::Info, text: NOT_SENT.to_string() }],
            "✕ sends nothing and says so"
        );
        assert!(h.state().state.held.is_none());
    }

    /// Cancel sends nothing, lets go of the order, and says so.
    #[test]
    fn cancel_sends_nothing_and_says_so() {
        let mut h = harness(buy());
        h.run();
        h.get_by_label("Cancel").click();
        h.run();
        assert_eq!(
            h.state().emitted,
            [TradeAction::Note { kind: StatusKind::Info, text: NOT_SENT.to_string() }]
        );
        assert!(h.state().state.held.is_none());
    }
}
