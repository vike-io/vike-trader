//! The STREAMING form of [`super::state_hash`]: a `serde_json` [`Formatter`] that writes the compact
//! JSON byte for byte as [`CompactFormatter`] does and, while it writes the engines, folds into an
//! FNV-1a64 digest the bytes `state_hash`'s canonical JSON has (the excluded fields cleared), with no
//! clone of an engine and no second serialization. Since the journal's payload became msgpack (format 34)
//! no JSON pass writes the record any more: [`super::state_hash`] runs this over a sink, on the journal's
//! write thread for a live core's cadence `Snapshot` (`vike_journal`'s `append_snap_deferred`). `at_key`
//! hashes the engines of a JSON record in the pass that writes it, which a JSON reader can still use.
//!
//! The exclusions are `hashed_as`'s table, the one place they are spelled; `state_hash`'s doc says why
//! each field is out, and `state_hash_by_clone` (the clone-and-clear spec) is what debug builds check
//! every hash against.

use std::io::{self, Write as _};

use serde_json::ser::{CharEscape, CompactFormatter, Formatter};
use vike_utility::fnv::Fnv1a64;

/// A `serde_json` formatter that writes compact JSON unchanged and takes [`super::state_hash`] of the
/// engines array it meets: the serialized value itself ([`Self::for_slice`]) or the value of an object
/// key ([`Self::at_key`]). Pass it as `&mut` to [`serde_json::Serializer::with_formatter`], then read
/// [`Self::finish`].
pub struct StateHashFormatter {
    /// The object key whose value is the engines array; `None` = the serialized value is.
    region_key: Option<&'static str>,
    phase: Phase,
    digest: Fnv1a64,
    /// Every container open inside the engines array, the array itself first.
    stack: Vec<Frame>,
    /// The object key being written, its quotes included.
    key: Vec<u8>,
    capturing_key: bool,
    /// While `Some(depth)`, nothing is hashed: a field the hash leaves out (or writes cleared) is being
    /// written in the object `depth` containers deep.
    suppressed_at: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Looking for `region_key`.
    Seek,
    /// The next container opened is the engines array.
    Armed,
    In,
    Done,
}

/// What a container inside the engines array is, as far as the excluded fields care.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Engines,
    /// An `EngineSnapshot`.
    Engine,
    /// Its `account`.
    Account,
    /// Its `registry`, and one `(order client id, OrderRecord)` entry of it.
    Registry,
    Entry,
    /// An `OrderRecord`, and its `request`.
    Order,
    Request,
    Other,
}

struct Frame {
    ctx: Ctx,
    /// What the next value written in this container is: set by its key or its element index.
    child: Ctx,
    elements: u32,
    /// An object has hashed a key already, so the next hashed key takes a comma.
    hashed_any: bool,
}

/// How `state_hash` hashes one object key.
enum Hashed {
    /// As written; its value is a container of this kind.
    As(Ctx),
    /// As this JSON: the field's cleared value.
    Cleared(&'static [u8]),
    /// Not at all: its cleared value is one its `skip_serializing_if` omits.
    Dropped,
}

/// `state_hash`'s exclusions (its doc says why each field is out), by the container the key is in.
fn hashed_as(ctx: Ctx, key: &[u8]) -> Hashed {
    match (ctx, key) {
        (Ctx::Engine, b"account") => Hashed::As(Ctx::Account),
        (Ctx::Engine, b"registry") => Hashed::As(Ctx::Registry),
        (Ctx::Order, b"request") => Hashed::As(Ctx::Request),
        (Ctx::Engine, b"now_ms") => Hashed::Cleared(b"0"),
        (Ctx::Account, b"marks") => Hashed::Cleared(b"[]"),
        (
            Ctx::Engine,
            b"seen_trade_days"
            | b"fsm_only_trade_days"
            | b"fill_window_days"
            | b"fill_floor_ms"
            | b"ended_orders_left_out"
            | b"tier"
            | b"native_time_release"
            | b"account_code"
            | b"broker_code_len"
            | b"funding_clocks",
        )
        | (Ctx::Order, b"deferred_events" | b"replaces" | b"lifetime_unknown")
        | (Ctx::Request, b"data_delay") => Hashed::Dropped,
        _ => Hashed::As(Ctx::Other),
    }
}

/// A written key without its quotes.
fn unquoted(key: &[u8]) -> &[u8] {
    key.get(1..key.len().saturating_sub(1)).unwrap_or(key)
}

impl StateHashFormatter {
    /// The serialized value is the engines array (`state_hash` of a slice).
    pub fn for_slice() -> Self {
        Self::new(None, Phase::Armed)
    }

    /// The value of the first object key `key` the serializer writes is the engines array.
    pub fn at_key(key: &'static str) -> Self {
        Self::new(Some(key), Phase::Seek)
    }

    fn new(region_key: Option<&'static str>, phase: Phase) -> Self {
        Self {
            region_key,
            phase,
            digest: Fnv1a64::new(),
            stack: Vec::new(),
            key: Vec::new(),
            capturing_key: false,
            suppressed_at: None,
        }
    }

    /// [`super::state_hash`] of the engines array, once it was written whole.
    pub fn finish(&self) -> Option<u64> {
        (self.phase == Phase::Done).then(|| self.digest.finish())
    }

