use super::*;
use crate::record::{JournalRecord, JournalRecordRef};
use crate::testutil::*;
use vike_model::test_support::file::plant;

/// MEASUREMENT, not a gate: how much of the journal append tail is FIRST-TOUCH PAGE FAULTS.
///
/// [`reserve_blocks`] removes the filesystem ALLOCATION from inside the write fault. It does
/// not remove the FAULT. A `MAP_SHARED` mapping starts with NO page-table entries, so the first
/// store into each 4 KiB page still traps into the kernel — whether or not its block is already
/// on disk.
///
/// ⚠ This paragraph ended "Nothing in this crate populates those entries: there is no
/// `MAP_POPULATE`, no `madvise`, no touch pass" until 2026-08-29, and that outlived the change
/// which falsified it. [`crate::segment`]'s `warm_ahead` DOES populate them, by
/// `advise_range(memmap2::Advice::PopulateWrite, ..)`, and carries its own A/B measurement of
/// the effect. So this harness now measures a mapping something else is warming, which is
/// exactly the case its own "how to read it" note below calls the signal that "this whole line
/// of attack is wrong" — read that note with the warming in mind rather than as a surprise.
/// ⚠ The one place the original sentence still holds is the ROLL: `map_segment` declares "THE
/// ROLL PATH IS NOT COVERED", so the appender's NEW mapping after a roll is warmed by nothing
/// and pays first-touch faults as it fills.
///
/// How to read what it prints: `faults` should land near `pages` if the tail really is
/// first-touch work, and `over_20us` should be the same order. If `faults` sits far BELOW
/// `pages`, something already warmed the mapping and this whole line of attack is wrong.
///
/// Counted from `/proc/self/stat` rather than `getrusage` deliberately — the crate lint is
/// `unsafe_code = "deny"` with exactly TWO audited carve-outs, and a measurement is not a good
/// enough reason to open a third.
///
/// Release-only, `--ignored`, filter `measure_first_touch`; run it the way
/// `crates/vike-core/CLAUDE.md`'s A/B recipe runs the latency harness, on the dedicated cores.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "measurement, not a gate — see crates/vike-core/CLAUDE.md's A/B recipe"]
fn measure_first_touch_faults_on_the_append_path() {
    /// Minor faults so far, field 10 of `/proc/self/stat`. Parsed after the LAST `)` because
    /// field 2 is the comm and may itself contain parentheses and spaces.
    fn minor_faults() -> u64 {
        let s = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
        let tail = &s[s.rfind(')').expect("comm closes") + 1..];
        tail.split_whitespace().nth(7).expect("minflt").parse().expect("minflt parses")
    }

    const APPENDS: usize = 100_000;
    let dir = tmp_dir("measure-faults");
    // The latency gate's own shape: 256 MiB so no roll happens across the run, flush 256.
    let config = JournalFileConfig { segment_bytes: 256 * 1024 * 1024, flush_every: 256 };
    let t_open = std::time::Instant::now();
    let mut j = Journal::open(&dir, config).unwrap();
    let open_us = t_open.elapsed().as_micros();

    let start_cursor = j.cursor;
    let f0 = minor_faults();
    let mut lat = Vec::with_capacity(APPENDS);
    for i in 0..APPENDS {
        let t = std::time::Instant::now();
        j.append_command(1_000 + i as i64, &ingest(i as u64)).unwrap();
        lat.push(t.elapsed().as_nanos() as u64);
    }
    let faults = minor_faults() - f0;
    let bytes = j.cursor - start_cursor;
    let pages = bytes.div_ceil(4096);

    lat.sort_unstable();
    let at = |q: f64| lat[((lat.len() as f64 * q) as usize).min(lat.len() - 1)];
    let over = |ns: u64| lat.iter().filter(|&&v| v > ns).count();

    println!(
        "MEASURE-FAULTS appends={APPENDS} bytes={bytes} pages={pages} faults={faults} open_us={open_us} \
             p50={} p99={} p999={} max={} over_20us={} over_100us={}",
        at(0.50),
        at(0.99),
        at(0.999),
        lat[lat.len() - 1],
        over(20_000),
        over(100_000),
    );
    drop(j);
    let _ = std::fs::remove_dir_all(&dir);
}

