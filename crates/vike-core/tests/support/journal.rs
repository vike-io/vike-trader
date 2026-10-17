//! Journal helpers: reading a journal back, copying its segments, re-framing a doctored payload,
//! and the shared source scenario the replay suites fence.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::{Command, OrderIntent};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_model::Clock;

use crate::kit::engines::sim_engine;
use crate::kit::events::sim_bare_fill;
use crate::kit::requests::sim_limit;

/// Every record of the journal at `dir`, in order.
pub(crate) fn records(dir: &Path) -> Vec<JournalRecord> {
    CommandJournal::read_all(dir).unwrap()
}

/// Copy only the `.vjl` segment files (not any temp/lock artifacts) from `src` into `dst`.
pub(crate) fn copy_journal_files(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|s| s.to_str()) == Some("vjl") {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
}

/// FNV-1a32 twin of the journal's frame checksum (the private `journal::fnv1a32`) — needed by the
/// byte-surgery tests to re-frame a doctored payload.
pub(crate) fn fnv1a32(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Run the source scenario, producing a journal at `dir` that spans a cadence Snap + the exit
/// Snap. `snapshot_every = 4`, 7 exec-lane messages: 4 bare fills (the 4th trips the cadence
/// Snap = the replay BASE), then a `Submit` + a bare fill + a `Cancel` in the tail (so the tail
/// is non-empty AND carries both an `Ingest::Event` and an `Ingest::Command`).
pub(crate) fn build_source_journal(dir: &Path) {
    let engine = sim_engine();
    // self-advancing clock: message k is dispatched with now_ms == k (distinct per message, so a
    // wrong QueueClock replay of the Submit's created_ms would break the hash)
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(sim_limit(
        "ord1", 1, 1.0, 100.0,
    )))));
    sender.blocking_send(sim_bare_fill("t5", 1.0, 100.0)).unwrap();
    handle.send_command(Command::Order(OrderIntent::Cancel("ord1".into())));
    handle.shutdown_and_join();
}
