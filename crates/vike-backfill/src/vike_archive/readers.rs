//! The two `ChunkReader`s the ingest pipeline reads through: ranged HTTP and a positional local file.

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::Arc;

use bytes::Bytes;
use datafusion::parquet::errors::ParquetError;
use datafusion::parquet::file::reader::{ChunkReader, Length};

use super::{API_KEY_HEADER, CTX};
use crate::error::CollectError;

// ---- ranged-HTTP ChunkReader --------------------------------------------------------------------

/// A `parquet::file::reader::ChunkReader` over one archive Parquet file, reached entirely through
/// blocking `Range:`-headered GETs (`ureq`, the workspace-pinned blocking HTTP stack — no new
/// dep). Built once per (date, stream); `len()` comes from a single `HEAD` at construction, and
/// every subsequent `get_bytes` issues exactly one ranged GET for the requested span. Cloning the
/// underlying `ureq::Agent` is cheap (pool-backed), so this struct is cheap to construct per file —
/// and cheap to `Clone` outright (`derive`d below): the streaming ingest (`ingest_stream_over`)
/// clones one already-`open`ed reader once per selected row group, rather than re-`HEAD`ing the URL.
#[derive(Clone)]
pub struct HttpRangeReader {
    agent: ureq::Agent,
    url: String,
    api_key: Option<String>,
    length: u64,
}

impl HttpRangeReader {
    /// `HEAD`s `url` to learn its total length (`Content-Length`), then returns a reader ready for
    /// ranged `get_bytes` calls. The `X-API-Key` header (if `api_key` is `Some`) rides both the
    /// `HEAD` and every subsequent ranged `GET` — harmless on the open `manifest.json`/`samples/`
    /// endpoints, required on the date-partitioned files.
    pub fn open(
        agent: ureq::Agent,
        url: String,
        api_key: Option<String>,
    ) -> Result<Self, CollectError> {
        let mut req = agent.head(&url);
        if let Some(k) = &api_key {
            req = req.header(API_KEY_HEADER, k);
        }
        let resp = req.call().map_err(|e| CollectError::Fetch(format!("{CTX} HEAD {url}: {e}")))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(CollectError::Fetch(format!("{CTX} HEAD {url}: HTTP {status}")));
        }
        let length: u64 = resp
            .headers()
            .get("Content-Length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                CollectError::Fetch(format!("{CTX} HEAD {url}: missing/unparsable Content-Length"))
            })?;
        Ok(Self { agent, url, api_key, length })
    }
}

impl Length for HttpRangeReader {
    fn len(&self) -> u64 {
        self.length
    }
}

/// A conservative cap on how many bytes [`HttpRangeReader::get_read`] eagerly fetches from its
/// requested `start` offset. Parquet's synchronous page reader (`serialized_reader.rs`'s
/// `SerializedPageReader`, the offset-based state used whenever a file carries no page/offset
/// index — true of every real archive file, which is why this was reachable) calls `get_read`
/// ONLY to read one page's HEADER at a time — a Thrift-compact-encoded struct, always a tiny
/// fraction of a page even with embedded column statistics — before switching to a PRECISE
/// `get_bytes(offset, exact_len)` call for that page's actual (possibly multi-MB) data. The
/// earlier implementation instead fetched EVERYTHING from `start` to end-of-file on EVERY
/// `get_read` call (`length - start` bytes): for a page near the start of a multi-GB archive
/// file, "the rest of the file" is nearly the WHOLE FILE, materialized into one `Bytes` buffer —
/// repeated once per page. This (not the per-symbol accumulation the rest of this module's
/// streaming rewrite fixes) turned out to be the actual root cause of the reported unbounded-RSS
/// defect: decode-time memory scaled with total file size, not with how much data a `get_read`
/// caller actually consumed. 1 MiB is a wide safety margin over any realistic page header while
/// keeping the eager-fetch window trivially bounded regardless of file or row-group size; the
/// call cadence (one `get_read` per page) is UNCHANGED from before — only the size fetched is.
const GET_READ_WINDOW_BYTES: u64 = 1024 * 1024;

/// The mean row group of the largest archive stream, from the vendor's own numbers quoted in this
/// module's intro: a `book_events` day is ~7.4 GB over 453 row groups.
const MEAN_ROW_GROUP_BYTES: u64 = 7_400_000_000 / 453; // ~16.3 MB

