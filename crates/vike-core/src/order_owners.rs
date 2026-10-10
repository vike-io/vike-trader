//! ORDER OWNERSHIP across a process restart: which mount owns each client order id, and each
//! mount's attribution ledger, kept in ONE JSON Lines file in the daemon's strategy-state directory
//! (`docs/decisions/0113-order-ownership-survives-a-restart-in-a-state-dir-file.md` is the verdict).
//!
//! # Why it exists
//!
//! `CoreThread::coid_mount` (coid -> mount slot) and `CoreThread::mount_attr` (the per-mount ledgers)
//! live in memory. Quotes stay resting across a restart by default
//! (`CoreConfig::cancel_orders_on_shutdown` is off), the journal is off by default and its restore
//! path has no production caller, so before this file a resting order's fill after a restart
//! belonged to NO mount, and a sell closing a pre-restart long booked its mount SHORT from zero.
//!
//! # The contract
//!
//! - **One file, one writer.** [`ORDER_OWNERS_FILE`] under the strategy-state directory, beside
//!   `runtime_mounts.json` ([`crate::mount_topology`]). Only the `vt-order-owners` thread this handle
//!   spawns appends to it; nothing locks it, exactly like the topology sidecar (two cores pointed at
//!   one state directory already clobber each other's sidecars).
//! - **Four record kinds, one JSON object per line**, folded in file order: `own` (a mount minted
//!   this coid), `forget` (its order has been terminal past the prune linger), `ledger` (a mount's
//!   ledger after a fill) and `unmount` (an explicit runtime unmount: forget the mount's coids and
//!   its ledger). An `f64` is written by `serde_json`, which prints the shortest decimal that parses
//!   back to the same bits and, with the workspace's `float_roundtrip` pin, parses it back exactly
//!   (`-0.0` included).
//! - **The fold thread only SENDS.** `OrderOwnerLog::record` is called once per ORDER (and once per
//!   fill for a ledger), never per market message: it is one send on an UNBOUNDED channel, so it can
//!   never block the fold. A bounded channel would have to drop a record under a disk stall, and a
//!   dropped `own` is exactly the unattributed fill this file exists to prevent; the cost of
//!   unbounded is memory during a stall, about a hundred bytes per order.
//! - **The writer POLLS.** It sleeps `POLL`, drains whatever arrived, and writes the batch with ONE
//!   `write_all` and one flush. A process crash loses at most the last poll's records (about
//!   20 ms of orders), which the owner accepted; the flush hands a batch to the OS without an
//!   `fsync`, so a power loss can lose what the OS had not yet written. Dropping the handle drains,
//!   flushes and joins.
//! - **Load at boot, on the caller's thread** (`assemble_core`): read the file, skip a torn or
//!   garbage line (counted, one `warn!`), fold the records, drop ownership older than [`TTL_MS`],
//!   keep at most [`OWNER_CAP`] owners (the oldest go first). Ledgers have NO time-to-live: a mount
//!   that holds a position and trades nothing for a month must not lose the position, and the rows
//!   are bounded by the number of mount ids anyway.
//! - **Compaction is the one rewrite, and it is of our own derived state.** The workspace rule is
//!   that nothing deletes, moves, truncates or wholesale-rewrites a database or a file; this file is
//!   not the user's data but a derived index this module alone writes, and an append-only log of
//!   every order would grow without bound. When the file carries many more lines than live entries
//!   (or a skipped line), the folded state is written to `order_owners.jsonl.tmp`, synced, and
//!   renamed over the file, at boot and in the writer. If the rename fails the OLD file is left as
//!   it was and the temporary one removed: compaction may fail, it may never cost the file.
//!   (`crate::strategy_state::write_json_atomic` is NOT reused for this on purpose: its fallback
//!   removes the destination before a second rename, which can leave no file at all.)
//! - **A file that exists and cannot be read is an ERROR, not "no owners".** It is logged with
//!   `tracing::error!`, the core runs WITHOUT persistence (an empty load, `OrderOwnerLog::record`
//!   a counted no-op) and the file is never touched. A non-empty file in which NOT ONE line parses
//!   is treated the same way: whatever it is, it is not ours to compact away. A missing file is the
//!   ordinary first boot and is created.
//! - **No environment variable and no setting.** The path is the caller's: the daemon derives it
//!   from the state directory it already has ([`OrderOwnerLog::in_state_dir`]).
//!
//! Time is an INPUT here (`crates/vike-ops/tests/architecture/clock_pin.rs`): a record's `ts` comes
//! from the core's `CoreConfig::clock`, the boot's time-to-live reference is the caller's, and the
//! writer's own compaction measures age against the newest `ts` it has seen. Nothing here reads a
//! clock.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crate::runtime::MountAttribution;

