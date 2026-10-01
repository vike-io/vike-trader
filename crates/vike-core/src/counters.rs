//! Opt-in mmap counter mirror (audit co9) — surface the live core's key health counters in a
//! small memory-mapped file an EXTERNAL process can read, so a headless trader on a remote box can
//! be inspected without attaching. The in-process [`crate::CoreSnapshot`] already carries these
//! counters for the GUI; this is the out-of-process twin.
//!
//! **Zero hot-path cost — the pinned constraint.** The per-message fold (the `p99 < 10µs` gate) is
//! NOT touched: nothing here moves the runtime's hot-path atomics into mmap and nothing adds a
//! per-message write. The file is a PERIODIC MIRROR, copied at the SAME coalesced cadence the
//! runtime already publishes a `CoreSnapshot` ([`crate::runtime`]'s `publish`) — a rare, off-the-
//! fold point (~16 ms while busy, or on idle). At that point the current counter values are copied
//! into the mmap slots. So the fold is byte-identical; the only new cost is at the existing rare
//! publish. Disabled by default (`CoreConfig::counters_path == None`) ⇒ nothing is opened and the
//! publish path is byte-identical too.
//!
//! **Layout** (fixed + versioned + cache-line-padded, so an external reader has a stable schema):
//! a header slot followed by one 128-byte slot per named counter. Each slot is its own cache line,
//! so the layout is false-sharing-free and every field has a stable, aligned offset.
//!
//! ```text
//! slot 0  (bytes    0..128): header  magic u32 | version u32 | seq u64 | updated_ms i64 | count u64
//! slot 1  (bytes  128..256): counters[0] value u64  (rest of the slot reserved / zero)
//! ...
//! slot N  (bytes  ...): counters[N-1] value u64
//! ```
//!
//! **Torn reads.** The writer wraps each mirror in a SEQLOCK: `seq` is bumped to an odd value
//! before the payload is written and to the next even value after (a `Release` fence between the
//! phases). A reader reads `seq`, the payload, then `seq` again and accepts the values only when
//! both reads are equal AND even — otherwise it retries. So a clean read never returns a mid-update
//! MIX of counters. As a documented fallback (retries exhausted, e.g. a writer paused mid-update),
//! the reader returns its last read with `clean = false`; because every counter is monotonic, even
//! a torn value is a valid point-in-time reading of THAT counter, off by at most one update.
//!
//! Modelled on `journal.rs` (the crate's other mmap site): the same audited unsafe carve-out, the
//! same magic/version header discipline.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::sync::atomic::{Ordering, fence};

use memmap2::{Mmap, MmapMut};

const MAGIC: u32 = 0x3143_4D56; // "VMC1" LE (Vike Mmap Counters v1)
/// v2 APPENDED `dropped_nonfinite` (slot 7). Append-only, so a v1 reader still decodes slots 0..=6
/// correctly: it takes `n = min(declared, fits, its own N_COUNTERS)` and simply never sees the new
/// slot. It DOES report the version mismatch, which is the intended signal to update it.
const VERSION: u32 = 2;
/// Every named counter (and the header) occupies its own 128-byte cache-line-padded slot, so the
/// layout is fixed, false-sharing-free, and each field has a stable aligned offset for an external
/// reader.
const SLOT: usize = 128;

/// Fixed counter order == the on-disk slot order (counter `i` lives in slot `1 + i`; slot 0 is the
/// header). This is the STABLE WIRE ORDER: only ever APPEND a new counter at the end, and bump
/// [`VERSION`] when you do — never reorder or remove, or an old reader mis-labels the values.
pub const COUNTER_NAMES: [&str; 8] = [
    "conflated_market_drops", // MarketSender latest-wins conflation drops (vike-exec lanes)
    "rejected_commands",      // GUI/command messages rejected on a full ingest queue (runtime)
    "exec_db_queue_depth",    // RETIRED (SQLite exec_db removed) — reserved slot, always 0
    "exec_db_errors",         // RETIRED (SQLite exec_db removed) — reserved slot, always 0
    "stranded_terminal_drops", // liveness/fill dropped onto an already-terminalized order
    "dropped_terminal_on_live", // terminal event dropped on a still-live order (audit C1)
    "dropped_unknown_coid",   // lifecycle event for a coid absent from the registry (audit C2)
    // ⚠ THE ONE COUNTER WITH NO BENIGN READING. The six above all have innocent explanations
    // (a reconnect replay, a shared-account order, an out-of-order frame); a money-lane event
    // carrying a NON-FINITE f64 has none — no venue sends `"NaN"`. Nonzero ⇒ venue fault or
    // compromised feed. Page on it.
    "dropped_nonfinite",
];
const N_COUNTERS: usize = COUNTER_NAMES.len();
/// Total mapped size: the header slot + one slot per counter.
const FILE_BYTES: usize = SLOT * (1 + N_COUNTERS);

