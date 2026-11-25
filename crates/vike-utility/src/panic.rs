//! **A panic payload as text** (decision 0141): [`payload_text`], the one spelling the
//! `catch_unwind` arms and `JoinHandle::join` errors of seven crates used to copy, with six
//! different words for a payload that is not text. The one word now is "a non-text panic payload".
//!
//! `panic!("literal")` carries a `&'static str`, `panic!("{x}")` a `String`, and
//! `std::panic::panic_any` anything at all. What a caller does with the text (a bound on its length,
//! the log line, the refusal it answers) stays at the call site.
//!
//! Two copies keep their own: `crates/vike-log/src/lib.rs`'s `panic_payload_text` (`vike-log` names
//! no `vike-*` crate, `crates/vike-ops/tests/architecture/layer_gate/vike_free.rs`'s
//! `VIKE_FREE_CRATES`) and `crates/vike-user-research/src/file/guest.rs`'s `panic_text` (its
//! `[dependencies]` table is the user-study surface, decision 0284).

use std::any::Any;

/// The answer for a payload that is neither a `&str` nor a `String`.
const NOT_TEXT: &str = "a non-text panic payload";

/// The text a panic carried, or "a non-text panic payload".
///
/// Pass the payload itself (`&*payload` for a `Box<dyn Any + Send>`). A `&Box<dyn Any + Send>`
/// coerces to `&(dyn Any + Send)` as the BOX, not as what it holds, so a boxed payload handed in
/// whole is opened here rather than answered as non-text.
pub fn payload_text(payload: &(dyn Any + Send)) -> &str {
    if let Some(text) = payload.downcast_ref::<&str>() {
        text
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text
    } else if let Some(boxed) = payload.downcast_ref::<Box<dyn Any + Send>>() {
        payload_text(&**boxed)
    } else {
        NOT_TEXT
    }
}

#[cfg(test)]
#[path = "panic_tests.rs"]
mod tests;
