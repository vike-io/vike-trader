//! The leak-once map from a daemon's `Unsupported` reason to the `&'static str` that
//! `vike_data::live::LiveDataError::Unsupported` requires.
//!
//! The in-process error carries a `&'static str` and the wire carries an owned `String`. Two ways
//! across, tried in order:
//!
//! 1. **The canonical reasons need no leak.** `vike_data::require_live_verb` is the one source of the
//!    five declared-capability refusals (`LiveVerb::{Bars, Quotes, Trades, Book, Depth}`), and its
//!    texts do not depend on the venue; asking it with the fail-closed unknown venue returns each
//!    `&'static str` itself. A forwarded reason equal to one of them maps back to the SAME string,
//!    pointer and all.
//! 2. **Anything else is interned: leaked once.** A bridge's own reason ("no live depth feed") is a
//!    string literal in its crate, which this crate cannot name. The first time a distinct text
//!    arrives it is `Box::leak`ed into a process-wide set; every later arrival of the same text
//!    returns that one `&'static str`. The set is bounded ([`MAX_INTERNED`] entries of at most
//!    [`MAX_INTERNED_BYTES`] bytes each), so a hostile or buggy daemon cannot grow the leak: past
//!    the bound a fixed overflow reason is returned (which still says the verb is unsupported, and
//!    names why the text is missing).
//!
//! `vike-data` is not edited (the seam is consumed, not modified in passing), and the leak is
//! bounded to a few kilobytes for the life of the process.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use vike_venues::LiveVerb;

/// The most distinct non-canonical reasons the set will hold. A venue feed has a handful of
/// `Unsupported` texts; 64 is generous and a ceiling on the leak.
pub const MAX_INTERNED: usize = 64;

/// The longest reason the set will leak, in bytes. A longer one is cut at a character boundary
/// before it is interned, so one entry costs at most this much for the life of the process.
pub const MAX_INTERNED_BYTES: usize = 512;

/// What a reason becomes once the set is full — still an honest `Unsupported`, and it says where the
/// real text went.
pub const OVERFLOW_REASON: &str = "unsupported by the gateway's venue client (the reason text is not shown: the client's \
     interning bound was reached)";

/// The five canonical reasons, as `vike_data::require_live_verb` spells them. Asked with an unknown
/// venue, which is refused for every verb, so the texts are returned whatever the roster says.
fn canonical() -> impl Iterator<Item = &'static str> {
    [LiveVerb::Bars, LiveVerb::Quotes, LiveVerb::Trades, LiveVerb::Book, LiveVerb::Depth]
        .into_iter()
        .filter_map(|verb| match vike_data::require_live_verb("", verb) {
            Err(vike_data::LiveDataError::Unsupported(reason)) => Some(reason),
            _ => None,
        })
}

fn set() -> &'static Mutex<HashMap<String, &'static str>> {
    static SET: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashMap::new()))
}

/// How many distinct texts have been leaked so far (tests and the status line).
pub fn interned_count() -> usize {
    set().lock().unwrap_or_else(PoisonError::into_inner).len()
}

/// Map a forwarded `Unsupported` reason to a `&'static str`: the canonical string when it is one, the
/// interned copy otherwise, [`OVERFLOW_REASON`] once the set is full. Total, never panics, and the
/// same text always yields the same pointer.
pub fn unsupported_reason(text: &str) -> &'static str {
    if let Some(known) = canonical().find(|known| *known == text) {
        return known;
    }
    let text = bounded(text);
    let mut set = set().lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&interned) = set.get(text) {
        return interned;
    }
    if set.len() >= MAX_INTERNED {
        return OVERFLOW_REASON;
    }
    let leaked: &'static str = Box::leak(text.to_string().into_boxed_str());
    set.insert(text.to_string(), leaked);
    leaked
}

/// `text` cut to at most [`MAX_INTERNED_BYTES`], on a character boundary.
fn bounded(text: &str) -> &str {
    if text.len() <= MAX_INTERNED_BYTES {
        return text;
    }
    let mut end = MAX_INTERNED_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ⚠ This once compared `interned_count()` before and after, and failed about one run in four: the set
    /// is process-wide and the tests of this module run in parallel, so another test interning its own
    /// foreign text between the two reads moved the count. The property is that a canonical reason is
    /// never an ENTRY of the set, which no other test can change.
    #[test]
    fn a_canonical_reason_maps_back_to_the_same_string_without_a_leak() {
        for verb in
            [LiveVerb::Bars, LiveVerb::Quotes, LiveVerb::Trades, LiveVerb::Book, LiveVerb::Depth]
        {
            let Err(vike_data::LiveDataError::Unsupported(canon)) =
                vike_data::require_live_verb("", verb)
            else {
                panic!("an unknown venue must be refused for every verb");
            };
            let back = unsupported_reason(canon);
            assert_eq!(back, canon);
            assert!(std::ptr::eq(back, canon), "the canonical reason is returned, not a copy");
            assert!(
                !set().lock().unwrap_or_else(PoisonError::into_inner).contains_key(canon),
                "a canonical reason leaks nothing: `{canon}` was interned"
            );
        }
    }

    #[test]
    fn a_foreign_reason_is_interned_once_and_the_pointer_is_stable() {
        let a = unsupported_reason("test reason: this bridge serves no such lane");
        let b = unsupported_reason("test reason: this bridge serves no such lane");
        assert_eq!(a, "test reason: this bridge serves no such lane");
        assert!(std::ptr::eq(a, b), "the same text yields the same &'static str");
    }

    #[test]
    fn a_long_reason_is_cut_on_a_character_boundary() {
        let long = "é".repeat(MAX_INTERNED_BYTES);
        let got = unsupported_reason(&long);
        assert!(got.len() <= MAX_INTERNED_BYTES);
        assert!(long.starts_with(got), "the interned text is a prefix of the reason");
    }
}