/// The ownership file's name under the strategy-state directory. Open to renaming (the owner has
/// not confirmed the name); it is spelled ONCE, here. A mount id is alphanumerics and `_`, so no
/// mount's `<mount_id>.json` sidecar can collide with it.
pub const ORDER_OWNERS_FILE: &str = "order_owners.jsonl";

/// How long an ownership entry survives without being forgotten: 30 days, measured from its `own`
/// record's `ts`. An order resting longer than this loses its owner at the next boot.
pub const TTL_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// The most ownership entries a load keeps; past it the OLDEST `ts` are evicted.
pub const OWNER_CAP: usize = 20_000;

/// How long the writer sleeps between batches.
const POLL: Duration = Duration::from_millis(20);

/// A file is compacted when it holds more lines than `max(COMPACT_MIN_LINES, COMPACT_FACTOR × live
/// entries)`. The floor keeps a small book from being rewritten every few orders.
const COMPACT_MIN_LINES: usize = 10_000;
/// See [`COMPACT_MIN_LINES`].
const COMPACT_FACTOR: usize = 4;

/// One line of the file. `kind` names the variant.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OwnerRecord {
    /// `mount_id` minted `coid` at `ts` (the core clock's ms).
    Own { coid: String, mount_id: String, ts: i64 },
    /// `coid`'s order has been terminal past the prune linger: it owns nothing any more.
    Forget { coid: String },
    /// `mount_id`'s whole attribution ledger after a fill, at `ts`.
    Ledger { mount_id: String, size: f64, avg_px: f64, realized_pnl: f64, fees_paid: f64, ts: i64 },
    /// An explicit runtime unmount: forget every coid of `mount_id` and its ledger.
    Unmount { mount_id: String },
}

/// What a boot load hands `assemble_core`: ownership and ledgers keyed by MOUNT ID (a slot index is
/// not stable across restarts), in a deterministic order (owners by `ts` then coid, ledgers by id).
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct LoadedOwners {
    /// `(coid, mount_id)`.
    pub(crate) coids: Vec<(String, String)>,
    /// `(mount_id, ledger)`.
    pub(crate) ledgers: Vec<(String, MountAttribution)>,
}

/// The folded state of a record stream: what is live after every record so far.
#[derive(Debug, Default)]
struct Folded {
    /// coid -> (mount_id, ts).
    owners: HashMap<String, (String, i64)>,
    /// mount_id -> (ledger, ts).
    ledgers: HashMap<String, (MountAttribution, i64)>,
    /// The newest `ts` seen: the writer's age reference.
    newest_ts: i64,
}

impl Folded {
    fn apply(&mut self, rec: OwnerRecord) {
        match rec {
            OwnerRecord::Own { coid, mount_id, ts } => {
                self.newest_ts = self.newest_ts.max(ts);
                self.owners.insert(coid, (mount_id, ts));
            }
            OwnerRecord::Forget { coid } => {
                self.owners.remove(&coid);
            }
            OwnerRecord::Ledger { mount_id, size, avg_px, realized_pnl, fees_paid, ts } => {
                self.newest_ts = self.newest_ts.max(ts);
                let attr = MountAttribution { size, avg_px, realized_pnl, fees_paid };
                self.ledgers.insert(mount_id, (attr, ts));
            }
            OwnerRecord::Unmount { mount_id } => {
                self.owners.retain(|_, (m, _)| *m != mount_id);
                self.ledgers.remove(&mount_id);
            }
        }
    }