// header slot (slot 0) field offsets
const OFF_MAGIC: usize = 0; // u32
const OFF_VERSION: usize = 4; // u32
const OFF_SEQ: usize = 8; // u64 seqlock: odd = write in progress, even = a completed write
const OFF_UPDATED_MS: usize = 16; // i64 wall-clock the writer stamped at its last mirror
const OFF_COUNT: usize = 24; // u64 number of counter slots the writer declared (fwd-compat)

/// The key runtime health counters mirrored into the mmap file. Plain `u64`s — no I/O, no
/// serde/wire change. Summed across the primary + every extra engine by the runtime (see
/// `CoreThread::collect_counters`), so a cross-venue core reports totals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub conflated_market_drops: u64,
    pub rejected_commands: u64,
    pub exec_db_queue_depth: u64,
    pub exec_db_errors: u64,
    pub stranded_terminal_drops: u64,
    pub dropped_terminal_on_live: u64,
    pub dropped_unknown_coid: u64,
    /// Money-lane events refused for carrying a non-finite f64 (`vike_exec`'s `dropped_nonfinite`).
    pub dropped_nonfinite: u64,
}

impl Counters {
    /// Values in the fixed slot order (matches [`COUNTER_NAMES`]) — the one place the field order
    /// binds to the wire order.
    fn to_slots(self) -> [u64; N_COUNTERS] {
        [
            self.conflated_market_drops,
            self.rejected_commands,
            self.exec_db_queue_depth,
            self.exec_db_errors,
            self.stranded_terminal_drops,
            self.dropped_terminal_on_live,
            self.dropped_unknown_coid,
            self.dropped_nonfinite,
        ]
    }

    /// Rebuild from a slot-ordered slice; a shorter slice (older/newer file, unknown trailing
    /// slots) leaves the missing fields at 0.
    fn from_slots(s: &[u64]) -> Self {
        let g = |i: usize| s.get(i).copied().unwrap_or(0);
        Counters {
            conflated_market_drops: g(0),
            rejected_commands: g(1),
            exec_db_queue_depth: g(2),
            exec_db_errors: g(3),
            stranded_terminal_drops: g(4),
            dropped_terminal_on_live: g(5),
            dropped_unknown_coid: g(6),
            dropped_nonfinite: g(7),
        }
    }

    /// `(name, value)` pairs in the fixed slot order — the reader CLI prints straight from this, so
    /// it needs no layout knowledge of its own.
    pub fn named(&self) -> Vec<(&'static str, u64)> {
        COUNTER_NAMES.iter().copied().zip(self.to_slots()).collect()
    }
}

/// Writer half: a pre-sized mmap the runtime mirrors counters into at publish cadence. Opened once
/// (opt-in) at core construction, held by the core thread. Dropping it unmaps the file.
pub struct CountersFile {
    map: MmapMut,
    /// last seqlock value written to the header (even after a completed [`Self::write`]).
    seq: u64,
}

