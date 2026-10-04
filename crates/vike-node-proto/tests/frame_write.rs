//! A frame reaches the writer in ONE `write`, and its bytes are what they always were.
//!
//! Two writes (the prefix, then the body) let Nagle hold the body on a socket until the peer ACKs
//! the prefix, which the peer — blocked reading the body — does only when its delayed-ACK timer
//! fires: 40 ms on Linux, per frame. MEASURED 2026-10-03 on a the latency box lane: 82 ms per loopback round
//! trip with two writes, 0.028 ms with one.
//!
//! ⚠ **This is the node wire, so most of this file pins what did NOT change.** The encoder
//! `write_frame` replaced is kept below, verbatim, as `two_write_frame`, and the tests drive BOTH
//! through the same writers and compare everything that reached them — the bytes, the result, the
//! flushes: at every body size from the empty payloads up, around the 4 KiB, 64 KiB and 128 KiB
//! boundaries; through a writer that takes a few bytes per call (`write_all` must loop), one that is
//! interrupted (`write_all` must retry), and one that fails partway as a timed-out or non-blocking
//! socket does (`write_all` must surface it, leaving a PREFIX of the frame and never other bytes).
//! The frame ceiling is pinned at its exact boundary.

use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};

use serde::Serialize;
use vike_node_proto::frame::{MAX_FRAME_LEN, configure_node_stream, read_frame_raw, write_frame};

