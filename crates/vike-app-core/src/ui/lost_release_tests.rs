use super::*;
use crate::ui::window::{WindowKind, WindowState, show_window, tool_title_bar};
use egui::{Rect, Vec2, pos2, vec2};

fn screen() -> Rect {
    Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 1000.0))
}

fn primary(at: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::default(),
    }
}

/// One tool window with the REAL title bar, driven frame by frame; `guarded` runs each frame's
/// input through [`end_lost_press`] first, as `vike-desktop`'s `raw_input_hook` does.
struct Rig {
    context: Context,
    time: f64,
    window: WindowState,
    guarded: bool,
}

impl Rig {
    fn new(guarded: bool) -> Rig {
        let context = Context::default();
        vike_ui_theme::appearance::install(
            &context,
            &vike_ui_theme::appearance::ThemeAppearance::default(),
        );
        let rect = Rect::from_min_size(pos2(200.0, 200.0), vec2(600.0, 400.0));
        let window = WindowState::tool("fixture", WindowKind::News, rect);
        let mut rig = Rig { context, time: 0.0, window, guarded };
        for _ in 0..14 {
            rig.frame(Vec::new()); // past the open pin
        }
        rig
    }

    fn frame(&mut self, events: Vec<Event>) {
        let mut raw = RawInput {
            screen_rect: Some(screen()),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        self.time += 1.0 / 60.0;
        if self.guarded {
            end_lost_press(&self.context, &mut raw);
        }
        self.context.begin_pass(raw);
        show_window(&self.context, &mut self.window, screen(), |ui, _| {
            tool_title_bar(ui, WindowKind::News, false).1
        });
        self.context.end_pass().drop_without_applying_deltas();
    }

    /// Hover the title bar's empty middle, press there and drag 30 pt right. The drag has to be
    /// live, or what follows proves nothing. Returns where the pointer is.
    fn grab_and_drag(&mut self) -> Pos2 {
        let painted = self.context.memory(|m| m.area_rect(self.window.id)).expect("shown");
        let at =
            pos2(painted.center().x, painted.top() + vike_ui_theme::chrome::TITLE_BAR_HEIGHT / 2.0);
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![primary(at, true)]);
        let before = self.window.screen_position;
        let to = at + vec2(30.0, 0.0);
        self.frame(vec![Event::PointerMoved(to)]);
        assert_eq!(
            self.window.screen_position - before,
            vec2(30.0, 0.0),
            "the title bar must drag"
        );
        to
    }

    /// Two bare moves (the first move after a `PointerGone` carries no delta in egui) and how far
    /// they took the window.
    fn moves_after(&mut self, from: Pos2) -> Vec2 {
        let before = self.window.screen_position;
        self.frame(vec![Event::PointerMoved(from + vec2(90.0, 40.0))]);
        self.frame(vec![Event::PointerMoved(from + vec2(120.0, 60.0))]);
        self.window.screen_position - before
    }
}

/// The owner's report: the release went to another window, which took the focus, and the Data
/// Manager then followed a bare pointer. Unguarded, egui does exactly that (the control leg).
#[test]
fn a_focus_loss_mid_drag_ends_the_drag() {
    for guarded in [true, false] {
        let mut rig = Rig::new(guarded);
        let at = rig.grab_and_drag();
        rig.frame(vec![Event::WindowFocused(false)]);
        rig.frame(vec![Event::WindowFocused(true)]);
        let moved = rig.moves_after(at);
        assert_eq!(moved == Vec2::ZERO, guarded, "guarded {guarded}: the window moved {moved:?}");
    }
}

/// The release went elsewhere with no focus change: the pointer left and came back, and no move was
/// reported outside in between, so no button was held.
#[test]
fn a_pointer_back_with_nothing_reported_outside_ends_the_drag() {
    for guarded in [true, false] {
        let mut rig = Rig::new(guarded);
        let at = rig.grab_and_drag();
        rig.frame(vec![Event::PointerGone]);
        let moved = rig.moves_after(at);
        assert_eq!(moved == Vec2::ZERO, guarded, "guarded {guarded}: the window moved {moved:?}");
    }
}

/// A drag really held past the viewport's edge is reported outside, and keeps dragging when it
/// comes back.
#[test]
fn a_held_drag_that_leaves_and_comes_back_keeps_dragging() {
    let mut rig = Rig::new(true);
    let at = rig.grab_and_drag();
    rig.frame(vec![Event::PointerGone, Event::PointerMoved(pos2(at.x, 1010.0))]);
    let moved = rig.moves_after(at);
    assert_ne!(moved, Vec2::ZERO, "a held drag must keep dragging");
}
