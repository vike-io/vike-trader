//! Test helpers shared by this crate's `*_tests.rs` files, kept in ONE place so no suite carries
//! its own copy. `#[cfg(test)]` at the `mod` declaration, so a normal build never compiles it.

use std::path::{Path, PathBuf};

use crate::JournalFileConfig;

/// A scratch directory owned by the test that allocated it, removed when the guard drops.
///
/// ⚠ **A SECOND guard, and the duplication is forced rather than chosen.** The original is
/// `crates/vike-core/src/scratch.rs`'s `Scratch`, and these very suites are why it exists: they
/// leaked **27,471 `/tmp/vjl-*` directories totalling 211 GB** on the the CI box CI box (measured
/// 2026-08-23). It could not come with them, and no edge reaches it: that module is declared
/// `#[cfg(test)] mod scratch;` and the type is `pub(crate)`, so it is invisible across a crate
/// boundary on a NORMAL dependency and on a dev one alike. Its own doc refuses the obvious cure
/// in advance — "no `test-support` feature to thread through CI — vike-core declares no features at
/// all, and a feature the derived roster lane does not pass would leave the `tests/` binaries
/// uncovered".
///
/// The copy is cheap BECAUSE the gate splits the same way:
/// `crates/vike-ops/tests/hygiene/journal_scratch_gate.rs` holds its STRICT one-guard-file rule over
/// `crates/vike-core` ALONE (its `tree_sources` skips that directory by NAME), and every other
/// crate answers to the PROPERTY rule — show a self-deleting handle, or be pinned. This type owns
/// a `tempfile::TempDir`, which is what that rule asks for. The `vjl-` prefix is kept
/// deliberately: a directory found on the CI box still names the family that leaked, whichever
/// crate minted it.
///
/// ⚠ `vike_paths::scratch::ScratchDir` is NOT the answer here, and the question is worth
/// answering in advance, because it is fully `pub`, this crate already depends on vike-model, and
/// it owns a `Drop` that removes its directory. It is a different JOB: the retention rule for
/// `<project>/tmp`, deliberately pure — `create_in(root, tag)` takes the root from a composition
/// root and reads no environment — so a test reaching for it would have to mint the temp root
/// itself, which is the whole of what `tempfile` is doing below.
#[derive(Debug)]
pub(super) struct Scratch {
    /// The owning temp root. Never read — held so its `Drop` runs at the end of the test.
    _root: tempfile::TempDir,
    /// The path handed to the code under test: the root itself, or a child of it.
    path: PathBuf,
}

impl Scratch {
    /// A path INSIDE a fresh temp root that does **not** exist yet — for the suites whose code
    /// under test calls `create_dir_all` itself, and the ones that assert on a directory which
    /// never appears. The root is still owned, so whatever the test creates there is cleaned up.
    pub(super) fn reserved(tag: &str) -> Self {
        let root = Self::root(tag);
        let path = root.path().join("j");
        Self { _root: root, path }
    }

    /// A directory that already EXISTS — for the suites that need it in place before the code
    /// under test runs.
    pub(super) fn created(tag: &str) -> Self {
        let root = Self::root(tag);
        let path = root.path().to_path_buf();
        Self { _root: root, path }
    }

    /// The temp root, prefixed with the caller's tag so a directory seen mid-run is attributable.
    fn root(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("vjl-{tag}-"))
            .tempdir()
            .expect("create scratch temp dir")
    }
}

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

/// A scratch journal directory that **exists on entry and is removed when the returned guard
/// drops**. Hold it for the whole test: binding it to `_` deletes the directory immediately.
/// It used to hand back a bare `PathBuf` and leak the directory forever — see
/// `crates/vike-core/src/scratch.rs` for the 211 GB that cost.
pub(super) fn tmp_dir(tag: &str) -> Scratch {
    Scratch::created(&format!("test-{tag}"))
}

/// The small-segment config most suites open with: 64 KiB segments, one flush per append (the
/// default 64 MiB pre-allocation is pointless for a test directory).
pub(super) fn small_config() -> JournalFileConfig {
    JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 1 }
}

pub(super) fn ingest(n: u64) -> vike_execution::Ingest {
    vike_execution::Ingest::Command(vike_execution::Command::Order(
        vike_execution::OrderIntent::Cancel(format!("c{n}")),
    ))
}

