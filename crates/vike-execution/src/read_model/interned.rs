//! [`Interned`]: the venue and symbol names a published row carries, copied for free.
//!
//! A [`crate::PositionView`] and a published mark name their venue and symbol, and the engine
//! already holds those names as interned [`ustr::Ustr`]s (its position and mark keys). Published as
//! `String`s they cost two allocations per row per publish, and a publish runs per event on a
//! sporadic feed (`vike_core::snapshot::build`, on the latency-gated hop), so an engine that
//! admitted symbols (decision 0173) paid that for every position it held. Published as `Interned`
//! a row is copied by value and a snapshot clone allocates nothing for its names.
//!
//! It reads as the string it names, so a reader changes little: it dereferences to `str`, compares
//! equal to a `str`, `&str` or `String`, and prints (`Display` and `Debug`) exactly as that string
//! does, so a `{:?}` of a row is the text it was before the type existed. What it is not: a
//! `String`. A reader that needs an owned one says `to_string()` (or `String::from`), and one that
//! builds a row from text says `.into()`, which interns it.
//!
//! The intern table is process-wide and never freed, which is the property that makes the copy free.
//! The names it holds are the venues and the symbols a process has traded or published, a bounded
//! set; a caller must not intern unbounded text (an order id, a free-form message) through it.

use std::fmt;
use std::ops::Deref;

use ustr::Ustr;

/// An interned venue or symbol name. See the module doc.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Interned(Ustr);

impl Interned {
    /// The name as a `str`; it lives as long as the process.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        self.0.as_str()
    }
}

impl Deref for Interned {
    type Target = str;

    fn deref(&self) -> &str {
        self.0.as_str()
    }
}

impl AsRef<str> for Interned {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for Interned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.0.as_str(), f)
    }
}

/// The quoted text a `String` prints, not `Ustr`'s `u!("..")`: a row's `{:?}` does not change.
impl fmt::Debug for Interned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.0.as_str(), f)
    }
}

impl From<Ustr> for Interned {
    fn from(name: Ustr) -> Self {
        Interned(name)
    }
}

impl From<&str> for Interned {
    fn from(name: &str) -> Self {
        Interned(Ustr::from(name))
    }
}

impl From<&String> for Interned {
    fn from(name: &String) -> Self {
        Interned(Ustr::from(name.as_str()))
    }
}

impl From<String> for Interned {
    fn from(name: String) -> Self {
        Interned(Ustr::from(name.as_str()))
    }
}

impl From<Interned> for String {
    fn from(name: Interned) -> Self {
        name.as_str().to_owned()
    }
}

impl PartialEq<str> for Interned {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Interned {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for Interned {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Interned> for str {
    fn eq(&self, other: &Interned) -> bool {
        self == other.as_str()
    }
}

impl PartialEq<Interned> for &str {
    fn eq(&self, other: &Interned) -> bool {
        *self == other.as_str()
    }
}

impl PartialEq<Interned> for String {
    fn eq(&self, other: &Interned) -> bool {
        self.as_str() == other.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// It prints as the `String` it names, in `{}`, `{:?}`, `{:>8}` and inside a derived `Debug`,
    /// so nothing that formats a row (a log line, a `format!("{:?}", ..)` equality) can tell.
    #[test]
    fn it_prints_exactly_as_the_string_it_names() {
        let name: Interned = "BTCUSDT".into();
        let text = String::from("BTCUSDT");
        assert_eq!(format!("{name}"), format!("{text}"));
        assert_eq!(format!("{name:?}"), format!("{text:?}"));
        assert_eq!(format!("{name:>10}|{name:<10}|"), format!("{text:>10}|{text:<10}|"));
        assert_eq!(format!("{:?}", (name, 1.5_f64)), format!("{:?}", (text, 1.5_f64)));
        let quoted: Interned = "a\"b\\c\n".into();
        assert_eq!(format!("{quoted:?}"), format!("{:?}", "a\"b\\c\n"));
    }

    /// Equality is the string's, whichever side holds which type, and the empty name is the
    /// default.
    #[test]
    fn it_compares_equal_to_the_text_it_names_and_to_nothing_else() {
        let name: Interned = String::from("ETH").into();
        assert_eq!(name, "ETH");
        assert_eq!(name, *"ETH");
        assert_eq!(name, String::from("ETH"));
        assert_eq!("ETH", name);
        assert_eq!(String::from("ETH"), name);
        assert_ne!(name, "ETH ");
        assert_ne!(name, "eth");
        assert_eq!(name, Interned::from(&String::from("ETH")));
        assert_eq!(Interned::default(), "");
        assert_eq!(name.len(), 3);
        assert_eq!(String::from(name), "ETH");
        assert_eq!(name.to_string(), "ETH");
    }
}
