//! A press whose RELEASE never reached this viewport.
//!
//! egui knows a button is up only from a release event. When another window takes the release (the
//! owner's case: a Snipping Tool overlay grabbed it), egui keeps the button down for good, so the
//! title bar under the last press stays dragged and every later move of a bare pointer drags that
//! window. [`end_lost_press`] edits each frame's input before egui reads it (`vike-desktop`'s
//! `raw_input_hook`) and releases a held button at either moment that proves the release went
//! elsewhere:
//!
//! - **the viewport lost focus** (`WindowFocused(false)`);
//! - **the pointer left the viewport and came back with no move reported outside it in between.**
//!   While a button is held the OS keeps reporting the pointer to the window that took the press,
//!   outside it too (Windows' mouse capture, X11's implicit grab, macOS's drag events), so a return
//!   with nothing reported outside means no button was held. A held drag that leaves and comes
//!   back is untouched.
//!
//! Not covered: asking the OS directly whether a button is down. That is an `unsafe` FFI call
//! (`GetAsyncKeyState` on Windows) and the workspace forbids `unsafe_code`.

use egui::{Context, Event, Id, Modifiers, PointerButton, Pos2, RawInput};

/// Every button egui tracks.
const BUTTONS: [PointerButton; 5] = [
    PointerButton::Primary,
    PointerButton::Secondary,
    PointerButton::Middle,
    PointerButton::Extra1,
    PointerButton::Extra2,
];

/// Releases, inside `raw`, every press egui still holds whose release cannot arrive any more (see
/// the module doc). Call it on each frame's input before the pass that reads it.
pub fn end_lost_press(context: &Context, raw: &mut RawInput) {
    // Whether the pointer left while a button was held, with no move outside since: across frames.
    let memory = Id::unique("vike_lost_press_gone");
    let viewport = raw.screen_rect.unwrap_or_else(|| context.viewport_rect());
    let (mut held, mut at, modifiers) = context.input(|i| {
        let held: Vec<PointerButton> =
            BUTTONS.into_iter().filter(|b| i.pointer.button_down(*b)).collect();
        (held, i.pointer.latest_pos(), i.modifiers)
    });
    let mut gone = context.data(|d| d.get_temp::<bool>(memory)).unwrap_or(false);
    let mut events = Vec::with_capacity(raw.events.len());
    for event in std::mem::take(&mut raw.events) {
        match &event {
            Event::PointerMoved(p) if gone => {
                gone = false;
                if viewport.contains(*p) {
                    release(&mut held, at, modifiers, &mut events); // back with nothing held
                }
            }
            Event::PointerGone => gone = !held.is_empty(),
            Event::PointerButton { button, pressed, .. } => {
                gone = false;
                held.retain(|b| b != button);
                if *pressed {
                    held.push(*button);
                }
            }
            _ => {}
        }
        if let Event::PointerMoved(p) | Event::PointerButton { pos: p, .. } = &event {
            at = Some(*p);
        }
        let focus_lost = matches!(event, Event::WindowFocused(false));
        events.push(event);
        if focus_lost {
            release(&mut held, at, modifiers, &mut events);
        }
    }
    raw.events = events;
    context.data_mut(|d| d.insert_temp(memory, gone));
}

fn release(
    held: &mut Vec<PointerButton>,
    at: Option<Pos2>,
    modifiers: Modifiers,
    out: &mut Vec<Event>,
) {
    let screen_position = at.unwrap_or(Pos2::ZERO);
    out.extend(held.drain(..).map(|button| Event::PointerButton {
        pos: screen_position,
        button,
        pressed: false,
        modifiers,
    }));
}

#[path = "lost_release_tests.rs"]
#[cfg(test)]
mod lost_release_tests;