/// A deliberately FAT ingest (~4 KiB framed) so a sync test can cross several `sync_chunk`
/// boundaries — whose FLOOR is `SYNC_CHUNK_MIN` = 4 MiB — in ~1 000 appends instead of ~50 000.
pub(super) fn fat_ingest(n: u64) -> vike_execution::Ingest {
    vike_execution::Ingest::Command(vike_execution::Command::Order(
        vike_execution::OrderIntent::Cancel(format!("c{n}-{}", "x".repeat(4000))),
    ))
}

/// A minimal single-engine `Snapshot` payload for the prune tests (the engine state itself is
/// irrelevant — pruning keys only off record sequences, not snapshot contents).
pub(super) fn snap_engines() -> Vec<vike_execution::EngineSnapshot> {
    use vike_execution::testing::RecordingClient;
    use vike_execution::{Account, BalanceMode, Engine, RiskGate};
    use vike_model::RiskLimits;
    let e = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    vec![e.snapshot_state()]
}

/// An archive policy that keeps nothing in the live directory and has no limit.
pub(super) fn keep_nothing() -> crate::JournalArchivePolicy {
    crate::JournalArchivePolicy {
        keep_ms: 0,
        max_bytes: u64::MAX,
        max_age_ms: i64::MAX,
        at_limit: vike_model::JournalArchiveFull::Keep,
    }
}

/// A mark past every segment: what a directory nothing writes any more passes the prune.
pub(super) const EVERY_SEGMENT_DURABLE: crate::JournalDurableMark =
    crate::JournalDurableMark { segment_index: u64::MAX, offset: usize::MAX };

/// The prune of a closed journal with [`keep_nothing`]: every superseded segment the stream's floor lets go
/// moves to the archive.
pub(super) fn prune_closed(dir: &Path) -> std::io::Result<crate::JournalArchivePass> {
    crate::Journal::prune_before_latest_snap(dir, &keep_nothing(), i64::MAX, EVERY_SEGMENT_DURABLE)
}

/// The `.vjl` segment file paths currently in `dir`.
pub(super) fn segment_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            (p.extension().and_then(|s| s.to_str()) == Some("vjl")).then_some(p)
        })
        .collect()
}

/// The JSON text a build before format 34 wrote for the record whose msgpack is `payload` (a JSON payload is
/// returned as it is): the schema tests read a record's KEYS and their order, and an older build's reader parses
/// this text. A straight transcode, in the payload's own order: both encodings are the one serde data model
/// (`crate::frame`'s `encode_into` writes structs as maps of their field names, in the human-readable mode), and
/// every scalar is formatted by `serde_json` itself, so the text is the bytes that build wrote (a NaN as `null`).
pub(super) fn payload_json(payload: &[u8]) -> String {
    if payload.first() == Some(&b'{') {
        return String::from_utf8(payload.to_vec()).expect("a JSON payload is text");
    }
    let mut out = String::new();
    let rest = transcode(payload, &mut out);
    assert!(rest.is_empty(), "one value per payload ({} bytes left)", rest.len());
    out
}

/// A scalar's JSON text, through `serde_json` itself, so the formatting is the JSON writer's.
fn json<T: serde::Serialize + ?Sized>(out: &mut String, v: &T) {
    out.push_str(&serde_json::to_string(v).expect("a scalar serializes"));
}