/// MEASUREMENT, not a gate: WHERE the journal append's time actually goes — serialization
/// (`serde_json::to_vec`) versus framing (the memcpy into the mapped pages plus the periodic
/// `flush_async`).
///
/// # Why this exists
///
/// [`warm_ahead`]'s doc carries the A/B that REFUTED the first-touch page-fault hypothesis:
/// warming removed every fault and moved the append's own p99 from 3 697 ns to ~290 ns, while
/// the CORE-HOP p99.9 the latency gate measures went 37 721 -> 37 300 ns — i.e. nothing.
/// [`Journal::map_segment`]'s doc names three causes for that residue; the third is now
/// eliminated, which leaves serialization and the memcpy. This harness separates those two
/// rather than assuming which one it is.
///
/// # How to read what it prints
///
/// Rows per mode, all in nanoseconds, plus a `clock` control row measuring an empty
/// `Instant::now()` pair so the instrumentation's own cost is visible rather than assumed (it
/// MATTERS at the p50 of a sub-µs append and is noise at the p99.9).
///
///   * `ser`   — building the payload bytes.
///   * `frame` — [`Journal::write_framed`]: the three `copy_from_slice`s into the
///     mapping, the counters, and (on one append in `flush_every`) the `flush_async` syscall.
///   * `frame-flush` / `frame-noflush` — `frame` split on whether THAT append tripped
///     `since_flush >= flush_every`. `flush_every` is 256, so the flush appends are 0.39% of
///     the run: exactly the p99.9 population. If the syscall is the tail, it shows up here as
///     a `frame-flush` p50 far above the `frame-noflush` p99.9.
///   * `total` — the two summed, i.e. what [`Journal::append_command`] costs.
///
/// FOUR MODES are run, ALTERNATING (A B C D A B C D …) so a drifting box cannot masquerade as
/// a difference, and the reported figure per statistic is the MEDIAN over the reps. They are
/// the cross of two axes:
///
///   * message shape — `cancel` is [`ingest`]'s small `OrderIntent::Cancel`; `fill` is
///     [`fill_ingest`], which is `crates/vike-core/tests/runtime_latency.rs`'s `fill_with_order_client_id`
///     field-for-field, i.e. the record the LATENCY GATE's journal variants actually write.
///     Read the `fill` rows for anything about the gate; the `cancel` rows are the floor.
///   * serializer — `to_vec` is today's code: a FRESH `Vec` allocated per append. `to_writer`
///     serializes the same bytes into a buffer the caller owns and reuses (`clear()` +
///     `serde_json::to_writer`). Byte-identical output; the ONLY difference is the per-append
///     allocation.
///
/// …plus one CONTROL, `fill/to_vec+warmall`, which populates every page table at open instead
/// of one `segment::warm_ahead_bytes` window. It re-runs the page-fault hypothesis at the APPEND level
/// — the level the core-hop A/B could not isolate — and a tail that does not move under it is
/// a tail that is not first-touch work.
///
/// A `payload-bytes` row states the record size each table was priced on — a size, not a
/// latency, so all four of its columns carry the same figure.
///
/// Release-only, `--ignored`, filter `measure_append_cost_split`; run it the way
/// `crates/vike-core/CLAUDE.md`'s A/B recipe runs the latency harness, on the dedicated cores.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "measurement, not a gate — see crates/vike-core/CLAUDE.md's A/B recipe"]
fn measure_append_cost_split() {
    const APPENDS: usize = 100_000;
    const REPS: usize = 8;
    const FLUSH_EVERY: u32 = 256;

    /// One rep's labelled statistics: `(what, [p50, p99, p999, max])` per row.
    type Rows = Vec<(&'static str, [u64; 4])>;

    /// One configuration under measurement. Named configurations rather than a full cross of
    /// the axes: the cross would spend reps on combinations nothing asks about.
    #[derive(Clone, Copy)]
    struct Mode {
        /// The gate's `FillEvent` (275 B) rather than [`ingest`]'s cancel (85 B).
        fill_shape: bool,
        /// `serde_json::to_writer` into a reused buffer rather than a per-append `to_vec`.
        reuse_buffer: bool,
        /// Populate the WHOLE mapping's page tables at open, not just one window.
        warm_all: bool,
        /// The segment size, which is also what picks `sync_chunk_bytes` — and therefore how
        /// often the syncer re-aims the warm window. 256 MiB is the `journal` gate variant's;
        /// 64 MiB is `JournalConfig::at`'s, i.e. what a real node runs.
        segment_bytes: u64,
        name: &'static str,
    }

    /// p50 / p99 / p99.9 / max of a sample, in the order this harness prints them.
    fn quants(v: &mut [u64]) -> [u64; 4] {
        v.sort_unstable();
        let at = |q: f64| v[((v.len() as f64 * q) as usize).min(v.len() - 1)];
        [at(0.50), at(0.99), at(0.999), v[v.len() - 1]]
    }

    /// One rep of `APPENDS` appends in one mode. Returns the labelled samples, each already
    /// reduced to `[p50, p99, p999, max]`.
    fn rep(mode: Mode, verbose: bool, tag: &str) -> Rows {
        let Mode { fill_shape, reuse_buffer, warm_all, segment_bytes, .. } = mode;
        let dir = tmp_dir(tag);
        let config = JournalFileConfig { segment_bytes, flush_every: FLUSH_EVERY };
        let mut j = Journal::open(&dir, config).unwrap();
        if warm_all {
            // The CONTROL for the page-fault hypothesis, re-run at the APPEND level. `open`
            // warms one `segment::warm_ahead_bytes` window and the syncer re-aims it after each chunk
            // `msync`, so a run whose WAL outgrows the window between refreshes faults over the
            // gap. This populates the whole mapping instead, which cannot leave a gap. If the
            // tail does not move, faults are refuted here as they already were at the core hop.
            let len = j.stage.len();
            j.stage.advise_range(memmap2::Advice::PopulateWrite, 0, len).unwrap();
        }
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);
        let mut ser = Vec::with_capacity(APPENDS);
        let mut frame_flush = Vec::new();
        let mut frame_noflush = Vec::with_capacity(APPENDS);
        let mut total = Vec::with_capacity(APPENDS);
        let mut clock = Vec::with_capacity(APPENDS);
        // WHERE in the run each cost lands, not just how large it is. `queued` records the
        // append index at which `queue_sync` last advanced the watermark — i.e. the moment the
        // SYNCER thread started `msync`ing a `sync_chunk` of pages this loop is still storing
        // into. If the tail is writeback contention rather than anything the append itself
        // does, the worst appends cluster immediately AFTER one of these indices.
        let mut chunk_at: Vec<usize> = Vec::new();
        let mut last_queued = j.queued_to;
        let mut by_index: Vec<(u64, usize)> = Vec::with_capacity(APPENDS);
        for i in 0..APPENDS {
            // The GATE's own message when `fill_shape`, the small cancel otherwise:
            // `runtime_latency.rs`'s journal variants journal a `FillEvent`, which serializes
            // to ~2.5x the cancel — so it is the shape whose cost the gate actually pays.
            let msg =
                if fill_shape { fill_ingest(1_000_000_000 + i as i64) } else { ingest(i as u64) };
            let now_ms = 1_000 + i as i64;
            let seq = j.next_sequence();
            let is_flush = (i as u32 + 1).is_multiple_of(FLUSH_EVERY);
            let t0 = std::time::Instant::now();
            if reuse_buffer {
                buffer.clear();
                serde_json::to_writer(
                    &mut buffer,
                    &JournalRecordRef::Command { sequence: seq, now_ms, message: &msg },
                )
                .expect("JournalRecordRef serializes");
            } else {
                buffer = serde_json::to_vec(&JournalRecordRef::Command {
                    sequence: seq,
                    now_ms,
                    message: &msg,
                })
                .expect("JournalRecordRef serializes");
            }
            let t1 = std::time::Instant::now();
            // Split the borrow through a length: `write_framed` takes `&mut self` while the
            // payload borrows `buffer`, a LOCAL here. Not a copy the real append path would pay.
            let n = buffer.len();
            j.write_framed(&buffer[..n]).unwrap();
            let t2 = std::time::Instant::now();
            // The instrumentation's own cost, sampled with the same clock, same cadence.
            let c0 = std::time::Instant::now();
            let c1 = std::time::Instant::now();
            clock.push((c1 - c0).as_nanos() as u64);
            ser.push((t1 - t0).as_nanos() as u64);
            total.push((t2 - t0).as_nanos() as u64);
            let f = (t2 - t1).as_nanos() as u64;
            if is_flush {
                frame_flush.push(f);
            } else {
                frame_noflush.push(f);
            }
            // Both AFTER `t2`, so neither is inside a measured interval.
            by_index.push((f, i));
            if j.queued_to != last_queued {
                last_queued = j.queued_to;
                chunk_at.push(i);
            }
        }
        if verbose {
            // The ten worst FRAMES with their positions, against the chunk-sync positions.
            by_index.sort_unstable_by_key(|&(ns, _)| std::cmp::Reverse(ns));
            let worst: Vec<(usize, u64)> =
                by_index.iter().take(10).map(|&(ns, i)| (i, ns)).collect();
            println!("SPLIT-TAIL {tag} chunk_sync_at={chunk_at:?} worst_frames_(i,ns)={worst:?}");
        }
        let bytes = buffer.len() as u64;
        let mut frame: Vec<u64> = frame_noflush.iter().chain(frame_flush.iter()).copied().collect();
        let out: Rows = vec![
            ("clock", quants(&mut clock)),
            ("ser", quants(&mut ser)),
            ("frame", quants(&mut frame)),
            ("frame-noflush", quants(&mut frame_noflush)),
            ("frame-flush", quants(&mut frame_flush)),
            ("total", quants(&mut total)),
            // A SIZE, not a latency — the payload length every row above was priced on,
            // carried in the same shape so one table states both.
            ("payload-bytes", [bytes; 4]),
        ];
        drop(j);
        out
    }

    // ALTERNATED (never mode-major) so box drift cannot pose as a mode difference.
    //
    //   * `cancel/*` is the FLOOR and doubles as a control: 85 B records put ~9.3 MB of WAL in
    //     the segment, which never reaches `sync_chunk_bytes` — so no chunk `msync` runs at all
    //     and its tail is first-touch faults alone.
    //   * `fill/*` is the shape the latency gate writes (275 B, ~28 MB, ONE chunk crossing).
    //   * `fill/to_vec+warmall` is `fill/to_vec` with every page table populated at open.
    const SEGMENT_GATE: u64 = 256 * 1024 * 1024; // the `journal` latency variant's segment
    const SEGMENT_PROD: u64 = 64 * 1024 * 1024; // `JournalConfig::at`'s — what a node runs
    let modes: [Mode; 6] = [
        Mode {
            fill_shape: false,
            reuse_buffer: false,
            warm_all: false,
            segment_bytes: SEGMENT_GATE,
            name: "cancel/to_vec",
        },
        Mode {
            fill_shape: false,
            reuse_buffer: true,
            warm_all: false,
            segment_bytes: SEGMENT_GATE,
            name: "cancel/to_writer",
        },
        Mode {
            fill_shape: true,
            reuse_buffer: false,
            warm_all: false,
            segment_bytes: SEGMENT_GATE,
            name: "fill/to_vec",
        },
        Mode {
            fill_shape: true,
            reuse_buffer: true,
            warm_all: false,
            segment_bytes: SEGMENT_GATE,
            name: "fill/to_writer",
        },
        Mode {
            fill_shape: true,
            reuse_buffer: false,
            warm_all: true,
            segment_bytes: SEGMENT_GATE,
            name: "fill/to_vec+warmall",
        },
        Mode {
            fill_shape: true,
            reuse_buffer: false,
            warm_all: false,
            segment_bytes: SEGMENT_PROD,
            name: "fill/to_vec/seg64",
        },
    ];
    let mut runs: Vec<(&str, Rows)> = Vec::new();
    for r in 0..REPS {
        for m in modes {
            // The positional detail only from the FIRST rep of each mode — it is one line per
            // rep and eight copies of it say nothing the first does not.
            let tag = format!("{}-{r}", m.name.replace(['/', '+'], "-"));
            runs.push((m.name, rep(m, r == 0, &tag)));
        }
    }

    for m in modes {
        let mode = m.name;
        let reps: Vec<&Rows> = runs.iter().filter(|(n, _)| *n == mode).map(|(_, v)| v).collect();
        for (row, (label, _)) in reps[0].iter().enumerate() {
            // MEDIAN of each statistic across the reps — never a best case (single runs lie on
            // this box; `crates/vike-core/CLAUDE.md`'s recipe is emphatic about it).
            let med = |col: usize| {
                let mut xs: Vec<u64> = reps.iter().map(|r| r[row].1[col]).collect();
                xs.sort_unstable();
                xs[xs.len() / 2]
            };
            println!(
                "SPLIT mode={mode:<20} what={label:<14} p50={:<8} p99={:<8} p999={:<8} max={}",
                med(0),
                med(1),
                med(2),
                med(3),
            );
        }
    }
}