impl CountersFile {
    /// Create (or reopen + re-stamp) the mmap counters file at `path`, pre-sized to [`FILE_BYTES`]
    /// and initialized to a clean, zeroed, even-seq state. The parent dir is created if needed.
    //
    // One of the crate's audited unsafe carve-outs (the crate lint is `unsafe_code = "deny"`; see
    // Cargo.toml). This per-fn `#[allow(unsafe_code)]` covers the single `MmapMut::map_mut` call —
    // the OS mmap entry point, no safe wrapper exists — same as journal.rs.
    #[allow(unsafe_code)]
    pub fn create(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        // truncate(false): reopening an existing file keeps its bytes (we re-stamp the header + write
        // a fresh zeroed payload below anyway); create(true) makes the first run.
        let file =
            OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        if file.metadata()?.len() < FILE_BYTES as u64 {
            file.set_len(FILE_BYTES as u64)?; // pre-allocate: writes are pure in-bounds memory copies
        }
        // SAFETY: `MmapMut::map_mut` is unsafe by signature — the caller promises the file is not
        // resized/truncated by another handle while mapped. This writer owns `path` for the map's
        // lifetime: it is pre-sized once above and never resized again; only this process writes into
        // the mapped pages, and external readers open the file READ-ONLY and never truncate it. So
        // the safety contract holds (the same argument journal.rs makes for its map_mut).
        let map = unsafe { MmapMut::map_mut(&file)? };
        let mut cf = CountersFile { map, seq: 0 };
        // Stamp the fixed header, then write a clean zeroed initial payload so the file starts in a
        // consistent even-seq state (never exposing leftover values from a prior run before the first
        // real mirror).
        cf.map[OFF_MAGIC..OFF_MAGIC + 4].copy_from_slice(&MAGIC.to_le_bytes());
        cf.map[OFF_VERSION..OFF_VERSION + 4].copy_from_slice(&VERSION.to_le_bytes());
        cf.map[OFF_COUNT..OFF_COUNT + 8].copy_from_slice(&(N_COUNTERS as u64).to_le_bytes());
        cf.write(0, &Counters::default());
        Ok(cf)
    }

    /// Mirror `c` into the mmap under a seqlock (see the module docs). PURE in-memory copies — the
    /// file is pre-mapped, so this issues NO syscall and NO flush (a same-host reader mmapping the
    /// same file sees the writes via shared page coherency). Called ONLY at the coalesced publish
    /// cadence (runtime.rs), NEVER the per-message fold.
    pub fn write(&mut self, updated_ms: i64, c: &Counters) {
        // ENTER the write: seq -> odd. A concurrent reader that observes an odd seq retries.
        self.seq = self.seq.wrapping_add(1);
        self.store_seq(self.seq);
        // Order the seq-odd store before the payload stores (prevents the compiler from sinking the
        // payload writes past the closing even store; on x86/ARM the hardware then keeps store order).
        fence(Ordering::Release);
        self.map[OFF_UPDATED_MS..OFF_UPDATED_MS + 8].copy_from_slice(&updated_ms.to_le_bytes());
        for (i, v) in c.to_slots().into_iter().enumerate() {
            let off = SLOT * (1 + i);
            self.map[off..off + 8].copy_from_slice(&v.to_le_bytes());
        }
        fence(Ordering::Release);
        // LEAVE the write: seq -> even (== a completed, readable snapshot).
        self.seq = self.seq.wrapping_add(1);
        self.store_seq(self.seq);
    }

    fn store_seq(&mut self, v: u64) {
        self.map[OFF_SEQ..OFF_SEQ + 8].copy_from_slice(&v.to_le_bytes());
    }
}

/// A read snapshot of the counters file: the header fields plus the decoded [`Counters`].
#[derive(Debug, Clone)]
pub struct CountersReport {
    /// on-disk format version (compare against the reader's expectation).
    pub version: u32,
    /// the seqlock value the payload was read under (even ⇒ a completed write).
    pub seq: u64,
    /// wall-clock ms the writer stamped at its last mirror (0 before the first real publish).
    pub updated_ms: i64,
    /// number of counter slots the writer declared (a newer writer may declare more than this
    /// reader knows; the extra slots are ignored).
    pub counter_count: u64,
    pub counters: Counters,
    /// true ⇒ the read completed under a stable, even seqlock (no torn read). false ⇒ retries were
    /// exhausted and the values MAY be a mid-update mix (each still individually monotonic).
    pub clean: bool,
}