    /// Drop ownership older than [`TTL_MS`] before `now_ms`, then evict the oldest past
    /// [`OWNER_CAP`]. Ledgers are kept (see the module doc).
    fn expire(&mut self, now_ms: i64) {
        self.owners.retain(|_, (_, ts)| now_ms.saturating_sub(*ts) <= TTL_MS);
        if self.owners.len() > OWNER_CAP {
            let mut by_age: Vec<(i64, String)> =
                self.owners.iter().map(|(c, (_, ts))| (*ts, c.clone())).collect();
            by_age.sort_unstable();
            for (_, coid) in by_age.into_iter().take(self.owners.len() - OWNER_CAP) {
                self.owners.remove(&coid);
            }
        }
    }

    fn live(&self) -> usize {
        self.owners.len() + self.ledgers.len()
    }

    /// The live state as records, in [`LoadedOwners`]' order — what a compaction writes.
    fn records(&self) -> Vec<OwnerRecord> {
        let mut owners: Vec<(&String, &(String, i64))> = self.owners.iter().collect();
        owners.sort_unstable_by_key(|(c, (_, ts))| (*ts, *c));
        let mut ledgers: Vec<(&String, &(MountAttribution, i64))> = self.ledgers.iter().collect();
        ledgers.sort_unstable_by_key(|(m, _)| *m);
        let own = owners.into_iter().map(|(c, (m, ts))| OwnerRecord::Own {
            coid: c.clone(),
            mount_id: m.clone(),
            ts: *ts,
        });
        let led = ledgers.into_iter().map(|(m, (a, ts))| OwnerRecord::Ledger {
            mount_id: m.clone(),
            size: a.size,
            avg_px: a.avg_px,
            realized_pnl: a.realized_pnl,
            fees_paid: a.fees_paid,
            ts: *ts,
        });
        own.chain(led).collect()
    }

    fn loaded(&self) -> LoadedOwners {
        let mut coids = Vec::with_capacity(self.owners.len());
        let mut ledgers = Vec::with_capacity(self.ledgers.len());
        for rec in self.records() {
            match rec {
                OwnerRecord::Own { coid, mount_id, .. } => coids.push((coid, mount_id)),
                OwnerRecord::Ledger { mount_id, size, avg_px, realized_pnl, fees_paid, .. } => {
                    ledgers.push((
                        mount_id,
                        MountAttribution { size, avg_px, realized_pnl, fees_paid },
                    ))
                }
                OwnerRecord::Forget { .. } | OwnerRecord::Unmount { .. } => {}
            }
        }
        LoadedOwners { coids, ledgers }
    }
}

/// Whether a file of `lines` lines holding `live` entries is worth rewriting.
fn needs_compaction(lines: usize, live: usize) -> bool {
    lines > COMPACT_MIN_LINES.max(COMPACT_FACTOR.saturating_mul(live))
}

/// Append one record as one line.
fn encode(buf: &mut Vec<u8>, rec: &OwnerRecord) {
    // A record is strings, integers and finite-or-not floats; `to_writer` into a `Vec` cannot fail
    // on any of them (a non-finite float is written as `null`, and that one line then fails to
    // parse at the next load and is skipped and counted).
    if serde_json::to_writer(&mut *buf, rec).is_ok() {
        buf.push(b'\n');
    }
}

