//! [`Symbol`]: a short id, interned. What every bar and tick names its instrument with, and the one interned
//! type for an instrument or a venue name.
//
// Engineering notes (not rendered): the per-row `String` it replaced cost a malloc, a free and 24 + ~32 bytes
// on every tick and stamped bar (a BTCUSDT.P day: 3.6 M trades, 115 MB of heap). A handle is one pointer to a
// leaked entry (the text's hash and the text), so the rows shrink (`TickTrade` 64 -> 48 B, `TickQuote` 72 ->
// 56 B, `Bar` 120 -> 104 B), copy without a malloc and drop without a free.
//
// ⚠ TWO TABLES, ONE MEANING. A strategy `.so` links its own copy of this crate, so it has its own table, and
// reads the host's rows in place (`crates/vike-rust-strategy/src/abi.rs`'s `layout`, which hashes
// `size_of::<Symbol>()` and `Symbol::REPRESENTATION`). So:
// - EQUALITY compares the pointers first (one table: equal exactly when one entry, at no cost), then the
//   stored hashes (different texts: almost always different, so no text is read), and the texts only when the
//   pointers differ and the hashes agree (one text in two tables, or a collision);
// - the HASH is computed once, when a text is interned, by a FIXED, seedless function of the text alone
//   (FNV-1a 64), so a handle of one text hashes alike in every table, process and strategy file; `Hash`
//   writes that stored value and reads no text;
// - nothing is ever freed: a host handle the guest copies into its state stays valid for the process's life.
// Serde writes the text, so no handle reaches a wire or a disk: every byte is `String`'s.
//
// INTERNING takes no lock for a text this thread has seen: the thread's last hit, then its own map of the
// texts it has met (a thread-local, so no lock and no sharing), and only a thread's FIRST sight of a text
// takes the process table's lock. Reading a handle (its text, its hash, a comparison) touches no table.
//
// The price is a leak by design: every distinct text a process interns stays, the text plus ~40 B (and a
// map slot in each thread that met it). A backtest child interns a handful; a long-lived recorder seeing new
// Polymarket token ids grows by roughly 0.1-0.2 MB a day until it restarts. No cap: a symbol must stay correct.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::ops::Deref;
use std::sync::{LazyLock, Mutex, PoisonError};

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A short id, interned: an instrument (`"BTCUSDT.P"`, `"EURUSD"`) or a venue (`"binance"`). 8 bytes, `Copy`,
/// and no allocation to make, copy or drop one after its text was first seen.
///
/// Reads as a `&str` (`Deref`, [`Symbol::as_str`], `AsRef<str>`), compares by its text (`symbol ==
/// "BTCUSDT.P"` works; two handles compare their pointers first), hashes by a value computed once from its
/// text, prints as its text (`Display`; `Debug` as a `String` does), and serializes as its text, so its serde
/// form is a `String`'s byte for byte.
///
/// ```
/// use vike_marketdata::Symbol;
///
/// let symbol = Symbol::new("BTCUSDT.P");
/// assert_eq!(symbol, "BTCUSDT.P");
/// assert_eq!(symbol.len(), 9);
/// assert_eq!(Symbol::default(), "");
/// assert_eq!(serde_json::to_string(&symbol).unwrap(), r#""BTCUSDT.P""#);
/// ```
#[derive(Clone, Copy)]
pub struct Symbol(&'static Entry);

/// One interned text and its hash, never freed. `repr(C)`: a strategy file reads the host's in place, under
/// [`Symbol::REPRESENTATION`].
#[repr(C)]
struct Entry {
    /// [`text_hash`] of `text`.
    hash: u64,
    text: &'static str,
}

/// FNV-1a 64's offset basis and prime: the stored hash is the same in every process and strategy file.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64 of `bytes`, from `state`.
const fn fnv1a(mut state: u64, bytes: &[u8]) -> u64 {
    let mut i = 0;
    while i < bytes.len() {
        state ^= bytes[i] as u64;
        state = state.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    state
}

/// The hash an entry stores: a function of the text alone, seedless.
const fn text_hash(text: &str) -> u64 {
    fnv1a(FNV_OFFSET, text.as_bytes())
}

/// The empty symbol's entry: a static, so [`Symbol::default`] takes no table entry.
static EMPTY: Entry = Entry { hash: text_hash(""), text: "" };

/// Every text this process has interned, each once (never freed: a handle is valid for the process's life).
/// Locked only on a thread's first sight of a text.
static TABLE: LazyLock<Mutex<HashMap<&'static str, Symbol>>> = LazyLock::new(Mutex::default);

/// A word-at-a-time `Hasher` (the Fx fold) for a thread's own map of the texts it has met: the lookup an
/// interning pays for every text after the thread's last hit, so it reads eight bytes a step where FNV-1a
/// reads one (measured by `crates/vike-model/benches/symbol_vs_ustr.rs`, deleted with `ustr`; its numbers are
/// decision 0296's amendment). Local to one thread, never stored.
#[derive(Default)]
struct WordHash(u64);

impl WordHash {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for WordHash {
    fn write(&mut self, bytes: &[u8]) {
        let (words, tail) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_le_bytes(*word));
        }
        let last =
            tail.iter().rev().fold(tail.len() as u64, |word, &byte| (word << 8) | u64::from(byte));
        self.add(last);
    }
    fn write_u8(&mut self, byte: u8) {
        self.add(u64::from(byte));
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

thread_local! {
    /// This thread's last hit, so a run of rows of one instrument (a decode, a feed's stream) hashes nothing.
    static LAST: Cell<Option<Symbol>> = const { Cell::new(None) };
    /// Every text this thread has met: a lookup takes no lock.
    static SEEN: RefCell<HashMap<&'static str, Symbol, BuildHasherDefault<WordHash>>> =
        RefCell::new(HashMap::default());
}

impl Symbol {
    /// The version of the handle's representation (a pointer to a `repr(C)` entry of the text's FNV-1a 64 hash
    /// and the text), which a strategy file's `layout` hashes beside `size_of::<Symbol>()`: a change of
    /// representation of the same size is still refused at load. Bump it with any change to what the pointer
    /// points at, or to the hash function.
    pub const REPRESENTATION: u64 = 2;

    /// The longest text a symbol READ from a peer may be (its `Deserialize`): a Polymarket token id is 77 or 78
    /// bytes and every other instrument or venue name shorter, so a longer text is a malformed reply, refused
    /// and never interned (an interned text is kept for the process's life).
    pub const MAX_READ_LEN: usize = 256;

    /// The symbol whose text is `text`: this process's handle for it, interned on first sight.
    pub fn new(text: &str) -> Self {
        if text.is_empty() {
            return Self::default();
        }
        if let Some(hit) = LAST.get()
            && hit.as_str() == text
        {
            return hit;
        }
        // A thread being torn down (its map gone) asks the table.
        let symbol = SEEN
            .try_with(|seen| {
                if let Some(&met) = seen.borrow().get(text) {
                    return met;
                }
                let met = intern(text);
                seen.borrow_mut().insert(met.as_str(), met);
                met
            })
            .unwrap_or_else(|_| intern(text));
        LAST.set(Some(symbol));
        symbol
    }

    /// The text.
    pub fn as_str(&self) -> &'static str {
        self.0.text
    }

    /// Whether this process's table holds `text`, without interning it: what a test asks of a decoder that
    /// must not intern the rows it drops. Test builds only.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_interned(text: &str) -> bool {
        TABLE.lock().unwrap_or_else(PoisonError::into_inner).contains_key(text)
    }
}

/// The table's handle for `text`, leaked into it on the process's first sight.
fn intern(text: &str) -> Symbol {
    let mut table = TABLE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(&found) = table.get(text) {
        return found;
    }
    let text: &'static str = Box::leak(Box::from(text));
    let symbol = Symbol(Box::leak(Box::new(Entry { hash: text_hash(text), text })));
    table.insert(text, symbol);
    symbol
}

impl Default for Symbol {
    /// `""`, which takes no table entry.
    fn default() -> Self {
        Symbol(&EMPTY)
    }
}

impl Deref for Symbol {
    type Target = str;
    fn deref(&self) -> &str {
        self.0.text
    }
}

impl AsRef<str> for Symbol {
    fn as_ref(&self) -> &str {
        self.0.text
    }
}

impl From<&str> for Symbol {
    fn from(text: &str) -> Self {
        Symbol::new(text)
    }
}

impl From<&String> for Symbol {
    fn from(text: &String) -> Self {
        Symbol::new(text)
    }
}

impl From<String> for Symbol {
    fn from(text: String) -> Self {
        Symbol::new(&text)
    }
}

impl From<Symbol> for String {
    fn from(symbol: Symbol) -> Self {
        symbol.as_str().to_owned()
    }
}

impl PartialEq for Symbol {
    fn eq(&self, other: &Self) -> bool {
        // One entry: equal. Two entries of one table hold two texts, which their hashes almost always tell
        // apart; one text in two tables (a strategy file's handle) agrees on the hash and on the text.
        std::ptr::eq(self.0, other.0)
            || (self.0.hash == other.0.hash && self.0.text == other.0.text)
    }
}

impl Eq for Symbol {}

impl Hash for Symbol {
    /// Writes the hash stored when the text was interned, a function of the text alone: a handle of one text
    /// hashes alike in every table. Reads no text.
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.0.hash);
    }
}