/// Map the counters file at `path` READ-ONLY and decode one seqlock-consistent snapshot. Errors on
/// a missing file, a too-small file, or a bad magic; a version mismatch is surfaced in the report
/// (not an error) so a newer file still prints its known counters.
//
// The crate's other audited unsafe carve-out: the single read-only `Mmap::map` call. See
// [`CountersFile::create`] for the deny-lint rationale.
#[allow(unsafe_code)]
pub fn read(path: &Path) -> io::Result<CountersReport> {
    let file = OpenOptions::new().read(true).open(path)?;
    // SAFETY: read-only map of a file this process never resizes. `Mmap::map` is unsafe by signature
    // (a concurrent truncation by ANOTHER process would be UB). The counters file is grown-once-then-
    // fixed by the writer and never truncated for its lifetime, so mapping it read-only is sound.
    let map = unsafe { Mmap::map(&file)? };
    if map.len() < SLOT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "counters file smaller than one 128-byte slot",
        ));
    }
    let magic = u32::from_le_bytes(map[OFF_MAGIC..OFF_MAGIC + 4].try_into().unwrap());
    if magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bad counters-file magic {magic:#010x} (expected {MAGIC:#010x})"),
        ));
    }
    let version = u32::from_le_bytes(map[OFF_VERSION..OFF_VERSION + 4].try_into().unwrap());
    let counter_count = u64::from_le_bytes(map[OFF_COUNT..OFF_COUNT + 8].try_into().unwrap());
    // How many counter slots we can actually read = min(declared, file-fits, known-to-this-reader).
    let avail = (map.len() / SLOT).saturating_sub(1) as u64;
    let n = counter_count.min(avail).min(N_COUNTERS as u64) as usize;

    // Seqlock read: retry until the pre/post seq are equal AND even; else fall back to the last read
    // (torn tolerance — the module docs' documented fallback).
    let mut slots = [0u64; N_COUNTERS];
    let mut updated_ms = 0i64;
    let mut seq = 0u64;
    let mut clean = false;
    for _ in 0..256 {
        let s1 = u64::from_le_bytes(map[OFF_SEQ..OFF_SEQ + 8].try_into().unwrap());
        fence(Ordering::Acquire);
        updated_ms =
            i64::from_le_bytes(map[OFF_UPDATED_MS..OFF_UPDATED_MS + 8].try_into().unwrap());
        for (i, slot) in slots.iter_mut().enumerate().take(n) {
            let off = SLOT * (1 + i);
            *slot = u64::from_le_bytes(map[off..off + 8].try_into().unwrap());
        }
        fence(Ordering::Acquire);
        let s2 = u64::from_le_bytes(map[OFF_SEQ..OFF_SEQ + 8].try_into().unwrap());
        seq = s2;
        if s1 == s2 && s1 % 2 == 0 {
            clean = true;
            break;
        }
    }
    Ok(CountersReport {
        version,
        seq,
        updated_ms,
        counter_count,
        counters: Counters::from_slots(&slots),
        clean,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::scratch::Scratch;

    /// A counters-file path inside a scratch directory the CALLER owns: the returned [`Scratch`]
    /// deletes that directory — and the `.bin` written into it — when it drops, on the unwind path
    /// too. **Bind the guard for the whole test** (`let (_dir, path) = tmp_path(..)`); binding it
    /// to a bare `_` drops it immediately and removes the directory before the file is written.
    ///
    /// ⚠ The old spelling was `env::temp_dir().join(format!("vmc-test-{tag}-{pid}.bin"))` plus a
    /// `remove_file` PRE-clean against PID reuse, and nothing removed the file afterwards — so
    /// every run of this module leaked one file per test, forever. MEASURED on the the CI box CI box on
    /// 2026-08-25: **2,073 `/tmp/vmc-test-*.bin`** dating back to 2026-08-08, growing by ~264 a
    /// day. The pre-clean could not close the second half either: the CI box runs tests as two users
    /// (`the CI user` for CI, `the operator` for the verification lanes), and a reused PID landing on the
    /// OTHER user's file fails the write with `PermissionDenied`. `Scratch`'s random suffix makes
    /// both impossible by construction, which is why no pre-clean survives here.
    fn tmp_path(tag: &str) -> (Scratch, std::path::PathBuf) {
        let dir = Scratch::created(&format!("counters-{tag}"));
        let path = dir.join(format!("vmc-test-{tag}.bin"));
        (dir, path)
    }

    fn sample() -> Counters {
        Counters {
            conflated_market_drops: 11,
            rejected_commands: 22,
            exec_db_queue_depth: 33,
            exec_db_errors: 44,
            stranded_terminal_drops: 55,
            dropped_terminal_on_live: 66,
            dropped_unknown_coid: 77,
            dropped_nonfinite: 88,
        }
    }

    #[test]
    fn write_then_read_roundtrip() {
        let (_dir, path) = tmp_path("roundtrip");
        // Keep the writer ALIVE while reading — the realistic case (an external reader maps the file
        // the live writer still holds mapped).
        let mut w = CountersFile::create(&path).unwrap();
        w.write(1_700_000_000_123, &sample());

        let r = read(&path).unwrap();
        assert_eq!(r.version, VERSION, "header version round-trips");
        assert_eq!(r.counter_count, N_COUNTERS as u64);
        assert_eq!(r.updated_ms, 1_700_000_000_123);
        assert!(r.clean, "no writer contention ⇒ a clean seqlock read");
        assert_eq!(r.seq % 2, 0, "a completed write leaves seq even");
        assert_eq!(r.counters, sample(), "every counter round-trips by slot");
        drop(w);
    }

    #[test]
    fn second_write_advances_seq_and_updates_values() {
        let (_dir, path) = tmp_path("advance");
        let mut w = CountersFile::create(&path).unwrap();
        // create() already did one write (seq 0->2); two more here.
        w.write(1, &Counters { rejected_commands: 1, ..Default::default() });
        let after_first = read(&path).unwrap();
        w.write(2, &Counters { rejected_commands: 9, ..Default::default() });
        let after_second = read(&path).unwrap();

        assert!(after_second.seq > after_first.seq, "seq is monotonic across writes");
        assert_eq!(after_second.seq % 2, 0);
        assert_eq!(after_second.counters.rejected_commands, 9, "latest values win");
        assert_eq!(after_second.updated_ms, 2);
        drop(w);
    }

    #[test]
    fn read_rejects_a_file_with_bad_magic() {
        let (_dir, path) = tmp_path("badmagic");
        // A zero-filled file the size of the layout has magic 0 != MAGIC.
        std::fs::write(&path, vec![0u8; FILE_BYTES]).unwrap();
        let err = read(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "bad magic is an explicit error");
    }

    #[test]
    fn read_of_missing_file_errors() {
        let (_dir, path) = tmp_path("missing");
        // No pre-clean: the scratch directory is fresh, so nothing has ever been written here.
        assert!(read(&path).is_err(), "a missing counters file is a clean io error, not a panic");
    }

    #[test]
    fn named_pairs_match_slot_order() {
        let named = sample().named();
        assert_eq!(named.len(), N_COUNTERS);
        assert_eq!(named[0], ("conflated_market_drops", 11));
        assert_eq!(named[4], ("stranded_terminal_drops", 55));
        assert_eq!(named[6], ("dropped_unknown_coid", 77));
        assert_eq!(named[7], ("dropped_nonfinite", 88), "the v2 append lands in the LAST slot");
    }

    /// The append-only wire rule, pinned: slots 0..=6 must keep the exact names (and therefore the
    /// exact meanings) a v1 reader assumes, so appending slot 7 cannot mis-label anything.
    #[test]
    fn the_v1_slot_names_are_unchanged_by_the_v2_append() {
        assert_eq!(
            &COUNTER_NAMES[..7],
            &[
                "conflated_market_drops",
                "rejected_commands",
                "exec_db_queue_depth",
                "exec_db_errors",
                "stranded_terminal_drops",
                "dropped_terminal_on_live",
                "dropped_unknown_coid",
            ],
            "COUNTER_NAMES is append-only — reordering or renaming a slot mis-labels every \
             existing reader's values"
        );
        assert_eq!(VERSION, 2, "bump VERSION with every append (see COUNTER_NAMES's doc)");
    }
}
