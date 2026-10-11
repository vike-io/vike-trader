//! The two readers: `LocalFileReader` against the bytes path, and `HttpRangeReader`'s bounded `get_bytes`.

use super::*;
use crate::vike_archive::datasets::Stream;
use crate::vike_archive::ingest_stream_over;
use crate::vike_archive::test_support::{BookRow, write_book_events_parquet};
use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::assert_matches;
use std::sync::atomic::{AtomicUsize, Ordering};
use vike_data::store::archive_store::VENUE;
use vike_data::{DataFusionHist, HistStore, TsRange};

// ---- local-file ChunkReader: proves the reader is the ONLY difference from the URL path -----

/// Opening a path that doesn't exist reports a `CollectError`, not a panic — same "one bad
/// file must not crash the whole run" posture the rest of this module holds elsewhere.
#[test]
fn local_file_reader_open_reports_a_missing_path() {
    let path = std::env::temp_dir().join("vike_archive_no_such_local_file.parquet");
    assert!(!path.exists());
    assert!(LocalFileReader::open(&path).is_err());
}

/// The headline correctness proof for the `--file` feature: the SAME Parquet bytes ingested
/// once through [`LocalFileReader`] (the new local path) and once through a plain `Bytes`
/// reader (standing in for [`HttpRangeReader`] — both are nothing more than `ChunkReader`
/// impls feeding the identical [`ingest_stream_over`] pipeline, and `Bytes` is exactly the
/// reader type every other streaming test in this module already exercises that pipeline
/// with) must decode into byte-identical mapped rows, proving the reader swap is the ONLY
/// difference between the two ingest paths — nothing about decode/mapping/commit-keying
/// changed.
#[test]
fn local_file_reader_and_a_bytes_reader_ingest_identical_rows() {
    let rows: Vec<BookRow<'_>> = vec![
        ("TOK_A", 1, 1, 1, "book", "none", 0.0, 0.0, "[[0.5,10.0]]", "[[0.51,10.0]]", 0.01, ""),
        ("TOK_A", 2, 2, 2, "price_change", "buy", 0.42, 7.0, "", "", 0.01, ""),
        ("TOK_B", 3, 3, 1, "book", "none", 0.0, 0.0, "[[0.4,20.0]]", "[[0.41,20.0]]", 0.01, ""),
    ];
    let bytes = write_book_events_parquet(&rows, 1);

    // "URL-equivalent" path: ingest straight from the in-memory `Bytes` reader.
    let url_dir = tempfile::tempdir().unwrap();
    let url_store = DataFusionHist::open(url_dir.path()).unwrap();
    let url_total =
        ingest_stream_over(bytes.clone(), &url_store, "2026-07-27", Stream::Book, None, |_| {})
            .unwrap();

    // Local-file path: the same bytes, written to a real file on disk, opened through
    // `LocalFileReader` — the only thing that differs from the block above.
    let file_dir = tempfile::tempdir().unwrap();
    let file_path = file_dir.path().join("book_events.parquet");
    std::fs::write(&file_path, &bytes).unwrap();
    let local_store_dir = tempfile::tempdir().unwrap();
    let local_store = DataFusionHist::open(local_store_dir.path()).unwrap();
    let local_reader = LocalFileReader::open(&file_path).unwrap();
    assert_eq!(local_reader.len(), bytes.len() as u64, "LocalFileReader sees the real file size");
    let local_total =
        ingest_stream_over(local_reader, &local_store, "2026-07-27", Stream::Book, None, |_| {})
            .unwrap();

    assert_eq!(url_total, local_total, "both readers decode the same row count");
    assert_eq!(url_total, 3);

    for sym in ["TOK_A", "TOK_B"] {
        let from_url = url_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        let from_local = local_store.scan_book_updates(VENUE, sym, TsRange::all()).unwrap();
        assert_eq!(
            from_url.len(),
            from_local.len(),
            "same row count for {sym} regardless of reader"
        );
        for (a, b) in from_url.iter().zip(from_local.iter()) {
            assert_eq!(a.ts, b.ts, "{sym}: ts must match");
            assert_eq!(a.local_ts, b.local_ts, "{sym}: local_ts must match");
            assert_eq!(a.seq, b.seq, "{sym}: seq must match");
            assert_eq!(a.kind, b.kind, "{sym}: kind must match");
            assert_eq!(a.tick_size, b.tick_size, "{sym}: tick_size must match");
            assert_eq!(a.bids, b.bids, "{sym}: bids must match");
            assert_eq!(a.asks, b.asks, "{sym}: asks must match");
            assert_eq!(a.symbol, b.symbol, "{sym}: symbol must match");
        }
    }
}