/// The ceiling on ONE [`HttpRangeReader::get_bytes`] buffer — and the reason the response body is
/// read through [`Read::take`] rather than a bare `read_to_end`.
///
/// `length` is not ours. It originates in the file's own Parquet footer, and
/// `ParquetMetaDataReader::parse_metadata` validates it against exactly one thing:
/// `ChunkReader::len()` — which for this reader is whatever `Content-Length` the server answered
/// our `HEAD` with ([`HttpRangeReader::open`]). A host serving a hostile or merely corrupt file
/// therefore controls BOTH sides of that check, and it used to flow straight into
/// `Vec::with_capacity(length)` — which does NOT return an error when the allocator refuses; it
/// calls `handle_alloc_error` and ABORTS the process — followed by a `read_to_end` with no bound
/// at all, which would keep growing the buffer for as long as the server kept sending. The guard
/// below therefore runs BEFORE the allocation and before the socket is dialled: placed after
/// `with_capacity` it would be unreachable in exactly the case it exists for.
///
/// `--base` defaults to first-party `data.vike.io`, so this hardens against a hostile or
/// misconfigured OVERRIDE, not a live exploit. It is the same unbounded-RSS family as
/// [`GET_READ_WINDOW_BYTES`] above, which this module already had to fix once.
///
/// Sized from what a legitimate request actually is, not from a round number. Parquet's
/// synchronous path calls `get_bytes` for exactly three things: the 8-byte footer tail, the footer
/// metadata (~1.17 MB, verified live — module intro), and ONE data page at a time
/// (`serialized_reader.rs`'s `SerializedPageReader`). A page sits inside a column chunk, which sits
/// inside a row group, so ONE ROW GROUP is a true upper bound on any legitimate single call. 16x
/// the measured mean row group covers a badly skewed group, or a future archive written with much
/// larger ones, while still refusing a request two orders of magnitude past anything this reader
/// has ever legitimately made.
const MAX_CHUNK_BYTES: u64 = 16 * MEAN_ROW_GROUP_BYTES; // ~261 MB

/// The cap must stay clear of the working range: an edit that tightened it to within a skewed row
/// group would start refusing legitimate page reads. Compile error, not a runtime one.
const _: () = assert!(MAX_CHUNK_BYTES > 8 * MEAN_ROW_GROUP_BYTES);

impl ChunkReader for HttpRangeReader {
    type T = Cursor<Bytes>;

    fn get_read(&self, start: u64) -> datafusion::parquet::errors::Result<Self::T> {
        let window = GET_READ_WINDOW_BYTES.min(self.length.saturating_sub(start));
        Ok(Cursor::new(self.get_bytes(start, window as usize)?))
    }

    fn get_bytes(&self, start: u64, length: usize) -> datafusion::parquet::errors::Result<Bytes> {
        if length == 0 {
            return Ok(Bytes::new());
        }
        // BEFORE the allocation and before the dial — see [`MAX_CHUNK_BYTES`]. A clean `Err` here
        // is the whole point: `Vec::with_capacity` on an implausible length aborts the process
        // instead of returning, so a caller would never get the chance to handle it.
        if length as u64 > MAX_CHUNK_BYTES {
            return Err(ParquetError::General(format!(
                "{CTX} range GET {}: refusing a {length}-byte chunk at offset {start} (cap \
                 {MAX_CHUNK_BYTES}) — the file's Parquet footer or the server's Content-Length \
                 ({}) is implausible",
                self.url, self.length
            )));
        }
        let end = start + length as u64 - 1;
        let range = format!("bytes={start}-{end}");
        let mut req = self.agent.get(&self.url).header("Range", &range);
        if let Some(k) = &self.api_key {
            req = req.header(API_KEY_HEADER, k);
        }
        let mut resp = req.call().map_err(|e| {
            ParquetError::General(format!("{CTX} range GET {} [{range}]: {e}", self.url))
        })?;
        let status = resp.status().as_u16();
        // 206 Partial Content is the expected case; a plain 200 (server ignored the Range header
        // and sent the whole body) is tolerated too — the length check below still catches a
        // mismatch, just as an EOF rather than a silently-wrong slice.
        if status != 206 && status != 200 {
            return Err(ParquetError::General(format!(
                "{CTX} range GET {} [{range}]: HTTP {status}",
                self.url
            )));
        }
        // `length` is capped above, so `with_capacity` can no longer abort. `take(length)` is what
        // bounds the OTHER direction: a server that answers a small `Range` with a huge body can no
        // longer grow this buffer past what we actually asked for. The short-body check below is
        // unchanged and still catches the opposite failure.
        let mut buf = Vec::with_capacity(length);
        resp.body_mut()
            .as_reader()
            .take(length as u64)
            .read_to_end(&mut buf)
            .map_err(|e| ParquetError::General(format!("{CTX} read body {}: {e}", self.url)))?;
        if buf.len() != length {
            return Err(ParquetError::EOF(format!(
                "{CTX} range GET {} [{range}]: expected {length} bytes, got {}",
                self.url,
                buf.len()
            )));
        }
        Ok(Bytes::from(buf))
    }
}