    fn hashing(&self) -> bool {
        self.phase == Phase::In && self.suppressed_at.is_none()
    }

    fn tap<'a, W: ?Sized + io::Write>(&'a mut self, w: &'a mut W) -> Tap<'a, W> {
        let to = if self.capturing_key {
            To::Key(&mut self.key)
        } else if self.hashing() {
            To::Digest(&mut self.digest)
        } else {
            To::Nowhere
        };
        Tap { w, to }
    }

    fn open(&mut self) {
        let ctx = match (self.phase, self.stack.last()) {
            (Phase::Armed, _) => {
                self.phase = Phase::In;
                Ctx::Engines
            }
            (Phase::In, Some(parent)) => parent.child,
            _ => return,
        };
        self.stack.push(Frame { ctx, child: Ctx::Other, elements: 0, hashed_any: false });
    }

    fn close(&mut self) {
        if self.phase == Phase::In {
            self.stack.pop();
            if self.stack.is_empty() {
                self.phase = Phase::Done;
            }
        }
    }

    fn next_element(&mut self) {
        if let Some(f) = self.stack.last_mut() {
            f.elements += 1;
            f.child = match (f.ctx, f.elements) {
                (Ctx::Engines, _) => Ctx::Engine,
                (Ctx::Registry, _) => Ctx::Entry,
                (Ctx::Entry, 2) => Ctx::Order,
                _ => Ctx::Other,
            };
        }
    }

    /// The key is written whole: find the engines array by it, or hash it (and its comma) as
    /// `state_hash`'s canonical JSON writes it.
    fn key_written(&mut self) {
        if !std::mem::take(&mut self.capturing_key) {
            return;
        }
        if self.phase == Phase::Seek {
            if self.region_key.map(str::as_bytes) == Some(unquoted(&self.key)) {
                self.phase = Phase::Armed;
            }
            return;
        }
        let depth = self.stack.len();
        let Some(frame) = self.stack.last_mut() else { return };
        let hashed = hashed_as(frame.ctx, unquoted(&self.key));
        if let Hashed::Dropped = hashed {
            self.suppressed_at = Some(depth);
            return;
        }
        if frame.hashed_any {
            self.digest.write(b",");
        }
        frame.hashed_any = true;
        self.digest.write(&self.key);
        match hashed {
            Hashed::As(child) => frame.child = child,
            Hashed::Cleared(value) => {
                self.digest.write(b":");
                self.digest.write(value);
                self.suppressed_at = Some(depth);
            }
            Hashed::Dropped => {}
        }
    }
}

/// Writes through to the serializer's writer, and copies the bytes to the digest or the key buffer.
struct Tap<'a, W: ?Sized> {
    w: &'a mut W,
    to: To<'a>,
}

enum To<'a> {
    Nowhere,
    Digest(&'a mut Fnv1a64),
    Key(&'a mut Vec<u8>),
}

impl<W: ?Sized + io::Write> io::Write for Tap<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.w.write_all(buf)?;
        match &mut self.to {
            To::Nowhere => {}
            To::Digest(d) => d.write(buf),
            To::Key(k) => k.extend_from_slice(buf),
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

/// Every scalar and string through the tap, written as [`CompactFormatter`] writes it.
macro_rules! tapped {
    ($($name:ident($($arg:ident: $ty:ty),*);)*) => {$(
        #[inline]
        fn $name<W: ?Sized + io::Write>(&mut self, w: &mut W $(, $arg: $ty)*) -> io::Result<()> {
            CompactFormatter.$name(&mut self.tap(w) $(, $arg)*)
        }
    )*};
}

impl Formatter for &mut StateHashFormatter {
    tapped! {
        write_null();
        write_bool(v: bool);
        write_i8(v: i8);
        write_i16(v: i16);
        write_i32(v: i32);
        write_i64(v: i64);
        write_i128(v: i128);
        write_u8(v: u8);
        write_u16(v: u16);
        write_u32(v: u32);
        write_u64(v: u64);
        write_u128(v: u128);
        write_f32(v: f32);
        write_f64(v: f64);
        write_number_str(v: &str);
        begin_string();
        end_string();
        write_string_fragment(v: &str);
        write_char_escape(v: CharEscape);
        write_raw_fragment(v: &str);
    }

    fn begin_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.open();
        self.tap(w).write_all(b"[")
    }

    fn end_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.tap(w).write_all(b"]")?;
        self.close();
        Ok(())
    }

    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.next_element();
        if first { Ok(()) } else { self.tap(w).write_all(b",") }
    }

    fn begin_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.open();
        self.tap(w).write_all(b"{")
    }

    fn end_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.tap(w).write_all(b"}")?;
        self.close();
        Ok(())
    }

    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        // The comma reaches the digest, or not, once the key is known (`key_written`).
        if !first {
            w.write_all(b",")?;
        }
        self.capturing_key = self.phase == Phase::Seek || self.hashing();
        self.key.clear();
        Ok(())
    }

    fn end_object_key<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        self.key_written();
        Ok(())
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.tap(w).write_all(b":")
    }

    fn end_object_value<W: ?Sized + io::Write>(&mut self, _w: &mut W) -> io::Result<()> {
        if self.suppressed_at == Some(self.stack.len()) {
            self.suppressed_at = None;
        }
        Ok(())
    }
}