#[test]
fn append_reopen_read_back_in_order() {
    let dir = tmp_dir("roundtrip");
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 4 };
    let mut j = Journal::open(&dir, config.clone()).unwrap();
    for i in 0..100 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j); // flush on drop
    let back = Journal::read_all(&dir).unwrap();
    assert_eq!(back.len(), 100);
    for (i, r) in back.iter().enumerate() {
        match r {
            JournalRecord::Command { sequence: seq, .. } => assert_eq!(*seq, i as u64),
            _ => panic!("unexpected"),
        }
    }
    // reopen resumes the sequence
    let j2 = Journal::open(&dir, config).unwrap();
    assert_eq!(j2.next_sequence(), 100);
}

#[test]
fn rolls_to_a_new_segment_when_full() {
    let dir = tmp_dir("roll");
    let config = JournalFileConfig { segment_bytes: 4 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 0..200 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    // Count SEGMENTS, not directory entries: the dir also holds the interlock sentinel
    // (`LOCK`), so a raw `read_dir` count would read 2 with only ONE segment present.
    let segs = segment_files(&dir);
    assert!(segs.len() >= 2, "expected multiple segments, got {}", segs.len());
    assert_eq!(Journal::read_all(&dir).unwrap().len(), 200);
}

// ---------------------------------------------------------------------------------------------
// What a crash leaves past the resume cursor, and that no read loses what is appended after it.
// ---------------------------------------------------------------------------------------------

/// A segment small enough for one frame to reach its end.
const TAIL_SEGMENT: usize = 16 * 1024;

fn tail_config() -> JournalFileConfig {
    JournalFileConfig { segment_bytes: TAIL_SEGMENT as u64, flush_every: 64 }
}

/// A record as long as `chars` of its order client id make it.
fn cancel_of(chars: usize) -> vike_execution::Ingest {
    vike_execution::Ingest::Command(vike_execution::Command::Order(
        vike_execution::OrderIntent::Cancel("k".repeat(chars)),
    ))
}

fn sequences_of(records: &[JournalRecord]) -> Vec<u64> {
    records.iter().map(record_sequence).collect()
}

/// **A kill mid-frame, then a restart, then a roll: every record after the tear is read.** One shape a kill leaves
/// is the check and part of the payload with NO length (the shape the old mapped write left; the write thread's one
/// `pwrite` can leave this one or a length with a torn payload, which the boot reads as a torn tail): the read ends
/// clean there and no reader or boot sees a tear. The next open resumed AT it, so a shorter record left the torn payload
/// behind itself, the next record that did not fit rolled, and the closed segment ended in those bytes: every reader
/// halted there for good, journal-wide, over everything in the later segments.
#[test]
fn records_appended_after_a_tail_torn_by_a_kill_are_all_read() {
    let dir = tmp_dir("kill-torn");
    let mut j = Journal::open(&dir, tail_config()).unwrap();
    for i in 0..3 {
        j.append_command(1_000 + i as i64, &fat_ingest(i)).unwrap();
    }
    drop(j);
    let seg = segment_files(&dir).remove(0);
    let at = crate::end_of_frames(&seg);
    let room = TAIL_SEGMENT - at - 8;
    let record = JournalRecord::Command { sequence: 3, now_ms: 1_003, message: cancel_of(room) };
    let payload = serde_json::to_vec(&record).unwrap();
    plant(&seg, at + 4, &fnv1a32(&payload).to_le_bytes());
    plant(&seg, at + 8, &payload[..room]);
    let (records, halt) = Journal::read_all_located(&dir).unwrap();
    assert_eq!((records.len(), halt), (3, None), "a kill writes no length: the read ends clean");

    let mut j = Journal::open(&dir, tail_config()).unwrap();
    assert_eq!(j.next_sequence(), 3);
    j.append_command(2_000, &ingest(10)).unwrap(); // short: it would end inside the torn payload
    j.append_command(2_001, &cancel_of(TAIL_SEGMENT / 3)).unwrap(); // too long for what is left there
    j.append_command(2_002, &ingest(12)).unwrap();
    drop(j);

    let (records, halt) = Journal::read_all_located(&dir).unwrap();
    assert_eq!(halt, None, "nothing halts a read");
    assert_eq!(sequences_of(&records), (0..6).collect::<Vec<u64>>(), "every record, in order");
    assert_eq!(
        sequences_of(&crate::read_since(&dir, 2).unwrap()),
        [3, 4, 5],
        "the tail read agrees"
    );
    let bytes = std::fs::read(&seg).unwrap();
    assert_eq!(
        &bytes[at + 8..at + 8 + room],
        &payload[..room],
        "the torn bytes stay where they fell"
    );

    // A restart after that resumes the new segment: a clean tail takes no second roll.
    let segments = segment_files(&dir).len();
    let j = Journal::open(&dir, tail_config()).unwrap();
    assert_eq!((j.next_sequence(), segment_files(&dir).len()), (6, segments));
}

/// **A tear whose LENGTH landed (a power cut) closes its segment the same way, and only the length goes**: a read
/// reaches the records after it, and the rest of the torn bytes stay for whoever looks.
#[test]
fn a_tear_whose_length_landed_loses_only_its_length() {
    let dir = tmp_dir("power-torn");
    let mut j = Journal::open(&dir, tail_config()).unwrap();
    for i in 0..3 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let seg = segment_files(&dir).remove(0);
    let at = crate::end_of_frames(&seg);
    let record = JournalRecord::Command { sequence: 3, now_ms: 1_003, message: ingest(3) };
    let payload = serde_json::to_vec(&record).unwrap();
    let landed = payload.len() / 2;
    plant(&seg, at, &(payload.len() as u32).to_le_bytes());
    plant(&seg, at + 4, &fnv1a32(&payload).to_le_bytes());
    plant(&seg, at + 8, &payload[..landed]);
    let (records, halt) = Journal::read_all_located(&dir).unwrap();
    let halt = halt.expect("the tear halts the read");
    assert_eq!((records.len(), halt.cause), (3, crate::JournalCorruptCause::CheckMismatch));

    let mut j = Journal::open(&dir, tail_config()).unwrap();
    j.append_command(2_000, &ingest(10)).unwrap();
    drop(j);

    let (records, halt) = Journal::read_all_located(&dir).unwrap();
    assert_eq!(halt, None);
    assert_eq!(sequences_of(&records), [0, 1, 2, 3]);
    let bytes = std::fs::read(&seg).unwrap();
    assert_eq!(&bytes[at..at + 4], &[0u8; 4], "the length no reader may follow");
    assert_eq!(&bytes[at + 8..at + 8 + landed], &payload[..landed], "and nothing else");
}

/// **Frames past a hole never come back behind a new frame.** A power cut can lose a page and keep the ones after it,
/// so valid frames lie past the stop. Resumed in place, a new record of the lost frame's length ended exactly on the
/// next old frame: every read then took the dead run's records for this one's.
#[test]
fn frames_past_a_hole_are_not_read_behind_a_new_frame() {
    let dir = tmp_dir("hole");
    let mut j = Journal::open(&dir, tail_config()).unwrap();
    for i in 0..5 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let seg = segment_files(&dir).remove(0);
    let starts = frame_starts(&seg);
    plant(&seg, starts[2] + 8, &vec![0u8; starts[3] - starts[2] - 8]);

    let mut j = Journal::open(&dir, tail_config()).unwrap();
    assert_eq!(j.next_sequence(), 2);
    j.append_command(1_002, &ingest(7)).unwrap(); // the lost frame's own length
    drop(j);

    let records = Journal::read_all(&dir).unwrap();
    assert_eq!(sequences_of(&records), [0, 1, 2], "the two stale frames are not read");
}

/// A clean tail resumes where it stopped: an ordinary restart takes no roll, so a box that restarts often does not
/// spend a segment each time.
#[test]
fn a_clean_tail_resumes_in_its_own_segment() {
    let dir = tmp_dir("clean-tail");
    let mut j = Journal::open(&dir, tail_config()).unwrap();
    j.append_command(1_000, &ingest(0)).unwrap();
    drop(j);
    let mut j = Journal::open(&dir, tail_config()).unwrap();
    j.append_command(1_001, &ingest(1)).unwrap();
    drop(j);
    assert_eq!(segment_files(&dir).len(), 1);
    assert_eq!(sequences_of(&Journal::read_all(&dir).unwrap()), [0, 1]);
}

/// **A frame another build wrote is never overwritten by an open.** Its check passes, so it is a record that build can
/// read. The boot moves such a directory aside before the open; when that move failed, the open leaves the frame whole
/// and goes on in a new segment, and the next boot's read still names the frame.
#[test]
fn an_open_does_not_overwrite_a_frame_another_build_wrote() {
    let dir = tmp_dir("foreign-frame");
    let mut j = Journal::open(&dir, tail_config()).unwrap();
    j.append_command(1_000, &ingest(0)).unwrap();
    drop(j);
    let seg = segment_files(&dir).remove(0);
    let at = crate::end_of_frames(&seg);
    let payload = br#"{"NotAVariantOfThisBuild":{"sequence":1}}"#;
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&fnv1a32(payload).to_le_bytes());
    frame.extend_from_slice(payload);
    plant(&seg, at, &frame);

    let mut j = Journal::open(&dir, tail_config()).unwrap();
    j.append_command(2_000, &ingest(1)).unwrap();
    drop(j);

    assert_eq!(&std::fs::read(&seg).unwrap()[at..at + frame.len()], &frame[..]);
    let (_, halt) = Journal::read_all_located(&dir).unwrap();
    let halt = halt.expect("this build still cannot read it");
    assert_eq!((halt.cause, halt.in_tail), (crate::JournalCorruptCause::ParseFailed, false));
}