impl PartialOrd for Symbol {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Symbol {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

/// `Symbol == text` and `text == Symbol`, for each text type a caller compares against.
macro_rules! text_eq {
    ($($text:ty),+) => {$(
        impl PartialEq<$text> for Symbol {
            fn eq(&self, other: &$text) -> bool {
                self.as_str() == <$text as AsRef<str>>::as_ref(other)
            }
        }
        impl PartialEq<Symbol> for $text {
            fn eq(&self, other: &Symbol) -> bool {
                <$text as AsRef<str>>::as_ref(self) == other.as_str()
            }
        }
    )+};
}
text_eq!(str, &str, String);

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl fmt::Debug for Symbol {
    /// As a `String` prints: the text, quoted.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl Serialize for Symbol {
    /// The text, as a `String` serializes.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Symbol {
    /// What a `String` reads (a string, or UTF-8 bytes), interned; a text longer than
    /// [`Symbol::MAX_READ_LEN`] is refused and never interned.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Text;
        impl Visitor<'_> for Text {
            type Value = Symbol;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a string of at most {} bytes", Symbol::MAX_READ_LEN)
            }
            fn visit_str<E: de::Error>(self, text: &str) -> Result<Symbol, E> {
                if text.len() > Symbol::MAX_READ_LEN {
                    return Err(E::invalid_length(text.len(), &self));
                }
                Ok(Symbol::new(text))
            }
            fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Symbol, E> {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| E::invalid_value(de::Unexpected::Bytes(bytes), &self))?;
                self.visit_str(text)
            }
        }
        deserializer.deserialize_string(Text)
    }
}

#[path = "symbol_tests.rs"]
#[cfg(test)]
mod symbol_tests;