// ---- HttpRangeReader: the get_bytes buffer is bounded whatever the server claims ------------
//
// These drive a LOOPBACK origin that lies, because the whole point of the cap is that the two
// numbers parquet cross-checks — the footer's `metadata_len` and `ChunkReader::len()` — both
// come from the server. Every assertion is on bytes the client actually BUFFERED (the returned
// `Bytes`) or on whether a request was dialled at all, never merely on "an error came back":
// the pre-cap code also returned an error in two of these three cases, just after allocating.

/// A minimal HTTP/1.1 origin on loopback. `HEAD` answers `claimed_len` as `Content-Length` —
/// the number a hostile or misconfigured host controls, and the ONLY thing parquet's own footer
/// validation checks a `get_bytes` length against. `GET` parses `Range: bytes=A-B` and replies
/// `206` with whatever `body_for(a, b)` returns, which the caller is free to make SHORTER or
/// LONGER than the range asked for. `gets` counts dialled range requests, so a test can assert
/// that a refusal happened before any socket was opened.
struct LyingOrigin {
    url: String,
    gets: Arc<AtomicUsize>,
}

fn spawn_lying_origin(
    claimed_len: u64,
    body_for: impl Fn(u64, u64) -> Vec<u8> + Send + Sync + 'static,
) -> LyingOrigin {
    use std::io::Write as _;
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let gets = Arc::new(AtomicUsize::new(0));
    let gets_srv = Arc::clone(&gets);

    // Daemon thread: the test process exits without joining it. Each connection is answered
    // once and closed (`Connection: close`), so there is no keep-alive state to get wrong.
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut sock) = stream else { break };
            // Read just the request head — everything up to the blank line.
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match sock.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let head = String::from_utf8_lossy(&head).to_string();
            if head.starts_with("HEAD ") {
                let _ = write!(
                    sock,
                    "HTTP/1.1 200 OK\r\nContent-Length: {claimed_len}\r\n\
                         Accept-Ranges: bytes\r\nConnection: close\r\n\r\n"
                );
                continue;
            }
            gets_srv.fetch_add(1, Ordering::SeqCst);
            // `Range: bytes=A-B` — the reader always sends an explicit closed range.
            let (a, b) = head
                .split("bytes=")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|r| r.split_once('-'))
                .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
                .unwrap_or((0u64, 0u64));
            let body = body_for(a, b);
            let _ = write!(
                sock,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{claimed_len}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            // The over-send case abandons this write once the client stops reading; a broken
            // pipe here is the EXPECTED outcome, not a test failure.
            let _ = sock.write_all(&body);
        }
    });

    LyingOrigin { url: format!("http://127.0.0.1:{port}/book_events.parquet"), gets }
}

fn test_agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).build().new_agent()
}

/// A server that answers a small `Range` with a MUCH larger body cannot grow the buffer past
/// what was asked for. This is the case `Read::take` exists for, and the one that most cleanly
/// separates the two implementations: with the bare `read_to_end` this test buffered all 8 MiB
/// and then failed the `buf.len() != length` check, so it would have gone RED on the exact
/// symptom (an 8 MiB allocation for a 4 KiB request) rather than on a cosmetic difference.
#[test]
fn get_bytes_stops_at_the_requested_length_when_the_server_over_sends() {
    const ASKED: usize = 4096;
    const OFFERED: usize = 8 * 1024 * 1024;
    let origin = spawn_lying_origin(64 * 1024 * 1024, |_a, _b| vec![0xABu8; OFFERED]);
    let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
        .expect("HEAD the lying origin");

    let got = reader.get_bytes(0, ASKED).expect("a bounded read still succeeds");

    // The direct measurement of bytes buffered: exactly what we asked for, out of 8 MiB
    // offered. 2048x less than the server was willing to hand over.
    assert_eq!(got.len(), ASKED, "buffer must be bounded by the requested length");
    assert!(got.iter().all(|b| *b == 0xAB), "and it is the server's actual bytes");
}