/// Write `records` to `<path>.tmp`, sync it, and rename it over `path`. On ANY failure the
/// temporary file is removed and `path` is left exactly as it was.
fn compact_to(path: &Path, records: &[OwnerRecord]) -> std::io::Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let mut bytes = Vec::with_capacity(records.len() * 96);
    for r in records {
        encode(&mut bytes, r);
    }
    let written = write_synced(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        // Best-effort cleanup of the temp file; the write error is what `written` returns.
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Create `path` holding exactly `bytes`, synced to disk before it is closed.
fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Open `path` for appending, creating it, and make sure the next line starts on a fresh line (a
/// torn last line left by a crash must not swallow the first record written after it).
fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    if f.metadata()?.len() > 0 {
        let mut last = [0u8; 1];
        let mut r = std::fs::File::open(path)?;
        r.seek(SeekFrom::End(-1))?;
        r.read_exact(&mut last)?;
        if last[0] != b'\n' {
            f.write_all(b"\n")?;
        }
    }
    Ok(f)
}

/// The handle a composition root puts on `CoreConfig::order_owners`, and the core keeps for its
/// lifetime. Constructing one touches nothing; `assemble_core` starts it (load, then the writer).
/// Dropping it drains every record sent so far into the file and joins the writer.
pub struct OrderOwnerLog {
    path: PathBuf,
    /// `None` until started, and forever when persistence is off (an unreadable file, a failed
    /// open or spawn): every record is then a counted no-op.
    tx: Option<mpsc::Sender<OwnerRecord>>,
    writer: Option<std::thread::JoinHandle<()>>,
    /// Records that did not reach the writer.
    unpersisted: u64,
}

impl std::fmt::Debug for OrderOwnerLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrderOwnerLog")
            .field("path", &self.path)
            .field("persisting", &self.tx.is_some())
            .field("unpersisted", &self.unpersisted)
            .finish()
    }
}

impl OrderOwnerLog {
    /// A log at `path` (normally [`Self::in_state_dir`]). Reads and creates nothing yet.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        OrderOwnerLog { path: path.into(), tx: None, writer: None, unpersisted: 0 }
    }

    /// [`ORDER_OWNERS_FILE`] under `state_dir` — the daemon's strategy-state directory, the one
    /// `CoreConfig::state_dir` names.
    pub fn in_state_dir(state_dir: &Path) -> Self {
        Self::open(state_dir.join(ORDER_OWNERS_FILE))
    }

    /// The file this log reads and appends.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// BOOT: load the file (time-to-live measured back from `now_ms`), compact it when worthwhile,
    /// and start the writer. Returns what the file says; an empty answer when the file is missing,
    /// or cannot be read (then persistence stays OFF and the error is logged). Call once.
    pub(crate) fn start(&mut self, now_ms: i64) -> LoadedOwners {
        if self.tx.is_some() {
            return LoadedOwners::default();
        }
        let path = self.path.clone();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::error!(
                target: "vike_core::order_owners",
                path = %path.display(),
                error = %e,
                "order-ownership file's directory cannot be created — running WITHOUT ownership \
                 persistence (a resting order's fill after the next restart reaches no mount)"
            );
            return LoadedOwners::default();
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                tracing::error!(
                    target: "vike_core::order_owners",
                    path = %path.display(),
                    error = %e,
                    "order-ownership file exists and cannot be read — running WITHOUT ownership \
                     persistence and leaving the file untouched (a resting order's fill after the \
                     next restart reaches no mount)"
                );
                return LoadedOwners::default();
            }
        };
        let (mut folded, lines, skipped) = fold_bytes(&bytes);
        if lines > 0 && lines == skipped {
            tracing::error!(
                target: "vike_core::order_owners",
                path = %path.display(),
                lines,
                "order-ownership file holds no line this build can read — running WITHOUT \
                 ownership persistence and leaving the file untouched"
            );
            return LoadedOwners::default();
        }
        if skipped > 0 {
            tracing::warn!(
                target: "vike_core::order_owners",
                path = %path.display(),
                skipped,
                lines,
                "order-ownership file had torn or unreadable lines — skipped; the file is compacted"
            );
        }
        folded.expire(now_ms);
        let loaded = folded.loaded();
        let mut file_lines = lines;
        if (skipped > 0 || needs_compaction(lines, folded.live()))
            && warn_compaction(&path, compact_to(&path, &folded.records()))
        {
            file_lines = folded.live();
        }
        let file = match open_append(&path) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(
                    target: "vike_core::order_owners",
                    path = %path.display(),
                    error = %e,
                    "order-ownership file cannot be opened for appending — running WITHOUT \
                     ownership persistence"
                );
                return loaded;
            }
        };
        let (tx, rx) = mpsc::channel::<OwnerRecord>();
        let writer = Writer { path: path.clone(), file: Some(file), folded, file_lines };
        match std::thread::Builder::new()
            .name("vt-order-owners".into())
            .spawn(move || writer.run(rx))
        {
            Ok(join) => {
                self.tx = Some(tx);
                self.writer = Some(join);
            }
            Err(e) => {
                tracing::error!(
                    target: "vike_core::order_owners",
                    error = %e,
                    "order-ownership writer thread cannot be spawned — running WITHOUT ownership \
                     persistence"
                );
            }
        }
        loaded
    }

    /// Send one record to the writer — the fold thread's whole cost (one channel send). A no-op,
    /// counted, when persistence is off.
    pub(crate) fn record(&mut self, rec: OwnerRecord) {
        let Some(tx) = self.tx.as_ref() else {
            self.unpersisted += 1;
            return;
        };
        if tx.send(rec).is_err() {
            // The writer is gone (it only exits on a disconnect, so this is a panic in it).
            if self.unpersisted == 0 {
                tracing::error!(
                    target: "vike_core::order_owners",
                    path = %self.path.display(),
                    "order-ownership writer stopped — later records are not persisted"
                );
            }
            self.unpersisted += 1;
        }
    }
}