/// The encoder `write_frame` was until 2026-10-03, VERBATIM: the length prefix and the body as two
/// `write_all` calls. Kept as the reference the one-write encoder must match byte for byte — it is
/// test code, so nothing ships it.
fn two_write_frame<W: Write>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let bytes =
        serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame body exceeds u32 length"))?;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame body of {len} bytes exceeds MAX_FRAME_LEN {MAX_FRAME_LEN}"),
        ));
    }
    w.write_all(&len.to_be_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

/// Records every `write` call it receives.
#[derive(Default)]
struct Recorder {
    writes: Vec<Vec<u8>>,
    flushes: usize,
}

impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writes.push(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn a_frame_reaches_the_writer_in_one_write() {
    let mut w = Recorder::default();
    write_frame(&mut w, &serde_json::json!({"verb": "Ping"})).expect("write");
    assert_eq!(
        w.writes.len(),
        1,
        "a frame written in {} pieces sends its length prefix alone, and Nagle then holds the body \
         for the peer's delayed ACK — 40 ms per frame on Linux",
        w.writes.len()
    );
    assert_eq!(w.flushes, 1, "the frame is still flushed once");
}

#[test]
fn the_bytes_on_the_wire_are_unchanged() {
    let mut w = Recorder::default();
    write_frame(&mut w, &serde_json::json!({"a": 1})).expect("write");
    let wire: Vec<u8> = w.writes.concat();
    assert_eq!(&wire[..4], &7u32.to_be_bytes(), "a big-endian u32 length of the 7-byte body");
    assert_eq!(&wire[4..], br#"{"a":1}"#, "the JSON body, byte for byte");
    let back = read_frame_raw(&mut &wire[..]).expect("decode");
    assert_eq!(back, br#"{"a":1}"#);
}

/// A writer that accepts at most 3 bytes per call — `write_all` must still deliver the whole frame.
struct Trickle(Vec<u8>);

impl Write for Trickle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(3);
        self.0.extend_from_slice(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_short_write_still_delivers_the_whole_frame() {
    let mut w = Trickle(Vec::new());
    write_frame(&mut w, &serde_json::json!({"payload": "x".repeat(100)})).expect("write");
    let back = read_frame_raw(&mut &w.0[..]).expect("decode");
    assert_eq!(back.len(), w.0.len() - 4);
}

/// A writer that behaves like a socket under pressure, configurably:
///
/// * at most `chunk` bytes per `write` — a SHORT write, which `write_all` must loop over;
/// * every `interrupt_every`-th call answers `Interrupted` (0 = never), which `write_all` retries;
/// * `fail_after = Some((n, kind))` accepts `n` bytes in total and then answers `kind` — a
///   timed-out (`TimedOut`) or non-blocking (`WouldBlock`) socket, which `write_all` surfaces;
/// * `flush_fails = Some(kind)` makes `flush` answer `kind`.
#[derive(Debug, Default)]
struct Socketish {
    chunk: usize,
    interrupt_every: usize,
    fail_after: Option<(usize, io::ErrorKind)>,
    flush_fails: Option<io::ErrorKind>,
    out: Vec<u8>,
    calls: usize,
    flushes: usize,
}

impl Socketish {
    fn chunked(chunk: usize) -> Self {
        Self { chunk, ..Self::default() }
    }
}

impl Write for Socketish {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.interrupt_every != 0 && self.calls.is_multiple_of(self.interrupt_every) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut n = buf.len().min(self.chunk);
        if let Some((limit, kind)) = self.fail_after {
            let room = limit.saturating_sub(self.out.len());
            if room == 0 {
                return Err(kind.into());
            }
            n = n.min(room);
        }
        self.out.extend_from_slice(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        match self.flush_fails {
            Some(kind) => Err(kind.into()),
            None => Ok(()),
        }
    }
}

/// Everything an encoder left behind: its result (as kind + message, since `io::Error` is not
/// `PartialEq`), the bytes that reached the writer, and how many flushes it issued.
#[derive(Debug, PartialEq)]
struct Seen {
    result: Result<(), (io::ErrorKind, String)>,
    bytes: Vec<u8>,
    flushes: usize,
}

/// Drive the old and the new encoder through two writers built by `make`, and return both views.
fn old_and_new(msg: &impl Serialize, make: &dyn Fn() -> Socketish) -> (Seen, Seen) {
    let seen = |r: io::Result<()>, w: Socketish| Seen {
        result: r.map_err(|e| (e.kind(), e.to_string())),
        bytes: w.out,
        flushes: w.flushes,
    };
    let mut old = make();
    let r_old = two_write_frame(&mut old, msg);
    let mut new = make();
    let r_new = write_frame(&mut new, msg);
    (seen(r_old, old), seen(r_new, new))
}

/// `assert_eq!` on two `Seen`s would print every byte of a 128 KiB frame; this names the first
/// difference instead.
fn assert_same(label: &str, old: &Seen, new: &Seen) {
    assert_eq!(new.result, old.result, "{label}: the result changed");
    assert_eq!(new.flushes, old.flushes, "{label}: the flush count changed");
    if new.bytes != old.bytes {
        let at = old.bytes.iter().zip(&new.bytes).position(|(a, b)| a != b);
        panic!(
            "{label}: the bytes changed — old {} bytes, new {} bytes, first difference at {at:?}",
            old.bytes.len(),
            new.bytes.len()
        );
    }
}

/// The body lengths a boundary could plausibly care about: every small one, and a window around
/// 4 KiB, 64 KiB (65,535 / 65,536 / 65,537 among them) and 128 KiB.
fn body_lengths() -> Vec<usize> {
    let mut out: Vec<usize> = (2..=70).collect();
    for edge in [4096usize, 65_536, 131_072] {
        out.extend(edge - 6..=edge + 6);
    }
    out
}

/// A JSON string whose ENCODED body is exactly `body` bytes: `body - 2` ASCII characters plus the
/// two quotes.
fn string_of_body(body: usize) -> String {
    "x".repeat(body - 2)
}

#[test]
fn both_encoders_put_the_same_bytes_on_the_wire_at_every_size_and_through_every_writer() {
    // The writer shapes a socket can present to `write_all`: whole, a few bytes at a time, a page,
    // a loopback segment, and short-AND-interrupted.
    let writers: [(&str, &dyn Fn() -> Socketish); 5] = [
        ("whole", &|| Socketish::chunked(usize::MAX)),
        ("3-byte writes", &|| Socketish::chunked(3)),
        ("4 KiB writes", &|| Socketish::chunked(4096)),
        ("64 KiB writes", &|| Socketish::chunked(65_536)),
        ("7-byte writes, every 3rd interrupted", &|| Socketish {
            chunk: 7,
            interrupt_every: 3,
            ..Socketish::default()
        }),
    ];

    // The EMPTY payloads first — the smallest bodies a frame carries — and one that the serializer
    // must escape, so the identity covers `to_vec` against `to_writer` on more than plain ASCII.
    let empties: [(&str, serde_json::Value); 4] = [
        ("null", serde_json::Value::Null),
        ("empty string", serde_json::json!("")),
        ("empty object", serde_json::json!({})),
        ("empty array", serde_json::json!([])),
    ];
    for (writer, make) in writers {
        for (name, value) in &empties {
            let (old, new) = old_and_new(value, make);
            assert!(old.result.is_ok(), "{name}: the reference encoder accepted it");
            assert_same(&format!("{name} through {writer}"), &old, &new);
        }
        let (old, new) = old_and_new(&(), make);
        assert_same(&format!("unit through {writer}"), &old, &new);
        let escaped = "quote\" backslash\\ newline\n tab\t euro\u{20ac} nul\u{0}";
        let (old, new) = old_and_new(&escaped, make);
        assert_same(&format!("escapes through {writer}"), &old, &new);

        for body in body_lengths() {
            let msg = string_of_body(body);
            let (old, new) = old_and_new(&msg, make);
            assert_eq!(old.bytes.len(), body + 4, "guard: the body is {body} bytes as intended");
            assert_same(&format!("{body}-byte body through {writer}"), &old, &new);
            assert_eq!(&new.bytes[..4], &(body as u32).to_be_bytes(), "{body}: the prefix");
            assert_eq!(new.flushes, 1, "{body}: flushed once");
        }
    }
}

/// A socket that stops accepting partway — a write timeout firing, or a non-blocking socket
/// answering `WouldBlock` — leaves EXACTLY the bytes the two-write encoder left, which is a prefix
/// of the frame and nothing else, and the same error. That is what makes the callers' existing rule
/// ("a write fault mid-frame desyncs the stream, so close it") still the whole story: one write
/// cannot leave anything two writes could not.
///
/// "Exactly" holds for this scripted writer, which stops at the same byte whatever the call
/// pattern. On a real socket the kernel decides where a write stops, so the claim there is the
/// weaker one `write_frame`'s doc makes: a prefix of the frame, as the two-write form could leave.
#[test]
fn a_write_that_fails_partway_leaves_the_same_prefix_and_the_same_error() {
    let msg = string_of_body(100);
    let frame_len = 104;
    let mut full = Vec::new();
    two_write_frame(&mut full, &msg).expect("the reference frame");
    assert_eq!(full.len(), frame_len, "guard: a 100-byte body");

    for kind in [io::ErrorKind::TimedOut, io::ErrorKind::WouldBlock] {
        for chunk in [usize::MAX, 3] {
            for fail_after in 0..=frame_len {
                let make = || Socketish {
                    chunk,
                    fail_after: Some((fail_after, kind)),
                    ..Socketish::default()
                };
                let (old, new) = old_and_new(&msg, &make);
                let label = format!("{kind:?} after {fail_after} bytes, {chunk}-byte writes");
                assert_same(&label, &old, &new);
                assert_eq!(new.bytes, full[..fail_after], "{label}: a prefix of the frame");
                if fail_after < frame_len {
                    assert_eq!(new.result.as_ref().map_err(|e| e.0), Err(kind), "{label}");
                    assert_eq!(new.flushes, 0, "{label}: a failed frame is not flushed");
                } else {
                    assert_eq!(new.result, Ok(()), "{label}: the whole frame fitted");
                }
            }
        }
    }

    // A flush that fails surfaces after the whole frame was written, from both encoders alike.
    let make = || Socketish {
        chunk: usize::MAX,
        flush_fails: Some(io::ErrorKind::BrokenPipe),
        ..Socketish::default()
    };
    let (old, new) = old_and_new(&msg, &make);
    assert_same("a failing flush", &old, &new);
    assert_eq!(new.bytes, full, "the frame was written before the flush failed");
    assert_eq!(new.result.as_ref().map_err(|e| e.0), Err(io::ErrorKind::BrokenPipe));
}

/// Counts what reached it without keeping a 64 MiB copy: calls, bytes, the first four bytes, flushes.
#[derive(Default)]
struct Tally {
    calls: usize,
    bytes: usize,
    head: Vec<u8>,
    flushes: usize,
}

impl Write for Tally {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        let want = 4usize.saturating_sub(self.head.len()).min(buf.len());
        self.head.extend_from_slice(&buf[..want]);
        self.bytes += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

/// The ceiling is where it was, at its exact boundary: a body of exactly [`MAX_FRAME_LEN`] bytes is
/// written (in one write), one byte more is refused as `InvalidData` naming both numbers — the
/// reference encoder's message, verbatim — and NOTHING reaches the writer, not even the prefix.
///
/// It serializes 64 MiB twice, which costs about a second at opt-level 0; the boundary cannot be
/// moved closer, because the constant is the thing under test.
#[test]
fn the_frame_ceiling_is_where_it_was_to_the_byte() {
    let max = MAX_FRAME_LEN as usize;
    let mut body = String::with_capacity(max);
    body.push_str(&"x".repeat(max - 2));

    let mut at = Tally::default();
    write_frame(&mut at, &body).expect("a body of exactly MAX_FRAME_LEN bytes is a frame");
    assert_eq!(at.calls, 1, "one write, at the ceiling too");
    assert_eq!(at.bytes, max + 4, "prefix + MAX_FRAME_LEN body bytes");
    assert_eq!(at.head, MAX_FRAME_LEN.to_be_bytes(), "the prefix declares MAX_FRAME_LEN");
    assert_eq!(at.flushes, 1);

    body.push('x');
    let mut over = Tally::default();
    let err = write_frame(&mut over, &body).expect_err("one byte over the ceiling is refused");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert_eq!(
        err.to_string(),
        format!("frame body of {} bytes exceeds MAX_FRAME_LEN {MAX_FRAME_LEN}", max + 1),
        "the refusal's message is the reference encoder's, verbatim"
    );
    assert_eq!(
        (over.calls, over.flushes),
        (0, 0),
        "a refused frame writes nothing at all — no prefix to desync the stream"
    );
}

/// [`configure_node_stream`] turns Nagle OFF on both ends of a real loopback connection. The guard
/// first: a socket fresh from the OS has Nagle on, or this would prove nothing.
#[test]
fn a_configured_node_stream_has_nagle_off() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let (served, _) = listener.accept().expect("accept");
    for (end, stream) in [("client", &client), ("served", &served)] {
        assert!(
            !stream.nodelay().expect("read TCP_NODELAY"),
            "guard: the {end} end starts with Nagle on"
        );
        configure_node_stream(stream).expect("configure the node stream");
        assert!(
            stream.nodelay().expect("read TCP_NODELAY"),
            "the {end} end must carry TCP_NODELAY once configured"
        );
    }
}