/// The opposite failure — a server that claims more than it sends — must still be a clean,
/// bounded `Err`, and must not be papered over by the `take` above into a silently short slice.
#[test]
fn get_bytes_errors_bounded_when_the_server_under_sends() {
    const ASKED: usize = 4096;
    const SENT: usize = 100;
    let origin = spawn_lying_origin(64 * 1024 * 1024, |_a, _b| vec![0x11u8; SENT]);
    let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
        .expect("HEAD the lying origin");

    let err = reader.get_bytes(0, ASKED).expect_err("a short body is an error");

    assert_matches!(err, ParquetError::EOF(_), "short body is EOF, not General: {err}");
    // The error names how much actually arrived — i.e. the buffer stopped at the 100 bytes the
    // server sent, not at the 4096 it claimed.
    let msg = err.to_string();
    assert!(msg.contains("expected 4096 bytes, got 100"), "unexpected message: {msg}");
}

/// An absurd `Content-Length` is refused BEFORE anything is allocated or dialled. `gets == 0`
/// is the assertion that matters: it proves the cap short-circuits ahead of
/// `Vec::with_capacity`, which on a length this size does not return an `Err` at all — it calls
/// `handle_alloc_error` and aborts the process.
#[test]
fn get_bytes_refuses_an_absurd_length_before_allocating_or_dialling() {
    let absurd = MAX_CHUNK_BYTES + 1;
    let origin = spawn_lying_origin(1 << 40, |_a, _b| Vec::new());
    let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
        .expect("HEAD the lying origin");
    assert_eq!(reader.len(), 1 << 40, "the reader believes the server's Content-Length");

    let err = reader.get_bytes(0, absurd as usize).expect_err("must refuse");

    assert_eq!(origin.gets.load(Ordering::SeqCst), 0, "no request may be dialled");
    let msg = err.to_string();
    assert!(msg.contains("refusing"), "unexpected message: {msg}");
    assert!(msg.contains(&MAX_CHUNK_BYTES.to_string()), "message must name the cap: {msg}");
}

/// The end-to-end shape this cap exists for: a hostile FILE, not a hostile caller. The last 8
/// bytes of a Parquet file are `[metadata_len: u32 LE][b"PAR1"]`, and
/// `ParquetMetaDataReader::parse_metadata` accepts any `metadata_len` that fits inside
/// `ChunkReader::len()` — which here is the server's own `Content-Length`. A host controlling
/// both therefore walks parquet straight into `get_bytes(_, 4_294_967_295)`. Without the cap
/// that is a 4 GiB `Vec::with_capacity`; with it, a clean `Err` and no second request.
#[test]
fn a_hostile_footer_cannot_drive_a_multi_gigabyte_read() {
    let claimed_len: u64 = 5_000_000_000;
    let mut footer = Vec::new();
    footer.extend_from_slice(&u32::MAX.to_le_bytes()); // metadata_len = 4_294_967_295
    footer.extend_from_slice(b"PAR1");
    let origin = spawn_lying_origin(claimed_len, move |a, b| {
        // Only the footer tail is ever served; any other range gets nothing, which is enough
        // because the cap must fire on the very next call.
        if a == claimed_len - 8 && b == claimed_len - 1 { footer.clone() } else { Vec::new() }
    });
    let reader = HttpRangeReader::open(test_agent(), origin.url.clone(), None)
        .expect("HEAD the lying origin");

    let err = ParquetRecordBatchReaderBuilder::try_new(reader)
        .err()
        .expect("a 4 GiB footer must not be honoured");

    let msg = err.to_string();
    assert!(msg.contains("refusing"), "the cap must be what stopped it, got: {msg}");
    assert!(msg.contains("4294967295"), "and it must name the refused size: {msg}");
    // Exactly one range GET: the 8-byte footer tail. The 4 GiB follow-up never left the process.
    assert_eq!(origin.gets.load(Ordering::SeqCst), 1, "the huge read was never dialled");
}