/// One msgpack value off the front of `b`, its JSON appended to `out`; the rest of `b`.
fn transcode<'a>(b: &'a [u8], out: &mut String) -> &'a [u8] {
    let be =
        |n: usize| -> u64 { b[1..1 + n].iter().fold(0u64, |acc, &x| (acc << 8) | u64::from(x)) };
    let (m, rest) = (b[0], &b[1..]);
    match m {
        0x00..=0x7f => json(out, &u64::from(m)),
        0xe0..=0xff => json(out, &i64::from(m as i8)),
        0xc0 => out.push_str("null"),
        0xc2 => out.push_str("false"),
        0xc3 => out.push_str("true"),
        0xcc..=0xcf => {
            let n = 1 << (m - 0xcc);
            json(out, &be(n));
            return &b[1 + n..];
        }
        0xd0..=0xd3 => {
            let n = 1 << (m - 0xd0);
            let shift = 64 - 8 * n as u32;
            json(out, &(((be(n) << shift) as i64) >> shift));
            return &b[1 + n..];
        }
        0xca => {
            json(out, &f64::from(f32::from_bits(be(4) as u32)));
            return &b[5..];
        }
        0xcb => {
            json(out, &f64::from_bits(be(8)));
            return &b[9..];
        }
        0xa0..=0xbf | 0xd9..=0xdb => {
            let (len, at) = match m {
                0xa0..=0xbf => (usize::from(m & 0x1f), 1),
                _ => {
                    let n = 1 << (m - 0xd9);
                    (be(n) as usize, 1 + n)
                }
            };
            json(out, std::str::from_utf8(&b[at..at + len]).expect("a msgpack str is UTF-8"));
            return &b[at + len..];
        }
        0x80..=0x9f | 0xdc..=0xdf => {
            let map = matches!(m, 0x80..=0x8f | 0xde | 0xdf);
            let (len, mut rest) = match m {
                0x80..=0x9f => (usize::from(m & 0x0f), rest),
                0xdc | 0xde => (be(2) as usize, &b[3..]),
                _ => (be(4) as usize, &b[5..]),
            };
            out.push(if map { '{' } else { '[' });
            for i in 0..len {
                if i > 0 {
                    out.push(',');
                }
                if map {
                    let mut key = String::new();
                    rest = transcode(rest, &mut key);
                    // JSON keys are strings: a non-string key (an integer) is quoted, as serde_json quotes it.
                    if key.starts_with('"') {
                        out.push_str(&key);
                    } else {
                        out.push('"');
                        out.push_str(&key);
                        out.push('"');
                    }
                    out.push(':');
                }
                rest = transcode(rest, out);
            }
            out.push(if map { '}' } else { ']' });
            return rest;
        }
        other => panic!("msgpack marker {other:#04x} is not one this journal writes"),
    }
    rest
}

/// Segment `index` of `dir` as a format 31..=33 build wrote it: a `version` header, each record a JSON frame,
/// sized to `segment_bytes` and zero-filled past the frames (a journal the binary step moves aside).
pub(super) fn write_json_segment(
    dir: &Path,
    index: u64,
    version: u32,
    records: &[crate::JournalRecord],
    segment_bytes: usize,
) {
    use crate::format::{HEADER, MAGIC};
    let first = records.first().map_or(0, crate::record_sequence);
    let mut bytes = Vec::with_capacity(segment_bytes);
    bytes.extend_from_slice(&MAGIC.to_le_bytes());
    bytes.extend_from_slice(&version.to_le_bytes());
    bytes.extend_from_slice(&first.to_le_bytes());
    assert_eq!(bytes.len(), HEADER);
    for rec in records {
        let payload = serde_json::to_vec(rec).expect("a record serializes");
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&crate::fnv1a32(&payload).to_le_bytes());
        bytes.extend_from_slice(&payload);
    }
    assert!(bytes.len() <= segment_bytes, "the records fit the segment");
    bytes.resize(segment_bytes, 0);
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(crate::segment::segment_path(dir, index), bytes).unwrap();
}

/// The byte offset of every written frame of segment file `seg`, in order (the walk stops at the
/// first zero length prefix, or where fewer than 8 bytes are left).
pub(super) fn frame_starts(seg: &Path) -> Vec<usize> {
    let bytes = std::fs::read(seg).unwrap();
    let mut at = crate::format::HEADER;
    let mut starts = Vec::new();
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        if len == 0 {
            break;
        }
        starts.push(at);
        at += 8 + len;
    }
    starts
}

/// The message the LATENCY GATE actually journals — `crates/vike-core/tests/runtime_latency.rs`'s
/// `fill_with_order_client_id` field-for-field, so a measurement taken here prices the same bytes the gate's
/// journal variants write (~2.5x the [`ingest`] cancel above, which is why the two are separate).
/// Kept in step BY HAND: the two files are in different targets and nothing links them.
///
/// `cfg(linux)` to match its ONLY caller, `journal::writer`'s `measure_append_cost_split` — without
/// it, a Windows `cargo check` reports it dead, which `just windows-check` prints and no gate
/// catches.
#[cfg(target_os = "linux")]
pub(super) fn fill_ingest(ts: i64) -> vike_execution::Ingest {
    vike_execution::Ingest::Event(vike_model::events::Event::Fill(vike_model::events::FillEvent {
        trade_id: "t".into(),
        order_client_id: String::new(),
        venue: "sim".into(),
        symbol: "OTHER".into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: String::new().into(),
        timestamp: ts,
        mark_price: None,
        position_side: vike_model::events::PositionSide::Both,
    }))
}