// ---- local-file ChunkReader (the `--file` path: same pipeline, no network) ----------------------

/// A `ChunkReader` over a Parquet file already sitting on local disk — the local-ingest twin of
/// `HttpRangeReader`. Feeds the EXACT SAME `ingest_stream_over`/`ingest_stream_bulk_over`
/// row-group-at-a-time pipeline (decode → map → commit/stage) as the HTTP path, so a
/// locally-downloaded file and a range-fetched URL for the same (date, stream) produce
/// byte-identical store output — the reader is the only thing that differs. Wrapping in
/// `Arc<File>` (which IS unconditionally `Clone`, regardless of what `File` itself is — only a
/// fallible `try_clone`) satisfies the streaming core's `T: ChunkReader + Clone` bound, sharing one
/// open file description across row groups exactly as `HttpRangeReader::clone` shares one pooled
/// connection.
///
/// **Reads are positional (`read_at`/`seek_read`), NOT `parquet::file::reader`'s own
/// `impl ChunkReader for File`** (`self.try_clone()?.seek(start)?.read(...)`) — deliberately: a
/// `File::try_clone()` (`dup(2)` on unix, `DuplicateHandle` on Windows) shares the SAME
/// open-file-description, and therefore the SAME file-offset cursor, as the handle it cloned from.
/// That's harmless for the SERIAL local-file path (one row group's reads always complete before
/// the next begins — no concurrent seeks to race), but `ingest_local_file_parallel`'s worker
/// threads all clone the ONE `Arc<File>` this struct wraps and issue seeks/reads concurrently: one
/// thread's `seek` can move the shared cursor a second thread is mid-`read` against, silently
/// splicing two threads' reads together. Reproduced live on the CI box the first time this module ran
/// with `--jobs 4` (never under `--jobs 1`, and only intermittently, per the race): a Parquet page
/// bytes were read at the WRONG offset and decompression failed with "External: Unknown frame
/// descriptor". `read_at`/`seek_read` take the offset as an explicit syscall argument and never
/// touch the shared cursor — safe for any number of concurrent callers over the same `Arc<File>`.
#[derive(Clone)]
pub struct LocalFileReader(Arc<File>);

impl LocalFileReader {
    /// Opens `path` for reading. No row data is read here — like [`HttpRangeReader::open`], the
    /// footer and every row group are read lazily by the Parquet reader built on top of this type.
    pub fn open(path: &Path) -> Result<Self, CollectError> {
        let file = File::open(path).map_err(|e| {
            CollectError::Fetch(format!("{CTX} open local file {}: {e}", path.display()))
        })?;
        Ok(Self(Arc::new(file)))
    }
}

impl Length for LocalFileReader {
    fn len(&self) -> u64 {
        Length::len(&*self.0)
    }
}

/// Positional read at `pos` into `buf`, returning the number of bytes read (`0` at EOF) — never
/// touches the file's shared cursor (see [`LocalFileReader`]'s doc for why that matters under
/// concurrent access). `unix`'s `read_at`/Windows' `seek_read` are both true pread-style syscalls.
#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, pos)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, pos)
}

/// A `Read` over a shared `Arc<File>`, advancing an OWN (per-instance, not OS-shared) position via
/// [`read_at`] on each call — the concurrency-safe replacement for `BufReader<File>`'s
/// shared-cursor `seek`+`read`. Returned by [`LocalFileReader::get_read`]; every instance owns its
/// `pos` independently, so `N` of these live concurrently over the same `Arc<File>` (one per
/// in-flight row group under `ingest_local_file_parallel`) without interfering.
pub struct PositionedReader {
    file: Arc<File>,
    pos: u64,
}

impl Read for PositionedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = read_at(&self.file, buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl ChunkReader for LocalFileReader {
    type T = PositionedReader;

    fn get_read(&self, start: u64) -> datafusion::parquet::errors::Result<Self::T> {
        Ok(PositionedReader { file: self.0.clone(), pos: start })
    }

    fn get_bytes(&self, start: u64, length: usize) -> datafusion::parquet::errors::Result<Bytes> {
        let mut buf = vec![0u8; length];
        let mut filled = 0usize;
        while filled < length {
            let n = read_at(&self.0, &mut buf[filled..], start + filled as u64).map_err(|e| {
                ParquetError::General(format!(
                    "{CTX} local file read at offset {}: {e}",
                    start + filled as u64
                ))
            })?;
            if n == 0 {
                return Err(ParquetError::EOF(format!(
                    "{CTX} local file: expected {length} bytes at offset {start}, got only {filled}"
                )));
            }
            filled += n;
        }
        Ok(Bytes::from(buf))
    }
}

#[cfg(test)]
#[path = "readers_tests.rs"]
mod readers_tests;