impl Drop for OrderOwnerLog {
    fn drop(&mut self) {
        // Disconnect first: the writer drains what is queued, writes it, and exits.
        self.tx = None;
        if let Some(join) = self.writer.take() {
            // A `join` Err is a writer panic; `append` already announced it (writer stopped).
            let _ = join.join();
        }
    }
}

/// Fold a file's bytes: `(state, non-blank lines, skipped lines)`.
fn fold_bytes(bytes: &[u8]) -> (Folded, usize, usize) {
    let mut folded = Folded::default();
    let (mut lines, mut skipped) = (0usize, 0usize);
    for line in bytes.split(|b| *b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        lines += 1;
        match serde_json::from_slice::<OwnerRecord>(line) {
            Ok(rec) => folded.apply(rec),
            Err(_) => skipped += 1,
        }
    }
    (folded, lines, skipped)
}

/// Log a failed compaction; `true` when it succeeded.
fn warn_compaction(path: &Path, outcome: std::io::Result<()>) -> bool {
    match outcome {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                target: "vike_core::order_owners",
                path = %path.display(),
                error = %e,
                "order-ownership file could not be compacted — kept as it was, still appended to"
            );
            false
        }
    }
}

/// The `vt-order-owners` thread's state.
struct Writer {
    path: PathBuf,
    /// `None` only transiently, while a compaction renames over the file.
    file: Option<std::fs::File>,
    /// The live state, kept so a compaction needs no read of the file.
    folded: Folded,
    /// Lines in the file now (or the count a compaction is next measured against).
    file_lines: usize,
}

impl Writer {
    fn run(mut self, rx: mpsc::Receiver<OwnerRecord>) {
        let mut buf: Vec<u8> = Vec::new();
        let mut failing = false;
        loop {
            std::thread::sleep(POLL);
            let mut closed = false;
            let mut batch = 0usize;
            loop {
                match rx.try_recv() {
                    Ok(rec) => {
                        encode(&mut buf, &rec);
                        self.folded.apply(rec);
                        batch += 1;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        closed = true;
                        break;
                    }
                }
            }
            if !buf.is_empty() {
                let wrote = match self.file.as_mut() {
                    Some(f) => f.write_all(&buf).and_then(|()| f.flush()),
                    None => Err(std::io::Error::other("file not open")),
                };
                match wrote {
                    Ok(()) => {
                        if failing {
                            tracing::warn!(
                                target: "vike_core::order_owners",
                                path = %self.path.display(),
                                "order-ownership file writable again"
                            );
                        }
                        failing = false;
                        self.file_lines += batch;
                    }
                    Err(e) => {
                        if !failing {
                            tracing::warn!(
                                target: "vike_core::order_owners",
                                path = %self.path.display(),
                                error = %e,
                                "order-ownership batch not written — these orders lose their \
                                 owner at the next restart"
                            );
                        }
                        failing = true;
                    }
                }
                buf.clear();
            }
            if needs_compaction(self.file_lines, self.folded.live()) {
                self.compact();
            }
            if closed {
                break;
            }
        }
    }