/// **A new segment a crash left unstamped carries the seq on.** `roll` names the file before it stamps the header, so a
/// kill between the two (or a power cut that lost the header page) leaves a highest segment of zeros. The open used to
/// stamp it `first_sequence` 0: sequences ran again from 0, `read_since` skipped them as already read, and the prune, which trusts
/// them to rise, deleted the NEWEST segment as older than the latest `Snapshot`.
#[test]
fn a_new_segment_a_crash_left_unstamped_carries_the_sequence_on() {
    let dir = tmp_dir("unstamped");
    let mut j = Journal::open(&dir, tail_config()).unwrap();
    for i in 0..3 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    j.append_snap(
        1_003,
        &snap_engines(),
        "sess",
        7,
        0,
        crate::record::JournalSnapshotParts::default(),
        1,
    )
    .unwrap();
    drop(j);
    let next = segment_path(&dir, 1);
    std::fs::File::create(&next).unwrap().set_len(TAIL_SEGMENT as u64).unwrap();

    let mut j = Journal::open(&dir, tail_config()).unwrap();
    assert_eq!(j.next_sequence(), 4, "on from the segment before it");
    j.append_command(2_000, &ingest(4)).unwrap();
    drop(j);

    assert_eq!(sequences_of(&Journal::read_all(&dir).unwrap()), [0, 1, 2, 3, 4]);
    Journal::prune_before_latest_snap(&dir).unwrap();
    assert_eq!(sequences_of(&crate::read_since(&dir, 3).unwrap()), [4], "the prune kept it");
}