    /// Rewrite the file to the folded state (time-to-live measured from the newest `ts` seen),
    /// closing the append handle first and reopening it after, whichever way the rename went.
    fn compact(&mut self) {
        self.folded.expire(self.folded.newest_ts);
        self.file = None;
        if warn_compaction(&self.path, compact_to(&self.path, &self.folded.records())) {
            self.file_lines = self.folded.live();
        } else {
            // Back off: the next attempt waits for another full threshold of lines.
            self.file_lines = 0;
        }
        match open_append(&self.path) {
            Ok(f) => self.file = Some(f),
            Err(e) => tracing::warn!(
                target: "vike_core::order_owners",
                path = %self.path.display(),
                error = %e,
                "order-ownership file cannot be reopened after compaction — later records are \
                 not persisted"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    fn own(coid: &str, mount_id: &str, ts: i64) -> OwnerRecord {
        OwnerRecord::Own { coid: coid.into(), mount_id: mount_id.into(), ts }
    }

    fn ledger(mount_id: &str, size: f64, avg_px: f64, realized: f64, fees: f64) -> OwnerRecord {
        OwnerRecord::Ledger {
            mount_id: mount_id.into(),
            size,
            avg_px,
            realized_pnl: realized,
            fees_paid: fees,
            ts: 1_000,
        }
    }

    fn lines_of(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path).unwrap().lines().map(str::to_string).collect()
    }

    /// Write `records` through a started log, then drop it (drain, flush, join).
    fn write_through(path: &Path, records: Vec<OwnerRecord>) {
        let mut log = OrderOwnerLog::open(path);
        log.start(1_000);
        assert!(log.tx.is_some(), "a fresh file persists");
        for r in records {
            log.record(r);
        }
    }

    fn reload(path: &Path, now_ms: i64) -> LoadedOwners {
        OrderOwnerLog::open(path).start(now_ms)
    }

    /// Awkward floats come back BIT-identical: `-0.0` (whose sign a decimal printer can lose),
    /// `1e-300` (a subnormal-adjacent exponent) and `0.1 + 0.2` (the shortest-repr classic).
    #[test]
    fn ledgers_round_trip_bit_exact() {
        let dir = Scratch::reserved("vco-owners-f64");
        let path = dir.join(ORDER_OWNERS_FILE);
        write_through(
            &path,
            vec![own("c1", "m_a", 1_000), ledger("m_a", -0.0, 1e-300, 0.1 + 0.2, -1.5e10)],
        );
        let loaded = reload(&path, 1_000);
        assert_eq!(loaded.coids, vec![("c1".to_string(), "m_a".to_string())]);
        let (id, a) = &loaded.ledgers[0];
        assert_eq!(id, "m_a");
        assert_eq!(a.size.to_bits(), (-0.0_f64).to_bits(), "the sign of zero survives");
        assert_eq!(a.avg_px.to_bits(), 1e-300_f64.to_bits());
        assert_eq!(a.realized_pnl.to_bits(), (0.1_f64 + 0.2).to_bits());
        assert_eq!(a.fees_paid.to_bits(), (-1.5e10_f64).to_bits());
    }

    /// The fold: a later `own` re-points, `forget` removes, `unmount` removes the mount's coids
    /// AND its ledger, and the last ledger of a mount wins.
    #[test]
    fn records_fold_in_file_order() {
        let dir = Scratch::reserved("vco-owners-fold");
        let path = dir.join(ORDER_OWNERS_FILE);
        write_through(
            &path,
            vec![
                own("c1", "m_a", 1),
                own("c2", "m_a", 2),
                own("c3", "m_b", 3),
                ledger("m_a", 1.0, 100.0, 0.0, 0.0),
                ledger("m_a", 2.0, 101.0, 0.0, 0.0),
                ledger("m_b", 5.0, 9.0, 0.0, 0.0),
                OwnerRecord::Forget { coid: "c1".into() },
                OwnerRecord::Unmount { mount_id: "m_b".into() },
            ],
        );
        let loaded = reload(&path, 1_000);
        assert_eq!(loaded.coids, vec![("c2".to_string(), "m_a".to_string())]);
        assert_eq!(loaded.ledgers.len(), 1, "the unmounted ledger is gone");
        assert_eq!(loaded.ledgers[0].0, "m_a");
        assert_eq!(loaded.ledgers[0].1.size, 2.0, "the last ledger wins");
    }

    /// A crash mid-write leaves a torn last line. It is skipped, the file is compacted (so the
    /// torn bytes are gone), and the next record starts on a line of its own.
    #[test]
    fn a_torn_last_line_is_skipped_and_the_next_record_is_whole() {
        let dir = Scratch::created("vco-owners-torn");
        let path = dir.join(ORDER_OWNERS_FILE);
        std::fs::write(
            &path,
            "{\"kind\":\"own\",\"coid\":\"c1\",\"mount_id\":\"m_a\",\"ts\":1}\n\
             {\"kind\":\"own\",\"coid\":\"c2\",\"mount_id\":\"m_a\",\"ts\":2}\n\
             {\"kind\":\"own\",\"coid\":\"c3\",\"mo",
        )
        .unwrap();
        let mut log = OrderOwnerLog::open(&path);
        let loaded = log.start(1_000);
        assert_eq!(loaded.coids.len(), 2, "the two whole lines load, the torn one is skipped");
        assert!(log.tx.is_some());
        log.record(own("c4", "m_b", 4));
        drop(log);
        for l in lines_of(&path) {
            serde_json::from_str::<OwnerRecord>(&l).expect("every line on disk parses");
        }
        assert_eq!(reload(&path, 1_000).coids.len(), 3);
    }

    /// A non-empty file in which NOTHING parses is not ours: no persistence, the bytes untouched
    /// (before and after a record and a drop), and the caller carries on with an empty load.
    #[test]
    fn a_garbage_only_file_is_left_byte_identical_and_persistence_is_off() {
        let dir = Scratch::created("vco-owners-garbage");
        let path = dir.join(ORDER_OWNERS_FILE);
        let garbage = b"PK\x03\x04 not a jsonl file\nsecond line\n".to_vec();
        std::fs::write(&path, &garbage).unwrap();
        let mut log = OrderOwnerLog::open(&path);
        assert_eq!(log.start(1_000), LoadedOwners::default());
        assert!(log.tx.is_none(), "persistence is off");
        log.record(own("c1", "m_a", 1));
        assert_eq!(log.unpersisted, 1, "the record is a counted no-op");
        drop(log);
        assert_eq!(std::fs::read(&path).unwrap(), garbage, "bytes UNCHANGED");
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        assert!(!PathBuf::from(tmp).exists(), "no compaction was attempted");
    }

    /// A path that exists and cannot be read (a DIRECTORY here) is an ERROR, logged, and the core
    /// runs without persistence; nothing at the path is touched.
    #[tracing_test::traced_test]
    #[test]
    fn an_unreadable_path_is_an_error_and_persistence_is_off() {
        let dir = Scratch::created("vco-owners-unreadable");
        let path = dir.join(ORDER_OWNERS_FILE);
        std::fs::create_dir_all(&path).unwrap();
        let mut log = OrderOwnerLog::open(&path);
        assert_eq!(log.start(1_000), LoadedOwners::default());
        assert!(log.tx.is_none(), "persistence is off");
        log.record(own("c1", "m_a", 1));
        assert_eq!(log.unpersisted, 1);
        drop(log);
        assert!(path.is_dir(), "the directory is still there, untouched");
        assert!(logs_contain("cannot be read"), "the refusal is logged as an error");
    }

    /// Ownership older than 30 days is dropped at load; past the cap the OLDEST go first; a ledger
    /// has no time-to-live.
    #[test]
    fn time_to_live_and_cap() {
        let dir = Scratch::reserved("vco-owners-ttl");
        let path = dir.join(ORDER_OWNERS_FILE);
        let now = 100 * TTL_MS;
        let mut recs = vec![
            own("old", "m_a", now - TTL_MS - 1),
            own("edge", "m_a", now - TTL_MS),
            OwnerRecord::Ledger {
                mount_id: "m_a".into(),
                size: 1.0,
                avg_px: 100.0,
                realized_pnl: 0.0,
                fees_paid: 0.0,
                ts: 0,
            },
        ];
        for i in 0..OWNER_CAP as i64 {
            recs.push(own(&format!("n{i}"), "m_a", now - 1_000 + i));
        }
        write_through(&path, recs);
        let loaded = reload(&path, now);
        assert_eq!(loaded.coids.len(), OWNER_CAP, "the cap holds");
        let kept: std::collections::HashSet<&str> =
            loaded.coids.iter().map(|(c, _)| c.as_str()).collect();
        assert!(!kept.contains("old"), "older than the time-to-live: dropped");
        assert!(!kept.contains("edge"), "the oldest past the cap is evicted first");
        assert!(kept.contains("n0") && kept.contains(&*format!("n{}", OWNER_CAP - 1)));
        assert_eq!(loaded.ledgers.len(), 1, "a month-old ledger is still a position");
    }

    /// A file far longer than its live state is compacted at load: the folded state survives
    /// exactly, the file is never missing, and no temporary file is left behind.
    #[test]
    fn compaction_at_load_keeps_the_folded_state() {
        let dir = Scratch::reserved("vco-owners-compact");
        let path = dir.join(ORDER_OWNERS_FILE);
        let mut recs = Vec::new();
        for i in 0..COMPACT_MIN_LINES {
            recs.push(own(&format!("gone{i}"), "m_a", i as i64));
            recs.push(OwnerRecord::Forget { coid: format!("gone{i}") });
        }
        recs.push(own("live", "m_b", 7));
        recs.push(ledger("m_b", 3.0, 50.0, 1.0, 0.25));
        // Through the WRITER: crossing the threshold compacts there too.
        write_through(&path, recs.clone());
        assert!(lines_of(&path).len() <= COMPACT_MIN_LINES, "the writer compacted");
        // Now the same history laid down raw, compacted by the boot.
        let mut raw = Vec::new();
        for r in &recs {
            encode(&mut raw, r);
        }
        std::fs::write(&path, &raw).unwrap();
        let before = reload(&path, 1_000);
        assert_eq!(lines_of(&path).len(), 2, "rewritten to the two live entries");
        assert_eq!(before, reload(&path, 1_000), "the folded state is unchanged by compaction");
        assert_eq!(before.coids, vec![("live".to_string(), "m_b".to_string())]);
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        assert!(!PathBuf::from(tmp).exists(), "the temporary file was renamed away");
    }

    /// A missing file (and a missing directory) is the first boot: both are created.
    #[test]
    fn a_missing_file_is_created() {
        let dir = Scratch::reserved("vco-owners-missing");
        let path = dir.join("strategy-state").join(ORDER_OWNERS_FILE);
        let mut log = OrderOwnerLog::open(&path);
        assert_eq!(log.start(1_000), LoadedOwners::default());
        assert!(log.tx.is_some());
        assert!(path.is_file(), "created at start");
    }
}