/// A by-hand comparison, run on two builds (it proved decision 0279 kept the format): the BYTES a
/// deterministic run leaves on disk, one line per segment file (its length and a 64-bit FNV-1a of the
/// whole file), so two builds' outputs compare line for line. 64 KiB segments so it rolls several
/// times; fat records cross pages; a v21 record raises the header mid-segment; a reopen resumes the
/// last segment and appends again.
#[test]
#[ignore = "investigation: run on two builds and compare the BYTEDUMP lines"]
fn dump_segment_digests() {
    fn fnv64(b: &[u8]) -> u64 {
        b.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &x| {
            (h ^ u64::from(x)).wrapping_mul(0x100_0000_01b3)
        })
    }
    let dir = tmp_dir("bytedump");
    let config = JournalFileConfig { segment_bytes: 64 * 1024, flush_every: 64 };
    let mut j = Journal::open(&dir, config.clone()).unwrap();
    for i in 0..400u64 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
        if i % 50 == 7 {
            j.append_command(5_000 + i as i64, &fat_ingest(i)).unwrap();
        }
        if i == 250 {
            let state =
                vike_execution::Ingest::Command(vike_execution::Command::SetAccountTradingState {
                    route_key: "binance#alt".into(),
                    state: vike_model::TradingState::Reducing,
                });
            j.append_command(9_000, &state).unwrap();
        }
    }
    drop(j);
    let mut j = Journal::open(&dir, config).unwrap();
    for i in 400..430u64 {
        j.append_command(1_000 + i as i64, &ingest(i)).unwrap();
    }
    drop(j);
    let mut segs = segment_files(&dir);
    segs.sort();
    for seg in &segs {
        let bytes = std::fs::read(seg).unwrap();
        let name = seg.file_name().unwrap().to_string_lossy().into_owned();
        println!("BYTEDUMP {name} len={} fnv64={:016x}", bytes.len(), fnv64(&bytes));
    }
    println!("BYTEDUMP records={}", Journal::read_all(&dir).unwrap().len());
}
